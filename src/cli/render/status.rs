use std::collections::BTreeMap;

use anyhow::Context;
use serde::Serialize;

use crate::store::{
    last_completed_work_item, list_decisions, list_observations_for_work,
    list_validation_records_for_work, list_work_items_filtered, next_ready_work_item,
    work_readiness, DecisionSummary, ObservationSummary, StoreContext, ValidationSummary, WorkItem,
    WorkItemStatus,
};

use super::brief_context::{
    brief_context, BriefActiveRun, BriefAdapterNamespace, BriefContext, BriefContextOptions,
    BriefDecision, BriefHandoff, BriefLoopState, BriefObservation, BriefValidation,
};

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusSummary {
    pub state: String,
    pub filters: StatusFilters,
    pub work_items: StatusWorkCounts,
    pub next: Option<StatusNextWork>,
    pub queue_by_priority: BTreeMap<String, usize>,
    pub queue_by_program: BTreeMap<String, usize>,
    pub held_by_reason: BTreeMap<String, usize>,
    pub last_completed: Option<String>,
    pub history_scope: Option<String>,
    pub observations: Vec<ObservationSummary>,
    pub validations: Vec<ValidationSummary>,
    pub decision: Option<DecisionSummary>,
    pub loop_state: BriefLoopState,
    pub active_runs: Vec<BriefActiveRun>,
    pub installed_adapter_namespaces: Vec<BriefAdapterNamespace>,
    pub handoff: BriefHandoff,
    pub next_commands: Vec<String>,
    pub brief_context_command: String,
    pub full_context_command: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub global_history: Option<StatusGlobalHistory>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusGlobalHistory {
    pub loop_state: BriefLoopState,
    pub active_runs: Vec<BriefActiveRun>,
    pub latest_decision: Option<BriefDecision>,
    pub latest_observations: Vec<BriefObservation>,
    pub latest_validations: Vec<BriefValidation>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusFilters {
    pub program: Option<String>,
    pub priority: Option<String>,
}

#[derive(Debug, Clone, Copy, Serialize)]
pub(crate) struct StatusWorkCounts {
    pub pending: usize,
    pub running: usize,
    pub held: usize,
    pub done: usize,
    pub canceled: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct StatusNextWork {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub priority: Option<String>,
    pub program: Option<String>,
    pub group: Option<String>,
    pub acceptance_criteria: Option<String>,
    pub ready: bool,
    pub dependencies: Vec<String>,
    pub blocked_by: Vec<String>,
    pub unblocks: Vec<String>,
}

pub(crate) fn build_status_summary(
    connection: &rusqlite::Connection,
    context: &StoreContext,
    program: Option<&str>,
    priority: Option<&str>,
    recent: usize,
    width: usize,
    full: bool,
) -> anyhow::Result<StatusSummary> {
    let work_items = list_work_items_filtered(connection, None, program, priority)?;
    let counts = work_counts(&work_items);
    let ready_item = next_ready_work_item(connection, program, priority)?;
    let next_item = ready_item.clone().or_else(|| {
        work_items
            .iter()
            .find(|item| item.status == WorkItemStatus::Pending)
            .cloned()
    });
    let next = next_item
        .as_ref()
        .map(|work_item| status_next(connection, work_item, width, full))
        .transpose()?;
    let state = if counts.running > 0 {
        "running".to_owned()
    } else if next.as_ref().is_some_and(|item| item.ready) {
        "idle, work available".to_owned()
    } else if counts.pending > 0 {
        "idle, work blocked".to_owned()
    } else if counts.held > 0 {
        "idle, held work only".to_owned()
    } else {
        "idle, queue empty".to_owned()
    };
    let target_slug = work_items
        .iter()
        .find(|item| item.status == WorkItemStatus::Running)
        .map(|item| item.slug.clone())
        .or_else(|| next_item.as_ref().map(|item| item.slug.clone()));
    let limit = i64::try_from(recent.min(50)).context("recent limit does not fit in i64")?;
    let (observations, validations, decision) = match target_slug.as_deref() {
        Some(slug) => (
            list_observations_for_work(connection, slug, limit)?,
            list_validation_records_for_work(connection, slug, limit)?,
            list_decisions(connection, Some(slug), 1)?
                .into_iter()
                .next(),
        ),
        None => (Vec::new(), Vec::new(), None),
    };
    let last_completed =
        last_completed_work_item(connection, program, priority)?.map(|item| item.slug);
    let brief = brief_context(
        context,
        BriefContextOptions {
            recent: recent.min(50),
            width: width.clamp(40, 2000),
        },
    );
    let global_history = full.then(|| StatusGlobalHistory {
        loop_state: brief.loop_state.clone(),
        active_runs: brief.active_runs.clone(),
        latest_decision: brief.latest_decision.clone(),
        latest_observations: brief.latest_observations.clone(),
        latest_validations: brief.latest_validations.clone(),
    });
    let filtered = program.is_some() || priority.is_some();
    let handoff = if filtered {
        BriefHandoff {
            has_active_run: counts.running > 0,
            has_next_work: next.is_some(),
            needs_decision: counts.running > 0,
        }
    } else {
        brief.handoff.clone()
    };
    let loop_state = if filtered {
        filtered_loop_state(&work_items, next.as_ref(), &brief)
    } else {
        brief.loop_state.clone()
    };
    let next_commands = if filtered {
        filtered_next_commands(&work_items, next.as_ref(), &brief)
    } else {
        brief.next_commands.clone()
    };
    Ok(StatusSummary {
        state,
        filters: StatusFilters {
            program: program.map(str::to_owned),
            priority: priority.map(|value| value.to_ascii_uppercase()),
        },
        work_items: counts,
        next,
        queue_by_priority: queue_by(&work_items, |item| {
            item.priority.as_deref().unwrap_or("unprioritized")
        }),
        queue_by_program: queue_by(&work_items, |item| {
            item.program.as_deref().unwrap_or("unassigned")
        }),
        held_by_reason: held_by_reason(&work_items),
        last_completed,
        history_scope: target_slug,
        observations,
        validations,
        decision,
        loop_state,
        active_runs: brief.active_runs.clone(),
        installed_adapter_namespaces: brief.installed_adapter_namespaces.clone(),
        handoff,
        next_commands,
        brief_context_command: brief.brief_context_command.clone(),
        full_context_command: brief.full_context_command.clone(),
        global_history,
    })
}

fn filtered_loop_state(
    work_items: &[WorkItem],
    next: Option<&StatusNextWork>,
    brief: &BriefContext,
) -> BriefLoopState {
    if let Some(running) = work_items
        .iter()
        .find(|item| item.status == WorkItemStatus::Running)
    {
        if brief.loop_state.work == running.slug {
            return brief.loop_state.clone();
        }
        return BriefLoopState {
            phase: "running".to_owned(),
            run: "none".to_owned(),
            work: running.slug.clone(),
            status: "running".to_owned(),
            progress: format!("Filtered work item {} is running.", running.slug),
        };
    }
    let progress = match next {
        Some(next) if next.ready => format!("Ready to start filtered work item {}.", next.slug),
        Some(next) => format!(
            "Filtered work item {} is blocked by {}.",
            next.slug,
            next.blocked_by.join(", ")
        ),
        None => "No work matches the active status filters.".to_owned(),
    };
    BriefLoopState {
        phase: "idle".to_owned(),
        run: "none".to_owned(),
        work: "none".to_owned(),
        status: "idle".to_owned(),
        progress,
    }
}

fn filtered_next_commands(
    work_items: &[WorkItem],
    next: Option<&StatusNextWork>,
    brief: &BriefContext,
) -> Vec<String> {
    let mut commands = brief
        .installed_adapter_namespaces
        .iter()
        .map(|namespace| namespace.help_command.clone())
        .collect::<Vec<_>>();
    if let Some(running) = work_items
        .iter()
        .find(|item| item.status == WorkItemStatus::Running)
    {
        if brief.active_runs.iter().any(|run| run.work == running.slug) {
            commands.push(format!("ldgr observe {} --body <evidence>", running.slug));
            commands.push(format!(
                "ldgr run close {} --status <success|partial|failed> --outcome <continue|stop> --rationale <why>",
                running.slug
            ));
        } else {
            commands.push(format!(
                "ldgr decision record {} --outcome <continue|stop> --rationale <why>",
                running.slug
            ));
        }
    } else if let Some(next) = next {
        if next.ready {
            commands.push(format!("ldgr run start {} --command <what-ran>", next.slug));
        } else {
            commands.push(format!("ldgr work show {}", next.slug));
            commands.extend(
                next.blocked_by
                    .iter()
                    .map(|slug| format!("ldgr work show {slug}")),
            );
        }
    } else {
        commands
            .push("ldgr work create <slug> --title <title> --description <description>".to_owned());
    }
    commands.sort();
    commands.dedup();
    commands
}

fn status_next(
    connection: &rusqlite::Connection,
    work_item: &WorkItem,
    width: usize,
    full: bool,
) -> anyhow::Result<StatusNextWork> {
    let readiness = work_readiness(connection, &work_item.slug)?;
    Ok(StatusNextWork {
        slug: work_item.slug.clone(),
        title: work_item.title.clone(),
        description: if full {
            work_item.description.clone()
        } else {
            compact_text(&work_item.description, width.clamp(40, 2000))
        },
        priority: work_item.priority.clone(),
        program: work_item.program.clone(),
        group: work_item.group.clone(),
        acceptance_criteria: full
            .then(|| work_item.acceptance_criteria.clone())
            .flatten(),
        ready: readiness.ready,
        dependencies: readiness.dependencies,
        blocked_by: readiness.blocked_by,
        unblocks: readiness.unblocks,
    })
}

fn work_counts(work_items: &[WorkItem]) -> StatusWorkCounts {
    let count = |status| {
        work_items
            .iter()
            .filter(|item| item.status == status)
            .count()
    };
    StatusWorkCounts {
        pending: count(WorkItemStatus::Pending),
        running: count(WorkItemStatus::Running),
        held: count(WorkItemStatus::Held),
        done: count(WorkItemStatus::Done),
        canceled: count(WorkItemStatus::Canceled),
    }
}

fn queue_by(work_items: &[WorkItem], key: impl Fn(&WorkItem) -> &str) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in work_items
        .iter()
        .filter(|item| item.status == WorkItemStatus::Pending)
    {
        *counts.entry(key(item).to_owned()).or_default() += 1;
    }
    counts
}

fn held_by_reason(work_items: &[WorkItem]) -> BTreeMap<String, usize> {
    let mut counts = BTreeMap::new();
    for item in work_items
        .iter()
        .filter(|item| item.status == WorkItemStatus::Held)
    {
        let kind = item
            .hold_kind
            .map(|kind| kind.as_str())
            .unwrap_or("blocked");
        *counts.entry(kind.to_owned()).or_default() += 1;
    }
    counts
}

pub(crate) fn print_status_summary(summary: &StatusSummary) {
    println!("LDGR brief context");
    println!("state: {}", summary.state);
    if summary.filters.program.is_some() || summary.filters.priority.is_some() {
        println!(
            "filters: program={} priority={}",
            summary.filters.program.as_deref().unwrap_or("all"),
            summary.filters.priority.as_deref().unwrap_or("all")
        );
    }
    println!(
        "work_items: pending={} running={} held={} done={} canceled={}",
        summary.work_items.pending,
        summary.work_items.running,
        summary.work_items.held,
        summary.work_items.done,
        summary.work_items.canceled
    );
    if let Some(next) = &summary.next {
        let priority = next
            .priority
            .as_ref()
            .map(|value| format!(" [{value}]"))
            .unwrap_or_default();
        println!("next: {}{} {}", next.slug, priority, next.title);
        println!("ready: {}", if next.ready { "yes" } else { "no" });
        println!(
            "dependencies: {}",
            if next.dependencies.is_empty() {
                "none declared".to_owned()
            } else if next.blocked_by.is_empty() {
                "satisfied".to_owned()
            } else {
                "unsatisfied".to_owned()
            }
        );
        println!(
            "blocked_by: {}",
            if next.blocked_by.is_empty() {
                "none".to_owned()
            } else {
                next.blocked_by.join(", ")
            }
        );
        println!(
            "unblocks: {}",
            if next.unblocks.is_empty() {
                "none".to_owned()
            } else {
                next.unblocks.join(", ")
            }
        );
        println!("next_description: {}", next.description);
        if let Some(criteria) = &next.acceptance_criteria {
            println!("acceptance_criteria: {criteria}");
        }
    } else {
        println!("next: none ready");
    }
    println!("queue: {}", render_counts(&summary.queue_by_priority));
    if summary.queue_by_program.len() > 1 || !summary.queue_by_program.contains_key("unassigned") {
        println!("programs: {}", render_counts(&summary.queue_by_program));
    }
    println!("held: {}", render_counts(&summary.held_by_reason));
    println!(
        "last_completed: {}",
        summary.last_completed.as_deref().unwrap_or("none")
    );
    if summary.loop_state.phase != "idle" {
        println!(
            "loop: phase={} run={} work={} status={}",
            summary.loop_state.phase,
            summary.loop_state.run,
            summary.loop_state.work,
            summary.loop_state.status
        );
    }
    if let Some(scope) = &summary.history_scope {
        print_scoped_history(summary, scope);
    }
    if let Some(global_history) = &summary.global_history {
        println!();
        println!("global_history:");
        print_global_history(global_history);
    } else {
        println!("full_status: ldgr status --full");
    }
    print_operational_handoff(summary);
}

fn print_global_history(history: &StatusGlobalHistory) {
    println!(
        "loop: phase={} run={} work={} status={}",
        history.loop_state.phase,
        history.loop_state.run,
        history.loop_state.work,
        history.loop_state.status
    );
    println!("progress: {}", history.loop_state.progress);
    if !history.active_runs.is_empty() {
        println!("active_runs:");
        for run in &history.active_runs {
            println!("- run={} work={} title={}", run.run, run.work, run.title);
            if let Some(command) = &run.command {
                println!("  command: {command}");
            }
        }
    }
    if let Some(decision) = &history.latest_decision {
        println!(
            "latest_decision: work={} outcome={} rationale={}",
            decision.work, decision.outcome, decision.rationale
        );
        if let Some(next_work) = &decision.next_work {
            println!("  next_work: {next_work}");
        }
    }
    if !history.latest_observations.is_empty() {
        println!("latest_observations:");
        for observation in &history.latest_observations {
            println!(
                "- run={} work={} body={}",
                observation.run, observation.work, observation.body
            );
        }
    }
    if !history.latest_validations.is_empty() {
        println!("latest_validations:");
        for validation in &history.latest_validations {
            println!(
                "- run={} work={} outcome={}",
                validation.run, validation.work, validation.outcome
            );
            if let Some(command) = &validation.command {
                println!("  command: {command}");
            }
            if let Some(rationale) = &validation.rationale {
                println!("  rationale: {rationale}");
            }
        }
    }
}

fn print_scoped_history(summary: &StatusSummary, scope: &str) {
    if summary.observations.is_empty()
        && summary.validations.is_empty()
        && summary.decision.is_none()
    {
        return;
    }
    println!("history_scope: {scope}");
    if !summary.observations.is_empty() {
        println!("latest_observations:");
        for observation in &summary.observations {
            println!("- observation: {}", observation.body);
        }
    }
    if !summary.validations.is_empty() {
        println!("latest_validations:");
        for validation in &summary.validations {
            println!("- validation: outcome={}", validation.outcome.as_str());
            if let Some(command) = &validation.command {
                println!("  command: {command}");
            }
            if let Some(rationale) = &validation.rationale {
                println!("  rationale: {rationale}");
            }
        }
    }
    if let Some(decision) = &summary.decision {
        println!(
            "decision: outcome={} rationale={}",
            decision.outcome.as_str(),
            decision.rationale
        );
    }
}

fn render_counts(counts: &BTreeMap<String, usize>) -> String {
    if counts.is_empty() {
        return "none".to_owned();
    }
    counts
        .iter()
        .map(|(key, count)| format!("{key}={count}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.chars().count() <= max_chars {
        return normalized;
    }
    let mut compact = normalized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    compact.push('…');
    compact
}

fn print_operational_handoff(summary: &StatusSummary) {
    println!(
        "handoff: active_run={} next_work={} needs_decision={}",
        summary.handoff.has_active_run,
        summary.handoff.has_next_work,
        summary.handoff.needs_decision
    );
    if !summary.installed_adapter_namespaces.is_empty() {
        println!("installed_adapter_namespaces:");
        for namespace in &summary.installed_adapter_namespaces {
            println!(
                "- adapter={} namespace={} command={} help_command={}",
                namespace.adapter, namespace.namespace, namespace.command, namespace.help_command
            );
            println!("  instruction: {}", namespace.instruction);
        }
    }
    println!("next_commands:");
    for command in &summary.next_commands {
        println!("- {command}");
    }
    println!("brief_context: {}", summary.brief_context_command);
    println!("full_context: {}", summary.full_context_command);
}
