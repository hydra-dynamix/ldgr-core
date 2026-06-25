use super::*;

const CONTEXT_ACTIVE_RUN_LIMIT: i64 = 5;
const CONTEXT_LATEST_OBSERVATION_LIMIT: i64 = 3;
const CONTEXT_GLOBAL_OBSERVATION_LIMIT: i64 = 10;
const CONTEXT_LATEST_ARTIFACT_LIMIT: i64 = 3;
const CONTEXT_LATEST_VALIDATION_LIMIT: i64 = 3;
const CONTEXT_LOOP_INTERVENTION_LIMIT: i64 = 10;
const CONTEXT_LATEST_EVENT_LIMIT: i64 = 10;
const CONTEXT_RUN_NARRATIVE_LIMIT: i64 = 6;

pub fn read_context(connection: &Connection) -> anyhow::Result<StoreContext> {
    Ok(StoreContext {
        pending_work_items: count_work_items_by_status(connection, WorkItemStatus::Pending)?,
        running_work_items: count_work_items_by_status(connection, WorkItemStatus::Running)?,
        held_work_items: count_work_items_by_status(connection, WorkItemStatus::Held)?,
        done_work_items: count_work_items_by_status(connection, WorkItemStatus::Done)?,
        canceled_work_items: count_work_items_by_status(connection, WorkItemStatus::Canceled)?,
        loop_state: read_loop_state(connection)?,
        active_runs: list_active_runs(connection, CONTEXT_ACTIVE_RUN_LIMIT)?,
        next_work_item: next_pending_work_item(connection)?,
        latest_decision: latest_decision(connection)?,
        latest_observations: latest_observations(connection, CONTEXT_LATEST_OBSERVATION_LIMIT)?,
        latest_validations: list_validation_records(
            connection,
            None,
            CONTEXT_LATEST_VALIDATION_LIMIT,
        )?,
        global_observations: list_global_observations(
            connection,
            Some(GlobalObservationStatus::Active),
            CONTEXT_GLOBAL_OBSERVATION_LIMIT,
        )?,
        latest_artifacts: latest_artifacts(connection, CONTEXT_LATEST_ARTIFACT_LIMIT)?,
        loop_interventions: list_loop_interventions(connection, CONTEXT_LOOP_INTERVENTION_LIMIT)?,
        latest_events: latest_events(connection, CONTEXT_LATEST_EVENT_LIMIT)?,
    })
}

pub fn request_loop_intervention(
    connection: &Connection,
    action: LoopInterventionAction,
    reason: &str,
    instruction: Option<&str>,
    requested_by: Option<&str>,
) -> anyhow::Result<LoopIntervention> {
    if reason.trim().is_empty() {
        bail!("loop intervention reason must not be empty");
    }
    if action == LoopInterventionAction::Steer
        && instruction.map(str::trim).unwrap_or("").is_empty()
    {
        bail!("steer intervention requires --instruction");
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO loop_intervention (action, reason, instruction, requested_by)
                 VALUES (?1, ?2, ?3, ?4)",
                params![action.as_str(), reason, instruction, requested_by],
            )
            .with_context(|| format!("failed to request loop {} intervention", action.as_str()))?;
        let intervention_id = connection.last_insert_rowid();
        let payload = serde_json::json!({
            "action": action.as_str(),
            "reason": reason,
            "instruction": instruction,
            "requested_by": requested_by,
        })
        .to_string();
        record_event(
            connection,
            "loop_intervention",
            intervention_id,
            "request",
            &payload,
        )?;
        get_loop_intervention_by_id(connection, intervention_id)
    })
}

pub fn clear_loop_intervention(
    connection: &Connection,
    intervention_id: i64,
    reason: Option<&str>,
) -> anyhow::Result<LoopIntervention> {
    let intervention = get_loop_intervention_by_id(connection, intervention_id)?;
    if intervention.status != LoopInterventionStatus::Pending {
        return Ok(intervention);
    }
    clear_pending_loop_intervention(connection, intervention_id, "clear", reason)?;
    get_loop_intervention_by_id(connection, intervention_id)
}

