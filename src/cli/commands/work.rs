use crate::store::{
    add_global_observation, clear_global_observation, create_work_item, delete_work_item,
    edit_global_observation, edit_work_item, get_work_item_by_slug, list_global_observations,
    list_work_items, next_pending_work_item, set_work_item_status, GlobalObservationStatus,
    WorkItemStatus,
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
            let work_items = list_work_items(connection, args.status.map(WorkItemStatus::from))?;
            emit(args.json, &work_items, |work_items| {
                print_work_items(work_items)
            })?;
        }
        WorkCommand::Show(args) => {
            let work_item = get_work_item_by_slug(connection, &args.slug)?;
            emit(args.json, &work_item, print_work_item)?;
        }
        WorkCommand::Create(args) => {
            let work_item =
                create_work_item(connection, None, &args.slug, &args.title, &args.description)?;
            println!("created work item {} {}", work_item.id, work_item.slug);
        }
        WorkCommand::Edit(args) => {
            let work_item = edit_work_item(
                connection,
                &args.slug,
                args.title.as_deref(),
                args.description.as_deref(),
            )?;
            println!("edited work item {}", work_item.slug);
        }
        WorkCommand::Status(args) => match args.command {
            WorkStatusCommand::Set(args) => {
                let work_item = set_work_item_status(
                    connection,
                    &args.slug,
                    args.status.into(),
                    args.reason.as_deref(),
                )?;
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
    }
    Ok(())
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
