use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::bail;
use dialoguer::{theme::ColorfulTheme, MultiSelect};

use crate::adapter_registry::AdapterRegistry;
use crate::loop_runtime::{
    run_loop_once, LoopAgent, LoopPromptSource, LoopRuntimeOptions, LoopRuntimeOutcome,
    LoopRuntimeResult,
};
use crate::store::{init_store, read_context_with_conduct_lifecycle};
use crate::tool_runner::parse_argv_json;
use crate::web::{generate_control_token, serve, WebOptions};

use super::super::args::{
    CliLoopAgent, ContextArgs, HarnessKind, InstallAdapterArgs, InstallArgs, InstallCommand,
    LoopArgs, LoopCommand, LoopRunArgs, StatusArgs, WebArgs,
};
use super::super::render::brief_context::{
    brief_context, print_brief_context, BriefContextOptions,
};
use super::super::render::context::print_context;
use super::super::render::emit;
use super::super::render::text::print_loop_result;
use super::super::{CLI_DEFAULT_HELP_SECTIONS, INIT_PROJECT_SETUP_PROMPT};

const LDGR_CONTEXT_EXTENSION: &str = include_str!("../../../extensions/ldgr-context.ts");
const AGENTCTL_REPO: &str = "https://github.com/hydra-dynamix/agentctl";

pub fn handle_init(db: &Path, artifact_root: &Path) -> anyhow::Result<()> {
    let existing_database = db.exists();
    init_store(db, artifact_root)?;
    if existing_database {
        println!("opened existing {} (no data erased)", db.display());
    } else {
        println!("initialized {}", db.display());
    }
    install_core_harness_resources()?;
    print_init_project_setup_prompt();
    print_cli_hierarchy();
    Ok(())
}

pub fn handle_install(args: InstallArgs) -> anyhow::Result<()> {
    if let Some(command) = &args.command {
        return match command {
            InstallCommand::Adapter(adapter_args) => handle_install_adapter(adapter_args),
        };
    }
    print_installer_header();
    let harnesses = select_harnesses(&args)?;
    if harnesses.is_empty() {
        return Ok(());
    }
    println!(
        "√ Harnesses: {}",
        harnesses
            .iter()
            .map(|h| harness_name(*h))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("│");

    let home = home_dir()?;
    let ldgr_home = home.join(".ldgr");
    fs::create_dir_all(&ldgr_home)?;

    println!("◇ Installing LDGR harness files...");
    let mut installed = Vec::new();
    for harness in &harnesses {
        installed.push(install_harness(*harness, &home)?);
    }
    let agentctl = ensure_agentctl_dependency(args.no_agentctl)?;
    let agentctl_config = install_agentctl_config(&home, &harnesses)?;

    let config = serde_json::json!({
        "schema_version": 1,
        "default_harness": harnesses.first().map(|harness| harness_name(*harness)).unwrap_or("pi"),
        "selected_harnesses": harnesses.iter().map(|harness| harness_name(*harness)).collect::<Vec<_>>(),
        "installed": installed,
        "agentctl": agentctl,
        "agentctl_config": agentctl_config,
        "adapter_files": {
            "default_global_path": "~/.ldgr/<adapter>",
            "note": "Adapter bundle files install globally under ~/.ldgr/<adapter>; adapter-owned skills/extensions install into the configured harness locations."
        },
        "notes": "Adapters should read this file, validate their own license when applicable, install adapter bundle files under ~/.ldgr/<adapter> by default, then install adapter-owned skills/extensions into the configured harness locations."
    });
    let config_path = ldgr_home.join("config.json");
    fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&config)?),
    )?;
    println!("├─ Wrote config {}", config_path.display());
    println!("│");
    println!("√ LDGR install complete");
    println!("│");
    println!("◇ Next steps");
    if harnesses.contains(&HarnessKind::Pi) {
        println!("│  Run /reload in Pi, then use /ldgr <args> or /ldgr-context.");
    }
    if harnesses.contains(&HarnessKind::Claude) {
        println!("│  Restart/reload Claude Code, then use /ldgr <args>.");
    }
    if harnesses.contains(&HarnessKind::Codex) {
        println!("│  Codex will use ~/.codex/instructions.md; ask it for /ldgr <args> behavior.");
    }
    if harnesses.contains(&HarnessKind::Openclaw) {
        println!(
            "│  Point OpenClaw/OpenCode at ~/.openclaw/commands and ~/.openclaw/skills if needed."
        );
    }
    println!("└─ Adapter bundles install under ~/.ldgr/<adapter>.");
    Ok(())
}

