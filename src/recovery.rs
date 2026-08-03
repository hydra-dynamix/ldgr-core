use std::env;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, bail, Context, Result};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

use crate::store::{
    in_write_transaction, link_error, record_error, ErrorClass, ErrorRetryability, ErrorSeverity,
    FingerprintProvenance, FingerprintProvenanceKind, RecordErrorInput, RecoveryOrigin,
};

static UNIQUE_COUNTER: AtomicU64 = AtomicU64::new(0);

pub const OPERATION_ID_ENV: &str = "LDGR_EXECUTION_OPERATION_ID";
pub const ATTEMPT_ID_ENV: &str = "LDGR_EXECUTION_ATTEMPT_ID";
pub const INTENT_PATH_ENV: &str = "LDGR_EXECUTION_INTENT_PATH";
pub const NO_SINK_DIAGNOSTIC: &str =
    "FATAL: no durable LDGR recovery sink is writable; worker execution was not started";
const LEGACY_INTENT_STALE_SECONDS: u64 = 300;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExecutionAttempt {
    pub operation_id: String,
    pub attempt_id: String,
    observed_at: String,
    project_root: PathBuf,
    intent_path: PathBuf,
}

#[derive(Debug, Clone, Copy)]
pub enum FailureKind {
    CoreUnavailable,
    Initialization,
    Spawn,
    ExitCode(i32),
    Signal(i32),
    UnexpectedDisappearance,
}

#[derive(Debug, Serialize)]
struct ProjectRef {
    project_id: Option<String>,
    locator: String,
    database_identity: Option<String>,
}

#[derive(Debug, Serialize)]
struct IntentEnvelope<'a> {
    format: &'static str,
    schema_version: u32,
    project: &'a ProjectRef,
    producer: &'static str,
    operation_id: &'a str,
    attempt_id: &'a str,
    boundary: &'static str,
    subject: &'static str,
    environment: Value,
    accepted_at: String,
    process_id: u32,
}

#[derive(Debug, Serialize)]
struct FingerprintInputs {
    class: &'static str,
    domain: &'static str,
    code: &'static str,
    boundary: &'static str,
    component: &'static str,
    subject: &'static str,
}

#[derive(Debug, Serialize)]
struct RecoveryFingerprint {
    version: &'static str,
    value: String,
    inputs: FingerprintInputs,
}

#[derive(Debug, Serialize)]
struct RecoveryError {
    class: &'static str,
    domain: &'static str,
    code: &'static str,
    severity: &'static str,
    retryability: &'static str,
    source: &'static str,
    summary: &'static str,
    details: Value,
    environment: Value,
}

#[derive(Debug, Serialize)]
struct RecoveryEnvelope<'a> {
    format: &'static str,
    schema_version: u32,
    project: &'a ProjectRef,
    producer: &'static str,
    idempotency_key: String,
    operation_id: &'a str,
    attempt_id: &'a str,
    occurrence_id: String,
    fingerprint: RecoveryFingerprint,
    error: RecoveryError,
    observed_at: String,
}

#[derive(Debug, Default, Clone, Serialize)]
pub struct StartupRecoveryReport {
    pub imported: usize,
    pub idempotent_replays: usize,
    pub archived: usize,
    pub quarantined: usize,
    pub skipped_other_projects: usize,
    pub live_attempts: usize,
    pub interrupted_attempts: usize,
    pub restored_runs: usize,
    pub blocking_error_ids: Vec<i64>,
    pub diagnostics: Vec<String>,
}

impl StartupRecoveryReport {
    pub fn changed(&self) -> bool {
        self.imported > 0
            || self.idempotent_replays > 0
            || self.archived > 0
            || self.quarantined > 0
            || self.interrupted_attempts > 0
            || self.restored_runs > 0
    }

