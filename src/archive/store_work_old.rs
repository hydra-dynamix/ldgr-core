use super::*;

#[derive(Debug, Clone, Copy)]
pub struct NextWorkSpec<'a> {
    pub slug: &'a str,
    pub title: Option<&'a str>,
    pub description: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CloseRunResult {
    pub run: InvestigationRun,
    pub decision: Decision,
    pub work_item: WorkItem,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ClaimedRun {
    pub work_item: WorkItem,
    pub run: InvestigationRun,
}

pub fn create_work_item(
    connection: &Connection,
    parent_work_item_id: Option<i64>,
    slug: &str,
    title: &str,
    description: &str,
) -> anyhow::Result<WorkItem> {
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO work_item (parent_work_item_id, slug, title, description)
             VALUES (?1, ?2, ?3, ?4)",
                params![parent_work_item_id, slug, title, description],
            )
            .with_context(|| format!("failed to create work item {slug}"))?;
        let work_item_id = connection.last_insert_rowid();
        record_event(connection, "work_item", work_item_id, "create", "{}")?;
        get_work_item_by_id(connection, work_item_id)
    })
}

pub fn edit_work_item(
    connection: &Connection,
    slug: &str,
    title: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<WorkItem> {
    if title.is_none() && description.is_none() {
        bail!("work edit requires --title and/or --description");
    }
    if title.is_some_and(|title| title.trim().is_empty()) {
        bail!("work title must not be empty");
    }
    if description.is_some_and(|description| description.trim().is_empty()) {
        bail!("work description must not be empty");
    }
    let work_item = require_work_item_by_slug(connection, slug)?;
    let next_title = title.unwrap_or(&work_item.title);
    let next_description = description.unwrap_or(&work_item.description);
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item
                 SET title = ?1, description = ?2, updated_at = datetime('now')
                 WHERE id = ?3",
                params![next_title, next_description, work_item.id],
            )
            .with_context(|| format!("failed to edit work item {slug}"))?;
        let payload = serde_json::json!({
            "title": title,
            "description": description,
        })
        .to_string();
        record_event(connection, "work_item", work_item.id, "edit", &payload)?;
        get_work_item_by_id(connection, work_item.id)
    })
}

pub fn cancel_work_item(
    connection: &Connection,
    slug: &str,
    reason: Option<&str>,
) -> anyhow::Result<WorkItem> {
    let work_item = require_work_item_by_slug(connection, slug)?;
    if work_item.status == WorkItemStatus::Done {
        bail!("work item {slug} is already done");
    }
    if work_item.status == WorkItemStatus::Canceled {
        return Ok(work_item);
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item SET status = 'canceled', updated_at = datetime('now') WHERE id = ?1",
                params![work_item.id],
            )
            .with_context(|| format!("failed to cancel work item {slug}"))?;
        let payload = reason
            .map(|reason| serde_json::json!({ "reason": reason }).to_string())
            .unwrap_or_else(|| "{}".to_owned());
        record_event(connection, "work_item", work_item.id, "cancel", &payload)?;
        get_work_item_by_id(connection, work_item.id)
    })
}

pub fn hold_work_item(
    connection: &Connection,
    slug: &str,
    reason: Option<&str>,
) -> anyhow::Result<WorkItem> {
    let work_item = require_work_item_by_slug(connection, slug)?;
    match work_item.status {
        WorkItemStatus::Pending | WorkItemStatus::Running => {}
        WorkItemStatus::Held => return Ok(work_item),
        WorkItemStatus::Done => bail!("work item {slug} is already done"),
        WorkItemStatus::Canceled => bail!("work item {slug} is canceled"),
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item SET status = 'held', updated_at = datetime('now') WHERE id = ?1",
                params![work_item.id],
            )
            .with_context(|| format!("failed to hold work item {slug}"))?;
        let notes = reason
            .map(|reason| format!("held work item: {reason}"))
            .unwrap_or_else(|| "held work item".to_owned());
        let run_ids = running_run_ids_for_work_item(connection, work_item.id)?;
        for run_id in run_ids {
            connection
                .execute(
                    "UPDATE run
                     SET status = 'partial', finished_at = datetime('now'), notes = ?1
                     WHERE id = ?2 AND status = 'running'",
                    params![notes, run_id],
                )
                .with_context(|| format!("failed to mark held run {run_id} partial"))?;
            let run_payload = serde_json::json!({
                "status": "partial",
                "notes": notes,
            })
            .to_string();
            record_event(connection, "run", run_id, "finish", &run_payload)?;
        }
        let payload = reason
            .map(|reason| serde_json::json!({ "reason": reason }).to_string())
            .unwrap_or_else(|| "{}".to_owned());
        record_event(connection, "work_item", work_item.id, "hold", &payload)?;
        get_work_item_by_id(connection, work_item.id)
    })
}

pub fn resume_work_item(
    connection: &Connection,
    slug: &str,
    reason: Option<&str>,
) -> anyhow::Result<WorkItem> {
    let work_item = require_work_item_by_slug(connection, slug)?;
    if work_item.status != WorkItemStatus::Held {
        bail!("work item {slug} is not held");
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item SET status = 'pending', updated_at = datetime('now') WHERE id = ?1",
                params![work_item.id],
            )
            .with_context(|| format!("failed to resume work item {slug}"))?;
        let payload = reason
            .map(|reason| serde_json::json!({ "reason": reason }).to_string())
            .unwrap_or_else(|| "{}".to_owned());
        record_event(connection, "work_item", work_item.id, "resume", &payload)?;
        get_work_item_by_id(connection, work_item.id)
    })
}

