use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect};
use sha2::{Digest, Sha256};

use crate::adapter_registry::AdapterRegistry;
use crate::loop_invariants::{LOOP_INVARIANTS_PROMPT, LOOP_INVARIANTS_PROMPT_FILE};
use crate::loop_runtime::{
    run_loop_once, LoopAgent, LoopPromptSource, LoopRuntimeOptions, LoopRuntimeOutcome,
    LoopRuntimeResult,
};
use crate::store::{init_store, read_context_with_conduct_lifecycle};
use crate::tool_runner::parse_argv_json;
use crate::web::{generate_control_token, serve, WebOptions};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};

use super::super::args::{
    CliLoopAgent, ContextArgs, HarnessKind, InstallAdapterArgs, InstallArgs, InstallCommand,
    LoopArgs, LoopCommand, LoopRunArgs, StatusArgs, UpdateArgs, WebArgs,
};
use super::super::render::brief_context::{
    brief_context, print_brief_context, BriefContextOptions,
};
use super::super::render::context::print_context;
use super::super::render::emit;
use super::super::render::text::print_loop_result;
use super::super::{CLI_DEFAULT_HELP_SECTIONS, INIT_PROJECT_SETUP_PROMPT};

const LDGR_CONTEXT_EXTENSION: &str = include_str!("../../../extensions/ldgr-context.ts");
const LDGR_CORE_LOOP_PROMPT: &str = include_str!("../../../prompts/loop-prompt.md");
const LDGR_LOOP_PLANNER_PROMPT: &str = include_str!("../../../prompts/ldgr-loop-planner.md");
const LDGR_LOOP_WORKER_PROMPT: &str = include_str!("../../../prompts/ldgr-loop-worker.md");
const LDGR_LOOP_SCRYB_PROMPT: &str = include_str!("../../../prompts/ldgr-loop-scryb.md");
const LDGR_LOOP_VALIDATOR_PROMPT: &str = include_str!("../../../prompts/ldgr-loop-validator.md");
const LDGR_CORE_LOOP_PROMPT_FILE: &str = "ldgr-core-loop.md";
const AGENTCTL_REPO: &str = "https://github.com/hydra-dynamix/agentctl";
const LDGR_CORE_REPO: &str = "https://github.com/hydra-dynamix/ldgr-core";
const CURRENT_LICENSE_YEAR: &str = "2026";
const FULL_LICENSE_FEATURE: &str = "full";
const LDGR_CORE_API_VERSION: &str = env!("CARGO_PKG_VERSION");

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
    let prompt_root = ldgr_home.join("prompts");
    seed_core_prompt_files(&prompt_root)?;
    let core_loop_prompt = prompt_root.join(LDGR_CORE_LOOP_PROMPT_FILE);
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
        "adapter_files": {
            "default_global_path": "~/.ldgr/<adapter>",
            "note": "Adapter bundle files install globally under ~/.ldgr/<adapter>; adapter-owned skills/extensions install into the configured harness locations."
        },
        "licenses": {},
        "notes": "Adapters should read this file, validate their own license when applicable, use licenses.<product>.license_file and licenses.<product>.keyring_file for offline commercial license paths, install adapter bundle files under ~/.ldgr/<adapter> by default, then install adapter-owned skills/extensions into the configured harness locations."
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
        println!("│  Run /reload in Pi, then use /ldgr <args>, /ldgr-context, or /run-loop.");
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
    let adapters = select_adapters(&args)?;
    if !adapters.is_empty() {
        println!("│");
        println!("◇ Installing adapter bundles...");
        for adapter in adapters {
            handle_install_adapter(&InstallAdapterArgs {
                name: adapter,
                source_root: None,
                install_root: None,
                adapter_version: None,
                yes: args.yes,
            })?;
        }
    }
    println!("└─ Adapter bundles install under ~/.ldgr/<adapter>.");
    Ok(())
}

pub(crate) fn handle_interactive_adapter_install(
    source_root: Option<PathBuf>,
    install_root: Option<PathBuf>,
    adapter_version: Option<String>,
    yes: bool,
) -> anyhow::Result<()> {
    if yes || !stdin_is_terminal() {
        print_available_adapter_catalog();
        println!("\nRun `ldgr adapter install <adapter>` to install one adapter, or run `ldgr adapter install` in an interactive terminal for the selection menu.");
        return Ok(());
    }
    if install_root.is_some() || adapter_version.is_some() {
        bail!("--install-root and --adapter-version require an adapter name; run `ldgr adapter install <adapter> --install-root <path> --adapter-version <version>`");
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
            adapter_version: None,
            yes,
        })?;
    }
    Ok(())
}

