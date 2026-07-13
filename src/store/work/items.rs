pub fn create_work_item(
    connection: &Connection,
    parent_work_item_id: Option<i64>,
    slug: &str,
    title: &str,
    description: &str,
) -> anyhow::Result<WorkItem> {
    create_work_item_with_metadata(
        connection,
        parent_work_item_id,
        slug,
        title,
        description,
        WorkItemMetadata::default(),
    )
}

pub fn create_work_item_with_metadata(
    connection: &Connection,
    parent_work_item_id: Option<i64>,
    slug: &str,
    title: &str,
    description: &str,
    metadata: WorkItemMetadata<'_>,
) -> anyhow::Result<WorkItem> {
    validate_work_fields(slug, title, description)?;
    let priority = normalize_priority(metadata.priority)?;
    validate_optional_label("program", metadata.program)?;
    validate_optional_label("group", metadata.group)?;
    validate_optional_text("acceptance criteria", metadata.acceptance_criteria)?;
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO work_item (
                    parent_work_item_id, slug, title, description, priority, program,
                    work_group, acceptance_criteria
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    parent_work_item_id,
                    slug,
                    title,
                    description,
                    priority,
                    metadata.program,
                    metadata.group,
                    metadata.acceptance_criteria,
                ],
            )
            .with_context(|| format!("failed to create work item {slug}"))?;
        let work_item_id = connection.last_insert_rowid();
        replace_work_dependencies(connection, work_item_id, metadata.dependencies)?;
        let payload = serde_json::json!({
            "priority": priority,
            "program": metadata.program,
            "group": metadata.group,
            "acceptance_criteria": metadata.acceptance_criteria,
            "dependencies": metadata.dependencies,
        })
        .to_string();
        record_event(connection, "work_item", work_item_id, "create", &payload)?;
        get_work_item_by_id(connection, work_item_id)
    })
}