    pub fn requires_disposition(&self) -> bool {
        !self.blocking_error_ids.is_empty()
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryProject {
    project_id: Option<String>,
    locator: String,
    #[serde(rename = "database_identity")]
    _database_identity: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryFingerprintOwned {
    version: String,
    value: String,
    inputs: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryErrorOwned {
    class: String,
    domain: String,
    code: String,
    severity: String,
    retryability: String,
    source: String,
    summary: String,
    details: Value,
    environment: Value,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RecoveryEnvelopeOwned {
    format: String,
    schema_version: u32,
    project: RecoveryProject,
    producer: String,
    idempotency_key: String,
    operation_id: String,
    attempt_id: String,
    occurrence_id: String,
    fingerprint: RecoveryFingerprintOwned,
    error: RecoveryErrorOwned,
    observed_at: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IntentEnvelopeOwned {
    format: String,
    schema_version: u32,
    project: RecoveryProject,
    producer: String,
    operation_id: String,
    attempt_id: String,
    boundary: String,
    subject: String,
    environment: Value,
    accepted_at: String,
    #[serde(default)]
    process_id: Option<u32>,
}

impl ExecutionAttempt {
    pub fn begin_or_adopt(project_root: &Path) -> Result<Self> {
        if let (Some(operation_id), Some(attempt_id), Some(intent_path)) = (
            nonempty_var(OPERATION_ID_ENV),
            nonempty_var(ATTEMPT_ID_ENV),
            nonempty_var(INTENT_PATH_ENV),
        ) {
            let intent_path = PathBuf::from(intent_path);
            if intent_path.is_file() {
                return Ok(Self {
                    operation_id: operation_id.to_string_lossy().into_owned(),
                    attempt_id: attempt_id.to_string_lossy().into_owned(),
                    observed_at: observed_at_from_uuid_v7(&attempt_id.to_string_lossy()),
                    project_root: project_root.to_path_buf(),
                    intent_path,
                });
            }
        }

        let operation_id = uuid_v7()?;
        let attempt_id = uuid_v7()?;
        let project = project_ref(project_root, None);
        let accepted_at = observed_at();
        let envelope = IntentEnvelope {
            format: "ldgr-execution-intent",
            schema_version: 1,
            project: &project,
            producer: "ldgr-loop-launcher",
            operation_id: &operation_id,
            attempt_id: &attempt_id,
            boundary: "loop-launch",
            subject: "autonomous-loop",
            environment: allowlisted_environment(),
            accepted_at: accepted_at.clone(),
            process_id: std::process::id(),
        };
        let filename = format!("{attempt_id}.intent.json");
        let bytes = serde_json::to_vec_pretty(&envelope)?;
        let intent_path = write_to_first_sink(project_root, &filename, &bytes)
            .map_err(|error| anyhow!("{NO_SINK_DIAGNOSTIC}: {error:#}"))?;
        Ok(Self {
            operation_id,
            attempt_id,
            observed_at: accepted_at,
            project_root: project_root.to_path_buf(),
            intent_path,
        })
    }

    pub fn configure_child(&self, command: &mut std::process::Command) {
        command
            .env(OPERATION_ID_ENV, &self.operation_id)
            .env(ATTEMPT_ID_ENV, &self.attempt_id)
            .env(INTENT_PATH_ENV, &self.intent_path);
    }

    pub fn record_durable(
        &self,
        connection: Option<&Connection>,
        failure: FailureKind,
        run_id: Option<i64>,
    ) -> Result<()> {
        if !self.intent_path.is_file() {
            return Ok(());
        }
        let classification = classification(failure);
        let inputs = FingerprintInputs {
            class: classification.class,
            domain: classification.domain,
            code: classification.code,
            boundary: classification.boundary,
            component: "ldgr-core",
            subject: "autonomous-loop",
        };
        let inputs_value = serde_json::to_value(&inputs)?;
        let canonical = serde_json::to_vec(&inputs_value)?;
        let fingerprint = format!("sha256:{:x}", Sha256::digest(canonical));
        let fingerprint_provenance = FingerprintProvenance {
            kind: FingerprintProvenanceKind::Computed,
            rationale: None,
            base_version: None,
        };
        let occurrence_id = deterministic_occurrence_id(&self.attempt_id, classification.code);
        let idempotency_key = format!("{}:{}", self.attempt_id, classification.code);
        let details = match failure {
            FailureKind::ExitCode(exit_code) => {
                json!({ "exit_code": exit_code, "run_id": run_id })
            }
            FailureKind::Signal(signal) => json!({ "signal": signal, "run_id": run_id }),
            _ => json!({ "run_id": run_id }),
        };
        let environment = allowlisted_environment();
        let observed_at = self.observed_at.clone();

        if let Some(connection) = connection {
            let result = record_error(
                connection,
                &RecordErrorInput {
                    occurrence_id: &occurrence_id,
                    producer: "ldgr-loop-launcher",
                    idempotency_key: &idempotency_key,
                    operation_id: &self.operation_id,
                    attempt_id: &self.attempt_id,
                    fingerprint_version: "structured-v1",
                    fingerprint: &fingerprint,
                    fingerprint_inputs: Some(&inputs_value),
                    fingerprint_provenance: Some(&fingerprint_provenance),
                    class: parse_class(classification.class),
                    domain: classification.domain,
                    code: classification.code,
                    severity: ErrorSeverity::Error,
                    retryability: parse_retryability(classification.retryability),
                    source: classification.source,
                    summary: classification.summary,
                    details: &details,
                    environment: &environment,
                    observed_at: &observed_at,
                    recovery_origin: RecoveryOrigin::Database,
                },
            );
            if result.is_ok() {
                self.complete();
                return Ok(());
            }
        }

        let project = project_ref(&self.project_root, connection);
        let envelope = RecoveryEnvelope {
            format: "ldgr-error-recovery",
            schema_version: 1,
            project: &project,
            producer: "ldgr-loop-launcher",
            idempotency_key,
            operation_id: &self.operation_id,
            attempt_id: &self.attempt_id,
            occurrence_id,
            fingerprint: RecoveryFingerprint {
                version: "structured-v1",
                value: fingerprint,
                inputs,
            },
            error: RecoveryError {
                class: classification.class,
                domain: classification.domain,
                code: classification.code,
                severity: "error",
                retryability: classification.retryability,
                source: classification.source,
                summary: classification.summary,
                details,
                environment,
            },
            observed_at,
        };
        let filename = format!("{}-{}.json", self.attempt_id, classification.code);
        let bytes = serde_json::to_vec_pretty(&envelope)?;
        write_to_first_sink(&self.project_root, &filename, &bytes).map_err(|error| {
            anyhow!(
                "FATAL: accepted loop failure could not be written to any durable LDGR sink: {error:#}"
            )
        })?;
        self.complete();
        Ok(())
    }

    pub fn complete(&self) {
        let _ = fs::remove_file(&self.intent_path);
    }
}

struct Classification {
    class: &'static str,
    domain: &'static str,
    code: &'static str,
    boundary: &'static str,
    retryability: &'static str,
    source: &'static str,
    summary: &'static str,
}

fn classification(failure: FailureKind) -> Classification {
    match failure {
        FailureKind::CoreUnavailable => Classification {
            class: "infrastructure-error",
            domain: "ldgr.loop.bootstrap",
            code: "core-unavailable",
            boundary: "core-open",
            retryability: "after-change",
            source: "ldgr-loop-launcher:core-open",
            summary: "LDGR Core storage was unavailable before loop worker startup.",
        },
        FailureKind::Initialization => Classification {
            class: "infrastructure-error",
            domain: "ldgr.loop.bootstrap",
            code: "initialization-failed",
            boundary: "loop-initialization",
            retryability: "after-change",
            source: "ldgr-loop-launcher:initialization",
            summary: "Loop initialization failed before trustworthy worker completion.",
        },
        FailureKind::Spawn => Classification {
            class: "infrastructure-error",
            domain: "ldgr.loop.spawn",
            code: "worker-spawn-failed",
            boundary: "worker-spawn",
            retryability: "after-change",
            source: "ldgr-loop-launcher:worker-spawn",
            summary: "The loop launcher could not start its configured worker.",
        },
        FailureKind::ExitCode(_) => Classification {
            class: "task-failure",
            domain: "ldgr.loop.worker",
            code: "nonzero-exit",
            boundary: "worker-exit",
            retryability: "unknown",
            source: "ldgr-loop-launcher:worker-exit",
            summary: "The loop worker returned a nonzero exit code.",
        },
        FailureKind::Signal(_) => Classification {
            class: "interruption",
            domain: "ldgr.loop.worker",
            code: "signal",
            boundary: "worker-exit",
            retryability: "unknown",
            source: "ldgr-loop-launcher:worker-exit",
            summary: "The loop worker was terminated by a signal.",
        },
        FailureKind::UnexpectedDisappearance => Classification {
            class: "interruption",
            domain: "ldgr.loop.supervisor",
            code: "unexpected-disappearance",
            boundary: "supervisor-reconciliation",
            retryability: "after-change",
            source: "ldgr-loop-launcher:reconciliation",
            summary: "The loop worker disappeared without a terminal process status.",
        },
    }
}

fn parse_class(value: &str) -> ErrorClass {
    match value {
        "task-failure" => ErrorClass::TaskFailure,
        "interruption" => ErrorClass::Interruption,
        _ => ErrorClass::InfrastructureError,
    }
}

fn parse_retryability(value: &str) -> ErrorRetryability {
    match value {
        "after-change" => ErrorRetryability::AfterChange,
        "transient" => ErrorRetryability::Transient,
        "never" => ErrorRetryability::Never,
        _ => ErrorRetryability::Unknown,
    }
}

pub fn reconcile_startup(
    connection: &Connection,
    project_root: &Path,
) -> Result<StartupRecoveryReport> {
    let project_id = connection
        .query_row(
            "SELECT project_id FROM project_identity WHERE id=1",
            [],
            |row| row.get::<_, String>(0),
        )
        .context("reading project identity for startup reconciliation")?;
    let locator = absolute_locator(project_root);
    let mut report = StartupRecoveryReport::default();
    let project_inbox = project_root.join(".ldgr/recovery/inbox");
    reconcile_directory(
        connection,
        &project_inbox,
        RecoveryOrigin::ProjectInbox,
        &project_id,
        &locator,
        &mut report,
    )?;
    if let Some(user_inbox) =
        user_recovery_directory().filter(|candidate| candidate != &project_inbox)
    {
        reconcile_directory(
            connection,
            &user_inbox,
            RecoveryOrigin::UserSpool,
            &project_id,
            &locator,
            &mut report,
        )?;
    }
    reconcile_stale_runs(connection, &mut report)?;
    report.blocking_error_ids.sort_unstable();
    report.blocking_error_ids.dedup();
    Ok(report)
}

pub fn print_startup_recovery_report(report: &StartupRecoveryReport) {
    if !report.changed() {
        return;
    }
    eprintln!(
        "startup recovery: imported={} replayed={} archived={} quarantined={} interrupted={} restored_runs={}",
        report.imported,
        report.idempotent_replays,
        report.archived,
        report.quarantined,
        report.interrupted_attempts,
        report.restored_runs,
    );
    for diagnostic in &report.diagnostics {
        eprintln!("startup recovery: {diagnostic}");
    }
    if report.requires_disposition() {
        eprintln!(
            "startup recovery: blocking errors require a recorded disposition before retry: {}",
            report
                .blocking_error_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}

fn reconcile_directory(
    connection: &Connection,
    inbox: &Path,
    origin: RecoveryOrigin,
    project_id: &str,
    locator: &str,
    report: &mut StartupRecoveryReport,
) -> Result<()> {
    macro_rules! claim_or_continue {
        ($path:expr) => {
            match claim_recovery_file($path)? {
                Some(claimed) => claimed,
                None => continue,
            }
        };
    }
    if !inbox.is_dir() {
        return Ok(());
    }
    let mut entries = fs::read_dir(inbox)
        .with_context(|| format!("reading recovery inbox {}", inbox.display()))?
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.path().is_file())
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    entries.sort();

    for path in entries {
        let physical_file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("recovery-record")
            .to_owned();
        if let Some(owner_pid) = reconciliation_claim_owner(&physical_file_name) {
            if owner_pid == std::process::id() || process_is_live(owner_pid)? {
                continue;
            }
        }
        let file_name = recovery_record_name(&physical_file_name).to_owned();
        if file_name.ends_with(".tmp") || file_name.contains(".tmp.") {
            let claimed = claim_or_continue!(&path);
            quarantine_file(
                &claimed,
                inbox,
                &file_name,
                "partial temporary recovery record",
                report,
            )?;
            continue;
        }
        if !file_name.ends_with(".json") {
            continue;
        }
        let bytes = match fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                report
                    .diagnostics
                    .push(format!("could not read {}: {}", file_name, error.kind()));
                continue;
            }
        };
        let format = serde_json::from_slice::<Value>(&bytes)
            .ok()
            .and_then(|value| {
                value
                    .get("format")
                    .and_then(Value::as_str)
                    .map(str::to_owned)
            });
        match format.as_deref() {
            Some("ldgr-error-recovery") => {
                let envelope = match serde_json::from_slice::<RecoveryEnvelopeOwned>(&bytes) {
                    Ok(envelope) => envelope,
                    Err(_) => {
                        let claimed = claim_or_continue!(&path);
                        quarantine_file(
                            &claimed,
                            inbox,
                            &file_name,
                            "invalid or unsupported error recovery envelope",
                            report,
                        )?;
                        if origin == RecoveryOrigin::ProjectInbox {
                            record_quarantine_error(
                                connection, &bytes, &file_name, origin, report,
                            )?;
                        }
                        continue;
                    }
                };
                match project_match(
                    &envelope.project,
                    origin,
                    project_id,
                    locator,
                    envelope.schema_version,
                    &envelope.format,
                ) {
                    ProjectMatch::Current => {}
                    ProjectMatch::Other => {
                        report.skipped_other_projects += 1;
                        continue;
                    }
                    ProjectMatch::Invalid(reason) => {
                        let claimed = claim_or_continue!(&path);
                        quarantine_file(&claimed, inbox, &file_name, reason, report)?;
                        if origin == RecoveryOrigin::ProjectInbox {
                            record_quarantine_error(
                                connection, &bytes, &file_name, origin, report,
                            )?;
                        }
                        continue;
                    }
                }
                let claimed = claim_or_continue!(&path);
                crate::fault_injection::crash_if("recovery-spool-import");
                match import_error_envelope(connection, &envelope, origin, report) {
                    Ok(()) => {
                        archive_file(&claimed, inbox, &file_name, report)?;
                    }
                    Err(error) => {
                        quarantine_file(
                            &claimed,
                            inbox,
                            &file_name,
                            "error envelope failed validation or transactional import",
                            report,
                        )?;
                        record_quarantine_error(connection, &bytes, &file_name, origin, report)?;
                        report.diagnostics.push(format!(
                            "{} was quarantined after import rejection: {}",
                            file_name,
                            concise_error(&error)
                        ));
                    }
                }
            }
            Some("ldgr-execution-intent") => {
                let intent = match serde_json::from_slice::<IntentEnvelopeOwned>(&bytes) {
                    Ok(intent) => intent,
                    Err(_) => {
                        let claimed = claim_or_continue!(&path);
                        quarantine_file(
                            &claimed,
                            inbox,
                            &file_name,
                            "invalid or unsupported execution intent",
                            report,
                        )?;
                        if origin == RecoveryOrigin::ProjectInbox {
                            record_quarantine_error(
                                connection, &bytes, &file_name, origin, report,
                            )?;
                        }
                        continue;
                    }
                };
                match project_match(
                    &intent.project,
                    origin,
                    project_id,
                    locator,
                    intent.schema_version,
                    &intent.format,
                ) {
                    ProjectMatch::Current => {}
                    ProjectMatch::Other => {
                        report.skipped_other_projects += 1;
                        continue;
                    }
                    ProjectMatch::Invalid(reason) => {
                        let claimed = claim_or_continue!(&path);
                        quarantine_file(&claimed, inbox, &file_name, reason, report)?;
                        continue;
                    }
                }
                if intent_process_is_live(&intent)? {
                    report.live_attempts += 1;
                    continue;
                }
                if intent.process_id.is_none()
                    && !legacy_intent_is_stale(&path, LEGACY_INTENT_STALE_SECONDS)?
                {
                    report.live_attempts += 1;
                    continue;
                }
                let claimed = claim_or_continue!(&path);
                crate::fault_injection::crash_if("recovery-spool-import");
                match import_interrupted_intent(connection, &intent, origin, report) {
                    Ok(()) => archive_file(&claimed, inbox, &file_name, report)?,
                    Err(error) => {
                        quarantine_file(
                            &claimed,
                            inbox,
                            &file_name,
                            "execution intent failed transactional reconciliation",
                            report,
                        )?;
                        report.diagnostics.push(format!(
                            "{} was quarantined after reconciliation rejection: {}",
                            file_name,
                            concise_error(&error)
                        ));
                    }
                }
            }
            _ => {
                let claimed = claim_or_continue!(&path);
                quarantine_file(
                    &claimed,
                    inbox,
                    &file_name,
                    "unknown recovery record format",
                    report,
                )?;
                if origin == RecoveryOrigin::ProjectInbox {
                    record_quarantine_error(connection, &bytes, &file_name, origin, report)?;
                }
            }
        }
    }
    Ok(())
}

enum ProjectMatch<'a> {
    Current,
    Other,
    Invalid(&'a str),
}

fn project_match<'a>(
    project: &RecoveryProject,
    origin: RecoveryOrigin,
    project_id: &str,
    locator: &str,
    schema_version: u32,
    format: &str,
) -> ProjectMatch<'a> {
    if schema_version != 1 {
        return ProjectMatch::Invalid("unsupported recovery schema version");
    }
    if !matches!(format, "ldgr-error-recovery" | "ldgr-execution-intent") {
        return ProjectMatch::Invalid("unsupported recovery format");
    }
    if project.project_id.as_deref() == Some(project_id) {
        return ProjectMatch::Current;
    }
    if project.project_id.is_some() {
        return ProjectMatch::Other;
    }
    if origin == RecoveryOrigin::ProjectInbox
        && normalize_locator(&project.locator) == normalize_locator(locator)
    {
        return ProjectMatch::Current;
    }
    ProjectMatch::Invalid("recovery record cannot be bound to this project")
}

fn normalize_locator(locator: &str) -> String {
    let mut normalized = locator.replace('\\', "/");
    if let Some(unc) = normalized.strip_prefix("//?/UNC/") {
        normalized = format!("//{unc}");
    } else if let Some(verbatim) = normalized.strip_prefix("//?/") {
        normalized = verbatim.to_owned();
    }
    #[cfg(windows)]
    {
        normalized.to_ascii_lowercase()
    }
    #[cfg(not(windows))]
    {
        normalized
    }
}

fn import_error_envelope(
    connection: &Connection,
    envelope: &RecoveryEnvelopeOwned,
    origin: RecoveryOrigin,
    report: &mut StartupRecoveryReport,
) -> Result<()> {
    validate_recovery_fingerprint(envelope)?;
    let class = ErrorClass::from_str(&envelope.error.class)?;
    let severity = ErrorSeverity::from_str(&envelope.error.severity)?;
    let retryability = ErrorRetryability::from_str(&envelope.error.retryability)?;
    let provenance = FingerprintProvenance {
        kind: FingerprintProvenanceKind::Computed,
        rationale: None,
        base_version: None,
    };
    let mut related_run = None;
    let result = in_write_transaction(connection, |connection| {
        let recorded = record_error(
            connection,
            &RecordErrorInput {
                occurrence_id: &envelope.occurrence_id,
                producer: &envelope.producer,
                idempotency_key: &envelope.idempotency_key,
                operation_id: &envelope.operation_id,
                attempt_id: &envelope.attempt_id,
                fingerprint_version: &envelope.fingerprint.version,
                fingerprint: &envelope.fingerprint.value,
                fingerprint_inputs: Some(&envelope.fingerprint.inputs),
                fingerprint_provenance: Some(&provenance),
                class,
                domain: &envelope.error.domain,
                code: &envelope.error.code,
                severity,
                retryability,
                source: &envelope.error.source,
                summary: &envelope.error.summary,
                details: &envelope.error.details,
                environment: &envelope.error.environment,
                observed_at: &envelope.observed_at,
                recovery_origin: origin,
            },
        )?;
        if let Some(run_id) = related_running_run(connection, &envelope.error.details)? {
            link_error_if_missing(
                connection,
                recorded.error.id,
                Some(&recorded.occurrence.occurrence_id),
                "observed-during",
                "run",
                &run_id.to_string(),
                "startup-reconciliation",
            )?;
            related_run = Some(run_id);
        }
        Ok(recorded)
    })?;
    if result.idempotent_replay {
        report.idempotent_replays += 1;
    } else {
        report.imported += 1;
    }
    if related_run.is_some() && is_blocking(&result.error) {
        report.blocking_error_ids.push(result.error.id);
    }
    Ok(())
}

fn validate_recovery_fingerprint(envelope: &RecoveryEnvelopeOwned) -> Result<()> {
    anyhow::ensure!(
        envelope.fingerprint.version == "structured-v1",
        "unsupported fingerprint version"
    );
    let inputs = envelope
        .fingerprint
        .inputs
        .as_object()
        .context("fingerprint inputs must be an object")?;
    for (field, expected) in [
        ("class", envelope.error.class.as_str()),
        ("domain", envelope.error.domain.as_str()),
        ("code", envelope.error.code.as_str()),
    ] {
        anyhow::ensure!(
            inputs.get(field).and_then(Value::as_str) == Some(expected),
            "fingerprint input {field} does not match error classification"
        );
    }
    let canonical = serde_json::to_vec(&envelope.fingerprint.inputs)?;
    let expected = format!("sha256:{:x}", Sha256::digest(canonical));
    let legacy_expected = legacy_struct_order_fingerprint(&envelope.fingerprint.inputs)?;
    anyhow::ensure!(
        expected == envelope.fingerprint.value
            || legacy_expected.as_deref() == Some(envelope.fingerprint.value.as_str()),
        "fingerprint digest does not match structured inputs"
    );
    Ok(())
}

fn legacy_struct_order_fingerprint(inputs: &Value) -> Result<Option<String>> {
    let Some(inputs) = inputs.as_object() else {
        return Ok(None);
    };
    let fields = [
        "class",
        "domain",
        "code",
        "boundary",
        "component",
        "subject",
    ];
    let mut pairs = Vec::new();
    for field in fields {
        let Some(value) = inputs.get(field).and_then(Value::as_str) else {
            return Ok(None);
        };
        pairs.push(format!(
            "{}:{}",
            serde_json::to_string(field)?,
            serde_json::to_string(value)?
        ));
    }
    let legacy = format!("{{{}}}", pairs.join(","));
    Ok(Some(format!(
        "sha256:{:x}",
        Sha256::digest(legacy.as_bytes())
    )))
}

fn related_running_run(connection: &Connection, details: &Value) -> Result<Option<i64>> {
    let Some(run_id) = details.get("run_id").and_then(Value::as_i64) else {
        return Ok(None);
    };
    connection
        .query_row(
            "SELECT id FROM run WHERE id=?1 AND status='running'",
            params![run_id],
            |row| row.get(0),
        )
        .optional()
        .context("checking recovery record run relation")
}

fn import_interrupted_intent(
    connection: &Connection,
    intent: &IntentEnvelopeOwned,
    origin: RecoveryOrigin,
    report: &mut StartupRecoveryReport,
) -> Result<()> {
    anyhow::ensure!(
        intent.environment.is_object(),
        "intent environment must be an object"
    );
    let inputs = json!({
        "class": "interruption",
        "domain": "ldgr.startup.reconciliation",
        "code": "interrupted-attempt",
        "boundary": intent.boundary,
        "component": intent.producer,
        "subject": intent.subject,
    });
    let fingerprint = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&inputs)?));
    let occurrence_id = deterministic_occurrence_id_from_seed(
        &intent.attempt_id,
        "startup-interrupted-attempt",
        &intent.accepted_at,
    );
    let idempotency_key = format!("{}:startup-interrupted-attempt", intent.attempt_id);
    let provenance = FingerprintProvenance {
        kind: FingerprintProvenanceKind::Computed,
        rationale: None,
        base_version: None,
    };
    let run_id = matching_stale_run(connection, intent.process_id)?;
    let details = json!({
        "boundary": intent.boundary,
        "subject": intent.subject,
        "last_known_process_id": intent.process_id,
        "run_id": run_id,
    });
    let result = in_write_transaction(connection, |connection| {
        let recorded = record_error(
            connection,
            &RecordErrorInput {
                occurrence_id: &occurrence_id,
                producer: "ldgr-startup-reconciliation",
                idempotency_key: &idempotency_key,
                operation_id: &intent.operation_id,
                attempt_id: &intent.attempt_id,
                fingerprint_version: "structured-v1",
                fingerprint: &fingerprint,
                fingerprint_inputs: Some(&inputs),
                fingerprint_provenance: Some(&provenance),
                class: ErrorClass::Interruption,
                domain: "ldgr.startup.reconciliation",
                code: "interrupted-attempt",
                severity: ErrorSeverity::Error,
                retryability: ErrorRetryability::AfterChange,
                source: "ldgr-core:startup-reconciliation",
                summary: "An accepted execution attempt ended without a terminal record.",
                details: &details,
                environment: &intent.environment,
                observed_at: &intent.accepted_at,
                recovery_origin: origin,
            },
        )?;
        if let Some(run_id) = run_id {
            link_error_if_missing(
                connection,
                recorded.error.id,
                Some(&recorded.occurrence.occurrence_id),
                "interrupted",
                "run",
                &run_id.to_string(),
                "startup-reconciliation",
            )?;
            restore_run_pending(connection, run_id, recorded.error.id)?;
        }
        Ok(recorded)
    })?;
    if result.idempotent_replay {
        report.idempotent_replays += 1;
    } else {
        report.imported += 1;
    }
    report.interrupted_attempts += 1;
    if run_id.is_some() {
        report.restored_runs += 1;
        report.blocking_error_ids.push(result.error.id);
    }
    Ok(())
}