pub(crate) fn handle_install_adapter(args: &InstallAdapterArgs) -> anyhow::Result<()> {
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
        .unwrap_or_else(|| home.join(".ldgr").join(&adapter));
    println!("◇ Installing LDGR adapter `{adapter}`");
    println!("├─ Install root {}", install_root.display());
    if let Some(source_root) = &args.source_root {
        install_adapter_from_source_root(entry, source_root, &install_root)?;
    } else if let Some(git) = entry.git {
        install_adapter_from_git(entry, git, &install_root, &home)?;
    } else if let Some(release) = entry.release {
        install_adapter_from_release(
            entry,
            release,
            &install_root,
            &home,
            args.adapter_version.as_deref(),
        )?;
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

pub fn handle_update(args: UpdateArgs) -> anyhow::Result<()> {
    println!("◇ Updating LDGR");
    if args.dry_run {
        println!("├─ dry-run: no commands will be executed");
    }
    if args.skip_core {
        println!("├─ Skipped core update (--skip-core)");
    } else {
        update_core(args.dry_run)?;
    }
    if args.skip_adapters {
        println!("├─ Skipped adapter updates (--skip-adapters)");
    } else {
        update_installed_adapters(args.source_root.as_deref(), args.dry_run)?;
    }
    println!("└─ Update complete");
    Ok(())
}

fn update_core(dry_run: bool) -> anyhow::Result<()> {
    let mut command = Command::new("cargo");
    command
        .arg("install")
        .arg("--git")
        .arg(LDGR_CORE_REPO)
        .arg("--locked")
        .arg("--force")
        .arg("ldgr-core");
    if dry_run {
        println!(
            "├─ would update core: cargo install --git {LDGR_CORE_REPO} --locked --force ldgr-core"
        );
        return Ok(());
    }
    println!("├─ Updating core from {LDGR_CORE_REPO}");
    run_checked(&mut command, "update ldgr core")
}

fn update_installed_adapters(source_root: Option<&Path>, dry_run: bool) -> anyhow::Result<()> {
    let registry = AdapterRegistry::discover();
    for warning in &registry.warnings {
        eprintln!(
            "warning: skipped adapter manifest {}: {}",
            warning.manifest_path.display(),
            warning.message
        );
    }
    if registry.adapters.is_empty() {
        println!("├─ No installed adapters discovered");
        return Ok(());
    }
    let home = home_dir()?;
    let today = today_iso_date();
    for adapter in &registry.adapters {
        let Some(entry) = available_adapter_catalog()
            .iter()
            .find(|entry| entry.slug == adapter.slug)
        else {
            println!(
                "├─ Skipped adapter `{}`: not in update catalog",
                adapter.slug
            );
            continue;
        };
        if let Some(reason) = adapter_update_skip_reason(entry, &home, &today)? {
            println!("├─ Skipped adapter `{}`: {reason}", adapter.slug);
            continue;
        }
        println!(
            "├─ Updating adapter `{}` at {}",
            adapter.slug,
            adapter.root_path.display()
        );
        if dry_run {
            println!("│  would reinstall adapter `{}`", adapter.slug);
            continue;
        }
        handle_install_adapter(&InstallAdapterArgs {
            name: adapter.slug.clone(),
            source_root: source_root.map(Path::to_path_buf),
            install_root: Some(adapter.root_path.clone()),
            adapter_version: None,
            yes: true,
        })?;
    }
    Ok(())
}

fn adapter_update_skip_reason(
    entry: &AvailableAdapter,
    home: &Path,
    today: &str,
) -> anyhow::Result<Option<String>> {
    let Some(release) = entry.release else {
        return Ok(None);
    };
    if release.repo != "hydra-dynamix/ldgr-releases" {
        return Ok(None);
    }
    let product = release.binary;
    match check_update_license(home, entry.slug, product, today) {
        LicenseUpdateDecision::Allow(diagnostic) => {
            println!("│  license: {diagnostic}");
            Ok(None)
        }
        LicenseUpdateDecision::Deny(diagnostic) => Ok(Some(diagnostic)),
    }
}

enum LicenseUpdateDecision {
    Allow(String),
    Deny(String),
}

#[derive(serde::Deserialize)]
struct LicenseEnvelope {
    payload: String,
    signature: String,
}

#[derive(serde::Deserialize)]
struct LicenseClaims {
    entitlements: Vec<ProductEntitlement>,
}

#[derive(serde::Deserialize)]
struct ProductEntitlement {
    product: String,
    version_families: Vec<VersionFamilyEntitlement>,
}

#[derive(serde::Deserialize)]
struct VersionFamilyEntitlement {
    family: String,
    features: Vec<String>,
    updates_until: Option<String>,
}

#[derive(serde::Deserialize)]
struct CommercialPublicKeyringFile {
    alg: String,
    keys: Vec<CommercialPublicKeyringEntry>,
}

#[derive(serde::Deserialize)]
struct CommercialPublicKeyringEntry {
    version_family: String,
    public_key: String,
}

fn check_update_license(
    home: &Path,
    slug: &str,
    product: &str,
    today: &str,
) -> LicenseUpdateDecision {
    let Some(license_path) = configured_license_path(home, slug, product) else {
        return LicenseUpdateDecision::Deny(format!(
            "commercial update requires a configured `{product}` license for {CURRENT_LICENSE_YEAR}"
        ));
    };
    let Some(keyring_path) = configured_keyring_path(home, slug, product) else {
        return LicenseUpdateDecision::Deny(
            "commercial update requires ~/.ldgr/licenses/keyrings/commercial-public-keyring.json or configured licenses.<product>.keyring_file".to_string(),
        );
    };
    match verify_update_license(&license_path, &keyring_path, product, today) {
        Ok(diagnostic) => LicenseUpdateDecision::Allow(diagnostic),
        Err(diagnostic) => LicenseUpdateDecision::Deny(diagnostic),
    }
}

fn verify_update_license(
    license_path: &Path,
    keyring_path: &Path,
    product: &str,
    today: &str,
) -> Result<String, String> {
    let license_text = fs::read_to_string(license_path)
        .map_err(|_| "license file is missing or unreadable".to_string())?;
    let envelope: LicenseEnvelope =
        toml::from_str(&license_text).map_err(|_| "license file is malformed".to_string())?;
    let claims: LicenseClaims = serde_json::from_str(&envelope.payload)
        .map_err(|_| "license file is malformed".to_string())?;
    let family = payload_version_family(&claims)
        .ok_or_else(|| "license file is malformed".to_string())?
        .to_string();
    if family != CURRENT_LICENSE_YEAR {
        return Err(format!(
            "license version family `{family}` does not match current update year `{CURRENT_LICENSE_YEAR}`"
        ));
    }
    let keyring_text = fs::read_to_string(keyring_path)
        .map_err(|_| "public keyring file is missing or unreadable".to_string())?;
    let keyring: CommercialPublicKeyringFile = serde_json::from_str(&keyring_text)
        .map_err(|_| "public keyring file is malformed".to_string())?;
    let public_key = keyring_public_key_bytes(&keyring, &family)?;
    verify_license_signature(
        &public_key,
        envelope.payload.as_bytes(),
        &envelope.signature,
    )?;
    evaluate_update_claims(&claims, product, today)
}

fn payload_version_family(claims: &LicenseClaims) -> Option<&str> {
    claims
        .entitlements
        .iter()
        .flat_map(|product| product.version_families.iter())
        .map(|family| family.family.as_str())
        .next()
}

fn keyring_public_key_bytes(
    keyring: &CommercialPublicKeyringFile,
    version_family: &str,
) -> Result<Vec<u8>, String> {
    if keyring.alg != "Ed25519" {
        return Err("public keyring file is malformed".to_string());
    }
    let entry = keyring
        .keys
        .iter()
        .find(|entry| entry.version_family == version_family)
        .ok_or_else(|| {
            format!("public keyring does not include version family `{version_family}`")
        })?;
    STANDARD
        .decode(entry.public_key.as_bytes())
        .map_err(|_| "public keyring file is malformed".to_string())
}

fn verify_license_signature(
    public_key: &[u8],
    payload: &[u8],
    signature: &str,
) -> Result<(), String> {
    let public_key: [u8; 32] = public_key
        .try_into()
        .map_err(|_| "public keyring file is malformed".to_string())?;
    let signature = STANDARD
        .decode(signature.as_bytes())
        .map_err(|_| "license file is malformed".to_string())?;
    let signature: [u8; 64] = signature
        .try_into()
        .map_err(|_| "license file is malformed".to_string())?;
    let verifying_key = VerifyingKey::from_bytes(&public_key)
        .map_err(|_| "public keyring file is malformed".to_string())?;
    verifying_key
        .verify(payload, &Signature::from_bytes(&signature))
        .map_err(|_| "license signature could not be verified".to_string())
}

fn evaluate_update_claims(
    claims: &LicenseClaims,
    product: &str,
    today: &str,
) -> Result<String, String> {
    let product_claim = claims
        .entitlements
        .iter()
        .find(|claim| claim.product == product)
        .ok_or_else(|| format!("license does not include product `{product}`"))?;
    let family = product_claim
        .version_families
        .iter()
        .find(|family| family.family == CURRENT_LICENSE_YEAR)
        .ok_or_else(|| {
            format!(
                "license for `{product}` does not include version family `{CURRENT_LICENSE_YEAR}`"
            )
        })?;
    if !family
        .features
        .iter()
        .any(|feature| feature == FULL_LICENSE_FEATURE)
    {
        return Err(format!(
            "license for `{product}` `{CURRENT_LICENSE_YEAR}` does not include feature `{FULL_LICENSE_FEATURE}`"
        ));
    }
    if let Some(updates_until) = family.updates_until.as_deref() {
        if updates_until < today {
            return Err(format!(
                "update entitlement expired on {updates_until} for `{product}` `{CURRENT_LICENSE_YEAR}`"
            ));
        }
    }
    Ok(format!(
        "license allows `{product}` `{CURRENT_LICENSE_YEAR}` updates through {}",
        family
            .updates_until
            .as_deref()
            .unwrap_or(CURRENT_LICENSE_YEAR)
    ))
}

fn configured_license_path(home: &Path, slug: &str, product: &str) -> Option<PathBuf> {
    configured_license_field(home, slug, product, &["license_file", "license_path"]).or_else(|| {
        first_existing([
            home.join(".ldgr/licenses")
                .join(slug)
                .join(format!("{CURRENT_LICENSE_YEAR}.ldgr")),
            home.join(".ldgr/licenses")
                .join(product)
                .join(format!("{CURRENT_LICENSE_YEAR}.ldgr")),
            home.join(".ldgr/.ldgr/licenses")
                .join(slug)
                .join(format!("{CURRENT_LICENSE_YEAR}.ldgr")),
            home.join(".ldgr/.ldgr/licenses")
                .join(product)
                .join(format!("{CURRENT_LICENSE_YEAR}.ldgr")),
        ])
    })
}

fn configured_keyring_path(home: &Path, slug: &str, product: &str) -> Option<PathBuf> {
    configured_license_field(
        home,
        slug,
        product,
        &[
            "keyring_file",
            "public_keyring_file",
            "keyring_path",
            "public_keyring_path",
        ],
    )
    .or_else(|| {
        first_existing([
            home.join(".ldgr/licenses/keyrings/commercial-public-keyring.json"),
            home.join(".ldgr/.ldgr/licenses/keyrings/commercial-public-keyring.json"),
        ])
    })
}

fn configured_license_field(
    home: &Path,
    slug: &str,
    product: &str,
    field_names: &[&str],
) -> Option<PathBuf> {
    let config_path = home.join(".ldgr/config.json");
    let text = fs::read_to_string(config_path).ok()?;
    let json: serde_json::Value = serde_json::from_str(&text).ok()?;
    for section in ["licenses", "adapter_licenses", "commercial_licenses"] {
        for key in [product, slug] {
            let Some(entry) = json.get(section).and_then(|section| section.get(key)) else {
                continue;
            };
            for field in field_names {
                if let Some(value) = entry.get(field).and_then(|value| value.as_str()) {
                    return Some(expand_home_path(home, value));
                }
            }
        }
    }
    None
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

fn first_existing(paths: impl IntoIterator<Item = PathBuf>) -> Option<PathBuf> {
    paths.into_iter().find(|path| path.is_file())
}

fn today_iso_date() -> String {
    let days = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs() / 86_400)
        .unwrap_or(0) as i64;
    let (year, month, day) = civil_from_days(days);
    format!("{year:04}-{month:02}-{day:02}")
}

fn civil_from_days(days_since_epoch: i64) -> (i32, u32, u32) {
    let z = days_since_epoch + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = mp + if mp < 10 { 3 } else { -9 };
    let year = y + i64::from(month <= 2);
    (year as i32, month as u32, day as u32)
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
    version_family: &'static str,
}

#[derive(Debug, serde::Deserialize)]
struct ReleaseMetadata {
    adapter: String,
    #[serde(default)]
    version: Option<String>,
    adapter_version: String,
    adapter_version_family: String,
    platform: String,
    artifact: String,
    sha256: String,
    ldgr_core_api_min: String,
    #[serde(default)]
    ldgr_core_api_max_exclusive: Option<String>,
    #[serde(default)]
    entitlement_family: Option<String>,
}

struct ResolvedReleaseAsset {
    tag: String,
    version: String,
    archive_name: String,
    archive_url: String,
    checksum_name: String,
    checksum_url: String,
    metadata_name: String,
    metadata_url: String,
    adapter_version_family: String,
    core_api_min: String,
    core_api_max_exclusive: Option<String>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AdapterCatalogSection {
    OpenSource,
    CommercialBinary,
}

struct AvailableAdapter {
    slug: &'static str,
    title: &'static str,
    source: &'static str,
    install: &'static str,
    section: AdapterCatalogSection,
    workspace_package: Option<&'static str>,
    git: Option<GitAdapterSource>,
    release: Option<ReleaseAdapterSource>,
}

static AVAILABLE_ADAPTERS: &[AvailableAdapter] = &[
    AvailableAdapter {
        slug: "conduct",
        title: "LDGR Conduct adapter",
        source: "binary release: https://github.com/hydra-dynamix/ldgr-releases",
        install: "ldgr adapter install conduct",
        section: AdapterCatalogSection::OpenSource,
        workspace_package: Some("ldgr-conduct"),
        git: None,
        release: Some(ReleaseAdapterSource {
            repo: "hydra-dynamix/ldgr-releases",
            tag_prefix: "conduct-v",
            asset_prefix: "conduct",
            root_prefix: "conduct",
            binary: "ldgr-conduct",
            version_family: "2026",
        }),
    },
    AvailableAdapter {
        slug: "research",
        title: "Research adapter",
        source: "open-source git: https://github.com/hydra-dynamix/ldgr-research",
        install: "ldgr adapter install research",
        section: AdapterCatalogSection::OpenSource,
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
            version_family: "2026",
        }),
    },
    AvailableAdapter {
        slug: "example",
        title: "Public example adapter",
        source: "open-source git: https://github.com/hydra-dynamix/ldgr-example-adapter",
        install: "ldgr adapter install example",
        section: AdapterCatalogSection::OpenSource,
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
            version_family: "2026",
        }),
    },
    AvailableAdapter {
        slug: "programbench",
        title: "Clean-room ProgramBench adapter",
        source: "open-source git: https://github.com/hydra-dynamix/ldgr-programbench",
        install: "ldgr adapter install programbench",
        section: AdapterCatalogSection::OpenSource,
        workspace_package: None,
        git: Some(GitAdapterSource {
            repo: "https://github.com/hydra-dynamix/ldgr-programbench",
            package: "ldgr-programbench",
            binary: "ldgr-programbench",
        }),
        release: None,
    },
    AvailableAdapter {
        slug: "code",
        title: "Coding adapter",
        source: "commercial binary: https://github.com/hydra-dynamix/ldgr-releases",
        install: "ldgr adapter install code",
        section: AdapterCatalogSection::CommercialBinary,
        workspace_package: None,
        git: None,
        release: Some(commercial_release("code", "ldgr-code")),
    },
    AvailableAdapter {
        slug: "security",
        title: "Security adapter",
        source: "commercial binary: https://github.com/hydra-dynamix/ldgr-releases",
        install: "ldgr adapter install security",
        section: AdapterCatalogSection::CommercialBinary,
        workspace_package: None,
        git: None,
        release: Some(commercial_release("security", "ldgr-security")),
    },
    AvailableAdapter {
        slug: "explore",
        title: "Explore adapter",
        source: "commercial binary: https://github.com/hydra-dynamix/ldgr-releases",
        install: "ldgr adapter install explore",
        section: AdapterCatalogSection::CommercialBinary,
        workspace_package: None,
        git: None,
        release: Some(commercial_release("explore", "ldgr-explore")),
    },
    AvailableAdapter {
        slug: "bench",
        title: "Bench adapter",
        source: "commercial binary: https://github.com/hydra-dynamix/ldgr-releases",
        install: "ldgr adapter install bench",
        section: AdapterCatalogSection::CommercialBinary,
        workspace_package: None,
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
        version_family: "2026",
    }
}

