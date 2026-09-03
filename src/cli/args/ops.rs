use std::path::PathBuf;

use clap::{Args, Subcommand, ValueEnum};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum HarnessKind {
    Pi,
    Codex,
    Claude,
    #[value(alias = "open-claw", alias = "open_claw")]
    Openclaw,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TelemetryInstallChoice {
    #[value(alias = "enabled", alias = "on")]
    Enable,
    #[value(alias = "disabled", alias = "off")]
    Disable,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr install\n  ldgr install --harness pi --harness claude --adapter conduct --yes --telemetry disable\n  ldgr install --adapter research --store ./store --yes\n  ldgr install --yes --no-agentctl\n  ldgr install adapter code --yes\n\nInteractive installs disclose both telemetry programs: privacy-minimized anonymous construction telemetry defaults to enabled and can be opted out, while detailed model-sanitized LDGR work donation defaults to disabled and requires opt-in. Non-interactive installs can opt out of anonymous telemetry with --telemetry disable or `ldgr telemetry disable`; experience donation is controlled separately by `ldgr telemetry donation enable`. Without --harness, the installer asks interactively and defaults to Pi. Multiple harnesses may be selected. In interactive mode the installer also offers adapter bundle selection. --store installs selected signed adapter releases without network access. The selected harness config is recorded in ~/.ldgr/config.toml with a legacy config.json compatibility mirror. agentctl is installed when missing unless --no-agentctl is passed."
)]
pub struct InstallArgs {
    #[command(subcommand)]
    pub command: Option<InstallCommand>,

    /// Harness to install LDGR integration into. Repeatable.
    #[arg(long, value_enum, ignore_case = true)]
    pub harness: Vec<HarnessKind>,

    /// Accept defaults and do not prompt. Defaults to Pi when --harness is omitted.
    #[arg(long)]
    pub yes: bool,

    /// Explicitly enable or disable numerical state-sequence collection.
    #[arg(long, value_enum, ignore_case = true)]
    pub telemetry: Option<TelemetryInstallChoice>,

    /// Do not install agentctl even if it is missing from PATH.
    #[arg(long)]
    pub no_agentctl: bool,

    /// How deep a requirements interview the agent should run: high, medium, low, or none.
    #[arg(long, value_name = "LEVEL")]
    pub interview_depth: Option<String>,

    /// Adapter bundle to install after harness setup. Repeatable.
    #[arg(long)]
    pub adapter: Vec<String>,

    /// Read requested adapter releases from this signed local release store.
    #[arg(long, value_name = "DIRECTORY")]
    pub store: Option<PathBuf>,
}

#[derive(Debug, Subcommand)]
pub enum InstallCommand {
    /// Install an open-source adapter bundle into ~/.ldgr/adapters/<adapter>.
    Adapter(InstallAdapterArgs),
}

#[derive(Debug, Args)]
pub struct InstallAdapterArgs {
    /// Adapter name, e.g. conduct, research, example, code, bench, explore, security.
    pub name: String,

    /// Monorepo root containing the adapter crate, or the adapter crate root itself.
    #[arg(long)]
    pub source_root: Option<PathBuf>,

    /// Exact install directory. Defaults to ~/.ldgr/adapters/<adapter>.
    #[arg(long)]
    pub install_root: Option<PathBuf>,

    #[arg(long)]
    pub version: Option<String>,

    #[arg(long)]
    pub prerelease: bool,

    #[arg(long)]
    pub offline: bool,

    /// Read the signed adapter release from this local release store.
    #[arg(long, value_name = "DIRECTORY", conflicts_with = "source_root")]
    pub store: Option<PathBuf>,

    /// Accept defaults and do not prompt.
    #[arg(long)]
    pub yes: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr context --brief\n  ldgr context --json\n\nContext is the expanded handoff view. Start with `ldgr status`; use context when you need deeper history. Before reading work, context diagnoses the central database, applies recognized Core migrations with a verified backup, and reports database_alignment in JSON. Unknown or unsafe states remain unchanged and return schema-doctor recovery details."
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
    after_help = "Examples:\n  ldgr status\n  ldgr status --program audit --priority P0\n  ldgr status --full\n  ldgr status --json\n\nStatus does not change work state. Before reading work, status diagnoses the central database, applies recognized Core migrations with a verified backup, and reports database_alignment in JSON. Unknown or unsafe states remain unchanged and return schema-doctor recovery details. Default output is scoped to actionable work; --full includes global history. To change work state, use `ldgr work status set <work> <status>`."
)]
pub struct StatusArgs {
    #[arg(long)]
    pub json: bool,

    /// Include global history, the last loop terminal state, and full next-item text.
    #[arg(long)]
    pub full: bool,

    /// Limit queue and next-item selection to one program.
    #[arg(long)]
    pub program: Option<String>,

    /// Limit queue and next-item selection to one priority label (for example P0 or high).
    #[arg(long)]
    pub priority: Option<String>,

    /// Number of recent records to include in the status summary.
    #[arg(long, default_value_t = 3)]
    pub recent: usize,

    /// Maximum characters for freeform status fields.
    #[arg(long, default_value_t = 240)]
    pub width: usize,
}

