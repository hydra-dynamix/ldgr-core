use super::*;
use sha2::{Digest, Sha256};

const ERROR_SELECT: &str = "
    SELECT id, project_id, fingerprint_version, fingerprint, class, domain, code,
           severity, retryability, state, first_seen_at, last_seen_at,
           occurrence_count, latest_occurrence_id, disposition_pending, created_at, updated_at
    FROM error_record";

const OCCURRENCE_SELECT: &str = "
    SELECT occurrence_id, error_id, producer, idempotency_key, operation_id, attempt_id,
           class, domain, code, severity, retryability, source, summary, details_json,
           environment_json, observed_at, recorded_at, recovery_origin, payload_digest
    FROM error_occurrence";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorClass {
    TaskFailure,
    ValidationFailure,
    InfrastructureError,
    Interruption,
    OperatorCancellation,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorRetryability {
    Never,
    AfterChange,
    Transient,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorState {
    Open,
    Acknowledged,
    Resolved,
    Accepted,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RecoveryOrigin {
    Database,
    ProjectInbox,
    UserSpool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ErrorDispositionAction {
    Retry,
    Workaround,
    Defer,
    Accept,
    Escalate,
    Cancel,
    Resolve,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RetryBasis {
    NewEvidence,
    ChangedCondition,
    ChangedDecision,
    ExplicitConfirmation,
}

macro_rules! string_enum {
    ($type:ty, {$($variant:ident => $value:literal),+ $(,)?}) => {
        impl $type {
            pub fn as_str(self) -> &'static str {
                match self { $(Self::$variant => $value),+ }
            }
        }

        impl fmt::Display for $type {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl FromStr for $type {
            type Err = anyhow::Error;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => bail!("invalid {} `{value}`", stringify!($type)),
                }
            }
        }
    };
}

string_enum!(ErrorClass, {
    TaskFailure => "task-failure",
    ValidationFailure => "validation-failure",
    InfrastructureError => "infrastructure-error",
    Interruption => "interruption",
    OperatorCancellation => "operator-cancellation",
});
string_enum!(ErrorSeverity, {
    Info => "info",
    Warning => "warning",
    Error => "error",
    Critical => "critical",
});
string_enum!(ErrorRetryability, {
    Never => "never",
    AfterChange => "after-change",
    Transient => "transient",
    Unknown => "unknown",
});
string_enum!(ErrorState, {
    Open => "open",
    Acknowledged => "acknowledged",
    Resolved => "resolved",
    Accepted => "accepted",
});
string_enum!(RecoveryOrigin, {
    Database => "database",
    ProjectInbox => "project-inbox",
    UserSpool => "user-spool",
});
string_enum!(ErrorDispositionAction, {
    Retry => "retry",
    Workaround => "workaround",
    Defer => "defer",
    Accept => "accept",
    Escalate => "escalate",
    Cancel => "cancel",
    Resolve => "resolve",
});
string_enum!(RetryBasis, {
    NewEvidence => "new-evidence",
    ChangedCondition => "changed-condition",
    ChangedDecision => "changed-decision",
    ExplicitConfirmation => "explicit-confirmation",
});

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorRecord {
    pub id: i64,
    pub project_id: String,
    pub fingerprint_version: String,
    pub fingerprint: String,
    pub class: ErrorClass,
    pub domain: String,
    pub code: String,
    pub severity: ErrorSeverity,
    pub retryability: ErrorRetryability,
    pub state: ErrorState,
    pub first_seen_at: String,
    pub last_seen_at: String,
    pub occurrence_count: i64,
    pub latest_occurrence_id: String,
    pub disposition_pending: bool,
    pub created_at: String,
    pub updated_at: String,
}

impl ErrorRecord {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            project_id: row.get("project_id")?,
            fingerprint_version: row.get("fingerprint_version")?,
            fingerprint: row.get("fingerprint")?,
            class: parse_sql_enum(row, "class")?,
            domain: row.get("domain")?,
            code: row.get("code")?,
            severity: parse_sql_enum(row, "severity")?,
            retryability: parse_sql_enum(row, "retryability")?,
            state: parse_sql_enum(row, "state")?,
            first_seen_at: row.get("first_seen_at")?,
            last_seen_at: row.get("last_seen_at")?,
            occurrence_count: row.get("occurrence_count")?,
            latest_occurrence_id: row.get("latest_occurrence_id")?,
            disposition_pending: row.get::<_, i64>("disposition_pending")? != 0,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorOccurrence {
    pub occurrence_id: String,
    pub error_id: i64,
    pub producer: String,
    pub idempotency_key: String,
    pub operation_id: String,
    pub attempt_id: String,
    pub class: ErrorClass,
    pub domain: String,
    pub code: String,
    pub severity: ErrorSeverity,
    pub retryability: ErrorRetryability,
    pub source: String,
    pub summary: String,
    pub details: serde_json::Value,
    pub environment: serde_json::Value,
    pub observed_at: String,
    pub recorded_at: String,
    pub recovery_origin: RecoveryOrigin,
    pub payload_digest: String,
}

impl ErrorOccurrence {
    fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            occurrence_id: row.get("occurrence_id")?,
            error_id: row.get("error_id")?,
            producer: row.get("producer")?,
            idempotency_key: row.get("idempotency_key")?,
            operation_id: row.get("operation_id")?,
            attempt_id: row.get("attempt_id")?,
            class: parse_sql_enum(row, "class")?,
            domain: row.get("domain")?,
            code: row.get("code")?,
            severity: parse_sql_enum(row, "severity")?,
            retryability: parse_sql_enum(row, "retryability")?,
            source: row.get("source")?,
            summary: row.get("summary")?,
            details: parse_json_column(row, "details_json")?,
            environment: parse_json_column(row, "environment_json")?,
            observed_at: row.get("observed_at")?,
            recorded_at: row.get("recorded_at")?,
            recovery_origin: parse_sql_enum(row, "recovery_origin")?,
            payload_digest: row.get("payload_digest")?,
        })
    }
}

fn parse_sql_enum<T>(row: &Row<'_>, column: &str) -> rusqlite::Result<T>
where
    T: FromStr,
    T::Err: fmt::Display,
{
    row.get::<_, String>(column)?.parse::<T>().map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(
            0,
            rusqlite::types::Type::Text,
            Box::new(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                error.to_string(),
            )),
        )
    })
}

fn parse_json_column(row: &Row<'_>, column: &str) -> rusqlite::Result<serde_json::Value> {
    serde_json::from_str(&row.get::<_, String>(column)?).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
    })
}

