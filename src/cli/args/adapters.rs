use clap::{Args, Subcommand};

const ADAPTER_HELP: &str = "Examples:\n  ldgr adapter install\n  ldgr adapter install list\n  ldgr adapter install conduct\n  ldgr adapter list\n  ldgr adapter show conduct\n  ldgr conduct --help\n  ldgr adapter dispatch conduct-batch-status\n\nAdapters are installed under ~/.ldgr/<adapter>/adapter.toml. Core dynamically dispatches installed adapter namespaces declared by adapter.toml.";

#[derive(Debug, Args)]
#[command(after_help = ADAPTER_HELP)]
pub struct AdapterArgs {
    #[command(subcommand)]
    pub command: AdapterCommand,
}

#[derive(Debug, Subcommand)]
pub enum AdapterCommand {
    /// Install an adapter or list adapters available to install.
    Install(AdapterInstallArgs),
    /// List installed adapters.
    List(ListAdapterArgs),
    /// Show one installed adapter by slug or alias.
    Show(ShowAdapterArgs),
    /// Resolve advertised adapter command metadata without executing it.
    Dispatch(DispatchAdapterArgs),
}

#[derive(Debug, Args)]
pub struct AdapterInstallArgs {
    /// Adapter slug to install, `list` to show available adapters, or omit for the selection menu.
    pub name: Option<String>,

    /// Source checkout root containing adapter crates. Optional override for local source installs.
    #[arg(long)]
    pub source_root: Option<std::path::PathBuf>,

    /// Exact install directory. Defaults to ~/.ldgr/<adapter>.
    #[arg(long)]
    pub install_root: Option<std::path::PathBuf>,

    /// Accept defaults and do not prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
pub struct ListAdapterArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ShowAdapterArgs {
    pub slug_or_alias: String,

    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct DispatchAdapterArgs {
    pub command: String,

    #[arg(long)]
    pub json: bool,
}
