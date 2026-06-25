use super::*;

pub fn add_validation_record(
    connection: &Connection,
    run_id: i64,
    outcome: ValidationOutcome,
    command: Option<&str>,
    rationale: Option<&str>,
) -> anyhow::Result<ValidationRecord> {
    ensure_run_exists(connection, run_id)?;
    let trimmed_rationale = rationale.map(str::trim).filter(|value| !value.is_empty());
    if outcome == ValidationOutcome::Skipped && trimmed_rationale.is_none() {
        bail!("skipped validation requires --rationale");
    }

    in_write_transaction(connection, |connection| {
        let payload = serde_json::json!({
            "run_id": run_id,
            "outcome": outcome.as_str(),
            "command": command,
            "rationale": trimmed_rationale,
        })
        .to_string();
        record_event(connection, "run", run_id, "validation", &payload)
            .with_context(|| format!("failed to record validation for run {run_id}"))?;
        let validation_id = connection.last_insert_rowid();
        get_validation_record(connection, validation_id)
    })
}

pub fn get_validation_record(
    connection: &Connection,
    validation_id: i64,
) -> anyhow::Result<ValidationRecord> {
    connection
        .query_row(
            "SELECT event_log.id AS id,
                    event_log.entity_id AS run_id,
                    event_log.payload_json AS payload_json,
                    event_log.created_at AS created_at
             FROM event_log
             WHERE event_log.id = ?1
               AND event_log.entity_type = 'run'
               AND event_log.event_type = 'validation'",
            params![validation_id],
            ValidationRecord::from_event_row,
        )
        .optional()?
        .with_context(|| format!("validation record {validation_id} not found"))
}

pub fn list_validation_records(
    connection: &Connection,
    run_id: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<ValidationSummary>> {
    let query = match run_id {
        Some(_) => {
            "SELECT event_log.id AS validation_id,
                    run.id AS run_id,
                    work_item.slug AS work_slug,
                    event_log.payload_json AS payload_json,
                    event_log.created_at AS created_at
             FROM event_log
             JOIN run ON run.id = event_log.entity_id
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE event_log.entity_type = 'run'
               AND event_log.event_type = 'validation'
               AND run.id = ?1
             ORDER BY event_log.created_at DESC, event_log.id DESC
             LIMIT ?2"
        }
        None => {
            "SELECT event_log.id AS validation_id,
                    run.id AS run_id,
                    work_item.slug AS work_slug,
                    event_log.payload_json AS payload_json,
                    event_log.created_at AS created_at
             FROM event_log
             JOIN run ON run.id = event_log.entity_id
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE event_log.entity_type = 'run'
               AND event_log.event_type = 'validation'
             ORDER BY event_log.created_at DESC, event_log.id DESC
             LIMIT ?1"
        }
    };
    let mut statement = connection
        .prepare(query)
        .context("failed to prepare validation record list query")?;
    let rows = match run_id {
        Some(run_id) => statement.query_map(params![run_id, limit], validation_summary_from_row)?,
        None => statement.query_map(params![limit], validation_summary_from_row)?,
    };
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read validation records")
}

pub(crate) fn validation_summary_from_row(row: &Row<'_>) -> rusqlite::Result<ValidationSummary> {
    let payload_json: String = row.get("payload_json")?;
    let payload = parse_validation_payload(&payload_json)?;
    let outcome_text = validation_payload_str(&payload, "outcome")?;
    let outcome = ValidationOutcome::from_str(outcome_text).map_err(parse_error_to_sql_error)?;
    Ok(ValidationSummary {
        validation_id: row.get("validation_id")?,
        run_id: row.get("run_id")?,
        work_slug: row.get("work_slug")?,
        outcome,
        command: validation_payload_optional_str(&payload, "command"),
        rationale: validation_payload_optional_str(&payload, "rationale"),
        created_at: row.get("created_at")?,
    })
}

pub(crate) fn parse_validation_payload(payload_json: &str) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str::<serde_json::Value>(payload_json)
        .map_err(|error| validation_payload_error(error.to_string()))
}

pub(crate) fn validation_payload_str<'a>(
    payload: &'a serde_json::Value,
    key: &str,
) -> rusqlite::Result<&'a str> {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .ok_or_else(|| validation_payload_error(format!("missing validation {key}")))
}

pub(crate) fn validation_payload_optional_str(
    payload: &serde_json::Value,
    key: &str,
) -> Option<String> {
    payload
        .get(key)
        .and_then(|value| value.as_str())
        .map(str::to_owned)
}

pub(crate) fn validation_payload_error(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}
