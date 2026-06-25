pub mod args;
pub mod commands;
pub(crate) mod render;

use std::ffi::OsString;
use std::path::PathBuf;

use anyhow::bail;
use clap::{error::ErrorKind, CommandFactory, Parser, Subcommand};

use crate::store::open_store;

use args::*;

pub const DEFAULT_DB_PATH: &str = ".ldgr/ldgr.db";
pub const DEFAULT_ARTIFACT_ROOT: &str = ".ldgr/artifacts";
pub const INIT_PROJECT_SETUP_PROMPT: &str =
    include_str!("../../prompts/ldgr-init-project-setup.md");
pub(crate) const CLI_DEFAULT_HELP_SECTIONS: &str = r#"Core loop:
  work create <slug> --title <title> --description <description>
  work edit <slug> --description <corrected-description>
  work status set <slug> held --reason <why>
  next
  run start <work-slug> --command <what-ran>
  observation add <run-id> --body <what-changed-or-was-learned>
  artifact add <run-id> --path <file> --description <why-it-matters>
  artifact show <artifact-id>
  validation record <run-id> --outcome <pass|fail|error|skipped> --rationale <why-if-skipped>
  decision record <work-slug> --outcome continue --rationale <why> --next-slug <slug> --next-title <title> --next-description <description>
  status
  context --brief
  context

Autonomous loop:
  loop run --prompt prompts/loop-prompt.md --agent codex

Default help shows the day-one workflow. Run `ldgr --full` for the core command map.
"#;

pub(crate) const CLI_FULL_HELP_SECTIONS: &str = r#"Core command tree:
  init
  status
  context
    --brief
  web
  next
  work
    list
    show
    create
    edit
    status
      set
    delete
  run
    list
    show
    start
    finish
    close
  observation
    list
    add
  artifact
    list
    show
    add
  validation
    list
    record
  decision
    list
    record
  prompt
    create
    import
    update
    activate
  bundle
    create
    seal
  notice
    list
    add
    edit
    clear
  loop
    run

Research/readiness commands moved to `ldgr-research`:
  issue, blocker, fact, expectation, failure, milestone, target-profile,
  profile, coverage, readiness, tool, skill, evidence, and chat.

Effective workflow:
  1. Create one small work item with `ldgr work create ...`.
  2. Start one run with `ldgr run start ...`.
  3. Record observations and artifacts while the work is happening.
  4. Record a decision that either queues the next work item or stops for a stated reason.
  5. Start each agent handoff with `ldgr status`; expand to `ldgr context` only when needed.
"#;

#[derive(Debug, Parser)]
#[command(name = "ldgr")]
#[command(about = "A minimal durable investigation loop.")]
#[command(version)]
#[command(after_help = CLI_DEFAULT_HELP_SECTIONS)]
#[command(after_long_help = CLI_DEFAULT_HELP_SECTIONS)]
struct Cli {
    #[arg(long, help = "Print the core command map")]
    full: bool,

    #[arg(long, global = true, default_value = DEFAULT_DB_PATH)]
    db: PathBuf,

    #[arg(long, global = true, default_value = DEFAULT_ARTIFACT_ROOT)]
    artifact_root: PathBuf,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Initialize local SQLite storage and print the on-ramp.
    Init,
    /// Manage durable work items.
    Work(WorkArgs),
    /// Manage global observations and notifications for out-of-run steering.
    Notice(NoticeArgs),
    /// Start and finish investigation runs.
    Run(RunArgs),
    /// Attach observations to runs.
    Observation(ObservationArgs),
    /// Attach artifacts to runs.
    Artifact(ArtifactArgs),
    /// Record generic validation outcomes for runs.
    Validation(ValidationArgs),
    /// Record decisions and optional next work.
    Decision(DecisionArgs),
    /// Manage durable loop prompt records.
    Prompt(PromptArgs),
    /// Manage sealed prompt bundles.
    Bundle(BundleArgs),
    /// Print the compact agent-first status summary.
    Status(StatusArgs),
    /// Print the operational cockpit.
    Context(ContextArgs),
    /// Serve the web cockpit UI.
    Web(WebArgs),
    /// Run the prompt-driven autonomous event loop runtime.
    Loop(LoopArgs),
    /// Print the next pending work item.
    Next,
}

pub fn run() -> anyhow::Result<()> {
    run_from(std::env::args_os())
}

pub fn run_from<I, T>(args: I) -> anyhow::Result<()>
where
    I: IntoIterator<Item = T>,
    T: Into<OsString>,
{
    let args = args.into_iter().map(Into::into).collect::<Vec<_>>();
    let cli = match Cli::try_parse_from(args.clone()) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.print()?;
            return Ok(());
        }
        Err(error) => {
            print_parse_error_with_help(error, args.into_iter().skip(1).collect())?;
            std::process::exit(2);
        }
    };
    handle_cli(cli)
}

pub fn command() -> clap::Command {
    Cli::command()
}

fn handle_cli(cli: Cli) -> anyhow::Result<()> {
    if cli.full {
        print!("{}", CLI_FULL_HELP_SECTIONS);
        return Ok(());
    }
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };
    match command {
        Command::Init => commands::ops::handle_init(&cli.db, &cli.artifact_root),
        Command::Work(args) => commands::work::handle_work(&open_store(&cli.db)?, args),
        Command::Notice(args) => commands::work::handle_notice(&open_store(&cli.db)?, args),
        Command::Run(args) => commands::runs::handle_run(&open_store(&cli.db)?, args),
        Command::Observation(args) => {
            commands::runs::handle_observation(&open_store(&cli.db)?, args)
        }
        Command::Artifact(args) => {
            commands::runs::handle_artifact(&open_store(&cli.db)?, &cli.artifact_root, args)
        }
        Command::Validation(args) => commands::runs::handle_validation(&open_store(&cli.db)?, args),
        Command::Decision(args) => commands::audit::handle_decision(&open_store(&cli.db)?, args),
        Command::Prompt(args) => commands::prompts::handle_prompt(&open_store(&cli.db)?, args),
        Command::Bundle(args) => commands::prompts::handle_bundle(&open_store(&cli.db)?, args),
        Command::Status(args) => commands::ops::handle_status(&open_store(&cli.db)?, args),
        Command::Context(args) => commands::ops::handle_context(&open_store(&cli.db)?, args),
        Command::Web(args) => commands::ops::handle_web(&cli.db, &cli.artifact_root, args),
        Command::Loop(args) => {
            commands::ops::handle_loop(&open_store(&cli.db)?, &cli.artifact_root, args)
        }
        Command::Next => commands::work::handle_next(&open_store(&cli.db)?),
    }
}

fn print_parse_error_with_help(error: clap::Error, args: Vec<OsString>) -> anyhow::Result<()> {
    error.print()?;
    eprintln!();
    let mut command = last_parsable_command(args);
    command.print_long_help()?;
    eprintln!();
    Ok(())
}

fn last_parsable_command(args: Vec<OsString>) -> clap::Command {
    let mut command = Cli::command();
    let mut index = 0;
    while index < args.len() {
        let Some(token) = args[index].to_str() else {
            break;
        };
        if token == "--db" || token == "--artifact-root" {
            index += 2;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        let Some(next) = command.find_subcommand(token).cloned() else {
            break;
        };
        command = next;
        index += 1;
    }
    command
}

pub(crate) fn checked_limit(limit: i64) -> anyhow::Result<i64> {
    if limit < 1 {
        bail!("--limit must be greater than zero");
    }
    Ok(limit)
}
