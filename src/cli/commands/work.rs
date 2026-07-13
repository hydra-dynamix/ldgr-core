use std::fs;
use std::io::{self, Read};

use crate::store::{
    add_global_observation, clear_global_observation, create_work_item_with_metadata,
    delete_work_item, edit_global_observation, edit_work_item_fields, export_schedule,
    get_work_item_by_slug, hold_work_item_with_kind, import_schedule, list_global_observations,
    list_work_items_filtered, next_pending_work_item, set_work_item_status,
    GlobalObservationStatus, ScheduleFile, WorkItemMetadata, WorkItemPatch, WorkItemStatus,
};

use super::super::args::{
    CliGlobalObservationStatus, NextArgs, NoticeArgs, NoticeCommand, WorkArgs, WorkCommand,
    WorkStatusCommand,
};
use super::super::checked_limit;
use super::super::render::brief_context::suggested_next_commands;
use super::super::render::emit;
use super::super::render::text::{print_global_observations, print_work_item, print_work_items};

pub fn handle_work(connection: &rusqlite::Connection, args: WorkArgs) -> anyhow::Result<()> {
    match args.command {
        WorkCommand::List(args) => {
            let work_items = list_work_items_filtered(
                connection,
                args.status.map(WorkItemStatus::from),
                args.program.as_deref(),
                args.priority.as_deref(),
            )?;
            emit(args.json, &work_items, |work_items| {
                print_work_items(work_items)
            })?;
        }
        WorkCommand::Show(args) => {
            let work_item = get_work_item_by_slug(connection, &args.slug)?;
            emit(args.json, &work_item, print_work_item)?;
        }
        WorkCommand::Create(args) => {
            let work_item = create_work_item_with_metadata(
                connection,
                None,
                &args.slug,
                &args.title,
                &args.description,
                WorkItemMetadata {
                    priority: args.priority.as_deref(),
                    program: args.program.as_deref(),
                    group: args.group.as_deref(),
                    acceptance_criteria: args.acceptance_criteria.as_deref(),
                    dependencies: &args.dependencies,
                },
            )?;
            println!("created work item {} {}", work_item.id, work_item.slug);
        }
        WorkCommand::Edit(args) => {
            let dependencies = if args.clear_dependencies {
                Some(&[][..])
            } else if args.dependencies.is_empty() {
                None
            } else {
                Some(args.dependencies.as_slice())
            };
            let work_item = edit_work_item_fields(
                connection,
                &args.slug,
                WorkItemPatch {
                    title: args.title.as_deref(),
                    description: args.description.as_deref(),
                    priority: optional_edit(args.priority.as_deref(), args.clear_priority),
                    program: optional_edit(args.program.as_deref(), args.clear_program),
                    group: optional_edit(args.group.as_deref(), args.clear_group),
                    acceptance_criteria: optional_edit(
                        args.acceptance_criteria.as_deref(),
                        args.clear_acceptance_criteria,
                    ),
                    dependencies,
                },
            )?;
            println!("edited work item {}", work_item.slug);
        }
        WorkCommand::Status(args) => match args.command {
            WorkStatusCommand::Set(args) => {
                let status: WorkItemStatus = args.status.into();
                if args.hold_kind.is_some() && status != WorkItemStatus::Held {
                    anyhow::bail!("--hold-kind is only valid when setting status to held");
                }
                let work_item = match (status, args.hold_kind) {
                    (WorkItemStatus::Held, Some(kind)) => hold_work_item_with_kind(
                        connection,
                        &args.slug,
                        kind.into(),
                        args.reason.as_deref(),
                    )?,
                    _ => set_work_item_status(
                        connection,
                        &args.slug,
                        status,
                        args.reason.as_deref(),
                    )?,
                };
                println!(
                    "work item {} status={}",
                    work_item.slug,
                    work_item.status.as_str()
                );
            }
        },
        WorkCommand::Delete(args) => {
            delete_work_item(connection, &args.slug)?;
            println!("deleted work item {}", args.slug);
        }
        WorkCommand::Import(args) => {
            let text = if args.path == "-" {
                let mut text = String::new();
                io::stdin().read_to_string(&mut text)?;
                text
            } else {
                fs::read_to_string(&args.path)?
            };
            let schedule: ScheduleFile = serde_json::from_str(&text)?;
            let result = import_schedule(connection, &schedule, args.upsert)?;
            println!(
                "imported schedule: created={} updated={} dependencies={}",
                result.created, result.updated, result.dependencies
            );
        }
        WorkCommand::Export(args) => {
            let schedule = export_schedule(
                connection,
                args.program.as_deref(),
                args.priority.as_deref(),
            )?;
            let text = format!("{}\n", serde_json::to_string_pretty(&schedule)?);
            if let Some(output) = args.output {
                fs::write(&output, text)?;
                println!("exported schedule to {}", output.display());
            } else {
                print!("{text}");
            }
        }
    }
    Ok(())
}

fn optional_edit(value: Option<&str>, clear: bool) -> Option<Option<&str>> {
    if clear {
        Some(None)
    } else {
        value.map(Some)
    }
}

pub fn handle_notice(connection: &rusqlite::Connection, args: NoticeArgs) -> anyhow::Result<()> {
    match args.command {
        NoticeCommand::List(args) => {
            let status = match args.status {
                CliGlobalObservationStatus::Active => Some(GlobalObservationStatus::Active),
                CliGlobalObservationStatus::Cleared => Some(GlobalObservationStatus::Cleared),
                CliGlobalObservationStatus::All => None,
            };
            let notices = list_global_observations(connection, status, checked_limit(args.limit)?)?;
            emit(args.json, &notices, |notices| {
                print_global_observations(notices)
            })?;
        }
        NoticeCommand::Add(args) => {
            let notice = add_global_observation(
                connection,
                args.kind.into(),
                &args.body,
                args.source.as_deref(),
            )?;
            println!("added global {} {}", notice.kind.as_str(), notice.id);
        }
        NoticeCommand::Edit(args) => {
            let source = if args.clear_source {
                Some(None)
            } else {
                args.source.as_deref().map(Some)
            };
            let notice = edit_global_observation(
                connection,
                args.id,
                args.kind.map(Into::into),
                args.body.as_deref(),
                source,
                args.status.map(Into::into),
            )?;
            println!(
                "edited global {} {} [{}]",
                notice.kind.as_str(),
                notice.id,
                notice.status.as_str()
            );
        }
        NoticeCommand::Clear(args) => {
            let notice = clear_global_observation(connection, args.id, args.reason.as_deref())?;
            println!(
                "cleared global {} {} [{}]",
                notice.kind.as_str(),
                notice.id,
                notice.status.as_str()
            );
        }
    }
    Ok(())
}

pub fn handle_next(connection: &rusqlite::Connection, args: NextArgs) -> anyhow::Result<()> {
    if args.commands {
        let context = crate::store::read_context(connection)?;
        for command in suggested_next_commands(&context) {
            println!("{command}");
        }
        return Ok(());
    }
    if let Some(work_item) = next_pending_work_item(connection)? {
        println!("{} {}", work_item.slug, work_item.title);
    } else {
        println!("No pending work items.");
    }
    Ok(())
}
