use std::fs;
use std::io::{self, Read};

use crate::store::{
    add_global_observation, add_work_dependency, audit_work_graph, build_work_graph,
    clear_global_observation, create_work_item_with_metadata, delete_work_item,
    dry_run_import_schedule, edit_global_observation, edit_work_item_fields, example_schedule,
    export_schedule, get_work_item_view_by_slug, hold_work_item_with_kind, import_schedule,
    list_global_observations, list_work_item_views_filtered, next_pending_work_item,
    remove_work_dependency, set_work_item_status, GlobalObservationStatus, ScheduleFile, WorkAudit,
    WorkGraph, WorkItemMetadata, WorkItemPatch, WorkItemStatus,
};

use super::super::args::{
    CliGlobalObservationStatus, CliWorkGraphFormat, NextArgs, NoticeArgs, NoticeCommand, WorkArgs,
    WorkCommand, WorkDependencyCommand, WorkStatusCommand,
};
use super::super::checked_limit;
use super::super::render::brief_context::suggested_next_commands;
use super::super::render::emit;
use super::super::render::text::{
    print_global_observations, print_work_item_view, print_work_item_views,
};

pub fn handle_work(connection: &rusqlite::Connection, args: WorkArgs) -> anyhow::Result<()> {
    match args.command {
        WorkCommand::List(args) => {
            let work_items = list_work_item_views_filtered(
                connection,
                args.status.map(WorkItemStatus::from),
                args.program.as_deref(),
                args.priority.as_deref(),
            )?;
            emit(args.json, &work_items, |work_items| {
                print_work_item_views(work_items)
            })?;
        }
        WorkCommand::Show(args) => {
            let work_item = get_work_item_view_by_slug(connection, &args.slug)?;
            emit(args.json, &work_item, print_work_item_view)?;
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
        WorkCommand::Dependency(args) => match args.command {
            WorkDependencyCommand::Add(args) => {
                add_work_dependency(connection, &args.child, &args.prerequisite)?;
                println!(
                    "added dependency: {} depends on {}",
                    args.child, args.prerequisite
                );
            }
            WorkDependencyCommand::Remove(args) => {
                remove_work_dependency(connection, &args.child, &args.prerequisite)?;
                println!(
                    "removed dependency: {} no longer depends on {}",
                    args.child, args.prerequisite
                );
            }
        },
        WorkCommand::Graph(args) => {
            let mut graph = build_work_graph(connection)?;
            if args.ready {
                graph.nodes.retain(|node| node.ready);
            } else if args.blocked {
                graph.nodes.retain(|node| {
                    !node.ready
                        && !matches!(
                            node.work_item.status,
                            WorkItemStatus::Done | WorkItemStatus::Canceled
                        )
                });
            }
            let visible = graph
                .nodes
                .iter()
                .map(|node| node.work_item.slug.as_str())
                .collect::<std::collections::BTreeSet<_>>();
            graph.edges.retain(|edge| {
                visible.contains(edge.child.as_str())
                    && visible.contains(edge.prerequisite.as_str())
            });
            match args.format {
                CliWorkGraphFormat::Human => print_work_graph(&graph),
                CliWorkGraphFormat::Json => {
                    println!("{}", serde_json::to_string_pretty(&graph)?)
                }
                CliWorkGraphFormat::Mermaid => print_work_graph_mermaid(&graph),
            }
        }
        WorkCommand::Audit(args) => {
            let audit = audit_work_graph(connection)?;
            emit(args.json, &audit, print_work_audit)?;
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
            let result = if args.dry_run {
                dry_run_import_schedule(connection, &schedule, args.upsert)?
            } else {
                import_schedule(connection, &schedule, args.upsert)?
            };
            println!(
                "{} schedule: created={} updated={} dependencies={}",
                if args.dry_run {
                    "validated"
                } else {
                    "imported"
                },
                result.created,
                result.updated,
                result.dependencies
            );
        }
        WorkCommand::Export(args) => {
            let schedule = if args.example {
                example_schedule()
            } else {
                export_schedule(
                    connection,
                    args.program.as_deref(),
                    args.priority.as_deref(),
                )?
            };
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

fn print_work_graph(graph: &WorkGraph) {
    if graph.nodes.is_empty() {
        println!("No matching work items.");
        return;
    }
    for node in &graph.nodes {
        let state = if node.ready {
            "ready"
        } else if node.work_item.status == WorkItemStatus::Pending {
            "blocked"
        } else {
            "not-ready"
        };
        println!(
            "{} [{}; {state}] {}",
            node.work_item.slug,
            node.work_item.status.as_str(),
            node.work_item.title
        );
        if node.dependencies.is_empty() {
            println!("  depends_on: none");
        } else {
            println!(
                "  depends_on: {}",
                node.dependencies
                    .iter()
                    .map(|dependency| format!(
                        "{} [{}; satisfied={}]",
                        dependency.slug,
                        dependency.status.as_str(),
                        dependency.satisfied
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }
        if !node.blocker_reasons.is_empty() {
            println!("  blocked_by: {}", node.blocker_reasons.join("; "));
        }
    }
}

fn print_work_graph_mermaid(graph: &WorkGraph) {
    println!("flowchart LR");
    for node in &graph.nodes {
        let slug = node
            .work_item
            .slug
            .replace('"', "&quot;")
            .replace('\n', " ");
        let title = node
            .work_item
            .title
            .replace('"', "&quot;")
            .replace('\n', " ");
        println!(
            "  n{}[\"{}: {} ({})\"]",
            node.work_item.id,
            slug,
            title,
            if node.ready {
                "ready"
            } else {
                node.work_item.status.as_str()
            }
        );
    }
    for edge in &graph.edges {
        let prerequisite = graph
            .nodes
            .iter()
            .find(|node| node.work_item.slug == edge.prerequisite);
        let child = graph
            .nodes
            .iter()
            .find(|node| node.work_item.slug == edge.child);
        if let (Some(prerequisite), Some(child)) = (prerequisite, child) {
            println!(
                "  n{} --> n{}",
                prerequisite.work_item.id, child.work_item.id
            );
        }
    }
}

fn print_work_audit(audit: &WorkAudit) {
    println!("work audit: {}", if audit.ok { "ok" } else { "findings" });
    println!(
        "roots: {}",
        if audit.roots.is_empty() {
            "none".to_owned()
        } else {
            audit.roots.join(", ")
        }
    );
    println!(
        "terminal_nodes: {}",
        if audit.terminal_nodes.is_empty() {
            "none".to_owned()
        } else {
            audit.terminal_nodes.join(", ")
        }
    );
    if audit.findings.is_empty() {
        println!("findings: none");
    } else {
        println!("findings:");
        for finding in &audit.findings {
            println!("- {}: {}", finding.code, finding.message);
        }
    }
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
