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