pub fn resume_loop(connection: &Connection, reason: &str) -> anyhow::Result<Vec<LoopIntervention>> {
    if reason.trim().is_empty() {
        bail!("resume reason must not be empty");
    }
    let paused = pending_loop_interventions(connection)?
        .into_iter()
        .filter(|intervention| intervention.action == LoopInterventionAction::Pause)
        .collect::<Vec<_>>();
    let mut resumed = Vec::new();
    for intervention in paused {
        clear_pending_loop_intervention(connection, intervention.id, "resume", Some(reason))?;
        resumed.push(get_loop_intervention_by_id(connection, intervention.id)?);
    }
    Ok(resumed)
}

fn clear_pending_loop_intervention(
    connection: &Connection,
    intervention_id: i64,
    event_type: &str,
    reason: Option<&str>,
) -> anyhow::Result<()> {
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE loop_intervention SET status = 'cleared', updated_at = datetime('now') WHERE id = ?1",
                params![intervention_id],
            )
            .with_context(|| format!("failed to clear loop intervention {intervention_id}"))?;
        let payload = reason
            .map(|reason| serde_json::json!({ "reason": reason }).to_string())
            .unwrap_or_else(|| "{}".to_owned());
        record_event(
            connection,
            "loop_intervention",
            intervention_id,
            event_type,
            &payload,
        )
    })
}

pub fn apply_loop_intervention(
    connection: &Connection,
    intervention_id: i64,
    run_id: Option<i64>,
) -> anyhow::Result<LoopIntervention> {
    if let Some(run_id) = run_id {
        ensure_run_exists(connection, run_id)?;
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE loop_intervention
                 SET status = 'applied', applied_run_id = ?2, updated_at = datetime('now')
                 WHERE id = ?1 AND status = 'pending'",
                params![intervention_id, run_id],
            )
            .with_context(|| format!("failed to apply loop intervention {intervention_id}"))?;
        let payload = serde_json::json!({ "run_id": run_id }).to_string();
        record_event(
            connection,
            "loop_intervention",
            intervention_id,
            "apply",
            &payload,
        )?;
        get_loop_intervention_by_id(connection, intervention_id)
    })
}

pub fn pending_loop_interventions(
    connection: &Connection,
) -> anyhow::Result<Vec<LoopIntervention>> {
    list_loop_interventions_by_status(connection, Some(LoopInterventionStatus::Pending), 100)
}

pub fn list_loop_interventions(
    connection: &Connection,
    limit: i64,
) -> anyhow::Result<Vec<LoopIntervention>> {
    list_loop_interventions_by_status(connection, None, limit)
}