pub(crate) fn handle_install_adapter(args: &InstallAdapterArgs) -> anyhow::Result<()> {
    let adapter = normalize_adapter_name(&args.name);
    let Some(package) = open_source_adapter_package(&adapter) else {
        bail!("adapter `{}` is listed but not installable from this source checkout yet; run `ldgr adapter install list` for source/release information", args.name);
    };
    let home = home_dir()?;
    let install_root = args
        .install_root
        .clone()
        .unwrap_or_else(|| home.join(".ldgr").join(&adapter));
    let source_root = match &args.source_root {
        Some(root) => root.clone(),
        None => find_source_root(std::env::current_dir()?)?,
    };
    println!("◇ Installing LDGR adapter `{adapter}`");
    println!("├─ Source {}", source_root.display());
    println!("├─ Install root {}", install_root.display());
    let status = Command::new("cargo")
        .arg("run")
        .arg("-p")
        .arg(package)
        .arg("--")
        .arg("adapter")
        .arg("install")
        .arg("--install-root")
        .arg(&install_root)
        .arg("--print-path")
        .current_dir(&source_root)
        .status()?;
    if !status.success() {
        bail!("adapter installer failed for `{adapter}` with status {status}");
    }
    install_adapter_harness_assets(&adapter, &install_root, &home)?;
    println!("└─ Installed adapter `{adapter}`. Try `ldgr {adapter} --help` or `ldgr adapter show {adapter}`.");
    Ok(())
}

fn normalize_adapter_name(name: &str) -> String {
    name.trim()
        .strip_prefix("ldgr-")
        .unwrap_or_else(|| name.trim())
        .to_ascii_lowercase()
}

fn open_source_adapter_package(adapter: &str) -> Option<&'static str> {
    available_adapter_catalog()
        .iter()
        .find(|entry| entry.slug == adapter)
        .and_then(|entry| entry.workspace_package)
}

struct AvailableAdapter {
    slug: &'static str,
    title: &'static str,
    source: &'static str,
    install: &'static str,
    workspace_package: Option<&'static str>,
}

fn available_adapter_catalog() -> &'static [AvailableAdapter] {
    &[
        AvailableAdapter {
            slug: "conduct",
            title: "LDGR Conduct adapter",
            source: "commercial release / local workspace",
            install: "ldgr adapter install conduct",
            workspace_package: Some("ldgr-conduct"),
        },
        AvailableAdapter {
            slug: "research",
            title: "Research adapter",
            source: "https://github.com/hydra-dynamix/ldgr-research",
            install: "ldgr adapter install research",
            workspace_package: Some("ldgr-research"),
        },
        AvailableAdapter {
            slug: "example",
            title: "Public example adapter",
            source: "https://github.com/hydra-dynamix/ldgr-example-adapter",
            install: "ldgr adapter install example",
            workspace_package: Some("ldgr-example-adapter"),
        },
        AvailableAdapter {
            slug: "programbench",
            title: "Clean-room ProgramBench adapter",
            source: "https://github.com/hydra-dynamix/ldgr-programbench",
            install: "ldgr adapter install programbench",
            workspace_package: None,
        },
        AvailableAdapter {
            slug: "code",
            title: "Coding adapter",
            source: "commercial release catalog",
            install: "ldgr adapter install code",
            workspace_package: None,
        },
        AvailableAdapter {
            slug: "security",
            title: "Security adapter",
            source: "commercial release catalog",
            install: "ldgr adapter install security",
            workspace_package: None,
        },
        AvailableAdapter {
            slug: "explore",
            title: "Explore adapter",
            source: "commercial release catalog",
            install: "ldgr adapter install explore",
            workspace_package: None,
        },
        AvailableAdapter {
            slug: "bench",
            title: "Bench adapter",
            source: "commercial release catalog",
            install: "ldgr adapter install bench",
            workspace_package: None,
        },
    ]
}

