use crate::store::{
    check_error_retry_authorization, error_context_packet, get_error_occurrence, link_error,
    list_error_occurrences, list_errors, record_error, record_error_disposition,
    resolve_fingerprint, show_error, transition_error, ErrorContextBounds, ErrorContextPacket,
    ErrorDisposition, ErrorOccurrence, ErrorRecord, ErrorRetryAuthorization, ErrorState, ErrorView,
    FingerprintRequest, RecordErrorDispositionInput, RecordErrorInput, RecordErrorResult,
};

use super::super::args::{
    ErrorArgs, ErrorCommand, ErrorDispositionArgs, ErrorLifecycleArgs, ErrorOccurrenceCommand,
    RecordErrorArgs,
};
use super::super::checked_limit;
use super::super::render::emit;

pub fn handle_error(connection: &rusqlite::Connection, args: ErrorArgs) -> anyhow::Result<()> {
    match args.command {
        ErrorCommand::Record(args) => handle_record(connection, *args),
        ErrorCommand::List(args) => {
            let errors = list_errors(
                connection,
                args.state.map(ErrorState::from),
                checked_limit(args.limit)?,
            )?;
            emit(args.json, &errors, |errors| print_errors(errors))
        }
        ErrorCommand::Show(args) => {
            let view = show_error(connection, args.error_id)?;
            emit(args.json, &view, print_error_view)
        }
        ErrorCommand::Context(args) => {
            let bounds = ErrorContextBounds::uniform(args.limit)?;
            let packet = error_context_packet(
                connection,
                args.error_id,
                args.occurrence_id.as_deref(),
                bounds,
            )?;
            emit(args.json, &packet, print_context_packet)
        }
        ErrorCommand::Occurrence(args) => match args.command {
            ErrorOccurrenceCommand::List(args) => {
                let occurrences =
                    list_error_occurrences(connection, args.error_id, checked_limit(args.limit)?)?;
                emit(args.json, &occurrences, |occurrences| {
                    print_occurrences(occurrences)
                })
            }
            ErrorOccurrenceCommand::Show(args) => {
                let occurrence = get_error_occurrence(connection, &args.occurrence_id)?;
                emit(args.json, &occurrence, print_occurrence)
            }
        },
        ErrorCommand::Disposition(args) => handle_disposition(connection, args),
        ErrorCommand::RetryCheck(args) => {
            let authorization = check_error_retry_authorization(connection, args.error_id)?;
            emit(args.json, &authorization, print_retry_authorization)
        }
        ErrorCommand::Acknowledge(args) => {
            handle_lifecycle(connection, args, ErrorState::Acknowledged)
        }
        ErrorCommand::Resolve(args) => handle_lifecycle(connection, args, ErrorState::Resolved),
        ErrorCommand::Accept(args) => handle_lifecycle(connection, args, ErrorState::Accepted),
        ErrorCommand::Link(args) => {
            let relation = link_error(
                connection,
                args.error_id,
                args.occurrence_id.as_deref(),
                &args.relation_kind,
                &args.entity_type,
                &args.entity_id,
                &args.source,
            )?;
            emit(args.json, &relation, |relation| {
                println!(
                    "linked error {} {} {}:{}",
                    relation.error_id,
                    relation.relation_kind,
                    relation.entity_type,
                    relation.entity_id
                );
            })
        }
    }
}

fn print_retry_authorization(authorization: &ErrorRetryAuthorization) {
    println!(
        "retry authorized for error {} occurrence {} by disposition {}",
        authorization.error.id,
        authorization.occurrence.occurrence_id,
        authorization.disposition.id
    );
    println!(
        "prior_context: occurrences={} dispositions={} decisions={}",
        authorization.context.prior_occurrences.len(),
        authorization.context.dispositions.len(),
        authorization.context.decisions.len()
    );
}

fn handle_disposition(
    connection: &rusqlite::Connection,
    args: ErrorDispositionArgs,
) -> anyhow::Result<()> {
    let disposition = record_error_disposition(
        connection,
        &RecordErrorDispositionInput {
            error_id: args.error_id,
            occurrence_id: args.occurrence_id.as_deref(),
            action: args.action.into(),
            actor: &args.actor,
            source: &args.source,
            rationale: &args.rationale,
            decision_id: args.decision_id,
            retry_basis: args.retry_basis.map(Into::into),
            prior_disposition_id: args.prior_disposition_id,
            evidence_relation_ids: &args.evidence_relation_ids,
        },
    )?;
    emit(args.json, &disposition, print_disposition)
}

