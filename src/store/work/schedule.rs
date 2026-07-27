pub const SCHEDULE_FORMAT: &str = "ldgr.schedule.v1";

pub fn example_schedule() -> ScheduleFile {
    ScheduleFile {
        format: SCHEDULE_FORMAT.to_owned(),
        work_items: vec![
            ScheduleWorkItem {
                slug: "foundation".to_owned(),
                title: "Build foundation".to_owned(),
                description: "Implement and validate the prerequisite work.".to_owned(),
                status: Some("pending".to_owned()),
                priority: Some("P0".to_owned()),
                program: Some("release".to_owned()),
                group: Some("implementation".to_owned()),
                acceptance_criteria: Some("Foundation validation passes.".to_owned()),
                hold_kind: None,
                hold_reason: None,
                dependencies: Vec::new(),
            },
            ScheduleWorkItem {
                slug: "release-gate".to_owned(),
                title: "Run release gate".to_owned(),
                description: "Validate the complete release candidate.".to_owned(),
                status: Some("pending".to_owned()),
                priority: Some("P1".to_owned()),
                program: Some("release".to_owned()),
                group: Some("validation".to_owned()),
                acceptance_criteria: Some("All release checks pass.".to_owned()),
                hold_kind: None,
                hold_reason: None,
                dependencies: vec!["foundation".to_owned()],
            },
        ],
    }
}

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
            let slug = item.slug.trim();
            let existing = connection
                .query_row(
                    "SELECT * FROM work_item WHERE slug = ?1",
                    params![slug],
                    WorkItem::from_row,
                )
                .optional()?;
            if existing.is_some() && !upsert {
                bail!(
                    "work item {} already exists; pass --upsert to update existing schedule entries",
                    item.slug
                );
            }
            let status = parse_schedule_work_status(item.status.as_deref().unwrap_or("pending"))?;
            let priority = normalize_priority(item.priority.as_deref())?;
            let program = item.program.as_deref().map(normalize_label);
            let group = item.group.as_deref().map(normalize_label);
            let acceptance_criteria = item.acceptance_criteria.as_deref().map(str::trim);
            let hold_kind = if status == WorkItemStatus::Held {
                Some(parse_schedule_hold_kind(
                    item.hold_kind.as_deref().unwrap_or("blocked"),
                )?)
            } else {
                None
            };
            let hold_reason = (status == WorkItemStatus::Held)
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
                        item.title.trim(),
                        item.description.trim(),
                        status.as_str(),
                        priority,
                        program,
                        group,
                        acceptance_criteria,
                        hold_kind.map(HoldKind::as_str),
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
                        slug,
                        item.title.trim(),
                        item.description.trim(),
                        status.as_str(),
                        priority,
                        program,
                        group,
                        acceptance_criteria,
                        hold_kind.map(HoldKind::as_str),
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
            dependency_count += item
                .dependencies
                .iter()
                .map(|slug| slug.trim())
                .filter(|slug| !slug.is_empty())
                .collect::<std::collections::BTreeSet<_>>()
                .len();
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

pub fn dry_run_import_schedule(
    connection: &Connection,
    schedule: &ScheduleFile,
    upsert: bool,
) -> anyhow::Result<ImportScheduleResult> {
    connection
        .execute_batch("SAVEPOINT ldgr_import_dry_run")
        .context("failed to begin schedule import dry-run")?;
    let result = import_schedule(connection, schedule, upsert);
    let rollback = connection.execute_batch(
        "ROLLBACK TO SAVEPOINT ldgr_import_dry_run; RELEASE SAVEPOINT ldgr_import_dry_run",
    );
    if let Err(error) = rollback {
        return Err(error).context("failed to roll back schedule import dry-run");
    }
    result
}

fn validate_schedule(schedule: &ScheduleFile) -> anyhow::Result<()> {
    let mut slugs = std::collections::BTreeSet::new();
    for item in &schedule.work_items {
        validate_work_fields(&item.slug, &item.title, &item.description)?;
        if !slugs.insert(item.slug.trim()) {
            bail!("schedule contains duplicate work item slug {}", item.slug);
        }
        normalize_priority(item.priority.as_deref())?;
        validate_optional_label("program", item.program.as_deref())?;
        validate_optional_label("group", item.group.as_deref())?;
        validate_optional_text("acceptance criteria", item.acceptance_criteria.as_deref())?;
        let status = parse_schedule_work_status(item.status.as_deref().unwrap_or("pending"))?;
        if let Some(kind) = item.hold_kind.as_deref() {
            parse_schedule_hold_kind(kind)?;
        }
        if status != WorkItemStatus::Held && item.hold_kind.is_some() {
            bail!("hold_kind is only valid for held work item {}", item.slug);
        }
    }
    Ok(())
}

fn parse_schedule_work_status(value: &str) -> anyhow::Result<WorkItemStatus> {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "pending" | "todo" | "queued" => Ok(WorkItemStatus::Pending),
        "running" | "active" | "in-progress" | "inprogress" => Ok(WorkItemStatus::Running),
        "held" | "blocked" | "paused" | "deferred" => Ok(WorkItemStatus::Held),
        "done" | "complete" | "completed" | "finished" | "success" | "succeeded"
        | "ok" => Ok(WorkItemStatus::Done),
        "canceled" | "cancelled" | "abandoned" | "dropped" => Ok(WorkItemStatus::Canceled),
        _ => bail!(
            "invalid work item status `{value}`; expected pending, running, held, done, or canceled"
        ),
    }
}

fn parse_schedule_hold_kind(value: &str) -> anyhow::Result<HoldKind> {
    let normalized = value.trim().to_ascii_lowercase().replace('_', "-");
    match normalized.as_str() {
        "blocked" => Ok(HoldKind::Blocked),
        "deferred" => Ok(HoldKind::Deferred),
        "external" | "validation" | "awaiting-validation" | "external-validation" => {
            Ok(HoldKind::ExternalValidation)
        }
        _ => bail!(
            "invalid hold kind `{value}`; expected blocked, deferred, or external-validation"
        ),
    }
}
