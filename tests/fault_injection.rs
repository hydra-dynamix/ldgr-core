use std::fs;
use std::path::{Path, PathBuf};

use anyhow::Context;
use assert_cmd::Command;
use rusqlite::Connection;
use serde_json::Value;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

const FAULT_ENV: &str = "LDGR_TEST_FAULT_INJECTION";
const FAULT_MARKER_ENV: &str = "LDGR_TEST_FAULT_MARKER";
const FAULT_EXIT_CODE: i32 = 86;
const RESUME_WORK: &str = "resume-after-crash";

fn command(project: &TempDir) -> anyhow::Result<Command> {
    let mut command = Command::cargo_bin("ldgr")?;
    command
        .current_dir(project.path())
        .env(
            "LDGR_ADAPTER_PATH",
            project.path().join(".ldgr/test-empty-adapters"),
        )
        .env(
            "LDGR_HOME",
            project.path().join(".ldgr/test-empty-ldgr-home"),
        )
        .env("LOCALAPPDATA", project.path().join(".ldgr/test-state"))
        .env("XDG_STATE_HOME", project.path().join(".ldgr/test-state"))
        .env("HOME", project.path().join(".ldgr/test-empty-home"))
        .arg("--db")
        .arg(project.path().join(".ldgr/ldgr.db"))
        .arg("--artifact-root")
        .arg(project.path().join(".ldgr/artifacts"));
    Ok(command)
}

fn run(project: &TempDir, args: &[&str]) -> anyhow::Result<()> {
    let output = command(project)?.args(args).output()?;
    anyhow::ensure!(
        output.status.success(),
        "command {:?} failed:\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn json_output(project: &TempDir, args: &[&str]) -> anyhow::Result<Value> {
    let output = command(project)?.args(args).output()?;
    anyhow::ensure!(
        output.status.success(),
        "command {:?} failed:\nstdout:\n{}\nstderr:\n{}",
        args,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).context("parsing ldgr JSON output")
}

fn ldgr_executable() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ldgr"))
}

fn setup_resumable_project() -> anyhow::Result<TempDir> {
    let project = TempDir::new()?;
    run(&project, &["init"])?;
    run(
        &project,
        &[
            "work",
            "create",
            "prior-decision",
            "--title",
            "Prior decision",
            "--description",
            "Establish durable causal history before fault injection.",
        ],
    )?;
    run(
        &project,
        &[
            "work",
            "create",
            RESUME_WORK,
            "--title",
            "Resume after crash",
            "--description",
            "The interrupted work that status must reconstruct.",
        ],
    )?;
    run(
        &project,
        &["run", "start", "prior-decision", "--command", "fixture"],
    )?;
    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    let run_id: i64 =
        connection.query_row("SELECT id FROM run ORDER BY id DESC LIMIT 1", [], |row| {
            row.get(0)
        })?;
    drop(connection);
    run(
        &project,
        &[
            "run",
            "close",
            &run_id.to_string(),
            "--status",
            "success",
            "--outcome",
            "continue",
            "--rationale",
            "The next bounded action is the crash-recovery fixture.",
            "--next-slug",
            RESUME_WORK,
        ],
    )?;
    fs::write(project.path().join("loop-prompt.md"), "{{ldgr_context}}")?;
    Ok(project)
}

fn crash_loop(project: &TempDir, point: &str) -> anyhow::Result<()> {
    let executable = ldgr_executable().to_string_lossy().into_owned();
    let agent_argv = if point == "error-database-recording" {
        serde_json::to_string(&vec![executable.as_str(), "not-a-real-command"])?
    } else {
        serde_json::to_string(&vec![executable.as_str(), "--help"])?
    };
    let summary_argv = serde_json::to_string(&vec![executable.as_str(), "--help"])?;
    let marker = project.path().join(format!("{point}.marker"));
    let output = command(project)?
        .env(FAULT_ENV, point)
        .env(FAULT_MARKER_ENV, &marker)
        .args(["loop", "run", "--prompt", "loop-prompt.md", "--agent-argv"])
        .arg(agent_argv)
        .arg("--summary-argv")
        .arg(summary_argv)
        .output()?;
    anyhow::ensure!(
        output.status.code() == Some(FAULT_EXIT_CODE),
        "{point} did not terminate at the deterministic fault boundary: status={:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    anyhow::ensure!(
        fs::read_to_string(&marker)? == point,
        "{point} did not publish its durable test marker"
    );
    Ok(())
}