fn handle_record(connection: &rusqlite::Connection, args: RecordErrorArgs) -> anyhow::Result<()> {
    let details = parse_object("--details", &args.details)?;
    let environment = parse_object("--environment", &args.environment)?;
    let class = args.class.into();
    let fingerprint = resolve_fingerprint(FingerprintRequest {
        version: &args.fingerprint_version,
        supplied_fingerprint: args.fingerprint.as_deref(),
        override_rationale: args.fingerprint_override_rationale.as_deref(),
        split_key: args.fingerprint_split.as_deref(),
        split_rationale: args.fingerprint_split_rationale.as_deref(),
        class,
        domain: &args.domain,
        code: &args.code,
        boundary: args.boundary.as_deref(),
        component: args.component.as_deref(),
        subject: args.subject.as_deref(),
    })?;
    let result = record_error(
        connection,
        &RecordErrorInput {
            occurrence_id: &args.occurrence_id,
            producer: &args.producer,
            idempotency_key: &args.idempotency_key,
            operation_id: &args.operation_id,
            attempt_id: &args.attempt_id,
            fingerprint_version: &fingerprint.version,
            fingerprint: &fingerprint.fingerprint,
            fingerprint_inputs: Some(&fingerprint.inputs),
            fingerprint_provenance: Some(&fingerprint.provenance),
            class,
            domain: &args.domain,
            code: &args.code,
            severity: args.severity.into(),
            retryability: args.retryability.into(),
            source: &args.source,
            summary: &args.summary,
            details: &details,
            environment: &environment,
            observed_at: &args.observed_at,
            recovery_origin: args.recovery_origin.into(),
        },
    )?;
    emit(args.json, &result, print_record_result)
}

fn parse_object(name: &str, value: &str) -> anyhow::Result<serde_json::Value> {
    let parsed: serde_json::Value = serde_json::from_str(value)
        .map_err(|error| anyhow::anyhow!("{name} must be valid JSON: {error}"))?;
    anyhow::ensure!(parsed.is_object(), "{name} must be a JSON object");
    Ok(parsed)
}

fn handle_lifecycle(
    connection: &rusqlite::Connection,
    args: ErrorLifecycleArgs,
    state: ErrorState,
) -> anyhow::Result<()> {
    let error = transition_error(
        connection,
        args.error_id,
        state,
        &args.actor,
        &args.source,
        &args.rationale,
    )?;
    emit(args.json, &error, |error| {
        println!("error {} [{}]", error.id, error.state);
    })
}

fn print_record_result(result: &RecordErrorResult) {
    let replay = if result.idempotent_replay {
        " idempotent-replay"
    } else {
        ""
    };
    println!(
        "recorded occurrence {} for error {} [{}] count={}{}",
        result.occurrence.occurrence_id,
        result.error.id,
        result.error.state,
        result.error.occurrence_count,
        replay
    );
    if result.recurrent {
        println!("recurrence: true");
        if let Some(gate) = &result.retry_gate {
            println!("retry_gate: {gate}");
        }
        if let Some(context) = &result.context {
            println!(
                "context: prior={} work={} runs={} decisions={} artifacts={} validations={} environment_differences={}",
                context.prior_occurrences.len(),
                context.related_work_items.len(),
                context.related_runs.len(),
                context.decisions.len(),
                context.artifacts.len(),
                context.validations.len(),
                context.environment_differences.len(),
            );
            if context.sections.values().any(|section| section.truncated) {
                println!("context_truncated: true");
            }
        }
    }
}

fn print_context_packet(packet: &ErrorContextPacket) {
    println!(
        "Error {} recurrence context for {}",
        packet.error.id, packet.current_occurrence_id
    );
    println!(
        "repeated: {} occurrences={} first_seen={} last_seen={}",
        packet.repeated,
        packet.error.occurrence_count,
        packet.error.first_seen_at,
        packet.error.last_seen_at
    );
    println!(
        "included: prior={} work={} runs={} dispositions={} decisions={} artifacts={} validations={} environment_differences={}",
        packet.prior_occurrences.len(),
        packet.related_work_items.len(),
        packet.related_runs.len(),
        packet.dispositions.len(),
        packet.decisions.len(),
        packet.artifacts.len(),
        packet.validations.len(),
        packet.environment_differences.len(),
    );
    let truncated = packet
        .sections
        .iter()
        .filter(|(_, section)| section.truncated)
        .map(|(name, _)| name.as_str())
        .collect::<Vec<_>>();
    if !truncated.is_empty() {
        println!("truncated: {}", truncated.join(","));
    }
    println!(
        "redaction: sensitive={} home_paths={} truncated_values={}",
        packet.redaction.sensitive_values,
        packet.redaction.home_paths,
        packet.redaction.truncated_values
    );
}