pub(crate) fn print_available_adapter_catalog() {
    println!("Available adapters:");
    for entry in available_adapter_catalog() {
        println!("  {} — {} [{}]", entry.slug, entry.title, entry.source);
        println!("    install: {}", entry.install);
        println!("    after install: ldgr {} --help", entry.slug);
    }
    println!("  installed adapters: ldgr adapter list");
    println!("  adapter details: ldgr adapter show <slug>");
}

fn find_source_root(start: PathBuf) -> anyhow::Result<PathBuf> {
    for candidate in start.ancestors() {
        if candidate.join("Cargo.toml").is_file() && candidate.join("ldgr-core").is_dir() {
            return Ok(candidate.to_path_buf());
        }
    }
    bail!("could not find LDGR source checkout; pass --source-root")
}

fn install_adapter_harness_assets(
    adapter: &str,
    install_root: &Path,
    home: &Path,
) -> anyhow::Result<()> {
    let skills = install_root.join("skills");
    if skills.is_dir() {
        let pi_skills = home.join(".pi/agent/skills");
        copy_directory_children(&skills, &pi_skills)?;
        let portable_skills = home.join(".agents/skills");
        copy_directory_children(&skills, &portable_skills)?;
        println!(
            "├─ Harness skills {} and {}",
            pi_skills.display(),
            portable_skills.display()
        );
    }
    let extensions = install_root.join("extensions");
    if extensions.is_dir() {
        let pi_extensions = home.join(".pi/agent/extensions");
        copy_directory_children(&extensions, &pi_extensions)?;
        println!("├─ Pi extensions {}", pi_extensions.display());
    }
    let marker = home.join(".ldgr/installed-adapters").join(adapter);
    write_file(
        &marker,
        &format!("install_root={}\n", install_root.display()),
    )?;
    Ok(())
}

fn copy_directory_children(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir_recursive(&source, &dest)?;
        } else if source.is_file() {
            write_file(&dest, &fs::read_to_string(&source)?)?;
        }
    }
    Ok(())
}

fn copy_dir_recursive(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        let source = entry.path();
        let dest = to.join(entry.file_name());
        if source.is_dir() {
            copy_dir_recursive(&source, &dest)?;
        } else if source.is_file() {
            fs::copy(&source, &dest)?;
        }
    }
    Ok(())
}

fn print_installer_header() {
    println!("◇ create-ldgr");
    println!("│");
    println!("◇ Welcome to the LDGR harness installer");
    println!("│  Configure one or more agent harnesses for LDGR context commands.");
    println!("│");
}

