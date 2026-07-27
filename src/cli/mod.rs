pub mod args;
pub mod commands;
pub(crate) mod render;

use std::ffi::OsString;
use std::fs;
use std::io::Write;
use std::path::PathBuf;
use std::process::Command as ProcessCommand;

use anyhow::{bail, Context};
use clap::{
    error::{ContextKind as ClapContextKind, ContextValue as ClapContextValue, ErrorKind},
    CommandFactory, Parser, Subcommand,
};
use serde::{Deserialize, Serialize};

use crate::adapter_registry::{AdapterCommandNamespace, AdapterRegistry};
use crate::store::open_store;

use args::*;

pub const DEFAULT_DB_PATH: &str = ".ldgr/ldgr.db";
pub const DEFAULT_ARTIFACT_ROOT: &str = ".ldgr/artifacts";
const RERUN_RECEIPT_PATH: &str = ".ldgr/last-rerun.json";
pub const INIT_PROJECT_SETUP_PROMPT: &str =
    include_str!("../../prompts/ldgr-init-project-setup.md");
pub(crate) const CLI_DEFAULT_HELP_SECTIONS: &str = r#"Core loop:
  work create <slug> --title <title> --description <description>
  work edit <slug> --description <corrected-description>
  work status set <slug> held --reason <why>
  next
  run start <work-slug> --command <what-ran>
  observe <run-id-or-work-slug> --body <what-changed-or-was-learned>
  observation add <run-id-or-work-slug> --body <what-changed-or-was-learned>
  artifact add <run-id-or-work-slug> --path <file> --description <why-it-matters>
  artifact show <artifact-id>
  validation record <run-id-or-work-slug> --outcome <pass|fail|error|skipped> --rationale <why-if-skipped>
  decision record <work-slug> --outcome continue --rationale <why> --next-slug <slug> --next-title <title> --next-description <description>
  status
  schema doctor [--json]
  migrate [--json]
  context --brief
  context
  rerun                         # execute the last saved non-destructive correction

Autonomous loop:
  loop run --prompt prompts/loop-prompt.md --agent agentctl

Adapters:
  adapter install              # selection menu
  adapter install list
  adapter install <slug>
  adapter list
  adapter show <slug-or-alias>
  <adapter-namespace> <args...>    # dynamically dispatched from installed adapter.toml

Default help shows the day-one workflow. Run `ldgr --full` for the core command map.
"#;

