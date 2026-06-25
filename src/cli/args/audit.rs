use clap::{Args, Subcommand, ValueEnum};

use crate::store::DecisionOutcome;

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr decision record fix-login --outcome continue --rationale \"Login fixed; docs remain.\" --next-slug docs --next-title \"Update docs\" --next-description \"Document login flow.\"\n  ldgr decision record fix-login --outcome continue --rationale \"Login fixed; docs already queued.\" --next-slug queued-docs\n  ldgr decision record docs --outcome stop --rationale \"All requested work is complete.\"\n\nDecisions close the narrative for a work item. --next-slug links an existing active work item, or creates one when --next-title and --next-description are supplied. Stop means the whole project is complete. Use observations for facts learned during the run."
)]
pub struct DecisionArgs {
    #[command(subcommand)]
    pub command: DecisionCommand,
}

#[derive(Debug, Subcommand)]
pub enum DecisionCommand {
    /// List decisions.
    List(ListDecisionArgs),
    /// Record a decision for a work item.
    Record(RecordDecisionArgs),
}

#[derive(Debug, Args)]
pub struct ListDecisionArgs {
    #[arg(long)]
    pub work_slug: Option<String>,

    #[arg(long, default_value_t = 20)]
    pub limit: i64,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct RecordDecisionArgs {
    pub work_slug: String,

    #[arg(long, value_enum)]
    pub outcome: CliDecisionOutcome,

    #[arg(long)]
    pub rationale: String,

    #[arg(long)]
    pub next_slug: Option<String>,

    #[arg(long)]
    pub next_title: Option<String>,

    #[arg(long)]
    pub next_description: Option<String>,
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum CliDecisionOutcome {
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

impl From<CliDecisionOutcome> for DecisionOutcome {
    fn from(outcome: CliDecisionOutcome) -> Self {
        match outcome {
            CliDecisionOutcome::Continue => Self::Continue,
            CliDecisionOutcome::Stop => Self::Stop,
            CliDecisionOutcome::Inconclusive => Self::Inconclusive,
        }
    }
}
