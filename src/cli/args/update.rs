use std::path::PathBuf;

use clap::Args;

const UPDATE_HELP: &str = "Examples:\n  ldgr update --check\n  ldgr update --check --json\n  ldgr update --adapters-only\n  ldgr update --adapters-only --yes\n  ldgr update --adapter research --adapter conduct\n  ldgr update --check --core-only\n  ldgr update --prerelease\n  ldgr update --offline\n  ldgr update --store ./store --check\n\nCheck mode authenticates configured catalogs and resolves one compatibility-bound plan without downloading or mutating. Apply mode stages every selected artifact before mutation and rolls back the whole selected set on failure. --store uses a signed, network-free local release store. Non-interactive apply requires --yes.";

/// Check for or apply compatible Core, agentctl, and adapter updates.
#[derive(Clone, Debug, Args)]
#[command(after_help = UPDATE_HELP)]
pub struct UpdateArgs {
    /// Resolve and report only; never download or install release artifacts.
    #[arg(long)]
    pub check: bool,
    /// Emit exactly one schema-versioned JSON document on stdout.
    #[arg(long)]
    pub json: bool,
    /// Confirm update application and safe legacy-install adoption without prompting.
    #[arg(long)]
    pub yes: bool,
    /// Select only the compatibility-bound Core and agentctl bundle.
    #[arg(long, conflicts_with_all = ["adapters_only", "adapters"])]
    pub core_only: bool,
    /// Select all eligible receipt-managed adapters and leave Core unchanged.
    #[arg(long)]
    pub adapters_only: bool,
    /// Select one installed adapter; repeat to select more than one.
    #[arg(long = "adapter", value_name = "SLUG", action = clap::ArgAction::Append)]
    pub adapters: Vec<String>,
    /// Permit prerelease Core and adapter targets.
    #[arg(long)]
    pub prerelease: bool,
    /// Use only configured local catalogs and local artifact references.
    #[arg(long)]
    pub offline: bool,
    /// Read signed catalogs, keyring, archives, and signatures from this local release store.
    #[arg(long, value_name = "DIRECTORY")]
    pub store: Option<PathBuf>,
}