fn reconcile_stale_runs(connection: &Connection, report: &mut StartupRecoveryReport) -> Result<()> {
    // A live producer may still own the one active loop even if its original
    // supervisor PID disappeared. Preserve that run until all accepted
    // producer attempts are terminal or dead.
    if report.live_attempts > 0 {
        return Ok(());
    }
    let stale_runs = stale_running_runs(connection)?;
    for stale in stale_runs {
        if process_is_live(stale.process_id)? {
            continue;
        }
        let inputs = json!({
            "class": "interruption",
            "domain": "ldgr.loop.supervisor",
            "code": "dead-worker",
            "boundary": "startup-reconciliation",
            "component": "ldgr-core",
            "subject": "autonomous-loop",
        });
        let fingerprint = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&inputs)?));
        let attempt_id = format!("recovered-run-{}", stale.run_id);
        let occurrence_id =
            deterministic_occurrence_id_from_seed(&attempt_id, "dead-worker", &stale.started_at);
        let observed_at = sqlite_time_to_rfc3339(&stale.started_at);
        let details = json!({
            "run_id": stale.run_id,
            "last_known_process_id": stale.process_id,
            "last_phase": stale.phase,
        });
        let provenance = FingerprintProvenance {
            kind: FingerprintProvenanceKind::Computed,
            rationale: None,
            base_version: None,
        };
        let recorded = in_write_transaction(connection, |connection| {
            let active: i64 = connection.query_row(
                "SELECT EXISTS(SELECT 1 FROM run WHERE id=?1 AND status='running')",
                params![stale.run_id],
                |row| row.get(0),
            )?;
            if active == 0 {
                return Ok(None);
            }
            let recorded = record_error(
                connection,
                &RecordErrorInput {
                    occurrence_id: &occurrence_id,
                    producer: "ldgr-startup-reconciliation",
                    idempotency_key: &format!("run:{}:dead-worker", stale.run_id),
                    operation_id: &format!("recovered-run-{}", stale.run_id),
                    attempt_id: &attempt_id,
                    fingerprint_version: "structured-v1",
                    fingerprint: &fingerprint,
                    fingerprint_inputs: Some(&inputs),
                    fingerprint_provenance: Some(&provenance),
                    class: ErrorClass::Interruption,
                    domain: "ldgr.loop.supervisor",
                    code: "dead-worker",
                    severity: ErrorSeverity::Error,
                    retryability: ErrorRetryability::AfterChange,
                    source: "ldgr-core:startup-reconciliation",
                    summary: "A loop worker process disappeared while its run remained active.",
                    details: &details,
                    environment: &allowlisted_environment(),
                    observed_at: &observed_at,
                    recovery_origin: RecoveryOrigin::Database,
                },
            )?;
            link_error_if_missing(
                connection,
                recorded.error.id,
                Some(&recorded.occurrence.occurrence_id),
                "interrupted",
                "run",
                &stale.run_id.to_string(),
                "startup-reconciliation",
            )?;
            restore_run_pending(connection, stale.run_id, recorded.error.id)?;
            Ok(Some(recorded))
        })?;
        let Some(recorded) = recorded else {
            continue;
        };
        if recorded.idempotent_replay {
            report.idempotent_replays += 1;
        } else {
            report.imported += 1;
        }
        report.interrupted_attempts += 1;
        report.restored_runs += 1;
        report.blocking_error_ids.push(recorded.error.id);
    }
    Ok(())
}

