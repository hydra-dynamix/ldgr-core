use super::*;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct WorkItem {
    pub id: i64,
    pub parent_work_item_id: Option<i64>,
    pub slug: String,
    pub title: String,
    pub description: String,
    pub status: WorkItemStatus,
    pub created_at: String,
    pub updated_at: String,
}

impl WorkItem {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let status_text: String = row.get("status")?;
        let status = WorkItemStatus::from_str(&status_text).map_err(parse_error_to_sql_error)?;
        Ok(Self {
            id: row.get("id")?,
            parent_work_item_id: row.get("parent_work_item_id")?,
            slug: row.get("slug")?,
            title: row.get("title")?,
            description: row.get("description")?,
            status,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct InvestigationRun {
    pub id: i64,
    pub work_item_id: i64,
    pub command: Option<String>,
    pub status: RunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub notes: Option<String>,
}

impl InvestigationRun {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let status_text: String = row.get("status")?;
        let status = RunStatus::from_str(&status_text).map_err(parse_error_to_sql_error)?;
        Ok(Self {
            id: row.get("id")?,
            work_item_id: row.get("work_item_id")?,
            command: row.get("command")?,
            status,
            started_at: row.get("started_at")?,
            finished_at: row.get("finished_at")?,
            notes: row.get("notes")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Observation {
    pub id: i64,
    pub run_id: i64,
    pub body: String,
    pub created_at: String,
}

impl Observation {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            run_id: row.get("run_id")?,
            body: row.get("body")?,
            created_at: row.get("created_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Prompt {
    pub id: i64,
    pub slug: String,
    pub role: String,
    pub body: String,
    pub content_hash: String,
    pub status: String,
    pub current_version: i64,
    pub current_version_id: Option<i64>,
    pub source_path: Option<String>,
    pub description: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl Prompt {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            id: row.get("id")?,
            slug: row.get("slug")?,
            role: row.get("role")?,
            body: row.get("body")?,
            content_hash: row.get("content_hash")?,
            status: row.get("status")?,
            current_version: row.get("current_version")?,
            current_version_id: row.get("current_version_id")?,
            source_path: row.get("source_path")?,
            description: row.get("description")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Artifact {
    pub id: i64,
    pub run_id: i64,
    pub kind: ArtifactKind,
    pub path: PathBuf,
    pub description: String,
    pub created_at: String,
}

impl Artifact {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let kind_text: String = row.get("kind")?;
        let kind = kind_text.parse().map_err(parse_error_to_sql_error)?;
        let path_text: String = row.get("path")?;
        Ok(Self {
            id: row.get("id")?,
            run_id: row.get("run_id")?,
            kind,
            path: PathBuf::from(path_text),
            description: row.get("description")?,
            created_at: row.get("created_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationRecord {
    pub id: i64,
    pub run_id: i64,
    pub outcome: ValidationOutcome,
    pub command: Option<String>,
    pub rationale: Option<String>,
    pub created_at: String,
}

impl ValidationRecord {
    pub(crate) fn from_event_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let payload_json: String = row.get("payload_json")?;
        let payload = serde_json::from_str::<serde_json::Value>(&payload_json)
            .map_err(|error| validation_payload_error(error.to_string()))?;
        let outcome_text = payload
            .get("outcome")
            .and_then(|value| value.as_str())
            .ok_or_else(|| validation_payload_error("missing validation outcome".to_owned()))?;
        let outcome =
            ValidationOutcome::from_str(outcome_text).map_err(parse_error_to_sql_error)?;
        Ok(Self {
            id: row.get("id")?,
            run_id: row.get("run_id")?,
            outcome,
            command: payload
                .get("command")
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            rationale: payload
                .get("rationale")
                .and_then(|value| value.as_str())
                .map(str::to_owned),
            created_at: row.get("created_at")?,
        })
    }
}

fn validation_payload_error(message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Decision {
    pub id: i64,
    pub work_item_id: i64,
    pub outcome: DecisionOutcome,
    pub rationale: String,
    pub next_work_item_id: Option<i64>,
    pub created_at: String,
}

impl Decision {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let outcome_text: String = row.get("outcome")?;
        let outcome = DecisionOutcome::from_str(&outcome_text).map_err(parse_error_to_sql_error)?;
        Ok(Self {
            id: row.get("id")?,
            work_item_id: row.get("work_item_id")?,
            outcome,
            rationale: row.get("rationale")?,
            next_work_item_id: row.get("next_work_item_id")?,
            created_at: row.get("created_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StoreContext {
    pub pending_work_items: i64,
    pub running_work_items: i64,
    pub held_work_items: i64,
    pub done_work_items: i64,
    pub canceled_work_items: i64,
    pub loop_invariants: crate::loop_invariants::LoopInvariantsSummary,
    pub loop_state: LoopStateSummary,
    pub active_runs: Vec<RunSummary>,
    pub next_work_item: Option<WorkItem>,
    pub latest_decision: Option<DecisionSummary>,
    pub latest_observations: Vec<ObservationSummary>,
    pub latest_validations: Vec<ValidationSummary>,
    #[serde(rename = "binding_directives")]
    pub global_observations: Vec<GlobalObservation>,
    pub latest_artifacts: Vec<ArtifactSummary>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conduct_lifecycle: Option<ConductLifecycleSummary>,
    pub loop_interventions: Vec<LoopIntervention>,
    pub latest_events: Vec<EventLogSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConductLifecycleSummary {
    pub batch_id: String,
    pub status: String,
    pub worker_counts: ConductWorkerCounts,
    pub graph_artifact_id: Option<i64>,
    pub ticket_index_artifact_id: Option<i64>,
    pub batch_state_artifact_id: Option<i64>,
    pub current_wave: Option<String>,
    pub blocked_count: usize,
    pub next_valid_action: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stale_next_work: Option<ConductStaleNextWorkWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConductWorkerCounts {
    pub total: usize,
    pub complete: usize,
    pub active: usize,
    pub blocked: usize,
    pub terminal: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConductStaleNextWorkWarning {
    pub work_slug: String,
    pub message: String,
    pub suggested_commands: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GlobalObservation {
    pub id: i64,
    pub kind: GlobalObservationKind,
    pub body: String,
    pub source: Option<String>,
    pub status: GlobalObservationStatus,
    pub created_at: String,
    pub updated_at: String,
}

impl GlobalObservation {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let kind_text: String = row.get("kind")?;
        let kind = GlobalObservationKind::from_str(&kind_text).map_err(parse_error_to_sql_error)?;
        let status_text: String = row.get("status")?;
        let status =
            GlobalObservationStatus::from_str(&status_text).map_err(parse_error_to_sql_error)?;
        Ok(Self {
            id: row.get("id")?,
            kind,
            body: row.get("body")?,
            source: row.get("source")?,
            status,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunSummary {
    pub run_id: i64,
    pub work_slug: String,
    pub work_title: String,
    pub command: Option<String>,
    pub started_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RunListItem {
    pub run_id: i64,
    pub work_slug: String,
    pub work_title: String,
    pub command: Option<String>,
    pub status: RunStatus,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub notes: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoopStateSummary {
    pub run_id: Option<i64>,
    pub work_slug: Option<String>,
    pub work_title: Option<String>,
    pub current_phase: String,
    pub progress_report: String,
    pub command: Option<String>,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub terminal_status: Option<RunStatus>,
    pub recent_cycle_narrative: Vec<LoopNarrativeEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoopNarrativeEntry {
    pub created_at: String,
    pub phase: Option<String>,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DecisionSummary {
    pub decision_id: i64,
    pub work_slug: String,
    pub outcome: DecisionOutcome,
    pub rationale: String,
    pub next_work_slug: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ObservationSummary {
    pub observation_id: i64,
    pub run_id: i64,
    pub work_slug: String,
    pub body: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ArtifactSummary {
    pub artifact_id: i64,
    pub run_id: i64,
    pub work_slug: String,
    pub kind: ArtifactKind,
    pub path: PathBuf,
    pub description: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ValidationSummary {
    pub validation_id: i64,
    pub run_id: i64,
    pub work_slug: String,
    pub outcome: ValidationOutcome,
    pub command: Option<String>,
    pub rationale: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EventLogSummary {
    pub event_id: i64,
    pub entity_type: String,
    pub entity_id: i64,
    pub event_type: String,
    pub payload_json: String,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoopIntervention {
    pub id: i64,
    pub action: LoopInterventionAction,
    pub reason: String,
    pub instruction: Option<String>,
    pub status: LoopInterventionStatus,
    pub requested_by: Option<String>,
    pub applied_run_id: Option<i64>,
    pub created_at: String,
    pub updated_at: String,
}

impl LoopIntervention {
    pub(crate) fn from_row(row: &Row<'_>) -> rusqlite::Result<Self> {
        let action_text: String = row.get("action")?;
        let action =
            LoopInterventionAction::from_str(&action_text).map_err(parse_error_to_sql_error)?;
        let status_text: String = row.get("status")?;
        let status =
            LoopInterventionStatus::from_str(&status_text).map_err(parse_error_to_sql_error)?;
        Ok(Self {
            id: row.get("id")?,
            action,
            reason: row.get("reason")?,
            instruction: row.get("instruction")?,
            status,
            requested_by: row.get("requested_by")?,
            applied_run_id: row.get("applied_run_id")?,
            created_at: row.get("created_at")?,
            updated_at: row.get("updated_at")?,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
#[error("invalid {enum_name} value: {value}")]
pub struct ParseEnumError {
    enum_name: &'static str,
    value: String,
}

impl ParseEnumError {
    fn new(enum_name: &'static str, value: &str) -> Self {
        Self {
            enum_name,
            value: value.to_owned(),
        }
    }
}

macro_rules! string_enum {
    ($name:ident, $enum_name:literal, { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq)]
        pub enum $name {
            $($variant),+
        }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self {
                    $(Self::$variant => $value),+
                }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                formatter.write_str(self.as_str())
            }
        }

        impl Serialize for $name {
            fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
            where
                S: serde::Serializer,
            {
                serializer.serialize_str(self.as_str())
            }
        }

        impl FromStr for $name {
            type Err = ParseEnumError;

            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value {
                    $($value => Ok(Self::$variant),)+
                    _ => Err(ParseEnumError::new($enum_name, value)),
                }
            }
        }
    };
}

string_enum!(WorkItemStatus, "work item status", {
    Pending => "pending",
    Running => "running",
    Held => "held",
    Done => "done",
    Canceled => "canceled",
});

string_enum!(RunStatus, "run status", {
    Running => "running",
    Success => "success",
    Failed => "failed",
    Partial => "partial",
});

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArtifactKind {
    Json,
    Csv,
    Report,
    Image,
    Other,
    Custom(String),
}

impl ArtifactKind {
    pub fn as_str(&self) -> &str {
        match self {
            Self::Json => "json",
            Self::Csv => "csv",
            Self::Report => "report",
            Self::Image => "image",
            Self::Other => "other",
            Self::Custom(value) => value.as_str(),
        }
    }
}

impl FromStr for ArtifactKind {
    type Err = ParseEnumError;

    fn from_str(value: &str) -> Result<Self, Self::Err> {
        let normalized = value.trim();
        if normalized.is_empty() {
            return Ok(Self::Other);
        }
        Ok(match normalized {
            "json" => Self::Json,
            "csv" => Self::Csv,
            "report" => Self::Report,
            "image" => Self::Image,
            "other" => Self::Other,
            custom => Self::Custom(custom.to_string()),
        })
    }
}

impl fmt::Display for ArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

impl Serialize for ArtifactKind {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        serializer.serialize_str(self.as_str())
    }
}

string_enum!(ValidationOutcome, "validation outcome", {
    Pass => "pass",
    Fail => "fail",
    Error => "error",
    Skipped => "skipped",
});

string_enum!(DecisionOutcome, "decision outcome", {
    Continue => "continue",
    Stop => "stop",
    Inconclusive => "inconclusive",
});

string_enum!(LoopInterventionAction, "loop intervention action", {
    Pause => "pause",
    Stop => "stop",
    Steer => "steer",
});

string_enum!(LoopInterventionStatus, "loop intervention status", {
    Pending => "pending",
    Applied => "applied",
    Cleared => "cleared",
});

string_enum!(GlobalObservationKind, "global observation kind", {
    Observation => "observation",
    Notification => "notification",
});

string_enum!(GlobalObservationStatus, "global observation status", {
    Active => "active",
    Cleared => "cleared",
});
