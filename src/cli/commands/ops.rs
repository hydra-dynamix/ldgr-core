use std::path::Path;
use std::process::Command;
use std::time::Duration;

use anyhow::bail;

use crate::loop_runtime::{
    run_loop_once, LoopAgent, LoopPromptSource, LoopRuntimeOptions, LoopRuntimeOutcome,
    LoopRuntimeResult,
};
use crate::store::{init_store, read_context};
use crate::tool_runner::parse_argv_json;
use crate::web::{generate_control_token, serve, WebOptions};

use super::super::args::{
    CliLoopAgent, ContextArgs, LoopArgs, LoopCommand, LoopRunArgs, StatusArgs, WebArgs,
};
use super::super::render::brief_context::{
    brief_context, print_brief_context, BriefContextOptions,
};
use super::super::render::context::print_context;
use super::super::render::emit;
use super::super::render::text::print_loop_result;
use super::super::{CLI_DEFAULT_HELP_SECTIONS, INIT_PROJECT_SETUP_PROMPT};

pub fn handle_init(db: &Path, artifact_root: &Path) -> anyhow::Result<()> {
    let existing_database = db.exists();
    init_store(db, artifact_root)?;
    if existing_database {
        println!("opened existing {} (no data erased)", db.display());
    } else {
        println!("initialized {}", db.display());
    }
    print_init_project_setup_prompt();
    print_cli_hierarchy();
    Ok(())
}

pub fn handle_status(connection: &rusqlite::Connection, args: StatusArgs) -> anyhow::Result<()> {
    let context = read_context(connection)?;
    emit(
        args.json,
        &brief_context(&context, brief_options(args.recent, args.width)),
        print_brief_context,
    )
}

pub fn handle_context(connection: &rusqlite::Connection, args: ContextArgs) -> anyhow::Result<()> {
    let context = read_context(connection)?;
    if args.brief {
        let brief = brief_context(&context, brief_options(args.recent, args.width));
        return emit(args.json, &brief, print_brief_context);
    }
    emit(args.json, &context, print_context)
}

fn brief_options(recent: usize, width: usize) -> BriefContextOptions {
    BriefContextOptions {
        recent: recent.min(50),
        width: width.clamp(40, 2000),
    }
}

pub fn handle_web(db: &Path, artifact_root: &Path, args: WebArgs) -> anyhow::Result<()> {
    let control_token = args
        .control_token
        .clone()
        .or_else(|| std::env::var("LDGR_WEB_CONTROL_TOKEN").ok())
        .filter(|token| !token.trim().is_empty())
        .map(Ok)
        .unwrap_or_else(generate_control_token)?;
    serve(
        db,
        artifact_root,
        &args.host,
        args.port,
        WebOptions {
            unsafe_expose: args.unsafe_expose,
            control_token,
        },
    )?;
    Ok(())
}

pub fn handle_loop(
    connection: &rusqlite::Connection,
    artifact_root: &Path,
    args: LoopArgs,
) -> anyhow::Result<()> {
    match args.command {
        LoopCommand::Run(args) => {
            let agent = resolve_loop_agent(&args)?;
            let prompt = resolve_loop_prompt(connection, &args)?;
            let options = LoopRuntimeOptions {
                prompt,
                agent,
                audit_argv: args
                    .audit_argv
                    .as_deref()
                    .map(parse_argv_json)
                    .transpose()?,
                project_complete_requested: args.project_complete_requested,
                dry_run: args.dry_run,
                stream_agent_output: args.stream_agent_output,
                agent_timeout: Duration::from_secs(args.agent_timeout_seconds),
            };
            let mut completed_iterations = 0_u32;
            for iteration in 1..=args.max_iterations {
                match run_loop_once(connection, artifact_root, &options)? {
                    LoopRuntimeOutcome::Completed(result) => {
                        print_loop_result(&result);
                        completed_iterations += 1;
                        if loop_result_failed(&result, &options) {
                            if args.max_iterations > 1 {
                                println!(
                                    "Loop stopped after {completed_iterations} iteration(s) because a subprocess failed."
                                );
                            }
                            break;
                        }
                        if iteration == args.max_iterations && args.max_iterations > 1 {
                            println!(
                                "Loop stopped after reaching max_iterations={}.",
                                args.max_iterations
                            );
                        }
                    }
                    LoopRuntimeOutcome::BlockedByIntervention => {
                        println!("Loop is blocked by an intervention.");
                        break;
                    }
                    LoopRuntimeOutcome::BlockedByIncompleteCycle { work_slug } => {
                        println!(
                            "Loop is blocked by unfinished work item {work_slug}; record a decision or cancel it before starting next work."
                        );
                        break;
                    }
                    LoopRuntimeOutcome::NoPendingWork => {
                        if completed_iterations == 0 {
                            bail!("No pending work items remain; add a next work item or record a stop decision only when the project is complete.");
                        }
                        if args.max_iterations > 1 {
                            println!(
                                "Loop stopped after {completed_iterations} iteration(s); no pending work items remain."
                            );
                        }
                        break;
                    }
                }
            }
        }
    }
    Ok(())
}