pub(crate) fn print_available_adapter_catalog() {
    println!("Available adapters:");
    print_available_adapter_catalog_section(
        "Open-source/source adapters",
        AdapterCatalogSection::OpenSource,
    );
    print_available_adapter_catalog_section(
        "Commercial binary adapters",
        AdapterCatalogSection::CommercialBinary,
    );
    println!("  installed adapters: ldgr adapter list");
    println!("  adapter details: ldgr adapter show <slug>");
    println!("  commercial binary source: ldgr-releases (https://github.com/hydra-dynamix/ldgr-releases)");
    println!("  commercial lookup contract: repo hydra-dynamix/ldgr-releases, newest compatible release metadata for adapter_version_family 2026 and ldgr_core_api {LDGR_CORE_API_VERSION}; pin with --adapter-version <version>; asset <adapter>-<adapter_version>-<platform>.tar.gz plus .sha256/.release.json, root <adapter>-<adapter_version>/, binary <platform>/ldgr-<adapter>");
}

fn print_available_adapter_catalog_section(title: &str, section: AdapterCatalogSection) {
    println!("  {title}:");
    for entry in available_adapter_catalog()
        .iter()
        .filter(|entry| entry.section == section)
    {
        println!("    {} — {} [{}]", entry.slug, entry.title, entry.source);
        println!("      install: {}", entry.install);
        println!("      after install: ldgr {} --help", entry.slug);
    }
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
    home: &Path,
) -> anyhow::Result<()> {
    println!("├─ Git source {}", git.repo);
    let cargo_root = home.join(".local");
    let mut command = cargo_install_git_command(git, &cargo_root);
    if let Err(error) = run_checked(&mut command, &format!("cargo install {}", git.package)) {
        if entry.slug == "programbench" {
            bail!(
                "adapter `programbench` is listed as an open-source git adapter at {}, but it is not yet released as an installable Cargo package: {error}",
                git.repo
            );
        }
        return Err(error);
    }
    let binary_path = cargo_root.join("bin").join(git.binary);
    run_adapter_binary_installer(binary_path.as_os_str(), entry.slug, install_root)?;
    patch_adapter_argv_to_installed_binary(install_root, git.binary, home)
}

