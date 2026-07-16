use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

use crate::store::{GlobalObservationKind, GlobalObservationStatus, HoldKind, WorkItemStatus};

const WORK_HELP: &str = "Examples:\n  ldgr work create fix-login --title \"Fix login\" --description \"Repair token refresh.\" --depends-on schema,registry\n  ldgr work edit fix-login --priority P0 --group release --acceptance-criteria \"Regression test passes.\"\n  ldgr work dependency add fix-login schema\n  ldgr work graph --format mermaid\n  ldgr work audit\n  ldgr work status set fix-login held --reason \"Waiting for dependency.\"\n\n--depends-on accepts a comma-separated list, repeated flags, or both. On work edit it replaces the complete dependency set; use work dependency add/remove for one edge. Imports are validated and committed transactionally.";

const NOTICE_HELP: &str = "Examples:\n  ldgr notice add --kind notification --body \"Prefer the simpler fix.\"\n  ldgr notice edit 1 --body \"Course correction handled.\" --clear-source\n  ldgr notice clear 1 --reason \"No longer relevant.\"\n\nNotices are operator-visible steering outside a run. Observations remain attached to runs.";

#[derive(Debug, Args)]
#[command(after_help = WORK_HELP)]
pub struct WorkArgs {
    #[command(subcommand)]
    pub command: WorkCommand,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr next\n  ldgr next --commands\n\nUse --commands to print exact suggested control-surface commands for the current adapter-aware handoff."
)]
pub struct NextArgs {
    /// Print suggested next commands instead of only the next work item.
    #[arg(long)]
    pub commands: bool,
}

#[derive(Debug, Subcommand)]
pub enum WorkCommand {
    /// List work items.
    List(ListWorkArgs),
    /// Show one work item.
    Show(ShowWorkArgs),
    /// Create a pending work item.
    Create(CreateWorkArgs),
    /// Edit work metadata or replace its complete dependency set.
    Edit(EditWorkArgs),
    /// Add or remove individual dependency edges.
    Dependency(WorkDependencyArgs),
    /// Inspect the work dependency graph.
    Graph(WorkGraphArgs),
    /// Audit the schedule for structural problems.
    Audit(WorkAuditArgs),
    /// Set a work item's lifecycle status.
    Status(WorkStatusArgs),
    /// Remove a work item and its dependent records.
    Delete(DeleteWorkArgs),
    /// Import many work items and dependencies from a JSON schedule.
    Import(ImportWorkArgs),
    /// Export the durable schedule as portable JSON.
    Export(ExportWorkArgs),
}

#[derive(Debug, Args)]
pub struct ListWorkArgs {
    #[arg(long, value_enum)]
    pub status: Option<CliWorkItemStatus>,

    #[arg(long)]
    pub program: Option<String>,

    #[arg(long)]
    pub priority: Option<String>,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowWorkArgs {
    pub slug: String,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct CreateWorkArgs {
    pub slug: String,

    #[arg(long)]
    pub title: String,

    #[arg(long)]
    pub description: String,

    #[arg(long)]
    pub priority: Option<String>,

    #[arg(long)]
    pub program: Option<String>,

    #[arg(long = "group")]
    pub group: Option<String>,

    #[arg(long)]
    pub acceptance_criteria: Option<String>,

    /// Dependency slugs; accepts commas and/or repeated --depends-on flags.
    #[arg(long = "depends-on", value_delimiter = ',')]
    pub dependencies: Vec<String>,
}

#[derive(Debug, Args)]
pub struct EditWorkArgs {
    pub slug: String,

    #[arg(long)]
    pub title: Option<String>,

    #[arg(long)]
    pub description: Option<String>,

    #[arg(long, conflicts_with = "clear_priority")]
    pub priority: Option<String>,

    #[arg(long)]
    pub clear_priority: bool,

    #[arg(long, conflicts_with = "clear_program")]
    pub program: Option<String>,

    #[arg(long)]
    pub clear_program: bool,

    #[arg(long = "group", conflicts_with = "clear_group")]
    pub group: Option<String>,

    #[arg(long)]
    pub clear_group: bool,

    #[arg(long, conflicts_with = "clear_acceptance_criteria")]
    pub acceptance_criteria: Option<String>,

    #[arg(long)]
    pub clear_acceptance_criteria: bool,

    /// Replacement dependency set; accepts commas and/or repeated flags.
    #[arg(
        long = "depends-on",
        value_delimiter = ',',
        conflicts_with = "clear_dependencies"
    )]
    pub dependencies: Vec<String>,

