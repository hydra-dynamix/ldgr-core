use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context};
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect};

use crate::adapter_registry::AdapterRegistry;
use crate::loop_runtime::{
    run_loop_once, LoopAgent, LoopPromptSource, LoopRuntimeOptions, LoopRuntimeOutcome,
    LoopRuntimeResult,
};
use crate::store::{init_store, read_context};
use crate::tool_runner::parse_argv_json;
use crate::web::{generate_control_token, serve, WebOptions};

use super::super::args::{
    AdapterReconcileArgs, AdapterUninstallArgs, AdapterUpdateArgs, CliLoopAgent, ContextArgs,
    HarnessKind, InstallAdapterArgs, InstallArgs, InstallCommand, LoopArgs, LoopCommand,
    LoopRunArgs, StatusArgs, WebArgs,
};
use super::super::render::brief_context::{
    brief_context, print_brief_context, BriefContextOptions,
};
use super::super::render::context::print_context;
use super::super::render::emit;
use super::super::render::status::{build_status_summary, print_status_summary};
use super::super::render::text::print_loop_result;
use super::super::{CLI_DEFAULT_HELP_SECTIONS, INIT_PROJECT_SETUP_PROMPT};

const LDGR_CONTEXT_EXTENSION: &str = include_str!("../../../extensions/ldgr-context.ts");
const LDGR_CORE_LOOP_PROMPT: &str = include_str!("../../../prompts/loop-prompt.md");
const LDGR_CORE_LOOP_PROMPT_FILE: &str = "ldgr-core-loop.md";
const LDGR_RELEASE_KEYRING: &str = include_str!("../../../release-keyring.json");
const LDGR_RELEASE_KEYRING_FILE: &str = "release-keyring.json";
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
    print_installed_adapter_summary();
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
    let release_keyring = ldgr_home.join(LDGR_RELEASE_KEYRING_FILE);
    fs::write(&release_keyring, LDGR_RELEASE_KEYRING)?;

    println!("◇ Installing LDGR harness files...");
    let prompt_root = ldgr_home.join("prompts");
    fs::create_dir_all(&prompt_root)?;
    let core_loop_prompt = prompt_root.join(LDGR_CORE_LOOP_PROMPT_FILE);
    fs::write(&core_loop_prompt, LDGR_CORE_LOOP_PROMPT)?;
    println!("├─ Core loop prompt {}", core_loop_prompt.display());
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
        "core_loop_prompt": core_loop_prompt,
        "adapter_release_keyring": release_keyring,
        "adapter_files": {
            "default_global_path": "~/.ldgr/adapters/<adapter>",
            "note": "Adapter bundle files install globally under ~/.ldgr/adapters/<adapter>; adapter-owned prompts, skills, commands, and extensions install into paths declared by the configured harness entries."
        },
        "notes": "Adapters should read this file, validate their own license when applicable, install adapter bundle files under ~/.ldgr/adapters/<adapter> by default, then install adapter-owned prompts, skills, commands, and extensions into paths declared by the configured harness entries."
    });
    let config_path = ldgr_home.join("config.json");
    fs::write(
        &config_path,
        format!("{}\n", serde_json::to_string_pretty(&config)?),
    )?;
    println!("├─ Wrote config {}", config_path.display());
    reconcile_installed_adapters(&home, None)?;
    println!("│");
    println!("√ LDGR install complete");
    println!("│");
    println!("◇ Next steps");
    if harnesses.contains(&HarnessKind::Pi) {
        println!("│  Run /reload in Pi, then use /ldgr <args>, /ldgr-context, or /run-loop.");
    }
    if harnesses.contains(&HarnessKind::Claude) {
        println!("│  Restart/reload Claude Code, then use /ldgr <args>.");
    }
    if harnesses.contains(&HarnessKind::Codex) {
        println!(
            "│  Codex will use prompts under ~/.codex/prompts; ask it for /ldgr <args> behavior."
        );
    }
    if harnesses.contains(&HarnessKind::Openclaw) {
        println!(
            "│  Point OpenClaw/OpenCode at ~/.openclaw/commands and ~/.openclaw/skills if needed."
        );
    }
    let adapters = select_adapters(&args)?;
    if !adapters.is_empty() {
        println!("│");
        println!("◇ Installing adapter bundles...");
        for adapter in adapters {
            handle_install_adapter(&InstallAdapterArgs {
                name: adapter,
                source_root: None,
                install_root: None,
                version: None,
                prerelease: false,
                offline: false,
                yes: args.yes,
            })?;
        }
    }
    println!("└─ Adapter bundles install under ~/.ldgr/adapters/<adapter>.");
    Ok(())
}

pub(crate) fn handle_interactive_adapter_install(
    source_root: Option<PathBuf>,
    install_root: Option<PathBuf>,
    yes: bool,
) -> anyhow::Result<()> {
    if yes || !stdin_is_terminal() {
        print_available_adapter_catalog();
        println!("\nRun `ldgr adapter install <adapter>` to install one adapter, or run `ldgr adapter install` in an interactive terminal for the selection menu.");
        return Ok(());
    }
    if install_root.is_some() {
        bail!("--install-root requires an adapter name; run `ldgr adapter install <adapter> --install-root <path>`");
    }
    let adapters = select_adapter_bundles()?;
    if adapters.is_empty() {
        println!("No adapter selected.");
        return Ok(());
    }
    for adapter in adapters {
        handle_install_adapter(&InstallAdapterArgs {
            name: adapter,
            source_root: source_root.clone(),
            install_root: None,
            version: None,
            prerelease: false,
            offline: false,
            yes,
        })?;
    }
    Ok(())
}

pub(crate) fn handle_install_adapter(args: &InstallAdapterArgs) -> anyhow::Result<()> {
    if args.source_root.is_some() {
        return install_adapter_from_catalog(args);
    }

    let configured_source = std::env::var(crate::release_index::ADAPTER_RELEASE_INDEX_ENV).ok();
    let source = configured_source
        .as_deref()
        .unwrap_or(crate::release_index::DEFAULT_ADAPTER_RELEASE_INDEX_URL);
    if args.offline && source.starts_with("http") {
        bail!("--offline requires LDGR_ADAPTER_INDEX to reference a local file");
    }
    match crate::release_index::load_release_index(source) {
        Ok(index) => install_adapter_from_index(args, &index),
        Err(index_error) if default_catalog_fallback_allowed(args, configured_source.is_some()) => {
            eprintln!(
                "warning: {index_error:#}; falling back to the built-in release/git installer for `{}`",
                args.name
            );
            install_adapter_from_catalog(args).with_context(|| {
                format!(
                    "built-in adapter fallback also failed after release index {source} was unavailable"
                )
            })
        }
        Err(index_error) => Err(index_error),
    }
}

fn default_catalog_fallback_allowed(args: &InstallAdapterArgs, index_is_explicit: bool) -> bool {
    !index_is_explicit && !args.offline && args.version.is_none() && !args.prerelease
}

fn install_adapter_from_catalog(args: &InstallAdapterArgs) -> anyhow::Result<()> {
    let adapter = resolve_adapter_install_name(&args.name, args.yes)?;
    let Some(entry) = available_adapter_catalog()
        .iter()
        .find(|entry| entry.slug == adapter)
    else {
        bail!(
            "unknown adapter `{}`; run `ldgr adapter install list`",
            args.name
        );
    };
    let home = home_dir()?;
    let install_root = args
        .install_root
        .clone()
        .unwrap_or_else(|| home.join(".ldgr/adapters").join(&adapter));
    println!("◇ Installing LDGR adapter `{adapter}`");
    println!("├─ Install root {}", install_root.display());
    if let Some(source_root) = &args.source_root {
        install_adapter_from_source_root(entry, source_root, &install_root)?;
    } else if let Some(release) = entry.release {
        install_adapter_from_release(entry, release, &install_root, &home)?;
    } else if let Some(git) = entry.git {
        install_adapter_from_git(entry, git, &install_root)?;
    } else if let Some(package) = entry.workspace_package {
        let source_root = find_source_root(std::env::current_dir()?)?;
        install_adapter_from_source_root_with_package(package, &source_root, &install_root)?;
    } else {
        bail!("adapter `{adapter}` has no release or source installer configured yet");
    }
    install_adapter_harness_assets(&adapter, &install_root, &home)?;
    println!("└─ Installed adapter `{adapter}`. Try `ldgr {adapter} --help` or `ldgr adapter show {adapter}`.");
    Ok(())
}

