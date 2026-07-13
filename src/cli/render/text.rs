use crate::loop_runtime::LoopRuntimeResult;
use crate::store::{
    Artifact, ArtifactSummary, DecisionSummary, GlobalObservation, InvestigationRun,
    LoopIntervention, ObservationSummary, RunListItem, ValidationRecord, ValidationSummary,
    WorkItem,
};

use super::display_exit_code;

pub(crate) fn print_work_items(work_items: &[WorkItem]) {
    if work_items.is_empty() {
        println!("No work items.");
        return;
    }
    for work_item in work_items {
        println!(
            "{} [{}] {}",
            work_item.slug,
            work_item.status.as_str(),
            work_item.title
        );
        println!("  id={} updated_at={}", work_item.id, work_item.updated_at);
    }
}

pub(crate) fn print_work_item(work_item: &WorkItem) {
    println!("Work item: {}", work_item.slug);
    println!("id: {}", work_item.id);
    println!("status: {}", work_item.status.as_str());
    if let Some(parent_work_item_id) = work_item.parent_work_item_id {
        println!("parent_work_item_id: {parent_work_item_id}");
    }
    println!("title: {}", work_item.title);
    println!("description: {}", work_item.description);
    if let Some(priority) = &work_item.priority {
        println!("priority: {priority}");
    }
    if let Some(program) = &work_item.program {
        println!("program: {program}");
    }
    if let Some(group) = &work_item.group {
        println!("group: {group}");
    }
    if let Some(criteria) = &work_item.acceptance_criteria {
        println!("acceptance_criteria: {criteria}");
    }
    if let Some(kind) = work_item.hold_kind {
        println!("hold_kind: {}", kind.as_str());
    }
    if let Some(reason) = &work_item.hold_reason {
        println!("hold_reason: {reason}");
    }
    println!("created_at: {}", work_item.created_at);
    println!("updated_at: {}", work_item.updated_at);
}

pub(crate) fn print_runs(runs: &[RunListItem]) {
    if runs.is_empty() {
        println!("No runs.");
        return;
    }
    for run in runs {
        println!(
            "run={} [{}] work={} title={} started_at={}",
            run.run_id,
            run.status.as_str(),
            run.work_slug,
            run.work_title,
            run.started_at
        );
        if let Some(command) = &run.command {
            println!("  command: {command}");
        }
        if let Some(finished_at) = &run.finished_at {
            println!("  finished_at: {finished_at}");
        }
    }
}

pub(crate) fn print_run(run: &InvestigationRun) {
    println!("Run: {}", run.id);
    println!("work_item_id: {}", run.work_item_id);
    println!("status: {}", run.status.as_str());
    if let Some(command) = &run.command {
        println!("command: {command}");
    }
    println!("started_at: {}", run.started_at);
    if let Some(finished_at) = &run.finished_at {
        println!("finished_at: {finished_at}");
    }
    if let Some(notes) = &run.notes {
        println!("notes: {notes}");
    }
}

pub(crate) fn print_observations(observations: &[ObservationSummary]) {
    if observations.is_empty() {
        println!("No observations.");
        return;
    }
    for observation in observations {
        println!(
            "observation={} run={} work={} created_at={}",
            observation.observation_id,
            observation.run_id,
            observation.work_slug,
            observation.created_at
        );
        println!("  body: {}", observation.body);
    }
}

pub(crate) fn print_artifact(artifact: &Artifact) {
    println!("Artifact: {}", artifact.id);
    println!("kind: {}", artifact.kind.as_str());
    println!("run_id: {}", artifact.run_id);
    println!("path: {}", artifact.path.display());
    println!("description: {}", artifact.description);
    println!("created_at: {}", artifact.created_at);
}

pub(crate) fn print_artifacts(artifacts: &[ArtifactSummary]) {
    if artifacts.is_empty() {
        println!("No artifacts.");
        return;
    }
    for artifact in artifacts {
        println!(
            "artifact={} [{}] run={} work={} path={} created_at={}",
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

pub(crate) fn print_validation_record(validation: &ValidationRecord) {
    println!("Validation: {}", validation.id);
    println!("run_id: {}", validation.run_id);
    println!("outcome: {}", validation.outcome.as_str());
    if let Some(command) = &validation.command {
        println!("command: {command}");
    }
    if let Some(rationale) = &validation.rationale {
        println!("rationale: {rationale}");
    }
    println!("created_at: {}", validation.created_at);
}

pub(crate) fn print_validations(validations: &[ValidationSummary]) {
    if validations.is_empty() {
        println!("No validations.");
        return;
    }
    for validation in validations {
        println!(
            "validation={} [{}] run={} work={} created_at={}",
            validation.validation_id,
            validation.outcome.as_str(),
            validation.run_id,
            validation.work_slug,
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

pub(crate) fn print_decisions(decisions: &[DecisionSummary]) {
    if decisions.is_empty() {
        println!("No decisions.");
        return;
    }
    for decision in decisions {
        println!(
            "decision={} [{}] work={} created_at={}",
            decision.decision_id,
            decision.outcome.as_str(),
            decision.work_slug,
            decision.created_at
        );
        println!("  rationale: {}", decision.rationale);
        if let Some(next_work_slug) = &decision.next_work_slug {
            println!("  next: {next_work_slug}");
        }
    }
}

pub(crate) fn print_global_observations(global_observations: &[GlobalObservation]) {
    if global_observations.is_empty() {
        println!("No global observations.");
        return;
    }
    for observation in global_observations {
        println!(
            "- global={} kind={} status={} created_at={}",
            observation.id,
            observation.kind.as_str(),
            observation.status.as_str(),
            observation.created_at
        );
        if let Some(source) = &observation.source {
            println!("  source: {source}");
        }
        println!("  body: {}", observation.body);
    }
}

pub(crate) fn print_loop_interventions(interventions: &[LoopIntervention]) {
    if interventions.is_empty() {
        println!("No loop interventions.");
        return;
    }
    for intervention in interventions {
        println!(
            "- intervention={} action={} status={} created_at={}",
            intervention.id,
            intervention.action.as_str(),
            intervention.status.as_str(),
            intervention.created_at
        );
        println!("  reason: {}", intervention.reason);
        if let Some(instruction) = &intervention.instruction {
            println!("  instruction: {instruction}");
        }
        if let Some(requested_by) = &intervention.requested_by {
            println!("  requested_by: {requested_by}");
        }
        if let Some(run_id) = intervention.applied_run_id {
            println!("  applied_run: {run_id}");
        }
    }
}

pub(crate) fn print_loop_result(result: &LoopRuntimeResult) {
    println!(
        "loop run={} work={} prompt={}",
        result.run_id,
        result.work_slug,
        result.prompt_artifact_path.display()
    );
    if let Some(audit_path) = &result.audit_artifact_path {
        println!("audit: {}", audit_path.display());
    }
    println!(
        "agent_exit_code: {}",
        display_exit_code(result.agent_exit_code)
    );
    println!(
        "audit_exit_code: {}",
        display_exit_code(result.audit_exit_code)
    );
}
