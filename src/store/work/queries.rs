pub fn next_pending_work_item(connection: &Connection) -> anyhow::Result<Option<WorkItem>> {
    next_ready_work_item(connection, None, None)
}

pub fn next_ready_work_item(
    connection: &Connection,
    program: Option<&str>,
    priority: Option<&str>,
) -> anyhow::Result<Option<WorkItem>> {
    let priority = normalize_priority(priority)?;
    connection
        .query_row(
            "SELECT * FROM work_item
             WHERE status = 'pending'
               AND (?1 IS NULL OR program = ?1)
               AND (?2 IS NULL OR priority = ?2)
               AND NOT EXISTS (
                   SELECT 1
                   FROM work_dependency AS dependency
                   JOIN work_item AS prerequisite
                     ON prerequisite.id = dependency.depends_on_work_item_id
                   WHERE dependency.work_item_id = work_item.id
                     AND prerequisite.status != 'done'
               )
             ORDER BY
               CASE WHEN priority GLOB 'P[0-9]*'
                    THEN CAST(substr(priority, 2) AS INTEGER)
                    ELSE 2147483647 END,
               created_at, id
             LIMIT 1",
            params![program, priority],
            WorkItem::from_row,
        )
        .optional()
        .context("failed to read next pending work item")
}

pub fn work_readiness(
    connection: &Connection,
    work_slug: &str,
) -> anyhow::Result<WorkReadiness> {
    let work_item = require_work_item_by_slug(connection, work_slug)?;
    let dependencies = dependency_slugs(connection, work_item.id, false)?;
    let blocked_by = dependency_slugs(connection, work_item.id, true)?;
    let unblocks = unblocked_slugs(connection, work_item.id)?;
    Ok(WorkReadiness {
        ready: work_item.status == WorkItemStatus::Pending && blocked_by.is_empty(),
        blocked_by,
        dependencies,
        unblocks,
    })
}

pub fn work_item_view(
    connection: &Connection,
    work_item: WorkItem,
) -> anyhow::Result<WorkItemView> {
    let dependencies = dependency_states(connection, work_item.id)?;
    let dependents = dependent_states(connection, work_item.id)?;
    let mut blocker_reasons = dependencies
        .iter()
        .filter(|dependency| !dependency.satisfied)
        .map(|dependency| {
            format!(
                "dependency {} is {}",
                dependency.slug,
                dependency.status.as_str()
            )
        })
        .collect::<Vec<_>>();
    if work_item.status != WorkItemStatus::Pending {
        blocker_reasons.insert(
            0,
            format!("work item status is {}", work_item.status.as_str()),
        );
    }
    Ok(WorkItemView {
        ready: work_item.status == WorkItemStatus::Pending && blocker_reasons.is_empty(),
        work_item,
        dependencies,
        dependents,
        blocker_reasons,
    })
}

pub fn get_work_item_view_by_slug(
    connection: &Connection,
    slug: &str,
) -> anyhow::Result<WorkItemView> {
    work_item_view(connection, get_work_item_by_slug(connection, slug)?)
}

pub fn list_work_item_views_filtered(
    connection: &Connection,
    status: Option<WorkItemStatus>,
    program: Option<&str>,
    priority: Option<&str>,
) -> anyhow::Result<Vec<WorkItemView>> {
    list_work_items_filtered(connection, status, program, priority)?
        .into_iter()
        .map(|work_item| work_item_view(connection, work_item))
        .collect()
}

