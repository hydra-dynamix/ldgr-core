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