fn print_init_project_setup_prompt() {
    println!();
    print!("{}", render_init_project_setup_prompt().trim_end());
    println!("\n");
}

fn render_init_project_setup_prompt() -> String {
    INIT_PROJECT_SETUP_PROMPT
        .replace("{{PWD}}", &current_directory_text())
        .replace("{{DEV_WALK}}", &dev_walk_text())
}

fn current_directory_text() -> String {
    std::env::current_dir()
        .map(|path| path.display().to_string())
        .unwrap_or_else(|error| format!("<failed to read current directory: {error}>"))
}

fn dev_walk_text() -> String {
    match Command::new("dev")
        .args(["walk", ".", "--stdout", "--no-content"])
        .output()
    {
        Ok(output) if output.status.success() => String::from_utf8_lossy(&output.stdout)
            .trim_end()
            .to_owned(),
        Ok(output) if String::from_utf8_lossy(&output.stderr).contains("--stdout") => {
            dev_walk_text_via_output_file()
        }
        Ok(output) => format!(
            "<dev walk . --stdout --no-content failed with status {}>\n{}{}",
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .trim_end()
        .to_owned(),
        Err(error) => format!("<failed to run dev walk . --stdout --no-content: {error}>"),
    }
}

fn dev_walk_text_via_output_file() -> String {
    let output_path = std::env::temp_dir().join(format!(
        "ldgr-init-dev-walk-{}-{}.md",
        std::process::id(),
        timestamp_nanos()
    ));
    let output_path_text = output_path.display().to_string();
    match Command::new("dev")
        .args(["walk", ".", "--no-content", "--output", &output_path_text])
        .output()
    {
        Ok(output) if output.status.success() => {
            let content = std::fs::read_to_string(&output_path).unwrap_or_else(|error| {
                format!(
                    "<failed to read dev walk output {}: {error}>",
                    output_path.display()
                )
            });
            let _ = std::fs::remove_file(&output_path);
            content.trim_end().to_owned()
        }
        Ok(output) => format!(
            "<dev walk . --no-content --output {} failed with status {}>\n{}{}",
            output_path.display(),
            output.status,
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        )
        .trim_end()
        .to_owned(),
        Err(error) => format!("<failed to run dev walk fallback: {error}>"),
    }
}

fn timestamp_nanos() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or(0)
}

fn print_cli_hierarchy() {
    print!("{CLI_DEFAULT_HELP_SECTIONS}");
    println!("Use `ldgr <command> --help` for flags, or `ldgr --full` for the core command map.");
}

fn resolve_loop_prompt(
    connection: &rusqlite::Connection,
    args: &LoopRunArgs,
) -> anyhow::Result<LoopPromptSource> {
    if let Some(prompt_path) = args.prompt.clone() {
        return Ok(LoopPromptSource::Path(prompt_path));
    }
    if let Some(slug) = args.prompt_slug.clone() {
        return Ok(LoopPromptSource::StoredPrompt { slug });
    }
    if let Some(slug) = args.bundle.clone() {
        return Ok(LoopPromptSource::Bundle {
            slug,
            prompt_role: args.prompt_role.clone(),
        });
    }
    let _ = connection;
    bail!("loop run requires --prompt, --prompt-slug, or --bundle")
}

fn resolve_loop_agent(args: &LoopRunArgs) -> anyhow::Result<LoopAgent> {
    if args.dry_run {
        return Ok(LoopAgent::DryRun);
    }
    if let Some(agent_argv) = args.agent_argv.as_deref() {
        if args.agent.is_some() {
            bail!("--agent and --agent-argv are mutually exclusive");
        }
        return Ok(LoopAgent::Argv(parse_argv_json(agent_argv)?));
    }
    match args.agent.unwrap_or(CliLoopAgent::Codex) {
        CliLoopAgent::Codex => Ok(LoopAgent::Codex),
    }
}

fn loop_result_failed(result: &LoopRuntimeResult, options: &LoopRuntimeOptions) -> bool {
    if options.dry_run {
        return false;
    }
    result.agent_exit_code != Some(0)
        || (options.project_complete_requested && result.audit_exit_code != Some(0))
}