#[derive(Debug, Clone, Serialize)]
pub struct RecordErrorInput<'a> {
    pub occurrence_id: &'a str,
    pub producer: &'a str,
    pub idempotency_key: &'a str,
    pub operation_id: &'a str,
    pub attempt_id: &'a str,
    pub fingerprint_version: &'a str,
    pub fingerprint: &'a str,
    pub fingerprint_inputs: Option<&'a serde_json::Value>,
    pub fingerprint_provenance: Option<&'a FingerprintProvenance>,
    pub class: ErrorClass,
    pub domain: &'a str,
    pub code: &'a str,
    pub severity: ErrorSeverity,
    pub retryability: ErrorRetryability,
    pub source: &'a str,
    pub summary: &'a str,
    pub details: &'a serde_json::Value,
    pub environment: &'a serde_json::Value,
    pub observed_at: &'a str,
    pub recovery_origin: RecoveryOrigin,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct RecordErrorResult {
    pub error: ErrorRecord,
    pub occurrence: ErrorOccurrence,
    pub idempotent_replay: bool,
    pub recurrent: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retry_gate: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context: Option<ErrorContextPacket>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorView {
    pub error: ErrorRecord,
    pub occurrences: Vec<ErrorOccurrence>,
    pub relations: Vec<ErrorRelation>,
    pub transitions: Vec<ErrorTransition>,
    pub dispositions: Vec<ErrorDisposition>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorTransition {
    pub id: i64,
    pub error_id: i64,
    pub occurrence_id: Option<String>,
    pub old_state: ErrorState,
    pub new_state: ErrorState,
    pub actor: String,
    pub source: String,
    pub rationale: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorRelation {
    pub id: i64,
    pub error_id: i64,
    pub occurrence_id: Option<String>,
    pub relation_kind: String,
    pub entity_type: String,
    pub entity_id: String,
    pub source: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorDisposition {
    pub id: i64,
    pub error_id: i64,
    pub occurrence_id: String,
    pub action: ErrorDispositionAction,
    pub actor: String,
    pub source: String,
    pub rationale: String,
    pub decision_id: Option<i64>,
    pub retry_basis: Option<RetryBasis>,
    pub prior_disposition_id: Option<i64>,
    pub evidence_relation_ids: Vec<i64>,
    pub resulting_work_transition: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ErrorSurfaceBounds {
    pub errors: usize,
    pub related_work_per_error: usize,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ErrorSurfaceCounts {
    pub total: i64,
    pub unresolved: i64,
    pub repeated: i64,
    pub disposition_pending: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorOccurrenceSurface {
    pub occurrence_id: String,
    pub operation_id: String,
    pub attempt_id: String,
    pub source: String,
    pub summary: String,
    pub observed_at: String,
    pub recorded_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorRelatedWorkSurface {
    pub slug: String,
    pub title: String,
    pub status: WorkItemStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorDispositionSurface {
    pub action: ErrorDispositionAction,
    pub occurrence_id: String,
    pub actor: String,
    pub source: String,
    pub rationale: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorSurfaceItem {
    pub error_id: i64,
    pub state: ErrorState,
    pub class: ErrorClass,
    pub domain: String,
    pub code: String,
    pub severity: ErrorSeverity,
    pub retryability: ErrorRetryability,
    pub occurrence_count: i64,
    pub repeated: bool,
    pub disposition_pending: bool,
    pub latest_occurrence: ErrorOccurrenceSurface,
    pub related_work: Vec<ErrorRelatedWorkSurface>,
    pub related_work_truncated: bool,
    pub latest_disposition: Option<ErrorDispositionSurface>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ErrorSurface {
    pub counts: ErrorSurfaceCounts,
    pub latest: Vec<ErrorSurfaceItem>,
    pub truncated: bool,
    pub bounds: ErrorSurfaceBounds,
}

#[derive(Debug, Clone)]
pub struct RecordErrorDispositionInput<'a> {
    pub error_id: i64,
    pub occurrence_id: Option<&'a str>,
    pub action: ErrorDispositionAction,
    pub actor: &'a str,
    pub source: &'a str,
    pub rationale: &'a str,
    pub decision_id: Option<i64>,
    pub retry_basis: Option<RetryBasis>,
    pub prior_disposition_id: Option<i64>,
    pub evidence_relation_ids: &'a [i64],
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ErrorRetryAuthorization {
    pub error: ErrorRecord,
    pub occurrence: ErrorOccurrence,
    pub disposition: ErrorDisposition,
    pub context: ErrorContextPacket,
}

pub fn record_error(
    connection: &Connection,
    input: &RecordErrorInput<'_>,
) -> anyhow::Result<RecordErrorResult> {
    validate_record_input(input)?;
    let observed_at_valid: i64 = connection.query_row(
        "SELECT julianday(?1) IS NOT NULL AND substr(?1, -1) = 'Z'",
        params![input.observed_at],
        |row| row.get(0),
    )?;
    anyhow::ensure!(
        observed_at_valid != 0,
        "observed_at must be an RFC 3339 UTC timestamp ending in `Z`"
    );
    let canonical = serde_json::to_vec(input).context("failed to canonicalize error occurrence")?;
    let payload_digest = sha256_digest(&canonical);
    let mut result = in_write_transaction(connection, |connection| {
        if let Some(existing) = find_occurrence_by_identity(
            connection,
            input.occurrence_id,
            input.producer,
            input.idempotency_key,
        )? {
            anyhow::ensure!(
                existing.payload_digest == payload_digest,
                "error occurrence idempotency conflict: the supplied occurrence identity or producer key already has different content"
            );
            let error = get_error(connection, existing.error_id)?;
            return Ok(RecordErrorResult {
                recurrent: error.occurrence_count > 1,
                error,
                occurrence: existing,
                idempotent_replay: true,
                retry_gate: None,
                context: None,
            });
        }

        let project_id = project_id(connection)?;
        let existing_error_id = connection
            .query_row(
                "SELECT id FROM error_record
                 WHERE project_id=?1 AND fingerprint_version=?2 AND fingerprint=?3",
                params![project_id, input.fingerprint_version, input.fingerprint],
                |row| row.get::<_, i64>(0),
            )
            .optional()?;
        let error_id = if let Some(id) = existing_error_id {
            id
        } else {
            connection.execute(
                "INSERT INTO error_record (
                    project_id, fingerprint_version, fingerprint, class, domain, code,
                    severity, retryability, state, first_seen_at, last_seen_at,
                    occurrence_count, latest_occurrence_id, disposition_pending
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'open', ?9, ?9, 0, ?10, 1)",
                params![
                    project_id,
                    input.fingerprint_version,
                    input.fingerprint,
                    input.class.as_str(),
                    input.domain,
                    input.code,
                    input.severity.as_str(),
                    input.retryability.as_str(),
                    input.observed_at,
                    input.occurrence_id,
                ],
            )?;
            connection.last_insert_rowid()
        };

        let previous = get_error(connection, error_id)?;
        anyhow::ensure!(
            previous.class == input.class
                && previous.domain == input.domain
                && previous.code == input.code,
            "fingerprint identity conflict: the existing aggregate has classification {}/{} ({})",
            previous.domain,
            previous.code,
            previous.class
        );
        if let (Some(existing_inputs), Some(supplied_inputs)) = (
            existing_fingerprint_inputs(connection, error_id)?,
            input.fingerprint_inputs,
        ) {
            anyhow::ensure!(
                existing_inputs == *supplied_inputs,
                "fingerprint collision: aggregate {error_id} has different structured inputs; use an explicit fingerprint split with rationale"
            );
        }
        connection.execute(
            "INSERT INTO error_occurrence (
                occurrence_id, error_id, producer, idempotency_key, operation_id, attempt_id,
                class, domain, code, severity, retryability, source, summary, details_json,
                environment_json, observed_at, recovery_origin, payload_digest
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                input.occurrence_id,
                error_id,
                input.producer,
                input.idempotency_key,
                input.operation_id,
                input.attempt_id,
                input.class.as_str(),
                input.domain,
                input.code,
                input.severity.as_str(),
                input.retryability.as_str(),
                input.source,
                input.summary,
                serde_json::to_string(input.details)?,
                serde_json::to_string(input.environment)?,
                input.observed_at,
                input.recovery_origin.as_str(),
                payload_digest,
            ],
        )?;

        if previous.state == ErrorState::Resolved && previous.occurrence_count > 0 {
            append_transition(
                connection,
                error_id,
                Some(input.occurrence_id),
                ErrorState::Resolved,
                ErrorState::Open,
                input.producer,
                input.source,
                "new occurrence reopened resolved error",
            )?;
        }
        rebuild_error_projection(connection, error_id)?;
        connection.execute(
            "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
             VALUES ('error', ?1, 'occurrence_recorded', ?2)",
            params![
                error_id,
                serde_json::to_string(&serde_json::json!({
                    "occurrence_id": input.occurrence_id,
                    "producer": input.producer,
                    "idempotency_key": input.idempotency_key,
                    "fingerprint_version": input.fingerprint_version,
                    "fingerprint": input.fingerprint,
                    "fingerprint_inputs": input.fingerprint_inputs,
                    "fingerprint_provenance": input.fingerprint_provenance,
                }))?
            ],
        )?;
        if let Some(provenance) = input.fingerprint_provenance {
            let event_type = match provenance.kind {
                FingerprintProvenanceKind::Computed => None,
                FingerprintProvenanceKind::Override => Some("fingerprint_override"),
                FingerprintProvenanceKind::Split => Some("fingerprint_split"),
            };
            if let Some(event_type) = event_type {
                connection.execute(
                    "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
                     VALUES ('error', ?1, ?2, ?3)",
                    params![
                        error_id,
                        event_type,
                        serde_json::to_string(&serde_json::json!({
                            "occurrence_id": input.occurrence_id,
                            "fingerprint_version": input.fingerprint_version,
                            "fingerprint": input.fingerprint,
                            "fingerprint_inputs": input.fingerprint_inputs,
                            "provenance": provenance,
                        }))?
                    ],
                )?;
            }
        }
        crate::fault_injection::crash_if("error-database-recording");
        let error = get_error(connection, error_id)?;
        let occurrence = get_error_occurrence(connection, input.occurrence_id)?;
        Ok(RecordErrorResult {
            recurrent: error.occurrence_count > 1,
            error,
            occurrence,
            idempotent_replay: false,
            retry_gate: None,
            context: None,
        })
    })?;
    if result.recurrent {
        result.context = Some(error_context_packet(
            connection,
            result.error.id,
            Some(&result.occurrence.occurrence_id),
            ErrorContextBounds::default(),
        )?);
        result.retry_gate = Some("disposition-required".to_owned());
    }
    Ok(result)
}

fn existing_fingerprint_inputs(
    connection: &Connection,
    error_id: i64,
) -> anyhow::Result<Option<serde_json::Value>> {
    let payload = connection
        .query_row(
            "SELECT payload_json FROM event_log
             WHERE entity_type='error' AND entity_id=?1 AND event_type='occurrence_recorded'
               AND json_type(payload_json, '$.fingerprint_inputs') IS NOT NULL
               AND json_type(payload_json, '$.fingerprint_inputs') != 'null'
             ORDER BY id LIMIT 1",
            params![error_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    payload
        .map(|payload| {
            let payload: serde_json::Value = serde_json::from_str(&payload)?;
            payload
                .get("fingerprint_inputs")
                .cloned()
                .context("fingerprint event is missing fingerprint_inputs")
        })
        .transpose()
}

fn validate_record_input(input: &RecordErrorInput<'_>) -> anyhow::Result<()> {
    for (name, value) in [
        ("occurrence_id", input.occurrence_id),
        ("producer", input.producer),
        ("idempotency_key", input.idempotency_key),
        ("operation_id", input.operation_id),
        ("attempt_id", input.attempt_id),
        ("fingerprint_version", input.fingerprint_version),
        ("domain", input.domain),
        ("code", input.code),
        ("source", input.source),
        ("summary", input.summary),
        ("observed_at", input.observed_at),
    ] {
        anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    }
    anyhow::ensure!(
        is_sha256_digest(input.fingerprint),
        "fingerprint must be `sha256:` followed by 64 lowercase hexadecimal characters"
    );
    anyhow::ensure!(input.details.is_object(), "details must be a JSON object");
    anyhow::ensure!(
        input.environment.is_object(),
        "environment must be a JSON object"
    );
    anyhow::ensure!(input.summary.len() <= 4096, "summary exceeds 4096 bytes");
    anyhow::ensure!(
        serde_json::to_vec(input.details)?.len() <= 65_536,
        "details exceeds 65536 bytes"
    );
    anyhow::ensure!(
        serde_json::to_vec(input.environment)?.len() <= 16_384,
        "environment exceeds 16384 bytes"
    );
    Ok(())
}

fn find_occurrence_by_identity(
    connection: &Connection,
    occurrence_id: &str,
    producer: &str,
    idempotency_key: &str,
) -> anyhow::Result<Option<ErrorOccurrence>> {
    let by_occurrence_sql = format!("{OCCURRENCE_SELECT} WHERE occurrence_id=?1");
    let by_occurrence = connection
        .query_row(
            &by_occurrence_sql,
            params![occurrence_id],
            ErrorOccurrence::from_row,
        )
        .optional()?;
    let by_key_sql = format!("{OCCURRENCE_SELECT} WHERE producer=?1 AND idempotency_key=?2");
    let by_key = connection
        .query_row(
            &by_key_sql,
            params![producer, idempotency_key],
            ErrorOccurrence::from_row,
        )
        .optional()?;
    if let (Some(left), Some(right)) = (&by_occurrence, &by_key) {
        anyhow::ensure!(
            left.occurrence_id == right.occurrence_id,
            "error occurrence idempotency conflict: occurrence ID and producer key identify different immutable occurrences"
        );
    }
    Ok(by_occurrence.or(by_key))
}

pub fn list_errors(
    connection: &Connection,
    state: Option<ErrorState>,
    limit: i64,
) -> anyhow::Result<Vec<ErrorRecord>> {
    anyhow::ensure!(limit >= 0, "limit must not be negative");
    let sql = format!(
        "{ERROR_SELECT}
         WHERE (?1 IS NULL OR state=?1)
         ORDER BY julianday(last_seen_at) DESC, id DESC LIMIT ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let result = statement
        .query_map(
            params![state.map(ErrorState::as_str), limit],
            ErrorRecord::from_row,
        )?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list errors");
    result
}

pub fn error_surface(
    connection: &Connection,
    error_limit: usize,
    related_work_limit: usize,
) -> anyhow::Result<ErrorSurface> {
    let counts = connection.query_row(
        "SELECT
             COUNT(*),
             COALESCE(SUM(CASE WHEN state IN ('open', 'acknowledged') THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(CASE WHEN occurrence_count > 1 THEN 1 ELSE 0 END), 0),
             COALESCE(SUM(disposition_pending), 0)
         FROM error_record",
        [],
        |row| {
            Ok(ErrorSurfaceCounts {
                total: row.get(0)?,
                unresolved: row.get(1)?,
                repeated: row.get(2)?,
                disposition_pending: row.get(3)?,
            })
        },
    )?;
    let errors = list_errors(
        connection,
        None,
        i64::try_from(error_limit).context("error surface limit does not fit in i64")?,
    )?;
    let latest = errors
        .into_iter()
        .map(|error| error_surface_item(connection, error, related_work_limit))
        .collect::<anyhow::Result<Vec<_>>>()?;
    Ok(ErrorSurface {
        truncated: counts.total > i64::try_from(latest.len()).unwrap_or(i64::MAX),
        counts,
        latest,
        bounds: ErrorSurfaceBounds {
            errors: error_limit,
            related_work_per_error: related_work_limit,
        },
    })
}

fn error_surface_item(
    connection: &Connection,
    error: ErrorRecord,
    related_work_limit: usize,
) -> anyhow::Result<ErrorSurfaceItem> {
    let occurrence = get_error_occurrence(connection, &error.latest_occurrence_id)?;
    let mut redaction = ContextRedaction::default();
    let latest_disposition_id = connection
        .query_row(
            "SELECT id FROM error_disposition
             WHERE error_id=?1
             ORDER BY datetime(created_at) DESC, id DESC
             LIMIT 1",
            params![error.id],
            |row| row.get::<_, i64>(0),
        )
        .optional()?;
    let latest_disposition = latest_disposition_id
        .map(|id| get_error_disposition(connection, id))
        .transpose()?
        .map(|disposition| ErrorDispositionSurface {
            action: disposition.action,
            occurrence_id: disposition.occurrence_id,
            actor: disposition.actor,
            source: super::error_context::redact_text(&disposition.source, &mut redaction, 256),
            rationale: super::error_context::redact_text(
                &disposition.rationale,
                &mut redaction,
                1024,
            ),
            created_at: disposition.created_at,
        });
    let mut related_work =
        error_related_work(connection, error.id, related_work_limit.saturating_add(1))?;
    let related_work_truncated = related_work.len() > related_work_limit;
    related_work.truncate(related_work_limit);
    for work in &mut related_work {
        work.title = super::error_context::redact_text(&work.title, &mut redaction, 512);
    }
    Ok(ErrorSurfaceItem {
        error_id: error.id,
        state: error.state,
        class: error.class,
        domain: error.domain,
        code: error.code,
        severity: error.severity,
        retryability: error.retryability,
        occurrence_count: error.occurrence_count,
        repeated: error.occurrence_count > 1,
        disposition_pending: error.disposition_pending,
        latest_occurrence: ErrorOccurrenceSurface {
            occurrence_id: occurrence.occurrence_id,
            operation_id: occurrence.operation_id,
            attempt_id: occurrence.attempt_id,
            source: super::error_context::redact_text(&occurrence.source, &mut redaction, 256),
            summary: super::error_context::redact_text(&occurrence.summary, &mut redaction, 1024),
            observed_at: occurrence.observed_at,
            recorded_at: occurrence.recorded_at,
        },
        related_work,
        related_work_truncated,
        latest_disposition,
    })
}

fn error_related_work(
    connection: &Connection,
    error_id: i64,
    limit: usize,
) -> anyhow::Result<Vec<ErrorRelatedWorkSurface>> {
    let limit = i64::try_from(limit).context("related work limit does not fit in i64")?;
    let mut statement = connection.prepare(
        "SELECT DISTINCT work.slug, work.title, work.status, work.updated_at, work.id
         FROM work_item AS work
         WHERE EXISTS (
             SELECT 1
             FROM error_relation AS relation
             LEFT JOIN run AS related_run
               ON relation.entity_type='run'
              AND CAST(relation.entity_id AS INTEGER)=related_run.id
             WHERE relation.error_id=?1
               AND (
                   (relation.entity_type='work_item'
                    AND CAST(relation.entity_id AS INTEGER)=work.id)
                   OR related_run.work_item_id=work.id
               )
         )
         ORDER BY datetime(work.updated_at) DESC, work.id DESC
         LIMIT ?2",
    )?;
    let related_work = statement
        .query_map(params![error_id, limit], |row| {
            let status = row
                .get::<_, String>(2)?
                .parse::<WorkItemStatus>()
                .map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        2,
                        rusqlite::types::Type::Text,
                        Box::new(std::io::Error::new(
                            std::io::ErrorKind::InvalidData,
                            error.to_string(),
                        )),
                    )
                })?;
            Ok(ErrorRelatedWorkSurface {
                slug: row.get(0)?,
                title: row.get(1)?,
                status,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list error-related work")?;
    Ok(related_work)
}

pub fn blocking_errors_for_work_item(
    connection: &Connection,
    work_item_id: i64,
) -> anyhow::Result<Vec<ErrorRecord>> {
    let sql = format!(
        "{ERROR_SELECT}
         WHERE (
             disposition_pending=1
             OR (state IN ('open', 'acknowledged') AND severity IN ('error', 'critical'))
         )
         AND EXISTS (
             SELECT 1 FROM error_relation AS relation
             LEFT JOIN run AS related_run
               ON relation.entity_type='run'
              AND CAST(relation.entity_id AS INTEGER)=related_run.id
             WHERE relation.error_id=error_record.id
               AND (
                   (relation.entity_type='work_item'
                    AND CAST(relation.entity_id AS INTEGER)=?1)
                   OR related_run.work_item_id=?1
               )
         )
         ORDER BY severity DESC, last_seen_at DESC, id"
    );
    let mut statement = connection.prepare(&sql)?;
    let result = statement
        .query_map(params![work_item_id], ErrorRecord::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list blocking errors for work item");
    result
}

pub fn ensure_no_blocking_errors_for_work_item(
    connection: &Connection,
    work_item_id: i64,
    action: &str,
) -> anyhow::Result<()> {
    let blockers = blocking_errors_for_work_item(connection, work_item_id)?;
    if blockers.is_empty() {
        return Ok(());
    }
    let summary = blockers
        .iter()
        .map(|error| {
            format!(
                "{}:{}/{} state={} disposition_pending={}",
                error.id, error.domain, error.code, error.state, error.disposition_pending
            )
        })
        .collect::<Vec<_>>()
        .join(", ");
    bail!(
        "cannot {action} while related blocking errors remain: {summary}; inspect with `ldgr error show <id>` and record an explicit disposition"
    )
}

pub fn get_error(connection: &Connection, error_id: i64) -> anyhow::Result<ErrorRecord> {
    let sql = format!("{ERROR_SELECT} WHERE id=?1");
    connection
        .query_row(&sql, params![error_id], ErrorRecord::from_row)
        .optional()?
        .with_context(|| format!("error {error_id} not found"))
}

pub fn show_error(connection: &Connection, error_id: i64) -> anyhow::Result<ErrorView> {
    Ok(ErrorView {
        error: get_error(connection, error_id)?,
        occurrences: list_error_occurrences(connection, Some(error_id), 1000)?,
        relations: list_error_relations(connection, error_id)?,
        transitions: list_error_transitions(connection, error_id)?,
        dispositions: list_error_dispositions(connection, error_id)?,
    })
}

pub fn get_error_occurrence(
    connection: &Connection,
    occurrence_id: &str,
) -> anyhow::Result<ErrorOccurrence> {
    let sql = format!("{OCCURRENCE_SELECT} WHERE occurrence_id=?1");
    connection
        .query_row(&sql, params![occurrence_id], ErrorOccurrence::from_row)
        .optional()?
        .with_context(|| format!("error occurrence {occurrence_id} not found"))
}

pub fn list_error_occurrences(
    connection: &Connection,
    error_id: Option<i64>,
    limit: i64,
) -> anyhow::Result<Vec<ErrorOccurrence>> {
    anyhow::ensure!(limit >= 0, "limit must not be negative");
    let sql = format!(
        "{OCCURRENCE_SELECT}
         WHERE (?1 IS NULL OR error_id=?1)
         ORDER BY julianday(observed_at) DESC, julianday(recorded_at) DESC, occurrence_id DESC LIMIT ?2"
    );
    let mut statement = connection.prepare(&sql)?;
    let result = statement
        .query_map(params![error_id, limit], ErrorOccurrence::from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list error occurrences");
    result
}

pub fn record_error_disposition(
    connection: &Connection,
    input: &RecordErrorDispositionInput<'_>,
) -> anyhow::Result<ErrorDisposition> {
    for (name, value) in [
        ("actor", input.actor),
        ("source", input.source),
        ("rationale", input.rationale),
    ] {
        anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    }
    anyhow::ensure!(
        input.rationale.len() <= 4096,
        "rationale exceeds 4096 bytes"
    );
    in_write_transaction(connection, |connection| {
        let error = get_error(connection, input.error_id)?;
        let occurrence_id = input.occurrence_id.unwrap_or(&error.latest_occurrence_id);
        let occurrence = get_error_occurrence(connection, occurrence_id)?;
        anyhow::ensure!(
            occurrence.error_id == error.id,
            "occurrence {occurrence_id} does not belong to error {}",
            error.id
        );
        if matches!(
            input.action,
            ErrorDispositionAction::Accept | ErrorDispositionAction::Resolve
        ) {
            anyhow::ensure!(
                occurrence_id == error.latest_occurrence_id,
                "{} must target the latest occurrence {}",
                input.action,
                error.latest_occurrence_id
            );
        }
        if let Some(decision_id) = input.decision_id {
            validate_relation_target(connection, "decision", &decision_id.to_string())?;
        }
        for relation_id in input.evidence_relation_ids {
            let relation_error_id = connection
                .query_row(
                    "SELECT error_id FROM error_relation WHERE id=?1",
                    params![relation_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
                .with_context(|| format!("evidence relation {relation_id} not found"))?;
            anyhow::ensure!(
                relation_error_id == error.id,
                "evidence relation {relation_id} does not belong to error {}",
                error.id
            );
        }

        let prior = input
            .prior_disposition_id
            .map(|id| get_error_disposition(connection, id))
            .transpose()?;
        if let Some(prior) = &prior {
            anyhow::ensure!(
                prior.error_id == error.id,
                "prior disposition {} does not belong to error {}",
                prior.id,
                error.id
            );
            anyhow::ensure!(
                occurrence_precedes(connection, &prior.occurrence_id, occurrence_id)?,
                "prior disposition must reference an earlier occurrence"
            );
        }
        let repeated = occurrence_has_predecessor(connection, &occurrence)?;
        validate_retry_disposition(
            connection,
            input,
            &error,
            &occurrence,
            prior.as_ref(),
            repeated,
        )?;

        connection.execute(
            "INSERT INTO error_disposition (
                error_id, occurrence_id, disposition, actor, source, rationale
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                error.id,
                occurrence_id,
                input.action.as_str(),
                input.actor,
                input.source,
                input.rationale,
            ],
        )?;
        let disposition_id = connection.last_insert_rowid();
        if let Some(decision_id) = input.decision_id {
            connection.execute(
                "INSERT OR IGNORE INTO error_relation (
                    error_id, occurrence_id, relation_kind, entity_type, entity_id, source
                 ) VALUES (?1, ?2, 'disposition-decision', 'decision', ?3, ?4)",
                params![
                    error.id,
                    occurrence_id,
                    decision_id.to_string(),
                    input.source
                ],
            )?;
        }

        let resulting_work_transition = resulting_work_transition(input.action);
        connection.execute(
            "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
             VALUES ('error', ?1, 'disposition_recorded', ?2)",
            params![
                error.id,
                serde_json::to_string(&serde_json::json!({
                    "disposition_id": disposition_id,
                    "occurrence_id": occurrence_id,
                    "action": input.action,
                    "actor": input.actor,
                    "source": input.source,
                    "rationale": input.rationale,
                    "decision_id": input.decision_id,
                    "retry_basis": input.retry_basis,
                    "prior_disposition_id": input.prior_disposition_id,
                    "evidence_relation_ids": input.evidence_relation_ids,
                    "resulting_work_transition": resulting_work_transition,
                }))?
            ],
        )?;

        if occurrence_id == error.latest_occurrence_id {
            connection.execute(
                "UPDATE error_record SET disposition_pending=0, updated_at=datetime('now')
                 WHERE id=?1",
                params![error.id],
            )?;
            let new_state = match input.action {
                ErrorDispositionAction::Accept => Some(ErrorState::Accepted),
                ErrorDispositionAction::Resolve => Some(ErrorState::Resolved),
                _ => None,
            };
            if let Some(new_state) = new_state {
                if error.state != new_state {
                    let valid = matches!(
                        (error.state, new_state),
                        (ErrorState::Open, ErrorState::Resolved)
                            | (ErrorState::Open, ErrorState::Accepted)
                            | (ErrorState::Acknowledged, ErrorState::Resolved)
                            | (ErrorState::Acknowledged, ErrorState::Accepted)
                            | (ErrorState::Resolved, ErrorState::Accepted)
                    );
                    anyhow::ensure!(
                        valid,
                        "invalid error lifecycle transition {} -> {}",
                        error.state,
                        new_state
                    );
                    append_transition(
                        connection,
                        error.id,
                        Some(occurrence_id),
                        error.state,
                        new_state,
                        input.actor,
                        input.source,
                        input.rationale,
                    )?;
                    connection.execute(
                        "UPDATE error_record SET state=?1, updated_at=datetime('now') WHERE id=?2",
                        params![new_state.as_str(), error.id],
                    )?;
                }
            }
        }
        get_error_disposition(connection, disposition_id)
    })
}

fn validate_retry_disposition(
    connection: &Connection,
    input: &RecordErrorDispositionInput<'_>,
    error: &ErrorRecord,
    occurrence: &ErrorOccurrence,
    prior: Option<&ErrorDisposition>,
    repeated: bool,
) -> anyhow::Result<()> {
    if input.action != ErrorDispositionAction::Retry {
        anyhow::ensure!(
            input.retry_basis.is_none() && input.prior_disposition_id.is_none(),
            "retry basis and prior disposition are valid only for a retry disposition"
        );
        return Ok(());
    }
    if !repeated {
        return Ok(());
    }
    let prior = prior.context(
        "retrying a repeated error requires --prior-disposition-id from the surfaced context",
    )?;
    let basis = input.retry_basis.context(
        "retrying a repeated error requires --retry-basis new-evidence, changed-condition, changed-decision, or explicit-confirmation",
    )?;
    match basis {
        RetryBasis::NewEvidence => {
            anyhow::ensure!(
                !input.evidence_relation_ids.is_empty(),
                "new-evidence retry requires at least one --evidence-relation-id"
            );
            let prior_event_id = connection.query_row(
                "SELECT id FROM event_log
                 WHERE entity_type='error' AND entity_id=?1 AND event_type='disposition_recorded'
                   AND CAST(json_extract(payload_json, '$.disposition_id') AS INTEGER)=?2
                 ORDER BY id DESC LIMIT 1",
                params![error.id, prior.id],
                |row| row.get::<_, i64>(0),
            )?;
            for relation_id in input.evidence_relation_ids {
                let is_new: i64 = connection.query_row(
                    "SELECT EXISTS(
                        SELECT 1 FROM event_log
                        WHERE entity_type='error' AND entity_id=?1
                          AND event_type='relation_added' AND id>?2
                          AND CAST(json_extract(payload_json, '$.relation_id') AS INTEGER)=?3
                    )",
                    params![error.id, prior_event_id, relation_id],
                    |row| row.get(0),
                )?;
                anyhow::ensure!(
                    is_new != 0,
                    "evidence relation {relation_id} is not newer than prior disposition {}",
                    prior.id
                );
            }
        }
        RetryBasis::ChangedCondition => {
            let context = error_context_packet(
                connection,
                error.id,
                Some(&occurrence.occurrence_id),
                ErrorContextBounds::default(),
            )?;
            anyhow::ensure!(
                !context.environment_differences.is_empty()
                    || !input.evidence_relation_ids.is_empty(),
                "changed-condition retry requires a surfaced environment difference or evidence relation"
            );
        }
        RetryBasis::ChangedDecision => {
            let decision_id = input
                .decision_id
                .context("changed-decision retry requires --decision-id")?;
            anyhow::ensure!(
                prior.decision_id != Some(decision_id),
                "changed-decision retry must reference a different causal decision"
            );
        }
        RetryBasis::ExplicitConfirmation => {}
    }
    Ok(())
}

fn occurrence_has_predecessor(
    connection: &Connection,
    occurrence: &ErrorOccurrence,
) -> anyhow::Result<bool> {
    let count: i64 = connection.query_row(
        "SELECT COUNT(*) FROM error_occurrence
         WHERE error_id=?1 AND (
             julianday(observed_at) < julianday(?2)
             OR (julianday(observed_at) = julianday(?2) AND julianday(recorded_at) < julianday(?3))
             OR (julianday(observed_at) = julianday(?2) AND julianday(recorded_at) = julianday(?3)
                 AND occurrence_id < ?4)
         )",
        params![
            occurrence.error_id,
            occurrence.observed_at,
            occurrence.recorded_at,
            occurrence.occurrence_id
        ],
        |row| row.get(0),
    )?;
    Ok(count > 0)
}

fn occurrence_precedes(
    connection: &Connection,
    prior_occurrence_id: &str,
    current_occurrence_id: &str,
) -> anyhow::Result<bool> {
    let precedes: i64 = connection.query_row(
        "SELECT EXISTS(
            SELECT 1
            FROM error_occurrence AS prior
            JOIN error_occurrence AS current ON current.occurrence_id=?2
            WHERE prior.occurrence_id=?1
              AND (
                  julianday(prior.observed_at) < julianday(current.observed_at)
                  OR (
                      julianday(prior.observed_at) = julianday(current.observed_at)
                      AND julianday(prior.recorded_at) < julianday(current.recorded_at)
                  )
                  OR (
                      julianday(prior.observed_at) = julianday(current.observed_at)
                      AND julianday(prior.recorded_at) = julianday(current.recorded_at)
                      AND prior.occurrence_id < current.occurrence_id
                  )
              )
        )",
        params![prior_occurrence_id, current_occurrence_id],
        |row| row.get(0),
    )?;
    Ok(precedes != 0)
}

fn resulting_work_transition(action: ErrorDispositionAction) -> &'static str {
    match action {
        ErrorDispositionAction::Retry => "retry-authorized",
        ErrorDispositionAction::Workaround => "workaround-selected",
        ErrorDispositionAction::Defer => "work-deferred",
        ErrorDispositionAction::Accept => "risk-accepted",
        ErrorDispositionAction::Escalate => "escalation-required",
        ErrorDispositionAction::Cancel => "operation-canceled",
        ErrorDispositionAction::Resolve => "error-resolved",
    }
}

#[derive(Debug, Default, Deserialize)]
struct ErrorDispositionMetadata {
    decision_id: Option<i64>,
    retry_basis: Option<RetryBasis>,
    prior_disposition_id: Option<i64>,
    #[serde(default)]
    evidence_relation_ids: Vec<i64>,
    #[serde(default)]
    resulting_work_transition: String,
}

fn get_error_disposition(
    connection: &Connection,
    disposition_id: i64,
) -> anyhow::Result<ErrorDisposition> {
    let mut disposition = connection
        .query_row(
            "SELECT id, error_id, occurrence_id, disposition, actor, source, rationale, created_at
             FROM error_disposition WHERE id=?1",
            params![disposition_id],
            |row| {
                Ok(ErrorDisposition {
                    id: row.get(0)?,
                    error_id: row.get(1)?,
                    occurrence_id: row.get(2)?,
                    action: parse_sql_enum(row, "disposition")?,
                    actor: row.get(4)?,
                    source: row.get(5)?,
                    rationale: row.get(6)?,
                    decision_id: None,
                    retry_basis: None,
                    prior_disposition_id: None,
                    evidence_relation_ids: Vec::new(),
                    resulting_work_transition: String::new(),
                    created_at: row.get(7)?,
                })
            },
        )
        .optional()?
        .with_context(|| format!("error disposition {disposition_id} not found"))?;
    let metadata = disposition_metadata(connection, disposition.error_id, disposition.id)?;
    disposition.decision_id = metadata.decision_id;
    disposition.retry_basis = metadata.retry_basis;
    disposition.prior_disposition_id = metadata.prior_disposition_id;
    disposition.evidence_relation_ids = metadata.evidence_relation_ids;
    disposition.resulting_work_transition = metadata.resulting_work_transition;
    Ok(disposition)
}

fn disposition_metadata(
    connection: &Connection,
    error_id: i64,
    disposition_id: i64,
) -> anyhow::Result<ErrorDispositionMetadata> {
    let payload = connection
        .query_row(
            "SELECT payload_json FROM event_log
             WHERE entity_type='error' AND entity_id=?1 AND event_type='disposition_recorded'
               AND CAST(json_extract(payload_json, '$.disposition_id') AS INTEGER)=?2
             ORDER BY id DESC LIMIT 1",
            params![error_id, disposition_id],
            |row| row.get::<_, String>(0),
        )
        .optional()?;
    payload
        .map(|payload| serde_json::from_str(&payload).context("invalid disposition event payload"))
        .transpose()
        .map(|metadata| metadata.unwrap_or_default())
}

fn list_error_dispositions(
    connection: &Connection,
    error_id: i64,
) -> anyhow::Result<Vec<ErrorDisposition>> {
    get_error(connection, error_id)?;
    let mut statement =
        connection.prepare("SELECT id FROM error_disposition WHERE error_id=?1 ORDER BY id")?;
    let ids = statement
        .query_map(params![error_id], |row| row.get::<_, i64>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    ids.into_iter()
        .map(|id| get_error_disposition(connection, id))
        .collect()
}

pub fn check_error_retry_authorization(
    connection: &Connection,
    error_id: i64,
) -> anyhow::Result<ErrorRetryAuthorization> {
    let error = get_error(connection, error_id)?;
    let occurrence = get_error_occurrence(connection, &error.latest_occurrence_id)?;
    let disposition = list_error_dispositions(connection, error_id)?
        .into_iter()
        .rev()
        .find(|disposition| disposition.occurrence_id == occurrence.occurrence_id)
        .with_context(|| {
            format!(
                "retry blocked: occurrence {} has no disposition; inspect `ldgr error context {error_id}` and record one",
                occurrence.occurrence_id
            )
        })?;
    anyhow::ensure!(
        disposition.action == ErrorDispositionAction::Retry,
        "retry blocked: latest disposition {} chose {}; record a changed retry decision instead of silently overriding it",
        disposition.id,
        disposition.action
    );
    if occurrence_has_predecessor(connection, &occurrence)? {
        anyhow::ensure!(
            disposition.retry_basis.is_some() && disposition.prior_disposition_id.is_some(),
            "retry blocked: repeated occurrence requires a retry basis and prior disposition reference"
        );
    }
    let context = error_context_packet(
        connection,
        error_id,
        Some(&occurrence.occurrence_id),
        ErrorContextBounds::default(),
    )?;
    Ok(ErrorRetryAuthorization {
        error,
        occurrence,
        disposition,
        context,
    })
}

pub fn transition_error(
    connection: &Connection,
    error_id: i64,
    new_state: ErrorState,
    actor: &str,
    source: &str,
    rationale: &str,
) -> anyhow::Result<ErrorRecord> {
    for (name, value) in [
        ("actor", actor),
        ("source", source),
        ("rationale", rationale),
    ] {
        anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    }
    in_write_transaction(connection, |connection| {
        let error = get_error(connection, error_id)?;
        let valid = matches!(
            (error.state, new_state),
            (ErrorState::Open, ErrorState::Acknowledged)
                | (ErrorState::Open, ErrorState::Resolved)
                | (ErrorState::Open, ErrorState::Accepted)
                | (ErrorState::Acknowledged, ErrorState::Resolved)
                | (ErrorState::Acknowledged, ErrorState::Accepted)
                | (ErrorState::Resolved, ErrorState::Accepted)
        );
        anyhow::ensure!(
            valid,
            "invalid error lifecycle transition {} -> {}",
            error.state,
            new_state
        );
        append_transition(
            connection,
            error_id,
            Some(&error.latest_occurrence_id),
            error.state,
            new_state,
            actor,
            source,
            rationale,
        )?;
        connection.execute(
            "UPDATE error_record SET state=?1, updated_at=datetime('now') WHERE id=?2",
            params![new_state.as_str(), error_id],
        )?;
        if matches!(new_state, ErrorState::Resolved | ErrorState::Accepted) {
            let action = if new_state == ErrorState::Resolved {
                ErrorDispositionAction::Resolve
            } else {
                ErrorDispositionAction::Accept
            };
            connection.execute(
                "INSERT INTO error_disposition (
                    error_id, occurrence_id, disposition, actor, source, rationale
                 ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    error_id,
                    error.latest_occurrence_id,
                    action.as_str(),
                    actor,
                    source,
                    rationale
                ],
            )?;
            let disposition_id = connection.last_insert_rowid();
            connection.execute(
                "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
                 VALUES ('error', ?1, 'disposition_recorded', ?2)",
                params![
                    error_id,
                    serde_json::to_string(&serde_json::json!({
                        "disposition_id": disposition_id,
                        "occurrence_id": error.latest_occurrence_id,
                        "action": action,
                        "actor": actor,
                        "source": source,
                        "rationale": rationale,
                        "decision_id": null,
                        "retry_basis": null,
                        "prior_disposition_id": null,
                        "evidence_relation_ids": [],
                        "resulting_work_transition": resulting_work_transition(action),
                    }))?
                ],
            )?;
            connection.execute(
                "UPDATE error_record SET disposition_pending=0 WHERE id=?1",
                params![error_id],
            )?;
        }
        connection.execute(
            "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
             VALUES ('error', ?1, 'state_changed', ?2)",
            params![
                error_id,
                serde_json::to_string(&serde_json::json!({
                    "old_state": error.state,
                    "new_state": new_state,
                    "actor": actor,
                    "source": source,
                    "rationale": rationale,
                }))?
            ],
        )?;
        get_error(connection, error_id)
    })
}

#[allow(clippy::too_many_arguments)]
fn append_transition(
    connection: &Connection,
    error_id: i64,
    occurrence_id: Option<&str>,
    old_state: ErrorState,
    new_state: ErrorState,
    actor: &str,
    source: &str,
    rationale: &str,
) -> anyhow::Result<()> {
    connection.execute(
        "INSERT INTO error_transition (
            error_id, occurrence_id, old_state, new_state, actor, source, rationale
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            error_id,
            occurrence_id,
            old_state.as_str(),
            new_state.as_str(),
            actor,
            source,
            rationale
        ],
    )?;
    Ok(())
}

pub fn link_error(
    connection: &Connection,
    error_id: i64,
    occurrence_id: Option<&str>,
    relation_kind: &str,
    entity_type: &str,
    entity_id: &str,
    source: &str,
) -> anyhow::Result<ErrorRelation> {
    for (name, value) in [
        ("relation_kind", relation_kind),
        ("entity_type", entity_type),
        ("entity_id", entity_id),
        ("source", source),
    ] {
        anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    }
    in_write_transaction(connection, |connection| {
        get_error(connection, error_id)?;
        if let Some(occurrence_id) = occurrence_id {
            let occurrence = get_error_occurrence(connection, occurrence_id)?;
            anyhow::ensure!(
                occurrence.error_id == error_id,
                "occurrence {occurrence_id} does not belong to error {error_id}"
            );
        }
        validate_relation_target(connection, entity_type, entity_id)?;
        connection.execute(
            "INSERT INTO error_relation (
                error_id, occurrence_id, relation_kind, entity_type, entity_id, source
             ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                error_id,
                occurrence_id,
                relation_kind,
                entity_type,
                entity_id,
                source
            ],
        )?;
        let id = connection.last_insert_rowid();
        connection.execute(
            "INSERT INTO event_log (entity_type, entity_id, event_type, payload_json)
             VALUES ('error', ?1, 'relation_added', ?2)",
            params![
                error_id,
                serde_json::to_string(&serde_json::json!({
                    "relation_id": id,
                    "occurrence_id": occurrence_id,
                    "relation_kind": relation_kind,
                    "entity_type": entity_type,
                    "entity_id": entity_id,
                    "source": source,
                }))?
            ],
        )?;
        connection
            .query_row(
                "SELECT id, error_id, occurrence_id, relation_kind, entity_type, entity_id, source, created_at
                 FROM error_relation WHERE id=?1",
                params![id],
                relation_from_row,
            )
            .context("failed to read created error relation")
    })
}

fn validate_relation_target(
    connection: &Connection,
    entity_type: &str,
    entity_id: &str,
) -> anyhow::Result<()> {
    let (table, label) = match entity_type {
        "work_item" => ("work_item", "work item"),
        "run" => ("run", "run"),
        "artifact" => ("artifact", "artifact"),
        "decision" => ("decision", "decision"),
        "event" => ("event_log", "event"),
        "validation" => {
            let parsed_id = entity_id
                .parse::<i64>()
                .context("validation relation ID must be an integer")?;
            let exists: i64 = connection.query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM event_log
                    WHERE id=?1 AND entity_type='run' AND event_type='validation'
                 )",
                params![parsed_id],
                |row| row.get(0),
            )?;
            anyhow::ensure!(exists != 0, "validation {entity_id} not found");
            return Ok(());
        }
        "external" => {
            anyhow::ensure!(
                entity_id.split_once(':').is_some_and(|(namespace, id)| !namespace.is_empty() && !id.is_empty()),
                "external entity IDs must be namespace-qualified as `<namespace>:<id>`"
            );
            return Ok(());
        }
        _ => bail!(
            "unsupported relation entity type `{entity_type}`; expected work_item, run, artifact, validation, decision, event, or external"
        ),
    };
    let parsed_id = entity_id
        .parse::<i64>()
        .with_context(|| format!("{label} relation ID must be an integer"))?;
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE id=?1)");
    let exists: i64 = connection.query_row(&sql, params![parsed_id], |row| row.get(0))?;
    anyhow::ensure!(exists != 0, "{label} {entity_id} not found");
    Ok(())
}

fn list_error_relations(
    connection: &Connection,
    error_id: i64,
) -> anyhow::Result<Vec<ErrorRelation>> {
    let mut statement = connection.prepare(
        "SELECT id, error_id, occurrence_id, relation_kind, entity_type, entity_id, source, created_at
         FROM error_relation WHERE error_id=?1 ORDER BY id",
    )?;
    let result = statement
        .query_map(params![error_id], relation_from_row)?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list error relations");
    result
}

fn relation_from_row(row: &Row<'_>) -> rusqlite::Result<ErrorRelation> {
    Ok(ErrorRelation {
        id: row.get("id")?,
        error_id: row.get("error_id")?,
        occurrence_id: row.get("occurrence_id")?,
        relation_kind: row.get("relation_kind")?,
        entity_type: row.get("entity_type")?,
        entity_id: row.get("entity_id")?,
        source: row.get("source")?,
        created_at: row.get("created_at")?,
    })
}

fn list_error_transitions(
    connection: &Connection,
    error_id: i64,
) -> anyhow::Result<Vec<ErrorTransition>> {
    let mut statement = connection.prepare(
        "SELECT id, error_id, occurrence_id, old_state, new_state, actor, source, rationale, created_at
         FROM error_transition WHERE error_id=?1 ORDER BY id",
    )?;
    let result = statement
        .query_map(params![error_id], |row| {
            Ok(ErrorTransition {
                id: row.get("id")?,
                error_id: row.get("error_id")?,
                occurrence_id: row.get("occurrence_id")?,
                old_state: parse_sql_enum(row, "old_state")?,
                new_state: parse_sql_enum(row, "new_state")?,
                actor: row.get("actor")?,
                source: row.get("source")?,
                rationale: row.get("rationale")?,
                created_at: row.get("created_at")?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .context("failed to list error transitions");
    result
}

fn rebuild_error_projection(connection: &Connection, error_id: i64) -> anyhow::Result<()> {
    let state = get_error(connection, error_id)?.state;
    let resulting_state = if state == ErrorState::Resolved {
        ErrorState::Open
    } else {
        state
    };
    connection.execute(
        "UPDATE error_record
         SET first_seen_at=(
             SELECT observed_at FROM error_occurrence WHERE error_id=?1
                 ORDER BY julianday(observed_at), julianday(recorded_at), occurrence_id LIMIT 1
             ),
             last_seen_at=(
             SELECT observed_at FROM error_occurrence WHERE error_id=?1
                 ORDER BY julianday(observed_at) DESC, julianday(recorded_at) DESC, occurrence_id DESC LIMIT 1
             ),
             occurrence_count=(SELECT COUNT(*) FROM error_occurrence WHERE error_id=?1),
             latest_occurrence_id=(
             SELECT occurrence_id FROM error_occurrence WHERE error_id=?1
                 ORDER BY julianday(observed_at) DESC, julianday(recorded_at) DESC, occurrence_id DESC LIMIT 1
             ),
             state=?2,
             disposition_pending=1,
             updated_at=datetime('now')
         WHERE id=?1",
        params![error_id, resulting_state.as_str()],
    )?;
    Ok(())
}

fn project_id(connection: &Connection) -> anyhow::Result<String> {
    connection
        .query_row(
            "SELECT project_id FROM project_identity WHERE id=1",
            [],
            |row| row.get(0),
        )
        .context("database is missing its project identity")
}

pub(crate) fn seed_project_identity(connection: &Connection) -> anyhow::Result<()> {
    let exists: i64 = connection.query_row(
        "SELECT EXISTS(SELECT 1 FROM project_identity WHERE id=1)",
        [],
        |row| row.get(0),
    )?;
    if exists != 0 {
        return Ok(());
    }
    let mut bytes = [0_u8; 16];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow::anyhow!("failed to generate project identity: {error}"))?;
    let id = bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    connection.execute(
        "INSERT INTO project_identity (id, project_id) VALUES (1, ?1)",
        params![format!("project-{id}")],
    )?;
    Ok(())
}

fn sha256_digest(value: &[u8]) -> String {
    format!("sha256:{:x}", Sha256::digest(value))
}

pub(crate) fn is_sha256_digest(value: &str) -> bool {
    value.strip_prefix("sha256:").is_some_and(|digest| {
        digest.len() == 64
            && digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn digest(character: char) -> String {
        format!("sha256:{}", character.to_string().repeat(64))
    }

    fn input<'a>(
        occurrence_id: &'a str,
        idempotency_key: &'a str,
        fingerprint: &'a str,
        details: &'a serde_json::Value,
        environment: &'a serde_json::Value,
    ) -> RecordErrorInput<'a> {
        RecordErrorInput {
            occurrence_id,
            producer: "core-test",
            idempotency_key,
            operation_id: "operation-1",
            attempt_id: occurrence_id,
            fingerprint_version: "structured-v1",
            fingerprint,
            fingerprint_inputs: None,
            fingerprint_provenance: None,
            class: ErrorClass::InfrastructureError,
            domain: "core.store",
            code: "write-failed",
            severity: ErrorSeverity::Error,
            retryability: ErrorRetryability::Transient,
            source: "core-test:store",
            summary: "write failed",
            details,
            environment,
            observed_at: "2026-07-31T00:00:00Z",
            recovery_origin: RecoveryOrigin::Database,
        }
    }

    #[test]
    fn record_replay_recurrence_and_lifecycle_are_durable() -> anyhow::Result<()> {
        let connection = Connection::open_in_memory()?;
        super::super::schema::ensure_schema(&connection)?;
        let details = serde_json::json!({"kind": "locked"});
        let environment = serde_json::json!({"os": "test"});
        let fingerprint = digest('a');
        let first = record_error(
            &connection,
            &input(
                "occurrence-1",
                "key-1",
                &fingerprint,
                &details,
                &environment,
            ),
        )?;
        assert!(!first.idempotent_replay);
        let replay = record_error(
            &connection,
            &input(
                "occurrence-1",
                "key-1",
                &fingerprint,
                &details,
                &environment,
            ),
        )?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.error.occurrence_count, 1);

        let second = record_error(
            &connection,
            &input(
                "occurrence-2",
                "key-2",
                &fingerprint,
                &details,
                &environment,
            ),
        )?;
        assert!(second.recurrent);
        assert_eq!(second.error.occurrence_count, 2);
        transition_error(
            &connection,
            second.error.id,
            ErrorState::Acknowledged,
            "tester",
            "test",
            "investigating",
        )?;
        let resolved = transition_error(
            &connection,
            second.error.id,
            ErrorState::Resolved,
            "tester",
            "test",
            "fixed",
        )?;
        assert_eq!(resolved.state, ErrorState::Resolved);
        assert!(!resolved.disposition_pending);

        let third = record_error(
            &connection,
            &input(
                "occurrence-3",
                "key-3",
                &fingerprint,
                &details,
                &environment,
            ),
        )?;
        assert_eq!(third.error.state, ErrorState::Open);
        assert!(third.error.disposition_pending);
        assert_eq!(
            show_error(&connection, third.error.id)?.transitions.len(),
            3
        );
        assert!(connection
            .execute(
                "UPDATE error_occurrence SET summary='mutated' WHERE occurrence_id='occurrence-1'",
                [],
            )
            .is_err());
        assert!(connection
            .execute(
                "DELETE FROM error_occurrence WHERE occurrence_id='occurrence-1'",
                [],
            )
            .is_err());
        let relation = link_error(
            &connection,
            third.error.id,
            Some("occurrence-3"),
            "reported-by",
            "external",
            "test-suite:case-1",
            "test",
        )?;
        assert_eq!(relation.error_id, third.error.id);
        let relation_events: i64 = connection.query_row(
            "SELECT COUNT(*) FROM event_log
             WHERE entity_type='error' AND entity_id=?1 AND event_type='relation_added'",
            params![third.error.id],
            |row| row.get(0),
        )?;
        assert_eq!(relation_events, 1);
        Ok(())
    }

    #[test]
    fn conflicting_idempotency_key_is_rejected_without_mutation() -> anyhow::Result<()> {
        let connection = Connection::open_in_memory()?;
        super::super::schema::ensure_schema(&connection)?;
        let details = serde_json::json!({});
        let environment = serde_json::json!({});
        let fingerprint = digest('b');
        record_error(
            &connection,
            &input("occurrence-1", "same", &fingerprint, &details, &environment),
        )?;
        let mut conflicting = input("occurrence-2", "same", &fingerprint, &details, &environment);
        conflicting.summary = "different";
        assert!(record_error(&connection, &conflicting)
            .unwrap_err()
            .to_string()
            .contains("idempotency conflict"));
        assert_eq!(list_error_occurrences(&connection, None, 10)?.len(), 1);
        Ok(())
    }

    #[test]
    fn concurrent_writers_preserve_every_occurrence_and_converge_replays() -> anyhow::Result<()> {
        let temp = tempfile::TempDir::new()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        super::super::helpers::init_store(&db_path, &temp.path().join("artifacts"))?;
        let fingerprint = digest('c');
        let barrier = std::sync::Arc::new(std::sync::Barrier::new(8));
        let mut writers = Vec::new();
        for index in 0..8 {
            let db_path = db_path.clone();
            let barrier = barrier.clone();
            let fingerprint = fingerprint.clone();
            writers.push(std::thread::spawn(move || -> anyhow::Result<()> {
                let connection = super::super::helpers::open_store(&db_path)?;
                let details = serde_json::json!({"writer": index});
                let environment = serde_json::json!({});
                let occurrence_id = format!("occurrence-{index}");
                let key = format!("writer-{index}");
                barrier.wait();
                record_error(
                    &connection,
                    &input(&occurrence_id, &key, &fingerprint, &details, &environment),
                )?;
                Ok(())
            }));
        }
        for writer in writers {
            writer.join().expect("error writer panicked")?;
        }
        let connection = super::super::helpers::open_store(&db_path)?;
        let errors = list_errors(&connection, None, 10)?;
        assert_eq!(errors.len(), 1);
        assert_eq!(errors[0].occurrence_count, 8);
        assert_eq!(list_error_occurrences(&connection, None, 20)?.len(), 8);

        let details = serde_json::json!({"writer": 0});
        let environment = serde_json::json!({});
        let replay = record_error(
            &connection,
            &input(
                "occurrence-0",
                "writer-0",
                &fingerprint,
                &details,
                &environment,
            ),
        )?;
        assert!(replay.idempotent_replay);
        assert_eq!(replay.error.occurrence_count, 8);
        Ok(())
    }

    #[test]
    fn every_supported_lifecycle_transition_is_audited() -> anyhow::Result<()> {
        let connection = Connection::open_in_memory()?;
        super::super::schema::ensure_schema(&connection)?;
        let details = serde_json::json!({});
        let environment = serde_json::json!({});
        let paths = [
            vec![ErrorState::Acknowledged],
            vec![ErrorState::Resolved],
            vec![ErrorState::Accepted],
            vec![ErrorState::Acknowledged, ErrorState::Resolved],
            vec![ErrorState::Acknowledged, ErrorState::Accepted],
            vec![ErrorState::Resolved, ErrorState::Accepted],
        ];
        for (index, path) in paths.iter().enumerate() {
            let occurrence = format!("lifecycle-{index}");
            let key = format!("lifecycle-key-{index}");
            let fingerprint = digest(char::from_digit(index as u32, 16).unwrap());
            let recorded = record_error(
                &connection,
                &input(&occurrence, &key, &fingerprint, &details, &environment),
            )?;
            for state in path {
                transition_error(
                    &connection,
                    recorded.error.id,
                    *state,
                    "tester",
                    "test",
                    "lifecycle coverage",
                )?;
            }
            let view = show_error(&connection, recorded.error.id)?;
            assert_eq!(view.error.state, *path.last().unwrap());
            assert_eq!(view.transitions.len(), path.len());
        }
        let first = list_errors(&connection, None, 20)?[0].id;
        assert!(transition_error(
            &connection,
            first,
            ErrorState::Acknowledged,
            "tester",
            "test",
            "invalid transition",
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn every_disposition_action_is_rationale_backed_and_audited() -> anyhow::Result<()> {
        let connection = Connection::open_in_memory()?;
        super::super::schema::ensure_schema(&connection)?;
        let details = serde_json::json!({});
        let environment = serde_json::json!({});
        let actions = [
            ErrorDispositionAction::Retry,
            ErrorDispositionAction::Workaround,
            ErrorDispositionAction::Defer,
            ErrorDispositionAction::Accept,
            ErrorDispositionAction::Escalate,
            ErrorDispositionAction::Cancel,
            ErrorDispositionAction::Resolve,
        ];
        for (index, action) in actions.into_iter().enumerate() {
            let occurrence = format!("disposition-{index}");
            let key = format!("disposition-key-{index}");
            let fingerprint = digest(char::from_digit(index as u32, 16).unwrap());
            let recorded = record_error(
                &connection,
                &input(&occurrence, &key, &fingerprint, &details, &environment),
            )?;
            let disposition = record_error_disposition(
                &connection,
                &RecordErrorDispositionInput {
                    error_id: recorded.error.id,
                    occurrence_id: None,
                    action,
                    actor: "tester",
                    source: "test",
                    rationale: "explicit test disposition",
                    decision_id: None,
                    retry_basis: None,
                    prior_disposition_id: None,
                    evidence_relation_ids: &[],
                },
            )?;
            assert_eq!(disposition.action, action);
            assert!(!get_error(&connection, recorded.error.id)?.disposition_pending);
        }
        let audited: i64 = connection.query_row(
            "SELECT COUNT(*) FROM event_log WHERE event_type='disposition_recorded'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(audited, actions.len() as i64);
        Ok(())
    }
}
