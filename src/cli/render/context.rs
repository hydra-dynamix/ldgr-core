use crate::store::StoreContext;

use super::display_optional_id;
use super::text::{print_global_observations, print_loop_interventions};

pub(crate) fn print_context(context: &StoreContext) {
    println!("LDGR context");
    println!(
        "work_items: pending={} running={} held={} done={} canceled={}",
        context.pending_work_items,
        context.running_work_items,
        context.held_work_items,
        context.done_work_items,
        context.canceled_work_items
    );
    match &context.next_work_item {
        Some(work_item) => {
            println!("next: {} {}", work_item.slug, work_item.title);
            println!("next_description: {}", work_item.description);
        }
        None => println!("next: none"),
    }

    println!(
        "loop_state: phase={} run={} work={} status={}",
        context.loop_state.current_phase,
        display_optional_id(context.loop_state.run_id),
        context.loop_state.work_slug.as_deref().unwrap_or("none"),
        context
            .loop_state
            .terminal_status
            .map(|status| status.as_str())
            .unwrap_or("running")
    );
    println!("loop_progress: {}", context.loop_state.progress_report);
    if !context.loop_state.recent_cycle_narrative.is_empty() {
        println!("loop_narrative:");
        for entry in &context.loop_state.recent_cycle_narrative {
            println!(
                "- phase={} created_at={}",
                entry.phase.as_deref().unwrap_or("none"),
                entry.created_at
            );
            println!("  message: {}", entry.message);
        }
    }

    if context.active_runs.is_empty() {
        println!("active_runs: none");
    } else {
        println!("active_runs:");
        for run in &context.active_runs {
            println!(
                "- run={} work={} title={} started_at={}",
                run.run_id, run.work_slug, run.work_title, run.started_at
            );
            if let Some(command) = &run.command {
                println!("  command: {command}");
            }
        }
    }

    match &context.latest_decision {
        Some(decision) => {
            println!(
                "latest_decision: id={} work={} outcome={} created_at={}",
                decision.decision_id,
                decision.work_slug,
                decision.outcome.as_str(),
                decision.created_at
            );
            println!("latest_decision_rationale: {}", decision.rationale);
            if let Some(next_work_slug) = &decision.next_work_slug {
                println!("latest_decision_next: {next_work_slug}");
            }
        }
        None => println!("latest_decision: none"),
    }

    if context.latest_observations.is_empty() {
        println!("latest_observations: none");
    } else {
        println!("latest_observations:");
        for observation in &context.latest_observations {
            println!(
                "- observation={} run={} work={} created_at={}",
                observation.observation_id,
                observation.run_id,
                observation.work_slug,
                observation.created_at
            );
            println!("  body: {}", observation.body);
        }
    }

    if context.latest_validations.is_empty() {
        println!("latest_validations: none");
    } else {
        println!("latest_validations:");
        for validation in &context.latest_validations {
            println!(
                "- validation={} run={} work={} outcome={} created_at={}",
                validation.validation_id,
                validation.run_id,
                validation.work_slug,
                validation.outcome.as_str(),
                validation.created_at
            );
            if let Some(command) = &validation.command {
                println!("  command: {command}");
            }
            if let Some(rationale) = &validation.rationale {
                println!("  rationale: {rationale}");
            }
        }
    }

    if context.global_observations.is_empty() {
        println!("global_observations: none");
    } else {
        println!("global_observations:");
        print_global_observations(&context.global_observations);
    }

    if context.latest_artifacts.is_empty() {
        println!("latest_artifacts: none");
    } else {
        println!("latest_artifacts:");
        for artifact in &context.latest_artifacts {
            println!(
                "- artifact={} kind={} run={} work={} path={} created_at={}",
                artifact.artifact_id,
                artifact.kind.as_str(),
                artifact.run_id,
                artifact.work_slug,
                artifact.path.display(),
                artifact.created_at
            );
            println!("  description: {}", artifact.description);
        }
    }

    if context.loop_interventions.is_empty() {
        println!("loop_interventions: none");
    } else {
        println!("loop_interventions:");
        print_loop_interventions(&context.loop_interventions);
    }

    if context.latest_events.is_empty() {
        println!("latest_events: none");
    } else {
        println!("latest_events:");
        for event in &context.latest_events {
            println!(
                "- event={} entity={}:{} type={} created_at={}",
                event.event_id,
                event.entity_type,
                event.entity_id,
                event.event_type,
                event.created_at
            );
            println!("  payload: {}", event.payload_json);
        }
    }
}
