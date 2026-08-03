use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr telemetry status\n  ldgr telemetry preview\n  ldgr telemetry transmit --collector https://collector.example\n  ldgr telemetry enable\n  ldgr telemetry disable\n\nTelemetry controls apply only to numerical state-sequence collection. Disable takes effect immediately and does not require a network request."
)]
pub struct TelemetryArgs {
    #[command(subcommand)]
    pub command: TelemetryCommand,
}

#[derive(Debug, Subcommand)]
pub enum TelemetryCommand {
    /// Show the stored decision and effective collection state.
    Status,
    /// Show exact pending raw arrays and destination protocol paths without sending them.
    Preview,
    /// Best-effort transmit pending numerical sequences to an HTTPS collector.
    Transmit(TelemetryTransmitArgs),
    /// Explicitly opt in to numerical state-sequence collection.
    Enable,
    /// Immediately opt out and delete unsent numerical sequences.
    Disable,
}

#[derive(Debug, Args)]
pub struct TelemetryTransmitArgs {
    /// Bare HTTPS collector origin. Falls back to LDGR_TELEMETRY_COLLECTOR.
    #[arg(long, value_name = "HTTPS_ORIGIN")]
    pub collector: Option<String>,

    /// Additional PEM root certificate for private collector TLS verification.
    #[arg(long = "root-ca-pem", value_name = "PATH")]
    pub root_ca_pem: Vec<PathBuf>,

    /// Maximum random delay before each send attempt, in milliseconds.
    #[arg(long, default_value_t = 30_000)]
    pub max_delay_ms: u64,

    /// Per-request timeout in milliseconds.
    #[arg(long, default_value_t = 10_000)]
    pub timeout_ms: u64,
}