fn select_harnesses(args: &InstallArgs) -> anyhow::Result<Vec<HarnessKind>> {
    if !args.harness.is_empty() {
        println!(
            "◇ Using harnesses from flags: {}",
            args.harness
                .iter()
                .map(|h| harness_name(*h))
                .collect::<Vec<_>>()
                .join(", ")
        );
        return Ok(args.harness.clone());
    }
    if args.yes || !stdin_is_terminal() {
        println!("◇ Which harness would you like to configure? › pi");
        return Ok(vec![HarnessKind::Pi]);
    }
    let theme = ColorfulTheme::default();
    let Some(selections) = MultiSelect::with_theme(&theme)
        .with_prompt(
            "Which harnesses would you like to configure? (Space to select, Enter to submit, Esc to cancel)",
        )
        .items(&[
            "pi — recommended; TypeScript extension + Agent Skills paths",
            "codex — instructions fallback for Codex CLI",
            "claude — Claude Code skill + slash-command prompt",
            "openclaw — OpenClaw/OpenCode-compatible skill + command prompt fallback",
        ])
        .defaults(&[true, false, false, false])
        .interact_opt()? else {
        println!("│");
        println!("└─ Install canceled");
        return Ok(Vec::new());
    };
    let mut harnesses = selections
        .into_iter()
        .filter_map(|index| match index {
            0 => Some(HarnessKind::Pi),
            1 => Some(HarnessKind::Codex),
            2 => Some(HarnessKind::Claude),
            3 => Some(HarnessKind::Openclaw),
            _ => None,
        })
        .collect::<Vec<_>>();
    if harnesses.is_empty() {
        harnesses.push(HarnessKind::Pi);
    }
    Ok(harnesses)
}

fn install_harness(harness: HarnessKind, home: &Path) -> anyhow::Result<serde_json::Value> {
    match harness {
        HarnessKind::Pi => install_pi_harness(home),
        HarnessKind::Codex => install_codex_harness(home),
        HarnessKind::Claude => install_claude_harness(home),
        HarnessKind::Openclaw => install_openclaw_harness(home),
    }
}

fn ensure_agentctl_dependency(skip: bool) -> anyhow::Result<serde_json::Value> {
    if skip {
        println!("├─ Skipped agentctl install (--no-agentctl)");
        return Ok(serde_json::json!({
            "required": true,
            "installed_by_ldgr": false,
            "status": "skipped",
            "install_hint": format!("cargo install --git {AGENTCTL_REPO}")
        }));
    }
    if command_on_path("agentctl") {
        println!("├─ agentctl already available on PATH");
        return Ok(serde_json::json!({
            "required": true,
            "installed_by_ldgr": false,
            "status": "already_on_path",
            "command": "agentctl"
        }));
    }

    println!("├─ Installing agentctl from {AGENTCTL_REPO}");
    let status = Command::new("cargo")
        .arg("install")
        .arg("--git")
        .arg(AGENTCTL_REPO)
        .stdin(Stdio::null())
        .status()
        .map_err(|error| anyhow::anyhow!("failed to start cargo install for agentctl: {error}"))?;
    if !status.success() {
        bail!(
            "agentctl install failed with status {status}; install it with `cargo install --git {AGENTCTL_REPO}` or rerun `ldgr install --no-agentctl` to manage it yourself"
        );
    }
    Ok(serde_json::json!({
        "required": true,
        "installed_by_ldgr": true,
        "status": "installed",
        "command": "agentctl",
        "source": AGENTCTL_REPO
    }))
}

fn install_agentctl_config(
    home: &Path,
    harnesses: &[HarnessKind],
) -> anyhow::Result<serde_json::Value> {
    let config_path = home.join(".ldgr/agentctl/harness.toml");
    let config = render_agentctl_config(harnesses);
    write_file(&config_path, &config)?;
    println!("├─ agentctl config {}", config_path.display());
    Ok(serde_json::json!({
        "path": config_path,
        "agents": harnesses.iter().map(|harness| harness_name(*harness)).collect::<Vec<_>>(),
        "task": "ldgr-loop",
        "note": "agentctl is the canonical LDGR agent control plane; ldgr loop run --agent agentctl uses this global harness config."
    }))
}

