// Core work-state instrumentation deliberately reconstructs only committed
// lifecycle codes from event types, run statuses, decision outcomes, and
// validation outcomes. It does not query work titles, descriptions,
// observations, artifacts, commands, notes, rationales, paths, or timestamps.
use crate::telemetry::buffer::queue_committed_terminal_sequence;
use crate::telemetry::command_experience::{
    record_private_episode, ArtifactRole, PrivateEpisodeProjection, ValidationBucket,
};
use crate::telemetry::transition::{
    StateCode, CANCELLED, COMPLETED_INCONCLUSIVE, COMPLETED_NEGATIVE, COMPLETED_POSITIVE,
    CORE_WORK_V1, HELD, OPERATIONAL_FAILURE, PENDING, RUNNING,
};

fn best_effort_queue_core_work_run_terminal(
    connection: &Connection,
    run_id: i64,
    run_status: RunStatus,
    decision_outcome: Option<DecisionOutcome>,
) {
    let _ = queue_core_work_run_terminal(connection, run_id, run_status, decision_outcome);
}

fn best_effort_queue_core_work_completion(
    connection: &Connection,
    work_item_id: i64,
    decision_outcome: Option<DecisionOutcome>,
) {
    let _ = queue_core_work_completion(connection, work_item_id, decision_outcome);
}

fn best_effort_queue_core_work_cancellation(connection: &Connection, work_item_id: i64) {
    let _ = queue_core_work_item_terminal(connection, work_item_id, "cancel", CANCELLED);
}

fn queue_core_work_run_terminal(
    connection: &Connection,
    run_id: i64,
    run_status: RunStatus,
    decision_outcome: Option<DecisionOutcome>,
) -> anyhow::Result<()> {
    let Some((work_item_id, terminal_event_id)) = run_finish_context(connection, run_id)? else {
        return Ok(());
    };
    let terminal = core_terminal_for_run(connection, run_id, run_status, decision_outcome)?;
    best_effort_record_command_experience(connection, run_id, terminal);
    queue_core_work_terminal_before_event(connection, work_item_id, terminal_event_id, terminal)
}

fn queue_core_work_completion(
    connection: &Connection,
    work_item_id: i64,
    decision_outcome: Option<DecisionOutcome>,
) -> anyhow::Result<()> {
    if let Some((run_id, run_status)) = latest_run_for_work_item(connection, work_item_id)? {
        if run_status == RunStatus::Failed {
            // Public `run finish --status failed` already emitted the operational
            // failure attempt. A later decision only closes the work narrative.
            return Ok(());
        }
        if let Some((_work_item_id, run_finish_event_id)) = run_finish_context(connection, run_id)?
        {
            let terminal = core_terminal_for_run(connection, run_id, run_status, decision_outcome)?;
            best_effort_record_command_experience(connection, run_id, terminal);
            return queue_core_work_terminal_before_event(
                connection,
                work_item_id,
                run_finish_event_id,
                terminal,
            );
        }
    }

    let terminal = match decision_outcome {
        Some(DecisionOutcome::Inconclusive) => COMPLETED_INCONCLUSIVE,
        _ => COMPLETED_POSITIVE,
    };
    queue_core_work_item_terminal(connection, work_item_id, "finish", terminal)
}

fn best_effort_record_command_experience(
    connection: &Connection,
    run_id: i64,
    terminal: StateCode,
) {
    let _ = record_command_experience(connection, run_id, terminal);
}

