use serde::Serialize;

use crate::store::StoreContext;

use super::display_optional_id;

const TITLE_WIDTH: usize = 160;
const COMMAND_WIDTH: usize = 200;

#[derive(Debug, Clone, Copy)]
pub(crate) struct BriefContextOptions {
    pub recent: usize,
    pub width: usize,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefContext {
    pub work_items: BriefWorkCounts,
    pub next: Option<BriefNextWork>,
    pub loop_state: BriefLoopState,
    pub active_runs: Vec<BriefActiveRun>,
    pub latest_decision: Option<BriefDecision>,
    pub latest_observations: Vec<BriefObservation>,
    pub latest_validations: Vec<BriefValidation>,
    pub handoff: BriefHandoff,
    pub next_commands: Vec<String>,
    pub brief_context_command: String,
    pub full_context_command: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefWorkCounts {
    pub pending: i64,
    pub running: i64,
    pub held: i64,
    pub done: i64,
    pub canceled: i64,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefNextWork {
    pub slug: String,
    pub title: String,
    pub description: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefLoopState {
    pub phase: String,
    pub run: String,
    pub work: String,
    pub status: String,
    pub progress: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefActiveRun {
    pub run: i64,
    pub work: String,
    pub title: String,
    pub command: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefDecision {
    pub work: String,
    pub outcome: String,
    pub rationale: String,
    pub next_work: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefObservation {
    pub run: i64,
    pub work: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefValidation {
    pub run: i64,
    pub work: String,
    pub outcome: String,
    pub command: Option<String>,
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub(crate) struct BriefHandoff {
    pub has_active_run: bool,
    pub has_next_work: bool,
    pub needs_decision: bool,
}

pub(crate) fn brief_context(context: &StoreContext, options: BriefContextOptions) -> BriefContext {
    let handoff = brief_handoff(context);
    BriefContext {
        work_items: BriefWorkCounts {
            pending: context.pending_work_items,
            running: context.running_work_items,
            held: context.held_work_items,
            done: context.done_work_items,
            canceled: context.canceled_work_items,
        },
        next: context
            .next_work_item
            .as_ref()
            .map(|work_item| BriefNextWork {
                slug: work_item.slug.clone(),
                title: compact_text(&work_item.title, TITLE_WIDTH),
                description: compact_text(&work_item.description, options.width),
            }),
        loop_state: BriefLoopState {
            phase: context.loop_state.current_phase.clone(),
            run: display_optional_id(context.loop_state.run_id),
            work: context
                .loop_state
                .work_slug
                .clone()
                .unwrap_or_else(|| "none".to_owned()),
            status: context
                .loop_state
                .terminal_status
                .map(|status| status.as_str().to_owned())
                .unwrap_or_else(|| "running".to_owned()),
            progress: compact_text(&context.loop_state.progress_report, options.width),
        },
        active_runs: context
            .active_runs
            .iter()
            .take(options.recent)
            .map(|run| BriefActiveRun {
                run: run.run_id,
                work: run.work_slug.clone(),
                title: compact_text(&run.work_title, TITLE_WIDTH),
                command: run
                    .command
                    .as_ref()
                    .map(|command| compact_text(command, COMMAND_WIDTH)),
            })
            .collect(),
        latest_decision: context
            .latest_decision
            .as_ref()
            .map(|decision| BriefDecision {
                work: decision.work_slug.clone(),
                outcome: decision.outcome.as_str().to_owned(),
                rationale: compact_text(&decision.rationale, options.width),
                next_work: decision.next_work_slug.clone(),
            }),
        latest_observations: context
            .latest_observations
            .iter()
            .take(options.recent)
            .map(|observation| BriefObservation {
                run: observation.run_id,
                work: observation.work_slug.clone(),
                body: compact_text(&observation.body, options.width),
            })
            .collect(),
        latest_validations: context
            .latest_validations
            .iter()
            .take(options.recent)
            .map(|validation| BriefValidation {
                run: validation.run_id,
                work: validation.work_slug.clone(),
                outcome: validation.outcome.as_str().to_owned(),
                command: validation
                    .command
                    .as_ref()
                    .map(|command| compact_text(command, COMMAND_WIDTH)),
                rationale: validation
                    .rationale
                    .as_ref()
                    .map(|rationale| compact_text(rationale, options.width)),
            })
            .collect(),
        handoff: handoff.clone(),
        next_commands: next_commands(context, &handoff),
        brief_context_command: "ldgr status".to_owned(),
        full_context_command: "ldgr context".to_owned(),
    }
}

pub(crate) fn print_brief_context(context: &BriefContext) {
    println!("LDGR brief context");
    println!(
        "work_items: pending={} running={} held={} done={} canceled={}",
        context.work_items.pending,
        context.work_items.running,
        context.work_items.held,
        context.work_items.done,
        context.work_items.canceled
    );
    match &context.next {
        Some(work_item) => {
            println!("next: {} {}", work_item.slug, work_item.title);
            println!("next_description: {}", work_item.description);
        }
        None => println!("next: none"),
    }
    println!(
        "loop: phase={} run={} work={} status={}",
        context.loop_state.phase,
        context.loop_state.run,
        context.loop_state.work,
        context.loop_state.status
    );
    println!("progress: {}", context.loop_state.progress);
    print_handoff(&context.handoff);
    print_active_runs(&context.active_runs);
    print_latest_decision(context.latest_decision.as_ref());
    print_latest_observations(&context.latest_observations);
    print_latest_validations(&context.latest_validations);
    print_next_commands(&context.next_commands);
    println!("brief_context: {}", context.brief_context_command);
    println!("full_context: {}", context.full_context_command);
}

fn brief_handoff(context: &StoreContext) -> BriefHandoff {
    let has_active_run = !context.active_runs.is_empty();
    BriefHandoff {
        has_active_run,
        has_next_work: context.next_work_item.is_some(),
        needs_decision: has_active_run || context.running_work_items > 0,
    }
}

fn next_commands(context: &StoreContext, handoff: &BriefHandoff) -> Vec<String> {
    if handoff.needs_decision {
        if let Some(run) = context.active_runs.first() {
            return vec![
                format!("ldgr observation add {} --body <evidence>", run.run_id),
                format!(
                    "ldgr run close {} --status <success|partial|failed> --outcome <continue|stop> --rationale <why>",
                    run.run_id
                ),
            ];
        }
        if let Some(work_slug) = &context.loop_state.work_slug {
            return vec![format!(
                "ldgr decision record {work_slug} --outcome <continue|stop> --rationale <why>"
            )];
        }
        return Vec::new();
    }
    if let Some(work_item) = &context.next_work_item {
        return vec![format!(
            "ldgr run start {} --command <what-ran>",
            work_item.slug
        )];
    }
    vec!["ldgr work create <slug> --title <title> --description <description>".to_owned()]
}

fn print_handoff(handoff: &BriefHandoff) {
    println!(
        "handoff: active_run={} next_work={} needs_decision={}",
        handoff.has_active_run, handoff.has_next_work, handoff.needs_decision
    );
}

fn print_active_runs(active_runs: &[BriefActiveRun]) {
    if active_runs.is_empty() {
        println!("active_run: none");
        return;
    }
    println!("active_runs:");
    for run in active_runs {
        println!("- run={} work={} title={}", run.run, run.work, run.title);
        if let Some(command) = &run.command {
            println!("  command: {command}");
        }
    }
}

fn print_latest_decision(decision: Option<&BriefDecision>) {
    match decision {
        Some(decision) => {
            println!(
                "latest_decision: work={} outcome={} rationale={}",
                decision.work, decision.outcome, decision.rationale
            );
            if let Some(next_work_slug) = &decision.next_work {
                println!("latest_decision_next: {next_work_slug}");
            }
        }
        None => println!("latest_decision: none"),
    }
}

fn print_latest_observations(observations: &[BriefObservation]) {
    if observations.is_empty() {
        println!("latest_observations: none");
        return;
    }
    println!("latest_observations:");
    for observation in observations {
        println!(
            "- run={} work={} body={}",
            observation.run, observation.work, observation.body
        );
    }
}

fn print_latest_validations(validations: &[BriefValidation]) {
    if validations.is_empty() {
        println!("latest_validations: none");
        return;
    }
    println!("latest_validations:");
    for validation in validations {
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

fn print_next_commands(commands: &[String]) {
    println!("next_commands:");
    for command in commands {
        println!("- {command}");
    }
}

fn compact_text(text: &str, max_chars: usize) -> String {
    let compact = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if compact.chars().count() <= max_chars {
        return compact;
    }
    let mut truncated = compact
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    truncated.push('…');
    truncated
}