fn list_loop_interventions_by_status(
    connection: &Connection,
    status: Option<LoopInterventionStatus>,
    limit: i64,
) -> anyhow::Result<Vec<LoopIntervention>> {
    let query = match status {
        Some(_) => "SELECT * FROM loop_intervention WHERE status = ?1 ORDER BY created_at DESC, id DESC LIMIT ?2",
        None => "SELECT * FROM loop_intervention ORDER BY created_at DESC, id DESC LIMIT ?1",
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare loop intervention list query")?;
    let rows = match status {
        Some(status) => {
            statement.query_map(params![status.as_str(), limit], LoopIntervention::from_row)?
        }
        None => statement.query_map(params![limit], LoopIntervention::from_row)?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read loop interventions")
}

fn read_loop_state(connection: &Connection) -> anyhow::Result<LoopStateSummary> {
    let next_work_item = next_pending_work_item(connection)?;
    let latest_run = latest_run_context(connection)?;
    let Some(run) = latest_run else {
        let progress_report = match next_work_item {
            Some(work_item) => format!("Ready to start next work item {}.", work_item.slug),
            None => "No loop run has started and no pending work items remain.".to_owned(),
        };
        return Ok(LoopStateSummary {
            run_id: None,
            work_slug: None,
            work_title: None,
            current_phase: "idle".to_owned(),
            progress_report,
            command: None,
            started_at: None,
            finished_at: None,
            terminal_status: None,
            recent_cycle_narrative: Vec::new(),
        });
    };

    let latest_event = latest_run_lifecycle_event(connection, run.run_id)?;
    let (current_phase, progress_report) = if run.status != RunStatus::Running
        && run.work_status == WorkItemStatus::Running
    {
        (
            "needs_decision".to_owned(),
            format!(
                "Run {} for {} finished with terminal status {}; record a decision to close the work item.",
                run.run_id,
                run.work_slug,
                run.status.as_str()
            ),
        )
    } else {
        match latest_event.as_ref() {
            Some(event) => current_phase_and_progress_from_event(event, &run),
            None => (
                default_phase_for_run_status(run.status).to_owned(),
                default_progress_for_run(&run),
            ),
        }
    };

    let recent_cycle_narrative = recent_run_narrative(
        connection,
        run.run_id,
        &run.work_slug,
        CONTEXT_RUN_NARRATIVE_LIMIT,
    )?;

    Ok(LoopStateSummary {
        run_id: Some(run.run_id),
        work_slug: Some(run.work_slug),
        work_title: Some(run.work_title),
        current_phase,
        progress_report,
        command: run.command,
        started_at: Some(run.started_at),
        finished_at: run.finished_at,
        terminal_status: (run.status != RunStatus::Running).then_some(run.status),
        recent_cycle_narrative,
    })
}

fn list_active_runs(connection: &Connection, limit: i64) -> anyhow::Result<Vec<RunSummary>> {
    let mut statement = connection
        .prepare(
            "SELECT run.id AS run_id,
                    work_item.slug AS work_slug,
                    work_item.title AS work_title,
                    run.command AS command,
                    run.started_at AS started_at
             FROM run
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE run.status = 'running'
             ORDER BY run.started_at, run.id
             LIMIT ?1",
        )
        .context("failed to prepare active run query")?;
    let rows = statement
        .query_map(params![limit], |row| {
            Ok(RunSummary {
                run_id: row.get("run_id")?,
                work_slug: row.get("work_slug")?,
                work_title: row.get("work_title")?,
                command: row.get("command")?,
                started_at: row.get("started_at")?,
            })
        })
        .context("failed to query active runs")?;
    let active_runs = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read active runs")?;
    Ok(active_runs)
}

pub(crate) fn run_list_item_from_row(row: &Row<'_>) -> rusqlite::Result<RunListItem> {
    let status_text: String = row.get("status")?;
    let status = RunStatus::from_str(&status_text).map_err(parse_error_to_sql_error)?;
    Ok(RunListItem {
        run_id: row.get("run_id")?,
        work_slug: row.get("work_slug")?,
        work_title: row.get("work_title")?,
        command: row.get("command")?,
        status,
        started_at: row.get("started_at")?,
        finished_at: row.get("finished_at")?,
        notes: row.get("notes")?,
    })
}

fn latest_decision(connection: &Connection) -> anyhow::Result<Option<DecisionSummary>> {
    connection
        .query_row(
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
             LIMIT 1",
            [],
            decision_summary_from_row,
        )
        .optional()
        .context("failed to read latest decision")
}

pub(crate) fn decision_summary_from_row(row: &Row<'_>) -> rusqlite::Result<DecisionSummary> {
    let outcome_text: String = row.get("outcome")?;
    let outcome = DecisionOutcome::from_str(&outcome_text).map_err(parse_error_to_sql_error)?;
    Ok(DecisionSummary {
        decision_id: row.get("decision_id")?,
        work_slug: row.get("work_slug")?,
        outcome,
        rationale: row.get("rationale")?,
        next_work_slug: row.get("next_work_slug")?,
        created_at: row.get("created_at")?,
    })
}

fn latest_observations(
    connection: &Connection,
    limit: i64,
) -> anyhow::Result<Vec<ObservationSummary>> {
    let mut statement = connection
        .prepare(
            "SELECT observation.id AS observation_id,
                    run.id AS run_id,
                    work_item.slug AS work_slug,
                    observation.body AS body,
                    observation.created_at AS created_at
             FROM observation
             JOIN run ON run.id = observation.run_id
             JOIN work_item ON work_item.id = run.work_item_id
             ORDER BY observation.created_at DESC, observation.id DESC
             LIMIT ?1",
        )
        .context("failed to prepare latest observation query")?;
    let rows = statement
        .query_map(params![limit], observation_summary_from_row)
        .context("failed to query latest observations")?;
    let observations = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read latest observations")?;
    Ok(observations)
}

pub(crate) fn observation_summary_from_row(row: &Row<'_>) -> rusqlite::Result<ObservationSummary> {
    Ok(ObservationSummary {
        observation_id: row.get("observation_id")?,
        run_id: row.get("run_id")?,
        work_slug: row.get("work_slug")?,
        body: row.get("body")?,
        created_at: row.get("created_at")?,
    })
}

#[derive(Debug, Clone)]
struct LatestRunContext {
    run_id: i64,
    work_slug: String,
    work_title: String,
    work_status: WorkItemStatus,
    command: Option<String>,
    status: RunStatus,
    started_at: String,
    finished_at: Option<String>,
}

fn latest_run_context(connection: &Connection) -> anyhow::Result<Option<LatestRunContext>> {
    connection
        .query_row(
            "SELECT run.id AS run_id,
                    work_item.slug AS work_slug,
                    work_item.title AS work_title,
                    work_item.status AS work_status,
                    run.command AS command,
                    run.status AS status,
                    run.started_at AS started_at,
                    run.finished_at AS finished_at
             FROM run
             JOIN work_item ON work_item.id = run.work_item_id
             ORDER BY run.started_at DESC, run.id DESC
             LIMIT 1",
            [],
            |row| {
                let status_text: String = row.get("status")?;
                let status = RunStatus::from_str(&status_text).map_err(parse_error_to_sql_error)?;
                let work_status_text: String = row.get("work_status")?;
                let work_status = WorkItemStatus::from_str(&work_status_text)
                    .map_err(parse_error_to_sql_error)?;
                Ok(LatestRunContext {
                    run_id: row.get("run_id")?,
                    work_slug: row.get("work_slug")?,
                    work_title: row.get("work_title")?,
                    work_status,
                    command: row.get("command")?,
                    status,
                    started_at: row.get("started_at")?,
                    finished_at: row.get("finished_at")?,
                })
            },
        )
        .optional()
        .context("failed to read latest run context")
}

fn latest_run_lifecycle_event(
    connection: &Connection,
    run_id: i64,
) -> anyhow::Result<Option<EventLogSummary>> {
    connection
        .query_row(
            "SELECT id AS event_id,
                    entity_type,
                    entity_id,
                    event_type,
                    payload_json,
                    created_at
             FROM event_log
             WHERE entity_type = 'run'
               AND entity_id = ?1
               AND event_type IN ('start', 'phase', 'finish')
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            params![run_id],
            |row| {
                Ok(EventLogSummary {
                    event_id: row.get("event_id")?,
                    entity_type: row.get("entity_type")?,
                    entity_id: row.get("entity_id")?,
                    event_type: row.get("event_type")?,
                    payload_json: row.get("payload_json")?,
                    created_at: row.get("created_at")?,
                })
            },
        )
        .optional()
        .context("failed to read latest run lifecycle event")
}

fn recent_run_narrative(
    connection: &Connection,
    run_id: i64,
    work_slug: &str,
    limit: i64,
) -> anyhow::Result<Vec<LoopNarrativeEntry>> {
    let mut statement = connection
        .prepare(
            "SELECT id AS event_id,
                    entity_type,
                    entity_id,
                    event_type,
                    payload_json,
                    created_at
             FROM event_log
             WHERE entity_type = 'run'
               AND entity_id = ?1
               AND event_type IN ('start', 'phase', 'finish')
             ORDER BY created_at DESC, id DESC
             LIMIT ?2",
        )
        .context("failed to prepare run narrative query")?;
    let rows = statement
        .query_map(params![run_id, limit], |row| {
            Ok(EventLogSummary {
                event_id: row.get("event_id")?,
                entity_type: row.get("entity_type")?,
                entity_id: row.get("entity_id")?,
                event_type: row.get("event_type")?,
                payload_json: row.get("payload_json")?,
                created_at: row.get("created_at")?,
            })
        })
        .context("failed to query run narrative")?;
    let mut events = rows
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read run narrative")?;
    events.reverse();
    events
        .iter()
        .map(|event| loop_narrative_entry_from_event(event, work_slug))
        .collect::<anyhow::Result<Vec<_>>>()
}

fn current_phase_and_progress_from_event(
    event: &EventLogSummary,
    run: &LatestRunContext,
) -> (String, String) {
    let payload = parse_event_payload(&event.payload_json);
    match event.event_type.as_str() {
        "phase" => {
            let phase = payload
                .get("phase")
                .and_then(|value| value.as_str())
                .unwrap_or_else(|| default_phase_for_run_status(run.status))
                .to_owned();
            let progress_report = payload
                .get("progress_report")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| default_progress_for_run(run));
            (phase, progress_report)
        }
        "finish" => (
            "completed".to_owned(),
            finish_message(&payload, run.status, run.work_slug.as_str()),
        ),
        "start" => ("started".to_owned(), default_progress_for_run(run)),
        _ => (
            default_phase_for_run_status(run.status).to_owned(),
            default_progress_for_run(run),
        ),
    }
}

fn loop_narrative_entry_from_event(
    event: &EventLogSummary,
    work_slug: &str,
) -> anyhow::Result<LoopNarrativeEntry> {
    let payload = parse_event_payload(&event.payload_json);
    let (phase, message) = match event.event_type.as_str() {
        "phase" => {
            let phase = payload
                .get("phase")
                .and_then(|value| value.as_str())
                .map(str::to_owned);
            let message = payload
                .get("progress_report")
                .and_then(|value| value.as_str())
                .map(str::to_owned)
                .unwrap_or_else(|| "Updated loop progress.".to_owned());
            (phase, message)
        }
        "finish" => (
            Some("completed".to_owned()),
            finish_message(
                &payload,
                payload
                    .get("status")
                    .and_then(|value| value.as_str())
                    .and_then(|value| RunStatus::from_str(value).ok())
                    .unwrap_or(RunStatus::Partial),
                work_slug,
            ),
        ),
        "start" => (Some("started".to_owned()), "Started loop cycle.".to_owned()),
        _ => (None, "Recorded loop lifecycle event.".to_owned()),
    };
    Ok(LoopNarrativeEntry {
        created_at: event.created_at.clone(),
        phase,
        message,
    })
}

fn finish_message(payload: &serde_json::Value, status: RunStatus, work_slug: &str) -> String {
    let base = format!(
        "Run for {work_slug} finished with terminal status {}.",
        status.as_str()
    );
    match payload.get("notes").and_then(|value| value.as_str()) {
        Some(notes) if !notes.trim().is_empty() => format!("{base} {notes}"),
        _ => base,
    }
}

fn default_phase_for_run_status(status: RunStatus) -> &'static str {
    match status {
        RunStatus::Running => "running",
        RunStatus::Success | RunStatus::Failed | RunStatus::Partial => "completed",
    }
}

fn default_progress_for_run(run: &LatestRunContext) -> String {
    match run.status {
        RunStatus::Running => format!("Run {} for {} is in progress.", run.run_id, run.work_slug),
        RunStatus::Success | RunStatus::Failed | RunStatus::Partial => format!(
            "Run {} for {} finished with terminal status {}.",
            run.run_id,
            run.work_slug,
            run.status.as_str()
        ),
    }
}

fn parse_event_payload(payload_json: &str) -> serde_json::Value {
    serde_json::from_str(payload_json).unwrap_or_else(|_| serde_json::json!({}))
}
