use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HarnessKind {
    Pi,
    Codex,
    Claude,
    Openclaw,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr install\n  ldgr install --harness pi --harness claude --adapter conduct --yes\n  ldgr install --yes --no-agentctl\n  ldgr install adapter code --yes\n\nWithout --harness, the installer asks interactively and defaults to Pi. Multiple harnesses may be selected. In interactive mode the installer also offers adapter bundle selection. The selected harness config is recorded under ~/.ldgr/. agentctl is installed when missing unless --no-agentctl is passed."
)]
pub struct InstallArgs {
    #[command(subcommand)]
    pub command: Option<InstallCommand>,

    /// Harness to install LDGR integration into. Repeatable.
    #[arg(long, value_enum)]
    pub harness: Vec<HarnessKind>,

    /// Accept defaults and do not prompt. Defaults to Pi when --harness is omitted.
    #[arg(long)]
    pub yes: bool,

    /// Do not install agentctl even if it is missing from PATH.
    #[arg(long)]
    pub no_agentctl: bool,

    /// Adapter bundle to install after harness setup. Repeatable.
    #[arg(long)]
    pub adapter: Vec<String>,
}

#[derive(Debug, Subcommand)]
pub enum InstallCommand {
    /// Install an open-source adapter bundle into ~/.ldgr/<adapter>.
    Adapter(InstallAdapterArgs),
}

#[derive(Debug, Args)]
pub struct InstallAdapterArgs {
    /// Adapter name, e.g. conduct, research, example, code, bench, explore, security.
    pub name: String,

    /// Source checkout root containing adapter crates. Optional override for local source installs.
    #[arg(long)]
    pub source_root: Option<PathBuf>,

    /// Exact install directory. Defaults to ~/.ldgr/<adapter>.
    #[arg(long)]
    pub install_root: Option<PathBuf>,

    /// Accept defaults and do not prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr context --brief\n  ldgr context --json\n\nContext is the expanded handoff view. Start with `ldgr status`; use context when you need deeper history."
)]
pub struct ContextArgs {
    #[arg(long)]
    pub json: bool,

    /// Print the compact agent on-ramp instead of the full cockpit.
    #[arg(long)]
    pub brief: bool,

    /// Number of recent records to include in brief context lists.
    #[arg(long, default_value_t = 3)]
    pub recent: usize,

    /// Maximum characters for freeform brief context fields.
    #[arg(long, default_value_t = 240)]
    pub width: usize,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr status\n  ldgr status --json\n\nStatus is read-only. To change work state, use `ldgr work status set <work> <status>`."
)]
pub struct StatusArgs {
    #[arg(long)]
    pub json: bool,

    /// Number of recent records to include in the status summary.
    #[arg(long, default_value_t = 3)]
    pub recent: usize,

    /// Maximum characters for freeform status fields.
    #[arg(long, default_value_t = 240)]
    pub width: usize,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr web\n  ldgr web --port 4321\n\nThe web cockpit binds to loopback by default and prints a startup URL containing an ephemeral control token for mutating routes. Non-loopback exposure requires --unsafe-expose and an explicit --control-token."
)]
pub struct WebArgs {
    #[arg(long, default_value = "127.0.0.1")]
    pub host: String,

    #[arg(long, default_value_t = 8686)]
    pub port: u16,

    /// Allow binding the cockpit to a non-loopback host. Requires --control-token.
    #[arg(long)]
    pub unsafe_expose: bool,

    /// Use this token in X-LDGR-Control-Token for mutating cockpit requests.
    ///
    /// When omitted on loopback, ldgr generates an ephemeral token at startup
    /// and prints a URL that seeds the existing browser session token storage.
    #[arg(long)]
    pub control_token: Option<String>,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr loop run --prompt prompts/loop-prompt.md --agent agentctl\n  ldgr loop run --prompt prompts/loop-prompt.md --agent agentctl --until-empty --summary-agent agentctl\n  ldgr loop run --prompt prompts/loop-prompt.md --dry-run\n  ldgr loop run --prompt prompts/loop-prompt.md --agent-argv '[\"my-agent\"]'\n\nLoop run executes bounded cycles from pending work items. Each cycle is a fresh agent invocation that rehydrates from LDGR context. Use --until-empty to keep launching one fresh cycle at a time until no pending work remains or the loop blocks."
)]
pub struct LoopArgs {
    #[command(subcommand)]
    pub command: LoopCommand,
}

#[derive(Debug, Subcommand)]
pub enum LoopCommand {
    /// Render context into the prompt and run bounded loop sessions.
    Run(LoopRunArgs),
}

#[derive(Debug, Args)]
pub struct LoopRunArgs {
    /// Editable prompt document used as the model system prompt template.
    #[arg(long, conflicts_with_all = ["prompt_slug", "bundle"])]
    pub prompt: Option<PathBuf>,

    /// Stored active prompt slug to render without reading an external prompt file.
    #[arg(long, conflicts_with_all = ["prompt", "bundle"])]
    pub prompt_slug: Option<String>,

    /// Sealed bundle slug to render without reading loose external prompt files.
    #[arg(long, conflicts_with_all = ["prompt", "prompt_slug"])]
    pub bundle: Option<String>,

    /// Prompt role to select when --bundle contains multiple prompts.
    #[arg(long, requires = "bundle")]
    pub prompt_role: Option<String>,

    /// Built-in agent preset. Values: agentctl. Use --agent-argv for custom commands.
    #[arg(long, value_enum)]
    pub agent: Option<CliLoopAgent>,

    /// Agent command argv as JSON array. The rendered prompt is written to stdin.
    #[arg(long)]
    pub agent_argv: Option<String>,

    /// Fresh audit command argv as JSON array for project-completion requests.
    #[arg(long)]
    pub audit_argv: Option<String>,

    /// Built-in post-run summarizer preset. Values: agentctl. Runs once after each completed worker cycle.
    #[arg(long, value_enum)]
    pub summary_agent: Option<CliLoopAgent>,

    /// Post-run summarizer command argv as JSON array. The summary prompt is written to stdin.
    #[arg(long)]
    pub summary_argv: Option<String>,

    /// Append post-run summaries to this markdown log.
    #[arg(long, default_value = ".ldgr/logs/loop-summary.md")]
    pub summary_log: PathBuf,

    /// Request whole-project completion handling with a fresh external audit first.
    #[arg(long)]
    pub project_complete_requested: bool,

    /// Render and persist artifacts without spawning agent/audit commands.
    #[arg(long)]
    pub dry_run: bool,

    /// Tee autonomous agent stdout/stderr to this terminal while still recording the output artifact.
    #[arg(long)]
    pub stream_agent_output: bool,

    /// Maximum seconds to wait for each spawned agent process. Zero disables the wall-clock timeout.
    #[arg(long, default_value_t = 0, value_parser = clap::value_parser!(u64))]
    pub agent_timeout_seconds: u64,

    /// Maximum number of loop sessions to run before returning. Ignored when --until-empty is set.
    #[arg(long, default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
    pub max_iterations: u32,

    /// Keep launching fresh single-agent loop cycles until no pending work remains or the loop blocks.
    #[arg(long)]
    pub until_empty: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CliLoopAgent {
    Agentctl,
}