    #[arg(long)]
    pub clear_dependencies: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr work dependency add child prerequisite\n  ldgr work dependency remove child prerequisite\n\nThe child is blocked until the prerequisite is done. Dependency changes reject self-edges and cycles."
)]
pub struct WorkDependencyArgs {
    #[command(subcommand)]
    pub command: WorkDependencyCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkDependencyCommand {
    /// Make CHILD depend on PREREQUISITE.
    Add(EditWorkDependencyArgs),
    /// Remove CHILD's dependency on PREREQUISITE.
    Remove(EditWorkDependencyArgs),
}

#[derive(Debug, Args)]
pub struct EditWorkDependencyArgs {
    /// Work item that is blocked by the dependency.
    pub child: String,
    /// Work item that must be completed first.
    pub prerequisite: String,
}

#[derive(Debug, Args)]
pub struct WorkGraphArgs {
    /// Show only work that is effectively ready.
    #[arg(long, conflicts_with = "blocked")]
    pub ready: bool,

    /// Show only nonterminal work that is not effectively ready.
    #[arg(long)]
    pub blocked: bool,

    #[arg(long, value_enum, default_value_t = CliWorkGraphFormat::Human)]
    pub format: CliWorkGraphFormat,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliWorkGraphFormat {
    Human,
    Json,
    Mermaid,
}

#[derive(Debug, Args)]
pub struct WorkAuditArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr work status set my-work pending\n  ldgr work status set my-work running\n  ldgr work status set my-work held --reason \"Blocked on review.\"\n  ldgr work status set my-work done --reason \"Closed manually.\"\n\nPrefer decisions to close ordinary completed work; status set is for explicit lifecycle correction/control."
)]
pub struct WorkStatusArgs {
    #[command(subcommand)]
    pub command: WorkStatusCommand,
}

#[derive(Debug, Subcommand)]
pub enum WorkStatusCommand {
    /// Set the status for one work item.
    Set(SetWorkStatusArgs),
}

#[derive(Debug, Args)]
pub struct SetWorkStatusArgs {
    pub slug: String,

    #[arg(value_enum)]
    pub status: CliWorkItemStatus,

    #[arg(long)]
    pub reason: Option<String>,

    /// Classify held work as blocked, deferred, or awaiting external validation.
    #[arg(long, value_enum)]
    pub hold_kind: Option<CliHoldKind>,
}

#[derive(Debug, Args)]
pub struct DeleteWorkArgs {
    pub slug: String,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Example document:\n  {\n    \"format\": \"ldgr.schedule.v1\",\n    \"work_items\": [\n      {\"slug\":\"base\",\"title\":\"Base\",\"description\":\"Build base.\"},\n      {\"slug\":\"gate\",\"title\":\"Gate\",\"description\":\"Validate.\",\"dependencies\":[\"base\"]}\n    ]\n  }\n\nThe complete import is one transaction: any invalid item, missing dependency, or cycle rolls it back. Use `ldgr work export --example` for a fuller document."
)]
pub struct ImportWorkArgs {
    /// JSON schedule path, or - to read from stdin.
    pub path: String,

    /// Update matching slugs instead of rejecting them.
    #[arg(long)]
    pub upsert: bool,

    /// Validate the complete import without changing the ledger.
    #[arg(long)]
    pub dry_run: bool,
}

#[derive(Debug, Args)]
pub struct ExportWorkArgs {
    /// Write JSON to this path instead of stdout.
    #[arg(long)]
    pub output: Option<PathBuf>,

    #[arg(long)]
    pub program: Option<String>,