struct StaleRun {
    run_id: i64,
    process_id: u32,
    phase: String,
    started_at: String,
}

fn stale_running_runs(connection: &Connection) -> Result<Vec<StaleRun>> {
    let mut statement = connection.prepare(
        "SELECT run.id,
                CAST(json_extract(phase.payload_json, '$.process_id') AS INTEGER),
                json_extract(phase.payload_json, '$.phase'),
                run.started_at
         FROM run
         JOIN event_log AS phase ON phase.id = (
             SELECT latest.id
             FROM event_log AS latest
             WHERE latest.entity_type='run'
               AND latest.entity_id=run.id
               AND latest.event_type='phase'
             ORDER BY latest.id DESC
             LIMIT 1
         )
         WHERE run.status='running'
           AND json_type(phase.payload_json, '$.process_id')='integer'
         ORDER BY run.started_at, run.id",
    )?;
    let runs = statement
        .query_map([], |row| {
            let process_id = row.get::<_, i64>(1)?;
            let process_id = u32::try_from(process_id).map_err(|error| {
                rusqlite::Error::FromSqlConversionFailure(
                    1,
                    rusqlite::types::Type::Integer,
                    Box::new(error),
                )
            })?;
            Ok(StaleRun {
                run_id: row.get(0)?,
                process_id,
                phase: row.get(2)?,
                started_at: row.get(3)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("reading active runs with supervisor process identities")?;
    Ok(runs)
}

fn matching_stale_run(connection: &Connection, process_id: Option<u32>) -> Result<Option<i64>> {
    let stale = stale_running_runs(connection)?;
    if let Some(process_id) = process_id {
        if let Some(run) = stale.iter().find(|run| run.process_id == process_id) {
            return Ok(Some(run.run_id));
        }
    }
    let dead = stale
        .into_iter()
        .filter_map(|run| match process_is_live(run.process_id) {
            Ok(false) => Some(Ok(run.run_id)),
            Ok(true) => None,
            Err(error) => Some(Err(error)),
        })
        .collect::<Result<Vec<_>>>()?;
    Ok(match dead.as_slice() {
        [run_id] => Some(*run_id),
        _ => None,
    })
}

fn restore_run_pending(connection: &Connection, run_id: i64, error_id: i64) -> Result<()> {
    let work_id = connection
        .query_row(
            "SELECT work_item_id FROM run WHERE id=?1 AND status='running'",
            params![run_id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?
        .with_context(|| format!("run {run_id} is no longer active"))?;
    connection.execute(
        "UPDATE run
         SET status='partial', finished_at=datetime('now'),
             notes='Startup reconciliation detected an interrupted worker; disposition required before retry.'
         WHERE id=?1 AND status='running'",
        params![run_id],
    )?;
    connection.execute(
        "UPDATE work_item
         SET status='pending', updated_at=datetime('now')
         WHERE id=?1 AND status='running'",
        params![work_id],
    )?;
    let run_payload = json!({
        "status": "partial",
        "notes": "startup reconciliation restored interrupted work",
        "error_id": error_id,
    });
    connection.execute(
        "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
         VALUES ('run', ?1, 'startup_interrupted', ?2)",
        params![run_id, serde_json::to_string(&run_payload)?],
    )?;
    let work_payload = json!({
        "run_id": run_id,
        "error_id": error_id,
        "reason": "interrupted run restored for explicit disposition and deterministic retry",
    });
    connection.execute(
        "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
         VALUES ('work_item', ?1, 'startup_restore_pending', ?2)",
        params![work_id, serde_json::to_string(&work_payload)?],
    )?;
    Ok(())
}

fn link_error_if_missing(
    connection: &Connection,
    error_id: i64,
    occurrence_id: Option<&str>,
    relation_kind: &str,
    entity_type: &str,
    entity_id: &str,
    source: &str,
) -> Result<()> {
    let exists: i64 = connection.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM error_relation
             WHERE error_id=?1
               AND occurrence_id IS ?2
               AND relation_kind=?3
               AND entity_type=?4
               AND entity_id=?5
         )",
        params![
            error_id,
            occurrence_id,
            relation_kind,
            entity_type,
            entity_id
        ],
        |row| row.get(0),
    )?;
    if exists == 0 {
        link_error(
            connection,
            error_id,
            occurrence_id,
            relation_kind,
            entity_type,
            entity_id,
            source,
        )?;
    }
    Ok(())
}

fn record_quarantine_error(
    connection: &Connection,
    bytes: &[u8],
    file_name: &str,
    origin: RecoveryOrigin,
    report: &mut StartupRecoveryReport,
) -> Result<()> {
    let digest = format!("sha256:{:x}", Sha256::digest(bytes));
    let seed = digest.trim_start_matches("sha256:");
    let occurrence_id =
        deterministic_occurrence_id_from_seed(seed, "quarantined-record", "2000-01-01T00:00:00Z");
    let inputs = json!({
        "class": "infrastructure-error",
        "domain": "ldgr.startup.reconciliation",
        "code": "recovery-record-quarantined",
        "boundary": "recovery-import",
        "component": "ldgr-core",
        "subject": "recovery-envelope",
    });
    let fingerprint = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&inputs)?));
    let details = json!({
        "record_name": bounded_file_name(file_name),
        "payload_digest": digest,
    });
    let provenance = FingerprintProvenance {
        kind: FingerprintProvenanceKind::Computed,
        rationale: None,
        base_version: None,
    };
    let recorded = record_error(
        connection,
        &RecordErrorInput {
            occurrence_id: &occurrence_id,
            producer: "ldgr-startup-reconciliation",
            idempotency_key: &format!("quarantine:{digest}"),
            operation_id: &occurrence_id,
            attempt_id: &occurrence_id,
            fingerprint_version: "structured-v1",
            fingerprint: &fingerprint,
            fingerprint_inputs: Some(&inputs),
            fingerprint_provenance: Some(&provenance),
            class: ErrorClass::InfrastructureError,
            domain: "ldgr.startup.reconciliation",
            code: "recovery-record-quarantined",
            severity: ErrorSeverity::Error,
            retryability: ErrorRetryability::AfterChange,
            source: "ldgr-core:startup-reconciliation",
            summary: "A recovery record was quarantined because it could not be imported safely.",
            details: &details,
            environment: &allowlisted_environment(),
            observed_at: "2000-01-01T00:00:00Z",
            recovery_origin: origin,
        },
    )?;
    if recorded.idempotent_replay {
        report.idempotent_replays += 1;
    } else {
        report.imported += 1;
    }
    Ok(())
}

fn is_blocking(error: &crate::store::ErrorRecord) -> bool {
    error.disposition_pending
        || (matches!(
            error.state,
            crate::store::ErrorState::Open | crate::store::ErrorState::Acknowledged
        ) && matches!(
            error.severity,
            ErrorSeverity::Error | ErrorSeverity::Critical
        ))
}

fn intent_process_is_live(intent: &IntentEnvelopeOwned) -> Result<bool> {
    intent
        .process_id
        .map(process_is_live)
        .transpose()
        .map(|value| value.unwrap_or(false))
}

fn legacy_intent_is_stale(path: &Path, threshold_seconds: u64) -> Result<bool> {
    let modified = fs::metadata(path)
        .with_context(|| format!("reading recovery metadata {}", path.display()))?
        .modified()
        .context("recovery record has no modification time")?;
    Ok(SystemTime::now()
        .duration_since(modified)
        .unwrap_or_default()
        .as_secs()
        >= threshold_seconds)
}

#[cfg(unix)]
fn process_is_live(process_id: u32) -> Result<bool> {
    let Ok(process_id) = libc::pid_t::try_from(process_id) else {
        return Ok(false);
    };
    if process_id <= 0 {
        return Ok(false);
    }
    let result = unsafe { libc::kill(process_id, 0) };
    if result == 0 {
        return Ok(true);
    }
    let error = std::io::Error::last_os_error();
    if error.raw_os_error() == Some(libc::ESRCH) {
        return Ok(false);
    }
    if error.raw_os_error() == Some(libc::EPERM) {
        return Ok(true);
    }
    Err(error).context("checking Unix process liveness")
}

#[cfg(windows)]
fn process_is_live(process_id: u32) -> Result<bool> {
    use windows_sys::Win32::Foundation::{CloseHandle, STILL_ACTIVE};
    use windows_sys::Win32::System::Threading::{
        GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    let handle = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, process_id) };
    if handle.is_null() {
        let error = std::io::Error::last_os_error();
        return match error.raw_os_error() {
            Some(5) => Ok(true),
            Some(87) | Some(1168) => Ok(false),
            _ => Ok(false),
        };
    }
    let mut exit_code = 0_u32;
    let ok = unsafe { GetExitCodeProcess(handle, &mut exit_code) };
    unsafe {
        CloseHandle(handle);
    }
    if ok == 0 {
        return Err(std::io::Error::last_os_error()).context("checking Windows process liveness");
    }
    Ok(exit_code == STILL_ACTIVE as u32)
}

