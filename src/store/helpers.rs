use super::*;

pub fn init_store(db_path: &Path, artifact_root: &Path) -> anyhow::Result<()> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create database directory {}", parent.display()))?;
    }
    fs::create_dir_all(artifact_root).with_context(|| {
        format!(
            "failed to create artifact root directory {}",
            artifact_root.display()
        )
    })?;
    open_store(db_path).map(|_| ())
}

pub fn open_store(db_path: &Path) -> anyhow::Result<Connection> {
    if let Some(parent) = db_path.parent() {
        if !parent.as_os_str().is_empty() && !parent.exists() {
            anyhow::bail!(
                "No LDGR ledger found at {}. Run `ldgr init` to create one, or pass --db <path>.",
                db_path.display()
            );
        }
    }
    let connection = Connection::open(db_path)
        .with_context(|| format!("failed to open SQLite database {}", db_path.display()))?;
    connection
        .pragma_update(None, "foreign_keys", "ON")
        .context("failed to enable SQLite foreign keys")?;
    connection
        .pragma_update(None, "busy_timeout", 5000)
        .context("failed to set SQLite busy timeout")?;
    let _granted_mode: String = connection
        .pragma_update_and_check(None, "journal_mode", "WAL", |row| row.get(0))
        .context("failed to negotiate SQLite journal mode")?;
    ensure_schema(&connection)?;
    Ok(connection)
}

pub(crate) fn in_write_transaction<T>(
    connection: &Connection,
    operation: impl FnOnce(&Connection) -> anyhow::Result<T>,
) -> anyhow::Result<T> {
    if connection.is_autocommit() {
        connection
            .execute_batch("BEGIN IMMEDIATE")
            .context("failed to begin write transaction")?;
        match operation(connection) {
            Ok(value) => {
                connection
                    .execute_batch("COMMIT")
                    .context("failed to commit write transaction")?;
                Ok(value)
            }
            Err(error) => {
                let _ = connection.execute_batch("ROLLBACK");
                Err(error)
            }
        }
    } else {
        connection
            .execute_batch("SAVEPOINT ldgr_write")
            .context("failed to begin nested write transaction")?;
        match operation(connection) {
            Ok(value) => {
                connection
                    .execute_batch("RELEASE SAVEPOINT ldgr_write")
                    .context("failed to commit nested write transaction")?;
                Ok(value)
            }
            Err(error) => {
                let _ = connection.execute_batch(
                    "ROLLBACK TO SAVEPOINT ldgr_write; RELEASE SAVEPOINT ldgr_write",
                );
                Err(error)
            }
        }
    }
}

pub(crate) fn count_active_work_items_excluding(
    connection: &Connection,
    excluded_id: i64,
) -> anyhow::Result<i64> {
    connection
        .query_row(
            "SELECT count(*) FROM work_item WHERE id != ?1 AND status IN ('pending', 'running', 'held')",
            params![excluded_id],
            |row| row.get(0),
        )
        .context("failed to count active work items")
}

pub(crate) fn count_work_items_by_status(
    connection: &Connection,
    status: WorkItemStatus,
) -> anyhow::Result<i64> {
    connection
        .query_row(
            "SELECT count(*) FROM work_item WHERE status = ?1",
            params![status.as_str()],
            |row| row.get(0),
        )
        .with_context(|| format!("failed to count {status} work items"))
}

pub(crate) fn latest_artifacts(
    connection: &Connection,
    limit: i64,
) -> anyhow::Result<Vec<ArtifactSummary>> {
    let mut statement = connection
        .prepare(
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
             LIMIT ?1",
        )
        .context("failed to prepare latest artifact query")?;
    let rows = statement
        .query_map(params![limit], artifact_summary_from_row)
        .context("failed to query latest artifacts")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read latest artifacts")
}

pub(crate) fn artifact_summary_from_row(row: &Row<'_>) -> rusqlite::Result<ArtifactSummary> {
    let kind_text: String = row.get("kind")?;
    let kind = kind_text.parse().map_err(parse_error_to_sql_error)?;
    let path_text: String = row.get("path")?;
    Ok(ArtifactSummary {
        artifact_id: row.get("artifact_id")?,
        run_id: row.get("run_id")?,
        work_slug: row.get("work_slug")?,
        kind,
        path: PathBuf::from(path_text),
        description: row.get("description")?,
        created_at: row.get("created_at")?,
    })
}

pub fn list_event_logs(
    connection: &Connection,
    limit: i64,
) -> anyhow::Result<Vec<EventLogSummary>> {
    latest_events(connection, limit)
}