fn cargo_install_git_command(git: GitAdapterSource, cargo_root: &Path) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg("install")
        .arg("--git")
        .arg(git.repo)
        .arg("--locked")
        .arg("--force")
        .arg("--root")
        .arg(cargo_root)
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
    pinned_version: Option<&str>,
) -> anyhow::Result<()> {
    let platform = platform_tag()?;
    let resolved = match resolve_release_asset(entry.slug, release, &platform, pinned_version) {
        Ok(resolved) => resolved,
        Err(error) => {
            if let Some(git) = entry.git {
                println!(
                    "├─ Release unavailable for {platform}; falling back to git install ({error})"
                );
                return install_adapter_from_git(entry, git, install_root, home);
            }
            if command_exists(release.binary) {
                println!(
                    "├─ Release unavailable for {platform}; falling back to installed `{}` ({error})",
                    release.binary
                );
                run_adapter_binary_installer(release.binary, entry.slug, install_root)?;
                patch_adapter_argv_to_installed_binary(install_root, release.binary, home)?;
                return Ok(());
            }
            bail!(
                "release asset unavailable for adapter `{}` on platform `{}` from `{}`: {error}; install `{}` or pass --source-root for a local source install",
                entry.slug,
                platform,
                release.repo,
                release.binary
            );
        }
    };
    println!(
        "├─ Release https://github.com/{}/releases/download/{}/{}",
        release.repo, resolved.tag, resolved.archive_name
    );
    println!(
        "├─ Resolved adapter version {} (family {}, core API {}..{}) for ldgr-core {}",
        resolved.version,
        resolved.adapter_version_family,
        resolved.core_api_min,
        resolved
            .core_api_max_exclusive
            .as_deref()
            .unwrap_or("unbounded"),
        LDGR_CORE_API_VERSION
    );
    let temp = std::env::temp_dir().join(format!(
        "ldgr-adapter-install-{}-{}",
        entry.slug,
        std::process::id()
    ));
    let _ = fs::remove_dir_all(&temp);
    fs::create_dir_all(&temp)?;

    let archive = temp.join(&resolved.archive_name);
    download_file(
        &resolved.archive_url,
        &archive,
        "download adapter release archive",
    )?;
    let checksum_path = temp.join(&resolved.checksum_name);
    download_file(
        &resolved.checksum_url,
        &checksum_path,
        "download adapter release checksum",
    )?;
    verify_sha256_file(&archive, &checksum_path)?;
    let metadata_path = temp.join(&resolved.metadata_name);
    download_file(
        &resolved.metadata_url,
        &metadata_path,
        "download adapter release metadata",
    )?;
    verify_release_metadata(
        &metadata_path,
        entry.slug,
        &platform,
        &resolved.archive_name,
        &archive,
    )?;

    run_checked(
        Command::new("tar")
            .arg("-xzf")
            .arg(&archive)
            .arg("-C")
            .arg(&temp),
        "extract adapter release archive",
    )?;
    let extracted = temp.join(format!("{}-{}", release.root_prefix, resolved.version));
    if !extracted.is_dir() {
        bail!(
            "release archive did not contain expected root {}",
            extracted.display()
        );
    }
    let _ = fs::remove_dir_all(install_root);
    copy_dir_recursive(&extracted, install_root)?;
    let installed_binary = install_release_binary(install_root, home, release.binary, &platform)?;
    let Some(binary_path) = installed_binary else {
        bail!(
            "release archive `{}` did not contain expected binary `{}` for platform `{}`",
            resolved.archive_name,
            release.binary,
            platform
        );
    };
    println!("├─ Running adapter installer from release binary");
    run_adapter_binary_installer(binary_path.as_os_str(), entry.slug, install_root)?;
    patch_adapter_argv_to_installed_binary(install_root, release.binary, home)?;
    let _ = fs::remove_dir_all(&temp);
    Ok(())
}