fn resume_from_status_only(project: &TempDir) -> anyhow::Result<Value> {
    // This is deliberately the first read after the crash. It models a fresh
    // conversation whose only instruction is "ldgr status and resume work".
    let status = json_output(project, &["status", "--full", "--json"])?;
    anyhow::ensure!(status["work_items"]["running"] == 0);
    anyhow::ensure!(status["work_items"]["pending"] == 1);
    anyhow::ensure!(status["next"]["slug"] == RESUME_WORK);
    anyhow::ensure!(status["active_runs"].as_array().is_some_and(Vec::is_empty));
    anyhow::ensure!(status["global_history"]["latest_decision"]["work"] == "prior-decision");
    anyhow::ensure!(status["global_history"]["latest_decision"]["next_work"] == RESUME_WORK);
    anyhow::ensure!(
        status["errors"]["latest"][0]["code"] == "interrupted-attempt",
        "unexpected recovered error: {}",
        status["errors"]
    );
    anyhow::ensure!(status["errors"]["latest"][0]["related_work"]
        .as_array()
        .is_some_and(|work| work.iter().any(|item| item["slug"] == RESUME_WORK)));
    let error_id = status["errors"]["latest"][0]["error_id"]
        .as_i64()
        .context("status omitted recovered error id")?;
    let disposition_command = format!("ldgr error disposition {error_id} ");
    anyhow::ensure!(
        status["next_commands"]
            .as_array()
            .is_some_and(|commands| commands.iter().any(|command| command
                .as_str()
                .is_some_and(|command| command.starts_with(&disposition_command)))),
        "status did not identify disposition as the correct next action: {}",
        status["next_commands"]
    );
    Ok(status)
}

fn authorize_retry(project: &TempDir, error_id: i64) -> anyhow::Result<()> {
    run(
        project,
        &[
            "error",
            "disposition",
            &error_id.to_string(),
            "--action",
            "retry",
            "--actor",
            "fault-injection-test",
            "--source",
            "cross-platform-subprocess",
            "--rationale",
            "Retry the unchanged operation to validate recurrent error reconstruction.",
            "--retry-basis",
            "explicit-confirmation",
        ],
    )
}

#[test]
fn abrupt_loop_boundaries_rehydrate_recurrence_decision_and_next_action() -> anyhow::Result<()> {
    for point in [
        "loop-before-spawn",
        "loop-after-spawn",
        "loop-mid-command",
        "loop-before-summary",
        "error-database-recording",
    ] {
        let project = setup_resumable_project()?;
        crash_loop(&project, point)?;
        let first = resume_from_status_only(&project)?;
        let error_id = first["errors"]["latest"][0]["error_id"]
            .as_i64()
            .context("first status omitted error id")?;
        assert_eq!(first["errors"]["latest"][0]["occurrence_count"], 1);

        authorize_retry(&project, error_id)?;
        crash_loop(&project, point)?;
        let repeated = resume_from_status_only(&project)?;
        assert_eq!(repeated["errors"]["latest"][0]["error_id"], error_id);
        assert_eq!(repeated["errors"]["latest"][0]["occurrence_count"], 2);
        assert_eq!(repeated["errors"]["latest"][0]["repeated"], true);
        assert_eq!(
            repeated["errors"]["latest"][0]["latest_disposition"]["action"],
            "retry"
        );

        let context = json_output(
            &project,
            &["error", "context", &error_id.to_string(), "--json"],
        )?;
        assert_eq!(context["repeated"], true);
        assert_eq!(context["prior_occurrences"].as_array().unwrap().len(), 1);
        assert_eq!(
            context["dispositions"][0]["disposition"], "retry",
            "{point} did not preserve the prior explicit retry decision"
        );
    }
    Ok(())
}

#[test]
fn init_status_and_context_recover_from_mid_migration_process_death() -> anyhow::Result<()> {
    for entrypoint in ["init", "status", "context"] {
        let project = TempDir::new()?;
        run(&project, &["init"])?;
        downgrade_to_v1(&project.path().join(".ldgr/ldgr.db"))?;
        let marker = project
            .path()
            .join(format!("migration-{entrypoint}.marker"));
        let output = command(&project)?
            .env(FAULT_ENV, "automatic-migration")
            .env(FAULT_MARKER_ENV, &marker)
            .arg(entrypoint)
            .output()?;
        assert_eq!(output.status.code(), Some(FAULT_EXIT_CODE), "{entrypoint}");
        assert_eq!(
            fs::read_to_string(&marker)?,
            "automatic-migration",
            "{entrypoint}"
        );
        assert_eq!(
            schema_version(&project)?,
            1,
            "{entrypoint} committed a torn migration"
        );
        assert!(
            migration_backups(&project)?
                .iter()
                .any(|path| path.is_file()),
            "{entrypoint} did not preserve the verified pre-migration backup"
        );

        run(&project, &[entrypoint])?;
        assert_eq!(schema_version(&project)?, 5, "{entrypoint}");
        let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
        let migration_events: i64 = connection.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE entity_type='schema' AND event_type='migration'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(migration_events, 1, "{entrypoint}");
    }
    Ok(())
}