pub(crate) fn latest_events(
    connection: &Connection,
    limit: i64,
) -> anyhow::Result<Vec<EventLogSummary>> {
    let mut statement = connection
        .prepare(
            "SELECT id AS event_id, entity_type, entity_id, event_type, payload_json, created_at
             FROM event_log
             WHERE entity_type IN (
                 'work_item',
                 'run',
                 'observation',
                 'global_observation',
                 'artifact',
                 'prompt',
                 'decision',
                 'loop_intervention'
             )
             ORDER BY created_at DESC, id DESC
             LIMIT ?1",
        )
        .context("failed to prepare event log query")?;
    let rows = statement
        .query_map(params![limit], |row| {
            Ok(EventLogSummary {
                event_id: row.get("event_id")?,
                entity_type: row.get("entity_type")?,
                entity_id: row.get("entity_id")?,
                event_type: row.get("event_type")?,
                payload_json: row.get("payload_json")?,
                created_at: row.get("created_at")?,
            })
        })
        .context("failed to query event log")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read event log")
}

pub(crate) fn get_work_item_by_id(
    connection: &Connection,
    work_item_id: i64,
) -> anyhow::Result<WorkItem> {
    connection
        .query_row(
            "SELECT * FROM work_item WHERE id = ?1",
            params![work_item_id],
            WorkItem::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read work item {work_item_id}"))?
        .with_context(|| format!("work item {work_item_id} not found"))
}

pub(crate) fn require_work_item_by_slug(
    connection: &Connection,
    slug: &str,
) -> anyhow::Result<WorkItem> {
    connection
        .query_row(
            "SELECT * FROM work_item WHERE slug = ?1",
            params![slug],
            WorkItem::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read work item {slug}"))?
        .with_context(|| format!("work item {slug} not found"))
}

pub(crate) fn running_run_ids_for_work_item(
    connection: &Connection,
    work_item_id: i64,
) -> anyhow::Result<Vec<i64>> {
    let mut statement = connection
        .prepare("SELECT id FROM run WHERE work_item_id = ?1 AND status = 'running' ORDER BY id")
        .context("failed to prepare active run query")?;
    let rows = statement
        .query_map(params![work_item_id], |row| row.get(0))
        .context("failed to query active runs")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read active runs")
}

pub(crate) fn get_run_by_id(
    connection: &Connection,
    run_id: i64,
) -> anyhow::Result<InvestigationRun> {
    connection
        .query_row(
            "SELECT * FROM run WHERE id = ?1",
            params![run_id],
            InvestigationRun::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read run {run_id}"))?
        .with_context(|| format!("run {run_id} not found"))
}

pub(crate) fn ensure_run_exists(connection: &Connection, run_id: i64) -> anyhow::Result<()> {
    get_run_by_id(connection, run_id).map(|_| ())
}

pub fn resolve_run_reference(connection: &Connection, reference: &str) -> anyhow::Result<i64> {
    let reference = reference.trim();
    if reference.is_empty() {
        bail!("run reference cannot be empty");
    }
    if let Some(run_id) = parse_run_id_reference(reference) {
        get_run_by_id(connection, run_id)?;
        return Ok(run_id);
    }
    if matches!(reference, "current" | "active") {
        return resolve_current_run_reference(connection, reference);
    }
    let latest_for_work = connection
        .query_row(
            "SELECT run.id
             FROM run
             JOIN work_item ON work_item.id = run.work_item_id
             WHERE work_item.slug = ?1
             ORDER BY CASE run.status WHEN 'running' THEN 0 ELSE 1 END,
                      run.started_at DESC,
                      run.id DESC
             LIMIT 1",
            params![reference],
            |row| row.get(0),
        )
        .optional()
        .with_context(|| format!("failed to resolve run reference {reference}"))?;
    if let Some(run_id) = latest_for_work {
        return Ok(run_id);
    }
    let work_exists = connection
        .query_row(
            "SELECT id FROM work_item WHERE slug = ?1",
            params![reference],
            |row| row.get::<_, i64>(0),
        )
        .optional()
        .with_context(|| format!("failed to inspect work item {reference}"))?;
    if work_exists.is_some() {
        bail!("work item {reference} has no runs; start one with `ldgr run start {reference}`");
    }
    bail!("run reference {reference} did not match a numeric run ID or work-item slug")
}

fn resolve_current_run_reference(connection: &Connection, reference: &str) -> anyhow::Result<i64> {
    let running_runs = running_run_ids(connection)?;
    match running_runs.as_slice() {
        [run_id] => Ok(*run_id),
        [] => bail!("run reference {reference} requested the active run, but no run is running"),
        _ => bail!(
            "run reference {reference} is ambiguous because multiple runs are active: {}",
            running_runs
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

fn running_run_ids(connection: &Connection) -> anyhow::Result<Vec<i64>> {
    let mut statement = connection
        .prepare("SELECT id FROM run WHERE status = 'running' ORDER BY started_at, id")
        .context("failed to prepare running run reference query")?;
    let rows = statement
        .query_map([], |row| row.get(0))
        .context("failed to query running run references")?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read running run references")
}

fn parse_run_id_reference(reference: &str) -> Option<i64> {
    let id_text = reference
        .strip_prefix("run-")
        .or_else(|| reference.strip_prefix("run:"))
        .or_else(|| reference.strip_prefix("run="))
        .or_else(|| reference.strip_prefix('#'))
        .unwrap_or(reference);
    id_text.parse::<i64>().ok()
}

pub(crate) fn get_observation_by_id(
    connection: &Connection,
    observation_id: i64,
) -> anyhow::Result<Observation> {
    connection
        .query_row(
            "SELECT * FROM observation WHERE id = ?1",
            params![observation_id],
            Observation::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read observation {observation_id}"))?
        .with_context(|| format!("observation {observation_id} not found"))
}

pub(crate) fn get_global_observation_by_id(
    connection: &Connection,
    id: i64,
) -> anyhow::Result<GlobalObservation> {
    connection
        .query_row(
            "SELECT * FROM global_observation WHERE id = ?1",
            params![id],
            GlobalObservation::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read global observation {id}"))?
        .with_context(|| format!("global observation {id} not found"))
}

pub(crate) fn get_artifact_by_id(
    connection: &Connection,
    artifact_id: i64,
) -> anyhow::Result<Artifact> {
    connection
        .query_row(
            "SELECT * FROM artifact WHERE id = ?1",
            params![artifact_id],
            Artifact::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read artifact {artifact_id}"))?
        .with_context(|| format!("artifact {artifact_id} not found"))
}

pub(crate) fn get_decision_by_id(
    connection: &Connection,
    decision_id: i64,
) -> anyhow::Result<Decision> {
    connection
        .query_row(
            "SELECT * FROM decision WHERE id = ?1",
            params![decision_id],
            Decision::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read decision {decision_id}"))?
        .with_context(|| format!("decision {decision_id} not found"))
}

pub(crate) fn get_loop_intervention_by_id(
    connection: &Connection,
    intervention_id: i64,
) -> anyhow::Result<LoopIntervention> {
    connection
        .query_row(
            "SELECT * FROM loop_intervention WHERE id = ?1",
            params![intervention_id],
            LoopIntervention::from_row,
        )
        .optional()
        .with_context(|| format!("failed to read loop intervention {intervention_id}"))?
        .with_context(|| format!("loop intervention {intervention_id} not found"))
}

pub(crate) fn record_event(
    connection: &Connection,
    entity_type: &str,
    entity_id: i64,
    event_type: &str,
    payload_json: &str,
) -> anyhow::Result<()> {
    connection
        .execute(
            "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![entity_type, entity_id, event_type, payload_json],
        )
        .with_context(|| {
            format!("failed to record {entity_type} {entity_id} event {event_type}")
        })?;
    Ok(())
}

pub(crate) fn parse_error_to_sql_error(error: ParseEnumError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn in_write_transaction_rolls_back_mid_sequence_failure() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        let result: anyhow::Result<()> = in_write_transaction(&connection, |connection| {
            connection.execute(
                "INSERT INTO work_item (slug, title, description) VALUES ('rollback', 'Rollback', 'Rollback')",
                [],
            )?;
            anyhow::bail!("intentional failure")
        });
        assert!(result.is_err());
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM work_item WHERE slug = 'rollback'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(count, 0);
        Ok(())
    }

    #[test]
    fn nested_in_write_transaction_rolls_back_only_failed_savepoint() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        in_write_transaction(&connection, |connection| {
            connection.execute(
                "INSERT INTO work_item (slug, title, description) VALUES ('outer', 'Outer', 'Outer')",
                [],
            )?;
            let nested: anyhow::Result<()> = in_write_transaction(connection, |connection| {
                connection.execute(
                    "INSERT INTO work_item (slug, title, description) VALUES ('inner', 'Inner', 'Inner')",
                    [],
                )?;
                anyhow::bail!("nested failure")
            });
            assert!(nested.is_err());
            Ok(())
        })?;
        let outer_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM work_item WHERE slug = 'outer'",
            [],
            |row| row.get(0),
        )?;
        let inner_count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM work_item WHERE slug = 'inner'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(outer_count, 1);
        assert_eq!(inner_count, 0);
        Ok(())
    }

    #[test]
    fn open_store_missing_parent_suggests_init() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let db_path = temp.path().join("missing/ldgr.db");
        let error = open_store(&db_path).expect_err("missing parent should be actionable");
        let message = format!("{error:#}");
        assert!(message.contains("No LDGR ledger found"), "{message}");
        assert!(message.contains("Run `ldgr init`"), "{message}");
        Ok(())
    }

    #[test]
    fn open_store_configures_concurrency_pragmas() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        let busy_timeout: i64 =
            connection.query_row("PRAGMA busy_timeout", [], |row| row.get(0))?;
        assert_eq!(busy_timeout, 5000);
        let journal_mode: String =
            connection.query_row("PRAGMA journal_mode", [], |row| row.get::<_, String>(0))?;
        assert!(
            ["wal", "delete", "truncate", "persist"].contains(&journal_mode.as_str()),
            "unexpected journal mode {journal_mode}"
        );
        Ok(())
    }
}
