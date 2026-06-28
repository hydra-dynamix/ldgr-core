use clap::{Args, Subcommand};

const ADAPTER_HELP: &str = "Examples:\n  ldgr adapter list\n  ldgr adapter show code\n  ldgr code --help\n  ldgr adapter dispatch code-check-all\n\nAdapters are discovered from LDGR_ADAPTER_PATH, .ldgr, LDGR_HOME, and ~/.ldgr. Core dynamically dispatches installed adapter namespaces declared by adapter.toml.";

#[derive(Debug, Args)]
#[command(after_help = ADAPTER_HELP)]
pub struct AdapterArgs {
    #[command(subcommand)]
    pub command: AdapterCommand,
}

#[derive(Debug, Subcommand)]
pub enum AdapterCommand {
    /// List installed adapters.
    List(ListAdapterArgs),
    /// Show one installed adapter by slug or alias.
    Show(ShowAdapterArgs),
    /// Resolve advertised adapter command metadata without executing it.
    Dispatch(DispatchAdapterArgs),
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