fn resolve_release_asset(
    adapter: &str,
    release: ReleaseAdapterSource,
    platform: &str,
    pinned_version: Option<&str>,
) -> anyhow::Result<ResolvedReleaseAsset> {
    let catalog_url = format!(
        "https://api.github.com/repos/{}/releases?per_page=100",
        release.repo
    );
    let temp = std::env::temp_dir().join(format!(
        "ldgr-release-catalog-{}-{}-{}.json",
        release.asset_prefix,
        platform,
        std::process::id()
    ));
    download_file(&catalog_url, &temp, "download adapter release catalog")?;
    let catalog: serde_json::Value = serde_json::from_str(&fs::read_to_string(&temp)?)
        .with_context(|| format!("parse release catalog {}", catalog_url))?;
    let releases = catalog
        .as_array()
        .ok_or_else(|| anyhow::anyhow!("release catalog response was not an array"))?;
    let tag_prefix = if release.tag_prefix.is_empty() {
        format!("{}-v", release.asset_prefix)
    } else {
        release.tag_prefix.to_string()
    };
    let archive_suffix = format!("-{platform}.tar.gz");
    let archive_prefix = format!("{}-", release.asset_prefix);
    let mut rejection_reasons = Vec::new();
    for item in releases {
        let Some(tag) = item.get("tag_name").and_then(|value| value.as_str()) else {
            continue;
        };
        if !tag.starts_with(&tag_prefix) {
            continue;
        }
        let Some(assets) = item.get("assets").and_then(|value| value.as_array()) else {
            continue;
        };
        let Some((archive_name, archive_url)) = find_release_asset(assets, |name| {
            name.starts_with(&archive_prefix) && name.ends_with(&archive_suffix)
        }) else {
            continue;
        };
        let version = archive_name
            .strip_prefix(&archive_prefix)
            .and_then(|tail| tail.strip_suffix(&archive_suffix))
            .ok_or_else(|| anyhow::anyhow!("could not parse version from asset `{archive_name}`"))?
            .to_string();
        if pinned_version.is_some_and(|pinned| pinned != version) {
            continue;
        }
        let checksum_name = format!("{archive_name}.sha256");
        let Some((_, checksum_url)) = find_release_asset(assets, |name| name == checksum_name)
        else {
            rejection_reasons.push(format!("{version}: missing checksum asset"));
            continue;
        };
        let metadata_name = format!("{}.release.json", archive_name.trim_end_matches(".tar.gz"));
        let Some((_, metadata_url)) = find_release_asset(assets, |name| name == metadata_name)
        else {
            rejection_reasons.push(format!("{version}: missing explicit release metadata"));
            continue;
        };
        let metadata_path = temp.with_file_name(format!(
            "ldgr-release-metadata-{}-{}-{}-{}.json",
            release.asset_prefix,
            version,
            platform,
            std::process::id()
        ));
        if let Err(error) = download_file(
            &metadata_url,
            &metadata_path,
            "download adapter release metadata for compatibility selection",
        ) {
            rejection_reasons.push(format!("{version}: could not download metadata: {error}"));
            let _ = fs::remove_file(&metadata_path);
            continue;
        }
        let metadata = match parse_release_metadata(&metadata_path) {
            Ok(metadata) => metadata,
            Err(error) => {
                rejection_reasons.push(format!("{version}: malformed metadata: {error}"));
                let _ = fs::remove_file(&metadata_path);
                continue;
            }
        };
        let _ = fs::remove_file(&metadata_path);
        if let Err(error) = validate_release_metadata_identity(
            &metadata,
            adapter,
            platform,
            &archive_name,
            Some(&version),
        ) {
            rejection_reasons.push(format!("{version}: {error}"));
            continue;
        }
        if metadata.adapter_version_family != release.version_family {
            rejection_reasons.push(format!(
                "{version}: adapter_version_family `{}` does not match required `{}`",
                metadata.adapter_version_family, release.version_family
            ));
            continue;
        }
        if let Err(error) = validate_core_api_compatibility(&metadata) {
            rejection_reasons.push(format!("{version}: {error}"));
            continue;
        }
        let _ = fs::remove_file(&temp);
        return Ok(ResolvedReleaseAsset {
            tag: tag.to_string(),
            version,
            archive_name,
            archive_url,
            checksum_name,
            checksum_url,
            metadata_name,
            metadata_url,
            adapter_version_family: metadata.adapter_version_family,
            core_api_min: metadata.ldgr_core_api_min,
            core_api_max_exclusive: metadata.ldgr_core_api_max_exclusive,
        });
    }
    let _ = fs::remove_file(&temp);
    let pin = pinned_version
        .map(|version| format!(" pinned version `{version}`"))
        .unwrap_or_default();
    bail!(
        "no compatible release asset matched tag prefix `{}`{} platform `{}` adapter_version_family `{}` ldgr_core_api `{}`{}",
        tag_prefix,
        pin,
        platform,
        release.version_family,
        LDGR_CORE_API_VERSION,
        if rejection_reasons.is_empty() {
            String::new()
        } else {
            format!("; rejected candidates: {}", rejection_reasons.join("; "))
        }
    )
}

