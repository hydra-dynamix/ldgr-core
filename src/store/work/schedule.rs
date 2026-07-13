pub const SCHEDULE_FORMAT: &str = "ldgr.schedule.v1";

pub fn export_schedule(
    connection: &Connection,
    program: Option<&str>,
    priority: Option<&str>,
) -> anyhow::Result<ScheduleFile> {
    let work_items = list_work_items_filtered(connection, None, program, priority)?;
    let work_items = work_items
        .into_iter()
        .map(|work_item| {
            let dependencies = dependency_slugs(connection, work_item.id, false)?;
            Ok(ScheduleWorkItem {
                slug: work_item.slug,
                title: work_item.title,
                description: work_item.description,
                status: Some(work_item.status.as_str().to_owned()),
                priority: work_item.priority,
                program: work_item.program,
                group: work_item.group,
                acceptance_criteria: work_item.acceptance_criteria,
                hold_kind: work_item.hold_kind.map(|kind| kind.as_str().to_owned()),
                hold_reason: work_item.hold_reason,
                dependencies,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(ScheduleFile {
        format: SCHEDULE_FORMAT.to_owned(),
        work_items,
    })
}

pub fn import_schedule(
    connection: &Connection,
    schedule: &ScheduleFile,
    upsert: bool,
) -> anyhow::Result<ImportScheduleResult> {
    if schedule.format != SCHEDULE_FORMAT {
        bail!(
            "unsupported schedule format {}; expected {SCHEDULE_FORMAT}",
            schedule.format
        );
    }
    validate_schedule(schedule)?;
    in_write_transaction(connection, |connection| {
        let mut created = 0;
        let mut updated = 0;
        for item in &schedule.work_items {
            let existing = connection
                .query_row(
                    "SELECT * FROM work_item WHERE slug = ?1",
                    params![item.slug],
                    WorkItem::from_row,
                )
                .optional()?;
            if existing.is_some() && !upsert {
                bail!(
                    "work item {} already exists; pass --upsert to update existing schedule entries",
                    item.slug
                );
            }
            let status = item.status.as_deref().unwrap_or("pending");
            let priority = normalize_priority(item.priority.as_deref())?;
            let hold_kind = if status == "held" {
                Some(item.hold_kind.as_deref().unwrap_or("blocked"))
            } else {
                None
            };
            let hold_reason = (status == "held")
                .then_some(item.hold_reason.as_deref())
                .flatten();
            if let Some(existing) = existing {
                connection.execute(
                    "UPDATE work_item
                     SET title = ?1, description = ?2, status = ?3, priority = ?4,
                         program = ?5, work_group = ?6, acceptance_criteria = ?7,
                         hold_kind = ?8, hold_reason = ?9, updated_at = datetime('now')
                     WHERE id = ?10",
                    params![
                        item.title,
                        item.description,
                        status,
                        priority,
                        item.program,
                        item.group,
                        item.acceptance_criteria,
                        hold_kind,
                        hold_reason,
                        existing.id,
                    ],
                )?;
                updated += 1;
            } else {
                connection.execute(
                    "INSERT INTO work_item (
                        slug, title, description, status, priority, program, work_group,
                        acceptance_criteria, hold_kind, hold_reason
                     ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                    params![
                        item.slug,
                        item.title,
                        item.description,
                        status,
                        priority,
                        item.program,
                        item.group,
                        item.acceptance_criteria,
                        hold_kind,
                        hold_reason,
                    ],
                )?;
                created += 1;
            }
        }

        let mut dependency_count = 0;
        for item in &schedule.work_items {
            let work_item = require_work_item_by_slug(connection, &item.slug)?;
            replace_work_dependencies(connection, work_item.id, &item.dependencies)?;
            dependency_count += item.dependencies.len();
            let payload = serde_json::json!({
                "source": SCHEDULE_FORMAT,
                "upsert": upsert,
                "dependencies": item.dependencies,
            })
            .to_string();
            record_event(connection, "work_item", work_item.id, "schedule_import", &payload)?;
        }
        Ok(ImportScheduleResult {
            created,
            updated,
            dependencies: dependency_count,
        })
    })
}

fn validate_schedule(schedule: &ScheduleFile) -> anyhow::Result<()> {
    let mut slugs = std::collections::BTreeSet::new();
    for item in &schedule.work_items {
        validate_work_fields(&item.slug, &item.title, &item.description)?;
        if !slugs.insert(item.slug.as_str()) {
            bail!("schedule contains duplicate work item slug {}", item.slug);
        }
        normalize_priority(item.priority.as_deref())?;
        validate_optional_label("program", item.program.as_deref())?;
        validate_optional_label("group", item.group.as_deref())?;
        validate_optional_text("acceptance criteria", item.acceptance_criteria.as_deref())?;
        let status = item.status.as_deref().unwrap_or("pending");
        WorkItemStatus::from_str(status)
            .map_err(|_| anyhow::anyhow!("invalid status {status} for work item {}", item.slug))?;
        if let Some(kind) = item.hold_kind.as_deref() {
            HoldKind::from_str(kind).map_err(|_| {
                anyhow::anyhow!("invalid hold kind {kind} for work item {}", item.slug)
            })?;
        }
    }
    Ok(())
}