pub fn set_work_item_status(
    connection: &Connection,
    slug: &str,
    status: WorkItemStatus,
    reason: Option<&str>,
) -> anyhow::Result<WorkItem> {
    match status {
        WorkItemStatus::Held => return hold_work_item(connection, slug, reason),
        WorkItemStatus::Pending => {
            let work_item = require_work_item_by_slug(connection, slug)?;
            if work_item.status == WorkItemStatus::Held {
                return resume_work_item(connection, slug, reason);
            }
        }
        WorkItemStatus::Canceled => return cancel_work_item(connection, slug, reason),
        WorkItemStatus::Running | WorkItemStatus::Done => {}
    }
    let work_item = require_work_item_by_slug(connection, slug)?;
    if work_item.status == status {
        return Ok(work_item);
    }
    if status == WorkItemStatus::Done {
        ensure_no_running_runs_for_work_item(connection, &work_item)?;
    }
    if work_item.status == WorkItemStatus::Canceled && status != WorkItemStatus::Canceled {
        bail!("work item {slug} is canceled");
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
                params![status.as_str(), work_item.id],
            )
            .with_context(|| format!("failed to set work item {slug} status"))?;
        let payload = serde_json::json!({
            "status": status.as_str(),
            "reason": reason,
        })
        .to_string();
        let event_type = if status == WorkItemStatus::Done {
            "finish"
        } else {
            "status_set"
        };
        record_event(connection, "work_item", work_item.id, event_type, &payload)?;
        get_work_item_by_id(connection, work_item.id)
    })
}

pub fn delete_work_item(connection: &Connection, slug: &str) -> anyhow::Result<()> {
    let work_item = require_work_item_by_slug(connection, slug)?;
    in_write_transaction(connection, |connection| {
        record_event(connection, "work_item", work_item.id, "delete", "{}")?;
        connection
            .execute("DELETE FROM work_item WHERE id = ?1", params![work_item.id])
            .with_context(|| format!("failed to delete work item {slug}"))?;
        Ok(())
    })
}

pub fn start_run(
    connection: &Connection,
    work_slug: &str,
    command: Option<&str>,
) -> anyhow::Result<InvestigationRun> {
    match claim_pending_run_by_slug(connection, work_slug, command)? {
        Some(claimed) => Ok(claimed.run),
        None => {
            let work_item = require_work_item_by_slug(connection, work_slug)?;
            match work_item.status {
                WorkItemStatus::Pending => {
                    bail!("work item {work_slug} could not be claimed for a run")
                }
                WorkItemStatus::Running => {
                    bail!("work item {work_slug} is already running")
                }
                WorkItemStatus::Held => bail!("work item {work_slug} is held"),
                WorkItemStatus::Done => bail!("work item {work_slug} is already done"),
                WorkItemStatus::Canceled => bail!("work item {work_slug} is canceled"),
            }
        }
    }
}

pub fn claim_next_pending_run(
    connection: &Connection,
    command: Option<&str>,
) -> anyhow::Result<Option<ClaimedRun>> {
    in_write_transaction(connection, |connection| {
        let work_item = connection
            .query_row(
                "UPDATE work_item
                 SET status = 'running', updated_at = datetime('now')
                 WHERE id = (
                     SELECT id
                     FROM work_item
                     WHERE status = 'pending'
                     ORDER BY created_at, id
                     LIMIT 1
                 )
                   AND status = 'pending'
                 RETURNING *",
                [],
                WorkItem::from_row,
            )
            .optional()
            .context("failed to claim next pending work item")?;
        work_item
            .map(|work_item| create_run_for_claimed_work(connection, work_item, command))
            .transpose()
    })
}

fn claim_pending_run_by_slug(
    connection: &Connection,
    work_slug: &str,
    command: Option<&str>,
) -> anyhow::Result<Option<ClaimedRun>> {
    in_write_transaction(connection, |connection| {
        let work_item = connection
            .query_row(
                "UPDATE work_item
                 SET status = 'running', updated_at = datetime('now')
                 WHERE slug = ?1
                   AND status = 'pending'
                 RETURNING *",
                params![work_slug],
                WorkItem::from_row,
            )
            .optional()
            .with_context(|| format!("failed to claim work item {work_slug}"))?;
        work_item
            .map(|work_item| create_run_for_claimed_work(connection, work_item, command))
            .transpose()
    })
}

fn create_run_for_claimed_work(
    connection: &Connection,
    work_item: WorkItem,
    command: Option<&str>,
) -> anyhow::Result<ClaimedRun> {
    connection
        .execute(
            "INSERT INTO run (work_item_id, command) VALUES (?1, ?2)",
            params![work_item.id, command],
        )
        .with_context(|| format!("failed to start run for work item {}", work_item.slug))?;
    let run_id = connection.last_insert_rowid();
    let work_payload = serde_json::json!({ "run_id": run_id }).to_string();
    record_event(
        connection,
        "work_item",
        work_item.id,
        "start_run",
        &work_payload,
    )?;
    let run_payload = serde_json::json!({ "command": command }).to_string();
    record_event(connection, "run", run_id, "start", &run_payload)?;
    let run = get_run_by_id(connection, run_id)?;
    Ok(ClaimedRun { work_item, run })
}