pub(crate) fn handle_update_adapter(args: &AdapterUpdateArgs) -> anyhow::Result<()> {
    use semver::Version;

    let registry = AdapterRegistry::discover();
    let installed = registry
        .find(&args.name)
        .with_context(|| format!("adapter `{}` is not installed", args.name))?;
    let receipt = installed
        .installation_receipt
        .as_ref()
        .context("installed adapter has no verified installation receipt; reinstall it first")?;
    let current_text = receipt
        .get("version")
        .and_then(serde_json::Value::as_str)
        .context("installation receipt has no version")?;
    let current = Version::parse(current_text).context("installed receipt version is invalid")?;
    let index = crate::release_index::load_configured_release_index()?;
    let core = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let platform = platform_tag()?;
    let resolved = crate::release_index::resolve_release(
        &index,
        &installed.slug,
        &core,
        &platform,
        None,
        args.prerelease,
    )?;
    if resolved.version <= current {
        println!(
            "adapter={} installed={} latest_compatible={} update_available=false",
            installed.slug, current, resolved.version
        );
        return Ok(());
    }
    println!(
        "adapter={} installed={} latest_compatible={} update_available=true",
        installed.slug, current, resolved.version
    );
    if args.check {
        return Ok(());
    }
    install_adapter_from_configured_index(&InstallAdapterArgs {
        name: installed.slug.clone(),
        source_root: None,
        install_root: Some(installed.root_path.clone()),
        version: Some(resolved.version.to_string()),
        prerelease: args.prerelease,
        offline: false,
        yes: true,
    })
}

pub(crate) fn handle_uninstall_adapter(args: &AdapterUninstallArgs) -> anyhow::Result<()> {
    let registry = AdapterRegistry::discover();
    let installed = registry
        .find(&args.name)
        .with_context(|| format!("adapter `{}` is not installed", args.name))?;
    let receipt_value = installed.installation_receipt.clone().context(
        "installed adapter has no verified installation receipt; refusing untracked removal",
    )?;
    let receipt: crate::release_index::InstallationReceipt =
        serde_json::from_value(receipt_value).context("installation receipt is invalid")?;
    let mut modified = Vec::new();
    if digest_bundle(&installed.root_path)? != receipt.bundle_sha256 {
        modified.push(installed.root_path.clone());
    }
    for resource in &receipt.owned_resources {
        let path = PathBuf::from(&resource.path);
        if path.exists() && digest_path(&path)? != resource.sha256 {
            modified.push(path);
        }
    }
    if let (Some(path), Some(expected)) = (&receipt.binary_path, &receipt.binary_sha256) {
        let path = PathBuf::from(path);
        if path.exists() && digest_path(&path)? != *expected {
            modified.push(path);
        }
    }
    if !modified.is_empty() && !args.force {
        bail!(
            "refusing to remove modified adapter-owned files:\n{}\nRe-run with --force to remove them.",
            modified
                .iter()
                .map(|path| format!("  {}", path.display()))
                .collect::<Vec<_>>()
                .join("\n")
        );
    }
    for resource in &receipt.owned_resources {
        remove_path_if_exists(Path::new(&resource.path))?;
    }
    if let Some(binary) = &receipt.binary_path {
        remove_path_if_exists(Path::new(binary))?;
    }
    remove_path_if_exists(&installed.root_path)?;
    let marker = home_dir()?
        .join(".ldgr/installed-adapters")
        .join(&installed.slug);
    remove_path_if_exists(&marker)?;
    println!("uninstalled adapter={}", installed.slug);
    Ok(())
}

pub(crate) fn handle_reconcile_adapters(args: &AdapterReconcileArgs) -> anyhow::Result<()> {
    reconcile_installed_adapters(&home_dir()?, args.name.as_deref())
}