fn dependency_states(
    connection: &Connection,
    work_item_id: i64,
) -> anyhow::Result<Vec<WorkDependencyState>> {
    let query = "SELECT prerequisite.slug, prerequisite.status
         FROM work_dependency AS dependency
         JOIN work_item AS prerequisite ON prerequisite.id = dependency.depends_on_work_item_id
         WHERE dependency.work_item_id = ?1
         ORDER BY prerequisite.slug";
    let mut statement = connection.prepare(query)?;
    let states = statement
        .query_map(params![work_item_id], |row| {
            let status_text: String = row.get(1)?;
            let status = WorkItemStatus::from_str(&status_text)
                .map_err(parse_error_to_sql_error)?;
            Ok(WorkDependencyState {
                slug: row.get(0)?,
                satisfied: status == WorkItemStatus::Done,
                status,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read work dependency states")?;
    Ok(states)
}

fn dependent_states(
    connection: &Connection,
    work_item_id: i64,
) -> anyhow::Result<Vec<WorkDependentState>> {
    let mut statement = connection.prepare(
        "SELECT dependent.slug, dependent.status
         FROM work_dependency AS dependency
         JOIN work_item AS dependent ON dependent.id = dependency.work_item_id
         WHERE dependency.depends_on_work_item_id = ?1
         ORDER BY dependent.slug",
    )?;
    let states = statement
        .query_map(params![work_item_id], |row| {
            let status_text: String = row.get(1)?;
            Ok(WorkDependentState {
                slug: row.get(0)?,
                status: WorkItemStatus::from_str(&status_text)
                    .map_err(parse_error_to_sql_error)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read dependent work item states")?;
    Ok(states)
}

pub fn list_work_items_filtered(
    connection: &Connection,
    status: Option<WorkItemStatus>,
    program: Option<&str>,
    priority: Option<&str>,
) -> anyhow::Result<Vec<WorkItem>> {
    let priority = normalize_priority(priority)?;
    let mut statement = connection.prepare(
        "SELECT * FROM work_item
         WHERE (?1 IS NULL OR status = ?1)
           AND (?2 IS NULL OR program = ?2)
           AND (?3 IS NULL OR priority = ?3)
         ORDER BY
           CASE WHEN priority GLOB 'P[0-9]*'
                THEN CAST(substr(priority, 2) AS INTEGER)
                ELSE 2147483647 END,
           created_at, id",
    )?;
    let rows = statement.query_map(
        params![status.map(WorkItemStatus::as_str), program, priority],
        WorkItem::from_row,
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read filtered work items")
}

pub fn last_completed_work_item(
    connection: &Connection,
    program: Option<&str>,
    priority: Option<&str>,
) -> anyhow::Result<Option<WorkItem>> {
    let priority = normalize_priority(priority)?;
    connection
        .query_row(
            "SELECT * FROM work_item
             WHERE status = 'done'
               AND (?1 IS NULL OR program = ?1)
               AND (?2 IS NULL OR priority = ?2)
             ORDER BY updated_at DESC, id DESC
             LIMIT 1",
            params![program, priority],
            WorkItem::from_row,
        )
        .optional()
        .context("failed to read last completed work item")
}

fn dependency_slugs(
    connection: &Connection,
    work_item_id: i64,
    only_unsatisfied: bool,
) -> anyhow::Result<Vec<String>> {
    let query = if only_unsatisfied {
        "SELECT prerequisite.slug
         FROM work_dependency AS dependency
         JOIN work_item AS prerequisite ON prerequisite.id = dependency.depends_on_work_item_id
         WHERE dependency.work_item_id = ?1 AND prerequisite.status != 'done'
         ORDER BY prerequisite.slug"
    } else {
        "SELECT prerequisite.slug
         FROM work_dependency AS dependency
         JOIN work_item AS prerequisite ON prerequisite.id = dependency.depends_on_work_item_id
         WHERE dependency.work_item_id = ?1
         ORDER BY prerequisite.slug"
    };
    let mut statement = connection.prepare(query)?;
    let rows = statement
        .query_map(params![work_item_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read work dependencies")?;
    Ok(rows)
}

fn unblocked_slugs(connection: &Connection, work_item_id: i64) -> anyhow::Result<Vec<String>> {
    let mut statement = connection.prepare(
        "SELECT dependent.slug
         FROM work_dependency AS dependency
         JOIN work_item AS dependent ON dependent.id = dependency.work_item_id
         WHERE dependency.depends_on_work_item_id = ?1
         ORDER BY dependent.slug",
    )?;
    let rows = statement
        .query_map(params![work_item_id], |row| row.get(0))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read work items unblocked by dependency")?;
    Ok(rows)
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

pub fn list_observations_for_work(
    connection: &Connection,
    work_slug: &str,
    limit: i64,
) -> anyhow::Result<Vec<ObservationSummary>> {
    require_work_item_by_slug(connection, work_slug)?;
    let mut statement = connection.prepare(
        "SELECT observation.id AS observation_id,
                run.id AS run_id,
                work_item.slug AS work_slug,
                observation.body AS body,
                observation.created_at AS created_at
         FROM observation
         JOIN run ON run.id = observation.run_id
         JOIN work_item ON work_item.id = run.work_item_id
         WHERE work_item.slug = ?1
         ORDER BY observation.created_at DESC, observation.id DESC
         LIMIT ?2",
    )?;
    let rows = statement.query_map(
        params![work_slug, limit],
        observation_summary_from_row,
    )?;
    rows.collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to read observations for work item")
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
