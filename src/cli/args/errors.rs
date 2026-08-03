use clap::{Args, Subcommand, ValueEnum};

use crate::store::{
    ErrorClass, ErrorDispositionAction, ErrorRetryability, ErrorSeverity, ErrorState,
    RecoveryOrigin, RetryBasis,
};

const ERROR_HELP: &str = "Examples:
  ldgr error record --occurrence-id 0198... --producer agentctl --idempotency-key 0198...:pre-spawn --operation-id 0198... --attempt-id 0198... --class infrastructure-error --domain agentctl.bootstrap --code home-unavailable --boundary config-discovery --component agentctl --subject ldgr-config --severity error --retryability after-change --source agentctl:pre-spawn --summary \"Home unavailable\" --observed-at 2026-07-31T00:00:00Z
  ldgr error list --state open --json
  ldgr error show 7
  ldgr error context 7 --limit 5 --json
  ldgr error occurrence list --error-id 7
  ldgr error disposition 7 --action retry --actor operator --source cli --rationale \"Retry after config change\"
  ldgr error acknowledge 7 --actor operator --source cli --rationale \"Investigating\"
  ldgr error resolve 7 --actor operator --source cli --rationale \"Verified fixed\"
  ldgr error link 7 --kind affected --entity-type run --entity-id 495 --source cli

Occurrences are immutable. Reusing an occurrence ID or producer idempotency key is safe only when the complete canonical payload is identical.";

#[derive(Debug, Args)]
#[command(after_help = ERROR_HELP)]
pub struct ErrorArgs {
    #[command(subcommand)]
    pub command: ErrorCommand,
}

#[derive(Debug, Subcommand)]
pub enum ErrorCommand {
    /// Record one immutable occurrence and create or update its aggregate.
    Record(Box<RecordErrorArgs>),
    /// List error aggregates.
    List(ListErrorArgs),
    /// Show an aggregate with occurrences, relations, and lifecycle history.
    Show(ShowErrorArgs),
    /// Assemble bounded redacted evidence for a repeated occurrence.
    Context(ErrorContextArgs),
    /// List or show immutable occurrences.
    Occurrence(ErrorOccurrenceArgs),
    /// Record a rationale-backed disposition for an occurrence.
    Disposition(ErrorDispositionArgs),
    /// Verify that the latest occurrence has an explicit audited retry authorization.
    RetryCheck(ErrorRetryCheckArgs),
    /// Mark an open aggregate as acknowledged.
    Acknowledge(ErrorLifecycleArgs),
    /// Resolve an open or acknowledged aggregate with rationale.
    Resolve(ErrorLifecycleArgs),
    /// Explicitly accept an aggregate's remaining impact.
    Accept(ErrorLifecycleArgs),
    /// Add an audited relation to a Core or namespace-qualified external entity.
    Link(LinkErrorArgs),
}