fn render_agentctl_config(harnesses: &[HarnessKind]) -> String {
    let primary = harnesses.first().copied().unwrap_or(HarnessKind::Pi);
    let mut commands = Vec::<Vec<&'static str>>::new();
    for harness in harnesses {
        commands.extend(agentctl_commands_for_harness(*harness));
    }
    commands.sort();
    commands.dedup();
    let allowed = commands
        .iter()
        .filter_map(|command| command.first().copied())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .map(|command| format!("\"{command}\""))
        .collect::<Vec<_>>()
        .join(", ");
    let mut rendered = format!(
        "allowed_commands = [{allowed}]\nallowed_builtins = [\"read\"]\nenv_allowlist = [\"PATH\", \"HOME\", \"ANTHROPIC_API_KEY\", \"ANTHROPIC_OAUTH_TOKEN\", \"OPENAI_API_KEY\", \"CODEX_HOME\", \"PI_CODING_AGENT_DIR\"]\n\n"
    );
    rendered.push_str("[tasks.ldgr-loop]\ncommands = [");
    rendered.push_str(&render_agentctl_command(&agentctl_primary_command(primary)));
    rendered.push_str("]\n\n");
    for harness in harnesses {
        rendered.push_str(&format!(
            "[tasks.ldgr-loop-{}]\ncommands = [",
            harness_name(*harness)
        ));
        rendered.push_str(&render_agentctl_command(&agentctl_primary_command(
            *harness,
        )));
        rendered.push_str("]\n\n");
    }
    rendered
}

fn agentctl_commands_for_harness(harness: HarnessKind) -> Vec<Vec<&'static str>> {
    match harness {
        HarnessKind::Pi => vec![vec!["pi", "-p"]],
        HarnessKind::Codex => vec![vec!["codex", "exec", "--sandbox", "workspace-write"]],
        HarnessKind::Claude => vec![vec!["claude", "-p"]],
        HarnessKind::Openclaw => vec![vec!["openclaw", "run"], vec!["opencode", "run"]],
    }
}

fn agentctl_primary_command(harness: HarnessKind) -> Vec<&'static str> {
    agentctl_commands_for_harness(harness)
        .into_iter()
        .next()
        .unwrap_or_else(|| vec!["pi", "-p"])
}

fn render_agentctl_command(command: &[&str]) -> String {
    format!(
        "[{}]",
        command
            .iter()
            .map(|part| format!("\"{part}\""))
            .collect::<Vec<_>>()
            .join(", ")
    )
}

fn command_on_path(command: &str) -> bool {
    Command::new(command)
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok()
}

fn install_pi_harness(home: &Path) -> anyhow::Result<serde_json::Value> {
    let extension = home.join(".pi/agent/extensions/ldgr-context.ts");
    write_file(&extension, LDGR_CONTEXT_EXTENSION)?;
    println!("├─ Pi extension {}", extension.display());
    Ok(serde_json::json!({
        "harness": "pi",
        "extension_paths": [extension],
        "skill_paths": [home.join(".pi/agent/skills"), home.join(".agents/skills")],
        "reload": "Run /reload in Pi, then use /ldgr <args> or /ldgr-context."
    }))
}

fn install_codex_harness(home: &Path) -> anyhow::Result<serde_json::Value> {
    let doc = home.join(".codex/ldgr/LDGR.md");
    write_file(&doc, LDGR_HARNESS_GUIDE)?;
    let instructions = home.join(".codex/instructions.md");
    append_marked_section(&instructions, "ldgr-core", CODEX_INSTRUCTIONS)?;
    println!("├─ Codex guide {}", doc.display());
    println!("├─ Codex instructions {}", instructions.display());
    Ok(serde_json::json!({
        "harness": "codex",
        "instruction_path": instructions,
        "guide_path": doc,
        "extension_equivalent": "Codex CLI has plugin/MCP surfaces, but no local Pi-style slash-command extension was detected; LDGR installs global instructions and a guide instead."
    }))
}

fn install_claude_harness(home: &Path) -> anyhow::Result<serde_json::Value> {
    let skill = home.join(".claude/skills/ldgr-core/SKILL.md");
    let command = home.join(".claude/commands/ldgr.md");
    write_file(&skill, LDGR_CORE_SKILL)?;
    write_file(&command, CLAUDE_LDGR_COMMAND)?;
    println!("├─ Claude Code skill {}", skill.display());
    println!("├─ Claude Code slash command {}", command.display());
    Ok(serde_json::json!({
        "harness": "claude",
        "skill_paths": [home.join(".claude/skills")],
        "command_paths": [command],
        "usage": "Restart/reload Claude Code, then use /ldgr <args>."
    }))
}