#[cfg(all(not(unix), not(windows)))]
fn process_is_live(_process_id: u32) -> Result<bool> {
    Ok(true)
}

fn claim_recovery_file(path: &Path) -> Result<Option<PathBuf>> {
    let parent = path.parent().context("recovery record has no parent")?;
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("record.json");
    let claimed = parent.join(format!(".reconciling-{}-{}", std::process::id(), name));
    match fs::rename(path, &claimed) {
        Ok(()) => Ok(Some(claimed)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => {
            Err(error).with_context(|| format!("claiming recovery record {}", path.display()))
        }
    }
}

fn archive_file(
    claimed: &Path,
    inbox: &Path,
    original_name: &str,
    report: &mut StartupRecoveryReport,
) -> Result<()> {
    move_preserving(claimed, &inbox.join("../archive"), original_name)?;
    report.archived += 1;
    Ok(())
}

fn quarantine_file(
    claimed: &Path,
    inbox: &Path,
    original_name: &str,
    reason: &str,
    report: &mut StartupRecoveryReport,
) -> Result<()> {
    let destination = move_preserving(claimed, &inbox.join("../quarantine"), original_name)?;
    report.quarantined += 1;
    report.diagnostics.push(format!(
        "quarantined {} as {} ({reason})",
        bounded_file_name(original_name),
        destination
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("quarantined-record")
    ));
    Ok(())
}

fn move_preserving(source: &Path, directory: &Path, original_name: &str) -> Result<PathBuf> {
    fs::create_dir_all(directory)
        .with_context(|| format!("creating recovery directory {}", directory.display()))?;
    let safe_name = bounded_file_name(original_name);
    for suffix in 0_u64.. {
        let destination = if suffix == 0 {
            directory.join(&safe_name)
        } else {
            directory.join(format!("{safe_name}.{suffix}"))
        };
        if destination.exists() {
            continue;
        }
        match fs::rename(source, &destination) {
            Ok(()) => return Ok(destination),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound && !source.exists() => {
                if let Some(existing) = find_preserved_record(directory, &safe_name)? {
                    return Ok(existing);
                }
                return Err(error).with_context(|| {
                    format!(
                        "recovery claim {} disappeared without a preserved destination",
                        source.display()
                    )
                });
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(error).with_context(|| {
                    format!(
                        "moving recovery record {} to {}",
                        source.display(),
                        destination.display()
                    )
                });
            }
        }
    }
    unreachable!("unbounded recovery archive suffix space")
}

fn find_preserved_record(directory: &Path, safe_name: &str) -> Result<Option<PathBuf>> {
    if !directory.is_dir() {
        return Ok(None);
    }
    let mut matches = fs::read_dir(directory)?
        .filter_map(|entry| entry.ok())
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name == safe_name || name.starts_with(&format!("{safe_name}.")))
        })
        .collect::<Vec<_>>();
    matches.sort();
    Ok(matches.into_iter().next())
}