pub(crate) const CLI_FULL_HELP_SECTIONS: &str = r#"Core command tree:
  init
  migrate
  status
  context
    --brief
  web
  next
  rerun
  work
    list
    show
    create
    edit
    import
    export
    status
      set
    delete
  run
    list
    show
    start
    finish
    close
  observation (alias: observe)
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
  adapter
    install
      list | <slug>
    list
    show
    dispatch
  telemetry
    status
    enable
    disable
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
    /// Install LDGR harness integrations and record ~/.ldgr config.
    Install(InstallArgs),
    /// Manage durable work items.
    #[command(alias = "works", alias = "work-items")]
    Work(WorkArgs),
    /// Manage global observations and notifications for out-of-run steering.
    #[command(alias = "notices")]
    Notice(NoticeArgs),
    /// Start and finish investigation runs.
    #[command(alias = "runs")]
    Run(RunArgs),
    /// Attach observations to runs.
    #[command(visible_alias = "observe", alias = "observations")]
    Observation(ObservationArgs),
    /// Attach artifacts to runs.
    #[command(alias = "artifacts")]
    Artifact(ArtifactArgs),
    /// Record generic validation outcomes for runs.
    #[command(alias = "validations")]
    Validation(ValidationArgs),
    /// Record decisions and optional next work.
    #[command(alias = "decisions")]
    Decision(DecisionArgs),
    /// Manage durable loop prompt records.
    #[command(alias = "prompts")]
    Prompt(PromptArgs),
    /// Manage sealed prompt bundles.
    #[command(alias = "bundles")]
    Bundle(BundleArgs),
    /// Print the compact agent-first status summary.
    Status(StatusArgs),
    /// Inspect the unified database contract and recovery state.
    Schema(SchemaArgs),
    /// Safely migrate a recognized older project database using LDGR Core.
    Migrate(MigrateArgs),
    /// Print the workflow an agent should follow on this project.
    Workflow(WorkflowArgs),
    /// Show or change LDGR configuration.
    Config(ConfigArgs),
    /// Print the operational cockpit.
    Context(ContextArgs),
    /// Serve the web cockpit UI.
    Web(WebArgs),
    /// Run the prompt-driven autonomous event loop runtime.
    Loop(LoopArgs),
    /// Discover installed adapter manifests and command metadata.
    #[command(alias = "adapters")]
    Adapter(AdapterArgs),
    /// Control opt-in numerical state-sequence collection.
    Telemetry(TelemetryArgs),
    /// Print the next pending work item.
    Next(NextArgs),
    /// Execute the last saved non-destructive parse correction.
    Rerun,
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
    let args = normalize_command_aliases(args);
    let cli = match Cli::try_parse_from(args.clone()) {
        Ok(cli) => cli,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            error.print()?;
            if matches!(error.kind(), ErrorKind::DisplayHelp)
                && should_print_adapter_help_for_display_help(&args)
            {
                commands::ops::print_available_adapter_catalog();
                print_dynamic_adapter_help();
            }
            return Ok(());
        }
        Err(error) => {
            if try_dispatch_adapter_namespace(&args)? {
                return Ok(());
            }
            maybe_print_adapter_namespace_hint(&args);
            maybe_print_adapter_command_hint(&args);
            print_parse_error_with_help(error, &args)?;
            std::process::exit(2);
        }
    };
    handle_cli(cli)
}

pub fn command() -> clap::Command {
    Cli::command()
}

fn normalize_command_aliases(mut args: Vec<OsString>) -> Vec<OsString> {
    normalize_long_option_names(&mut args);
    let Some(command_index) = first_command_arg_index(&args) else {
        return args;
    };

    // Command identifiers are ASCII and canonical. Exact aliases, harmless
    // case differences, and snake_case-for-kebab-case variants can therefore
    // be canonicalized without guessing or introducing ambiguity.
    let mut command = Cli::command();
    let mut index = command_index;
    loop {
        let Some(token) = args.get(index).and_then(|argument| argument.to_str()) else {
            break;
        };
        let comparable = token.replace('_', "-");
        let matches = command
            .get_subcommands()
            .filter(|subcommand| {
                subcommand.get_name().eq_ignore_ascii_case(&comparable)
                    || subcommand
                        .get_all_aliases()
                        .any(|alias| alias.eq_ignore_ascii_case(&comparable))
            })
            .collect::<Vec<_>>();
        let [matched] = matches.as_slice() else {
            break;
        };
        let canonical = matched.get_name().to_owned();
        let next_command = (*matched).clone();
        args[index] = OsString::from(canonical);
        command = next_command;

        let next_index = index + 1;
        if next_index >= args.len() || command.get_subcommands().next().is_none() {
            break;
        }
        index = next_index;
    }

    let Some(command) = args[command_index].to_str() else {
        return args;
    };
    if command != "observation" {
        return args;
    }
    args[command_index] = OsString::from("observation");
    let next_token = args.get(command_index + 1).and_then(|arg| arg.to_str());
    if matches!(next_token, Some("add" | "list" | "--help" | "-h") | None) {
        return args;
    }
    args.insert(command_index + 1, OsString::from("add"));
    args
}

fn normalize_long_option_names(args: &mut [OsString]) {
    let mut known = vec!["help".to_owned(), "version".to_owned()];
    collect_long_option_names(&Cli::command(), &mut known);
    for argument in args.iter_mut().skip(1) {
        let Some(raw) = argument.to_str() else {
            continue;
        };
        let Some(long) = raw.strip_prefix("--") else {
            continue;
        };
        let (name, attached_value) = long
            .split_once('=')
            .map(|(name, value)| (name, Some(value)))
            .unwrap_or((long, None));
        let comparable = name.replace('_', "-");
        let matches = known
            .iter()
            .filter(|candidate| candidate.eq_ignore_ascii_case(&comparable))
            .collect::<Vec<_>>();
        let [canonical] = matches.as_slice() else {
            continue;
        };
        *argument = OsString::from(match attached_value {
            Some(value) => format!("--{canonical}={value}"),
            None => format!("--{canonical}"),
        });
    }
}

