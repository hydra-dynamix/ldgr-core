use assert_cmd::Command;
use rusqlite::Connection;
use serde_json::Value;
use tempfile::TempDir;

fn command(project: &TempDir) -> anyhow::Result<Command> {
    let mut command = Command::cargo_bin("ldgr")?;
    command
        .current_dir(project.path())
        .arg("--db")
        .arg(project.path().join(".ldgr/ldgr.db"))
        .arg("--artifact-root")
        .arg(project.path().join(".ldgr/artifacts"));
    Ok(command)
}

fn json_output(command: &mut Command) -> anyhow::Result<Value> {
    let output = command.output()?;
    anyhow::ensure!(
        output.status.success(),
        "command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(serde_json::from_slice(&output.stdout)?)
}

fn record_structured(
    project: &TempDir,
    occurrence: &str,
    code: &str,
    summary: &str,
    observed_at: &str,
    extra: &[&str],
) -> anyhow::Result<Value> {
    let mut command = command(project)?;
    command
        .args(["error", "record"])
        .args(["--occurrence-id", occurrence])
        .args(["--producer", "structured-cli-test"])
        .args(["--idempotency-key", occurrence])
        .args(["--operation-id", "operation-structured"])
        .args(["--attempt-id", occurrence])
        .args(["--class", "infrastructure-error"])
        .args(["--domain", "test.structured"])
        .args(["--code", code])
        .args(["--boundary", "test-boundary"])
        .args(["--component", "ldgr-core"])
        .args(["--subject", "recurrence"])
        .args(["--severity", "error"])
        .args(["--retryability", "after-change"])
        .args(["--source", "cli-test:structured"])
        .args(["--summary", summary])
        .args(["--observed-at", observed_at])
        .args(extra)
        .arg("--json");
    json_output(&mut command)
}

#[test]
fn shorthand_error_report_generates_metadata_and_links_the_active_run() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    command(&project)?.arg("init").assert().success();
    command(&project)?
        .args([
            "work",
            "create",
            "fix-tests",
            "--title",
            "Fix tests",
            "--description",
            "Repair the focused test failure",
        ])
        .assert()
        .success();
    command(&project)?
        .args(["run", "start", "fix-tests", "--command", "repair tests"])
        .assert()
        .success();

    let output = command(&project)?
        .args([
            "error",
            "cargo-test",
            "validation",
            "focused",
            "test",
            "still",
            "fails",
        ])
        .output()?;
    anyhow::ensure!(
        output.status.success(),
        "shorthand error command failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8(output.stdout)?;
    assert!(stdout.contains("command=cargo-test type=validation"));
    assert!(stdout.contains("work=fix-tests"));
    assert!(stdout.contains("message: focused test still fails"));

    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let occurrence = connection.query_row(
        "SELECT class, domain, code, summary, json_extract(details_json, '$.command')
         FROM error_occurrence",
        [],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, String>(4)?,
            ))
        },
    )?;
    assert_eq!(
        occurrence,
        (
            "validation-failure".to_owned(),
            "agent.command".to_owned(),
            "validation".to_owned(),
            "focused test still fails".to_owned(),
            "cargo-test".to_owned(),
        )
    );
    let relation_count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM error_relation WHERE relation_kind = 'affected'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(relation_count, 2);
    Ok(())
}

