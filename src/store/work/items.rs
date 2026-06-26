pub fn create_work_item(
    connection: &Connection,
    parent_work_item_id: Option<i64>,
    slug: &str,
    title: &str,
    description: &str,
) -> anyhow::Result<WorkItem> {
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO work_item (parent_work_item_id, slug, title, description)
             VALUES (?1, ?2, ?3, ?4)",
                params![parent_work_item_id, slug, title, description],
            )
            .with_context(|| format!("failed to create work item {slug}"))?;
        let work_item_id = connection.last_insert_rowid();
        record_event(connection, "work_item", work_item_id, "create", "{}")?;
        get_work_item_by_id(connection, work_item_id)
    })
}

pub fn edit_work_item(
    connection: &Connection,
    slug: &str,
    title: Option<&str>,
    description: Option<&str>,
) -> anyhow::Result<WorkItem> {
    if title.is_none() && description.is_none() {
        bail!("work edit requires --title and/or --description");
    }
    if title.is_some_and(|title| title.trim().is_empty()) {
        bail!("work title must not be empty");
    }
    if description.is_some_and(|description| description.trim().is_empty()) {
        bail!("work description must not be empty");
    }
    let work_item = require_work_item_by_slug(connection, slug)?;
    let next_title = title.unwrap_or(&work_item.title);
    let next_description = description.unwrap_or(&work_item.description);
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item
                 SET title = ?1, description = ?2, updated_at = datetime('now')
                 WHERE id = ?3",
                params![next_title, next_description, work_item.id],
            )
            .with_context(|| format!("failed to edit work item {slug}"))?;
        let payload = serde_json::json!({
            "title": title,
            "description": description,
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
                "UPDATE work_item SET status = 'canceled', updated_at = datetime('now') WHERE id = ?1",
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
    let work_item = require_work_item_by_slug(connection, slug)?;
    match work_item.status {
        WorkItemStatus::Pending | WorkItemStatus::Running => {}
        WorkItemStatus::Held => return Ok(work_item),
        WorkItemStatus::Done => bail!("work item {slug} is already done"),
        WorkItemStatus::Canceled => bail!("work item {slug} is canceled"),
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item SET status = 'held', updated_at = datetime('now') WHERE id = ?1",
                params![work_item.id],
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
        let payload = reason
            .map(|reason| serde_json::json!({ "reason": reason }).to_string())
            .unwrap_or_else(|| "{}".to_owned());
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
                "UPDATE work_item SET status = 'pending', updated_at = datetime('now') WHERE id = ?1",
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
    if work_item.status == WorkItemStatus::Canceled && status != WorkItemStatus::Canceled {
        bail!("work item {slug} is canceled");
    }
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "UPDATE work_item SET status = ?1, updated_at = datetime('now') WHERE id = ?2",
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