fn bounded_file_name(name: &str) -> String {
    let sanitized = name
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(160)
        .collect::<String>();
    if sanitized.is_empty() {
        "recovery-record".to_owned()
    } else {
        sanitized
    }
}

fn reconciliation_claim_owner(file_name: &str) -> Option<u32> {
    file_name
        .strip_prefix(".reconciling-")?
        .split('-')
        .next()?
        .parse()
        .ok()
}

fn recovery_record_name(file_name: &str) -> &str {
    let mut current = file_name;
    while let Some(rest) = current.strip_prefix(".reconciling-") {
        let Some((owner, original)) = rest.split_once('-') else {
            break;
        };
        if owner.parse::<u32>().is_err() {
            break;
        }
        current = original;
    }
    current
}

fn concise_error(error: &anyhow::Error) -> String {
    error
        .chain()
        .next()
        .map(ToString::to_string)
        .unwrap_or_else(|| "unknown error".to_owned())
}

fn sqlite_time_to_rfc3339(value: &str) -> String {
    if value.ends_with('Z') {
        value.to_owned()
    } else {
        format!("{}Z", value.replace(' ', "T"))
    }
}

fn deterministic_occurrence_id_from_seed(seed: &str, code: &str, time_seed: &str) -> String {
    let digest = Sha256::digest(format!("ldgr-startup:{seed}:{code}:{time_seed}"));
    let time_digest = Sha256::digest(time_seed);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[..6].copy_from_slice(&time_digest[..6]);
    bytes[6] = (bytes[6] & 0x0f) | 0x70;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
        u32::from_be_bytes(bytes[0..4].try_into().expect("four bytes")),
        u16::from_be_bytes(bytes[4..6].try_into().expect("two bytes")),
        u16::from_be_bytes(bytes[6..8].try_into().expect("two bytes")),
        u16::from_be_bytes(bytes[8..10].try_into().expect("two bytes")),
        u64::from_be_bytes([
            0, 0, bytes[10], bytes[11], bytes[12], bytes[13], bytes[14], bytes[15],
        ])
    )
}

pub fn repair_process_home() -> Result<()> {
    #[cfg(windows)]
    if let Some(profile) = nonempty_var("USERPROFILE").filter(|_| nonempty_var("HOME").is_none()) {
        // SAFETY: called before any loop process, reader, or watcher threads.
        unsafe {
            env::set_var("HOME", profile);
        }
    }
    Ok(())
}

pub fn project_root_for_db(db: &Path) -> PathBuf {
    db.parent()
        .and_then(Path::parent)
        .filter(|path| !path.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."))
        .to_path_buf()
}

fn project_ref(project_root: &Path, connection: Option<&Connection>) -> ProjectRef {
    let project_id = connection.and_then(|connection| {
        connection
            .query_row(
                "SELECT project_id FROM project_identity WHERE id=1",
                [],
                |row| row.get::<_, String>(0),
            )
            .ok()
    });
    ProjectRef {
        project_id,
        locator: absolute_locator(project_root),
        database_identity: None,
    }
}

fn absolute_locator(project_root: &Path) -> String {
    let unresolved = if project_root.is_absolute() {
        project_root.to_path_buf()
    } else {
        env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(project_root)
    };
    let absolute = fs::canonicalize(&unresolved).unwrap_or(unresolved);
    for name in ["HOME", "USERPROFILE"] {
        let Some(home) = nonempty_var(name).map(PathBuf::from) else {
            continue;
        };
        let Ok(relative) = absolute.strip_prefix(home) else {
            continue;
        };
        if relative.as_os_str().is_empty() {
            return "$HOME".to_owned();
        }
        return format!("$HOME/{}", relative.to_string_lossy().replace('\\', "/"));
    }
    normalize_locator(&absolute.to_string_lossy())
}

fn write_to_first_sink(project_root: &Path, filename: &str, bytes: &[u8]) -> Result<PathBuf> {
    let mut failures = Vec::new();
    for directory in recovery_directories(project_root) {
        match atomic_write(&directory, filename, bytes) {
            Ok(path) => return Ok(path),
            Err(error) => failures.push(format!("{}: {error:#}", directory.display())),
        }
    }
    bail!("{}", failures.join("; "))
}

fn recovery_directories(project_root: &Path) -> Vec<PathBuf> {
    let mut directories = vec![project_root.join(".ldgr/recovery/inbox")];
    if let Some(user) =
        user_recovery_directory().filter(|candidate| !directories.contains(candidate))
    {
        directories.push(user);
    }
    directories
}

fn user_recovery_directory() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        nonempty_var("LOCALAPPDATA").map(|root| PathBuf::from(root).join("ldgr/recovery/inbox"))
    }
    #[cfg(not(windows))]
    {
        nonempty_var("XDG_STATE_HOME")
            .map(PathBuf::from)
            .or_else(|| nonempty_var("HOME").map(|home| PathBuf::from(home).join(".local/state")))
            .map(|root| root.join("ldgr/recovery/inbox"))
    }
}

