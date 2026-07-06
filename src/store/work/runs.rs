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
    enforce_role_run_closure_authority(run_id)?;
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
    enforce_role_run_closure_authority(run_id)?;
    enforce_loop_stop_authority(outcome)?;
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

fn enforce_loop_stop_authority(outcome: DecisionOutcome) -> anyhow::Result<()> {
    if outcome != DecisionOutcome::Stop {
        return Ok(());
    }
    if std::env::var("LDGR_LOOP_STOP_AUTHORITY").ok().as_deref() != Some("planner") {
        return Ok(());
    }
    // Single-agent loop subprocesses may close their assigned run when the
    // runtime explicitly exports LDGR_LOOP_MAY_CLOSE_RUN=1.
    if std::env::var("LDGR_LOOP_MAY_CLOSE_RUN").ok().as_deref() == Some("1") {
        return Ok(());
    }
    let role = std::env::var("LDGR_LOOP_ROLE").unwrap_or_else(|_| "unknown".to_owned());
    if role == "planner" {
        return Ok(());
    }
    bail!(
        "loop stop decisions require explicit loop closure authority; role {role} may record recommendations but cannot close with outcome stop"
    )
}

/// Compatibility guard for older role-based loop subprocesses. Current loop
/// runs export `LDGR_LOOP_MAY_CLOSE_RUN=1` to the single assigned agent, so this
/// guard is normally inactive. It remains to prevent stale role wrappers from
/// closing an assigned run unless they were explicitly granted closure authority.
fn enforce_role_run_closure_authority(run_id: i64) -> anyhow::Result<()> {
    let role = match std::env::var("LDGR_LOOP_ROLE") {
        Ok(role) => role,
        Err(_) => return Ok(()),
    };
    // The final role is explicitly authorized to close the assigned run.
    if std::env::var("LDGR_LOOP_MAY_CLOSE_RUN").ok().as_deref() == Some("1") {
        return Ok(());
    }
    // Only block closure of the assigned run; closures of unrelated runs
    // (which should not occur from a role) are left to other invariants.
    if let Ok(assigned) = std::env::var("LDGR_LOOP_ASSIGNED_RUN_ID") {
        if let Ok(assigned_id) = assigned.parse::<i64>() {
            if assigned_id == run_id {
                bail!(
                    "role {role} may not close run {run_id}; this stale role wrapper lacks explicit loop closure authority. Record observations/artifacts or run the current single-agent loop instead."
                );
            }
        }
    }
    Ok(())
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