fn print_errors(errors: &[ErrorRecord]) {
    if errors.is_empty() {
        println!("No errors.");
        return;
    }
    for error in errors {
        println!(
            "{} [{}] {}/{} severity={} occurrences={} latest={} disposition_pending={}",
            error.id,
            error.state,
            error.domain,
            error.code,
            error.severity,
            error.occurrence_count,
            error.last_seen_at,
            error.disposition_pending
        );
    }
}

fn print_error_view(view: &ErrorView) {
    let error = &view.error;
    println!("Error {}", error.id);
    println!("state: {}", error.state);
    println!("class: {}", error.class);
    println!("identity: {}/{}", error.domain, error.code);
    println!(
        "fingerprint: {} {}",
        error.fingerprint_version, error.fingerprint
    );
    println!("severity: {}", error.severity);
    println!("retryability: {}", error.retryability);
    println!("occurrence_count: {}", error.occurrence_count);
    println!("latest_occurrence_id: {}", error.latest_occurrence_id);
    println!("disposition_pending: {}", error.disposition_pending);
    println!("occurrences:");
    print_occurrences(&view.occurrences);
    if !view.relations.is_empty() {
        println!("relations:");
        for relation in &view.relations {
            println!(
                "- {} {}:{} source={}",
                relation.relation_kind, relation.entity_type, relation.entity_id, relation.source
            );
        }
    }
    if !view.transitions.is_empty() {
        println!("transitions:");
        for transition in &view.transitions {
            println!(
                "- {} -> {} actor={} rationale={}",
                transition.old_state, transition.new_state, transition.actor, transition.rationale
            );
        }
    }
    if !view.dispositions.is_empty() {
        println!("dispositions:");
        for disposition in &view.dispositions {
            print_disposition(disposition);
        }
    }
}

fn print_disposition(disposition: &ErrorDisposition) {
    println!(
        "- disposition {} error={} occurrence={} action={} actor={} transition={} rationale={}",
        disposition.id,
        disposition.error_id,
        disposition.occurrence_id,
        disposition.action,
        disposition.actor,
        disposition.resulting_work_transition,
        disposition.rationale
    );
    if let Some(basis) = disposition.retry_basis {
        println!(
            "  retry_basis={} prior_disposition={}",
            basis,
            disposition
                .prior_disposition_id
                .map(|id| id.to_string())
                .unwrap_or_else(|| "none".to_owned())
        );
    }
}

fn print_occurrences(occurrences: &[ErrorOccurrence]) {
    if occurrences.is_empty() {
        println!("No error occurrences.");
        return;
    }
    for occurrence in occurrences {
        println!(
            "- {} error={} observed={} producer={} source={} summary={}",
            occurrence.occurrence_id,
            occurrence.error_id,
            occurrence.observed_at,
            occurrence.producer,
            occurrence.source,
            occurrence.summary
        );
    }
}

fn print_occurrence(occurrence: &ErrorOccurrence) {
    println!("Occurrence {}", occurrence.occurrence_id);
    println!("error_id: {}", occurrence.error_id);
    println!("producer: {}", occurrence.producer);
    println!("idempotency_key: {}", occurrence.idempotency_key);
    println!("operation_id: {}", occurrence.operation_id);
    println!("attempt_id: {}", occurrence.attempt_id);
    println!("class: {}", occurrence.class);
    println!("identity: {}/{}", occurrence.domain, occurrence.code);
    println!("severity: {}", occurrence.severity);
    println!("retryability: {}", occurrence.retryability);
    println!("source: {}", occurrence.source);
    println!("summary: {}", occurrence.summary);
    println!("observed_at: {}", occurrence.observed_at);
    println!("recorded_at: {}", occurrence.recorded_at);
    println!("recovery_origin: {}", occurrence.recovery_origin);
}
