use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use crate::store::{DecisionOutcome, RunStatus, ValidationOutcome};

const RUN_HELP: &str = "Examples:\n  ldgr run start fix-login --command \"cargo test login\"\n  ldgr run finish 7 --status success --notes \"Tests pass.\"\n  ldgr run close fix-login --status success --outcome stop --rationale \"Project complete.\"\n  ldgr run close 7 --status success --outcome continue --rationale \"Next slice identified.\" --next-slug docs --next-title \"Update docs\" --next-description \"Document token refresh.\"\n  ldgr run close fix-login --status success --outcome continue --rationale \"Continue with queued work.\" --next-slug queued-docs\n\nRun references may be numeric run IDs or work-item slugs. Runs capture bounded execution. Decisions close the work narrative. Use --outcome stop only for terminal closure. Use --outcome continue only with --next-slug, either linking an existing nonterminal work item or creating one with --next-title and --next-description.";

const OBSERVATION_HELP: &str = "Examples:\n  ldgr observation add 7 --body \"Token refresh fails when the cache is empty.\"\n  ldgr observe add fix-login --body \"Token refresh fails when the cache is empty.\"\n  ldgr observe fix-login --body \"Token refresh fails when the cache is empty.\"\n  ldgr observation list --run-id fix-login\n\nRun references may be numeric run IDs or work-item slugs. Observations are append-only run notes. Add a correction observation instead of editing history.";

const ARTIFACT_HELP: &str = "Examples:\n  ldgr artifact add 7 --kind report --path target/test-output.txt --description \"Test transcript.\"\n  ldgr artifact add fix-login --kind report --path target/test-output.txt --description \"Test transcript.\"\n  ldgr artifact show 3\n  ldgr artifact list --run-id fix-login\n\nRun references may be numeric run IDs or work-item slugs. Artifacts preserve files or durable references produced during a run.";

const VALIDATION_HELP: &str = "Examples:\n  ldgr validation record 7 --outcome pass --command \"cargo test\"\n  ldgr validation record fix-login --outcome skipped --rationale \"No TypeScript files changed.\"\n  ldgr validation list --run-id fix-login\n\nRun references may be numeric run IDs or work-item slugs. Validation records capture generic PASS, FAIL, ERROR, and SKIPPED outcomes. Skipped validation requires a durable rationale.";

#[derive(Debug, Args)]
#[command(after_help = RUN_HELP)]
pub struct RunArgs {
    #[command(subcommand)]
    pub command: RunCommand,
}

#[derive(Debug, Subcommand)]
pub enum RunCommand {
    /// List runs.
    List(ListRunArgs),
    /// Show one run.
    Show(ShowRunArgs),
    /// Start a run for a work item.
    Start(StartRunArgs),
    /// Finish a run, leaving its work decision pending.
    Finish(FinishRunArgs),
    /// Finish a run and record the associated work decision.
    Close(CloseRunArgs),
}

#[derive(Debug, Args)]
pub struct ListRunArgs {
    #[arg(long, value_enum)]
    pub status: Option<CliRunFilterStatus>,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowRunArgs {
    pub run_id: String,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct StartRunArgs {
    pub work_slug: String,

    #[arg(long)]
    pub command: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "`run finish` records only the run result. The work item remains running in the \"run finished, work decision pending\" state until `ldgr decision record ...` is called. Prefer `ldgr run close ...` when the run result and work decision are both known."
)]
pub struct FinishRunArgs {
    pub run_id: String,

    #[arg(long, value_enum)]
    pub status: CliRunStatus,

    #[arg(long)]
    pub notes: Option<String>,
}

#[derive(Debug, Args)]
pub struct CloseRunArgs {
    pub run_id: String,

    #[arg(long, value_enum)]
    pub status: CliRunStatus,

    #[arg(long, value_enum)]
    pub outcome: CliCloseDecisionOutcome,

    #[arg(long)]
    pub rationale: String,

    #[arg(long)]
    pub notes: Option<String>,

    #[arg(long)]
    pub next_slug: Option<String>,

    #[arg(long)]
    pub next_title: Option<String>,

    #[arg(long)]
    pub next_description: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliRunStatus {
    #[value(
        alias = "done",
        alias = "complete",
        alias = "completed",
        alias = "finished",
        alias = "ok",
        alias = "passed"
    )]
    Success,
    #[value(
        alias = "fail",
        alias = "failure",
        alias = "error",
        alias = "errored",
        alias = "blocked"
    )]
    Failed,
    #[value(alias = "incomplete", alias = "unfinished")]
    Partial,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliCloseDecisionOutcome {
    #[value(alias = "next", alias = "continue-next")]
    Continue,
    #[value(
        alias = "done",
        alias = "complete",
        alias = "completed",
        alias = "finished"
    )]
    Stop,
    #[value(alias = "unclear", alias = "unknown", alias = "partial")]
    Inconclusive,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliRunFilterStatus {
    Running,
    #[value(
        alias = "done",
        alias = "complete",
        alias = "completed",
        alias = "finished",
        alias = "ok",
        alias = "passed"
    )]
    Success,
    #[value(
        alias = "fail",
        alias = "failure",
        alias = "error",
        alias = "errored",
        alias = "blocked"
    )]
    Failed,
    #[value(alias = "incomplete", alias = "unfinished")]
    Partial,
}