fn atomic_write(directory: &Path, filename: &str, bytes: &[u8]) -> Result<PathBuf> {
    fs::create_dir_all(directory)
        .with_context(|| format!("creating recovery directory {}", directory.display()))?;
    let destination = directory.join(filename);
    if fs::read(&destination).is_ok_and(|existing| existing == bytes) {
        return Ok(destination);
    }
    let temporary = directory.join(format!(
        ".{filename}.{}.{}.tmp",
        std::process::id(),
        UNIQUE_COUNTER.fetch_add(1, Ordering::Relaxed)
    ));
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options
        .open(&temporary)
        .with_context(|| format!("creating {}", temporary.display()))?;
    file.write_all(bytes)
        .with_context(|| format!("writing {}", temporary.display()))?;
    file.sync_all()
        .with_context(|| format!("syncing {}", temporary.display()))?;
    drop(file);
    if let Err(error) = fs::rename(&temporary, &destination) {
        if fs::read(&destination).is_ok_and(|existing| existing == bytes) {
            let _ = fs::remove_file(&temporary);
            return Ok(destination);
        }
        let _ = fs::remove_file(&temporary);
        return Err(error)
            .with_context(|| format!("publishing recovery record {}", destination.display()));
    }
    #[cfg(unix)]
    fs::File::open(directory)
        .and_then(|directory| directory.sync_all())
        .with_context(|| format!("syncing recovery directory {}", directory.display()))?;
    Ok(destination)
}

fn allowlisted_environment() -> Value {
    json!({
        "os": env::consts::OS,
        "arch": env::consts::ARCH,
        "family": env::consts::FAMILY,
    })
}

fn uuid_v7() -> Result<String> {
    let millis = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?
        .as_millis() as u64;
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow!("generating execution identity: {error}"))?;
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

fn deterministic_occurrence_id(attempt_id: &str, code: &str) -> String {
    let digest = Sha256::digest(format!("ldgr-loop-launcher:{attempt_id}:{code}"));
    let timestamp = attempt_id
        .chars()
        .filter(|character| *character != '-')
        .take(12)
        .collect::<String>();
    let random = format!("{:x}", digest);
    let variant = match &random[3..4] {
        "0" | "1" | "2" | "3" => "8",
        "4" | "5" | "6" | "7" => "9",
        "8" | "9" | "a" | "b" => "a",
        _ => "b",
    };
    format!(
        "{}-{}-7{}-{}{}-{}",
        &timestamp[0..8],
        &timestamp[8..12],
        &random[0..3],
        variant,
        &random[4..7],
        &random[7..19],
    )
}

fn observed_at_from_uuid_v7(id: &str) -> String {
    let timestamp = id
        .chars()
        .filter(|character| *character != '-')
        .take(12)
        .collect::<String>();
    let millis = u64::from_str_radix(&timestamp, 16).unwrap_or(0);
    observed_at_from_seconds((millis / 1_000) as i64)
}

fn observed_at() -> String {
    let seconds = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() as i64)
        .unwrap_or(0);
    observed_at_from_seconds(seconds)
}

fn observed_at_from_seconds(seconds: i64) -> String {
    let days = seconds.div_euclid(86_400);
    let second_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    format!(
        "{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z",
        second_of_day / 3_600,
        (second_of_day % 3_600) / 60,
        second_of_day % 60
    )
}

fn civil_from_days(days_since_epoch: i64) -> (i64, i64, i64) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = z - era * 146_097;
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let mut year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = day_of_year - (153 * month_prime + 2) / 5 + 1;
    let month = month_prime + if month_prime < 10 { 3 } else { -9 };
    year += i64::from(month <= 2);
    (year, month, day)
}