    #[arg(long)]
    pub priority: Option<String>,

    /// Print an example schedule document instead of exporting the ledger.
    #[arg(long, conflicts_with_all = ["output", "program", "priority"])]
    pub example: bool,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliHoldKind {
    Blocked,
    Deferred,
    ExternalValidation,
}

impl From<CliHoldKind> for HoldKind {
    fn from(kind: CliHoldKind) -> Self {
        match kind {
            CliHoldKind::Blocked => Self::Blocked,
            CliHoldKind::Deferred => Self::Deferred,
            CliHoldKind::ExternalValidation => Self::ExternalValidation,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliWorkItemStatus {
    #[value(alias = "todo", alias = "queued")]
    Pending,
    #[value(alias = "active", alias = "in-progress")]
    Running,
    #[value(alias = "blocked", alias = "paused", alias = "deferred")]
    Held,
    #[value(
        alias = "complete",
        alias = "completed",
        alias = "finished",
        alias = "success",
        alias = "succeeded",
        alias = "ok"
    )]
    Done,
    #[value(alias = "cancelled", alias = "abandoned", alias = "dropped")]
    Canceled,
}

impl From<CliWorkItemStatus> for WorkItemStatus {
    fn from(status: CliWorkItemStatus) -> Self {
        match status {
            CliWorkItemStatus::Pending => Self::Pending,
            CliWorkItemStatus::Running => Self::Running,
            CliWorkItemStatus::Held => Self::Held,
            CliWorkItemStatus::Done => Self::Done,
            CliWorkItemStatus::Canceled => Self::Canceled,
        }
    }
}

#[derive(Debug, Args)]
#[command(after_help = NOTICE_HELP)]
pub struct NoticeArgs {
    #[command(subcommand)]
    pub command: NoticeCommand,
}

#[derive(Debug, Subcommand)]
pub enum NoticeCommand {
    /// List global observations and notifications.
    List(ListNoticeArgs),
    /// Add a global observation or notification visible in context.
    Add(AddNoticeArgs),
    /// Edit a global observation or notification.
    Edit(EditNoticeArgs),
    /// Clear a global observation or notification after it is no longer relevant.
    Clear(ClearNoticeArgs),
}

#[derive(Debug, Args)]
pub struct ListNoticeArgs {
    #[arg(long, value_enum, default_value = "active")]
    pub status: CliGlobalObservationStatus,

    #[arg(long, default_value_t = 20)]
    pub limit: i64,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct AddNoticeArgs {
    #[arg(long, value_enum, default_value = "observation")]
    pub kind: CliGlobalObservationKind,

    #[arg(long)]
    pub body: String,

    #[arg(long)]
    pub source: Option<String>,
}

#[derive(Debug, Args)]
pub struct EditNoticeArgs {
    pub id: i64,

    #[arg(long, value_enum)]
    pub kind: Option<CliGlobalObservationKind>,

    #[arg(long)]
    pub body: Option<String>,

    #[arg(long)]
    pub source: Option<String>,

    #[arg(long)]
    pub clear_source: bool,

    #[arg(long, value_enum)]
    pub status: Option<CliGlobalObservationStatus>,
}

#[derive(Debug, Args)]
pub struct ClearNoticeArgs {
    pub id: i64,

    #[arg(long)]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliGlobalObservationKind {
    Observation,
    Notification,
}

impl From<CliGlobalObservationKind> for GlobalObservationKind {
    fn from(kind: CliGlobalObservationKind) -> Self {
        match kind {
            CliGlobalObservationKind::Observation => Self::Observation,
            CliGlobalObservationKind::Notification => Self::Notification,
        }
    }
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliGlobalObservationStatus {
    Active,
    Cleared,
    All,
}

impl From<CliGlobalObservationStatus> for GlobalObservationStatus {
    fn from(status: CliGlobalObservationStatus) -> Self {
        match status {
            CliGlobalObservationStatus::Active => Self::Active,
            CliGlobalObservationStatus::Cleared => Self::Cleared,
            CliGlobalObservationStatus::All => Self::Active,
        }
    }
}