fn reconcile_installed_adapters(home: &Path, requested: Option<&str>) -> anyhow::Result<()> {
    let registry = AdapterRegistry::discover();
    let adapters = registry
        .adapters
        .iter()
        .filter(|adapter| {
            requested.is_none_or(|name| {
                adapter.slug == name || adapter.aliases.iter().any(|alias| alias == name)
            })
        })
        .collect::<Vec<_>>();
    if requested.is_some() && adapters.is_empty() {
        bail!("requested adapter is not installed");
    }
    for adapter in adapters {
        let Some(value) = adapter.installation_receipt.clone() else {
            continue;
        };
        let mut receipt: crate::release_index::InstallationReceipt =
            serde_json::from_value(value).context("installation receipt is invalid")?;
        let desired_plan =
            typed_harness_resource_plan(&adapter.root_path, home, &receipt.resource_manifest)?;
        let desired_targets = desired_plan
            .iter()
            .map(|(_, target)| target.clone())
            .collect::<Vec<_>>();
        let temp = std::env::temp_dir().join(format!(
            "ldgr-adapter-reconcile-{}-{}",
            adapter.slug,
            std::process::id()
        ));
        remove_path_if_exists(&temp)?;
        let mut transaction = InstallTransaction::new(temp.join("rollback"))?;
        transaction.snapshot(&adapter.root_path)?;
        for resource in &receipt.owned_resources {
            let path = PathBuf::from(&resource.path);
            if path.exists() && digest_path(&path)? != resource.sha256 {
                bail!(
                    "refusing to reconcile modified adapter resource {}",
                    path.display()
                );
            }
            transaction.snapshot(&path)?;
        }
        for target in &desired_targets {
            transaction.snapshot(target)?;
        }
        for resource in &receipt.owned_resources {
            let path = PathBuf::from(&resource.path);
            if !desired_targets.iter().any(|target| target == &path) {
                remove_path_if_exists(&path)?;
            }
        }
        install_typed_harness_resources(&desired_plan)?;
        receipt.owned_resources = desired_targets
            .iter()
            .map(|path| {
                Ok(crate::release_index::OwnedResource {
                    path: path.display().to_string(),
                    sha256: digest_path(path)?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?;
        fs::write(
            adapter.root_path.join("installation-receipt.json"),
            format!("{}\n", serde_json::to_string_pretty(&receipt)?),
        )?;
        transaction.commit()?;
        remove_path_if_exists(&temp)?;
        println!(
            "reconciled adapter={} resources={}",
            adapter.slug,
            receipt.owned_resources.len()
        );
    }
    Ok(())
}

fn install_adapter_from_configured_index(args: &InstallAdapterArgs) -> anyhow::Result<()> {
    let configured_source = std::env::var(crate::release_index::ADAPTER_RELEASE_INDEX_ENV)
        .unwrap_or_else(|_| crate::release_index::DEFAULT_ADAPTER_RELEASE_INDEX_URL.to_owned());
    if args.offline && configured_source.starts_with("http") {
        bail!("--offline requires LDGR_ADAPTER_INDEX to reference a local file");
    }
    let index = crate::release_index::load_release_index(&configured_source)?;
    install_adapter_from_index(args, &index)
}

fn install_adapter_from_index(
    args: &InstallAdapterArgs,
    index: &crate::release_index::AdapterReleaseIndex,
) -> anyhow::Result<()> {
    use semver::Version;

    let requested = normalize_adapter_name(&args.name);
    let adapter = index
        .adapters
        .iter()
        .find(|entry| {
            entry.domain == requested || entry.aliases.iter().any(|alias| alias == &requested)
        })
        .with_context(|| {
            format!(
                "unknown adapter `{}` in configured release index",
                args.name
            )
        })?;
    let exact = args
        .version
        .as_deref()
        .map(Version::parse)
        .transpose()
        .context("--version must be a semantic version")?;
    let core = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let platform = platform_tag()?;
    let resolved = crate::release_index::resolve_release(
        index,
        &adapter.domain,
        &core,
        &platform,
        exact.as_ref(),
        args.prerelease,
    )?;
    if args.offline
        && (!resolved.platform.asset_url.starts_with("file://")
            || !resolved.platform.signature_url.starts_with("file://"))
    {
        bail!("--offline requires file:// archive and signature URLs in the release index");
    }
    let home = home_dir()?;
    let install_root = args
        .install_root
        .clone()
        .unwrap_or_else(|| home.join(".ldgr/adapters").join(&adapter.domain));
    println!("◇ Installing LDGR adapter `{}`", adapter.domain);
    println!("├─ Resolved version {} for {platform}", resolved.version);
    println!("├─ Install root {}", install_root.display());
    install_resolved_index_release(&resolved, &install_root, &home)?;
    println!(
        "└─ Installed adapter `{}`. Try `ldgr {} --help` or `ldgr adapter show {}`.",
        adapter.domain, adapter.domain, adapter.domain
    );
    Ok(())
}

fn install_resolved_index_release(
    resolved: &crate::release_index::ResolvedAdapterRelease<'_>,
    install_root: &Path,
    home: &Path,
) -> anyhow::Result<()> {
    let temp = std::env::temp_dir().join(format!(
        "ldgr-adapter-index-install-{}-{}",
        resolved.adapter.domain,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;
    let archive = temp.join("adapter.tar.gz");
    run_checked(
        Command::new("curl")
            .arg("-fsSL")
            .arg(&resolved.platform.asset_url)
            .arg("-o")
            .arg(&archive),
        "download indexed adapter release",
    )?;
    crate::release_index::verify_file_sha256(&archive, &resolved.platform.sha256)?;
    let signature = temp.join("adapter.sig");
    run_checked(
        Command::new("curl")
            .arg("-fsSL")
            .arg(&resolved.platform.signature_url)
            .arg("-o")
            .arg(&signature),
        "download indexed adapter signature",
    )?;
    let keyring = configured_release_keyring(home)?;
    crate::release_index::verify_detached_release_signature(
        &archive,
        &signature,
        &keyring,
        &resolved.platform.signing_key_id,
    )?;
    crate::release_index::extract_safe_tar_gz(&archive, &temp, &resolved.platform.archive_root)?;
    let extracted = temp.join(&resolved.platform.archive_root);
    if !extracted.is_dir() {
        bail!(
            "release archive did not contain expected root {}",
            extracted.display()
        );
    }
    let mut transaction = InstallTransaction::new(temp.join("rollback"))?;
    transaction.snapshot(install_root)?;
    let binary_source = extracted
        .join(&resolved.platform.platform)
        .join(&resolved.platform.binary);
    if binary_source.is_file() {
        transaction.snapshot(&home.join(".local/bin").join(&resolved.platform.binary))?;
    }
    transaction.snapshot(
        &home
            .join(".ldgr/installed-adapters")
            .join(&resolved.adapter.domain),
    )?;
    let resource_plan =
        typed_harness_resource_plan(&extracted, home, &resolved.platform.resource_manifest)?;
    let resource_targets = resource_plan
        .iter()
        .map(|(_, target)| target.clone())
        .collect::<Vec<_>>();
    for target in &resource_targets {
        transaction.snapshot(target)?;
    }
    activate_bundle_atomically(&extracted, install_root)?;
    let installed_binary = install_release_binary(
        install_root,
        home,
        &resolved.platform.binary,
        &resolved.platform.platform,
    )?;
    if installed_binary.is_none()
        && adapter_manifest_references_binary(install_root, &resolved.platform.binary)?
    {
        bail!(
            "adapter release {} is missing required executable {}/{}; installation was rolled back",
            resolved.adapter.domain,
            resolved.platform.platform,
            resolved.platform.binary
        );
    }
    if let Some(binary_path) = installed_binary {
        run_adapter_binary_installer(
            binary_path.as_os_str(),
            &resolved.adapter.domain,
            install_root,
        )?;
    }
    patch_adapter_argv_to_installed_binary(install_root, &resolved.platform.binary, home)?;
    install_typed_harness_resources(&resource_plan)?;
    write_file(
        &home
            .join(".ldgr/installed-adapters")
            .join(&resolved.adapter.domain),
        &format!("install_root={}\n", install_root.display()),
    )?;
    let binary_path = binary_source
        .is_file()
        .then(|| home.join(".local/bin").join(&resolved.platform.binary));
    write_installation_receipt(
        install_root,
        resolved,
        binary_path.as_deref(),
        &resource_targets,
    )?;
    transaction.commit()?;
    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

fn configured_release_keyring(home: &Path) -> anyhow::Result<PathBuf> {
    let keyring = std::env::var_os(crate::release_index::ADAPTER_RELEASE_KEYRING_ENV)
        .map(PathBuf::from)
        .unwrap_or_else(|| home.join(".ldgr").join(LDGR_RELEASE_KEYRING_FILE));
    if !keyring.is_file() {
        bail!(
            "trusted adapter release keyring not found at {}; run `ldgr install` first or set {}",
            keyring.display(),
            crate::release_index::ADAPTER_RELEASE_KEYRING_ENV
        );
    }
    Ok(keyring)
}

fn write_installation_receipt(
    install_root: &Path,
    resolved: &crate::release_index::ResolvedAdapterRelease<'_>,
    binary_path: Option<&Path>,
    resources: &[PathBuf],
) -> anyhow::Result<()> {
    let installed_at_unix_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    let receipt = crate::release_index::InstallationReceipt {
        schema_version: 1,
        domain: resolved.adapter.domain.clone(),
        version: resolved.version.to_string(),
        source_url: resolved.platform.asset_url.clone(),
        sha256: resolved.platform.sha256.clone(),
        signing_key_id: resolved.platform.signing_key_id.clone(),
        core_compatibility: resolved.release.core_compatibility.clone(),
        platform: resolved.platform.platform.clone(),
        resource_manifest: resolved.platform.resource_manifest.clone(),
        installed_at_unix_seconds,
        bundle_sha256: digest_bundle(install_root)?,
        binary_path: binary_path.map(|path| path.display().to_string()),
        binary_sha256: binary_path.map(digest_path).transpose()?,
        owned_resources: resources
            .iter()
            .map(|path| {
                Ok(crate::release_index::OwnedResource {
                    path: path.display().to_string(),
                    sha256: digest_path(path)?,
                })
            })
            .collect::<anyhow::Result<Vec<_>>>()?,
    };
    fs::write(
        install_root.join("installation-receipt.json"),
        format!("{}\n", serde_json::to_string_pretty(&receipt)?),
    )?;
    Ok(())
}

fn digest_path(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};

    if path.is_file() {
        return Ok(format!("{:x}", Sha256::digest(fs::read(path)?)));
    }
    if !path.is_dir() {
        bail!(
            "cannot digest missing or unsupported path {}",
            path.display()
        );
    }
    let mut files = Vec::new();
    collect_digest_files(path, path, &mut files)?;
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, bytes) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn digest_bundle(path: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};

    let mut files = Vec::new();
    collect_digest_files(path, path, &mut files)?;
    files.retain(|(relative, _)| relative != "installation-receipt.json");
    files.sort_by(|left, right| left.0.cmp(&right.0));
    let mut hasher = Sha256::new();
    for (relative, bytes) in files {
        hasher.update(relative.as_bytes());
        hasher.update([0]);
        hasher.update(bytes);
        hasher.update([0]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn collect_digest_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_digest_files(root, &path, files)?;
        } else if path.is_file() {
            files.push((
                path.strip_prefix(root)?
                    .to_string_lossy()
                    .replace('\\', "/"),
                fs::read(path)?,
            ));
        }
    }
    Ok(())
}

#[derive(Debug)]
struct InstallSnapshot {
    target: PathBuf,
    backup: PathBuf,
    existed: bool,
    was_dir: bool,
}

struct InstallTransaction {
    backup_root: PathBuf,
    snapshots: Vec<InstallSnapshot>,
    committed: bool,
}

impl InstallTransaction {
    fn new(backup_root: PathBuf) -> anyhow::Result<Self> {
        fs::create_dir_all(&backup_root)?;
        Ok(Self {
            backup_root,
            snapshots: Vec::new(),
            committed: false,
        })
    }

    fn snapshot(&mut self, target: &Path) -> anyhow::Result<()> {
        if self
            .snapshots
            .iter()
            .any(|snapshot| snapshot.target == target)
        {
            return Ok(());
        }
        let backup = self.backup_root.join(self.snapshots.len().to_string());
        let existed = target.exists();
        let was_dir = target.is_dir();
        if existed {
            if was_dir {
                copy_dir_recursive(target, &backup)?;
            } else {
                if let Some(parent) = backup.parent() {
                    fs::create_dir_all(parent)?;
                }
                fs::copy(target, &backup)?;
            }
        }
        self.snapshots.push(InstallSnapshot {
            target: target.to_path_buf(),
            backup,
            existed,
            was_dir,
        });
        Ok(())
    }

    fn commit(mut self) -> anyhow::Result<()> {
        self.committed = true;
        fs::remove_dir_all(&self.backup_root).or_else(|error| {
            (error.kind() == io::ErrorKind::NotFound)
                .then_some(())
                .ok_or(error)
        })?;
        Ok(())
    }

    fn rollback(&self) -> anyhow::Result<()> {
        for snapshot in self.snapshots.iter().rev() {
            remove_path_if_exists(&snapshot.target)?;
            if snapshot.existed {
                if snapshot.was_dir {
                    copy_dir_recursive(&snapshot.backup, &snapshot.target)?;
                } else {
                    if let Some(parent) = snapshot.target.parent() {
                        fs::create_dir_all(parent)?;
                    }
                    fs::copy(&snapshot.backup, &snapshot.target)?;
                }
            }
        }
        Ok(())
    }
}

impl Drop for InstallTransaction {
    fn drop(&mut self) {
        if !self.committed {
            let _ = self.rollback();
        }
    }
}

fn remove_path_if_exists(path: &Path) -> anyhow::Result<()> {
    if path.is_dir() {
        fs::remove_dir_all(path)?;
    } else if path.exists() {
        fs::remove_file(path)?;
    }
    Ok(())
}

fn activate_bundle_atomically(extracted: &Path, install_root: &Path) -> anyhow::Result<()> {
    let parent = install_root
        .parent()
        .context("install root has no parent")?;
    fs::create_dir_all(parent)?;
    let staging = parent.join(format!(
        ".{}.staging-{}",
        install_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("adapter"),
        std::process::id()
    ));
    remove_path_if_exists(&staging)?;
    copy_dir_recursive(extracted, &staging)?;
    remove_path_if_exists(install_root)?;
    fs::rename(&staging, install_root).with_context(|| {
        format!(
            "failed to atomically activate adapter at {}",
            install_root.display()
        )
    })
}

fn typed_harness_resource_plan(
    bundle: &Path,
    home: &Path,
    manifest_path: &str,
) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    use crate::harness_config::HarnessResourceKind;
    use crate::release_index::AdapterResourceKind;

    let config = read_ldgr_harness_config(home).context(
        "typed adapter resources require a valid ~/.ldgr/config.json; run `ldgr install` first",
    )?;
    let manifest = crate::release_index::parse_resource_manifest(
        &fs::read_to_string(bundle.join(manifest_path)).with_context(|| {
            format!("adapter bundle is missing resource manifest `{manifest_path}`")
        })?,
    )?;
    let mut plan = Vec::<(PathBuf, PathBuf)>::new();
    for resource in manifest.resources {
        let source = bundle.join(&resource.source);
        if !source.exists() {
            bail!(
                "adapter resource source does not exist: {}",
                source.display()
            );
        }
        let kind = match resource.kind {
            AdapterResourceKind::Prompt => HarnessResourceKind::Prompt,
            AdapterResourceKind::Skill => HarnessResourceKind::Skill,
            AdapterResourceKind::Extension => HarnessResourceKind::Extension,
            AdapterResourceKind::Command => HarnessResourceKind::Command,
        };
        for harness in resource.harnesses {
            for root in config.harness_resource_paths(&harness, kind) {
                let root = expand_home_path(home, root.to_string_lossy().as_ref());
                let target = if matches!(
                    kind,
                    HarnessResourceKind::Extension | HarnessResourceKind::Command
                ) && root.extension().is_some()
                {
                    root.parent().unwrap_or(&root).join(&resource.destination)
                } else {
                    root.join(&resource.destination)
                };
                if plan.iter().any(|(_, existing)| existing == &target) {
                    bail!(
                        "adapter resource destination collision: {}",
                        target.display()
                    );
                }
                plan.push((source.clone(), target));
            }
        }
    }
    Ok(plan)
}

fn install_typed_harness_resources(plan: &[(PathBuf, PathBuf)]) -> anyhow::Result<()> {
    for (source, target) in plan {
        if source.is_dir() {
            copy_dir_recursive(source, target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source, target)?;
        }
        println!("├─ Harness resource {}", target.display());
    }
    Ok(())
}

fn resolve_adapter_install_name(name: &str, assume_yes: bool) -> anyhow::Result<String> {
    let normalized = normalize_adapter_name(name);
    if available_adapter_catalog()
        .iter()
        .any(|entry| entry.slug == normalized)
    {
        return Ok(normalized);
    }
    let candidates = adapter_name_suggestions(&normalized);
    match candidates.as_slice() {
        [candidate] => {
            if !assume_yes && stdin_is_terminal() {
                let accepted = Confirm::with_theme(&ColorfulTheme::default())
                    .with_prompt(format!(
                        "Unknown adapter `{}`. Did you mean `{}`?",
                        name, candidate
                    ))
                    .default(false)
                    .interact()?;
                if accepted {
                    return Ok(candidate.clone());
                }
            }
            bail!(
                "unknown adapter `{}`\n\nDid you mean `{}`?\n\nRun:\n  ldgr adapter install {}\n\nAvailable adapters:\n{}",
                name,
                candidate,
                candidate,
                available_adapter_names().join(", ")
            );
        }
        [] => bail!(
            "unknown adapter `{}`; run `ldgr adapter install list`\n\nAvailable adapters: {}",
            name,
            available_adapter_names().join(", ")
        ),
        many => bail!(
            "unknown adapter `{}`; input is ambiguous\n\nPossible adapters: {}\n\nRun `ldgr adapter install <adapter>` with one exact name.",
            name,
            many.join(", ")
        ),
    }
}

fn normalize_adapter_name(name: &str) -> String {
    name.trim()
        .strip_prefix("ldgr-")
        .unwrap_or_else(|| name.trim())
        .to_ascii_lowercase()
}

fn available_adapter_names() -> Vec<String> {
    available_adapter_catalog()
        .iter()
        .map(|entry| entry.slug.to_string())
        .collect()
}

fn adapter_name_suggestions(input: &str) -> Vec<String> {
    let mut scored = available_adapter_catalog()
        .iter()
        .filter_map(|entry| {
            let distance = edit_distance(input, entry.slug);
            let threshold = typo_suggestion_threshold(input.len().max(entry.slug.len()));
            (distance <= threshold).then_some((distance, entry.slug.to_string()))
        })
        .collect::<Vec<_>>();
    scored.sort_by(|left, right| left.0.cmp(&right.0).then_with(|| left.1.cmp(&right.1)));
    let Some(best_distance) = scored.first().map(|(distance, _)| *distance) else {
        return Vec::new();
    };
    scored
        .into_iter()
        .filter(|(distance, _)| *distance == best_distance)
        .map(|(_, slug)| slug)
        .collect()
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

#[derive(Clone, Copy)]
struct GitAdapterSource {
    repo: &'static str,
    package: &'static str,
    binary: &'static str,
}

#[derive(Clone, Copy)]
struct ReleaseAdapterSource {
    repo: &'static str,
    tag_prefix: &'static str,
    asset_prefix: &'static str,
    root_prefix: &'static str,
    binary: &'static str,
}

struct AvailableAdapter {
    slug: &'static str,
    title: &'static str,
    source: &'static str,
    install: &'static str,
    workspace_package: Option<&'static str>,
    git: Option<GitAdapterSource>,
    release: Option<ReleaseAdapterSource>,
}

static AVAILABLE_ADAPTERS: &[AvailableAdapter] = &[
    AvailableAdapter {
        slug: "conduct",
        title: "LDGR Conduct adapter",
        source: "hydra-dynamix/ldgr-releases release bundle",
        install: "ldgr adapter install conduct",
        workspace_package: Some("ldgr-conduct"),
        git: None,
        release: Some(ReleaseAdapterSource {
            repo: "hydra-dynamix/ldgr-releases",
            tag_prefix: "conduct-v",
            asset_prefix: "conduct",
            root_prefix: "conduct",
            binary: "ldgr-conduct",
        }),
    },
    AvailableAdapter {
        slug: "research",
        title: "Research adapter",
        source: "https://github.com/hydra-dynamix/ldgr-research release/git",
        install: "ldgr adapter install research",
        workspace_package: Some("ldgr-research"),
        git: Some(GitAdapterSource {
            repo: "https://github.com/hydra-dynamix/ldgr-research",
            package: "ldgr-research",
            binary: "ldgr-research",
        }),
        release: Some(ReleaseAdapterSource {
            repo: "hydra-dynamix/ldgr-research",
            tag_prefix: "v",
            asset_prefix: "ldgr-research",
            root_prefix: "ldgr-research",
            binary: "ldgr-research",
        }),
    },
    AvailableAdapter {
        slug: "example",
        title: "Public example adapter",
        source: "https://github.com/hydra-dynamix/ldgr-example-adapter release/git",
        install: "ldgr adapter install example",
        workspace_package: Some("ldgr-example-adapter"),
        git: Some(GitAdapterSource {
            repo: "https://github.com/hydra-dynamix/ldgr-example-adapter",
            package: "ldgr-example-adapter",
            binary: "ldgr-example-adapter",
        }),
        release: Some(ReleaseAdapterSource {
            repo: "hydra-dynamix/ldgr-example-adapter",
            tag_prefix: "v",
            asset_prefix: "ldgr-example-adapter",
            root_prefix: "ldgr-example-adapter",
            binary: "ldgr-example-adapter",
        }),
    },
    AvailableAdapter {
        slug: "programbench",
        title: "ProgramBench historical reproduction adapter",
        source: "hydra-dynamix/ldgr-releases release bundle / public git fallback",
        install: "ldgr adapter install programbench",
        workspace_package: None,
        git: Some(GitAdapterSource {
            repo: "https://github.com/hydra-dynamix/ldgr-programbench",
            package: "ldgr-programbench",
            binary: "ldgr-programbench",
        }),
        release: Some(ReleaseAdapterSource {
            repo: "hydra-dynamix/ldgr-releases",
            tag_prefix: "programbench-v",
            asset_prefix: "ldgr-programbench",
            root_prefix: "ldgr-programbench",
            binary: "ldgr-programbench",
        }),
    },
    AvailableAdapter {
        slug: "code",
        title: "Coding adapter",
        source: "",
        install: "ldgr adapter install code",
        workspace_package: Some("ldgr-code"),
        git: None,
        release: Some(commercial_release("code", "ldgr-code")),
    },
    AvailableAdapter {
        slug: "security",
        title: "Security adapter",
        source: "",
        install: "ldgr adapter install security",
        workspace_package: Some("ldgr-security"),
        git: None,
        release: Some(commercial_release("security", "ldgr-security")),
    },
    AvailableAdapter {
        slug: "explore",
        title: "Explore adapter",
        source: "",
        install: "ldgr adapter install explore",
        workspace_package: Some("ldgr-explore"),
        git: None,
        release: Some(commercial_release("explore", "ldgr-explore")),
    },
    AvailableAdapter {
        slug: "bench",
        title: "Bench adapter",
        source: "",
        install: "ldgr adapter install bench",
        workspace_package: Some("ldgr-bench"),
        git: None,
        release: Some(commercial_release("bench", "ldgr-bench")),
    },
];

fn available_adapter_catalog() -> &'static [AvailableAdapter] {
    AVAILABLE_ADAPTERS
}

const fn commercial_release(adapter: &'static str, binary: &'static str) -> ReleaseAdapterSource {
    ReleaseAdapterSource {
        repo: "hydra-dynamix/ldgr-releases",
        tag_prefix: "",
        asset_prefix: adapter,
        root_prefix: adapter,
        binary,
    }
}

pub(crate) fn print_available_adapter_catalog() {
    if std::env::var_os(crate::release_index::ADAPTER_RELEASE_INDEX_ENV).is_some() {
        match crate::release_index::load_configured_release_index() {
            Ok(index) => {
                print_release_index_catalog(&index);
                return;
            }
            Err(error) => {
                eprintln!("warning: {error:#}");
            }
        }
    }
    println!("Available adapters:");
    for entry in available_adapter_catalog() {
        if entry.source.is_empty() {
            println!("  {} — {}", entry.slug, entry.title);
        } else {
            println!("  {} — {} [{}]", entry.slug, entry.title, entry.source);
        }
        println!("    install: {}", entry.install);
        println!("    after install: ldgr {} --help", entry.slug);
    }
    println!("  installed adapters: ldgr adapter list");
    println!("  adapter details: ldgr adapter show <slug>");
}

fn print_release_index_catalog(index: &crate::release_index::AdapterReleaseIndex) {
    println!("Available adapters:");
    for adapter in &index.adapters {
        let source = adapter
            .source_url
            .as_deref()
            .map(|source| format!(" [{source}]"))
            .unwrap_or_default();
        println!("  {} — {}{}", adapter.domain, adapter.title, source);
        println!("    install: ldgr adapter install {}", adapter.domain);
        println!("    after install: ldgr {} --help", adapter.domain);
    }
    println!("  installed adapters: ldgr adapter list");
    println!("  adapter details: ldgr adapter show <slug>");
}

fn install_adapter_from_source_root(
    entry: &AvailableAdapter,
    source_root: &Path,
    install_root: &Path,
) -> anyhow::Result<()> {
    let Some(package) = entry.workspace_package else {
        bail!(
            "adapter `{}` does not have a workspace package; use release/git install instead",
            entry.slug
        );
    };
    install_adapter_from_source_root_with_package(package, source_root, install_root)
}

fn install_adapter_from_source_root_with_package(
    package: &str,
    source_root: &Path,
    install_root: &Path,
) -> anyhow::Result<()> {
    println!("├─ Source checkout {}", source_root.display());
    let status = Command::new("cargo")
        .arg("run")
        .arg("-p")
        .arg(package)
        .arg("--")
        .arg("adapter")
        .arg("install")
        .arg("--install-root")
        .arg(install_root)
        .arg("--print-path")
        .current_dir(source_root)
        .status()?;
    if !status.success() {
        bail!("adapter installer failed for package `{package}` with status {status}");
    }
    patch_adapter_argv_to_source_runner(install_root, package, source_root)?;
    Ok(())
}

fn install_adapter_from_git(
    entry: &AvailableAdapter,
    git: GitAdapterSource,
    install_root: &Path,
) -> anyhow::Result<()> {
    println!("├─ Git source {}", git.repo);
    let mut command = cargo_install_git_command(git);
    run_checked(&mut command, &format!("cargo install {}", git.package))?;
    run_adapter_binary_installer(git.binary, entry.slug, install_root)
}

fn cargo_install_git_command(git: GitAdapterSource) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg("install")
        .arg("--git")
        .arg(git.repo)
        .arg("--locked")
        .arg("--force")
        .arg(git.package);
    command
}

fn run_adapter_binary_installer(
    binary: impl AsRef<std::ffi::OsStr>,
    adapter: &str,
    install_root: &Path,
) -> anyhow::Result<()> {
    let binary_ref = binary.as_ref();
    let status = Command::new(binary_ref)
        .arg("adapter")
        .arg("install")
        .arg("--install-root")
        .arg(install_root)
        .arg("--print-path")
        .status()?;
    if !status.success() {
        bail!(
            "adapter installer `{}` failed for `{adapter}` with status {status}",
            Path::new(binary_ref).display()
        );
    }
    Ok(())
}

fn install_adapter_from_release(
    entry: &AvailableAdapter,
    release: ReleaseAdapterSource,
    install_root: &Path,
    home: &Path,
) -> anyhow::Result<()> {
    let version = env!("CARGO_PKG_VERSION");
    let platform = platform_tag()?;
    let tag = if release.tag_prefix.is_empty() {
        format!("{}-v{}", release.asset_prefix, version)
    } else {
        format!("{}{}", release.tag_prefix, version)
    };
    let archive_name = format!("{}-{}-{}.tar.gz", release.asset_prefix, version, platform);
    let url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        release.repo, tag, archive_name
    );
    println!("├─ Release {}", url);
    let temp = std::env::temp_dir().join(format!(
        "ldgr-adapter-install-{}-{}",
        entry.slug,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;
    let archive = temp.join(&archive_name);
    let download = Command::new("curl")
        .arg("-fsSL")
        .arg(&url)
        .arg("-o")
        .arg(&archive)
        .status();
    match download {
        Ok(status) if status.success() => {}
        _ => {
            if let Some(git) = entry.git {
                println!("├─ Release unavailable for {platform}; falling back to git install");
                return install_adapter_from_git(entry, git, install_root);
            }
            if command_exists(release.binary) {
                println!(
                    "├─ Release unavailable for {platform}; falling back to installed `{}`",
                    release.binary
                );
                return run_adapter_binary_installer(release.binary, entry.slug, install_root);
            }
            bail!(
                "release asset unavailable for adapter `{}` on platform `{}`: {}; install `{}` or pass --source-root for a local source install",
                entry.slug,
                platform,
                url,
                release.binary
            );
        }
    }
    run_checked(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&temp),
        "extract adapter release archive",
    )?;
    let extracted = temp.join(format!("{}-{}", release.root_prefix, version));
    if !extracted.is_dir() {
        bail!(
            "release archive did not contain expected root {}",
            extracted.display()
        );
    }
    let release_binary = extracted.join(&platform).join(release.binary);
    if !release_binary.is_file() && adapter_manifest_references_binary(&extracted, release.binary)?
    {
        bail!(
            "adapter release {} is missing required executable {}/{}; existing installation was left unchanged",
            entry.slug,
            platform,
            release.binary
        );
    }
    let _ = fs::remove_dir_all(install_root);
    copy_dir_recursive(&extracted, install_root)?;
    let installed_binary = install_release_binary(install_root, home, release.binary, &platform)?;
    if let Some(binary_path) = installed_binary {
        println!("├─ Running adapter installer from release binary");
        run_adapter_binary_installer(binary_path.as_os_str(), entry.slug, install_root)?;
    }
    patch_adapter_argv_to_installed_binary(install_root, release.binary, home)?;
    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

fn install_release_binary(
    install_root: &Path,
    home: &Path,
    binary: &str,
    platform: &str,
) -> anyhow::Result<Option<PathBuf>> {
    let source = install_root.join(platform).join(binary);
    if !source.is_file() {
        return Ok(None);
    }
    let bin_dir = home.join(".local/bin");
    fs::create_dir_all(&bin_dir)?;
    let dest = bin_dir.join(binary);
    fs::copy(&source, &dest)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut perms = fs::metadata(&dest)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&dest, perms)?;
    }
    println!("├─ Installed binary {}", dest.display());
    Ok(Some(dest))
}

fn adapter_manifest_references_binary(install_root: &Path, binary: &str) -> anyhow::Result<bool> {
    let manifest_path = install_root.join("adapter.toml");
    if !manifest_path.is_file() {
        return Ok(false);
    }
    let manifest = fs::read_to_string(&manifest_path)?;
    let manifest_binary = binary.strip_suffix(".exe").unwrap_or(binary);
    let value: toml::Value = toml::from_str(&manifest)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    Ok(value
        .get("commands")
        .and_then(toml::Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|command| command.get("argv"))
        .filter_map(toml::Value::as_array)
        .filter_map(|argv| argv.first())
        .filter_map(toml::Value::as_str)
        .any(|command| command == manifest_binary || command == binary))
}

fn patch_adapter_argv_to_source_runner(
    install_root: &Path,
    package: &str,
    source_root: &Path,
) -> anyhow::Result<()> {
    let manifest = install_root.join("adapter.toml");
    if !manifest.is_file() {
        return Ok(());
    }
    let cargo_manifest = source_root.join("Cargo.toml");
    let target_dir = install_root.join("source-target");
    let source_runner = [
        "cargo".to_string(),
        "run".to_string(),
        "--quiet".to_string(),
        "--manifest-path".to_string(),
        cargo_manifest.display().to_string(),
        "--target-dir".to_string(),
        target_dir.display().to_string(),
        "-p".to_string(),
        package.to_string(),
        "--".to_string(),
    ]
    .into_iter()
    .map(|part| toml::Value::String(part).to_string())
    .collect::<Vec<_>>()
    .join(", ");
    patch_adapter_argv_command(&manifest, package, &source_runner)
}

fn patch_adapter_argv_to_installed_binary(
    install_root: &Path,
    binary: &str,
    home: &Path,
) -> anyhow::Result<()> {
    let manifest = install_root.join("adapter.toml");
    if !manifest.is_file() {
        return Ok(());
    }
    let bin_path = home.join(".local/bin").join(binary);
    if !bin_path.is_file() {
        return Ok(());
    }
    let quoted_path = toml::Value::String(bin_path.display().to_string()).to_string();
    let manifest_binary = binary.strip_suffix(".exe").unwrap_or(binary);
    patch_adapter_argv_command(&manifest, manifest_binary, &quoted_path)
}

fn patch_adapter_argv_command(
    manifest: &Path,
    binary: &str,
    replacement: &str,
) -> anyhow::Result<()> {
    let quoted_binary = format!("\"{}\"", binary);
    let text = fs::read_to_string(manifest)?;
    let patched = text
        .lines()
        .map(|line| {
            if line.trim_start().starts_with("argv =") {
                line.replace(&quoted_binary, replacement)
            } else {
                line.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
        + "\n";
    fs::write(manifest, patched)?;
    Ok(())
}

fn platform_tag() -> anyhow::Result<String> {
    let os = std::env::consts::OS;
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("unsupported adapter release architecture `{other}`"),
    };
    match os {
        "linux" => Ok(format!("linux-{arch}")),
        "macos" => Ok(format!("macos-{arch}")),
        "windows" => Ok(format!("windows-{arch}")),
        other => bail!("unsupported adapter release OS `{other}`"),
    }
}

fn run_checked(command: &mut Command, label: &str) -> anyhow::Result<()> {
    let status = command.status()?;
    if !status.success() {
        bail!("{label} failed with status {status}");
    }
    Ok(())
}

fn command_exists(binary: &str) -> bool {
    Command::new(binary)
        .arg("--version")
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|status| status.success())
        .unwrap_or(false)
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
    let config = read_ldgr_harness_config(home);
    let prompts = install_root.join("prompts");
    if prompts.is_dir() {
        for prompt_root in configured_prompt_dirs(home, &config) {
            copy_directory_children(&prompts, &prompt_root)?;
            println!("├─ Harness prompts {}", prompt_root.display());
        }
    }
    let skills = install_root.join("skills");
    if skills.is_dir() {
        for skill_root in configured_skill_dirs(home, &config) {
            copy_directory_children(&skills, &skill_root)?;
            println!("├─ Harness skills {}", skill_root.display());
        }
    }
    let extensions = install_root.join("extensions");
    if extensions.is_dir() {
        for extension_root in configured_extension_dirs(home, &config) {
            copy_directory_children(&extensions, &extension_root)?;
            println!("├─ Harness extensions {}", extension_root.display());
        }
    }
    let marker = home.join(".ldgr/installed-adapters").join(adapter);
    write_file(
        &marker,
        &format!("install_root={}\n", install_root.display()),
    )?;
    Ok(())
}

fn read_ldgr_harness_config(home: &Path) -> Option<crate::harness_config::HarnessConfig> {
    let text = fs::read_to_string(home.join(".ldgr/config.json")).ok()?;
    crate::harness_config::parse_harness_config(&text).ok()
}

fn configured_prompt_dirs(
    home: &Path,
    config: &Option<crate::harness_config::HarnessConfig>,
) -> Vec<PathBuf> {
    let mut dirs = configured_path_dirs(
        home,
        config,
        crate::harness_config::HarnessResourceKind::Prompt,
    );
    if dirs.is_empty() {
        dirs.push(home.join(".ldgr/prompts"));
    }
    dedup_paths(dirs)
}

fn configured_skill_dirs(
    home: &Path,
    config: &Option<crate::harness_config::HarnessConfig>,
) -> Vec<PathBuf> {
    let mut dirs = configured_path_dirs(
        home,
        config,
        crate::harness_config::HarnessResourceKind::Skill,
    );
    if dirs.is_empty() {
        dirs.push(home.join(".pi/agent/skills"));
    }
    dedup_paths(dirs)
}

fn configured_extension_dirs(
    home: &Path,
    config: &Option<crate::harness_config::HarnessConfig>,
) -> Vec<PathBuf> {
    let mut dirs = configured_path_dirs(
        home,
        config,
        crate::harness_config::HarnessResourceKind::Extension,
    )
    .into_iter()
    .map(|path| {
        if path.extension().is_some() {
            path.parent().map(Path::to_path_buf).unwrap_or(path)
        } else {
            path
        }
    })
    .collect::<Vec<_>>();
    if dirs.is_empty() {
        dirs.push(home.join(".pi/agent/extensions"));
    }
    dedup_paths(dirs)
}

fn configured_path_dirs(
    home: &Path,
    config: &Option<crate::harness_config::HarnessConfig>,
    kind: crate::harness_config::HarnessResourceKind,
) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    if let Some(config) = config {
        dirs.extend(
            config
                .resource_paths(kind)
                .iter()
                .map(|path| expand_home_path(home, path.to_string_lossy().as_ref())),
        );
    }
    dirs
}

fn expand_home_path(home: &Path, value: &str) -> PathBuf {
    if value == "~" {
        home.to_path_buf()
    } else if let Some(rest) = value.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(value)
    }
}

fn dedup_paths(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut deduped = Vec::new();
    for path in paths {
        if !deduped.iter().any(|existing| existing == &path) {
            deduped.push(path);
        }
    }
    deduped
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

fn select_adapters(args: &InstallArgs) -> anyhow::Result<Vec<String>> {
    if !args.adapter.is_empty() {
        let adapters = args
            .adapter
            .iter()
            .map(|adapter| normalize_adapter_name(adapter))
            .collect::<Vec<_>>();
        println!("◇ Using adapters from flags: {}", adapters.join(", "));
        return Ok(adapters);
    }
    if args.yes || !stdin_is_terminal() {
        return Ok(Vec::new());
    }
    select_adapter_bundles()
}

fn select_adapter_bundles() -> anyhow::Result<Vec<String>> {
    let entries = available_adapter_catalog();
    let items = entries
        .iter()
        .map(|entry| {
            if entry.source.is_empty() {
                format!("{} — {}", entry.slug, entry.title)
            } else {
                format!("{} — {} [{}]", entry.slug, entry.title, entry.source)
            }
        })
        .collect::<Vec<_>>();
    let theme = ColorfulTheme::default();
    let Some(selections) = MultiSelect::with_theme(&theme)
        .with_prompt(
            "Which adapter bundles would you like to install? (Space to select, Enter to submit, Esc to skip)",
        )
        .items(&items)
        .defaults(&vec![false; items.len()])
        .interact_opt()? else {
        return Ok(Vec::new());
    };
    Ok(selections
        .into_iter()
        .filter_map(|index| entries.get(index).map(|entry| entry.slug.to_string()))
        .collect())
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
    let config_path = home.join(".agentctl/config.toml");
    let config = if config_path.is_file() {
        let existing = fs::read_to_string(&config_path)?;
        merge_agentctl_config(&existing, harnesses)?
    } else {
        render_agentctl_config(harnesses)
    };
    write_file(&config_path, &config)?;
    println!("├─ agentctl config {}", config_path.display());
    Ok(serde_json::json!({
        "path": config_path,
        "agents": harnesses.iter().map(|harness| harness_name(*harness)).collect::<Vec<_>>(),
        "task": "ldgr-loop",
        "note": "agentctl is the canonical LDGR agent control plane; ldgr loop run --agent agentctl runs `agentctl run ldgr-loop` with the rendered prompt on stdin."
    }))
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
struct AgentctlCommandSpec {
    argv: Vec<&'static str>,
    prompt_stdin: bool,
}

fn render_agentctl_config(harnesses: &[HarnessKind]) -> String {
    let mut config = default_agentctl_config_value();
    add_ldgr_agentctl_agents(&mut config, harnesses)
        .expect("default agentctl config should accept LDGR agents");
    toml::to_string_pretty(&config).expect("default agentctl config should serialize")
}

fn merge_agentctl_config(existing: &str, harnesses: &[HarnessKind]) -> anyhow::Result<String> {
    let mut config = if existing.trim().is_empty() {
        default_agentctl_config_value()
    } else {
        toml::from_str(existing).context("failed to parse existing agentctl config")?
    };
    add_ldgr_agentctl_agents(&mut config, harnesses)?;
    toml::to_string_pretty(&config).context("failed to serialize agentctl config")
}

fn default_agentctl_config_value() -> toml::Value {
    toml::from_str(
        r#"[summary]
max_output_bytes = 16384
tail_bytes = 4096
max_preview_lines = 12

[agents.codex]
command = ["codex", "exec", "--sandbox", "workspace-write"]
prompt_stdin = true

[agents.claude-code]
command = ["claude", "-p"]
prompt_stdin = false

[agents.claude]
command = ["claude", "-p"]
prompt_stdin = false

[agents.ollama]
command = ["ollama", "run", "llama3"]
prompt_stdin = true

[agents.openai-rest]
command = ["openai-rest-agent"]
prompt_stdin = true

[agents.openai-websocket]
command = ["openai-websocket-agent"]
prompt_stdin = true
"#,
    )
    .expect("embedded default agentctl config should parse")
}

fn add_ldgr_agentctl_agents(
    config: &mut toml::Value,
    harnesses: &[HarnessKind],
) -> anyhow::Result<()> {
    let root = config
        .as_table_mut()
        .context("agentctl config root must be a TOML table")?;
    root.entry("summary".to_string())
        .or_insert_with(|| default_agentctl_config_value()["summary"].clone());
    let agents = root
        .entry("agents".to_string())
        .or_insert_with(|| toml::Value::Table(toml::map::Map::new()))
        .as_table_mut()
        .context("agentctl config [agents] must be a table")?;

    let primary = harnesses.first().copied().unwrap_or(HarnessKind::Pi);
    let primary_command = agentctl_primary_command(primary);
    agents.insert(
        "ldgr-loop".to_string(),
        agentctl_agent_value(&primary_command),
    );
    agents.insert(
        "ldgr-summary".to_string(),
        agentctl_agent_value(&primary_command),
    );
    for harness in harnesses {
        agents.insert(
            format!("ldgr-loop-{}", harness_name(*harness)),
            agentctl_agent_value(&agentctl_primary_command(*harness)),
        );
    }
    Ok(())
}

fn agentctl_agent_value(command: &AgentctlCommandSpec) -> toml::Value {
    let mut table = toml::map::Map::new();
    table.insert(
        "command".to_string(),
        toml::Value::Array(
            command
                .argv
                .iter()
                .map(|part| toml::Value::String((*part).to_string()))
                .collect(),
        ),
    );
    table.insert(
        "prompt_stdin".to_string(),
        toml::Value::Boolean(command.prompt_stdin),
    );
    toml::Value::Table(table)
}

fn agentctl_commands_for_harness(harness: HarnessKind) -> Vec<AgentctlCommandSpec> {
    match harness {
        HarnessKind::Pi => vec![AgentctlCommandSpec {
            argv: vec!["pi", "-p"],
            prompt_stdin: false,
        }],
        HarnessKind::Codex => vec![AgentctlCommandSpec {
            argv: vec!["codex", "exec", "--sandbox", "workspace-write"],
            prompt_stdin: true,
        }],
        HarnessKind::Claude => vec![AgentctlCommandSpec {
            argv: vec!["claude", "-p"],
            prompt_stdin: false,
        }],
        HarnessKind::Openclaw => vec![
            AgentctlCommandSpec {
                argv: vec!["openclaw", "run"],
                prompt_stdin: false,
            },
            AgentctlCommandSpec {
                argv: vec!["opencode", "run"],
                prompt_stdin: false,
            },
        ],
    }
}

fn agentctl_primary_command(harness: HarnessKind) -> AgentctlCommandSpec {
    agentctl_commands_for_harness(harness)
        .into_iter()
        .next()
        .unwrap_or_else(|| AgentctlCommandSpec {
            argv: vec!["pi", "-p"],
            prompt_stdin: false,
        })
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
        "skill_paths": [home.join(".pi/agent/skills")],
        "reload": "Run /reload in Pi, then use /ldgr <args>, /ldgr-context, or /run-loop [adapter] [loop args]."
    }))
}

