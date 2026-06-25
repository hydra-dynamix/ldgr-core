use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionLogRun {
    pub run_id: i64,
    pub status: RunStatus,
    pub command: Option<String>,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub notes: Option<String>,
    pub observations: Vec<ObservationSummary>,
    pub artifacts: Vec<ArtifactSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionLogEntry {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub status: WorkItemStatus,
    pub created_at: String,
    pub updated_at: String,
    pub decisions: Vec<DecisionSummary>,
    pub runs: Vec<MissionLogRun>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionLogTotals {
    pub work_done: i64,
    pub work_pending: i64,
    pub work_running: i64,
    pub work_held: i64,
    pub work_canceled: i64,
    pub runs_succeeded: i64,
    pub runs_failed: i64,
    pub observations_recorded: i64,
    pub artifacts_recorded: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct MissionLog {
    pub totals: MissionLogTotals,
    pub entries: Vec<MissionLogEntry>,
}

const NESTED_RECORD_LIMIT: i64 = 50;

pub fn read_mission_log(connection: &Connection, limit: i64) -> anyhow::Result<MissionLog> {
    let mut statement = connection
        .prepare(
            "SELECT id, slug, title, description, status, created_at, updated_at
             FROM work_item
             ORDER BY updated_at DESC, id DESC
             LIMIT ?1",
        )
        .context("failed to prepare mission log work item query")?;
    let work_rows = statement
        .query_map(params![limit], |row| {
            Ok((
                row.get::<_, i64>("id")?,
                row.get::<_, String>("slug")?,
                row.get::<_, String>("title")?,
                row.get::<_, String>("description")?,
                row.get::<_, String>("status")?,
                row.get::<_, String>("created_at")?,
                row.get::<_, String>("updated_at")?,
            ))
        })
        .context("failed to query mission log work items")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read mission log work items")?;

    let mut entries = Vec::with_capacity(work_rows.len());
    for (work_item_id, slug, title, description, status, created_at, updated_at) in work_rows {
        let status = status
            .parse::<WorkItemStatus>()
            .map_err(|error| anyhow::anyhow!("invalid work item status for {slug}: {error}"))?;
        let decisions = list_decisions(connection, Some(&slug), NESTED_RECORD_LIMIT)?;
        let runs = mission_log_runs(connection, work_item_id)?;
        entries.push(MissionLogEntry {
            slug,
            title,
            description,
            status,
            created_at,
            updated_at,
            decisions,
            runs,
        });
    }

    Ok(MissionLog {
        totals: mission_log_totals(connection)?,
        entries,
    })
}

fn mission_log_runs(
    connection: &Connection,
    work_item_id: i64,
) -> anyhow::Result<Vec<MissionLogRun>> {
    let mut statement = connection
        .prepare(
            "SELECT id, status, command, started_at, finished_at, notes
             FROM run
             WHERE work_item_id = ?1
             ORDER BY started_at DESC, id DESC
             LIMIT ?2",
        )
        .context("failed to prepare mission log run query")?;
    let run_rows = statement
        .query_map(params![work_item_id, NESTED_RECORD_LIMIT], |row| {
            Ok((
                row.get::<_, i64>("id")?,
                row.get::<_, String>("status")?,
                row.get::<_, Option<String>>("command")?,
                row.get::<_, String>("started_at")?,
                row.get::<_, Option<String>>("finished_at")?,
                row.get::<_, Option<String>>("notes")?,
            ))
        })
        .context("failed to query mission log runs")?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read mission log runs")?;

    let mut runs = Vec::with_capacity(run_rows.len());
    for (run_id, status, command, started_at, finished_at, notes) in run_rows {
        let status = status
            .parse::<RunStatus>()
            .map_err(|error| anyhow::anyhow!("invalid run status for run {run_id}: {error}"))?;
        runs.push(MissionLogRun {
            run_id,
            status,
            command,
            started_at,
            finished_at,
            notes,
            observations: list_observations(connection, Some(run_id), NESTED_RECORD_LIMIT)?,
            artifacts: list_artifacts(connection, Some(run_id), NESTED_RECORD_LIMIT)?,
        });
    }
    Ok(runs)
}

fn mission_log_totals(connection: &Connection) -> anyhow::Result<MissionLogTotals> {
    Ok(MissionLogTotals {
        work_done: count_work_items_by_status(connection, WorkItemStatus::Done)?,
        work_pending: count_work_items_by_status(connection, WorkItemStatus::Pending)?,
        work_running: count_work_items_by_status(connection, WorkItemStatus::Running)?,
        work_held: count_work_items_by_status(connection, WorkItemStatus::Held)?,
        work_canceled: count_work_items_by_status(connection, WorkItemStatus::Canceled)?,
        runs_succeeded: count_rows(
            connection,
            "SELECT COUNT(*) FROM run WHERE status = 'success'",
        )?,
        runs_failed: count_rows(
            connection,
            "SELECT COUNT(*) FROM run WHERE status = 'failed'",
        )?,
        observations_recorded: count_rows(connection, "SELECT COUNT(*) FROM observation")?,
        artifacts_recorded: count_rows(connection, "SELECT COUNT(*) FROM artifact")?,
    })
}

fn count_rows(connection: &Connection, query: &str) -> anyhow::Result<i64> {
    connection
        .query_row(query, [], |row| row.get(0))
        .with_context(|| format!("failed to run mission log count query: {query}"))
}