impl From<CliRunFilterStatus> for RunStatus {
    fn from(status: CliRunFilterStatus) -> Self {
        match status {
            CliRunFilterStatus::Running => Self::Running,
            CliRunFilterStatus::Success => Self::Success,
            CliRunFilterStatus::Failed => Self::Failed,
            CliRunFilterStatus::Partial => Self::Partial,
        }
    }
}

impl From<CliCloseDecisionOutcome> for DecisionOutcome {
    fn from(outcome: CliCloseDecisionOutcome) -> Self {
        match outcome {
            CliCloseDecisionOutcome::Continue => Self::Continue,
            CliCloseDecisionOutcome::Stop => Self::Stop,
            CliCloseDecisionOutcome::Inconclusive => Self::Inconclusive,
        }
    }
}

impl From<CliRunStatus> for RunStatus {
    fn from(status: CliRunStatus) -> Self {
        match status {
            CliRunStatus::Success => Self::Success,
            CliRunStatus::Failed => Self::Failed,
            CliRunStatus::Partial => Self::Partial,
        }
    }
}

#[derive(Debug, Args)]
#[command(after_help = OBSERVATION_HELP)]
pub struct ObservationArgs {
    #[command(subcommand)]
    pub command: ObservationCommand,
}

#[derive(Debug, Subcommand)]
pub enum ObservationCommand {
    /// List observations.
    List(ListObservationArgs),
    /// Add a textual observation to a run.
    Add(AddObservationArgs),
}

#[derive(Debug, Args)]
pub struct ListObservationArgs {
    #[arg(long)]
    pub run_id: Option<String>,

    #[arg(long, default_value_t = 20)]
    pub limit: i64,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AddObservationArgs {
    pub run_id: String,

    #[arg(long)]
    pub body: String,
}

#[derive(Debug, Args)]
#[command(after_help = ARTIFACT_HELP)]
pub struct ArtifactArgs {
    #[command(subcommand)]
    pub command: ArtifactCommand,
}

#[derive(Debug, Subcommand)]
pub enum ArtifactCommand {
    /// List artifacts.
    List(ListArtifactArgs),
    /// Show one artifact record.
    Show(ShowArtifactArgs),
    /// Add an artifact path to a run.
    Add(AddArtifactArgs),
}

#[derive(Debug, Args)]
pub struct ListArtifactArgs {
    #[arg(long)]
    pub run_id: Option<String>,

    #[arg(long, default_value_t = 20)]
    pub limit: i64,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowArtifactArgs {
    pub artifact_id: i64,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AddArtifactArgs {
    pub run_id: String,

    #[arg(long, default_value = "other")]
    pub kind: String,

    #[arg(long)]
    pub path: PathBuf,

    #[arg(long)]
    pub description: String,
}

#[derive(Debug, Args)]
#[command(after_help = VALIDATION_HELP)]
pub struct ValidationArgs {
    #[command(subcommand)]
    pub command: ValidationCommand,
}

#[derive(Debug, Subcommand)]
pub enum ValidationCommand {
    /// List validation records.
    List(ListValidationArgs),
    /// Record a validation outcome for a run.
    Record(RecordValidationArgs),
}

#[derive(Debug, Args)]
pub struct ListValidationArgs {
    #[arg(long)]
    pub run_id: Option<String>,

    #[arg(long, default_value_t = 20)]
    pub limit: i64,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RecordValidationArgs {
    pub run_id: String,

    #[arg(long, value_enum)]
    pub outcome: CliValidationOutcome,

    #[arg(long)]
    pub command: Option<String>,

    #[arg(long)]
    pub rationale: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliValidationOutcome {
    #[value(
        alias = "passed",
        alias = "success",
        alias = "succeeded",
        alias = "ok",
        alias = "done",
        alias = "complete"
    )]
    Pass,
    #[value(alias = "failed", alias = "failure")]
    Fail,
    #[value(alias = "errored")]
    Error,
    #[value(alias = "skip")]
    Skipped,
}

impl From<CliValidationOutcome> for ValidationOutcome {
    fn from(outcome: CliValidationOutcome) -> Self {
        match outcome {
            CliValidationOutcome::Pass => Self::Pass,
            CliValidationOutcome::Fail => Self::Fail,
            CliValidationOutcome::Error => Self::Error,
            CliValidationOutcome::Skipped => Self::Skipped,
        }
    }
}