fn collect_long_option_names(command: &clap::Command, names: &mut Vec<String>) {
    for argument in command.get_arguments() {
        if let Some(long) = argument.get_long() {
            if !names.iter().any(|known| known == long) {
                names.push(long.to_owned());
            }
        }
    }
    for subcommand in command.get_subcommands() {
        collect_long_option_names(subcommand, names);
    }
}

fn first_command_arg_index(args: &[OsString]) -> Option<usize> {
    let mut index = 1;
    while index < args.len() {
        let token = args[index].to_str()?;
        if token == "--db" || token == "--artifact-root" {
            index += 2;
            continue;
        }
        if token.starts_with("--db=") || token.starts_with("--artifact-root=") {
            index += 1;
            continue;
        }
        if token == "--full" {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        return Some(index);
    }
    None
}

fn handle_cli(cli: Cli) -> anyhow::Result<()> {
    if cli.full {
        print!("{}", CLI_FULL_HELP_SECTIONS);
        return Ok(());
    }
    let Some(command) = cli.command else {
        Cli::command().print_help()?;
        println!();
        commands::ops::print_available_adapter_catalog();
        print_dynamic_adapter_help();
        return Ok(());
    };
    match command {
        Command::Init => commands::ops::handle_init(&cli.db, &cli.artifact_root),
        Command::Install(args) => commands::ops::handle_install(args),
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
        Command::Status(args) => {
            let (connection, migration) = crate::store::open_store_with_migration_info(&cli.db)?;
            commands::ops::print_migration_notice(migration.as_ref());
            commands::ops::handle_status(&connection, &cli.artifact_root, args)
        }
        Command::Schema(args) => commands::ops::handle_schema(&cli.db, args),
        Command::Migrate(args) => commands::ops::handle_migrate(&cli.db, args),
        Command::Workflow(args) => commands::ops::handle_workflow(args),
        Command::Config(args) => commands::ops::handle_config(args),
        Command::Context(args) => {
            let (connection, migration) = crate::store::open_store_with_migration_info(&cli.db)?;
            commands::ops::print_migration_notice(migration.as_ref());
            commands::ops::handle_context(&connection, &cli.artifact_root, args)
        }
        Command::Web(args) => commands::ops::handle_web(&cli.db, &cli.artifact_root, args),
        Command::Loop(args) => {
            commands::ops::handle_loop(&open_store(&cli.db)?, &cli.artifact_root, args)
        }
        Command::Adapter(args) => commands::adapters::handle_adapter(args),
        Command::Telemetry(args) => commands::ops::handle_telemetry(args),
        Command::Next(args) => commands::work::handle_next(&open_store(&cli.db)?, args),
        Command::Rerun => handle_rerun(),
    }
}

fn should_print_adapter_help_for_display_help(args: &[OsString]) -> bool {
    let mut index = 1;
    while index < args.len() {
        let Some(token) = args[index].to_str() else {
            index += 1;
            continue;
        };
        if matches!(token, "--help" | "-h") {
            return true;
        }
        if token == "--db" || token == "--artifact-root" {
            index += 2;
            continue;
        }
        if token.starts_with("--db=") || token.starts_with("--artifact-root=") {
            index += 1;
            continue;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        return token == "adapter";
    }
    true
}

fn try_dispatch_adapter_namespace(args: &[OsString]) -> anyhow::Result<bool> {
    let Some(mut request) = adapter_namespace_request(args) else {
        return Ok(false);
    };
    let registry = AdapterRegistry::discover();
    let normalized = request
        .namespace
        .trim()
        .to_ascii_lowercase()
        .replace('_', "-");
    let Some(command) = registry
        .resolve_namespace(&request.namespace)
        .or_else(|| registry.resolve_namespace(&normalized))
    else {
        return Ok(false);
    };
    request.namespace = command.namespace.clone();
    dispatch_adapter_namespace(command, request)?;
    Ok(true)
}

struct AdapterNamespaceRequest {
    db: PathBuf,
    artifact_root: PathBuf,
    namespace: String,
    remaining: Vec<OsString>,
}

fn adapter_namespace_request(args: &[OsString]) -> Option<AdapterNamespaceRequest> {
    let mut db = PathBuf::from(DEFAULT_DB_PATH);
    let mut artifact_root = PathBuf::from(DEFAULT_ARTIFACT_ROOT);
    let mut index = 1;
    while index < args.len() {
        let token = args[index].to_str()?;
        if token == "--db" {
            index += 1;
            db = PathBuf::from(args.get(index)?);
            index += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--db=") {
            db = PathBuf::from(value);
            index += 1;
            continue;
        }
        if token == "--artifact-root" {
            index += 1;
            artifact_root = PathBuf::from(args.get(index)?);
            index += 1;
            continue;
        }
        if let Some(value) = token.strip_prefix("--artifact-root=") {
            artifact_root = PathBuf::from(value);
            index += 1;
            continue;
        }
        if token == "--full" {
            return None;
        }
        if token.starts_with('-') {
            index += 1;
            continue;
        }
        return Some(AdapterNamespaceRequest {
            db,
            artifact_root,
            namespace: token.to_owned(),
            remaining: args.iter().skip(index + 1).cloned().collect(),
        });
    }
    None
}

fn dispatch_adapter_namespace(
    command: &AdapterCommandNamespace,
    request: AdapterNamespaceRequest,
) -> anyhow::Result<()> {
    if command.argv.is_empty() {
        bail!("adapter namespace `{}` has empty argv", command.namespace);
    }
    if let Some(contract) = &command.adapter_contract_json {
        let help_only = request
            .remaining
            .iter()
            .any(|argument| matches!(argument.to_str(), Some("--help" | "-h")));
        if request.db.is_file() {
            crate::store::open_store_for_adapter(&request.db, contract).with_context(|| {
                format!(
                    "adapter {} is incompatible with the active database; run the current `ldgr` Core command first",
                    command.adapter_slug
                )
            })?;
        } else if !help_only {
            bail!(
                "adapter {} requires a Core-initialized database at {}; run `ldgr init` first",
                command.adapter_slug,
                request.db.display()
            );
        }
    }
    let working_dir = std::env::current_dir().context("failed to resolve current directory")?;
    let mut process = ProcessCommand::new(&command.argv[0]);
    process
        .args(&command.argv[1..])
        .args(request.remaining)
        .env("LDGR_DB", &request.db)
        .env("LDGR_ARTIFACT_ROOT", &request.artifact_root)
        .env("LDGR_WORKING_DIR", working_dir)
        .env("LDGR_ADAPTER_SLUG", &command.adapter_slug)
        .env("LDGR_ADAPTER_NAMESPACE", &request.namespace);
    if let Some(hash) = &command.database_contract_hash {
        process.env("LDGR_DATABASE_CONTRACT_HASH", hash);
    }
    if let Some(version) = command.core_schema_version {
        process.env("LDGR_CORE_SCHEMA_VERSION", version.to_string());
    }
    if let Some(version) = command.component_schema_version {
        process.env("LDGR_ADAPTER_SCHEMA_VERSION", version.to_string());
    }
    let status = process.status().with_context(|| {
        format!(
            "failed to execute adapter `{}` namespace `{}` command `{}`",
            command.adapter_slug, command.namespace, command.argv[0]
        )
    })?;
    if !status.success() {
        std::process::exit(status.code().unwrap_or(1));
    }
    Ok(())
}

fn print_dynamic_adapter_help() {
    let registry = AdapterRegistry::discover();
    if registry.adapters.is_empty() {
        return;
    }
    println!();
    println!("Installed adapter control surface:");
    for adapter in &registry.adapters {
        println!("  {} — {}", adapter.slug, adapter.title);
        for namespace in &adapter.command_namespaces {
            let description = namespace
                .summary
                .as_ref()
                .or(namespace.description.as_ref())
                .map(|text| format!(" — {text}"))
                .unwrap_or_default();
            println!("    ldgr {} <args...>{}", namespace.namespace, description);
        }
        for profile in &adapter.target_profiles {
            println!("    profile {} — {}", profile.slug, profile.title);
        }
    }
}

fn maybe_print_adapter_command_hint(args: &[OsString]) {
    let mut tokens = args.iter().skip(1);
    while let Some(token) = tokens.next() {
        let Some(token) = token.to_str() else {
            return;
        };
        if token == "--db" || token == "--artifact-root" {
            let _ = tokens.next();
            continue;
        }
        if token.starts_with('-') {
            continue;
        }
        commands::adapters::print_adapter_command_hint(token);
        return;
    }
}

fn maybe_print_adapter_namespace_hint(args: &[OsString]) {
    let Some(command_index) = first_command_arg_index(args) else {
        return;
    };
    let Some(input) = args
        .get(command_index)
        .and_then(|argument| argument.to_str())
    else {
        return;
    };
    let input = input.to_ascii_lowercase().replace('_', "-");
    let registry = AdapterRegistry::discover();
    let mut scored = Vec::new();
    for adapter in &registry.adapters {
        for namespace in &adapter.command_namespaces {
            let distance = std::iter::once(namespace.namespace.as_str())
                .chain(namespace.aliases.iter().map(String::as_str))
                .map(|candidate| edit_distance(&input, candidate))
                .min()
                .unwrap_or(usize::MAX);
            if distance <= typo_suggestion_threshold(input.len().max(namespace.namespace.len())) {
                scored.push((distance, namespace.namespace.clone()));
            }
        }
    }
    scored.sort();
    scored.dedup();
    let Some(best_distance) = scored.first().map(|(distance, _)| *distance) else {
        return;
    };
    let candidates = scored
        .into_iter()
        .filter(|(distance, _)| *distance == best_distance)
        .map(|(_, namespace)| namespace)
        .collect::<Vec<_>>();
    eprintln!();
    if let [candidate] = candidates.as_slice() {
        let mut corrected = args.to_vec();
        corrected[command_index] = OsString::from(candidate);
        eprintln!("Unknown adapter namespace `{input}`. Did you mean `{candidate}`?");
        if let Some(rendered) = render_rerun_command(&corrected) {
            eprintln!("Suggested rerun (not executed):");
            eprintln!("  {rendered}");
            print_rerun_instruction(&corrected);
        }
    } else {
        eprintln!("Adapter namespace `{input}` is ambiguous.");
        eprintln!("Possible namespaces: {}", candidates.join(", "));
    }
}

fn typo_suggestion_threshold(len: usize) -> usize {
    match len {
        0..=4 => 1,
        5..=8 => 2,
        _ => 3,
    }
}

fn edit_distance(left: &str, right: &str) -> usize {
    let left = left.chars().collect::<Vec<_>>();
    let right = right.chars().collect::<Vec<_>>();
    let mut previous = (0..=right.len()).collect::<Vec<_>>();
    let mut current = vec![0; right.len() + 1];
    for (i, left_ch) in left.iter().enumerate() {
        current[0] = i + 1;
        for (j, right_ch) in right.iter().enumerate() {
            let substitution = previous[j] + usize::from(left_ch != right_ch);
            let insertion = current[j] + 1;
            let deletion = previous[j + 1] + 1;
            current[j + 1] = substitution.min(insertion).min(deletion);
        }
        std::mem::swap(&mut previous, &mut current);
    }
    previous[right.len()]
}

fn print_parse_error_with_help(error: clap::Error, args: &[OsString]) -> anyhow::Result<()> {
    error.print()?;
    print_preserved_intent_suggestion(&error, args);
    eprintln!();
    let mut command = last_parsable_command(args.iter().skip(1).cloned().collect());
    command.print_long_help()?;
    eprintln!();
    Ok(())
}

fn print_preserved_intent_suggestion(error: &clap::Error, args: &[OsString]) {
    let correction = [
        (
            ClapContextKind::InvalidSubcommand,
            ClapContextKind::SuggestedSubcommand,
        ),
        (ClapContextKind::InvalidArg, ClapContextKind::SuggestedArg),
        (
            ClapContextKind::InvalidValue,
            ClapContextKind::SuggestedValue,
        ),
    ]
    .into_iter()
    .find_map(|(invalid_kind, suggestion_kind)| {
        let ClapContextValue::String(invalid) = error.get(invalid_kind)? else {
            return None;
        };
        let suggested = match error.get(suggestion_kind)? {
            ClapContextValue::String(suggested) => suggested.clone(),
            ClapContextValue::Strings(suggested) if suggested.len() == 1 => suggested[0].clone(),
            ClapContextValue::Strings(suggested)
                if suggestion_kind == ClapContextKind::SuggestedSubcommand =>
            {
                canonical_subcommand_suggestion(args, suggested)?
            }
            _ => return None,
        };
        Some((invalid.clone(), suggested))
    });
    let Some((invalid, suggested)) = correction else {
        return;
    };
    let Some(index) = args
        .iter()
        .enumerate()
        .skip(1)
        .find_map(|(index, argument)| {
            (argument.to_str() == Some(invalid.as_str())).then_some(index)
        })
    else {
        return;
    };
    let mut corrected = args.to_vec();
    corrected[index] = OsString::from(suggested);
    let Some(rendered) = render_rerun_command(&corrected) else {
        return;
    };
    eprintln!();
    eprintln!("Suggested rerun (not executed):");
    eprintln!("  {rendered}");
    print_rerun_instruction(&corrected);
}

fn canonical_subcommand_suggestion(args: &[OsString], suggestions: &[String]) -> Option<String> {
    let command = last_parsable_command(args.iter().skip(1).cloned().collect());
    let canonical = suggestions
        .iter()
        .filter_map(|suggestion| command.find_subcommand(suggestion))
        .map(|subcommand| subcommand.get_name().to_owned())
        .collect::<std::collections::BTreeSet<_>>();
    let mut canonical = canonical.into_iter();
    let suggestion = canonical.next()?;
    canonical.next().is_none().then_some(suggestion)
}

#[derive(Debug, Serialize, Deserialize)]
struct RerunReceipt {
    schema_version: u32,
    argv: Vec<String>,
}

fn print_rerun_instruction(corrected: &[OsString]) {
    if !is_rerunnable_correction(corrected) {
        eprintln!("This correction is incomplete and was not saved for `ldgr rerun`.");
        return;
    }
    if is_destructive_rerun(corrected) {
        eprintln!("This correction is destructive and was not saved for `ldgr rerun`.");
        return;
    }
    match save_rerun_receipt(corrected) {
        Ok(()) => eprintln!("Use `ldgr rerun` to execute this command."),
        Err(error) => eprintln!("Could not save `ldgr rerun` command: {error:#}"),
    }
}

fn is_rerunnable_correction(args: &[OsString]) -> bool {
    match Cli::try_parse_from(args.to_vec()) {
        Ok(_) => true,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            true
        }
        Err(_) => {
            let Some(request) = adapter_namespace_request(args) else {
                return false;
            };
            let normalized = request
                .namespace
                .trim()
                .to_ascii_lowercase()
                .replace('_', "-");
            let registry = AdapterRegistry::discover();
            registry.resolve_namespace(&request.namespace).is_some()
                || registry.resolve_namespace(&normalized).is_some()
        }
    }
}

fn is_destructive_rerun(args: &[OsString]) -> bool {
    matches!(
        Cli::try_parse_from(args.to_vec())
            .ok()
            .and_then(|cli| cli.command),
        Some(Command::Work(WorkArgs {
            command: WorkCommand::Delete(_),
        })) | Some(Command::Adapter(AdapterArgs {
            command: AdapterCommand::Uninstall(_),
        }))
    )
}

fn save_rerun_receipt(args: &[OsString]) -> anyhow::Result<()> {
    let argv = args
        .iter()
        .skip(1)
        .map(|argument| {
            argument
                .to_str()
                .map(str::to_owned)
                .context("rerun arguments must be valid UTF-8")
        })
        .collect::<anyhow::Result<Vec<_>>>()?;
    let receipt = RerunReceipt {
        schema_version: 1,
        argv,
    };
    let path = PathBuf::from(RERUN_RECEIPT_PATH);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create {}", parent.display()))?;
    }
    write_rerun_receipt_file(&path, &serde_json::to_vec_pretty(&receipt)?)
}