#[test]
fn recovery_import_reclaims_a_record_claimed_by_the_crashed_process() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    run(&project, &["init"])?;
    let occurrence = "0198f100-0000-7000-8000-00000000f501";
    write_recovery_fixture(&project, occurrence)?;
    let marker = project.path().join("recovery-import.marker");
    let output = command(&project)?
        .env(FAULT_ENV, "recovery-spool-import")
        .env(FAULT_MARKER_ENV, &marker)
        .args(["status", "--json"])
        .output()?;
    assert_eq!(output.status.code(), Some(FAULT_EXIT_CODE));
    assert_eq!(fs::read_to_string(marker)?, "recovery-spool-import");
    assert_eq!(occurrence_count(&project, occurrence)?, 0);
    assert!(
        fs::read_dir(project.path().join(".ldgr/recovery/inbox"))?
            .filter_map(Result::ok)
            .any(|entry| entry
                .file_name()
                .to_string_lossy()
                .starts_with(".reconciling-")),
        "crash did not leave the atomic recovery claim to be reclaimed"
    );

    let resumed = json_output(&project, &["status", "--full", "--json"])?;
    assert_eq!(occurrence_count(&project, occurrence)?, 1);
    assert_eq!(resumed["errors"]["latest"][0]["code"], "emergency-spool");
    assert!(project
        .path()
        .join(".ldgr/recovery/archive/spooled.json")
        .is_file());
    assert!(fs::read_dir(project.path().join(".ldgr/recovery/inbox"))?
        .filter_map(Result::ok)
        .all(|entry| !entry
            .file_name()
            .to_string_lossy()
            .starts_with(".reconciling-")));
    Ok(())
}

fn schema_version(project: &TempDir) -> anyhow::Result<i64> {
    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    Ok(
        connection.query_row("SELECT version FROM schema_version WHERE id=1", [], |row| {
            row.get(0)
        })?,
    )
}

fn migration_backups(project: &TempDir) -> anyhow::Result<Vec<PathBuf>> {
    Ok(fs::read_dir(project.path().join(".ldgr"))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .is_some_and(|name| name.to_string_lossy().contains("backup-schema-v1-to-v5"))
                && path
                    .extension()
                    .is_some_and(|extension| extension == "sqlite3")
        })
        .collect())
}

fn occurrence_count(project: &TempDir, occurrence_id: &str) -> anyhow::Result<i64> {
    let connection = Connection::open(project.path().join(".ldgr/ldgr.db"))?;
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM error_occurrence WHERE occurrence_id=?1",
        [occurrence_id],
        |row| row.get(0),
    )?)
}

fn write_recovery_fixture(project: &TempDir, occurrence_id: &str) -> anyhow::Result<()> {
    let db_path = project.path().join(".ldgr/ldgr.db");
    let connection = Connection::open(&db_path)?;
    let project_id: String = connection.query_row(
        "SELECT project_id FROM project_identity WHERE id=1",
        [],
        |row| row.get(0),
    )?;
    let inputs = serde_json::json!({
        "class": "infrastructure-error",
        "domain": "test.fault-injection",
        "code": "emergency-spool",
        "boundary": "recovery-import",
        "component": "fault-injection-test",
        "subject": "recovery-envelope",
    });
    let fingerprint = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&inputs)?));
    let envelope = serde_json::json!({
        "format": "ldgr-error-recovery",
        "schema_version": 1,
        "project": {
            "project_id": project_id,
            "locator": project.path().to_string_lossy().replace('\\', "/"),
            "database_identity": null,
        },
        "producer": "fault-injection-test",
        "idempotency_key": format!("{occurrence_id}:emergency-spool"),
        "operation_id": occurrence_id,
        "attempt_id": occurrence_id,
        "occurrence_id": occurrence_id,
        "fingerprint": {
            "version": "structured-v1",
            "value": fingerprint,
            "inputs": inputs,
        },
        "error": {
            "class": "infrastructure-error",
            "domain": "test.fault-injection",
            "code": "emergency-spool",
            "severity": "error",
            "retryability": "after-change",
            "source": "fault-injection-test:recovery-import",
            "summary": "A deterministic recovery record was claimed before process death.",
            "details": {},
            "environment": {"os": std::env::consts::OS},
        },
        "observed_at": "2026-07-31T00:00:00Z",
    });
    let inbox = project.path().join(".ldgr/recovery/inbox");
    fs::create_dir_all(&inbox)?;
    fs::write(
        inbox.join("spooled.json"),
        serde_json::to_vec_pretty(&envelope)?,
    )?;
    Ok(())
}

fn downgrade_to_v1(db_path: &Path) -> anyhow::Result<()> {
    let connection = Connection::open(db_path)?;
    connection.execute_batch(
        r#"
        DROP TABLE component_record;
        DROP TABLE component_ingest;
        DROP TABLE schema_component;
        DROP TABLE error_disposition;
        DROP TABLE error_transition;
        DROP TABLE error_relation;
        DROP TABLE error_occurrence;
        DROP TABLE error_record;
        DROP TABLE project_identity;
        DROP TRIGGER IF EXISTS trg_work_dependency_no_cycle;
        DROP INDEX IF EXISTS idx_work_dependency_depends_on;
        DROP INDEX IF EXISTS idx_work_item_priority_program;
        DROP TABLE work_dependency;
        ALTER TABLE work_item DROP COLUMN hold_reason;
        ALTER TABLE work_item DROP COLUMN hold_kind;
        ALTER TABLE work_item DROP COLUMN acceptance_criteria;
        ALTER TABLE work_item DROP COLUMN work_group;
        ALTER TABLE work_item DROP COLUMN program;
        ALTER TABLE work_item DROP COLUMN priority;
        UPDATE schema_version SET version = 1 WHERE id = 1;
        "#,
    )?;
    Ok(())
}