fn parse_release_metadata(metadata_path: &Path) -> anyhow::Result<ReleaseMetadata> {
    serde_json::from_str(&fs::read_to_string(metadata_path)?)
        .with_context(|| format!("parse release metadata {}", metadata_path.display()))
}

fn validate_release_metadata_identity(
    metadata: &ReleaseMetadata,
    adapter: &str,
    platform: &str,
    archive_name: &str,
    expected_adapter_version: Option<&str>,
) -> anyhow::Result<()> {
    if metadata.adapter != adapter {
        bail!("release metadata adapter field did not match `{adapter}`");
    }
    if metadata.platform != platform {
        bail!("release metadata platform field did not match `{platform}`");
    }
    if metadata.artifact != archive_name {
        bail!("release metadata artifact field did not match `{archive_name}`");
    }
    if let Some(expected_adapter_version) = expected_adapter_version {
        if metadata.adapter_version != expected_adapter_version {
            bail!(
                "release metadata adapter_version `{}` did not match asset version `{}`",
                metadata.adapter_version,
                expected_adapter_version
            );
        }
        if let Some(legacy_version) = metadata.version.as_deref() {
            if legacy_version != expected_adapter_version {
                bail!(
                    "release metadata version `{legacy_version}` did not match asset version `{expected_adapter_version}`"
                );
            }
        }
    }
    if metadata.adapter_version_family.trim().is_empty() {
        bail!("release metadata missing adapter_version_family");
    }
    if metadata.ldgr_core_api_min.trim().is_empty() {
        bail!("release metadata missing ldgr_core_api_min");
    }
    if metadata
        .entitlement_family
        .as_deref()
        .is_some_and(|family| family.trim().is_empty())
    {
        bail!("release metadata entitlement_family was present but empty");
    }
    Ok(())
}

