use clap::{Args, Subcommand};

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr telemetry status\n  ldgr telemetry enable\n  ldgr telemetry disable\n\nTelemetry controls apply only to numerical state-sequence collection. Disable takes effect immediately and does not require a network request."
)]
pub struct TelemetryArgs {
    #[command(subcommand)]
    pub command: TelemetryCommand,
}

#[derive(Debug, Subcommand)]
pub enum TelemetryCommand {
    /// Show the stored decision and effective collection state.
    Status,
    /// Explicitly opt in to numerical state-sequence collection.
    Enable,
    /// Immediately opt out and delete unsent numerical sequences.
    Disable,
}