fn nonempty_var(name: &str) -> Option<std::ffi::OsString> {
    env::var_os(name).filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        create_work_item, get_run_by_id, get_work_item_by_slug, init_store, list_errors,
        record_run_phase, start_run, ErrorState, RunStatus, WorkItemStatus,
    };

    fn initialized_project() -> Result<(tempfile::TempDir, Connection)> {
        let project = tempfile::tempdir()?;
        let db = project.path().join(".ldgr/ldgr.db");
        init_store(&db, &project.path().join(".ldgr/artifacts"))?;
        let connection = crate::store::open_store(&db)?;
        Ok((project, connection))
    }

    fn project_id(connection: &Connection) -> Result<String> {
        connection
            .query_row(
                "SELECT project_id FROM project_identity WHERE id=1",
                [],
                |row| row.get(0),
            )
            .context("reading test project identity")
    }

    #[cfg(unix)]
    #[test]
    fn unix_process_liveness_rejects_non_positive_or_overflowing_pids() -> Result<()> {
        assert!(!process_is_live(0)?);
        assert!(!process_is_live(u32::MAX)?);
        assert!(process_is_live(std::process::id())?);
        Ok(())
    }

    fn valid_error_envelope(
        connection: &Connection,
        project_root: &Path,
        occurrence: &str,
    ) -> Result<Value> {
        let inputs = json!({
            "class": "infrastructure-error",
            "domain": "test.recovery",
            "code": "spooled",
            "boundary": "test",
            "component": "test",
            "subject": "startup",
        });
        let fingerprint = format!("sha256:{:x}", Sha256::digest(serde_json::to_vec(&inputs)?));
        Ok(json!({
            "format": "ldgr-error-recovery",
            "schema_version": 1,
            "project": {
                "project_id": project_id(connection)?,
                "locator": absolute_locator(project_root),
                "database_identity": null,
            },
            "producer": "test",
            "idempotency_key": format!("{occurrence}:spooled"),
            "operation_id": occurrence,
            "attempt_id": occurrence,
            "occurrence_id": occurrence,
            "fingerprint": {
                "version": "structured-v1",
                "value": fingerprint,
                "inputs": inputs,
            },
            "error": {
                "class": "infrastructure-error",
                "domain": "test.recovery",
                "code": "spooled",
                "severity": "error",
                "retryability": "after-change",
                "source": "test:recovery",
                "summary": "A test recovery record was spooled.",
                "details": {},
                "environment": {"os": env::consts::OS},
            },
            "observed_at": "2026-07-31T00:00:00Z",
        }))
    }

    fn reconcile_test_project(
        connection: &Connection,
        project_root: &Path,
    ) -> Result<StartupRecoveryReport> {
        let mut report = StartupRecoveryReport::default();
        reconcile_directory(
            connection,
            &project_root.join(".ldgr/recovery/inbox"),
            RecoveryOrigin::ProjectInbox,
            &project_id(connection)?,
            &absolute_locator(project_root),
            &mut report,
        )?;
        reconcile_stale_runs(connection, &mut report)?;
        report.blocking_error_ids.sort_unstable();
        report.blocking_error_ids.dedup();
        Ok(report)
    }

    #[test]
    fn intent_is_atomic_and_redacted() -> Result<()> {
        let project = tempfile::tempdir()?;
        let attempt = ExecutionAttempt::begin_or_adopt(project.path())?;
        let text = fs::read_to_string(&attempt.intent_path)?;
        assert!(text.contains("ldgr-execution-intent"));
        assert!(!text.contains("prompt"));
        assert!(fs::read_dir(project.path().join(".ldgr/recovery/inbox"))?
            .filter_map(|entry| entry.ok())
            .all(|entry| entry.path().extension().is_some_and(|ext| ext == "json")));
        attempt.complete();
        Ok(())
    }

    #[test]
    fn project_locator_canonicalizes_relative_parent_segments() -> Result<()> {
        let project = tempfile::tempdir()?;
        let child = project.path().join("child");
        fs::create_dir_all(&child)?;
        assert_eq!(
            absolute_locator(&child.join("..")),
            absolute_locator(project.path())
        );
        Ok(())
    }

    #[test]
    fn database_path_locator_matches_its_project_parent() {
        let db = Path::new("../.ldgr/ldgr.db");
        assert_eq!(
            absolute_locator(&project_root_for_db(db)),
            absolute_locator(Path::new(".."))
        );
    }

    #[test]
    fn locator_normalization_removes_windows_verbatim_prefix() {
        assert_eq!(
            normalize_locator(r"\\?\E:\apps\ldgr"),
            normalize_locator("E:/apps/ldgr")
        );
    }

    #[test]
    fn startup_imports_and_archives_valid_recovery_records() -> Result<()> {
        let (project, connection) = initialized_project()?;
        let inbox = project.path().join(".ldgr/recovery/inbox");
        fs::create_dir_all(&inbox)?;
        let occurrence = "0198f000-0000-7000-8000-000000000001";
        fs::write(
            inbox.join("spooled.json"),
            serde_json::to_vec_pretty(&valid_error_envelope(
                &connection,
                project.path(),
                occurrence,
            )?)?,
        )?;

        let report = reconcile_test_project(&connection, project.path())?;
        assert_eq!(report.imported, 1);
        assert_eq!(report.archived, 1);
        assert_eq!(report.quarantined, 0);
        assert!(!inbox.join("spooled.json").exists());
        assert!(project
            .path()
            .join(".ldgr/recovery/archive/spooled.json")
            .is_file());
        let errors = list_errors(&connection, None, 10)?;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].domain, "test.recovery");
        assert_eq!(errors[0].state, ErrorState::Open);
        Ok(())
    }

    #[test]
    fn startup_accepts_original_v1_struct_order_fingerprint() -> Result<()> {
        let (project, connection) = initialized_project()?;
        let inbox = project.path().join(".ldgr/recovery/inbox");
        fs::create_dir_all(&inbox)?;
        let occurrence = "0198f000-0000-7000-8000-000000000005";
        let mut envelope = valid_error_envelope(&connection, project.path(), occurrence)?;
        envelope["fingerprint"]["value"] = Value::String(
            legacy_struct_order_fingerprint(&envelope["fingerprint"]["inputs"])?
                .context("fixture has every legacy fingerprint field")?,
        );
        fs::write(
            inbox.join("legacy-v1.json"),
            serde_json::to_vec_pretty(&envelope)?,
        )?;

        let report = reconcile_test_project(&connection, project.path())?;
        assert_eq!(report.imported, 1);
        assert_eq!(report.quarantined, 0);
        assert!(project
            .path()
            .join(".ldgr/recovery/archive/legacy-v1.json")
            .is_file());
        Ok(())
    }

    #[test]
    fn startup_preserves_live_attempt_without_duplicate_execution_or_error() -> Result<()> {
        let (project, connection) = initialized_project()?;
        let attempt = ExecutionAttempt::begin_or_adopt(project.path())?;
        let report = reconcile_test_project(&connection, project.path())?;
        assert_eq!(report.live_attempts, 1);
        assert_eq!(report.imported, 0);
        assert!(attempt.intent_path.is_file());
        assert!(list_errors(&connection, None, 10)?.is_empty());
        attempt.complete();
        Ok(())
    }

    #[test]
    fn startup_quarantines_corrupt_project_records_without_deleting_bytes() -> Result<()> {
        let (project, connection) = initialized_project()?;
        let inbox = project.path().join(".ldgr/recovery/inbox");
        fs::create_dir_all(&inbox)?;
        let corrupt =
            br#"{"format":"ldgr-error-recovery","schema_version":1,"secret":"not parsed"}"#;
        fs::write(inbox.join("corrupt.json"), corrupt)?;

        let report = reconcile_test_project(&connection, project.path())?;
        assert_eq!(report.quarantined, 1);
        assert_eq!(
            fs::read(
                project
                    .path()
                    .join(".ldgr/recovery/quarantine/corrupt.json")
            )?,
            corrupt
        );
        let errors = list_errors(&connection, None, 10)?;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].code, "recovery-record-quarantined");
        Ok(())
    }

    #[test]
    fn user_spool_imports_only_exact_project_identity() -> Result<()> {
        let (project, connection) = initialized_project()?;
        let user_inbox = project.path().join("user-state/ldgr/recovery/inbox");
        fs::create_dir_all(&user_inbox)?;
        let current_occurrence = "0198f000-0000-7000-8000-000000000011";
        fs::write(
            user_inbox.join("current.json"),
            serde_json::to_vec_pretty(&valid_error_envelope(
                &connection,
                project.path(),
                current_occurrence,
            )?)?,
        )?;
        let mut other = valid_error_envelope(
            &connection,
            project.path(),
            "0198f000-0000-7000-8000-000000000012",
        )?;
        other["project"]["project_id"] = Value::String("another-project".to_owned());
        fs::write(
            user_inbox.join("other.json"),
            serde_json::to_vec_pretty(&other)?,
        )?;

        let mut report = StartupRecoveryReport::default();
        reconcile_directory(
            &connection,
            &user_inbox,
            RecoveryOrigin::UserSpool,
            &project_id(&connection)?,
            &absolute_locator(project.path()),
            &mut report,
        )?;
        assert_eq!(report.imported, 1);
        assert_eq!(report.skipped_other_projects, 1);
        assert!(user_inbox.join("other.json").is_file());
        assert!(project
            .path()
            .join("user-state/ldgr/recovery/archive/current.json")
            .is_file());
        Ok(())
    }

    #[test]
    fn concurrent_startup_reconciliation_converges_without_duplicate_imports() -> Result<()> {
        let (project, connection) = initialized_project()?;
        let inbox = project.path().join(".ldgr/recovery/inbox");
        fs::create_dir_all(&inbox)?;
        let occurrence = "0198f000-0000-7000-8000-000000000021";
        fs::write(
            inbox.join("concurrent.json"),
            serde_json::to_vec_pretty(&valid_error_envelope(
                &connection,
                project.path(),
                occurrence,
            )?)?,
        )?;
        drop(connection);

        let barrier = std::sync::Arc::new(std::sync::Barrier::new(2));
        let mut workers = Vec::new();
        for _ in 0..2 {
            let barrier = barrier.clone();
            let root = project.path().to_path_buf();
            workers.push(std::thread::spawn(
                move || -> Result<StartupRecoveryReport> {
                    let connection = crate::store::open_store(&root.join(".ldgr/ldgr.db"))?;
                    barrier.wait();
                    reconcile_test_project(&connection, &root)
                },
            ));
        }
        for worker in workers {
            worker.join().expect("reconciliation worker panicked")?;
        }
        let connection = crate::store::open_store(&project.path().join(".ldgr/ldgr.db"))?;
        let count: i64 = connection.query_row(
            "SELECT COUNT(*) FROM error_occurrence WHERE occurrence_id=?1",
            [occurrence],
            |row| row.get(0),
        )?;
        assert_eq!(count, 1);
        assert!(project
            .path()
            .join(".ldgr/recovery/archive/concurrent.json")
            .is_file());
        Ok(())
    }

    #[test]
    fn startup_interrupts_dead_worker_and_restores_work_behind_disposition_gate() -> Result<()> {
        let (project, connection) = initialized_project()?;
        create_work_item(
            &connection,
            None,
            "resume-me",
            "Resume me",
            "Restore this work after a dead worker.",
        )?;
        let run = start_run(&connection, "resume-me", Some("agentctl"))?;
        record_run_phase(&connection, run.id, "running_agent", "Worker is active.")?;
        let dead_pid = u32::MAX;
        connection.execute(
            "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
             VALUES ('run', ?1, 'phase', ?2)",
            params![
                run.id,
                json!({
                    "phase": "running_agent",
                    "progress_report": "Worker last observed before interruption.",
                    "process_id": dead_pid,
                })
                .to_string()
            ],
        )?;
        let inbox = project.path().join(".ldgr/recovery/inbox");
        fs::create_dir_all(&inbox)?;
        let attempt_id = "0198f000-0000-7000-8000-000000000002";
        fs::write(
            inbox.join(format!("{attempt_id}.intent.json")),
            serde_json::to_vec_pretty(&json!({
                "format": "ldgr-execution-intent",
                "schema_version": 1,
                "project": {
                    "project_id": project_id(&connection)?,
                    "locator": absolute_locator(project.path()),
                    "database_identity": null,
                },
                "producer": "ldgr-loop-launcher",
                "operation_id": "0198f000-0000-7000-8000-000000000003",
                "attempt_id": attempt_id,
                "boundary": "loop-launch",
                "subject": "autonomous-loop",
                "environment": {"os": env::consts::OS},
                "accepted_at": "2026-07-31T00:00:00Z",
                "process_id": dead_pid,
            }))?,
        )?;

        let report = reconcile_test_project(&connection, project.path())?;
        assert_eq!(report.interrupted_attempts, 1);
        assert_eq!(report.restored_runs, 1);
        assert!(report.requires_disposition());
        assert_eq!(
            get_run_by_id(&connection, run.id)?.status,
            RunStatus::Partial
        );
        assert_eq!(
            get_work_item_by_slug(&connection, "resume-me")?.status,
            WorkItemStatus::Pending
        );
        let retry = start_run(&connection, "resume-me", Some("retry"))
            .expect_err("blocking recovered interruption must prevent retry");
        assert!(
            format!("{retry:#}").contains("blocked by: error:"),
            "{retry:#}"
        );
        Ok(())
    }
}