fn write_rerun_receipt_file(path: &std::path::Path, content: &[u8]) -> anyhow::Result<()> {
    if fs::symlink_metadata(path).is_ok_and(|metadata| metadata.file_type().is_symlink()) {
        bail!(
            "refusing to write rerun receipt through symlink {}",
            path.display()
        );
    }
    let mut options = fs::OpenOptions::new();
    options.create(true).truncate(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let mut file = options
        .open(path)
        .with_context(|| format!("failed to open {}", path.display()))?;
    if let Err(error) = restrict_rerun_receipt_permissions(path) {
        drop(file);
        let _ = fs::remove_file(path);
        return Err(error);
    }
    file.write_all(content)
        .with_context(|| format!("failed to write {}", path.display()))?;
    file.flush()
        .with_context(|| format!("failed to flush {}", path.display()))
}

#[cfg(unix)]
fn restrict_rerun_receipt_permissions(path: &std::path::Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600))
        .with_context(|| format!("failed to restrict {} permissions", path.display()))
}

#[cfg(not(unix))]
fn restrict_rerun_receipt_permissions(_path: &std::path::Path) -> anyhow::Result<()> {
    Ok(())
}

fn handle_rerun() -> anyhow::Result<()> {
    let path = PathBuf::from(RERUN_RECEIPT_PATH);
    let text = fs::read_to_string(&path).with_context(|| {
        format!(
            "no saved rerun command at {}; rerun is available only after LDGR suggests a non-destructive correction",
            path.display()
        )
    })?;
    let receipt: RerunReceipt = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse {}", path.display()))?;
    if receipt.schema_version != 1 {
        bail!(
            "unsupported rerun receipt schema {}; expected 1",
            receipt.schema_version
        );
    }
    if receipt.argv.is_empty() {
        bail!("saved rerun command is empty");
    }

    let mut rerun_args = vec![OsString::from("ldgr")];
    rerun_args.extend(receipt.argv.iter().map(OsString::from));
    let command_index = first_command_arg_index(&rerun_args)
        .context("saved rerun command does not contain a command")?;
    if rerun_args[command_index] == "rerun" {
        bail!("saved rerun command cannot invoke `ldgr rerun`");
    }
    let rendered = render_rerun_command(&rerun_args).context("failed to render rerun command")?;
    println!("rerunning: {rendered}");

    fs::remove_file(&path).with_context(|| format!("failed to consume {}", path.display()))?;
    match run_from(rerun_args) {
        Ok(()) => Ok(()),
        Err(error) => {
            let restore_result = write_rerun_receipt_file(&path, text.as_bytes())
                .with_context(|| format!("failed to restore {}", path.display()));
            if let Err(restore_error) = restore_result {
                return Err(error).context(format!(
                    "rerun failed and its receipt could not be restored: {restore_error:#}"
                ));
            }
            Err(error)
        }
    }
}

fn render_rerun_command(args: &[OsString]) -> Option<String> {
    let mut rendered = vec!["ldgr".to_owned()];
    for argument in args.iter().skip(1) {
        rendered.push(shell_quote(argument.to_str()?));
    }
    Some(rendered.join(" "))
}

fn shell_quote(argument: &str) -> String {
    if !argument.is_empty()
        && argument
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || "-._/:=@,+".contains(character))
    {
        return argument.to_owned();
    }
    format!("'{}'", argument.replace('\'', "'\\''"))
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
    if limit < 0 {
        bail!("--limit must not be negative");
    }
    Ok(limit)
}