pub fn finish_run(
    connection: &Connection,
    run_id: i64,
    status: RunStatus,
    notes: Option<&str>,
) -> anyhow::Result<InvestigationRun> {
    if status == RunStatus::Running {
        bail!("run finish requires a terminal status");
    }
    let run = get_run_by_id(connection, run_id)?;
    if run.status != RunStatus::Running {
        bail!("run {run_id} is already {}", run.status);
    }
    in_write_transaction(connection, |connection| {
        finish_run_unchecked(connection, run_id, status, notes)
    })
}

fn finish_run_unchecked(
    connection: &Connection,
    run_id: i64,
    status: RunStatus,
    notes: Option<&str>,
) -> anyhow::Result<InvestigationRun> {
    connection
        .execute(
            "UPDATE run
             SET status = ?1, finished_at = datetime('now'), notes = ?2
             WHERE id = ?3",
            params![status.as_str(), notes, run_id],
        )
        .with_context(|| format!("failed to finish run {run_id}"))?;
    let payload = serde_json::json!({
        "status": status.as_str(),
        "notes": notes,
    })
    .to_string();
    record_event(connection, "run", run_id, "finish", &payload)?;
    get_run_by_id(connection, run_id)
}

pub fn close_run(
    connection: &Connection,
    run_id: i64,
    status: RunStatus,
    notes: Option<&str>,
    outcome: DecisionOutcome,
    rationale: &str,
    next_work: Option<NextWorkSpec<'_>>,
) -> anyhow::Result<CloseRunResult> {
    if status == RunStatus::Running {
        bail!("run close requires a terminal status");
    }
    in_write_transaction(connection, |connection| {
        let run = get_run_by_id(connection, run_id)?;
        if run.status != RunStatus::Running {
            bail!("run {run_id} is already {}", run.status);
        }
        let work_item = get_work_item_by_id(connection, run.work_item_id)?;
        ensure_no_other_running_runs_for_work_item(connection, &work_item, run_id)?;
        validate_decision_invariants(connection, &work_item, outcome, next_work.is_some())?;
        let next_work_item_id = match next_work {
            Some(next_work) => Some(resolve_next_work_item_id(
                connection, &work_item, next_work,
            )?),
            None => None,
        };
        let finished = finish_run_unchecked(connection, run_id, status, notes)?;
        let decision = record_decision_unchecked(
            connection,
            &work_item,
            outcome,
            rationale,
            next_work_item_id,
        )?;
        Ok(CloseRunResult {
            run: finished,
            decision,
            work_item,
        })
    })
}

pub fn restore_work_item_pending_after_dry_run(
    connection: &Connection,
    work_slug: &str,
    run_id: i64,
) -> anyhow::Result<WorkItem> {
    let work_item = require_work_item_by_slug(connection, work_slug)?;
    if work_item.status != WorkItemStatus::Running {
        return Ok(work_item);
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item SET status = 'pending', updated_at = datetime('now') WHERE id = ?1",
                params![work_item.id],
            )
            .with_context(|| format!("failed to restore dry-run work item {work_slug} to pending"))?;
        let payload = serde_json::json!({
            "run_id": run_id,
            "reason": "dry-run completed without consuming the work item",
        })
        .to_string();
        record_event(
            connection,
            "work_item",
            work_item.id,
            "dry_run_restore",
            &payload,
        )?;
        get_work_item_by_id(connection, work_item.id)
    })
}

pub fn record_run_phase(
    connection: &Connection,
    run_id: i64,
    phase: &str,
    progress_report: &str,
) -> anyhow::Result<()> {
    ensure_run_exists(connection, run_id)?;
    let payload = serde_json::json!({
        "phase": phase,
        "progress_report": progress_report,
    })
    .to_string();
    record_event(connection, "run", run_id, "phase", &payload)
}

pub fn add_observation(
    connection: &Connection,
    run_id: i64,
    body: &str,
) -> anyhow::Result<Observation> {
    ensure_run_exists(connection, run_id)?;
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO observation (run_id, body) VALUES (?1, ?2)",
                params![run_id, body],
            )
            .with_context(|| format!("failed to add observation to run {run_id}"))?;
        let observation_id = connection.last_insert_rowid();
        record_event(connection, "observation", observation_id, "add", "{}")?;
        get_observation_by_id(connection, observation_id)
    })
}

pub fn add_global_observation(
    connection: &Connection,
    kind: GlobalObservationKind,
    body: &str,
    source: Option<&str>,
) -> anyhow::Result<GlobalObservation> {
    if body.trim().is_empty() {
        bail!("global observation body must not be empty");
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO global_observation (kind, body, source) VALUES (?1, ?2, ?3)",
                params![kind.as_str(), body, source],
            )
            .with_context(|| format!("failed to add global {}", kind.as_str()))?;
        let id = connection.last_insert_rowid();
        let payload = serde_json::json!({
            "kind": kind.as_str(),
            "source": source,
        })
        .to_string();
        record_event(connection, "global_observation", id, "add", &payload)?;
        get_global_observation_by_id(connection, id)
    })
}

pub fn edit_global_observation(
    connection: &Connection,
    id: i64,
    kind: Option<GlobalObservationKind>,
    body: Option<&str>,
    source: Option<Option<&str>>,
    status: Option<GlobalObservationStatus>,
) -> anyhow::Result<GlobalObservation> {
    let global_observation = get_global_observation_by_id(connection, id)?;
    if body.is_some_and(|body| body.trim().is_empty()) {
        bail!("global observation body must not be empty");
    }
    let next_kind = kind.unwrap_or(global_observation.kind);
    let next_body = body.unwrap_or(&global_observation.body);
    let next_source = source.unwrap_or(global_observation.source.as_deref());
    let next_status = status.unwrap_or(global_observation.status);
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE global_observation
                 SET kind = ?1, body = ?2, source = ?3, status = ?4, updated_at = datetime('now')
                 WHERE id = ?5",
                params![
                    next_kind.as_str(),
                    next_body,
                    next_source,
                    next_status.as_str(),
                    id
                ],
            )
            .with_context(|| format!("failed to edit global observation {id}"))?;
        let payload = serde_json::json!({
            "kind": kind.map(|kind| kind.as_str()),
            "body": body,
            "source_changed": source.is_some(),
            "status": status.map(|status| status.as_str()),
        })
        .to_string();
        record_event(connection, "global_observation", id, "edit", &payload)?;
        get_global_observation_by_id(connection, id)
    })
}