fn validate_core_api_compatibility(metadata: &ReleaseMetadata) -> anyhow::Result<()> {
    if compare_semverish(LDGR_CORE_API_VERSION, &metadata.ldgr_core_api_min)? < 0 {
        bail!(
            "requires ldgr_core_api >= {}, current {}",
            metadata.ldgr_core_api_min,
            LDGR_CORE_API_VERSION
        );
    }
    if let Some(max_exclusive) = metadata.ldgr_core_api_max_exclusive.as_deref() {
        if compare_semverish(LDGR_CORE_API_VERSION, max_exclusive)? >= 0 {
            bail!(
                "requires ldgr_core_api < {}, current {}",
                max_exclusive,
                LDGR_CORE_API_VERSION
            );
        }
    }
    Ok(())
}

fn compare_semverish(left: &str, right: &str) -> anyhow::Result<i8> {
    let left = parse_semverish_triplet(left)?;
    let right = parse_semverish_triplet(right)?;
    Ok(match left.cmp(&right) {
        std::cmp::Ordering::Less => -1,
        std::cmp::Ordering::Equal => 0,
        std::cmp::Ordering::Greater => 1,
    })
}

fn parse_semverish_triplet(version: &str) -> anyhow::Result<(u64, u64, u64)> {
    let core = version
        .find(['-', '+'])
        .map(|index| &version[..index])
        .unwrap_or(version);
    let mut parts = core.split('.');
    let major = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid semantic version `{version}`"))?
        .parse::<u64>()?;
    let minor = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid semantic version `{version}`"))?
        .parse::<u64>()?;
    let patch = parts
        .next()
        .ok_or_else(|| anyhow::anyhow!("invalid semantic version `{version}`"))?
        .parse::<u64>()?;
    Ok((major, minor, patch))
}

fn find_release_asset(
    assets: &[serde_json::Value],
    predicate: impl Fn(&str) -> bool,
) -> Option<(String, String)> {
    assets.iter().find_map(|asset| {
        let name = asset.get("name")?.as_str()?;
        if !predicate(name) {
            return None;
        }
        let url = asset.get("browser_download_url")?.as_str()?;
        Some((name.to_string(), url.to_string()))
    })
}

fn download_file(url: &str, dest: &Path, label: &str) -> anyhow::Result<()> {
    let mut command = Command::new("curl");
    command.arg("-fsSL").arg(url).arg("-o").arg(dest);
    run_checked(&mut command, label)
}

fn verify_sha256_file(archive: &Path, checksum_path: &Path) -> anyhow::Result<String> {
    let checksum_text = fs::read_to_string(checksum_path)?;
    let expected = checksum_text
        .split_whitespace()
        .next()
        .ok_or_else(|| anyhow::anyhow!("checksum file {} was empty", checksum_path.display()))?
        .to_ascii_lowercase();
    let actual = sha256_hex(archive)?;
    if actual != expected {
        bail!(
            "checksum mismatch for {}: expected {}, got {}",
            archive.display(),
            expected,
            actual
        );
    }
    println!("├─ Verified checksum {}", checksum_path.display());
    Ok(actual)
}

fn verify_release_metadata(
    metadata_path: &Path,
    adapter: &str,
    platform: &str,
    archive_name: &str,
    archive: &Path,
) -> anyhow::Result<()> {
    let metadata = parse_release_metadata(metadata_path)?;
    validate_release_metadata_identity(&metadata, adapter, platform, archive_name, None)?;
    validate_core_api_compatibility(&metadata)?;
    if metadata.sha256.trim().is_empty() {
        bail!("release metadata missing sha256 field");
    }
    let expected_sha = metadata.sha256.as_str();
    let actual_sha = sha256_hex(archive)?;
    if actual_sha != expected_sha.to_ascii_lowercase() {
        bail!(
            "release metadata sha256 mismatch for {}: expected {}, got {}",
            archive.display(),
            expected_sha,
            actual_sha
        );
    }
    println!("├─ Verified release metadata {}", metadata_path.display());
    Ok(())
}