#[derive(Debug, Args)]
pub struct SchemaArgs {
    #[command(subcommand)]
    pub command: SchemaCommand,
}

#[derive(Debug, Subcommand)]
pub enum SchemaCommand {
    /// Inspect compatibility, pending migrations, components, and recovery without mutation.
    Doctor(SchemaDoctorArgs),
}

#[derive(Debug, Args)]
pub struct SchemaDoctorArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr migrate\n  ldgr migrate --json\n\nMigration is owned exclusively by LDGR Core. It creates and verifies a backup before changing a recognized older database. Use `ldgr schema doctor` first for a non-mutating preview."
)]
pub struct MigrateArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr workflow\n  ldgr workflow --json\n\nPrints the workflow this project expects an agent to follow. Installed adapters expose their own workflow through `ldgr <adapter> workflow`."
)]
pub struct WorkflowArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
#[command(
    after_help = "Examples:\n  ldgr config show\n  ldgr config set interview-depth low\n  ldgr config set updates.check never\n  ldgr config set updates.interval-hours 12\n  ldgr config set updates.channel prerelease\n  ldgr config set updates.include-adapters false\n  ldgr config set updates.notify false\n\nReads and writes canonical ~/.ldgr/config.toml plus the legacy config.json mirror. Unknown extensions are preserved."
)]
pub struct ConfigArgs {
    #[command(subcommand)]
    pub command: ConfigCommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Print the resolved LDGR configuration.
    Show(ConfigShowArgs),
    /// Set one configuration value.
    Set(ConfigSetArgs),
}

#[derive(Debug, Args)]
pub struct ConfigShowArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct ConfigSetArgs {
    /// Configuration key: interview-depth or an updates.* key shown in config --help.
    pub key: String,
    /// Value to set.
    pub value: String,
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

const LOOP_HELP: &str = "Examples:\n  ldgr loop run --prompt prompts/loop-prompt.md --agent agentctl\n  ldgr loop run --prompt-slug implementation-loop --until-empty\n  ldgr loop run --bundle release --prompt-role implementation-loop --detach\n\n`loop run` renders durable LDGR context into a selected prompt and executes bounded agent sessions. Run `ldgr loop run --help` for prompt sources, active-run resumption, iteration controls, artifacts, completion audits, and detailed examples.";

const LOOP_RUN_HELP: &str = r#"Execution model:
  * Choose exactly one prompt source: --prompt, --prompt-slug, or --bundle.
  * If a run is already active, the loop resumes that run and its work item instead
    of claiming another item. A running work item with no active run still requires
    a decision or lifecycle correction before new work can start.
  * With no active run, the loop atomically claims the next ready pending item.
  * Every cycle renders fresh LDGR context, invokes one fresh agent process, records
    prompt provenance and process output, and finishes the run if it is still active.

Agents and output:
  --agent agentctl is the default. Use --agent-argv with a JSON argv array for a
  custom process; the rendered prompt is sent on stdin. --stream-agent-output tees
  stdout/stderr while retaining full artifact files. --summary-agent or
  --summary-argv adds a separate one-shot summary and --summary-log selects its log.
  --agent-timeout-seconds 0 disables the timeout.

Iteration and background execution:
  The default is one cycle. --max-iterations N bounds repeated cycles;
  --until-empty continues until the queue drains, a subprocess fails, or the loop
  blocks. --detach launches the same loop in the background and writes stdout/stderr
  under .ldgr/logs. --dry-run renders and records artifacts without spawning agents.

Project completion:
  --project-complete-requested requires --audit-argv. The fresh audit runs before
  the worker, and both processes must succeed for a successful loop result.

Examples:
  ldgr loop run --prompt prompts/loop-prompt.md
  ldgr loop run --prompt-slug implementation-loop --agent agentctl
  ldgr loop run --bundle release --prompt-role implementation-loop --until-empty
  ldgr loop run --prompt prompts/loop-prompt.md --agent-argv '["my-agent","--batch"]'
  ldgr loop run --prompt prompts/loop-prompt.md --dry-run
  ldgr loop run --prompt prompts/loop-prompt.md --until-empty --summary-agent agentctl
  ldgr loop run --prompt prompts/loop-prompt.md --until-empty --detach
  ldgr loop run --prompt prompts/loop-prompt.md --project-complete-requested \
    --audit-argv '["my-auditor","--fresh"]'
"#;

#[derive(Debug, Args)]
#[command(after_help = LOOP_HELP)]
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
#[command(after_help = LOOP_RUN_HELP)]
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
    #[arg(long, value_enum, ignore_case = true)]
    pub agent: Option<CliLoopAgent>,

    /// Agent command argv as JSON array. The rendered prompt is written to stdin.
    #[arg(long)]
    pub agent_argv: Option<String>,

    /// Fresh audit command argv as JSON array for project-completion requests.
    #[arg(long)]
    pub audit_argv: Option<String>,

    /// Built-in post-run summarizer preset. Values: agentctl. Runs once after each completed worker cycle.
    #[arg(long, value_enum, ignore_case = true)]
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

    /// Run the loop in a detached background process with output under the LDGR logs directory.
    #[arg(long)]
    pub detach: bool,

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
    #[value(alias = "agent-ctl", alias = "agent_ctl")]
    Agentctl,
}