fn install_codex_harness(home: &Path) -> anyhow::Result<serde_json::Value> {
    let doc = home.join(".codex/ldgr/LDGR.md");
    write_file(&doc, LDGR_HARNESS_GUIDE)?;
    let instructions = home.join(".codex/prompts/ldgr-core.md");
    write_file(&instructions, CODEX_INSTRUCTIONS)?;
    println!("├─ Codex guide {}", doc.display());
    println!("├─ Codex prompt {}", instructions.display());
    Ok(serde_json::json!({
        "harness": "codex",
        "prompt_paths": [home.join(".codex/prompts")],
        "skill_paths": [home.join(".codex/skills")],
        "prompt_file": instructions,
        "guide_path": doc,
        "extension_equivalent": "Codex CLI has plugin/MCP surfaces, but no local Pi-style slash-command extension was detected; LDGR installs global prompts, skills, and a guide instead."
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

Adapter installers should read `~/.ldgr/config.json`, validate their own license when applicable, install adapter bundle files under `~/.ldgr/adapters/<adapter>` by default, and install adapter-owned prompts, skills, commands, and extensions into the paths declared by each configured harness entry.
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
    _artifact_root: &Path,
    args: StatusArgs,
) -> anyhow::Result<()> {
    let context = read_context(connection)?;
    let status = build_status_summary(
        connection,
        &context,
        args.program.as_deref(),
        args.priority.as_deref(),
        args.recent,
        args.width,
        args.full,
    )?;
    emit(args.json, &status, print_status_summary)?;
    Ok(())
}

pub fn handle_context(
    connection: &rusqlite::Connection,
    _artifact_root: &Path,
    args: ContextArgs,
) -> anyhow::Result<()> {
    let context = read_context(connection)?;
    if args.brief {
        let brief = brief_context(&context, brief_options(args.recent, args.width));
        return emit(args.json, &brief, print_brief_context);
    }
    if args.json {
        let mut value = serde_json::to_value(&context)?;
        value["installed_adapter_namespaces"] =
            serde_json::to_value(AdapterRegistry::discover().installed_domains())?;
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        print_context(&context);
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
    for domain in registry.installed_domains() {
        println!(
            "- adapter={} namespace={} command={}",
            domain.adapter, domain.namespace, domain.command
        );
        println!("  instruction: {}", domain.instruction);
        if let Some(status_command) = &domain.status_command {
            println!("  status_command: {status_command}");
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
            let summary_agent = resolve_summary_agent(&args)?;
            let prompt = resolve_loop_prompt(connection, &args)?;
            let options = LoopRuntimeOptions {
                prompt,
                agent,
                audit_argv: args
                    .audit_argv
                    .as_deref()
                    .map(parse_argv_json)
                    .transpose()?,
                summary_agent,
                summary_log: args.summary_log.clone(),
                project_complete_requested: args.project_complete_requested,
                dry_run: args.dry_run,
                stream_agent_output: args.stream_agent_output,
                agent_timeout: Duration::from_secs(args.agent_timeout_seconds),
            };
            let mut completed_iterations = 0_u32;
            let max_iterations = if args.until_empty {
                u32::MAX
            } else {
                args.max_iterations
            };
            for iteration in 1..=max_iterations {
                match run_loop_once(connection, artifact_root, &options)? {
                    LoopRuntimeOutcome::Completed(result) => {
                        print_loop_result(&result);
                        completed_iterations += 1;
                        if loop_result_failed(&result, &options) {
                            if args.until_empty || args.max_iterations > 1 {
                                println!(
                                    "Loop stopped after {completed_iterations} iteration(s) because a subprocess failed."
                                );
                            }
                            break;
                        }
                        if !args.until_empty
                            && iteration == args.max_iterations
                            && args.max_iterations > 1
                        {
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
                        if args.until_empty || args.max_iterations > 1 {
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
If your agent harness is Pi, run `/reload` so `/ldgr <args>`, `/ldgr-context`, and `/run-loop` become available. `/ldgr` runs the LDGR CLI in the project and pipes stdout/stderr back into the conversation; with no args it runs `ldgr context --brief`. `/run-loop [adapter] [loop args]` selects an installed adapter loop prompt and runs `ldgr loop run --agent agentctl --until-empty --summary-agent agentctl`, launching one fresh worker agent per LDGR work item and one separate fresh summarizer call per completed cycle until no pending work remains or the loop blocks.\n\n\
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

fn resolve_summary_agent(args: &LoopRunArgs) -> anyhow::Result<Option<LoopAgent>> {
    if args.dry_run {
        return Ok(None);
    }
    if let Some(summary_argv) = args.summary_argv.as_deref() {
        if args.summary_agent.is_some() {
            bail!("--summary-agent and --summary-argv are mutually exclusive");
        }
        return Ok(Some(LoopAgent::Argv(parse_argv_json(summary_argv)?)));
    }
    Ok(args.summary_agent.map(|CliLoopAgent::Agentctl| {
        LoopAgent::Argv(vec![
            "agentctl".to_owned(),
            "run".to_owned(),
            std::env::var("LDGR_SUMMARY_AGENTCTL_TASK")
                .unwrap_or_else(|_| "ldgr-summary".to_owned()),
        ])
    }))
}

fn loop_result_failed(result: &LoopRuntimeResult, options: &LoopRuntimeOptions) -> bool {
    if options.dry_run {
        return false;
    }
    result.agent_exit_code != Some(0)
        || (options.project_complete_requested && result.audit_exit_code != Some(0))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_typo_suggestion_handles_conduct_transposition() {
        assert_eq!(
            adapter_name_suggestions("coduct"),
            vec!["conduct".to_string()]
        );
    }

    #[test]
    fn adapter_typo_suggestion_is_empty_for_unrelated_input() {
        assert!(adapter_name_suggestions("xyzzy").is_empty());
    }

    #[test]
    fn edit_distance_counts_single_deletion() {
        assert_eq!(edit_distance("coduct", "conduct"), 1);
    }

    #[test]
    fn cargo_git_install_uses_positional_crate_name() {
        let command = cargo_install_git_command(GitAdapterSource {
            repo: "https://github.com/hydra-dynamix/ldgr-research",
            package: "ldgr-research",
            binary: "ldgr-research",
        });
        let args = command
            .get_args()
            .map(|arg| arg.to_string_lossy().to_string())
            .collect::<Vec<_>>();
        assert_eq!(
            args,
            vec![
                "install".to_string(),
                "--git".to_string(),
                "https://github.com/hydra-dynamix/ldgr-research".to_string(),
                "--locked".to_string(),
                "--force".to_string(),
                "ldgr-research".to_string(),
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--package"));
    }

    #[test]
    fn default_index_failure_falls_back_only_for_unconstrained_online_installs() {
        let make_args = || InstallAdapterArgs {
            name: "research".to_string(),
            source_root: None,
            install_root: None,
            version: None,
            prerelease: false,
            offline: false,
            yes: true,
        };
        let base = make_args();
        assert!(default_catalog_fallback_allowed(&base, false));
        assert!(!default_catalog_fallback_allowed(&base, true));

        let mut offline = make_args();
        offline.offline = true;
        assert!(!default_catalog_fallback_allowed(&offline, false));

        let mut exact = make_args();
        exact.version = Some("0.1.4".to_string());
        assert!(!default_catalog_fallback_allowed(&exact, false));

        let mut prerelease = make_args();
        prerelease.prerelease = true;
        assert!(!default_catalog_fallback_allowed(&prerelease, false));
    }

    #[test]
    fn workspace_adapters_expose_source_root_recovery_packages() {
        for (slug, package) in [
            ("code", "ldgr-code"),
            ("security", "ldgr-security"),
            ("explore", "ldgr-explore"),
            ("bench", "ldgr-bench"),
        ] {
            let adapter = available_adapter_catalog()
                .iter()
                .find(|adapter| adapter.slug == slug)
                .expect("adapter is catalogued");
            assert_eq!(adapter.workspace_package, Some(package));
        }
    }

    #[test]
    fn agentctl_config_defines_ldgr_loop_agents_for_current_cli() {
        let config = render_agentctl_config(&[HarnessKind::Pi, HarnessKind::Codex]);
        assert!(config.contains("[agents.ldgr-loop]"));
        assert!(config.contains("[agents.ldgr-loop-pi]"));
        assert!(config.contains("[agents.ldgr-loop-codex]"));
        let parsed =
            toml::from_str::<toml::Value>(&config).expect("agentctl config should parse as TOML");
        let agents = parsed["agents"].as_table().expect("agents table");
        assert_eq!(
            agents["ldgr-loop"]["command"].as_array().expect("command"),
            &vec![
                toml::Value::String("pi".to_string()),
                toml::Value::String("-p".to_string()),
            ]
        );
        assert_eq!(agents["ldgr-loop"]["prompt_stdin"].as_bool(), Some(false));
        assert_eq!(
            agents["ldgr-loop-codex"]["command"]
                .as_array()
                .expect("command"),
            &vec![
                toml::Value::String("codex".to_string()),
                toml::Value::String("exec".to_string()),
                toml::Value::String("--sandbox".to_string()),
                toml::Value::String("workspace-write".to_string()),
            ]
        );
        assert_eq!(
            agents["ldgr-loop-codex"]["prompt_stdin"].as_bool(),
            Some(true)
        );
    }

    #[test]
    fn agentctl_config_merge_preserves_existing_agents() -> anyhow::Result<()> {
        let merged = merge_agentctl_config(
            r#"[summary]
max_output_bytes = 99
tail_bytes = 10
max_preview_lines = 3

[agents.custom]
command = ["custom-agent"]
prompt_stdin = true
"#,
            &[HarnessKind::Pi],
        )?;
        let parsed = toml::from_str::<toml::Value>(&merged)?;
        let agents = parsed["agents"].as_table().expect("agents table");
        assert!(agents.contains_key("custom"));
        assert!(agents.contains_key("ldgr-loop"));
        assert_eq!(parsed["summary"]["max_output_bytes"].as_integer(), Some(99));
        Ok(())
    }

    #[test]
    fn adapter_harness_assets_install_central_prompts() -> anyhow::Result<()> {
        let install_root = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        std::fs::create_dir_all(install_root.path().join("prompts"))?;
        std::fs::write(
            install_root.path().join("prompts/research-loop.md"),
            "prompt",
        )?;

        install_adapter_harness_assets("research", install_root.path(), home.path())?;

        assert_eq!(
            std::fs::read_to_string(home.path().join(".ldgr/prompts/research-loop.md"))?,
            "prompt"
        );
        assert!(home
            .path()
            .join(".ldgr/installed-adapters/research")
            .is_file());
        Ok(())
    }

    #[test]
    fn adapter_harness_assets_follow_configured_prompt_paths() -> anyhow::Result<()> {
        let install_root = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        std::fs::create_dir_all(install_root.path().join("prompts"))?;
        std::fs::write(
            install_root.path().join("prompts/research-loop.md"),
            "prompt",
        )?;
        std::fs::create_dir_all(home.path().join(".ldgr"))?;
        std::fs::write(
            home.path().join(".ldgr/config.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "installed": [{
                    "harness": "codex",
                    "prompt_paths": [home.path().join(".codex/prompts")]
                }]
            }))?,
        )?;

        install_adapter_harness_assets("research", install_root.path(), home.path())?;

        assert_eq!(
            std::fs::read_to_string(home.path().join(".codex/prompts/research-loop.md"))?,
            "prompt"
        );
        assert!(!home.path().join(".ldgr/prompts/research-loop.md").exists());
        Ok(())
    }

    #[test]
    fn source_root_install_patches_adapter_argv_to_cargo_runner() -> anyhow::Result<()> {
        let install_root = tempfile::tempdir()?;
        let source_root = tempfile::tempdir()?;
        std::fs::write(source_root.path().join("Cargo.toml"), "[workspace]\n")?;
        std::fs::write(
            install_root.path().join("adapter.toml"),
            r#"[adapter]
slug = "conduct"

[[commands]]
namespace = "conduct"
argv = ["ldgr-conduct"]

[[commands]]
namespace = "conduct-status"
argv = ["ldgr-conduct", "status"]
"#,
        )?;

        patch_adapter_argv_to_source_runner(
            install_root.path(),
            "ldgr-conduct",
            source_root.path(),
        )?;
        let manifest = std::fs::read_to_string(install_root.path().join("adapter.toml"))?;
        assert!(manifest.contains("argv = [\"cargo\", \"run\", \"--quiet\", \"--manifest-path\""));
        assert!(manifest.contains(&format!(
            "\"{}\"",
            source_root.path().join("Cargo.toml").display()
        )));
        assert!(manifest.contains("\"--target-dir\""));
        assert!(manifest.contains(&format!(
            "\"{}\"",
            install_root.path().join("source-target").display()
        )));
        assert!(manifest.contains("\"-p\", \"ldgr-conduct\", \"--\"]"));
        assert!(manifest.contains("\"--\", \"status\"]"));
        toml::from_str::<toml::Value>(&manifest).expect("patched manifest should parse as TOML");
        Ok(())
    }

    #[test]
    fn installed_windows_binary_patches_extensionless_manifest_command() -> anyhow::Result<()> {
        let install_root = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        std::fs::create_dir_all(home.path().join(".local/bin"))?;
        std::fs::write(home.path().join(".local/bin/ldgr-code.exe"), b"binary")?;
        std::fs::write(
            install_root.path().join("adapter.toml"),
            r#"[adapter]
slug = "code"

[[commands]]
namespace = "code"
argv = ["ldgr-code"]
"#,
        )?;

        patch_adapter_argv_to_installed_binary(install_root.path(), "ldgr-code.exe", home.path())?;

        let manifest = std::fs::read_to_string(install_root.path().join("adapter.toml"))?;
        assert!(manifest.contains("ldgr-code.exe"));
        assert!(!manifest.contains("argv = [\"ldgr-code\"]"));
        toml::from_str::<toml::Value>(&manifest).expect("patched manifest should parse as TOML");
        Ok(())
    }

    #[test]
    fn adapter_manifest_detects_required_release_executable() -> anyhow::Result<()> {
        let install_root = tempfile::tempdir()?;
        std::fs::write(
            install_root.path().join("adapter.toml"),
            r#"[adapter]
slug = "code"

[[commands]]
namespace = "code"
argv = ["ldgr-code"]
"#,
        )?;
        assert!(adapter_manifest_references_binary(
            install_root.path(),
            "ldgr-code"
        )?);
        assert!(adapter_manifest_references_binary(
            install_root.path(),
            "ldgr-code.exe"
        )?);
        assert!(!adapter_manifest_references_binary(
            install_root.path(),
            "ldgr-research"
        )?);
        Ok(())
    }
}