fn install_openclaw_harness(home: &Path) -> anyhow::Result<serde_json::Value> {
    let skill = home.join(".openclaw/skills/ldgr-core/SKILL.md");
    let command = home.join(".openclaw/commands/ldgr.md");
    write_file(&skill, LDGR_CORE_SKILL)?;
    write_file(&command, CLAW_LDGR_COMMAND)?;
    println!("├─ OpenClaw skill fallback {}", skill.display());
    println!("├─ OpenClaw command fallback {}", command.display());
    Ok(serde_json::json!({
        "harness": "openclaw",
        "skill_paths": [home.join(".openclaw/skills")],
        "command_paths": [command],
        "extension_equivalent": "OpenClaw compatibility is recorded as skill/command prompt files; adapt these paths if your OpenClaw distribution uses different resource roots."
    }))
}

fn write_file(path: &Path, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    fs::write(path, content)?;
    Ok(())
}

fn append_marked_section(path: &Path, marker: &str, content: &str) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }
    let start = format!("<!-- LDGR:{marker}:START -->");
    let end = format!("<!-- LDGR:{marker}:END -->");
    let section = format!("{start}\n{}\n{end}\n", content.trim_end());
    let existing = fs::read_to_string(path).unwrap_or_default();
    let next =
        if let (Some(start_idx), Some(end_idx)) = (existing.find(&start), existing.find(&end)) {
            let after = end_idx + end.len();
            format!(
                "{}{}{}",
                &existing[..start_idx],
                section.trim_end(),
                &existing[after..]
            )
        } else if existing.trim().is_empty() {
            section
        } else {
            format!("{}\n\n{}", existing.trim_end(), section)
        };
    fs::write(path, next)?;
    Ok(())
}

fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory from HOME/USERPROFILE"))
}

fn stdin_is_terminal() -> bool {
    use std::io::IsTerminal;
    io::stdin().is_terminal()
}

fn harness_name(harness: HarnessKind) -> &'static str {
    match harness {
        HarnessKind::Pi => "pi",
        HarnessKind::Codex => "codex",
        HarnessKind::Claude => "claude",
        HarnessKind::Openclaw => "openclaw",
    }
}

const LDGR_HARNESS_GUIDE: &str = r#"# LDGR harness guide

Use LDGR as the durable project ledger. Start with `ldgr status`; expand to `ldgr context --brief` or `ldgr context` when needed. When a user asks for `/ldgr <args>` behavior in a harness without a local extension API, run `ldgr <args>` from the shell and paste stdout/stderr back into the conversation.

Adapter installers should read `~/.ldgr/config.json`, validate their own license when applicable, install adapter bundle files under `~/.ldgr/<adapter>` by default, and install adapter-owned skills/extensions into the configured harness locations.
"#;

const CODEX_INSTRUCTIONS: &str = r#"# LDGR

When the user asks for LDGR state or says `/ldgr <args>`, run `ldgr <args>` from the shell and report stdout/stderr. With no args, run `ldgr context --brief`. Use `ldgr status` first for project on-ramp work.
"#;

const LDGR_CORE_SKILL: &str = r#"---
name: ldgr-core
description: Use when working with LDGR durable project ledgers, context, status, work items, runs, observations, artifacts, validations, or decisions.
---

# LDGR Core

- Start with `ldgr status`.
- Use `ldgr context --brief` for compact handoff context and `ldgr context` for deeper history.
- When asked for `/ldgr <args>` behavior, run `ldgr <args>` and include stdout/stderr in the conversation.
- Record durable work with one work item, one run, observations/artifacts, validation evidence, and a closing decision.
"#;