fn record_command_experience(
    connection: &Connection,
    run_id: i64,
    terminal: StateCode,
) -> anyhow::Result<()> {
    let Some(ldgr_home) = telemetry_ldgr_home() else { return Ok(()); };
    if !crate::telemetry::anonymous_collection_is_eligible(&ldgr_home) {
        return Ok(());
    }
    let terminal = crate::telemetry::transition::NormalizedTerminal::try_from(terminal)?;
    let raw_input = connection
        .query_row(
            "SELECT COALESCE(run.command, work_item.title)
             FROM run
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE run.id = ?1",
            params![run_id],
            |row| row.get::<_, String>(0),
        )
        .context("failed to read the local command-experience classification input")?;

    let mut validation_statement = connection.prepare(
        "SELECT json_extract(payload_json, '$.outcome')
         FROM event_log
         WHERE entity_type = 'run' AND entity_id = ?1 AND event_type = 'validation'
         ORDER BY id",
    )?;
    let outcomes = validation_statement
        .query_map(params![run_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let validation = if outcomes.iter().any(|value| value == "fail" || value == "error") {
        ValidationBucket::AnyFail
    } else {
        let passed = outcomes.iter().filter(|value| value.as_str() == "pass").count();
        if passed == 0 {
            ValidationBucket::None
        } else if passed == outcomes.len() {
            ValidationBucket::AllPass
        } else {
            ValidationBucket::SomePass
        }
    };

    let mut artifact_statement = connection.prepare(
        "SELECT kind FROM artifact WHERE run_id = ?1 ORDER BY id",
    )?;
    let artifact_kinds = artifact_statement
        .query_map(params![run_id], |row| row.get::<_, String>(0))?
        .collect::<Result<Vec<_>, _>>()?;
    let artifact_roles = artifact_kinds.iter().map(|kind| match kind.as_str() {
        "json" | "csv" => ArtifactRole::Data,
        "report" | "image" => ArtifactRole::Report,
        _ => ArtifactRole::Other,
    });
    let episode = PrivateEpisodeProjection::from_local_inputs(
        &raw_input,
        validation,
        artifact_roles,
        artifact_kinds.len(),
        terminal,
    );
    // The raw input is dropped here. Only the finite projection and bucketed
    // counts can cross into the local telemetry construction store.
    record_private_episode(&ldgr_home, &episode)
}

fn queue_core_work_item_terminal(
    connection: &Connection,
    work_item_id: i64,
    event_type: &str,
    terminal: StateCode,
) -> anyhow::Result<()> {
    let Some(terminal_event_id) = latest_work_item_event_id(connection, work_item_id, event_type)?
    else {
        return Ok(());
    };
    queue_core_work_terminal_before_event(connection, work_item_id, terminal_event_id, terminal)
}

fn queue_core_work_terminal_before_event(
    connection: &Connection,
    work_item_id: i64,
    terminal_event_id: i64,
    terminal: StateCode,
) -> anyhow::Result<()> {
    let mut states = core_work_states_before_event(connection, work_item_id, terminal_event_id)?;
    let previous = *states
        .last()
        .expect("core work state collection always starts with pending");
    if !CORE_WORK_V1.permits(previous, terminal) {
        return Ok(());
    }
    states.push(terminal);
    let Some(ldgr_home) = telemetry_ldgr_home() else {
        return Ok(());
    };
    let _ = queue_committed_terminal_sequence(ldgr_home, &CORE_WORK_V1, &states)?;
    Ok(())
}

fn core_work_states_before_event(
    connection: &Connection,
    work_item_id: i64,
    before_event_id: i64,
) -> anyhow::Result<Vec<StateCode>> {
    let mut statement = connection.prepare(
        "SELECT event_log.entity_type AS entity_type,
                event_log.event_type AS event_type,
                run.status AS run_status
         FROM event_log
         LEFT JOIN run
           ON event_log.entity_type = 'run'
          AND run.id = event_log.entity_id
         WHERE event_log.id < ?1
           AND (
             (event_log.entity_type = 'work_item' AND event_log.entity_id = ?2)
             OR (event_log.entity_type = 'run'
                 AND event_log.event_type = 'finish'
                 AND run.work_item_id = ?2)
           )
         ORDER BY event_log.id",
    )?;
    let mut rows = statement.query(params![before_event_id, work_item_id])?;
    let mut states = vec![PENDING];
    while let Some(row) = rows.next()? {
        let entity_type: String = row.get("entity_type")?;
        let event_type: String = row.get("event_type")?;
        match (entity_type.as_str(), event_type.as_str()) {
            ("work_item", "start_run") => push_core_work_state(&mut states, RUNNING),
            ("work_item", "hold") => push_core_work_state(&mut states, HELD),
            ("work_item", "finish" | "cancel") => reset_core_work_state(&mut states),
            ("run", "finish") => {
                let run_status = row
                    .get::<_, Option<String>>("run_status")?
                    .as_deref()
                    .map(RunStatus::from_str)
                    .transpose()
                    .map_err(|error| anyhow::anyhow!(error.to_string()))?;
                if run_status == Some(RunStatus::Failed) {
                    reset_core_work_state(&mut states);
                }
            }
            _ => {}
        }
    }
    Ok(states)
}

fn push_core_work_state(states: &mut Vec<StateCode>, state: StateCode) {
    let previous = *states
        .last()
        .expect("core work state collection always starts with pending");
    if previous == state || !CORE_WORK_V1.permits(previous, state) {
        return;
    }
    states.push(state);
}

fn reset_core_work_state(states: &mut Vec<StateCode>) {
    states.clear();
    states.push(PENDING);
}

fn core_terminal_for_run(
    connection: &Connection,
    run_id: i64,
    run_status: RunStatus,
    decision_outcome: Option<DecisionOutcome>,
) -> anyhow::Result<StateCode> {
    if run_status == RunStatus::Failed || run_has_validation_outcome(connection, run_id, "error")? {
        return Ok(OPERATIONAL_FAILURE);
    }
    if run_status == RunStatus::Partial || decision_outcome == Some(DecisionOutcome::Inconclusive) {
        return Ok(COMPLETED_INCONCLUSIVE);
    }
    if run_has_validation_outcome(connection, run_id, "fail")? {
        return Ok(COMPLETED_NEGATIVE);
    }
    Ok(COMPLETED_POSITIVE)
}

fn run_has_validation_outcome(
    connection: &Connection,
    run_id: i64,
    outcome: &str,
) -> anyhow::Result<bool> {
    connection
        .query_row(
            "SELECT EXISTS (
                 SELECT 1
                 FROM event_log
                 WHERE entity_type = 'run'
                   AND entity_id = ?1
                   AND event_type = 'validation'
                   AND json_extract(payload_json, '$.outcome') = ?2
             )",
            params![run_id, outcome],
            |row| row.get(0),
        )
        .context("failed to inspect validation outcomes for telemetry")
}

fn run_finish_context(connection: &Connection, run_id: i64) -> anyhow::Result<Option<(i64, i64)>> {
    connection
        .query_row(
            "SELECT run.work_item_id, event_log.id
             FROM run
             JOIN event_log
               ON event_log.entity_type = 'run'
              AND event_log.entity_id = run.id
              AND event_log.event_type = 'finish'
             WHERE run.id = ?1
             ORDER BY event_log.id DESC
             LIMIT 1",
            params![run_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .context("failed to locate run terminal event for telemetry")
}

fn latest_run_for_work_item(
    connection: &Connection,
    work_item_id: i64,
) -> anyhow::Result<Option<(i64, RunStatus)>> {
    connection
        .query_row(
            "SELECT id, status
             FROM run
             WHERE work_item_id = ?1
             ORDER BY id DESC
             LIMIT 1",
            params![work_item_id],
            |row| {
                let status_text: String = row.get(1)?;
                let status = RunStatus::from_str(&status_text).map_err(parse_error_to_sql_error)?;
                Ok((row.get(0)?, status))
            },
        )
        .optional()
        .context("failed to inspect latest run for telemetry")
}

fn latest_work_item_event_id(
    connection: &Connection,
    work_item_id: i64,
    event_type: &str,
) -> anyhow::Result<Option<i64>> {
    connection
        .query_row(
            "SELECT id
             FROM event_log
             WHERE entity_type = 'work_item'
               AND entity_id = ?1
               AND event_type = ?2
             ORDER BY id DESC
             LIMIT 1",
            params![work_item_id, event_type],
            |row| row.get(0),
        )
        .optional()
        .context("failed to locate work terminal event for telemetry")
}

fn telemetry_ldgr_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".ldgr"))
}