#[derive(Debug, Args)]
pub struct RecordErrorArgs {
    #[arg(long)]
    pub occurrence_id: String,
    #[arg(long)]
    pub producer: String,
    #[arg(long)]
    pub idempotency_key: String,
    #[arg(long)]
    pub operation_id: String,
    #[arg(long)]
    pub attempt_id: String,
    #[arg(long, default_value = "structured-v1")]
    pub fingerprint_version: String,
    #[arg(long)]
    pub fingerprint: Option<String>,
    /// Stable boundary dimension included in automatic structured fingerprinting.
    #[arg(long)]
    pub boundary: Option<String>,
    /// Stable component dimension included in automatic structured fingerprinting.
    #[arg(long)]
    pub component: Option<String>,
    /// Stable subject dimension included in automatic structured fingerprinting.
    #[arg(long)]
    pub subject: Option<String>,
    /// Rationale required when overriding Core's computed fingerprint.
    #[arg(long, requires = "fingerprint")]
    pub fingerprint_override_rationale: Option<String>,
    /// Stable explicit split key for separating a known collision or causal branch.
    #[arg(long, conflicts_with = "fingerprint")]
    pub fingerprint_split: Option<String>,
    /// Rationale required for an explicit fingerprint split.
    #[arg(long, requires = "fingerprint_split")]
    pub fingerprint_split_rationale: Option<String>,
    #[arg(long, value_enum, ignore_case = true)]
    pub class: CliErrorClass,
    #[arg(long)]
    pub domain: String,
    #[arg(long)]
    pub code: String,
    #[arg(long, value_enum, ignore_case = true)]
    pub severity: CliErrorSeverity,
    #[arg(long, value_enum, ignore_case = true)]
    pub retryability: CliErrorRetryability,
    #[arg(long)]
    pub source: String,
    #[arg(long)]
    pub summary: String,
    #[arg(long, default_value = "{}")]
    pub details: String,
    #[arg(long, default_value = "{}")]
    pub environment: String,
    #[arg(long)]
    pub observed_at: String,
    #[arg(long, value_enum, default_value = "database", ignore_case = true)]
    pub recovery_origin: CliRecoveryOrigin,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ListErrorArgs {
    #[arg(long, value_enum, ignore_case = true)]
    pub state: Option<CliErrorState>,
    #[arg(long, default_value_t = 20)]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowErrorArgs {
    pub error_id: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ErrorContextArgs {
    pub error_id: i64,
    /// Anchor the packet at a specific occurrence; defaults to the aggregate's latest.
    #[arg(long)]
    pub occurrence_id: Option<String>,
    /// Maximum entries included in each context section.
    #[arg(long, default_value_t = 5)]
    pub limit: usize,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ErrorOccurrenceArgs {
    #[command(subcommand)]
    pub command: ErrorOccurrenceCommand,
}

#[derive(Debug, Subcommand)]
pub enum ErrorOccurrenceCommand {
    /// List immutable occurrences, optionally for one aggregate.
    List(ListErrorOccurrenceArgs),
    /// Show one immutable occurrence by its caller-provided identity.
    Show(ShowErrorOccurrenceArgs),
}

#[derive(Debug, Args)]
pub struct ListErrorOccurrenceArgs {
    #[arg(long)]
    pub error_id: Option<i64>,
    #[arg(long, default_value_t = 20)]
    pub limit: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowErrorOccurrenceArgs {
    pub occurrence_id: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ErrorLifecycleArgs {
    pub error_id: i64,
    #[arg(long)]
    pub actor: String,
    #[arg(long)]
    pub source: String,
    #[arg(long)]
    pub rationale: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ErrorDispositionArgs {
    pub error_id: i64,
    /// Occurrence receiving the disposition; defaults to the aggregate's latest occurrence.
    #[arg(long)]
    pub occurrence_id: Option<String>,
    #[arg(long, value_enum, ignore_case = true)]
    pub action: CliErrorDispositionAction,
    #[arg(long)]
    pub actor: String,
    #[arg(long)]
    pub source: String,
    #[arg(long)]
    pub rationale: String,
    /// Existing causal decision supporting this disposition.
    #[arg(long)]
    pub decision_id: Option<i64>,
    /// Required for retrying a repeated error.
    #[arg(long, value_enum, ignore_case = true)]
    pub retry_basis: Option<CliRetryBasis>,
    /// Prior disposition explicitly considered before retrying a repeated error.
    #[arg(long)]
    pub prior_disposition_id: Option<i64>,
    /// Existing error relation IDs that provide new evidence.
    #[arg(long = "evidence-relation-id")]
    pub evidence_relation_ids: Vec<i64>,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ErrorRetryCheckArgs {
    pub error_id: i64,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct LinkErrorArgs {
    pub error_id: i64,
    #[arg(long)]
    pub occurrence_id: Option<String>,
    #[arg(long = "kind")]
    pub relation_kind: String,
    #[arg(long)]
    pub entity_type: String,
    #[arg(long)]
    pub entity_id: String,
    #[arg(long)]
    pub source: String,
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliErrorClass {
    TaskFailure,
    ValidationFailure,
    InfrastructureError,
    Interruption,
    OperatorCancellation,
}

impl From<CliErrorClass> for ErrorClass {
    fn from(value: CliErrorClass) -> Self {
        match value {
            CliErrorClass::TaskFailure => Self::TaskFailure,
            CliErrorClass::ValidationFailure => Self::ValidationFailure,
            CliErrorClass::InfrastructureError => Self::InfrastructureError,
            CliErrorClass::Interruption => Self::Interruption,
            CliErrorClass::OperatorCancellation => Self::OperatorCancellation,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliErrorSeverity {
    Info,
    Warning,
    Error,
    Critical,
}
impl From<CliErrorSeverity> for ErrorSeverity {
    fn from(value: CliErrorSeverity) -> Self {
        match value {
            CliErrorSeverity::Info => Self::Info,
            CliErrorSeverity::Warning => Self::Warning,
            CliErrorSeverity::Error => Self::Error,
            CliErrorSeverity::Critical => Self::Critical,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliErrorRetryability {
    Never,
    AfterChange,
    Transient,
    Unknown,
}
impl From<CliErrorRetryability> for ErrorRetryability {
    fn from(value: CliErrorRetryability) -> Self {
        match value {
            CliErrorRetryability::Never => Self::Never,
            CliErrorRetryability::AfterChange => Self::AfterChange,
            CliErrorRetryability::Transient => Self::Transient,
            CliErrorRetryability::Unknown => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliErrorState {
    Open,
    Acknowledged,
    Resolved,
    Accepted,
}
impl From<CliErrorState> for ErrorState {
    fn from(value: CliErrorState) -> Self {
        match value {
            CliErrorState::Open => Self::Open,
            CliErrorState::Acknowledged => Self::Acknowledged,
            CliErrorState::Resolved => Self::Resolved,
            CliErrorState::Accepted => Self::Accepted,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliRecoveryOrigin {
    Database,
    ProjectInbox,
    UserSpool,
}
impl From<CliRecoveryOrigin> for RecoveryOrigin {
    fn from(value: CliRecoveryOrigin) -> Self {
        match value {
            CliRecoveryOrigin::Database => Self::Database,
            CliRecoveryOrigin::ProjectInbox => Self::ProjectInbox,
            CliRecoveryOrigin::UserSpool => Self::UserSpool,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliErrorDispositionAction {
    Retry,
    Workaround,
    Defer,
    Accept,
    Escalate,
    Cancel,
    Resolve,
}
impl From<CliErrorDispositionAction> for ErrorDispositionAction {
    fn from(value: CliErrorDispositionAction) -> Self {
        match value {
            CliErrorDispositionAction::Retry => Self::Retry,
            CliErrorDispositionAction::Workaround => Self::Workaround,
            CliErrorDispositionAction::Defer => Self::Defer,
            CliErrorDispositionAction::Accept => Self::Accept,
            CliErrorDispositionAction::Escalate => Self::Escalate,
            CliErrorDispositionAction::Cancel => Self::Cancel,
            CliErrorDispositionAction::Resolve => Self::Resolve,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliRetryBasis {
    NewEvidence,
    ChangedCondition,
    ChangedDecision,
    ExplicitConfirmation,
}
impl From<CliRetryBasis> for RetryBasis {
    fn from(value: CliRetryBasis) -> Self {
        match value {
            CliRetryBasis::NewEvidence => Self::NewEvidence,
            CliRetryBasis::ChangedCondition => Self::ChangedCondition,
            CliRetryBasis::ChangedDecision => Self::ChangedDecision,
            CliRetryBasis::ExplicitConfirmation => Self::ExplicitConfirmation,
        }
    }
}