const CLAUDE_LDGR_COMMAND: &str = r#"Run `ldgr $ARGUMENTS` in the current project and report stdout/stderr back to the conversation. If no arguments are provided, run `ldgr context --brief`.
"#;

const CLAW_LDGR_COMMAND: &str = r#"Run `ldgr $ARGUMENTS` in the current project and report stdout/stderr back to the conversation. If no arguments are provided, run `ldgr context --brief`.
"#;

pub fn handle_status(
    connection: &rusqlite::Connection,
    artifact_root: &Path,
    args: StatusArgs,
) -> anyhow::Result<()> {
    let context = read_context_with_conduct_lifecycle(connection, artifact_root)?;
    let brief = brief_context(&context, brief_options(args.recent, args.width));
    emit(args.json, &brief, print_brief_context)?;
    if !args.json {
        print_installed_adapter_summary();
    }
    Ok(())
}

pub fn handle_context(
    connection: &rusqlite::Connection,
    artifact_root: &Path,
    args: ContextArgs,
) -> anyhow::Result<()> {
    let context = read_context_with_conduct_lifecycle(connection, artifact_root)?;
    if args.brief {
        let brief = brief_context(&context, brief_options(args.recent, args.width));
        return emit(args.json, &brief, print_brief_context);
    }
    emit(args.json, &context, print_context)?;
    if !args.json {
        print_installed_adapter_summary();
    }
    Ok(())
}

fn print_installed_adapter_summary() {
    let registry = AdapterRegistry::discover();
    if registry.adapters.is_empty() {
        return;
    }
    println!();
    println!("installed_adapters:");
    for adapter in registry.adapters {
        let namespaces = adapter
            .command_namespaces
            .iter()
            .map(|command| format!("ldgr {}", command.namespace))
            .collect::<Vec<_>>()
            .join(", ");
        let profiles = adapter
            .target_profiles
            .iter()
            .map(|profile| profile.slug.clone())
            .collect::<Vec<_>>()
            .join(", ");
        println!("- {} ({})", adapter.slug, adapter.title);
        if !namespaces.is_empty() {
            println!("  commands: {namespaces}");
        }
        if !profiles.is_empty() {
            println!("  profiles: {profiles}");
        }
    }
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

fn install_core_harness_resources() -> anyhow::Result<()> {
    fs::create_dir_all(".pi/extensions")?;
    fs::write(".pi/extensions/ldgr-context.ts", LDGR_CONTEXT_EXTENSION)?;
    fs::create_dir_all(".ldgr")?;
    fs::write(
        ".ldgr/harness-setup.md",
        "# LDGR harness setup\n\n\
`ldgr init` installed the Pi project-local extension `.pi/extensions/ldgr-context.ts`.\n\n\
If your agent harness is Pi, run `/reload` so `/ldgr <args>` and `/ldgr-context` become available. `/ldgr` runs the LDGR CLI in the project and pipes stdout/stderr back into the conversation; with no args it runs `ldgr context --brief`.\n\n\
If your agent harness is not Pi or does not load project-local Pi extensions, point the agent at this document and ask it to adapt the installed extension for its harness. The extension is optional; core `ldgr ...` commands continue to work from the shell.\n",
    )?;
    println!("installed Pi extension .pi/extensions/ldgr-context.ts");
    println!("wrote fallback harness notes .ldgr/harness-setup.md");
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
    print_available_adapter_catalog();
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
    match args.agent.unwrap_or(CliLoopAgent::Agentctl) {
        CliLoopAgent::Agentctl => Ok(LoopAgent::Agentctl),
    }
}

fn loop_result_failed(result: &LoopRuntimeResult, options: &LoopRuntimeOptions) -> bool {
    if options.dry_run {
        return false;
    }
    result.agent_exit_code != Some(0)
        || (options.project_complete_requested && result.audit_exit_code != Some(0))
}