pub fn clear_global_observation(
    connection: &Connection,
    id: i64,
    reason: Option<&str>,
) -> anyhow::Result<GlobalObservation> {
    let global_observation = get_global_observation_by_id(connection, id)?;
    if global_observation.status == GlobalObservationStatus::Cleared {
        return Ok(global_observation);
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE global_observation
                 SET status = 'cleared', updated_at = datetime('now')
                 WHERE id = ?1",
                params![id],
            )
            .with_context(|| format!("failed to clear global observation {id}"))?;
        let payload = reason
            .map(|reason| serde_json::json!({ "reason": reason }).to_string())
            .unwrap_or_else(|| "{}".to_owned());
        record_event(connection, "global_observation", id, "clear", &payload)?;
        get_global_observation_by_id(connection, id)
    })
}

pub fn list_global_observations(
    connection: &Connection,
    status: Option<GlobalObservationStatus>,
    limit: i64,
) -> anyhow::Result<Vec<GlobalObservation>> {
    let query = match status {
        Some(_) => {
            "SELECT * FROM global_observation
             WHERE status = ?1
             ORDER BY created_at DESC, id DESC
             LIMIT ?2"
        }
        None => {
            "SELECT * FROM global_observation
             ORDER BY created_at DESC, id DESC
             LIMIT ?1"
        }
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare global observation list query")?;
    let rows = match status {
        Some(status) => statement
            .query_map(params![status.as_str(), limit], GlobalObservation::from_row)
            .context("failed to query global observations")?,
        None => statement
            .query_map(params![limit], GlobalObservation::from_row)
            .context("failed to query global observations")?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read global observations")
}

pub fn add_artifact(
    connection: &Connection,
    artifact_root: &Path,
    run_id: i64,
    kind: ArtifactKind,
    path: &Path,
    description: &str,
) -> anyhow::Result<Artifact> {
    ensure_run_exists(connection, run_id)?;
    let managed_path = managed_artifact_record_path(artifact_root, run_id, path)?;
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO artifact (run_id, kind, path, description)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    run_id,
                    kind.as_str(),
                    managed_path.display().to_string(),
                    description
                ],
            )
            .with_context(|| format!("failed to add artifact to run {run_id}"))?;
        let artifact_id = connection.last_insert_rowid();
        record_event(connection, "artifact", artifact_id, "add", "{}")?;
        get_artifact_by_id(connection, artifact_id)
    })
}

fn managed_artifact_record_path(
    artifact_root: &Path,
    run_id: i64,
    submitted_path: &Path,
) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(artifact_root).with_context(|| {
        format!(
            "failed to create artifact root directory {}",
            artifact_root.display()
        )
    })?;
    let root = artifact_root.canonicalize().with_context(|| {
        format!(
            "failed to resolve artifact root {}",
            artifact_root.display()
        )
    })?;
    let submitted_resolved = resolve_submitted_artifact_path(&root, submitted_path)?;
    if submitted_resolved.starts_with(&root) {
        return submitted_resolved
            .strip_prefix(&root)
            .map(PathBuf::from)
            .with_context(|| {
                format!(
                    "failed to normalize artifact path {} against {}",
                    submitted_path.display(),
                    artifact_root.display()
                )
            });
    }

    let submitted_dir = root.join("submitted");
    fs::create_dir_all(&submitted_dir).with_context(|| {
        format!(
            "failed to create submitted artifact directory {}",
            submitted_dir.display()
        )
    })?;
    let file_name = submitted_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_artifact_file_name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "artifact".to_owned());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_nanos();
    let managed_path = submitted_dir.join(format!("run-{run_id}-{timestamp}-{file_name}"));
    fs::copy(&submitted_resolved, &managed_path).with_context(|| {
        format!(
            "failed to copy artifact {} to {}",
            submitted_path.display(),
            managed_path.display()
        )
    })?;
    managed_path
        .strip_prefix(&root)
        .map(PathBuf::from)
        .with_context(|| {
            format!(
                "failed to normalize managed artifact path {} against {}",
                managed_path.display(),
                artifact_root.display()
            )
        })
}

fn resolve_submitted_artifact_path(root: &Path, submitted_path: &Path) -> anyhow::Result<PathBuf> {
    if submitted_path.is_absolute() {
        return submitted_path
            .canonicalize()
            .with_context(|| format!("failed to resolve artifact {}", submitted_path.display()));
    }

    let cwd_candidate = std::env::current_dir()
        .context("failed to read current directory")?
        .join(submitted_path);
    if cwd_candidate.exists() {
        return cwd_candidate
            .canonicalize()
            .with_context(|| format!("failed to resolve artifact {}", submitted_path.display()));
    }

    root.join(submitted_path)
        .canonicalize()
        .with_context(|| format!("failed to resolve artifact {}", submitted_path.display()))
}

fn sanitize_artifact_file_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