#[test]
fn durable_error_cli_covers_classes_replay_lifecycle_and_relations() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    command(&project)?.arg("init").assert().success();

    let classes = [
        "task-failure",
        "validation-failure",
        "infrastructure-error",
        "interruption",
        "operator-cancellation",
    ];
    let mut first_error_id = None;
    for (index, class) in classes.iter().enumerate() {
        let occurrence = format!("occurrence-{index}");
        let key = format!("key-{index}");
        let fingerprint = format!("sha256:{}", format!("{:x}", index + 10).repeat(64));
        let value = json_output(
            command(&project)?
                .args(["error", "record"])
                .args(["--occurrence-id", &occurrence])
                .args(["--producer", "cli-test"])
                .args(["--idempotency-key", &key])
                .args(["--operation-id", "operation-1"])
                .args(["--attempt-id", &occurrence])
                .args(["--fingerprint", &fingerprint])
                .args([
                    "--fingerprint-override-rationale",
                    "exercise explicit historical producer fingerprint",
                ])
                .args(["--class", class])
                .args(["--domain", "test.cli"])
                .args(["--code", &format!("class-{index}")])
                .args(["--severity", "error"])
                .args(["--retryability", "after-change"])
                .args(["--source", "cli-test:boundary"])
                .args(["--summary", "durable CLI occurrence"])
                .args(["--observed-at", "2026-07-31T00:00:00Z"])
                .arg("--json"),
        )?;
        assert_eq!(value["occurrence"]["class"], *class);
        assert_eq!(value["idempotent_replay"], false);
        first_error_id.get_or_insert(value["error"]["id"].as_i64().unwrap());
    }

    let replay_fingerprint = format!("sha256:{}", "a".repeat(64));
    let replay = json_output(
        command(&project)?
            .args(["error", "record"])
            .args(["--occurrence-id", "occurrence-0"])
            .args(["--producer", "cli-test"])
            .args(["--idempotency-key", "key-0"])
            .args(["--operation-id", "operation-1"])
            .args(["--attempt-id", "occurrence-0"])
            .args(["--fingerprint", &replay_fingerprint])
            .args([
                "--fingerprint-override-rationale",
                "exercise explicit historical producer fingerprint",
            ])
            .args(["--class", "task-failure"])
            .args(["--domain", "test.cli"])
            .args(["--code", "class-0"])
            .args(["--severity", "error"])
            .args(["--retryability", "after-change"])
            .args(["--source", "cli-test:boundary"])
            .args(["--summary", "durable CLI occurrence"])
            .args(["--observed-at", "2026-07-31T00:00:00Z"])
            .arg("--json"),
    )?;
    assert_eq!(replay["idempotent_replay"], true);

    let listed = json_output(command(&project)?.args(["error", "list", "--json"]))?;
    assert_eq!(listed.as_array().unwrap().len(), classes.len());
    let error_id = first_error_id.unwrap().to_string();
    let shown = json_output(command(&project)?.args(["error", "show", &error_id, "--json"]))?;
    assert_eq!(shown["occurrences"].as_array().unwrap().len(), 1);

    let acknowledged = json_output(
        command(&project)?
            .args(["error", "acknowledge", &error_id])
            .args(["--actor", "operator"])
            .args(["--source", "cli"])
            .args(["--rationale", "investigating"])
            .arg("--json"),
    )?;
    assert_eq!(acknowledged["state"], "acknowledged");
    let resolved = json_output(
        command(&project)?
            .args(["error", "resolve", &error_id])
            .args(["--actor", "operator"])
            .args(["--source", "cli"])
            .args(["--rationale", "verified fixed"])
            .arg("--json"),
    )?;
    assert_eq!(resolved["state"], "resolved");
    assert_eq!(resolved["disposition_pending"], false);

    let occurrence = json_output(command(&project)?.args([
        "error",
        "occurrence",
        "show",
        "occurrence-0",
        "--json",
    ]))?;
    assert_eq!(occurrence["occurrence_id"], "occurrence-0");
    let relation = json_output(
        command(&project)?
            .args(["error", "link", &error_id])
            .args(["--kind", "reported-by"])
            .args(["--entity-type", "external"])
            .args(["--entity-id", "supervisor:run-1"])
            .args(["--source", "cli"])
            .arg("--json"),
    )?;
    assert_eq!(relation["entity_id"], "supervisor:run-1");

    command(&project)?
        .args(["error", "link", &error_id])
        .args(["--kind", "reported-by"])
        .args(["--entity-type", "run"])
        .args(["--entity-id", "999999"])
        .args(["--source", "cli"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("run 999999 not found"));
    Ok(())
}

