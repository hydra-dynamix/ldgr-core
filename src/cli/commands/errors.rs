use std::ffi::OsString;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context};
use clap::ValueEnum;

use crate::store::{
    check_error_retry_authorization, error_context_packet, error_fingerprint_inputs, get_error,
    get_error_occurrence, link_error, list_error_occurrences, list_errors, oldest_active_run,
    record_error, record_error_disposition, resolve_fingerprint, show_error, transition_error,
    ErrorContextBounds, ErrorContextPacket, ErrorDisposition, ErrorOccurrence, ErrorRecord,
    ErrorRetryAuthorization, ErrorState, ErrorView, FingerprintRequest,
    RecordErrorDispositionInput, RecordErrorInput, RecordErrorResult, RecoveryOrigin,
};

use super::super::args::{
    CliSimpleErrorType, ErrorArgs, ErrorCommand, ErrorDispositionArgs, ErrorLifecycleArgs,
    ErrorOccurrenceCommand, RecordErrorArgs,
};
use super::super::checked_limit;
use super::super::render::emit;

pub fn handle_error(connection: &rusqlite::Connection, args: ErrorArgs) -> anyhow::Result<()> {
    match args.command {
        ErrorCommand::Report(args) => handle_simple_report(connection, args),
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

#[derive(Debug, Clone, Copy)]
enum SimpleErrorAction {
    Record,
    Resolve(i64),
    Update(i64),
}

struct SimpleErrorArgs {
    action: SimpleErrorAction,
    message: String,
    legacy_identity: Option<(String, CliSimpleErrorType)>,
    json: bool,
}

fn handle_simple_report(
    connection: &rusqlite::Connection,
    external_args: Vec<OsString>,
) -> anyhow::Result<()> {
    let args = parse_simple_report(external_args)?;
    match args.action {
        SimpleErrorAction::Record => record_simple_error(connection, &args),
        SimpleErrorAction::Resolve(error_id) => {
            let error = transition_error(
                connection,
                error_id,
                ErrorState::Resolved,
                "agent",
                "ldgr-cli:raw",
                &args.message,
            )?;
            emit(args.json, &error, |error| {
                println!("resolved error {}: {}", error.id, args.message)
            })
        }
        SimpleErrorAction::Update(error_id) => update_simple_error(connection, error_id, &args),
    }
}

fn record_simple_error(
    connection: &rusqlite::Connection,
    args: &SimpleErrorArgs,
) -> anyhow::Result<()> {
    let active = oldest_active_run(connection)?;
    let occurrence_id = simple_uuid_v7()?;
    let operation_id = simple_uuid_v7()?;
    let attempt_id = simple_uuid_v7()?;
    let (failed_command, error_type, domain, code, boundary, split_key, split_rationale) =
        if let Some((failed_command, error_type)) = &args.legacy_identity {
            (
                failed_command.as_str(),
                *error_type,
                "agent.command",
                error_type.as_str(),
                "cli-shorthand",
                None,
                None,
            )
        } else {
            (
                "agent",
                CliSimpleErrorType::Task,
                "agent.report",
                "unclassified",
                "raw-text",
                Some(occurrence_id.as_str()),
                Some("raw reports remain distinct unless --update explicitly selects an aggregate"),
            )
        };
    validate_command_label(failed_command)?;
    let (class, severity, retryability) = error_type.policy();
    let fingerprint = resolve_fingerprint(FingerprintRequest {
        version: "structured-v1",
        supplied_fingerprint: None,
        override_rationale: None,
        split_key,
        split_rationale,
        class,
        domain,
        code,
        boundary: Some(boundary),
        component: Some(failed_command),
        subject: active.as_ref().map(|run| run.work_item.slug.as_str()),
    })?;
    let idempotency_key = format!("{occurrence_id}:{failed_command}");
    let observed_at = current_error_time(connection)?;
    let details = if args.legacy_identity.is_some() {
        serde_json::json!({"command": failed_command})
    } else {
        serde_json::json!({})
    };
    let environment = serde_json::json!({});
    let mut result = record_error(
        connection,
        &RecordErrorInput {
            occurrence_id: &occurrence_id,
            producer: "ldgr-cli",
            idempotency_key: &idempotency_key,
            operation_id: &operation_id,
            attempt_id: &attempt_id,
            fingerprint_version: &fingerprint.version,
            fingerprint: &fingerprint.fingerprint,
            fingerprint_inputs: Some(&fingerprint.inputs),
            fingerprint_provenance: Some(&fingerprint.provenance),
            class,
            domain,
            code,
            severity,
            retryability,
            source: "ldgr-cli:raw",
            summary: &args.message,
            details: &details,
            environment: &environment,
            observed_at: &observed_at,
            recovery_origin: RecoveryOrigin::Database,
        },
    )?;
    link_active_error_context(connection, &mut result, active.as_ref())?;

    emit(args.json, &result, |result| {
        if let Some(active) = &active {
            if args.legacy_identity.is_some() {
                println!(
                    "recorded error {} command={} type={} run={} work={}",
                    result.error.id,
                    failed_command,
                    error_type.as_str(),
                    active.run.id,
                    active.work_item.slug
                );
            } else {
                println!(
                    "recorded error {} run={} work={}",
                    result.error.id, active.run.id, active.work_item.slug
                );
            }
        } else if args.legacy_identity.is_some() {
            println!(
                "recorded error {} command={} type={}",
                result.error.id,
                failed_command,
                error_type.as_str()
            );
        } else {
            println!("recorded error {}", result.error.id);
        }
        println!("message: {}", args.message);
        if result.recurrent {
            println!("recurrence: true");
            println!("next: ldgr error context {}", result.error.id);
        }
    })
}

fn update_simple_error(
    connection: &rusqlite::Connection,
    error_id: i64,
    args: &SimpleErrorArgs,
) -> anyhow::Result<()> {
    let error = get_error(connection, error_id)?;
    let fingerprint_inputs = error_fingerprint_inputs(connection, error_id)?;
    let occurrence_id = simple_uuid_v7()?;
    let operation_id = simple_uuid_v7()?;
    let attempt_id = simple_uuid_v7()?;
    let idempotency_key = format!("{occurrence_id}:update:{error_id}");
    let observed_at = current_error_time(connection)?;
    let details = serde_json::json!({"updated_error_id": error_id});
    let environment = serde_json::json!({});
    let mut result = record_error(
        connection,
        &RecordErrorInput {
            occurrence_id: &occurrence_id,
            producer: "ldgr-cli",
            idempotency_key: &idempotency_key,
            operation_id: &operation_id,
            attempt_id: &attempt_id,
            fingerprint_version: &error.fingerprint_version,
            fingerprint: &error.fingerprint,
            fingerprint_inputs: fingerprint_inputs.as_ref(),
            fingerprint_provenance: None,
            class: error.class,
            domain: &error.domain,
            code: &error.code,
            severity: error.severity,
            retryability: error.retryability,
            source: "ldgr-cli:raw-update",
            summary: &args.message,
            details: &details,
            environment: &environment,
            observed_at: &observed_at,
            recovery_origin: RecoveryOrigin::Database,
        },
    )?;
    let active = oldest_active_run(connection)?;
    link_active_error_context(connection, &mut result, active.as_ref())?;
    emit(args.json, &result, |result| {
        println!(
            "updated error {} occurrence={}",
            result.error.id, result.occurrence.occurrence_id
        );
        println!("message: {}", args.message);
    })
}

fn link_active_error_context(
    connection: &rusqlite::Connection,
    result: &mut RecordErrorResult,
    active: Option<&crate::store::ClaimedRun>,
) -> anyhow::Result<()> {
    let Some(active) = active else {
        return Ok(());
    };
    for (entity_type, entity_id) in [("run", active.run.id), ("work_item", active.work_item.id)] {
        link_error(
            connection,
            result.error.id,
            Some(&result.occurrence.occurrence_id),
            "affected",
            entity_type,
            &entity_id.to_string(),
            "ldgr-cli:raw",
        )?;
    }
    // The store built the recurrence context before these relations existed.
    if result.context.is_some() {
        result.context = Some(error_context_packet(
            connection,
            result.error.id,
            Some(&result.occurrence.occurrence_id),
            ErrorContextBounds::default(),
        )?);
    }
    Ok(())
}

fn current_error_time(connection: &rusqlite::Connection) -> anyhow::Result<String> {
    connection
        .query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |row| {
            row.get(0)
        })
        .map_err(Into::into)
}

fn validate_command_label(failed_command: &str) -> anyhow::Result<()> {
    anyhow::ensure!(
        (1..=128).contains(&failed_command.len())
            && failed_command.chars().all(|character| {
                character.is_ascii_alphanumeric() || "-_.:".contains(character)
            }),
        "<command> must be a 1-128 character stable label using only letters, numbers, -, _, ., or :; omit command arguments"
    );
    Ok(())
}

fn parse_simple_report(external_args: Vec<OsString>) -> anyhow::Result<SimpleErrorArgs> {
    let values = external_args
        .into_iter()
        .map(|value| {
            value
                .into_string()
                .map_err(|_| anyhow!("error text must be valid UTF-8"))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let mut positional = Vec::new();
    let mut action = SimpleErrorAction::Record;
    let mut action_selected = false;
    let mut json = false;
    let mut parse_options = true;
    let mut index = 0;
    while index < values.len() {
        let value = &values[index];
        if parse_options && value == "--" {
            parse_options = false;
            index += 1;
            continue;
        }
        if parse_options && value == "--json" {
            json = true;
            index += 1;
            continue;
        }
        let parsed_action = if parse_options && matches!(value.as_str(), "--resolve" | "--update") {
            let flag = value.as_str();
            index += 1;
            let id = values
                .get(index)
                .with_context(|| format!("{flag} requires an error ID"))?;
            Some((flag, id.as_str()))
        } else if parse_options {
            value
                .split_once('=')
                .filter(|(flag, _)| matches!(*flag, "--resolve" | "--update"))
        } else {
            None
        };
        if let Some((flag, id)) = parsed_action {
            anyhow::ensure!(!action_selected, "use only one of --resolve or --update");
            let id = id
                .parse::<i64>()
                .with_context(|| format!("{flag} requires an integer error ID"))?;
            anyhow::ensure!(id > 0, "{flag} requires a positive error ID");
            action = if flag == "--resolve" {
                SimpleErrorAction::Resolve(id)
            } else {
                SimpleErrorAction::Update(id)
            };
            action_selected = true;
            index += 1;
            continue;
        }
        if parse_options && value.starts_with("--") {
            anyhow::bail!("unknown raw error option `{value}`; expected --resolve <id>, --update <id>, --json, or --");
        }
        positional.push(value.clone());
        index += 1;
    }
    anyhow::ensure!(
        !positional.is_empty(),
        "usage: ldgr error <message> [--resolve <id> | --update <id>]"
    );

    let legacy_identity = if matches!(action, SimpleErrorAction::Record) && positional.len() >= 3 {
        CliSimpleErrorType::from_str(&positional[1], true)
            .ok()
            .map(|error_type| (positional[0].clone(), error_type))
    } else {
        None
    };
    let message = if legacy_identity.is_some() {
        positional[2..].join(" ")
    } else {
        positional.join(" ")
    };
    anyhow::ensure!(
        !message.trim().is_empty() && message.len() <= 4096,
        "error explanation must contain 1-4096 characters"
    );
    Ok(SimpleErrorArgs {
        action,
        message,
        legacy_identity,
        json,
    })
}

fn simple_uuid_v7() -> anyhow::Result<String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis() as u64;
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow!("generating error occurrence identity: {error}"))?;
    bytes[..6].copy_from_slice(&millis.to_be_bytes()[2..]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Ok(format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().expect("four bytes")),
        u16::from_be_bytes(bytes[4..6].try_into().expect("two bytes")),
        u16::from_be_bytes(bytes[6..8].try_into().expect("two bytes")),
        u16::from_be_bytes(bytes[8..10].try_into().expect("two bytes")),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ])
    ))
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