fn validate_decision_invariants(
    connection: &Connection,
    work_item: &WorkItem,
    outcome: DecisionOutcome,
    creates_next_work: bool,
) -> anyhow::Result<()> {
    let other_active_work = count_active_work_items_excluding(connection, work_item.id)?;
    match outcome {
        DecisionOutcome::Stop => {
            if creates_next_work {
                bail!("project completion decisions must not also create next work");
            }
            if other_active_work > 0 {
                bail!("cannot complete project while {other_active_work} other work item(s) are pending, running, or held; stop means the whole project is complete, use continue or inconclusive to finish only this work item");
            }
        }
        DecisionOutcome::Continue | DecisionOutcome::Inconclusive => {
            if !creates_next_work && other_active_work == 0 {
                bail!("continuing requires a next work item; use --next-slug to link existing active work or add --next-title/--next-description to create it; record --outcome stop only when the project is complete");
            }
        }
    }
    Ok(())
}

fn resolve_next_work_item_id(
    connection: &Connection,
    current_work_item: &WorkItem,
    next_work: NextWorkSpec<'_>,
) -> anyhow::Result<i64> {
    let existing = connection
        .query_row(
            "SELECT * FROM work_item WHERE slug = ?1",
            params![next_work.slug],
            WorkItem::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read work item {}", next_work.slug))?;
    if let Some(existing) = existing {
        if existing.id == current_work_item.id {
            bail!("--next-slug must name a different work item");
        }
        match existing.status {
            WorkItemStatus::Pending | WorkItemStatus::Running | WorkItemStatus::Held => {
                return Ok(existing.id);
            }
            WorkItemStatus::Done | WorkItemStatus::Canceled => bail!(
                "work item {} already exists but is {}; next work must be pending, running, or held",
                next_work.slug,
                existing.status.as_str()
            ),
        }
    }

    let (Some(title), Some(description)) = (next_work.title, next_work.description) else {
        bail!(
            "work item {} does not exist; supply --next-title and --next-description to create it",
            next_work.slug
        );
    };
    let created = create_work_item(
        connection,
        Some(current_work_item.id),
        next_work.slug,
        title,
        description,
    )?;
    Ok(created.id)
}

pub fn record_decision(
    connection: &Connection,
    work_slug: &str,
    outcome: DecisionOutcome,
    rationale: &str,
    next_work: Option<NextWorkSpec<'_>>,
) -> anyhow::Result<Decision> {
    in_write_transaction(connection, |connection| {
        let work_item = require_work_item_by_slug(connection, work_slug)?;
        ensure_no_running_runs_for_work_item(connection, &work_item)?;
        validate_decision_invariants(connection, &work_item, outcome, next_work.is_some())?;
        let next_work_item_id = match next_work {
            Some(next_work) => Some(resolve_next_work_item_id(
                connection, &work_item, next_work,
            )?),
            None => None,
        };
        record_decision_unchecked(
            connection,
            &work_item,
            outcome,
            rationale,
            next_work_item_id,
        )
    })
}

fn record_decision_unchecked(
    connection: &Connection,
    work_item: &WorkItem,
    outcome: DecisionOutcome,
    rationale: &str,
    next_work_item_id: Option<i64>,
) -> anyhow::Result<Decision> {
    connection
        .execute(
            "INSERT INTO decision (work_item_id, outcome, rationale, next_work_item_id)
             VALUES (?1, ?2, ?3, ?4)",
            params![work_item.id, outcome.as_str(), rationale, next_work_item_id],
        )
        .with_context(|| format!("failed to record decision for work item {}", work_item.slug))?;
    let decision_id = connection.last_insert_rowid();
    connection
        .execute(
            "UPDATE work_item SET status = 'done', updated_at = datetime('now') WHERE id = ?1",
            params![work_item.id],
        )
        .with_context(|| format!("failed to mark work item {} done", work_item.slug))?;
    record_event(connection, "decision", decision_id, "record", "{}")?;
    record_event(connection, "work_item", work_item.id, "finish", "{}")?;
    get_decision_by_id(connection, decision_id)
}

pub fn next_pending_work_item(connection: &Connection) -> anyhow::Result<Option<WorkItem>> {
    connection
        .query_row(
            "SELECT * FROM work_item
             WHERE status = 'pending'
             ORDER BY created_at, id
             LIMIT 1",
            [],
            WorkItem::from_row,
        )
        .optional()
        .context("failed to read next pending work item")
}

pub fn oldest_running_work_item(connection: &Connection) -> anyhow::Result<Option<WorkItem>> {
    let active_run_work_item = connection
        .query_row(
            "SELECT work_item.*
             FROM run
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE run.status = 'running'
             ORDER BY run.started_at, run.id
             LIMIT 1",
            [],
            WorkItem::from_row,
        )
        .optional()
        .context("failed to read oldest active run work item")?;
    if active_run_work_item.is_some() {
        return Ok(active_run_work_item);
    }

    connection
        .query_row(
            "SELECT * FROM work_item
             WHERE status = 'running'
             ORDER BY updated_at, id
             LIMIT 1",
            [],
            WorkItem::from_row,
        )
        .optional()
        .context("failed to read oldest running work item")
}

fn ensure_no_running_runs_for_work_item(
    connection: &Connection,
    work_item: &WorkItem,
) -> anyhow::Result<()> {
    if let Some(run_id) = running_run_ids_for_work_item(connection, work_item.id)?
        .into_iter()
        .next()
    {
        bail!(
            "work item {} has active run {}; use `ldgr run close {}` to finish the run and record the decision together",
            work_item.slug,
            run_id,
            run_id
        );
    }
    Ok(())
}

fn ensure_no_other_running_runs_for_work_item(
    connection: &Connection,
    work_item: &WorkItem,
    closing_run_id: i64,
) -> anyhow::Result<()> {
    if let Some(run_id) = running_run_ids_for_work_item(connection, work_item.id)?
        .into_iter()
        .find(|run_id| *run_id != closing_run_id)
    {
        bail!(
            "work item {} also has active run {}; finish or close it before closing run {}",
            work_item.slug,
            run_id,
            closing_run_id
        );
    }
    Ok(())
}

pub fn get_work_item_by_slug(connection: &Connection, slug: &str) -> anyhow::Result<WorkItem> {
    require_work_item_by_slug(connection, slug)
}

pub fn list_work_items(
    connection: &Connection,
    status: Option<WorkItemStatus>,
) -> anyhow::Result<Vec<WorkItem>> {
    let query = match status {
        Some(_) => {
            "SELECT * FROM work_item
             WHERE status = ?1
             ORDER BY created_at, id"
        }
        None => {
            "SELECT * FROM work_item
             ORDER BY created_at, id"
        }
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare work item list query")?;
    let rows = match status {
        Some(status) => statement
            .query_map(params![status.as_str()], WorkItem::from_row)
            .context("failed to query work items")?,
        None => statement
            .query_map([], WorkItem::from_row)
            .context("failed to query work items")?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read work items")
}

pub fn get_run(connection: &Connection, run_id: i64) -> anyhow::Result<InvestigationRun> {
    get_run_by_id(connection, run_id)
}

pub fn list_runs(
    connection: &Connection,
    status: Option<RunStatus>,
) -> anyhow::Result<Vec<RunListItem>> {
    let query = match status {
        Some(_) => {
            "SELECT run.id AS run_id,
                    work_item.slug AS work_slug,
                    work_item.title AS work_title,
                    run.command AS command,
                    run.status AS status,
                    run.started_at AS started_at,
                    run.finished_at AS finished_at,
                    run.notes AS notes
             FROM run
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE run.status = ?1
             ORDER BY run.started_at, run.id"
        }
        None => {
            "SELECT run.id AS run_id,
                    work_item.slug AS work_slug,
                    work_item.title AS work_title,
                    run.command AS command,
                    run.status AS status,
                    run.started_at AS started_at,
                    run.finished_at AS finished_at,
                    run.notes AS notes
             FROM run
             JOIN work_item ON work_item.id = run.work_item_id
             ORDER BY run.started_at, run.id"
        }
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare run list query")?;
    let rows = match status {
        Some(status) => statement
            .query_map(params![status.as_str()], run_list_item_from_row)
            .context("failed to query runs")?,
        None => statement
            .query_map([], run_list_item_from_row)
            .context("failed to query runs")?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read runs")
}

pub fn list_observations(
    connection: &Connection,
    run_id: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<ObservationSummary>> {
    if let Some(run_id) = run_id {
        ensure_run_exists(connection, run_id)?;
    }
    let query = match run_id {
        Some(_) => {
            "SELECT observation.id AS observation_id,
                    run.id AS run_id,
                    work_item.slug AS work_slug,
                    observation.body AS body,
                    observation.created_at AS created_at
             FROM observation
             JOIN run ON run.id = observation.run_id
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE run.id = ?1
             ORDER BY observation.created_at DESC, observation.id DESC
             LIMIT ?2"
        }
        None => {
            "SELECT observation.id AS observation_id,
                    run.id AS run_id,
                    work_item.slug AS work_slug,
                    observation.body AS body,
                    observation.created_at AS created_at
             FROM observation
             JOIN run ON run.id = observation.run_id
             JOIN work_item ON work_item.id = run.work_item_id
             ORDER BY observation.created_at DESC, observation.id DESC
             LIMIT ?1"
        }
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare observation list query")?;
    let rows = match run_id {
        Some(run_id) => statement
            .query_map(params![run_id, limit], observation_summary_from_row)
            .context("failed to query observations")?,
        None => statement
            .query_map(params![limit], observation_summary_from_row)
            .context("failed to query observations")?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read observations")
}

pub fn get_artifact(connection: &Connection, artifact_id: i64) -> anyhow::Result<Artifact> {
    get_artifact_by_id(connection, artifact_id)
}

pub fn list_artifacts(
    connection: &Connection,
    run_id: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<ArtifactSummary>> {
    if let Some(run_id) = run_id {
        ensure_run_exists(connection, run_id)?;
    }
    let query = match run_id {
        Some(_) => {
            "SELECT artifact.id AS artifact_id,
                    run.id AS run_id,
                    work_item.slug AS work_slug,
                    artifact.kind AS kind,
                    artifact.path AS path,
                    artifact.description AS description,
                    artifact.created_at AS created_at
             FROM artifact
             JOIN run ON run.id = artifact.run_id
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE run.id = ?1
             ORDER BY artifact.created_at DESC, artifact.id DESC
             LIMIT ?2"
        }
        None => {
            "SELECT artifact.id AS artifact_id,
                    run.id AS run_id,
                    work_item.slug AS work_slug,
                    artifact.kind AS kind,
                    artifact.path AS path,
                    artifact.description AS description,
                    artifact.created_at AS created_at
             FROM artifact
             JOIN run ON run.id = artifact.run_id
             JOIN work_item ON work_item.id = run.work_item_id
             ORDER BY artifact.created_at DESC, artifact.id DESC
             LIMIT ?1"
        }
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare artifact list query")?;
    let rows = match run_id {
        Some(run_id) => statement
            .query_map(params![run_id, limit], artifact_summary_from_row)
            .context("failed to query artifacts")?,
        None => statement
            .query_map(params![limit], artifact_summary_from_row)
            .context("failed to query artifacts")?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read artifacts")
}

pub fn list_decisions(
    connection: &Connection,
    work_slug: Option<&str>,
    limit: i64,
) -> anyhow::Result<Vec<DecisionSummary>> {
    if let Some(work_slug) = work_slug {
        require_work_item_by_slug(connection, work_slug)?;
    }
    let query = match work_slug {
        Some(_) => {
            "SELECT decision.id AS decision_id,
                    work_item.slug AS work_slug,
                    decision.outcome AS outcome,
                    decision.rationale AS rationale,
                    next_work_item.slug AS next_work_slug,
                    decision.created_at AS created_at
             FROM decision
             JOIN work_item ON work_item.id = decision.work_item_id
             LEFT JOIN work_item AS next_work_item ON next_work_item.id = decision.next_work_item_id
             WHERE work_item.slug = ?1
             ORDER BY decision.created_at DESC, decision.id DESC
             LIMIT ?2"
        }
        None => {
            "SELECT decision.id AS decision_id,
                    work_item.slug AS work_slug,
                    decision.outcome AS outcome,
                    decision.rationale AS rationale,
                    next_work_item.slug AS next_work_slug,
                    decision.created_at AS created_at
             FROM decision
             JOIN work_item ON work_item.id = decision.work_item_id
             LEFT JOIN work_item AS next_work_item ON next_work_item.id = decision.next_work_item_id
             ORDER BY decision.created_at DESC, decision.id DESC
             LIMIT ?1"
        }
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare decision list query")?;
    let rows = match work_slug {
        Some(work_slug) => statement
            .query_map(params![work_slug, limit], decision_summary_from_row)
            .context("failed to query decisions")?,
        None => statement
            .query_map(params![limit], decision_summary_from_row)
            .context("failed to query decisions")?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read decisions")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    fn temp_store() -> anyhow::Result<(TempDir, Connection)> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        Ok((temp, connection))
    }

    #[test]
    fn add_observation_rolls_back_when_event_recording_fails() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(
            &connection,
            None,
            "atomic-observation",
            "Atomic observation",
            "Observation insert and event recording must commit together.",
        )?;
        let run = start_run(&connection, "atomic-observation", Some("manual"))?;
        connection.execute_batch(
            "CREATE TRIGGER fail_observation_event
             BEFORE INSERT ON event_log
             WHEN NEW.entity_type = 'observation'
             BEGIN
                 SELECT RAISE(ABORT, 'blocked observation event');
             END;",
        )?;

        let error = add_observation(&connection, run.id, "must roll back").unwrap_err();

        assert!(
            format!("{error:#}").contains("blocked observation event"),
            "{error:#}"
        );
        let observation_count: i64 = connection.query_row(
            "SELECT count(*) FROM observation WHERE run_id = ?1",
            params![run.id],
            |row| row.get(0),
        )?;
        assert_eq!(observation_count, 0);
        Ok(())
    }

    #[test]
    fn concurrent_manual_and_loop_claims_create_one_run_for_pending_work() -> anyhow::Result<()> {
        let (temp, connection) = temp_store()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        create_work_item(
            &connection,
            None,
            "claim-race",
            "Claim race",
            "Only one concurrent claimant should start this work.",
        )?;
        drop(connection);

        let claimant_count = 12;
        let barrier = Arc::new(Barrier::new(claimant_count));
        let mut handles = Vec::new();
        for index in 0..claimant_count {
            let db_path = db_path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || -> anyhow::Result<Option<i64>> {
                let connection = open_store(&db_path)?;
                barrier.wait();
                if index % 2 == 0 {
                    match start_run(&connection, "claim-race", Some("manual")) {
                        Ok(run) => Ok(Some(run.id)),
                        Err(_) => Ok(None),
                    }
                } else {
                    Ok(claim_next_pending_run(&connection, Some("loop"))?
                        .map(|claimed| claimed.run.id))
                }
            }));
        }

        let mut claimed_run_ids = Vec::new();
        for handle in handles {
            if let Some(run_id) = handle.join().expect("claim thread panicked")? {
                claimed_run_ids.push(run_id);
            }
        }

        let connection = open_store(&db_path)?;
        let runs = list_runs(&connection, None)?;
        assert_eq!(claimed_run_ids.len(), 1, "{claimed_run_ids:?}");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, claimed_run_ids[0]);
        assert_eq!(
            get_work_item_by_slug(&connection, "claim-race")?.status,
            WorkItemStatus::Running
        );

        Ok(())
    }

    #[test]
    fn continuing_without_next_work_is_blocked_when_no_other_work_exists() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;

        let error = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "more remains",
            None,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("continuing requires a next work item"));
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Pending
        );

        Ok(())
    }

    #[test]
    fn continuing_with_next_work_finishes_current_and_creates_child() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        let current = create_work_item(&connection, None, "current", "Current", "Current work")?;

        let decision = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "queue next",
            Some(NextWorkSpec {
                slug: "next",
                title: Some("Next"),
                description: Some("Next work"),
            }),
        )?;

        let next = get_work_item_by_slug(&connection, "next")?;
        assert_eq!(decision.next_work_item_id, Some(next.id));
        assert_eq!(next.parent_work_item_id, Some(current.id));
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Done
        );

        Ok(())
    }

    #[test]
    fn concurrent_decisions_share_next_work_without_duplicate_slug_race() -> anyhow::Result<()> {
        let (temp, connection) = temp_store()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        let decider_count = 8;
        for index in 0..decider_count {
            create_work_item(
                &connection,
                None,
                &format!("current-{index}"),
                &format!("Current {index}"),
                "Concurrent current work",
            )?;
        }
        drop(connection);

        let barrier = Arc::new(Barrier::new(decider_count));
        let mut handles = Vec::new();
        for index in 0..decider_count {
            let db_path = db_path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || -> anyhow::Result<i64> {
                let connection = open_store(&db_path)?;
                barrier.wait();
                let decision = record_decision(
                    &connection,
                    &format!("current-{index}"),
                    DecisionOutcome::Continue,
                    "share the same next slice",
                    Some(NextWorkSpec {
                        slug: "shared-next",
                        title: Some("Shared next"),
                        description: Some("Only one work item should be created."),
                    }),
                )?;
                decision
                    .next_work_item_id
                    .context("continue decision should link next work")
            }));
        }

        let mut next_ids = Vec::new();
        for handle in handles {
            next_ids.push(handle.join().expect("decision thread panicked")?);
        }

        let connection = open_store(&db_path)?;
        let shared_next = get_work_item_by_slug(&connection, "shared-next")?;
        assert!(next_ids.iter().all(|id| *id == shared_next.id));
        let shared_next_count: i64 = connection.query_row(
            "SELECT count(*) FROM work_item WHERE slug = 'shared-next'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(shared_next_count, 1);

        Ok(())
    }

    #[test]
    fn continuing_can_link_existing_next_work() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;
        let existing = create_work_item(&connection, None, "next", "Next", "Next work")?;

        let decision = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "link existing next",
            Some(NextWorkSpec {
                slug: "next",
                title: None,
                description: None,
            }),
        )?;

        assert_eq!(decision.next_work_item_id, Some(existing.id));
        assert_eq!(get_work_item_by_slug(&connection, "next")?.id, existing.id);
        assert_eq!(list_work_items(&connection, None)?.len(), 2);

        Ok(())
    }

    #[test]
    fn missing_existing_next_requires_create_details() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;

        let error = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "missing details",
            Some(NextWorkSpec {
                slug: "missing",
                title: None,
                description: None,
            }),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("supply --next-title and --next-description"));
        assert_eq!(list_work_items(&connection, None)?.len(), 1);

        Ok(())
    }

    #[test]
    fn close_run_invalid_continue_without_next_leaves_run_and_work_unchanged() -> anyhow::Result<()>
    {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;
        let run = start_run(&connection, "current", Some("cargo test"))?;

        let error = close_run(
            &connection,
            run.id,
            RunStatus::Success,
            Some("should not persist"),
            DecisionOutcome::Continue,
            "more remains",
            None,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("continuing requires a next work item"));
        let unchanged_run = get_run_by_id(&connection, run.id)?;
        assert_eq!(unchanged_run.status, RunStatus::Running);
        assert_eq!(unchanged_run.finished_at, None);
        assert_eq!(unchanged_run.notes, None);
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Running
        );
        assert!(list_decisions(&connection, None, 10)?.is_empty());

        Ok(())
    }

    #[test]
    fn close_run_invalid_next_work_leaves_run_and_work_unchanged() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;
        let run = start_run(&connection, "current", Some("cargo test"))?;

        let error = close_run(
            &connection,
            run.id,
            RunStatus::Success,
            Some("should not persist"),
            DecisionOutcome::Continue,
            "queue missing next",
            Some(NextWorkSpec {
                slug: "missing",
                title: None,
                description: None,
            }),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("supply --next-title and --next-description"));
        let unchanged_run = get_run_by_id(&connection, run.id)?;
        assert_eq!(unchanged_run.status, RunStatus::Running);
        assert_eq!(unchanged_run.finished_at, None);
        assert_eq!(unchanged_run.notes, None);
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Running
        );
        assert!(list_decisions(&connection, None, 10)?.is_empty());
        assert_eq!(list_work_items(&connection, None)?.len(), 1);

        Ok(())
    }

    #[test]
    fn managed_artifact_record_path_keeps_artifacts_inside_root() -> anyhow::Result<()> {
        let (temp, connection) = temp_store()?;
        create_work_item(
            &connection,
            None,
            "artifact-work",
            "Artifacts",
            "Record artifacts",
        )?;
        let run = start_run(&connection, "artifact-work", Some("test"))?;
        let artifact_root = temp.path().join("artifacts");
        fs::create_dir_all(&artifact_root)?;

        let internal_path = artifact_root.join("report.md");
        fs::write(&internal_path, "inside")?;
        let internal = add_artifact(
            &connection,
            &artifact_root,
            run.id,
            ArtifactKind::Report,
            &internal_path,
            "internal",
        )?;
        assert_eq!(internal.path, PathBuf::from("report.md"));

        let external_path = temp.path().join("external report?.md");
        fs::write(&external_path, "outside")?;
        let external = add_artifact(
            &connection,
            &artifact_root,
            run.id,
            ArtifactKind::Report,
            &external_path,
            "external",
        )?;

        assert!(external.path.starts_with("submitted"));
        assert!(external.path.to_string_lossy().contains("submitted"));
        assert!(external
            .path
            .to_string_lossy()
            .ends_with("external_report_.md"));
        assert_eq!(
            fs::read_to_string(artifact_root.join(external.path))?,
            "outside"
        );

        Ok(())
    }
}