pub fn edit_work_item(
    connection: &Connection,
    slug: &str,
    title: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<WorkItem> {
    edit_work_item_fields(
        connection,
        slug,
        WorkItemPatch {
            title,
            description,
            ..WorkItemPatch::default()
        },
    )
}

pub fn edit_work_item_fields(
    connection: &Connection,
    slug: &str,
    patch: WorkItemPatch<'_>,
) -> anyhow::Result<WorkItem> {
    if patch.title.is_none()
        && patch.description.is_none()
        && patch.priority.is_none()
        && patch.program.is_none()
        && patch.group.is_none()
        && patch.acceptance_criteria.is_none()
        && patch.dependencies.is_none()
    {
        bail!("work edit requires at least one field or --depends-on");
    }
    if patch.title.is_some_and(|title| title.trim().is_empty()) {
        bail!("work title must not be empty");
    }
    if patch.description.is_some_and(|description| description.trim().is_empty()) {
        bail!("work description must not be empty");
    }
    let priority = patch
        .priority
        .map(|value| normalize_priority(value))
        .transpose()?;
    if let Some(value) = patch.program {
        validate_optional_label("program", value)?;
    }
    if let Some(value) = patch.group {
        validate_optional_label("group", value)?;
    }
    if let Some(value) = patch.acceptance_criteria {
        validate_optional_text("acceptance criteria", value)?;
    }
    let work_item = require_work_item_by_slug(connection, slug)?;
    let next_title = patch.title.unwrap_or(&work_item.title);
    let next_description = patch.description.unwrap_or(&work_item.description);
    let next_priority = priority.unwrap_or_else(|| work_item.priority.clone());
    let next_program = patch
        .program
        .map(|value| value.map(str::to_owned))
        .unwrap_or_else(|| work_item.program.clone());
    let next_group = patch
        .group
        .map(|value| value.map(str::to_owned))
        .unwrap_or_else(|| work_item.group.clone());
    let next_acceptance = patch
        .acceptance_criteria
        .map(|value| value.map(str::to_owned))
        .unwrap_or_else(|| work_item.acceptance_criteria.clone());
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item
                 SET title = ?1, description = ?2, priority = ?3, program = ?4,
                     work_group = ?5, acceptance_criteria = ?6, updated_at = datetime('now')
                 WHERE id = ?7",
                params![
                    next_title,
                    next_description,
                    next_priority,
                    next_program,
                    next_group,
                    next_acceptance,
                    work_item.id,
                ],
            )
            .with_context(|| format!("failed to edit work item {slug}"))?;
        if let Some(dependencies) = patch.dependencies {
            replace_work_dependencies(connection, work_item.id, dependencies)?;
        }
        let payload = serde_json::json!({
            "title": patch.title,
            "description": patch.description,
            "priority": patch.priority.map(|_| next_priority),
            "program": patch.program,
            "group": patch.group,
            "acceptance_criteria": patch.acceptance_criteria,
            "dependencies": patch.dependencies,
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
                "UPDATE work_item
                 SET status = 'canceled', hold_kind = NULL, hold_reason = NULL,
                     updated_at = datetime('now')
                 WHERE id = ?1",
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
    hold_work_item_with_kind(connection, slug, HoldKind::Blocked, reason)
}

pub fn hold_work_item_with_kind(
    connection: &Connection,
    slug: &str,
    kind: HoldKind,
    reason: Option<&str>,
) -> anyhow::Result<WorkItem> {
    let work_item = require_work_item_by_slug(connection, slug)?;
    match work_item.status {
        WorkItemStatus::Pending | WorkItemStatus::Running => {}
        WorkItemStatus::Held => {}
        WorkItemStatus::Done => bail!("work item {slug} is already done"),
        WorkItemStatus::Canceled => bail!("work item {slug} is canceled"),
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item
                 SET status = 'held', hold_kind = ?1, hold_reason = ?2,
                     updated_at = datetime('now')
                 WHERE id = ?3",
                params![kind.as_str(), reason, work_item.id],
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
        let payload = serde_json::json!({ "kind": kind.as_str(), "reason": reason }).to_string();
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
                "UPDATE work_item
                 SET status = 'pending', hold_kind = NULL, hold_reason = NULL,
                     updated_at = datetime('now')
                 WHERE id = ?1",
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
    if status == WorkItemStatus::Running {
        let readiness = work_readiness(connection, slug)?;
        if !readiness.blocked_by.is_empty() {
            bail!(
                "work item {slug} is blocked by: {}",
                readiness.blocked_by.join(", ")
            );
        }
    }
    if work_item.status == WorkItemStatus::Canceled && status != WorkItemStatus::Canceled {
        bail!("work item {slug} is canceled");
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item
                 SET status = ?1, hold_kind = NULL, hold_reason = NULL,
                     updated_at = datetime('now')
                 WHERE id = ?2",
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

fn validate_work_fields(slug: &str, title: &str, description: &str) -> anyhow::Result<()> {
    if slug.trim().is_empty() {
        bail!("work slug must not be empty");
    }
    if title.trim().is_empty() {
        bail!("work title must not be empty");
    }
    if description.trim().is_empty() {
        bail!("work description must not be empty");
    }
    Ok(())
}

pub(crate) fn normalize_priority(priority: Option<&str>) -> anyhow::Result<Option<String>> {
    let Some(priority) = priority else {
        return Ok(None);
    };
    let normalized = priority.trim().to_ascii_uppercase();
    if normalized.len() < 2
        || !normalized.starts_with('P')
        || !normalized[1..].chars().all(|character| character.is_ascii_digit())
    {
        bail!("priority must use P<number> form, for example P0 or P1");
    }
    Ok(Some(normalized))
}

fn validate_optional_label(name: &str, value: Option<&str>) -> anyhow::Result<()> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        bail!("work {name} must not be empty; use the matching --clear-{name} flag");
    }
    Ok(())
}

fn validate_optional_text(name: &str, value: Option<&str>) -> anyhow::Result<()> {
    if value.is_some_and(|value| value.trim().is_empty()) {
        bail!("work {name} must not be empty when provided");
    }
    Ok(())
}

pub(crate) fn replace_work_dependencies(
    connection: &Connection,
    work_item_id: i64,
    dependencies: &[String],
) -> anyhow::Result<()> {
    connection.execute(
        "DELETE FROM work_dependency WHERE work_item_id = ?1",
        params![work_item_id],
    )?;
    let mut seen = std::collections::BTreeSet::new();
    for dependency_slug in dependencies {
        if !seen.insert(dependency_slug) {
            continue;
        }
        let dependency = require_work_item_by_slug(connection, dependency_slug)?;
        connection
            .execute(
                "INSERT INTO work_dependency (work_item_id, depends_on_work_item_id)
                 VALUES (?1, ?2)",
                params![work_item_id, dependency.id],
            )
            .with_context(|| {
                format!(
                    "failed to add dependency from work item {work_item_id} to {dependency_slug} (dependency graph must remain acyclic)"
                )
            })?;
    }
    Ok(())
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