fn sha256_hex(path: &Path) -> anyhow::Result<String> {
    let bytes = fs::read(path)?;
    let digest = Sha256::digest(bytes);
    Ok(format!("{digest:x}"))
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
    patch_adapter_argv_command(&manifest, binary, &quoted_path)
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
    let prompts = install_root.join("prompts");
    if prompts.is_dir() {
        let prompt_root = home.join(".ldgr/prompts");
        copy_directory_children(&prompts, &prompt_root)?;
        println!("├─ LDGR prompts {}", prompt_root.display());
    }
    let skills = install_root.join("skills");
    if skills.is_dir() {
        let pi_skills = home.join(".pi/agent/skills");
        copy_directory_children(&skills, &pi_skills)?;
        println!("├─ Harness skills {}", pi_skills.display());
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
                live_progress: !args.no_live_progress,
                progress_heartbeat: Duration::from_secs(args.progress_heartbeat_seconds),
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
    seed_core_prompt_files(Path::new(".ldgr/prompts"))?;
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

fn seed_core_prompt_files(prompt_root: &Path) -> anyhow::Result<Vec<PathBuf>> {
    fs::create_dir_all(prompt_root)?;
    let mut seeded = Vec::new();
    for (file_name, body) in core_prompt_files() {
        let path = prompt_root.join(file_name);
        if path.exists() {
            println!("├─ Preserved user prompt {}", path.display());
        } else {
            fs::write(&path, body)
                .with_context(|| format!("failed to seed prompt {}", path.display()))?;
            println!("├─ Seeded prompt {}", path.display());
            seeded.push(path);
        }
    }
    Ok(seeded)
}

fn core_prompt_files() -> [(&'static str, &'static str); 6] {
    [
        (LDGR_CORE_LOOP_PROMPT_FILE, LDGR_CORE_LOOP_PROMPT),
        (LOOP_INVARIANTS_PROMPT_FILE, LOOP_INVARIANTS_PROMPT),
        ("ldgr-loop-planner.md", LDGR_LOOP_PLANNER_PROMPT),
        ("ldgr-loop-worker.md", LDGR_LOOP_WORKER_PROMPT),
        ("ldgr-loop-scryb.md", LDGR_LOOP_SCRYB_PROMPT),
        ("ldgr-loop-validator.md", LDGR_LOOP_VALIDATOR_PROMPT),
    ]
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
    fn release_metadata_validates_explicit_core_api_compatibility() -> anyhow::Result<()> {
        let metadata = ReleaseMetadata {
            adapter: "code".to_string(),
            version: Some("2026.1.0".to_string()),
            adapter_version: "2026.1.0".to_string(),
            adapter_version_family: "2026".to_string(),
            platform: "linux-x86_64".to_string(),
            artifact: "code-2026.1.0-linux-x86_64.tar.gz".to_string(),
            sha256: "abc".to_string(),
            ldgr_core_api_min: "0.1.0".to_string(),
            ldgr_core_api_max_exclusive: Some("0.2.0".to_string()),
            entitlement_family: Some("2026-commercial".to_string()),
        };

        validate_release_metadata_identity(
            &metadata,
            "code",
            "linux-x86_64",
            "code-2026.1.0-linux-x86_64.tar.gz",
            Some("2026.1.0"),
        )?;
        validate_core_api_compatibility(&metadata)?;
        Ok(())
    }

    #[test]
    fn release_metadata_rejects_implicit_or_incompatible_adapter_version() {
        let metadata = ReleaseMetadata {
            adapter: "code".to_string(),
            version: Some("0.1.1".to_string()),
            adapter_version: "2026.1.0".to_string(),
            adapter_version_family: "2026".to_string(),
            platform: "linux-x86_64".to_string(),
            artifact: "code-2026.1.0-linux-x86_64.tar.gz".to_string(),
            sha256: "abc".to_string(),
            ldgr_core_api_min: "0.1.0".to_string(),
            ldgr_core_api_max_exclusive: Some("0.2.0".to_string()),
            entitlement_family: Some("2026-commercial".to_string()),
        };

        let error = validate_release_metadata_identity(
            &metadata,
            "code",
            "linux-x86_64",
            "code-2026.1.0-linux-x86_64.tar.gz",
            Some("2026.1.0"),
        )
        .expect_err("legacy version alias must not silently override adapter_version");
        assert!(error
            .to_string()
            .contains("release metadata version `0.1.1` did not match asset version `2026.1.0`"));
    }

    #[test]
    fn cargo_git_install_uses_positional_crate_name() {
        let command = cargo_install_git_command(
            GitAdapterSource {
                repo: "https://github.com/hydra-dynamix/ldgr-research",
                package: "ldgr-research",
                binary: "ldgr-research",
            },
            Path::new("/tmp/ldgr-cargo-root"),
        );
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
                "--root".to_string(),
                "/tmp/ldgr-cargo-root".to_string(),
                "ldgr-research".to_string(),
            ]
        );
        assert!(!args.iter().any(|arg| arg == "--package"));
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
    fn core_prompt_seed_installs_role_prompts_and_preserves_user_edits() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let prompt_root = root.path().join("prompts");

        let seeded = seed_core_prompt_files(&prompt_root)?;
        assert_eq!(seeded.len(), 6);
        assert!(prompt_root.join("ldgr-core-loop.md").is_file());
        assert!(prompt_root.join("ldgr-loop-invariants.md").is_file());
        assert!(prompt_root.join("ldgr-loop-planner.md").is_file());
        assert!(prompt_root.join("ldgr-loop-worker.md").is_file());
        assert!(prompt_root.join("ldgr-loop-scryb.md").is_file());
        assert!(prompt_root.join("ldgr-loop-validator.md").is_file());
        assert!(
            std::fs::read_to_string(prompt_root.join("ldgr-loop-worker.md"))?
                .contains("fresh, ephemeral agent")
        );
        assert!(
            std::fs::read_to_string(prompt_root.join("ldgr-loop-invariants.md"))?
                .contains("durable guidance for ephemeral agents")
        );

        std::fs::write(prompt_root.join("ldgr-loop-planner.md"), "custom planner")?;
        let reseeded = seed_core_prompt_files(&prompt_root)?;
        assert!(reseeded.is_empty());
        assert_eq!(
            std::fs::read_to_string(prompt_root.join("ldgr-loop-planner.md"))?,
            "custom planner"
        );
        Ok(())
    }

    #[test]
    fn installed_binary_patch_replaces_adapter_argv_with_absolute_binary() -> anyhow::Result<()> {
        let install_root = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        let bin_dir = home.path().join(".local/bin");
        std::fs::create_dir_all(&bin_dir)?;
        std::fs::write(bin_dir.join("ldgr-conduct"), "#!/bin/sh\n")?;
        std::fs::write(
            install_root.path().join("adapter.toml"),
            r#"[adapter]
slug = "conduct"

[[commands]]
namespace = "conduct"
argv = ["ldgr-conduct"]

[[tools]]
name = "conduct-status"
argv = ["ldgr-conduct", "status"]
"#,
        )?;

        patch_adapter_argv_to_installed_binary(install_root.path(), "ldgr-conduct", home.path())?;

        let manifest = std::fs::read_to_string(install_root.path().join("adapter.toml"))?;
        let installed_binary = home.path().join(".local/bin/ldgr-conduct");
        assert!(manifest.contains(&format!("argv = [\"{}\"]", installed_binary.display())));
        assert!(manifest.contains(&format!(
            "argv = [\"{}\", \"status\"]",
            installed_binary.display()
        )));
        toml::from_str::<toml::Value>(&manifest).expect("patched manifest should parse as TOML");
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
}
