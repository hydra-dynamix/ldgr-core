use clap::{Args, Subcommand, ValueEnum};

use crate::store::{GlobalObservationKind, GlobalObservationStatus, WorkItemStatus};

const WORK_HELP: &str = "Examples:\n  ldgr work create fix-login --title \"Fix login\" --description \"Repair token refresh.\"\n  ldgr work edit fix-login --description \"Repair refresh and add regression coverage.\"\n  ldgr work status set fix-login held --reason \"Waiting for dependency.\"\n\nUse work status set for lifecycle changes; use work edit only for title/description corrections.";

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
    /// Edit a work item's title or description.
    Edit(EditWorkArgs),
    /// Set a work item's lifecycle status.
    Status(WorkStatusArgs),
    /// Remove a work item and its dependent records.
    Delete(DeleteWorkArgs),
}

#[derive(Debug, Args)]
pub struct ListWorkArgs {
    #[arg(long, value_enum)]
    pub status: Option<CliWorkItemStatus>,

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
}

#[derive(Debug, Args)]
pub struct EditWorkArgs {
    pub slug: String,

    #[arg(long)]
    pub title: Option<String>,

    #[arg(long)]
    pub description: Option<String>,
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
}

#[derive(Debug, Args)]
pub struct DeleteWorkArgs {
    pub slug: String,
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
