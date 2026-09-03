use std::path::PathBuf;

use clap::{Args, Subcommand};

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr telemetry status\n  ldgr telemetry preview\n  ldgr telemetry transmit\n  ldgr telemetry disable\n  ldgr telemetry donation enable\n\nAnonymous construction telemetry is enabled by default and can be disabled immediately. Experience donation is separate and remains off until explicitly enabled."
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
    /// Control the separate, sanitized LDGR work-episode donation program.
    Donation(TelemetryDonationArgs),
}

#[derive(Debug, Args)]
pub struct TelemetryDonationArgs {
    #[command(subcommand)]
    pub command: TelemetryDonationCommand,
}

#[derive(Debug, Subcommand)]
pub enum TelemetryDonationCommand {
    /// Show whether experience donation has been explicitly enabled.
    Status,
    /// Opt in to automatic sanitized LDGR work-episode donation and delivery.
    Enable,
    /// Disable donation and delete unsent work episodes. Anonymous telemetry is unchanged.
    Disable,
}

#[derive(Debug, Args)]
pub struct TelemetryTransmitArgs {
    /// Bare HTTPS collector origin. Falls back to LDGR_TELEMETRY_COLLECTOR, then https://ldgr.run.
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