#[test]
fn structured_fingerprints_group_recurrence_and_support_context_override_and_split(
) -> anyhow::Result<()> {
    let project = TempDir::new()?;
    command(&project)?.arg("init").assert().success();

    let first = record_structured(
        &project,
        "structured-1",
        "same-cause",
        "message variant one",
        "2026-07-31T00:00:01Z",
        &[],
    )?;
    let second = record_structured(
        &project,
        "structured-2",
        "same-cause",
        "harmlessly different message",
        "2026-07-31T00:00:02Z",
        &[],
    )?;
    assert_eq!(first["error"]["id"], second["error"]["id"]);
    assert_eq!(second["recurrent"], true);
    assert_eq!(second["error"]["occurrence_count"], 2);
    assert_eq!(second["error"]["first_seen_at"], "2026-07-31T00:00:01Z");
    assert_eq!(second["error"]["last_seen_at"], "2026-07-31T00:00:02Z");
    assert_eq!(second["context"]["repeated"], true);
    assert_eq!(
        second["context"]["prior_occurrences"][0]["occurrence_id"],
        "structured-1"
    );
    command(&project)?
        .args(["error", "record"])
        .args(["--occurrence-id", "structured-human"])
        .args(["--producer", "structured-cli-test"])
        .args(["--idempotency-key", "structured-human"])
        .args(["--operation-id", "operation-structured"])
        .args(["--attempt-id", "structured-human"])
        .args(["--class", "infrastructure-error"])
        .args(["--domain", "test.structured"])
        .args(["--code", "same-cause"])
        .args(["--boundary", "test-boundary"])
        .args(["--component", "ldgr-core"])
        .args(["--subject", "recurrence"])
        .args(["--severity", "error"])
        .args(["--retryability", "after-change"])
        .args(["--source", "cli-test:structured"])
        .args(["--summary", "human recurrence output"])
        .args(["--observed-at", "2026-07-31T00:00:03Z"])
        .assert()
        .success()
        .stdout(predicates::str::contains("recurrence: true"))
        .stdout(predicates::str::contains("context: prior="));

    let unrelated = record_structured(
        &project,
        "structured-3",
        "different-cause",
        "harmlessly different message",
        "2026-07-31T00:00:03Z",
        &[],
    )?;
    assert_ne!(first["error"]["id"], unrelated["error"]["id"]);

    let split = record_structured(
        &project,
        "structured-split",
        "same-cause",
        "same message",
        "2026-07-31T00:00:04Z",
        &[
            "--fingerprint-split",
            "distinct-causal-branch",
            "--fingerprint-split-rationale",
            "environment evidence separates this branch",
        ],
    )?;
    assert_ne!(first["error"]["id"], split["error"]["id"]);
    assert_eq!(
        split["error"]["fingerprint_version"],
        "structured-v1+split-v1"
    );

    let error_id = first["error"]["id"].as_i64().unwrap().to_string();
    let context = json_output(
        command(&project)?
            .args(["error", "context", &error_id])
            .args(["--occurrence-id", "structured-2"])
            .args(["--limit", "0"])
            .arg("--json"),
    )?;
    assert_eq!(context["prior_occurrences"].as_array().unwrap().len(), 0);
    assert_eq!(context["sections"]["prior_occurrences"]["truncated"], true);

    let collision_fingerprint = first["error"]["fingerprint"].as_str().unwrap();
    command(&project)?
        .args(["error", "record"])
        .args(["--occurrence-id", "structured-collision"])
        .args(["--producer", "structured-cli-test"])
        .args(["--idempotency-key", "structured-collision"])
        .args(["--operation-id", "operation-structured"])
        .args(["--attempt-id", "structured-collision"])
        .args(["--fingerprint", collision_fingerprint])
        .args([
            "--fingerprint-override-rationale",
            "deliberate collision test",
        ])
        .args(["--class", "infrastructure-error"])
        .args(["--domain", "test.structured"])
        .args(["--code", "same-cause"])
        .args(["--boundary", "different-boundary"])
        .args(["--component", "ldgr-core"])
        .args(["--subject", "recurrence"])
        .args(["--severity", "error"])
        .args(["--retryability", "after-change"])
        .args(["--source", "cli-test:structured"])
        .args(["--summary", "collision"])
        .args(["--observed-at", "2026-07-31T00:00:05Z"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("fingerprint collision"));

    let historical = record_structured(
        &project,
        "structured-v2",
        "same-cause",
        "historical policy",
        "2026-07-31T00:00:06Z",
        &[
            "--fingerprint-version",
            "structured-v2",
            "--fingerprint",
            collision_fingerprint,
            "--fingerprint-override-rationale",
            "preserve historical v2 producer interpretation",
        ],
    )?;
    assert_ne!(first["error"]["id"], historical["error"]["id"]);
    assert_eq!(historical["error"]["fingerprint_version"], "structured-v2");
    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let split_events: i64 = connection.query_row(
        "SELECT COUNT(*) FROM event_log WHERE event_type='fingerprint_split'",
        [],
        |row| row.get(0),
    )?;
    let override_events: i64 = connection.query_row(
        "SELECT COUNT(*) FROM event_log WHERE event_type='fingerprint_override'",
        [],
        |row| row.get(0),
    )?;
    assert_eq!(split_events, 1);
    assert_eq!(override_events, 1);
    Ok(())
}

#[test]
fn status_and_context_surface_bounded_error_recurrence_work_and_disposition() -> anyhow::Result<()>
{
    let project = TempDir::new()?;
    command(&project)?.arg("init").assert().success();
    command(&project)?
        .args(["work", "create", "error-surface-work"])
        .args(["--title", "Error surface work"])
        .args([
            "--description",
            "Exercise related work in handoff surfaces.",
        ])
        .assert()
        .success();

    let first = record_structured(
        &project,
        "surface-repeat-1",
        "surface-repeat",
        "first occurrence",
        "2026-08-01T00:00:01Z",
        &[],
    )?;
    let repeated = record_structured(
        &project,
        "surface-repeat-2",
        "surface-repeat",
        "latest repeated occurrence",
        "2026-08-01T00:00:02Z",
        &[],
    )?;
    let error_id = repeated["error"]["id"].as_i64().unwrap();
    assert_eq!(first["error"]["id"], repeated["error"]["id"]);

    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let work_id: i64 = connection.query_row(
        "SELECT id FROM work_item WHERE slug='error-surface-work'",
        [],
        |row| row.get(0),
    )?;
    drop(connection);
    command(&project)?
        .args(["error", "link", &error_id.to_string()])
        .args(["--kind", "affected-work"])
        .args(["--entity-type", "work_item"])
        .args(["--entity-id", &work_id.to_string()])
        .args(["--source", "cli-test"])
        .assert()
        .success();
    command(&project)?
        .args(["error", "disposition", &error_id.to_string()])
        .args(["--action", "defer"])
        .args(["--actor", "operator"])
        .args(["--source", "cli-test"])
        .args(["--rationale", "Preserve for the next bounded work item."])
        .assert()
        .success();

    for index in 0..6 {
        record_structured(
            &project,
            &format!("surface-other-{index}"),
            &format!("surface-other-{index}"),
            "older independent occurrence",
            &format!("2026-07-31T23:59:{index:02}Z"),
            &[],
        )?;
    }

    let status = json_output(command(&project)?.args(["status", "--json"]))?;
    assert_eq!(status["errors"]["counts"]["total"], 7);
    assert_eq!(status["errors"]["counts"]["unresolved"], 7);
    assert_eq!(status["errors"]["counts"]["repeated"], 1);
    assert_eq!(status["errors"]["counts"]["disposition_pending"], 6);
    assert_eq!(status["errors"]["bounds"]["errors"], 1);
    assert_eq!(status["errors"]["truncated"], true);
    assert_eq!(status["errors"]["latest"].as_array().unwrap().len(), 1);
    assert_eq!(
        status["errors"]["latest"][0]["latest_occurrence"]["occurrence_id"],
        "surface-repeat-2"
    );
    assert_eq!(status["errors"]["latest"][0]["repeated"], true);
    assert_eq!(
        status["errors"]["latest"][0]["latest_disposition"]["action"],
        "defer"
    );
    assert_eq!(
        status["errors"]["latest"][0]["related_work"][0]["slug"],
        "error-surface-work"
    );

    let context = json_output(command(&project)?.args(["context", "--json"]))?;
    assert_eq!(context["errors"]["bounds"]["errors"], 5);
    assert_eq!(context["errors"]["truncated"], true);
    assert_eq!(context["errors"]["latest"].as_array().unwrap().len(), 5);
    assert_eq!(
        context["errors"]["latest"][0]["latest_occurrence"]["occurrence_id"],
        "surface-repeat-2"
    );
    assert_eq!(
        context["errors"]["latest"][0]["related_work"][0]["status"],
        "pending"
    );
    assert_eq!(
        context["errors"]["latest"][0]["latest_disposition"]["action"],
        "defer"
    );

    command(&project)?
        .arg("status")
        .assert()
        .success()
        .stdout(predicates::str::contains(
            "errors: unresolved=7 repeated=1 disposition_pending=6 total=7",
        ))
        .stdout(predicates::str::contains("latest_error: error="))
        .stdout(predicates::str::contains(
            "occurrence=surface-repeat-2 state=open repeated=true occurrences=2 disposition=defer",
        ));
    command(&project)?
        .arg("context")
        .assert()
        .success()
        .stdout(predicates::str::contains("latest_errors:"))
        .stdout(predicates::str::contains(
            "related_work: error-surface-work(pending)",
        ))
        .stdout(predicates::str::contains(
            "latest_disposition: action=defer occurrence=surface-repeat-2",
        ));
    Ok(())
}

#[test]
fn dispositions_link_decisions_gate_retries_and_block_false_success() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    command(&project)?.arg("init").assert().success();

    command(&project)?
        .args(["work", "create", "decision-source"])
        .args(["--title", "Decision source"])
        .args(["--description", "Create an existing causal decision."])
        .assert()
        .success();
    command(&project)?
        .args(["run", "start", "decision-source"])
        .assert()
        .success();
    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let decision_run_id: i64 =
        connection.query_row("SELECT MAX(id) FROM run", [], |row| row.get(0))?;
    drop(connection);
    command(&project)?
        .args(["run", "close", &decision_run_id.to_string()])
        .args(["--status", "success"])
        .args(["--outcome", "stop"])
        .args(["--rationale", "Decision fixture is complete."])
        .assert()
        .success();
    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let decision_id: i64 =
        connection.query_row("SELECT MAX(id) FROM decision", [], |row| row.get(0))?;
    drop(connection);

    let first = record_structured(
        &project,
        "disposition-1",
        "disposition-policy",
        "first failure",
        "2026-07-31T01:00:00Z",
        &[],
    )?;
    let error_id = first["error"]["id"].as_i64().unwrap();
    let error_id_text = error_id.to_string();
    let first_disposition = json_output(
        command(&project)?
            .args(["error", "disposition", &error_id_text])
            .args(["--action", "defer"])
            .args(["--actor", "operator"])
            .args(["--source", "cli-test"])
            .args(["--rationale", "Wait for the approved dependency."])
            .args(["--decision-id", &decision_id.to_string()])
            .arg("--json"),
    )?;
    assert_eq!(first_disposition["action"], "defer");
    assert_eq!(first_disposition["decision_id"], decision_id);
    assert_eq!(
        first_disposition["resulting_work_transition"],
        "work-deferred"
    );
    let prior_disposition_id = first_disposition["id"].as_i64().unwrap().to_string();

    let second = record_structured(
        &project,
        "disposition-2",
        "disposition-policy",
        "same cause, second attempt",
        "2026-07-31T01:00:01Z",
        &[],
    )?;
    assert_eq!(second["retry_gate"], "disposition-required");
    assert_eq!(second["context"]["dispositions"][0]["disposition"], "defer");
    command(&project)?
        .args(["error", "retry-check", &error_id_text])
        .assert()
        .failure()
        .stderr(predicates::str::contains("retry blocked"));
    command(&project)?
        .args(["error", "disposition", &error_id_text])
        .args(["--action", "retry"])
        .args(["--actor", "operator"])
        .args(["--source", "cli-test"])
        .args(["--rationale", "Try the identical operation again."])
        .assert()
        .failure()
        .stderr(predicates::str::contains("requires --prior-disposition-id"));
    let retry = json_output(
        command(&project)?
            .args(["error", "disposition", &error_id_text])
            .args(["--action", "retry"])
            .args(["--actor", "operator"])
            .args(["--source", "cli-test"])
            .args([
                "--rationale",
                "Explicitly confirm retry after reviewing the prior defer decision.",
            ])
            .args(["--retry-basis", "explicit-confirmation"])
            .args(["--prior-disposition-id", &prior_disposition_id])
            .arg("--json"),
    )?;
    assert_eq!(retry["retry_basis"], "explicit-confirmation");
    assert_eq!(
        retry["prior_disposition_id"].as_i64().unwrap().to_string(),
        prior_disposition_id
    );
    let authorization = json_output(
        command(&project)?
            .args(["error", "retry-check", &error_id_text])
            .arg("--json"),
    )?;
    assert_eq!(
        authorization["disposition"]["id"], retry["id"],
        "{authorization}"
    );
    assert_eq!(authorization["context"]["repeated"], true);

    command(&project)?
        .args(["work", "create", "blocked-success"])
        .args(["--title", "Blocked success"])
        .args(["--description", "Exercise error completion gating."])
        .assert()
        .success();
    command(&project)?
        .args(["run", "start", "blocked-success"])
        .assert()
        .success();
    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let blocked_run_id: i64 =
        connection.query_row("SELECT MAX(id) FROM run", [], |row| row.get(0))?;
    let blocked_run_id = blocked_run_id.to_string();
    drop(connection);
    command(&project)?
        .args(["error", "link", &error_id_text])
        .args(["--kind", "affected"])
        .args(["--entity-type", "run"])
        .args(["--entity-id", &blocked_run_id])
        .args(["--source", "cli-test"])
        .assert()
        .success();
    command(&project)?
        .args(["run", "close", &blocked_run_id])
        .args(["--status", "success"])
        .args(["--outcome", "stop"])
        .args(["--rationale", "This must not pass while the error is open."])
        .assert()
        .failure()
        .stderr(predicates::str::contains("related blocking errors remain"));

    let accepted = json_output(
        command(&project)?
            .args(["error", "disposition", &error_id_text])
            .args(["--action", "accept"])
            .args(["--actor", "operator"])
            .args(["--source", "cli-test"])
            .args([
                "--rationale",
                "Explicitly accept the remaining impact for this bounded fixture.",
            ])
            .arg("--json"),
    )?;
    assert_eq!(accepted["action"], "accept");
    let shown = json_output(
        command(&project)?
            .args(["error", "show", &error_id_text])
            .arg("--json"),
    )?;
    assert_eq!(shown["error"]["state"], "accepted");
    assert_eq!(shown["error"]["disposition_pending"], false);
    assert_eq!(shown["dispositions"].as_array().unwrap().len(), 3);
    command(&project)?
        .args(["run", "close", &blocked_run_id])
        .args(["--status", "success"])
        .args(["--outcome", "stop"])
        .args(["--rationale", "Explicit acceptance removed the blocker."])
        .assert()
        .success();

    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let decision_links: i64 = connection.query_row(
        "SELECT COUNT(*) FROM error_relation
         WHERE error_id=?1 AND relation_kind='disposition-decision'
           AND entity_type='decision' AND entity_id=?2",
        rusqlite::params![error_id, decision_id.to_string()],
        |row| row.get(0),
    )?;
    assert_eq!(decision_links, 1);
    Ok(())
}
