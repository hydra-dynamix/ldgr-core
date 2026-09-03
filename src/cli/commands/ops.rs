use std::fs;
use std::io::{self, BufRead, Write};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::{bail, Context};
use dialoguer::{theme::ColorfulTheme, Confirm, MultiSelect, Select};

use crate::adapter_registry::AdapterRegistry;
use crate::loop_runtime::{
    configure_child_home, run_loop_once, LoopAgent, LoopPromptSource, LoopRuntimeOptions,
    LoopRuntimeOutcome, LoopRuntimeResult,
};
use crate::recovery::{
    print_startup_recovery_report, project_root_for_db, reconcile_startup, ExecutionAttempt,
    FailureKind,
};
use crate::release_index::{
    adapter_installation_receipt_schema_version, parse_adapter_installation_receipt,
    validate_release_installation_receipt, validate_source_installation_receipt,
    AdapterInstallationReceipt, SOURCE_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION,
};
use crate::store::{doctor_schema, open_store_with_migration_info, read_context};
use crate::telemetry::automation::acquire_delivery_lock;
use crate::telemetry::donation::{
    clear_unsent_experience_donations, pending_experience_donation_count,
};
use crate::telemetry::transmission::{
    preview_pending_sequences, TransmissionClient, TransmissionReport,
};
use crate::telemetry::{
    clear_unsent_telemetry, load_telemetry_consent, save_telemetry_consent,
    telemetry_kill_switch_active, TelemetryConsent, TelemetryConsentDecision,
    DEFAULT_TELEMETRY_COLLECTOR_ORIGIN, NUMERICAL_SEQUENCE_PROTOCOLS_V1,
    RELEASED_NUMERICAL_PROTOCOLS_V1, TELEMETRY_CONSENT_POLICY_VERSION,
};
use crate::tool_runner::parse_argv_json;
use crate::update::adapter::InstallTransaction;
use crate::web::{generate_control_token, serve, WebOptions};

use super::super::args::{
    AdapterReconcileArgs, AdapterUninstallArgs, AdapterUpdateArgs, CliLoopAgent, ConfigArgs,
    ConfigCommand, ContextArgs, HarnessKind, InstallAdapterArgs, InstallArgs, InstallCommand,
    LoopArgs, LoopCommand, LoopRunArgs, MigrateArgs, SchemaArgs, SchemaCommand, StatusArgs,
    TelemetryArgs, TelemetryCommand, TelemetryDonationCommand, TelemetryInstallChoice,
    TelemetryTransmitArgs, WebArgs, WorkflowArgs,
};
use super::super::render::brief_context::{
    brief_context, print_brief_context, BriefContextOptions,
};
use super::super::render::context::print_context;
use super::super::render::emit;
use super::super::render::status::{build_status_summary, print_status_summary};
use super::super::render::text::print_loop_result;
use super::super::{CLI_DEFAULT_HELP_SECTIONS, INIT_PROJECT_SETUP_PROMPT};
use crate::harness_config::{HarnessConfig, InterviewDepth, UpdateChannel, UpdateCheck};

const LDGR_CORE_LOOP_PROMPT: &str = include_str!("../../../prompts/loop-prompt.md");
const LDGR_CORE_LOOP_PROMPT_FILE: &str = "ldgr-core-loop.md";
const LDGR_RELEASE_KEYRING: &str = include_str!("../../../release-keyring.json");
const LDGR_RELEASE_KEYRING_FILE: &str = "release-keyring.json";
const AGENTCTL_REPO: &str = "https://github.com/hydra-dynamix/agentctl";
pub(crate) const AGENTCTL_VERSION: &str = "0.1.2";
const AGENTCTL_REQUIREMENT: &str = ">=0.1.2, <0.2.0";
const LAUNCHER_COMPATIBILITY_SCHEMA: &str = "ldgr.launcher-compatibility.v1";
const ERROR_RECOVERY_SCHEMA_VERSION: u32 = 1;
const CORE_OPERATOR_ERROR_GUIDE: &str = include_str!("../../../guidance/operator-errors.md");
const CORE_AGENT_ERROR_GUIDE: &str = include_str!("../../../guidance/agent-errors.md");

pub fn handle_compatibility(agentctl_version: &str, json_output: bool) -> anyhow::Result<()> {
    let version = semver::Version::parse(agentctl_version)
        .context("--agentctl-version must be a semantic version")?;
    let requirement =
        semver::VersionReq::parse(AGENTCTL_REQUIREMENT).expect("valid agentctl requirement");
    let compatible = requirement.matches(&version);
    let executable =
        std::env::current_exe().context("failed to resolve current ldgr executable")?;
    let report = serde_json::json!({
        "schema": LAUNCHER_COMPATIBILITY_SCHEMA,
        "compatible": compatible,
        "core_version": env!("CARGO_PKG_VERSION"),
        "core_executable": executable,
        "agentctl_version": agentctl_version,
        "agentctl_requirement": AGENTCTL_REQUIREMENT,
        "error_recovery_schema": ERROR_RECOVERY_SCHEMA_VERSION,
    });
    if json_output {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "LDGR Core {} is {} with agentctl {} (required {}).",
            env!("CARGO_PKG_VERSION"),
            if compatible {
                "compatible"
            } else {
                "incompatible"
            },
            agentctl_version,
            AGENTCTL_REQUIREMENT
        );
    }
    if !compatible {
        bail!(
            "agentctl {agentctl_version} is incompatible with LDGR Core {}; install the paired release bundle or use agentctl {AGENTCTL_REQUIREMENT}",
            env!("CARGO_PKG_VERSION")
        );
    }
    Ok(())
}

pub fn handle_init(db: &Path, artifact_root: &Path) -> anyhow::Result<()> {
    let existing_database = db.exists();
    let (connection, alignment) =
        super::super::database_alignment::align_or_initialize_database(db, artifact_root)?;
    super::super::database_alignment::print_migration_notice(&alignment);
    super::super::database_alignment::print_database_alignment(&alignment);
    if existing_database {
        println!("opened existing {} (no data erased)", db.display());
    } else {
        println!("initialized {}", db.display());
    }
    let recovery = reconcile_startup(&connection, &project_root_for_db(db))?;
    print_startup_recovery_report(&recovery);
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
    let home = home_dir()?;
    let ldgr_home = home.join(".ldgr");
    let harnesses = select_harnesses(&args)?;
    if harnesses.is_empty() {
        return Ok(());
    }
    let telemetry_consent = resolve_install_telemetry_consent(&args, &ldgr_home)?;
    let interview_depth = select_interview_depth(&args)?;
    println!(
        "√ Harnesses: {}",
        harnesses
            .iter()
            .map(|h| harness_name(*h))
            .collect::<Vec<_>>()
            .join(", ")
    );
    println!("│");

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
    let agentctl = ensure_agentctl_dependency(args.no_agentctl, args.store.is_some())?;
    let agentctl_config = install_agentctl_config(&home, &harnesses)?;

    let config = serde_json::json!({
        "schema_version": 1,
        "default_harness": harnesses.first().map(|harness| harness_name(*harness)).unwrap_or("pi"),
        "selected_harnesses": harnesses.iter().map(|harness| harness_name(*harness)).collect::<Vec<_>>(),
        "interview_depth": interview_depth.as_str(),
        "installed": installed,
        "agentctl": agentctl,
        "compatibility": {
            "core_version": env!("CARGO_PKG_VERSION"),
            "agentctl_version": AGENTCTL_VERSION,
            "agentctl_requirement": AGENTCTL_REQUIREMENT,
            "launcher_schema": LAUNCHER_COMPATIBILITY_SCHEMA,
            "error_recovery_schema": ERROR_RECOVERY_SCHEMA_VERSION
        },
        "agentctl_config": agentctl_config,
        "core_loop_prompt": core_loop_prompt,
        "adapter_release_keyring": release_keyring,
        "adapter_files": {
            "default_global_path": "~/.ldgr/adapters/<adapter>",
            "note": "Adapter bundle files install globally under ~/.ldgr/adapters/<adapter>; adapter-owned prompts, skills, commands, and extensions install into paths declared by the configured harness entries."
        },
        "notes": "Adapters should read this file, validate their own license when applicable, install adapter bundle files under ~/.ldgr/adapters/<adapter> by default, then install adapter-owned prompts, skills, commands, and extensions into paths declared by the configured harness entries."
    });
    let harness_config: HarnessConfig = serde_json::from_value(config)?;
    let (config_path, legacy_config_path) = write_harness_config_files(&home, &harness_config)?;
    println!("├─ Wrote config {}", config_path.display());
    println!(
        "├─ Wrote legacy compatibility config {}",
        legacy_config_path.display()
    );
    reconcile_installed_adapters(&home, None)?;
    println!("│");
    println!("√ LDGR install complete");
    write_sequence_collection_status_summary(
        &mut io::stdout().lock(),
        "│  ",
        sequence_collection_status(&telemetry_consent),
    )?;
    println!("│");
    println!("◇ Next steps");
    println!("│  Run `ldgr workflow` to understand this project's workflow.");
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
                offline: args.store.is_some(),
                store: args.store.clone(),
                yes: args.yes,
            })?;
        }
    }
    println!("└─ Adapter bundles install under ~/.ldgr/adapters/<adapter>.");
    Ok(())
}

pub fn handle_telemetry(args: TelemetryArgs) -> anyhow::Result<()> {
    let ldgr_home = home_dir()?.join(".ldgr");
    match args.command {
        TelemetryCommand::Status => print_telemetry_status(&ldgr_home),
        TelemetryCommand::Preview => print_telemetry_preview(&ldgr_home),
        TelemetryCommand::Transmit(transmit_args) => transmit_telemetry(&ldgr_home, transmit_args),
        TelemetryCommand::Enable => {
            let stdout = io::stdout();
            let mut output = stdout.lock();
            print_telemetry_scope(&mut output)?;
            let mut consent = load_telemetry_consent(&ldgr_home)?;
            consent.decision = TelemetryConsentDecision::Enabled;
            save_telemetry_consent(&ldgr_home, &consent)?;
            writeln!(output, "sequence collection: enabled")?;
            if telemetry_kill_switch_active() {
                writeln!(
                    output,
                    "effective collection: disabled by LDGR_TELEMETRY=off"
                )?;
            }
            Ok(())
        }
        TelemetryCommand::Disable => {
            let mut consent = load_telemetry_consent(&ldgr_home)?;
            consent.decision = TelemetryConsentDecision::Disabled;
            save_telemetry_consent(&ldgr_home, &consent)?;
            clear_unsent_telemetry(&ldgr_home)?;
            println!("sequence collection: disabled");
            Ok(())
        }
        TelemetryCommand::Donation(donation) => {
            let mut consent = load_telemetry_consent(&ldgr_home)?;
            match donation.command {
                TelemetryDonationCommand::Status => {}
                TelemetryDonationCommand::Enable => {
                    print_experience_donation_scope(&mut io::stdout().lock())?;
                    consent = consent.with_donation(TelemetryConsentDecision::Enabled);
                    save_telemetry_consent(&ldgr_home, &consent)?;
                }
                TelemetryDonationCommand::Disable => {
                    consent = consent.with_donation(TelemetryConsentDecision::Disabled);
                    save_telemetry_consent(&ldgr_home, &consent)?;
                    clear_unsent_experience_donations(&ldgr_home)?;
                }
            }
            println!(
                "experience donation: {}",
                if consent.donation_enabled() {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            println!(
                "anonymous construction telemetry: {}",
                if consent.collection_enabled() {
                    "enabled"
                } else {
                    "disabled"
                }
            );
            Ok(())
        }
    }
}

fn print_experience_donation_scope(output: &mut impl Write) -> anyhow::Result<()> {
    writeln!(output, "Experience donation sends model-sanitized LDGR work records: task summaries, generic commands, observations, artifact metadata, decisions, errors, timestamps, project identifiers, and provenance.")?;
    writeln!(output, "LDGR agent instructions prohibit credentials, secrets, PII, raw prompts, raw tool output, environment values, and identifying absolute paths in ledger records. Direct Pi conversations and session events are not donated.")?;
    writeln!(output, "After opt-in, completed runs are captured and sent automatically until `ldgr telemetry donation disable`.")?;
    Ok(())
}

fn print_telemetry_status(ldgr_home: &Path) -> anyhow::Result<()> {
    let consent = load_telemetry_consent(ldgr_home)?;
    let kill_switch = telemetry_kill_switch_active();
    let effective = consent.collection_enabled() && !kill_switch;
    println!(
        "anonymous construction telemetry decision: {}",
        consent.decision.as_str()
    );
    println!(
        "effective collection: {}",
        if effective { "enabled" } else { "disabled" }
    );
    println!("consent policy version: {}", consent.policy_version);
    println!(
        "current consent policy version: {}",
        TELEMETRY_CONSENT_POLICY_VERSION
    );
    println!(
        "environment kill switch: {}",
        if kill_switch { "active" } else { "inactive" }
    );
    println!(
        "eligible numerical protocols: {}",
        NUMERICAL_SEQUENCE_PROTOCOLS_V1.join(", ")
    );
    println!(
        "experience donation: {} (separate opt-in)",
        if consent.donation_enabled() {
            "enabled"
        } else {
            "disabled"
        }
    );
    println!("disable: ldgr telemetry disable");
    Ok(())
}

fn print_telemetry_preview(ldgr_home: &Path) -> anyhow::Result<()> {
    let release = crate::telemetry::command_experience::release_eligible_constructions(ldgr_home)?;
    println!(
        "local construction release: eligible={} queued={} rare_suppressed={} cap_suppressed={}",
        release.eligible, release.queued, release.suppressed_rare, release.suppressed_cap
    );
    let mut previews = Vec::new();
    let mut invalid = 0;
    let mut unreadable = 0;
    for protocol in RELEASED_NUMERICAL_PROTOCOLS_V1 {
        let report = preview_pending_sequences(ldgr_home, protocol)?;
        previews.extend(report.payloads);
        invalid += report.invalid;
        unreadable += report.unreadable;
    }
    previews.sort_by(|left, right| {
        left.protocol_endpoint
            .cmp(right.protocol_endpoint)
            .then_with(|| left.raw_array.cmp(&right.raw_array))
    });

    println!("pending telemetry payloads: {}", previews.len());
    for preview in previews {
        let raw_array = std::str::from_utf8(&preview.raw_array)
            .context("validated telemetry payload was not utf-8")?;
        println!("- destination protocol: {}", preview.protocol_endpoint);
        println!("  raw array: {raw_array}");
        if preview.protocol_endpoint
            == crate::telemetry::command_experience::COMMAND_EXPERIENCE_V1.endpoint()
        {
            let states = crate::telemetry::serializer::parse_exact_sequence(
                &crate::telemetry::command_experience::COMMAND_EXPERIENCE_V1,
                &preview.raw_array,
            )?;
            println!(
                "  decoded: {}",
                crate::telemetry::command_experience::decode_command_experience(&states)?
            );
        }
    }
    if invalid > 0 {
        println!("invalid pending payloads: {invalid} (not shown; transmission will drop them)");
    }
    if unreadable > 0 {
        println!(
            "unreadable pending payloads: {unreadable} (not shown; transmission will retain them)"
        );
    }
    println!(
        "pending experience donations: {}",
        pending_experience_donation_count(ldgr_home)?
    );
    Ok(())
}

fn transmit_telemetry(ldgr_home: &Path, args: TelemetryTransmitArgs) -> anyhow::Result<()> {
    let Some(_lock) = acquire_delivery_lock(ldgr_home)? else {
        println!("telemetry transmission: another local transmission is already running");
        return Ok(());
    };
    let release = crate::telemetry::command_experience::release_eligible_constructions(ldgr_home)?;
    println!(
        "local construction release: eligible={} queued={} rare_suppressed={} cap_suppressed={}",
        release.eligible, release.queued, release.suppressed_rare, release.suppressed_cap
    );
    let collector = args
        .collector
        .or_else(|| std::env::var("LDGR_TELEMETRY_COLLECTOR").ok())
        .unwrap_or_else(|| DEFAULT_TELEMETRY_COLLECTOR_ORIGIN.to_owned());
    let mut client = TransmissionClient::new(&collector)?
        .with_max_delay(Duration::from_millis(args.max_delay_ms))
        .with_timeout(Duration::from_millis(args.timeout_ms));
    for path in &args.root_ca_pem {
        let certificate = fs::read(path)
            .with_context(|| format!("failed to read root CA PEM {}", path.display()))?;
        client = client
            .with_root_certificate_pem(&certificate)
            .with_context(|| format!("failed to parse root CA PEM {}", path.display()))?;
    }

    let mut total = TransmissionReport::default();
    for protocol in RELEASED_NUMERICAL_PROTOCOLS_V1 {
        let report = client.transmit_pending(ldgr_home, protocol);
        println!(
            "protocol {}: attempted={} accepted={} retained={} invalid_dropped={} disabled={}",
            protocol.endpoint(),
            report.attempted,
            report.accepted,
            report.retained,
            report.invalid_dropped,
            report.disabled
        );
        total.disabled |= report.disabled;
        total.attempted += report.attempted;
        total.accepted += report.accepted;
        total.retained += report.retained;
        total.invalid_dropped += report.invalid_dropped;
    }
    let anonymous_disabled = total.disabled;
    let donation = client.transmit_pending_donations(ldgr_home);
    println!(
        "protocol /donations/experiences/v1: attempted={} accepted={} retained={} invalid_dropped={} disabled={}",
        donation.attempted,
        donation.accepted,
        donation.retained,
        donation.invalid_dropped,
        donation.disabled
    );
    total.attempted += donation.attempted;
    total.accepted += donation.accepted;
    total.retained += donation.retained;
    total.invalid_dropped += donation.invalid_dropped;

    println!(
        "telemetry transmission: attempted={} accepted={} retained={} invalid_dropped={}",
        total.attempted, total.accepted, total.retained, total.invalid_dropped
    );
    if anonymous_disabled && donation.disabled {
        println!("effective collection: disabled; no further telemetry was attempted");
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SequenceCollectionStatus {
    Enabled,
    Disabled,
}

impl SequenceCollectionStatus {
    fn as_str(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }
}

fn sequence_collection_status(consent: &TelemetryConsent) -> SequenceCollectionStatus {
    if consent.collection_enabled() && !telemetry_kill_switch_active() {
        SequenceCollectionStatus::Enabled
    } else {
        SequenceCollectionStatus::Disabled
    }
}

fn current_sequence_collection_status() -> SequenceCollectionStatus {
    home_dir()
        .ok()
        .map(|home| home.join(".ldgr"))
        .and_then(|ldgr_home| load_telemetry_consent(&ldgr_home).ok())
        .as_ref()
        .map(sequence_collection_status)
        .unwrap_or(SequenceCollectionStatus::Disabled)
}

fn write_sequence_collection_status_summary(
    output: &mut impl Write,
    prefix: &str,
    status: SequenceCollectionStatus,
) -> anyhow::Result<()> {
    writeln!(output, "{prefix}sequence collection: {}", status.as_str())?;
    if status == SequenceCollectionStatus::Enabled {
        writeln!(output, "{prefix}disable: ldgr telemetry disable")?;
    }
    Ok(())
}

pub(crate) fn print_current_sequence_collection_status_summary(prefix: &str) -> anyhow::Result<()> {
    write_sequence_collection_status_summary(
        &mut io::stdout().lock(),
        prefix,
        current_sequence_collection_status(),
    )
}

pub(crate) fn handle_interactive_adapter_install(
    source_root: Option<PathBuf>,
    install_root: Option<PathBuf>,
    store: Option<PathBuf>,
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
            offline: store.is_some(),
            store: store.clone(),
            yes,
        })?;
    }
    Ok(())
}

pub(crate) fn handle_install_adapter(args: &InstallAdapterArgs) -> anyhow::Result<()> {
    if args.source_root.is_some() {
        return install_adapter_from_catalog(args);
    }

    let local_store = args
        .store
        .as_ref()
        .map(crate::update::local_store::LocalReleaseStore::open)
        .transpose()
        .context("failed to open local release store")?;
    let offline = args.offline || local_store.is_some();
    let configured_source = std::env::var(crate::release_index::ADAPTER_RELEASE_INDEX_ENV).ok();
    let source = configured_source
        .as_deref()
        .unwrap_or(crate::release_index::DEFAULT_ADAPTER_RELEASE_INDEX_URL);
    if offline && local_store.is_none() && source.starts_with("http") {
        bail!("--offline requires LDGR_ADAPTER_INDEX to reference a local file");
    }
    let signed_index = (|| {
        let sources = match &local_store {
            Some(store) => store.adapter_catalog_sources()?,
            None => crate::update::catalog::AdapterCatalogSources::configured(offline)?,
        };
        let client = match local_store.clone() {
            Some(store) => crate::update::network::UpdateNetworkClient::with_local_store(store)?,
            None => crate::update::network::UpdateNetworkClient::new(offline)?,
        };
        match crate::update::catalog::fetch_signed_adapter_update_catalog(&client, &sources, None)?
        {
            crate::update::catalog::AdapterCatalogFetch::Modified { verified, .. } => Ok(verified),
            crate::update::catalog::AdapterCatalogFetch::NotModified { .. } => {
                bail!("adapter release index unexpectedly returned not-modified")
            }
        }
    })();
    match signed_index {
        Ok(index) => install_adapter_from_index(args, &index, local_store.as_ref()),
        Err(index_error)
            if local_store.is_none()
                && default_catalog_fallback_allowed(args, configured_source.is_some()) =>
        {
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
        install_adapter_from_source_root(entry, source_root, &install_root, &home)?;
    } else if let Some(release) = entry.release {
        install_adapter_from_release(entry, release, &install_root, &home)?;
        install_adapter_harness_assets(&adapter, &install_root, &home)?;
    } else if let Some(git) = entry.git {
        install_adapter_from_git(entry, git, &install_root)?;
        install_adapter_harness_assets(&adapter, &install_root, &home)?;
    } else if let Some(package) = entry.workspace_package {
        let source_root = find_source_root(std::env::current_dir()?)?;
        install_adapter_from_source_root_with_package(
            &adapter,
            package,
            &source_root,
            &install_root,
            &home,
        )?;
    } else {
        bail!("adapter `{adapter}` has no release or source installer configured yet");
    }
    println!("└─ Installed adapter `{adapter}`. Try `ldgr {adapter} --help` or `ldgr adapter show {adapter}`.");
    Ok(())
}

pub(crate) fn handle_update_adapter(args: &AdapterUpdateArgs) -> anyhow::Result<()> {
    let local_store = args
        .store
        .as_ref()
        .map(crate::update::local_store::LocalReleaseStore::open)
        .transpose()
        .context("failed to open local release store")?;
    let inspection = crate::update::adapter::inspect_adapter_installation(&args.name)?;
    let verified_catalog = match &local_store {
        Some(store) if inspection.is_release() => {
            let sources = store.adapter_catalog_sources()?;
            let client =
                crate::update::network::UpdateNetworkClient::with_local_store(store.clone())?;
            match crate::update::catalog::fetch_signed_adapter_update_catalog(
                &client, &sources, None,
            )? {
                crate::update::catalog::AdapterCatalogFetch::Modified { verified, .. } => {
                    Some(verified)
                }
                crate::update::catalog::AdapterCatalogFetch::NotModified { .. } => {
                    bail!("adapter release index unexpectedly returned not-modified")
                }
            }
        }
        _ => None,
    };
    let target_core_version = semver::Version::parse(env!("CARGO_PKG_VERSION"))?;
    let channel = if args.prerelease {
        UpdateChannel::Prerelease
    } else {
        UpdateChannel::Stable
    };
    let plan = crate::update::adapter::plan_adapter_update(
        inspection,
        &target_core_version,
        channel,
        verified_catalog.as_ref().map(|verified| &verified.catalog),
    )?;
    plan.print_status();
    if args.check || !plan.should_apply_for_single_adapter_command() {
        return Ok(());
    }
    let temp = canonical_temp_path(format!(
        "ldgr-adapter-update-{}-{}",
        normalize_adapter_name(&args.name),
        std::process::id()
    ))?;
    remove_path_if_exists(&temp)?;
    let mut transaction = InstallTransaction::new(temp.join("rollback"))?;
    let result = crate::update::adapter::stage_and_apply_adapter_update(
        &plan,
        verified_catalog
            .as_ref()
            .map(|verified| &verified.archive_keyring),
        local_store.as_ref(),
        &mut transaction,
    );
    finish_ephemeral_installation(transaction, &temp, result)
}

pub(crate) fn handle_uninstall_adapter(args: &AdapterUninstallArgs) -> anyhow::Result<()> {
    let registry = AdapterRegistry::discover();
    let installed = registry
        .find(&args.name)
        .with_context(|| format!("adapter `{}` is not installed", args.name))?;
    let receipt_value = installed.installation_receipt.clone().context(
        "installed adapter has no tracked installation receipt; refusing untracked removal",
    )?;
    let receipt = parse_adapter_installation_receipt(receipt_value)?;
    if let AdapterInstallationReceipt::Source(receipt) = receipt {
        let home = home_dir()?;
        let modified = source_receipt_drift(&installed.root_path, &home, &receipt)?;
        if !modified.is_empty() && !args.force {
            bail!(
                "refusing to remove modified source adapter-owned files:\n{}\nRe-run with --force to remove them.",
                format_drift_paths(&modified)
            );
        }
        for resource in &receipt.owned_resources {
            remove_path_if_exists(Path::new(&resource.path))?;
        }
        remove_path_if_exists(&installed.root_path)?;
        remove_path_if_exists(Path::new(&receipt.ownership.marker_path))?;
        println!(
            "uninstalled adapter={} install_kind=local_source source_checkout_preserved=true",
            installed.slug
        );
        return Ok(());
    }
    let AdapterInstallationReceipt::Release(receipt) = receipt else {
        unreachable!()
    };
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
        let receipt = parse_adapter_installation_receipt(value)?;
        if let AdapterInstallationReceipt::Source(receipt) = receipt {
            reconcile_source_adapter(adapter, home, receipt)?;
            continue;
        }
        let AdapterInstallationReceipt::Release(mut receipt) = receipt else {
            unreachable!()
        };
        let desired_plan =
            typed_harness_resource_plan(&adapter.root_path, home, &receipt.resource_manifest)?;
        let desired_targets = desired_plan
            .iter()
            .map(|(_, target)| target.clone())
            .collect::<Vec<_>>();
        let temp = canonical_temp_path(format!(
            "ldgr-adapter-reconcile-{}-{}",
            adapter.slug,
            std::process::id()
        ))?;
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
        install_typed_harness_resources(&desired_plan, false)?;
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

fn reconcile_source_adapter(
    adapter: &crate::adapter_registry::DiscoveredAdapter,
    home: &Path,
    mut receipt: crate::release_index::SourceInstallationReceipt,
) -> anyhow::Result<()> {
    let drift = source_receipt_drift(&adapter.root_path, home, &receipt)?;
    if !drift.is_empty() {
        bail!(
            "refusing to reconcile modified source adapter-owned files:\n{}",
            format_drift_paths(&drift)
        );
    }
    let plan = source_harness_resource_plan(&adapter.root_path, home)?;
    let old_targets = receipt
        .owned_resources
        .iter()
        .map(|resource| PathBuf::from(&resource.path))
        .collect::<Vec<_>>();
    for resource in &plan {
        if resource.target.exists() && !old_targets.iter().any(|path| path == &resource.target) {
            bail!(
                "refusing to overwrite unowned harness resource {}",
                resource.target.display()
            );
        }
    }
    let temp = canonical_temp_path(format!(
        "ldgr-adapter-source-reconcile-{}-{}",
        adapter.slug,
        std::process::id()
    ))?;
    remove_path_if_exists(&temp)?;
    let mut transaction = InstallTransaction::new(temp.join("rollback"))?;
    transaction.snapshot(&adapter.root_path)?;
    for path in &old_targets {
        transaction.snapshot(path)?;
    }
    for resource in &plan {
        transaction.snapshot(&resource.target)?;
    }
    for old in &old_targets {
        if !plan.iter().any(|resource| &resource.target == old) {
            remove_path_if_exists(old)?;
        }
    }
    install_source_harness_resources(&plan, false)?;
    receipt.owned_resources = source_owned_resources(&plan)?;
    receipt.ownership.external_resource_roots = source_resource_roots(&plan)?;
    write_source_receipt_file(&adapter.root_path, &receipt)?;
    transaction.commit()?;
    remove_path_if_exists(&temp)?;
    println!(
        "reconciled adapter={} install_kind=local_source resources={}",
        adapter.slug,
        receipt.owned_resources.len()
    );
    Ok(())
}

fn prepare_source_reinstall(
    adapter: &str,
    install_root: &Path,
    home: &Path,
) -> anyhow::Result<Option<crate::release_index::SourceInstallationReceipt>> {
    if !install_root.exists() {
        return Ok(None);
    }
    anyhow::ensure!(
        install_root.is_dir(),
        "adapter install root {} is not a directory",
        install_root.display()
    );
    if fs::read_dir(install_root)?.next().is_none() {
        return Ok(None);
    }
    let path = install_root.join("installation-receipt.json");
    let text = fs::read_to_string(&path).with_context(|| {
        format!(
            "refusing to overwrite untracked adapter install at {}; uninstall or move it first",
            install_root.display()
        )
    })?;
    let parsed = parse_adapter_installation_receipt(
        serde_json::from_str(&text).context("installation receipt is invalid JSON")?,
    )?;
    let AdapterInstallationReceipt::Source(receipt) = parsed else {
        bail!(
            "refusing to replace a signed release installation with local source; run `ldgr adapter uninstall {adapter}` first"
        );
    };
    anyhow::ensure!(
        receipt.domain == adapter,
        "source receipt domain `{}` does not match requested adapter `{adapter}`",
        receipt.domain
    );
    let drift = source_receipt_drift(install_root, home, &receipt)?;
    if !drift.is_empty() {
        bail!(
            "refusing to reinstall over modified source adapter-owned files:\n{}\nRestore them or run `ldgr adapter uninstall {adapter} --force` first.",
            format_drift_paths(&drift)
        );
    }
    Ok(Some(receipt))
}

pub(crate) fn inspect_source_installation_for_update(
    install_root: &Path,
    home: &Path,
    receipt: &crate::release_index::SourceInstallationReceipt,
) -> anyhow::Result<(PathBuf, String, bool)> {
    let drift = source_receipt_drift(install_root, home, receipt)?;
    if !drift.is_empty() {
        bail!(
            "refusing to update modified source adapter-owned files:\n{}\nRestore them or run `ldgr adapter uninstall {} --force` before reinstalling.",
            format_drift_paths(&drift),
            receipt.domain
        );
    }
    let source = resolve_adapter_source_package(
        &receipt.source.package,
        Path::new(&receipt.source.bundle_root),
    )?;
    verify_source_identity_paths(receipt, &source)?;
    let current_source_sha256 = digest_source_bundle(&source.bundle_root)?;
    let source_changed = current_source_sha256 != receipt.source.bundle_sha256;
    Ok((source.bundle_root, current_source_sha256, source_changed))
}

pub(crate) fn inspect_release_installation_for_update(
    install_root: &Path,
    home: &Path,
    receipt: &crate::release_index::InstallationReceipt,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        digest_bundle(install_root)? == receipt.bundle_sha256,
        "modified adapter-owned bundle"
    );
    let allowed_roots = source_allowed_resource_roots(home)?;
    for resource in &receipt.owned_resources {
        let path = absolute_path(Path::new(&resource.path))?;
        anyhow::ensure!(
            allowed_roots
                .iter()
                .any(|root| path != *root && path.starts_with(root)),
            "adapter-owned resource is outside configured harness boundaries"
        );
        anyhow::ensure!(
            path.exists() && digest_path(&path)? == resource.sha256,
            "modified adapter-owned resource"
        );
    }
    if let (Some(path), Some(expected)) = (&receipt.binary_path, &receipt.binary_sha256) {
        let path = Path::new(path);
        anyhow::ensure!(
            path.is_absolute() && path.is_file() && digest_path(path)? == *expected,
            "modified adapter-owned binary"
        );
    } else {
        anyhow::ensure!(
            receipt.binary_path.is_none() && receipt.binary_sha256.is_none(),
            "adapter binary ownership fields must be paired"
        );
    }
    Ok(())
}

fn source_receipt_drift(
    install_root: &Path,
    home: &Path,
    receipt: &crate::release_index::SourceInstallationReceipt,
) -> anyhow::Result<Vec<PathBuf>> {
    verify_source_receipt_boundaries(install_root, home, receipt)?;
    let mut drift = Vec::new();
    let expected_files = receipt
        .installed_files
        .iter()
        .map(|file| file.path.clone())
        .collect::<std::collections::BTreeSet<_>>();
    for file in &receipt.installed_files {
        let path = install_root.join(&file.path);
        if !path.is_file() || digest_path(&path)? != file.sha256 {
            drift.push(path);
        }
    }
    let actual_files = source_installed_file_paths(install_root)?
        .into_iter()
        .collect::<std::collections::BTreeSet<_>>();
    for unexpected in actual_files.difference(&expected_files) {
        drift.push(install_root.join(unexpected));
    }
    for missing in expected_files.difference(&actual_files) {
        let path = install_root.join(missing);
        if !drift.iter().any(|existing| existing == &path) {
            drift.push(path);
        }
    }
    for resource in &receipt.owned_resources {
        let path = PathBuf::from(&resource.path);
        if !path.exists() || digest_path(&path)? != resource.sha256 {
            drift.push(path);
        }
    }
    let marker = PathBuf::from(&receipt.ownership.marker_path);
    let expected_marker = format!(
        "install_root={}\ninstall_kind=local_source\n",
        receipt.ownership.install_root
    );
    if fs::read_to_string(&marker).ok().as_deref() != Some(expected_marker.as_str()) {
        drift.push(marker);
    }
    drift.sort();
    drift.dedup();
    Ok(drift)
}

fn verify_source_receipt_boundaries(
    install_root: &Path,
    home: &Path,
    receipt: &crate::release_index::SourceInstallationReceipt,
) -> anyhow::Result<()> {
    validate_source_installation_receipt(receipt)?;
    anyhow::ensure!(
        Path::new(&receipt.ownership.install_root).is_absolute()
            && Path::new(&receipt.ownership.marker_path).is_absolute()
            && Path::new(&receipt.source.bundle_root).is_absolute()
            && Path::new(&receipt.source.cargo_manifest).is_absolute(),
        "source receipt identity and ownership paths must be absolute"
    );
    anyhow::ensure!(
        !paths_overlap(
            Path::new(&receipt.ownership.install_root),
            Path::new(&receipt.source.bundle_root)
        )?,
        "source receipt install root overlaps its preserved source checkout"
    );
    anyhow::ensure!(
        paths_match(install_root, Path::new(&receipt.ownership.install_root))?,
        "source receipt install-root boundary does not match discovered adapter root"
    );
    let expected_marker = home.join(".ldgr/installed-adapters").join(&receipt.domain);
    anyhow::ensure!(
        paths_match(&expected_marker, Path::new(&receipt.ownership.marker_path))?,
        "source receipt marker boundary is outside the adapter marker path"
    );
    let allowed_roots = source_allowed_resource_roots(home)?;
    let mut recorded_roots = Vec::new();
    for recorded_root in &receipt.ownership.external_resource_roots {
        anyhow::ensure!(
            Path::new(recorded_root).is_absolute(),
            "source receipt resource roots must be absolute"
        );
        let recorded_root = absolute_path(Path::new(recorded_root))?;
        anyhow::ensure!(
            allowed_roots
                .iter()
                .any(|allowed| paths_match(allowed, &recorded_root).unwrap_or(false)),
            "source receipt resource root {} is not a currently configured harness boundary",
            recorded_root.display()
        );
        recorded_roots.push(recorded_root);
    }
    for resource in &receipt.owned_resources {
        anyhow::ensure!(
            Path::new(&resource.path).is_absolute(),
            "source receipt resource paths must be absolute"
        );
        let path = absolute_path(Path::new(&resource.path))?;
        anyhow::ensure!(
            allowed_roots
                .iter()
                .any(|root| path != *root && path.starts_with(root)),
            "source receipt resource {} is outside configured harness boundaries",
            path.display()
        );
        anyhow::ensure!(
            recorded_roots
                .iter()
                .any(|root| path != *root && path.starts_with(root)),
            "source receipt resource {} is outside its recorded ownership roots",
            path.display()
        );
    }
    for file in &receipt.installed_files {
        validate_source_relative_path(&file.path)?;
    }
    Ok(())
}

fn validate_source_relative_path(path: &str) -> anyhow::Result<()> {
    let path = Path::new(path);
    anyhow::ensure!(
        !path.is_absolute()
            && path
                .components()
                .all(|part| matches!(part, std::path::Component::Normal(_))),
        "source receipt installed-file path must be relative: {}",
        path.display()
    );
    anyhow::ensure!(
        path != Path::new("installation-receipt.json") && !path.starts_with("source-target"),
        "source receipt installed-file path crosses a generated or receipt boundary: {}",
        path.display()
    );
    Ok(())
}

fn format_drift_paths(paths: &[PathBuf]) -> String {
    paths
        .iter()
        .map(|path| format!("  {}", path.display()))
        .collect::<Vec<_>>()
        .join("\n")
}

fn paths_match(left: &Path, right: &Path) -> anyhow::Result<bool> {
    Ok(absolute_path(left)? == absolute_path(right)?)
}

fn paths_overlap(left: &Path, right: &Path) -> anyhow::Result<bool> {
    let left = absolute_path(left)?;
    let right = absolute_path(right)?;
    Ok(left.starts_with(&right) || right.starts_with(&left))
}

fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()?.join(path)
    };
    #[cfg(windows)]
    {
        let text = absolute.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return Ok(PathBuf::from(format!(r"\\{rest}")));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return Ok(PathBuf::from(rest));
        }
    }
    Ok(absolute)
}

fn install_adapter_from_index(
    args: &InstallAdapterArgs,
    verified: &crate::update::catalog::VerifiedAdapterUpdateCatalog,
    local_store: Option<&crate::update::local_store::LocalReleaseStore>,
) -> anyhow::Result<()> {
    let index = &verified.catalog;
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
        && local_store.is_none()
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
    install_resolved_index_release(
        &resolved,
        &verified.archive_keyring,
        &install_root,
        &home,
        args.offline || local_store.is_some(),
        local_store,
    )?;
    println!(
        "└─ Installed adapter `{}`. Try `ldgr {} --help` or `ldgr adapter show {}`.",
        adapter.domain, adapter.domain, adapter.domain
    );
    Ok(())
}

fn install_resolved_index_release(
    resolved: &crate::release_index::ResolvedAdapterRelease<'_>,
    archive_keyring: &crate::release_index::ReleaseKeyring,
    install_root: &Path,
    home: &Path,
    offline: bool,
    local_store: Option<&crate::update::local_store::LocalReleaseStore>,
) -> anyhow::Result<()> {
    let temp = canonical_temp_path(format!(
        "ldgr-adapter-index-install-{}-{}",
        resolved.adapter.domain,
        std::process::id()
    ))?;
    remove_path_if_exists(&temp)?;
    fs::create_dir_all(&temp)?;
    let mut transaction = InstallTransaction::new(temp.join("rollback"))?;
    let result = stage_and_apply_resolved_index_release(
        resolved,
        archive_keyring,
        install_root,
        home,
        offline,
        local_store,
        &temp,
        &mut transaction,
    );
    finish_ephemeral_installation(transaction, &temp, result)
}

pub(crate) fn stage_and_apply_resolved_index_release(
    resolved: &crate::release_index::ResolvedAdapterRelease<'_>,
    archive_keyring: &crate::release_index::ReleaseKeyring,
    install_root: &Path,
    home: &Path,
    offline: bool,
    local_store: Option<&crate::update::local_store::LocalReleaseStore>,
    staging_root: &Path,
    transaction: &mut InstallTransaction,
) -> anyhow::Result<()> {
    let archive = staging_root.join("adapter.tar.gz");
    let update_client = match local_store.cloned() {
        Some(store) => crate::update::network::UpdateNetworkClient::with_local_store(store)?,
        None => crate::update::network::UpdateNetworkClient::new(offline)?,
    };
    update_client.download_artifact(
        &resolved.platform.asset_url,
        &archive,
        crate::update::network::MAX_UPDATE_ARTIFACT_BYTES,
    )?;
    crate::release_index::verify_file_sha256(&archive, &resolved.platform.sha256)?;
    let signature = staging_root.join("adapter.sig");
    update_client.download_artifact(
        &resolved.platform.signature_url,
        &signature,
        crate::update::network::MAX_UPDATE_SIGNATURE_BYTES,
    )?;
    let envelope =
        crate::release_index::parse_detached_signature(&fs::read_to_string(&signature)?)?;
    crate::release_index::verify_detached_signature_bytes(
        &fs::read(&archive)?,
        &envelope,
        archive_keyring,
        &resolved.platform.signing_key_id,
        "adapter release archive",
    )?;
    crate::release_index::extract_safe_tar_gz(
        &archive,
        staging_root,
        &resolved.platform.archive_root,
    )?;
    let extracted = staging_root.join(&resolved.platform.archive_root);
    if !extracted.is_dir() {
        bail!(
            "release archive did not contain expected root {}",
            extracted.display()
        );
    }
    if resolved.release.compatibility.is_some() {
        validate_adapter_bundle_contract(&extracted, &resolved.adapter.domain)?;
        crate::release_index::verify_resolved_v2_sidecar(&extracted, resolved)?;
    } else {
        validate_legacy_adapter_bundle_contract(&extracted, &resolved.adapter.domain)?;
    }
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
    apply_staged_resolved_index_release(
        resolved,
        &extracted,
        install_root,
        home,
        transaction,
        false,
        false,
    )
}

pub(crate) fn apply_staged_resolved_index_release(
    resolved: &crate::release_index::ResolvedAdapterRelease<'_>,
    extracted: &Path,
    install_root: &Path,
    home: &Path,
    transaction: &mut InstallTransaction,
    quiet: bool,
    migrate_legacy: bool,
) -> anyhow::Result<()> {
    if resolved.release.compatibility.is_some() {
        validate_adapter_bundle_contract(extracted, &resolved.adapter.domain)?;
        crate::release_index::verify_resolved_v2_sidecar(extracted, resolved)?;
    } else {
        validate_legacy_adapter_bundle_contract(extracted, &resolved.adapter.domain)?;
    }
    if migrate_legacy {
        let receipt = install_root.join("installation-receipt.json");
        match fs::symlink_metadata(&receipt) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(error).with_context(|| {
                    format!("failed to inspect legacy adapter receipt {}", receipt.display())
                });
            }
            Ok(_) => bail!(
                "legacy adapter gained an installation receipt before activation; resolve the new ownership state and retry"
            ),
        }
    }
    let binary_source = extracted
        .join(&resolved.platform.platform)
        .join(&resolved.platform.binary);
    let fresh_install = !install_root.exists();
    let previous_receipt = if migrate_legacy {
        None
    } else {
        read_release_update_receipt(install_root, &resolved.adapter.domain)?
    };
    if fresh_install {
        ensure_adapter_harness_config(home, transaction, quiet)?;
    }
    let resource_plan =
        typed_harness_resource_plan(extracted, home, &resolved.platform.resource_manifest)?;
    let resource_targets = resource_plan
        .iter()
        .map(|(_, target)| target.clone())
        .collect::<Vec<_>>();
    let previously_owned = previous_receipt
        .as_ref()
        .map(|receipt| {
            receipt
                .owned_resources
                .iter()
                .map(|resource| PathBuf::from(&resource.path))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for target in &resource_targets {
        if !migrate_legacy
            && target.exists()
            && !previously_owned.iter().any(|owned| owned == target)
        {
            bail!(
                "refusing to overwrite unowned harness resource {}; remove it or choose a different harness resource path",
                target.display()
            );
        }
        transaction.snapshot(target)?;
    }
    if let Some(previous) = &previous_receipt {
        for resource in &previous.owned_resources {
            transaction.snapshot(Path::new(&resource.path))?;
        }
    }
    transaction.begin_activation()?;
    if let Some(previous_binary) = previous_receipt
        .as_ref()
        .and_then(|receipt| receipt.binary_path.as_deref())
    {
        let desired_binary = binary_source
            .is_file()
            .then(|| home.join(".local/bin").join(&resolved.platform.binary));
        if desired_binary
            .as_ref()
            .is_none_or(|desired| desired != Path::new(previous_binary))
        {
            remove_path_if_exists(Path::new(previous_binary))?;
        }
    }
    if let Some(previous) = &previous_receipt {
        for resource in &previous.owned_resources {
            let path = PathBuf::from(&resource.path);
            if !resource_targets.iter().any(|desired| desired == &path) {
                remove_path_if_exists(&path)?;
            }
        }
    }
    activate_bundle_atomically(extracted, install_root)?;
    let installed_binary = install_release_binary(
        install_root,
        home,
        &resolved.platform.binary,
        &resolved.platform.platform,
        quiet,
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
        run_adapter_binary_installer_with_output(
            binary_path.as_os_str(),
            &resolved.adapter.domain,
            install_root,
            quiet,
        )?;
    }
    patch_adapter_argv_to_installed_binary(install_root, &resolved.platform.binary, home)?;
    install_typed_harness_resources(&resource_plan, quiet)?;
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
    Ok(())
}

fn read_release_update_receipt(
    install_root: &Path,
    adapter: &str,
) -> anyhow::Result<Option<crate::release_index::InstallationReceipt>> {
    if !install_root.exists() {
        return Ok(None);
    }
    let path = install_root.join("installation-receipt.json");
    let value: serde_json::Value = serde_json::from_str(
        &fs::read_to_string(&path)
            .with_context(|| format!("installed adapter `{adapter}` has no readable receipt"))?,
    )
    .context("installation receipt is invalid JSON")?;
    let parsed = parse_adapter_installation_receipt(value)?;
    let AdapterInstallationReceipt::Release(receipt) = parsed else {
        bail!("refusing to replace local-source adapter `{adapter}` with a signed release");
    };
    anyhow::ensure!(
        receipt.domain == adapter,
        "release receipt domain `{}` does not match requested adapter `{adapter}`",
        receipt.domain
    );
    Ok(Some(receipt))
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
        schema_version: adapter_installation_receipt_schema_version(resolved.release),
        domain: resolved.adapter.domain.clone(),
        version: resolved.version.to_string(),
        source_url: resolved.platform.asset_url.clone(),
        sha256: resolved.platform.sha256.clone(),
        signing_key_id: resolved.platform.signing_key_id.clone(),
        core_compatibility: resolved.release.core_compatibility.clone(),
        compatibility: resolved.release.compatibility.clone(),
        compatibility_sha256: resolved.release.compatibility_sha256.clone(),
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
    validate_release_installation_receipt(&receipt)?;
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

fn finish_ephemeral_installation<T>(
    mut transaction: InstallTransaction,
    temp: &Path,
    result: anyhow::Result<T>,
) -> anyhow::Result<T> {
    match result {
        Ok(value) => {
            transaction.commit()?;
            remove_path_if_exists(temp)
                .context("failed to clean successful adapter transaction")?;
            Ok(value)
        }
        Err(error) => {
            if let Err(rollback) = transaction.rollback() {
                return Err(error.context(format!(
                    "adapter transaction rollback failed and recovery data was retained at {}: {rollback:#}",
                    temp.display()
                )));
            }
            drop(transaction);
            if let Err(cleanup) = remove_path_if_exists(temp) {
                return Err(error.context(format!(
                    "adapter transaction rolled back but staging cleanup failed at {}: {cleanup:#}",
                    temp.display()
                )));
            }
            Err(error)
        }
    }
}

fn canonical_temp_path(name: impl AsRef<Path>) -> anyhow::Result<PathBuf> {
    let root = fs::canonicalize(std::env::temp_dir())
        .context("failed to resolve the canonical system temporary directory")?;
    anyhow::ensure!(
        root.is_dir(),
        "canonical system temporary directory is not a directory"
    );
    Ok(root.join(name))
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

pub(crate) fn typed_harness_resource_plan(
    bundle: &Path,
    home: &Path,
    manifest_path: &str,
) -> anyhow::Result<Vec<(PathBuf, PathBuf)>> {
    use crate::harness_config::HarnessResourceKind;
    use crate::release_index::AdapterResourceKind;

    let (config, _) = effective_adapter_harness_config(home)?;
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

fn install_typed_harness_resources(plan: &[(PathBuf, PathBuf)], quiet: bool) -> anyhow::Result<()> {
    for (source, target) in plan {
        if source.is_dir() {
            copy_dir_recursive(source, target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(source, target)?;
        }
        if !quiet {
            println!("├─ Harness resource {}", target.display());
        }
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
    AvailableAdapter {
        slug: "evidence",
        title: "Evidence adapter",
        source: "",
        install: "ldgr adapter install evidence",
        workspace_package: Some("ldgr-evidence"),
        git: None,
        release: Some(commercial_release("evidence", "ldgr-evidence")),
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
    home: &Path,
) -> anyhow::Result<()> {
    let Some(package) = entry.workspace_package else {
        bail!(
            "adapter `{}` does not have a workspace package; use release/git install instead",
            entry.slug
        );
    };
    install_adapter_from_source_root_with_package(
        entry.slug,
        package,
        source_root,
        install_root,
        home,
    )
}

#[derive(Debug, PartialEq, Eq)]
struct AdapterSourcePackage {
    bundle_root: PathBuf,
    cargo_manifest: PathBuf,
}

fn resolve_adapter_source_package(
    package: &str,
    source_root: &Path,
) -> anyhow::Result<AdapterSourcePackage> {
    let source_root = source_root
        .canonicalize()
        .with_context(|| format!("failed to resolve source root {}", source_root.display()))?;
    let candidates = [source_root.join(package), source_root.clone()];

    for bundle_root in candidates {
        let adapter_manifest = bundle_root.join("adapter.toml");
        let cargo_manifest = bundle_root.join("Cargo.toml");
        if !adapter_manifest.is_file() || !cargo_manifest.is_file() {
            continue;
        }
        let cargo: toml::Value = toml::from_str(&fs::read_to_string(&cargo_manifest)?)
            .with_context(|| format!("failed to parse {}", cargo_manifest.display()))?;
        let cargo_package = cargo
            .get("package")
            .and_then(|value| value.get("name"))
            .and_then(toml::Value::as_str);
        if cargo_package == Some(package) {
            return Ok(AdapterSourcePackage {
                bundle_root,
                cargo_manifest,
            });
        }
    }

    bail!(
        "source root {} does not contain adapter package `{package}`; expected adapter.toml and Cargo.toml in the source root or its {package}/ child",
        source_root.display()
    )
}

fn source_adapter_install_command(
    package: &str,
    source: &AdapterSourcePackage,
    install_root: &Path,
) -> Command {
    let mut command = Command::new("cargo");
    command
        .arg("run")
        .arg("--manifest-path")
        .arg(&source.cargo_manifest)
        .arg("-p")
        .arg(package)
        .arg("--")
        .arg("adapter")
        .arg("install")
        .arg("--install-root")
        .arg(install_root)
        .arg("--print-path")
        .current_dir(&source.bundle_root);
    command
}

fn install_adapter_from_source_root_with_package(
    adapter: &str,
    package: &str,
    source_root: &Path,
    install_root: &Path,
    home: &Path,
) -> anyhow::Result<()> {
    let temp = canonical_temp_path(format!(
        "ldgr-adapter-source-install-{adapter}-{}",
        std::process::id()
    ))?;
    remove_path_if_exists(&temp)?;
    let mut transaction = InstallTransaction::new(temp.join("rollback"))?;
    let result = apply_source_adapter_update(
        adapter,
        package,
        source_root,
        install_root,
        home,
        None,
        &mut transaction,
        false,
    );
    finish_ephemeral_installation(transaction, &temp, result)
}

#[allow(clippy::too_many_arguments)]
pub(crate) fn apply_source_adapter_update(
    adapter: &str,
    package: &str,
    source_root: &Path,
    install_root: &Path,
    home: &Path,
    expected_source_sha256: Option<&str>,
    transaction: &mut InstallTransaction,
    quiet: bool,
) -> anyhow::Result<()> {
    let source = resolve_adapter_source_package(package, source_root)?;
    if let Some(expected) = expected_source_sha256 {
        let actual = digest_source_bundle(&source.bundle_root)?;
        anyhow::ensure!(
            actual == expected,
            "local source changed after adapter update planning; inspect and plan again"
        );
    }
    let namespace = package.strip_prefix("ldgr-").unwrap_or(package);
    let namespace = namespace.strip_suffix("-adapter").unwrap_or(namespace);
    anyhow::ensure!(
        namespace == adapter,
        "source package `{package}` resolves namespace `{namespace}`, not requested adapter `{adapter}`"
    );
    anyhow::ensure!(
        !paths_overlap(&source.bundle_root, install_root)?,
        "source bundle and install root must not overlap; choose an install root outside {}",
        source.bundle_root.display()
    );
    validate_adapter_bundle_contract(&source.bundle_root, namespace)?;
    let previous_receipt = prepare_source_reinstall(adapter, install_root, home)?;
    transaction.snapshot(install_root)?;
    let marker = home.join(".ldgr/installed-adapters").join(adapter);
    transaction.snapshot(&marker)?;
    if let Some(previous) = &previous_receipt {
        for resource in &previous.owned_resources {
            transaction.snapshot(Path::new(&resource.path))?;
        }
    }
    if previous_receipt.is_none() {
        ensure_adapter_harness_config(home, transaction, quiet)?;
    }
    let anticipated_resources = source_harness_resource_plan(&source.bundle_root, home)?;
    let previously_owned = previous_receipt
        .as_ref()
        .map(|receipt| {
            receipt
                .owned_resources
                .iter()
                .map(|resource| PathBuf::from(&resource.path))
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    for resource in &anticipated_resources {
        if resource.target.exists()
            && !previously_owned
                .iter()
                .any(|owned| owned == &resource.target)
        {
            bail!(
                "refusing to overwrite unowned harness resource {}; remove it or choose a different harness resource path",
                resource.target.display()
            );
        }
        transaction.snapshot(&resource.target)?;
    }
    if !quiet {
        println!("├─ Source checkout {}", source_root.display());
        println!("├─ Adapter manifest {}", source.cargo_manifest.display());
    }
    let mut command = source_adapter_install_command(package, &source, install_root);
    if quiet {
        command.stdout(std::process::Stdio::null());
    }
    let status = command.status()?;
    if !status.success() {
        bail!("adapter installer failed for package `{package}` with status {status}");
    }
    patch_adapter_argv_to_source_runner(install_root, package, &source.cargo_manifest)?;
    let resource_plan = source_harness_resource_plan(install_root, home)?;
    for resource in &resource_plan {
        anyhow::ensure!(
            anticipated_resources
                .iter()
                .any(|anticipated| anticipated.target == resource.target),
            "adapter installer introduced an unanticipated harness resource {}",
            resource.target.display()
        );
        transaction.snapshot(&resource.target)?;
    }
    transaction.begin_activation()?;
    if let Some(previous) = &previous_receipt {
        for resource in &previous.owned_resources {
            let path = PathBuf::from(&resource.path);
            if !resource_plan.iter().any(|desired| desired.target == path) {
                remove_path_if_exists(&path)?;
            }
        }
    }
    install_source_harness_resources(&resource_plan, quiet)?;
    let normalized_install_root = absolute_path(install_root)?;
    write_file(
        &marker,
        &format!(
            "install_root={}\ninstall_kind=local_source\n",
            normalized_install_root.display()
        ),
    )?;
    write_source_installation_receipt(
        adapter,
        package,
        &source,
        install_root,
        &marker,
        &resource_plan,
    )?;
    Ok(())
}

#[derive(Debug)]
pub(crate) struct SourceHarnessResource {
    pub(crate) source: PathBuf,
    pub(crate) target: PathBuf,
    pub(crate) root: PathBuf,
}

pub(crate) fn source_harness_resource_plan(
    install_root: &Path,
    home: &Path,
) -> anyhow::Result<Vec<SourceHarnessResource>> {
    let config = read_ldgr_harness_config(home);
    let mut plan = Vec::new();
    append_source_resource_children(
        &mut plan,
        &install_root.join("prompts"),
        configured_prompt_dirs(home, &config),
    )?;
    append_source_resource_children(
        &mut plan,
        &install_root.join("skills"),
        configured_skill_dirs(home, &config),
    )?;
    append_source_resource_children(
        &mut plan,
        &install_root.join("extensions"),
        configured_extension_dirs(home, &config),
    )?;
    let mut targets = std::collections::BTreeSet::new();
    for resource in &plan {
        anyhow::ensure!(
            targets.insert(resource.target.clone()),
            "source adapter harness resource collision at {}",
            resource.target.display()
        );
    }
    Ok(plan)
}

fn append_source_resource_children(
    plan: &mut Vec<SourceHarnessResource>,
    source_root: &Path,
    target_roots: Vec<PathBuf>,
) -> anyhow::Result<()> {
    if !source_root.is_dir() {
        return Ok(());
    }
    let mut children = fs::read_dir(source_root)?.collect::<Result<Vec<_>, _>>()?;
    children.sort_by_key(|entry| entry.file_name());
    for root in target_roots {
        let root = absolute_path(&root)?;
        for child in &children {
            plan.push(SourceHarnessResource {
                source: child.path(),
                target: root.join(child.file_name()),
                root: root.clone(),
            });
        }
    }
    Ok(())
}

fn install_source_harness_resources(
    plan: &[SourceHarnessResource],
    quiet: bool,
) -> anyhow::Result<()> {
    for resource in plan {
        if resource.source.is_dir() {
            copy_dir_recursive(&resource.source, &resource.target)?;
        } else {
            if let Some(parent) = resource.target.parent() {
                fs::create_dir_all(parent)?;
            }
            fs::copy(&resource.source, &resource.target)?;
        }
        if !quiet {
            println!(
                "\u{251c}\u{2500} Harness resource {}",
                resource.target.display()
            );
        }
    }
    Ok(())
}

pub(crate) fn source_allowed_resource_roots(home: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let config = read_ldgr_harness_config(home);
    let mut roots = configured_prompt_dirs(home, &config);
    roots.extend(configured_skill_dirs(home, &config));
    roots.extend(configured_extension_dirs(home, &config));
    roots.extend([
        home.join(".ldgr/prompts"),
        home.join(".pi/agent/skills"),
        home.join(".pi/agent/extensions"),
    ]);
    let mut normalized = Vec::new();
    for root in roots {
        let root = absolute_path(&root)?;
        if !normalized.iter().any(|existing| existing == &root) {
            normalized.push(root);
        }
    }
    Ok(normalized)
}

fn source_owned_resources(
    plan: &[SourceHarnessResource],
) -> anyhow::Result<Vec<crate::release_index::OwnedResource>> {
    plan.iter()
        .map(|resource| {
            Ok(crate::release_index::OwnedResource {
                path: absolute_path(&resource.target)?.display().to_string(),
                sha256: digest_path(&resource.target)?,
            })
        })
        .collect()
}

fn source_resource_roots(plan: &[SourceHarnessResource]) -> anyhow::Result<Vec<String>> {
    let mut roots = Vec::new();
    for resource in plan {
        let root = absolute_path(&resource.root)?.display().to_string();
        if !roots.iter().any(|existing| existing == &root) {
            roots.push(root);
        }
    }
    Ok(roots)
}

fn write_source_installation_receipt(
    adapter: &str,
    package: &str,
    source: &AdapterSourcePackage,
    install_root: &Path,
    marker: &Path,
    resources: &[SourceHarnessResource],
) -> anyhow::Result<()> {
    let installed_at_unix_seconds = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_secs();
    let source_adapter_manifest = source.bundle_root.join("adapter.toml");
    let installed_adapter_manifest = install_root.join("adapter.toml");
    let source_resource_manifest = source.bundle_root.join("adapter-resources.json");
    let installed_resource_manifest = install_root.join("adapter-resources.json");
    let receipt = crate::release_index::SourceInstallationReceipt {
        schema_version: SOURCE_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION,
        install_kind: "local_source".to_owned(),
        domain: adapter.to_owned(),
        installed_at_unix_seconds,
        source: crate::release_index::SourceInstallIdentity {
            package: package.to_owned(),
            bundle_root: absolute_path(&source.bundle_root)?.display().to_string(),
            cargo_manifest: absolute_path(&source.cargo_manifest)?.display().to_string(),
            bundle_sha256: digest_source_bundle(&source.bundle_root)?,
        },
        manifest_digests: crate::release_index::SourceManifestDigests {
            source_adapter_manifest_sha256: digest_path(&source_adapter_manifest)?,
            source_cargo_manifest_sha256: digest_path(&source.cargo_manifest)?,
            installed_adapter_manifest_sha256: digest_path(&installed_adapter_manifest)?,
            source_resource_manifest_sha256: source_resource_manifest
                .is_file()
                .then(|| digest_path(&source_resource_manifest))
                .transpose()?,
            installed_resource_manifest_sha256: installed_resource_manifest
                .is_file()
                .then(|| digest_path(&installed_resource_manifest))
                .transpose()?,
        },
        installer_invocation: source_adapter_installer_argv(package, source, install_root),
        executable_invocations: source_executable_invocations(&installed_adapter_manifest)?,
        installed_files: source_installed_files(install_root)?,
        owned_resources: source_owned_resources(resources)?,
        ownership: crate::release_index::SourceOwnershipBoundaries {
            install_root: absolute_path(install_root)?.display().to_string(),
            marker_path: absolute_path(marker)?.display().to_string(),
            source_checkout_owned: false,
            generated_paths: vec!["source-target".to_owned()],
            external_resource_roots: source_resource_roots(resources)?,
        },
        verified_release: false,
    };
    write_source_receipt_file(install_root, &receipt)
}

fn write_source_receipt_file(
    install_root: &Path,
    receipt: &crate::release_index::SourceInstallationReceipt,
) -> anyhow::Result<()> {
    validate_source_installation_receipt(receipt)?;
    fs::write(
        install_root.join("installation-receipt.json"),
        format!("{}\n", serde_json::to_string_pretty(receipt)?),
    )?;
    Ok(())
}

fn source_adapter_installer_argv(
    package: &str,
    source: &AdapterSourcePackage,
    install_root: &Path,
) -> Vec<String> {
    vec![
        "cargo".to_owned(),
        "run".to_owned(),
        "--manifest-path".to_owned(),
        source.cargo_manifest.display().to_string(),
        "-p".to_owned(),
        package.to_owned(),
        "--".to_owned(),
        "adapter".to_owned(),
        "install".to_owned(),
        "--install-root".to_owned(),
        install_root.display().to_string(),
        "--print-path".to_owned(),
    ]
}

fn source_executable_invocations(
    manifest_path: &Path,
) -> anyhow::Result<Vec<crate::release_index::SourceExecutableInvocation>> {
    let manifest: toml::Value = toml::from_str(&fs::read_to_string(manifest_path)?)
        .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
    let mut invocations = Vec::new();
    for (table, kind, name_field) in [
        ("commands", "namespace", "namespace"),
        ("tools", "tool", "name"),
    ] {
        for entry in manifest
            .get(table)
            .and_then(toml::Value::as_array)
            .into_iter()
            .flatten()
        {
            let name = entry
                .get(name_field)
                .and_then(toml::Value::as_str)
                .with_context(|| format!("installed adapter {kind} has no {name_field}"))?
                .to_owned();
            let argv = entry
                .get("argv")
                .and_then(toml::Value::as_array)
                .with_context(|| format!("installed adapter {kind} has no argv"))?
                .iter()
                .map(|value| {
                    value.as_str().map(str::to_owned).with_context(|| {
                        format!("installed adapter {kind} argv must contain strings")
                    })
                })
                .collect::<anyhow::Result<Vec<_>>>()?;
            invocations.push(crate::release_index::SourceExecutableInvocation {
                kind: kind.to_owned(),
                name,
                argv,
            });
        }
    }
    Ok(invocations)
}

fn source_installed_files(
    install_root: &Path,
) -> anyhow::Result<Vec<crate::release_index::OwnedResource>> {
    source_installed_file_paths(install_root)?
        .into_iter()
        .map(|relative| {
            let path = install_root.join(&relative);
            Ok(crate::release_index::OwnedResource {
                path: relative,
                sha256: digest_path(&path)?,
            })
        })
        .collect()
}

fn source_installed_file_paths(install_root: &Path) -> anyhow::Result<Vec<String>> {
    let mut files = Vec::new();
    collect_digest_files(install_root, install_root, &mut files)?;
    let mut paths = files
        .into_iter()
        .map(|(relative, _)| relative)
        .filter(|relative| {
            relative != "installation-receipt.json"
                && relative != "source-target"
                && !relative.starts_with("source-target/")
        })
        .collect::<Vec<_>>();
    paths.sort();
    Ok(paths)
}

fn digest_source_bundle(source_root: &Path) -> anyhow::Result<String> {
    use sha2::{Digest, Sha256};

    let mut files = Vec::new();
    collect_source_identity_files(source_root, source_root, &mut files)?;
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

fn collect_source_identity_files(
    root: &Path,
    current: &Path,
    files: &mut Vec<(String, Vec<u8>)>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(current)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        if path.is_dir() && (name == ".git" || name == "target" || name == "source-target") {
            continue;
        }
        if path.is_dir() {
            collect_source_identity_files(root, &path, files)?;
        } else if path.is_file() && name != ".git" {
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

fn verify_source_identity_paths(
    receipt: &crate::release_index::SourceInstallationReceipt,
    source: &AdapterSourcePackage,
) -> anyhow::Result<()> {
    anyhow::ensure!(
        paths_match(Path::new(&receipt.source.bundle_root), &source.bundle_root)?,
        "recorded source bundle root no longer resolves to the adapter package"
    );
    anyhow::ensure!(
        paths_match(
            Path::new(&receipt.source.cargo_manifest),
            &source.cargo_manifest
        )?,
        "recorded Cargo manifest no longer resolves to the adapter package"
    );
    Ok(())
}

pub(crate) fn validate_adapter_bundle_contract(bundle: &Path, adapter: &str) -> anyhow::Result<()> {
    let v2_path = bundle.join("adapter-compatibility.json");
    if v2_path.exists() {
        let sidecar = crate::adapter_compatibility::parse_adapter_compatibility_v2(
            &fs::read_to_string(&v2_path)
                .with_context(|| format!("failed to read {}", v2_path.display()))?,
        )
        .map_err(anyhow::Error::new)
        .with_context(|| format!("adapter {adapter} has invalid v2 compatibility metadata"))?;
        anyhow::ensure!(
            sidecar.adapter == adapter,
            "adapter bundle identity {} does not match requested adapter {adapter}",
            sidecar.adapter
        );
        return Ok(());
    }

    validate_legacy_adapter_bundle_contract(bundle, adapter)
}

pub(crate) fn validate_legacy_adapter_bundle_contract(
    bundle: &Path,
    adapter: &str,
) -> anyhow::Result<()> {
    let manifest_path = bundle.join("adapter.toml");
    if manifest_path.is_file() {
        let manifest: toml::Value = toml::from_str(&fs::read_to_string(&manifest_path)?)
            .with_context(|| format!("failed to parse {}", manifest_path.display()))?;
        let generated = manifest
            .get("adapter")
            .and_then(|value| value.get("core_version"))
            .and_then(toml::Value::as_str)
            == Some("generated");
        if !generated {
            return Ok(());
        }
    }
    let path = bundle.join("adapter-database-contract.json");
    let text = fs::read_to_string(&path).with_context(|| {
        format!(
            "adapter {adapter} bundle is missing generated database contract {}",
            path.display()
        )
    })?;
    let contract = crate::database_contract::parse_and_validate_adapter_contract(&text)
        .with_context(|| format!("adapter {adapter} is incompatible with this Core release"))?;
    anyhow::ensure!(
        contract.component.namespace == adapter,
        "adapter bundle namespace {} does not match requested adapter {adapter}",
        contract.component.namespace
    );
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
    run_adapter_binary_installer_with_output(binary, adapter, install_root, false)
}

fn run_adapter_binary_installer_with_output(
    binary: impl AsRef<std::ffi::OsStr>,
    adapter: &str,
    install_root: &Path,
    quiet: bool,
) -> anyhow::Result<()> {
    let binary_ref = binary.as_ref();
    let mut command = Command::new(binary_ref);
    command
        .arg("adapter")
        .arg("install")
        .arg("--install-root")
        .arg(install_root)
        .arg("--print-path");
    if quiet {
        command.stdout(std::process::Stdio::null());
    }
    let status = command.status()?;
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
    let temp = canonical_temp_path(format!(
        "ldgr-adapter-install-{}-{}",
        entry.slug,
        std::process::id()
    ))?;
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
    let installed_binary =
        install_release_binary(install_root, home, release.binary, &platform, false)?;
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
    quiet: bool,
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
    if !quiet {
        println!("├─ Installed binary {}", dest.display());
    }
    Ok(Some(dest))
}

pub(crate) fn adapter_manifest_references_binary(
    install_root: &Path,
    binary: &str,
) -> anyhow::Result<bool> {
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
    cargo_manifest: &Path,
) -> anyhow::Result<()> {
    let manifest = install_root.join("adapter.toml");
    if !manifest.is_file() {
        return Ok(());
    }
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
    let toml_path = home.join(".ldgr/config.toml");
    if let Ok(text) = fs::read_to_string(&toml_path) {
        if let Ok(config) = crate::harness_config::parse_harness_config_toml(&text) {
            return Some(config);
        }
    }
    let text = fs::read_to_string(home.join(".ldgr/config.json")).ok()?;
    crate::harness_config::parse_harness_config_json(&text).ok()
}

fn default_adapter_harness_config(home: &Path) -> HarnessConfig {
    HarnessConfig {
        default_harness: Some("pi".to_owned()),
        selected_harnesses: vec!["pi".to_owned()],
        installed: vec![crate::harness_config::InstalledHarness {
            harness: "pi".to_owned(),
            prompt_paths: vec![home.join(".ldgr/prompts")],
            skill_paths: vec![home.join(".pi/agent/skills")],
            extension_paths: vec![home.join(".pi/agent/extensions")],
            command_paths: Vec::new(),
            extensions: Default::default(),
        }],
        ..HarnessConfig::default()
    }
}

fn effective_adapter_harness_config(home: &Path) -> anyhow::Result<(HarnessConfig, bool)> {
    if let Some(config) = read_ldgr_harness_config(home) {
        return Ok((config, false));
    }
    let toml_path = home.join(".ldgr/config.toml");
    let json_path = home.join(".ldgr/config.json");
    anyhow::ensure!(
        !toml_path.exists() && !json_path.exists(),
        "adapter installation found an unreadable or invalid LDGR harness config; repair {} or {} before retrying",
        toml_path.display(),
        json_path.display()
    );
    Ok((default_adapter_harness_config(home), true))
}

fn ensure_adapter_harness_config(
    home: &Path,
    transaction: &mut InstallTransaction,
    quiet: bool,
) -> anyhow::Result<HarnessConfig> {
    let (config, needs_write) = effective_adapter_harness_config(home)?;
    if needs_write {
        let toml_path = home.join(".ldgr/config.toml");
        let json_path = home.join(".ldgr/config.json");
        transaction.snapshot(&toml_path)?;
        transaction.snapshot(&json_path)?;
        let (toml_path, json_path) = write_harness_config_files(home, &config)?;
        if !quiet {
            println!(
                "├─ Created default Pi harness config {} and {}",
                toml_path.display(),
                json_path.display()
            );
        }
    }
    Ok(config)
}

fn write_harness_config_files(
    home: &Path,
    config: &HarnessConfig,
) -> anyhow::Result<(PathBuf, PathBuf)> {
    let config_path = home.join(".ldgr/config.toml");
    let legacy_config_path = home.join(".ldgr/config.json");
    write_file(
        &config_path,
        &format!("{}\n", toml::to_string_pretty(config)?),
    )?;
    write_file(
        &legacy_config_path,
        &format!("{}\n", serde_json::to_string_pretty(config)?),
    )?;
    Ok((config_path, legacy_config_path))
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

fn resolve_install_telemetry_consent(
    args: &InstallArgs,
    ldgr_home: &Path,
) -> anyhow::Result<TelemetryConsent> {
    let stdin_is_interactive = stdin_is_terminal();
    let stdin = io::stdin();
    let mut input = stdin.lock();
    let stdout = io::stdout();
    let mut output = stdout.lock();
    resolve_install_telemetry_consent_with_io(
        args,
        ldgr_home,
        stdin_is_interactive,
        &mut input,
        &mut output,
    )
}

fn resolve_install_telemetry_consent_with_io(
    args: &InstallArgs,
    ldgr_home: &Path,
    stdin_is_interactive: bool,
    input: &mut impl BufRead,
    output: &mut impl Write,
) -> anyhow::Result<TelemetryConsent> {
    let mut consent = load_telemetry_consent(ldgr_home)?;
    if let Some(choice) = args.telemetry {
        consent.decision = match choice {
            TelemetryInstallChoice::Enable => TelemetryConsentDecision::Enabled,
            TelemetryInstallChoice::Disable => TelemetryConsentDecision::Disabled,
        };
    }

    if args.yes || !stdin_is_interactive {
        if args.telemetry.is_some() {
            save_telemetry_consent(ldgr_home, &consent)?;
        }
        return Ok(consent);
    }

    writeln!(output, "◇ Telemetry choices")?;
    print_telemetry_scope(output)?;
    consent.decision = if args.telemetry.is_some() {
        writeln!(
            output,
            "Basic anonymous telemetry selected by --telemetry: {}.",
            consent.decision.as_str()
        )?;
        consent.decision
    } else {
        prompt_telemetry_decision(
            input,
            output,
            "Enable basic anonymous telemetry?",
            consent.decision,
        )?
    };

    writeln!(output)?;
    print_experience_donation_scope(output)?;
    consent.donation_decision = prompt_telemetry_decision(
        input,
        output,
        "Enable detailed experience donation?",
        consent.donation_decision,
    )?;
    save_telemetry_consent(ldgr_home, &consent)?;
    writeln!(output, "│")?;
    Ok(consent)
}

fn prompt_telemetry_decision(
    input: &mut impl BufRead,
    output: &mut impl Write,
    question: &str,
    default: TelemetryConsentDecision,
) -> anyhow::Result<TelemetryConsentDecision> {
    let default_enabled = default == TelemetryConsentDecision::Enabled;
    let options = if default_enabled { "Y/n" } else { "y/N" };
    loop {
        write!(output, "{question} [{options}] ")?;
        output.flush()?;
        let mut answer = String::new();
        if input.read_line(&mut answer)? == 0 {
            bail!("input closed while waiting for telemetry choice: {question}");
        }
        match answer.trim().to_ascii_lowercase().as_str() {
            "" => {
                return Ok(if default_enabled {
                    TelemetryConsentDecision::Enabled
                } else {
                    TelemetryConsentDecision::Disabled
                });
            }
            "y" | "yes" => return Ok(TelemetryConsentDecision::Enabled),
            "n" | "no" => return Ok(TelemetryConsentDecision::Disabled),
            _ => writeln!(output, "Please answer yes or no.")?,
        }
    }
}

fn print_telemetry_scope(output: &mut impl Write) -> anyhow::Result<()> {
    writeln!(
        output,
        "LDGR shares privacy-minimized anonymous construction telemetry by default."
    )?;
    writeln!(
        output,
        "Finite command, execution, validation, artifact, and outcome classes are consolidated locally; rare constructions are suppressed before release."
    )?;
    writeln!(
        output,
        "It does not include prompts, content, paths, commands, arguments, output, names, arbitrary labels, identifiers, exact timestamps, or linkable installation data. Disable with `ldgr telemetry disable`. Experience donation is separate and off by default."
    )?;
    Ok(())
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

/// Resolve the requirements-interview depth from the flag, or ask for it.
/// Non-interactive installs take the default rather than blocking.
fn select_interview_depth(args: &InstallArgs) -> anyhow::Result<InterviewDepth> {
    if let Some(raw) = &args.interview_depth {
        let depth = InterviewDepth::parse(raw).ok_or_else(|| {
            anyhow::anyhow!(
                "unknown --interview-depth `{raw}`; expected high, medium, low, or none"
            )
        })?;
        println!("◇ Requirements interview: {}", depth.as_str());
        return Ok(depth);
    }
    if args.yes || !stdin_is_terminal() {
        let depth = InterviewDepth::default();
        println!("◇ Requirements interview: {} (default)", depth.as_str());
        return Ok(depth);
    }
    let items = InterviewDepth::VALUES
        .iter()
        .map(|depth| format!("{} — {}", depth.as_str(), depth.describe()))
        .collect::<Vec<_>>();
    let theme = ColorfulTheme::default();
    let selection = Select::with_theme(&theme)
        .with_prompt("How thoroughly should the agent interview you about project requirements?")
        .items(&items)
        .default(1)
        .interact_opt()?;
    Ok(selection
        .and_then(|index| InterviewDepth::VALUES.get(index).copied())
        .unwrap_or_default())
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
    install_harness_skill(harness, home)
}

fn ensure_agentctl_dependency(
    skip: bool,
    network_forbidden: bool,
) -> anyhow::Result<serde_json::Value> {
    if skip {
        println!("├─ Skipped agentctl install (--no-agentctl)");
        return Ok(serde_json::json!({
            "required": true,
            "installed_by_ldgr": false,
            "status": "skipped",
            "install_hint": format!("cargo install --git {AGENTCTL_REPO} --tag v{AGENTCTL_VERSION} --locked --force"),
            "required_version": AGENTCTL_VERSION
        }));
    }
    if installed_agentctl_is_compatible()? {
        println!("├─ compatible agentctl {AGENTCTL_VERSION} already available on PATH");
        return Ok(serde_json::json!({
            "required": true,
            "installed_by_ldgr": false,
            "status": "already_on_path",
            "command": "agentctl",
            "version": AGENTCTL_VERSION
        }));
    }
    if network_forbidden {
        bail!(
            "local release store mode forbids downloading agentctl; install the paired Core/agentctl bundle first or rerun with --no-agentctl"
        );
    }

    println!("├─ Installing agentctl {AGENTCTL_VERSION} from {AGENTCTL_REPO}");
    let status = Command::new("cargo")
        .arg("install")
        .arg("--git")
        .arg(AGENTCTL_REPO)
        .arg("--tag")
        .arg(format!("v{AGENTCTL_VERSION}"))
        .arg("--locked")
        .arg("--force")
        .stdin(Stdio::null())
        .status()
        .map_err(|error| anyhow::anyhow!("failed to start cargo install for agentctl: {error}"))?;
    if !status.success() {
        bail!(
            "agentctl install failed with status {status}; install it with `cargo install --git {AGENTCTL_REPO} --tag v{AGENTCTL_VERSION} --locked --force` or rerun `ldgr install --no-agentctl` to manage it yourself"
        );
    }
    if !installed_agentctl_is_compatible()? {
        bail!(
            "agentctl {AGENTCTL_VERSION} was installed but `agentctl` on PATH still resolves to a different version; update the resolved binary directory or install the paired LDGR Core release bundle"
        );
    }
    Ok(serde_json::json!({
        "required": true,
        "installed_by_ldgr": true,
        "status": "installed",
        "command": "agentctl",
        "source": AGENTCTL_REPO,
        "version": AGENTCTL_VERSION
    }))
}

fn installed_agentctl_is_compatible() -> anyhow::Result<bool> {
    if !command_on_path("agentctl") {
        return Ok(false);
    }
    let output = Command::new("agentctl")
        .arg("--version")
        .stdin(Stdio::null())
        .output()
        .context("failed to inspect agentctl on PATH")?;
    if !output.status.success() {
        return Ok(false);
    }
    let text = String::from_utf8(output.stdout).context("agentctl --version was not UTF-8")?;
    let Some(version) = text.split_whitespace().nth(1) else {
        return Ok(false);
    };
    let Ok(version) = semver::Version::parse(version) else {
        return Ok(false);
    };
    let requirement =
        semver::VersionReq::parse(AGENTCTL_REQUIREMENT).expect("valid agentctl requirement");
    Ok(requirement.matches(&version))
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

/// Global skill root for a harness. See docs/harness-skill-locations.md.
fn harness_skill_root(harness: HarnessKind, home: &Path) -> PathBuf {
    match harness {
        HarnessKind::Pi => home.join(".pi/agent/skills"),
        HarnessKind::Codex => home.join(".codex/skills"),
        HarnessKind::Claude => home.join(".claude/skills"),
        HarnessKind::Openclaw => home.join(".openclaw/skills"),
    }
}

/// Installing ldgr into a harness means one thing: writing the single skill.
/// There are no extensions, slash commands, or per-harness guides — the skill
/// routes the agent to the CLI, and the CLI describes itself.
fn install_harness_skill(harness: HarnessKind, home: &Path) -> anyhow::Result<serde_json::Value> {
    let root = harness_skill_root(harness, home);
    let skill = root.join("ldgr/SKILL.md");
    write_file(&skill, LDGR_SKILL)?;
    println!("├─ {} skill {}", harness_name(harness), skill.display());
    Ok(serde_json::json!({
        "harness": harness_name(harness),
        "skill_paths": [root],
        "skill_file": skill,
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

/// The one skill ldgr installs. Everything else an agent needs is discoverable
/// from the CLI itself via `ldgr workflow` and `--help`.
const LDGR_SKILL: &str = include_str!("../../../skills/ldgr/SKILL.md");

const CORE_WORKFLOW: &str = include_str!("../../../workflows/core.md");

/// Read the configured interview depth, falling back to the default when no
/// config exists yet. A missing or unreadable config is not an error here —
/// the workflow is still worth printing.
fn configured_interview_depth() -> InterviewDepth {
    home_dir()
        .ok()
        .and_then(|home| read_ldgr_harness_config(&home))
        .map(|config| config.interview_depth)
        .unwrap_or_default()
}

pub fn handle_workflow(args: WorkflowArgs) -> anyhow::Result<()> {
    let depth = configured_interview_depth();
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "namespace": "core",
                "workflow": CORE_WORKFLOW,
                "interview_depth": depth.as_str(),
                "interview_depth_behavior": depth.describe(),
                "adapter_workflows": "ldgr <adapter> workflow",
            }))?
        );
    } else {
        print!("{CORE_WORKFLOW}");
        println!(
            "\nConfigured requirements interview: {} — {}.",
            depth.as_str(),
            depth.describe()
        );
        println!("Change it with `ldgr config set interview-depth <high|medium|low|none>`.");
        println!("Installed adapters expose their own workflow: `ldgr <adapter> workflow`.");
    }
    Ok(())
}

pub fn handle_config(args: ConfigArgs) -> anyhow::Result<()> {
    let home = home_dir()?;
    let config_path = home.join(".ldgr/config.toml");
    let legacy_config_path = home.join(".ldgr/config.json");
    match args.command {
        ConfigCommand::Show(show) => {
            let config = read_ldgr_harness_config(&home).unwrap_or_default();
            if show.json {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&serde_json::json!({
                        "config_path": config_path,
                        "legacy_config_path": legacy_config_path,
                        "exists": config_path.is_file(),
                        "interview_depth": config.interview_depth.as_str(),
                        "updates": {
                            "check": config.updates.check.as_str(),
                            "interval_hours": config.updates.interval_hours,
                            "channel": config.updates.channel.as_str(),
                            "include_adapters": config.updates.include_adapters,
                            "notify": config.updates.notify,
                        },
                    }))?
                );
            } else {
                println!("config: {}", config_path.display());
                if !config_path.is_file() {
                    println!("status: not written yet; run `ldgr install`");
                }
                println!(
                    "interview_depth: {} — {}",
                    config.interview_depth.as_str(),
                    config.interview_depth.describe()
                );
                println!("updates.check: {}", config.updates.check.as_str());
                println!("updates.interval-hours: {}", config.updates.interval_hours);
                println!("updates.channel: {}", config.updates.channel.as_str());
                println!(
                    "updates.include-adapters: {}",
                    config.updates.include_adapters
                );
                println!("updates.notify: {}", config.updates.notify);
            }
            Ok(())
        }
        ConfigCommand::Set(set) => {
            let key = set.key.trim().to_ascii_lowercase().replace('_', "-");
            // Preserve every known field and unknown extension while writing
            // canonical TOML plus the legacy JSON compatibility mirror.
            let mut config = read_ldgr_harness_config(&home).unwrap_or_default();
            let rendered = set_harness_config_value(&mut config, &key, &set.value)?;
            let (config_path, legacy_config_path) = write_harness_config_files(&home, &config)?;
            println!("{key}: {rendered}");
            println!("wrote {}", config_path.display());
            println!("wrote {}", legacy_config_path.display());
            Ok(())
        }
    }
}

fn set_harness_config_value(
    config: &mut HarnessConfig,
    key: &str,
    value: &str,
) -> anyhow::Result<String> {
    match key {
        "interview-depth" => {
            let depth = InterviewDepth::parse(value).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown interview-depth `{value}`; expected high, medium, low, or none"
                )
            })?;
            config.interview_depth = depth;
            Ok(format!("{} — {}", depth.as_str(), depth.describe()))
        }
        "updates.check" => {
            let check = UpdateCheck::parse(value).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown updates.check `{value}`; expected startup or never"
                )
            })?;
            config.updates.check = check;
            Ok(check.as_str().to_owned())
        }
        "updates.interval-hours" => {
            let interval = value.trim().parse::<u64>().map_err(|_| {
                anyhow::anyhow!(
                    "invalid updates.interval-hours `{value}`; expected a non-negative integer"
                )
            })?;
            config.updates.interval_hours = interval;
            Ok(interval.to_string())
        }
        "updates.channel" => {
            let channel = UpdateChannel::parse(value).ok_or_else(|| {
                anyhow::anyhow!(
                    "unknown updates.channel `{value}`; expected stable or prerelease"
                )
            })?;
            config.updates.channel = channel;
            Ok(channel.as_str().to_owned())
        }
        "updates.include-adapters" => {
            let include_adapters = parse_config_bool(key, value)?;
            config.updates.include_adapters = include_adapters;
            Ok(include_adapters.to_string())
        }
        "updates.notify" => {
            let notify = parse_config_bool(key, value)?;
            config.updates.notify = notify;
            Ok(notify.to_string())
        }
        _ => anyhow::bail!(
            "unknown config key `{key}`; expected interview-depth, updates.check, updates.interval-hours, updates.channel, updates.include-adapters, or updates.notify"
        ),
    }
}

fn parse_config_bool(key: &str, value: &str) -> anyhow::Result<bool> {
    match value.trim().to_ascii_lowercase().as_str() {
        "true" => Ok(true),
        "false" => Ok(false),
        _ => anyhow::bail!("invalid {key} `{value}`; expected true or false"),
    }
}

pub fn handle_schema(db: &Path, args: SchemaArgs) -> anyhow::Result<()> {
    match args.command {
        SchemaCommand::Doctor(args) => {
            let report = doctor_schema(db);
            if args.json {
                println!("{}", serde_json::to_string_pretty(&report)?);
            } else {
                println!("database: {}", report.database.display());
                println!("readable: {}", report.readable);
                println!("compatible: {}", report.compatible);
                println!(
                    "schema: active={} target={}",
                    report
                        .active_schema_version
                        .map(|value| value.to_string())
                        .unwrap_or_else(|| "unknown".to_string()),
                    report.target_schema_version
                );
                println!("contract: {}", report.contract_hash);
                if !report.pending_migrations.is_empty() {
                    println!(
                        "pending migrations: {}",
                        report
                            .pending_migrations
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(",")
                    );
                    println!("upgrade: run `ldgr migrate` (or `ldgr status`/`ldgr context`) to migrate safely");
                }
                println!("components: {}", report.components.len());
                for component in &report.components {
                    println!(
                        "  {} schema-v{} {}",
                        component.namespace, component.schema_version, component.contract_hash
                    );
                }
                if let Some(backup) = &report.last_backup {
                    println!("last backup: {}", backup.display());
                }
                if let Some(problem) = &report.problem {
                    println!("problem: {problem}");
                }
                if let Some(command) = &report.recovery_command {
                    println!("recovery: {command}");
                }
            }
            anyhow::ensure!(
                report.compatible,
                "database contract doctor found a problem"
            );
            Ok(())
        }
    }
}

#[derive(serde::Serialize)]
struct CliMigrationReport<'a> {
    database: &'a Path,
    migrated: bool,
    from_schema_version: i64,
    to_schema_version: i64,
    contract_hash: &'a str,
    backup: Option<&'a Path>,
}

pub fn handle_migrate(db: &Path, args: MigrateArgs) -> anyhow::Result<()> {
    let before = doctor_schema(db);
    anyhow::ensure!(before.readable, "database is not readable");
    anyhow::ensure!(
        before.compatible,
        "database is not eligible for automatic migration: {}",
        before
            .problem
            .as_deref()
            .unwrap_or("unknown schema problem")
    );
    let from_schema_version = before
        .active_schema_version
        .context("database does not report an active schema version")?;
    let (connection, migration) = open_store_with_migration_info(db)?;
    drop(connection);
    let report = CliMigrationReport {
        database: db,
        migrated: migration.is_some(),
        from_schema_version,
        to_schema_version: crate::store::CURRENT_SCHEMA_VERSION,
        contract_hash: crate::database_contract::DATABASE_CONTRACT_HASH,
        backup: migration.as_ref().map(|info| info.backup.as_path()),
    };
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else if let Some(migration) = migration.as_ref() {
        println!(
            "migrated {}: Core schema v{} -> v{}",
            db.display(),
            migration.from_schema_version,
            migration.to_schema_version
        );
        println!("verified backup: {}", migration.backup.display());
        println!("contract: {}", migration.contract_hash);
    } else {
        println!(
            "no migration needed: {} is already on Core schema v{}",
            db.display(),
            crate::store::CURRENT_SCHEMA_VERSION
        );
        println!(
            "contract: {}",
            crate::database_contract::DATABASE_CONTRACT_HASH
        );
    }
    Ok(())
}

pub fn handle_status(
    connection: &rusqlite::Connection,
    _artifact_root: &Path,
    alignment: &super::super::database_alignment::DatabaseAlignment,
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
        alignment.clone(),
    )?;
    emit(args.json, &status, print_status_summary)?;
    Ok(())
}

pub fn handle_context(
    connection: &rusqlite::Connection,
    _artifact_root: &Path,
    alignment: &super::super::database_alignment::DatabaseAlignment,
    args: ContextArgs,
) -> anyhow::Result<()> {
    let context = read_context(connection)?;
    if args.brief {
        let brief = brief_context(&context, brief_options(args.recent, args.width));
        if args.json {
            let mut value = serde_json::to_value(&brief)?;
            value["database_alignment"] = serde_json::to_value(alignment)?;
            println!("{}", serde_json::to_string_pretty(&value)?);
        } else {
            super::super::database_alignment::print_database_alignment(alignment);
            print_brief_context(&brief);
        }
        return Ok(());
    }
    if args.json {
        let mut value = serde_json::to_value(&context)?;
        value["database_alignment"] = serde_json::to_value(alignment)?;
        value["installed_adapter_namespaces"] =
            serde_json::to_value(AdapterRegistry::discover().installed_domains())?;
        println!("{}", serde_json::to_string_pretty(&value)?);
    } else {
        super::super::database_alignment::print_database_alignment(alignment);
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

pub fn handle_loop_entry(db: &Path, artifact_root: &Path, args: LoopArgs) -> anyhow::Result<()> {
    let project_root = project_root_for_db(db);
    let attempt = ExecutionAttempt::begin_or_adopt(&project_root)?;
    let connection = match crate::store::open_store(db) {
        Ok(connection) => connection,
        Err(error) => {
            attempt.record_durable(None, FailureKind::CoreUnavailable, None)?;
            return Err(error);
        }
    };
    let recovery = reconcile_startup(&connection, &project_root)?;
    print_startup_recovery_report(&recovery);
    if recovery.requires_disposition() {
        attempt.complete();
        bail!(
            "startup reconciliation restored interrupted work with blocking error(s) {}; record a disposition before continuing",
            recovery
                .blocking_error_ids
                .iter()
                .map(i64::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
    let detached = matches!(&args.command, LoopCommand::Run(run) if run.detach);
    let result = handle_loop(&connection, artifact_root, args, attempt.clone());
    match result {
        Ok(()) => {
            if !detached {
                attempt.complete();
            }
            Ok(())
        }
        Err(error) => {
            let failure = classify_launcher_error(&error);
            attempt.record_durable(Some(&connection), failure, None)?;
            Err(error)
        }
    }
}

fn classify_launcher_error(error: &anyhow::Error) -> FailureKind {
    let text = format!("{error:#}");
    if text.contains("failed to spawn") || text.contains("failed to start detached loop") {
        FailureKind::Spawn
    } else if text.contains("failed to wait") || text.contains("reader stopped") {
        FailureKind::UnexpectedDisappearance
    } else {
        FailureKind::Initialization
    }
}

fn handle_loop(
    connection: &rusqlite::Connection,
    artifact_root: &Path,
    args: LoopArgs,
    attempt: ExecutionAttempt,
) -> anyhow::Result<()> {
    match args.command {
        LoopCommand::Run(args) => {
            let agent = resolve_loop_agent(&args)?;
            let summary_agent = resolve_summary_agent(&args)?;
            let prompt = resolve_loop_prompt(connection, &args)?;
            let audit_argv = args
                .audit_argv
                .as_deref()
                .map(parse_argv_json)
                .transpose()?;
            if args.project_complete_requested && audit_argv.is_none() {
                bail!("--audit-argv is required when --project-complete-requested is supplied");
            }
            if args.detach {
                return spawn_detached_loop(artifact_root, &attempt);
            }
            let options = LoopRuntimeOptions {
                prompt,
                agent,
                audit_argv,
                summary_agent,
                summary_log: args.summary_log.clone(),
                project_complete_requested: args.project_complete_requested,
                dry_run: args.dry_run,
                stream_agent_output: args.stream_agent_output,
                agent_timeout: Duration::from_secs(args.agent_timeout_seconds),
                attempt: attempt.clone(),
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

fn spawn_detached_loop(artifact_root: &Path, attempt: &ExecutionAttempt) -> anyhow::Result<()> {
    let executable =
        std::env::current_exe().context("failed to resolve current ldgr executable")?;
    let child_args = std::env::args_os()
        .skip(1)
        .filter(|argument| argument != "--detach")
        .collect::<Vec<_>>();
    let logs_root = artifact_root
        .parent()
        .unwrap_or_else(|| Path::new(".ldgr"))
        .join("logs");
    fs::create_dir_all(&logs_root).with_context(|| {
        format!(
            "failed to create detached loop log directory {}",
            logs_root.display()
        )
    })?;
    let suffix = format!("{}-{}", std::process::id(), timestamp_nanos());
    let stdout_path = logs_root.join(format!("loop-detached-{suffix}.stdout.log"));
    let stderr_path = logs_root.join(format!("loop-detached-{suffix}.stderr.log"));
    let stdout = fs::File::create(&stdout_path)
        .with_context(|| format!("failed to create {}", stdout_path.display()))?;
    let stderr = fs::File::create(&stderr_path)
        .with_context(|| format!("failed to create {}", stderr_path.display()))?;

    let mut command = Command::new(&executable);
    command
        .args(child_args)
        .stdin(Stdio::null())
        .stdout(Stdio::from(stdout))
        .stderr(Stdio::from(stderr));
    configure_child_home(&mut command);
    attempt.configure_child(&mut command);
    configure_detached_process(&mut command);
    let child = command
        .spawn()
        .with_context(|| format!("failed to start detached loop via {}", executable.display()))?;

    println!("detached loop pid={}", child.id());
    println!("stdout: {}", stdout_path.display());
    println!("stderr: {}", stderr_path.display());
    println!("status: ldgr context");
    Ok(())
}

#[cfg(windows)]
fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn configure_detached_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(all(not(unix), not(windows)))]
fn configure_detached_process(_command: &mut Command) {}

fn install_core_harness_resources() -> anyhow::Result<()> {
    fs::create_dir_all(".ldgr")?;
    fs::write(".ldgr/operator-errors.md", CORE_OPERATOR_ERROR_GUIDE)?;
    fs::write(".ldgr/agent-errors.md", CORE_AGENT_ERROR_GUIDE)?;
    fs::write(
        ".ldgr/harness-setup.md",
        "# LDGR harness setup\n\n\
ldgr installs one skill, `ldgr`, into your harness's global skill directory. It routes an agent to the CLI; the CLI describes itself from there.\n\n\
If the skill is not installed, run `ldgr install` (interactive, human-operated) and select your harness. If your harness is not listed, copy the skill directory into whatever global skill path it reads, or point the agent at the CLI directly — `ldgr` works from any shell without a skill.\n\n\
An agent that has not been given the skill should start with `ldgr status` (or `ldgr init` if no `.ldgr/ldgr.db` exists) and then run `ldgr workflow`.\n\n\
LDGR-owned profiles require the paired agentctl/Core release. Run `agentctl discover --json`; if Core compatibility is false, install or roll back both binaries together before starting a loop.\n\n\
Read `.ldgr/operator-errors.md` for the operator policy and `.ldgr/agent-errors.md` for the agent checkpoint requirements.\n",
    )?;
    println!("wrote harness notes .ldgr/harness-setup.md");
    println!("wrote error guidance .ldgr/operator-errors.md .ldgr/agent-errors.md");
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
    fn every_core_agent_prompt_enforces_the_ldgr_pii_boundary() {
        for (name, prompt) in [
            ("installed skill", LDGR_SKILL),
            ("loop prompt", LDGR_CORE_LOOP_PROMPT),
            ("init prompt", INIT_PROJECT_SETUP_PROMPT),
            ("workflow", CORE_WORKFLOW),
        ] {
            let normalized = prompt.split_whitespace().collect::<Vec<_>>().join(" ");
            assert!(
                normalized.contains("personally identifiable information (PII)"),
                "{name}"
            );
            assert!(normalized.contains("credentials, secrets"), "{name}");
            assert!(normalized.contains("raw user prompts"), "{name}");
            assert!(normalized.contains("internal reasoning"), "{name}");
            assert!(normalized.contains("tool output"), "{name}");
            assert!(normalized.contains("project-relative paths"), "{name}");
            assert!(normalized.contains("sanitized summary"), "{name}");
        }
    }

    #[test]
    fn transactional_temp_paths_use_the_canonical_system_directory() -> anyhow::Result<()> {
        let root = fs::canonicalize(std::env::temp_dir())?;
        let path = canonical_temp_path("ldgr-canonical-temp-test")?;
        assert_eq!(path.parent(), Some(root.as_path()));
        Ok(())
    }

    fn install_args_for_telemetry(
        yes: bool,
        telemetry: Option<TelemetryInstallChoice>,
    ) -> InstallArgs {
        InstallArgs {
            command: None,
            harness: Vec::new(),
            yes,
            telemetry,
            no_agentctl: true,
            interview_depth: None,
            adapter: Vec::new(),
            store: None,
        }
    }

    #[test]
    fn first_interactive_install_discloses_separate_opt_out_and_opt_in_choices(
    ) -> anyhow::Result<()> {
        let ldgr_home = tempfile::tempdir()?;
        let args = install_args_for_telemetry(false, None);
        let mut input = std::io::Cursor::new(b"n\ny\n");
        let mut output = Vec::new();

        let consent = resolve_install_telemetry_consent_with_io(
            &args,
            ldgr_home.path(),
            true,
            &mut input,
            &mut output,
        )?;

        assert_eq!(consent.decision, TelemetryConsentDecision::Disabled);
        assert_eq!(consent.donation_decision, TelemetryConsentDecision::Enabled);
        let output = String::from_utf8(output)?;
        assert!(output.contains("◇ Telemetry choices"));
        assert!(output.contains("privacy-minimized anonymous construction telemetry"));
        assert!(output.contains("Enable basic anonymous telemetry? [Y/n]"));
        assert!(output.contains("Experience donation sends model-sanitized LDGR work records"));
        assert!(output.contains("agent instructions prohibit credentials, secrets, PII"));
        assert!(output.contains("Direct Pi conversations and session events are not donated"));
        assert!(output.contains("Enable detailed experience donation? [y/N]"));
        assert_eq!(
            load_telemetry_consent(ldgr_home.path())?,
            consent,
            "both choices must be persisted together"
        );

        let mut later_input = std::io::Cursor::new(b"\n\n");
        let mut later_output = Vec::new();
        let later = resolve_install_telemetry_consent_with_io(
            &args,
            ldgr_home.path(),
            true,
            &mut later_input,
            &mut later_output,
        )?;

        assert_eq!(
            later, consent,
            "reinstall defaults must preserve both choices"
        );
        let later_output = String::from_utf8(later_output)?;
        assert!(later_output.contains("Enable basic anonymous telemetry? [y/N]"));
        assert!(later_output.contains("Enable detailed experience donation? [Y/n]"));
        Ok(())
    }

    #[test]
    fn explicit_anonymous_choice_still_offers_interactive_donation_opt_in() -> anyhow::Result<()> {
        let ldgr_home = tempfile::tempdir()?;
        let args = install_args_for_telemetry(false, Some(TelemetryInstallChoice::Disable));
        let mut input = std::io::Cursor::new(b"y\n");
        let mut output = Vec::new();

        let consent = resolve_install_telemetry_consent_with_io(
            &args,
            ldgr_home.path(),
            true,
            &mut input,
            &mut output,
        )?;

        assert_eq!(consent.decision, TelemetryConsentDecision::Disabled);
        assert_eq!(consent.donation_decision, TelemetryConsentDecision::Enabled);
        let output = String::from_utf8(output)?;
        assert!(output.contains("Basic anonymous telemetry selected by --telemetry: disabled"));
        assert!(!output.contains("Enable basic anonymous telemetry?"));
        assert!(output.contains("Enable detailed experience donation? [y/N]"));
        assert_eq!(load_telemetry_consent(ldgr_home.path())?, consent);
        Ok(())
    }

    #[test]
    fn non_interactive_install_uses_default_and_accepts_explicit_overrides() -> anyhow::Result<()> {
        let ldgr_home = tempfile::tempdir()?;
        let mut input = std::io::Cursor::new(b"Yes\n");
        let mut output = Vec::new();
        let yes_without_telemetry = install_args_for_telemetry(true, None);

        let default = resolve_install_telemetry_consent_with_io(
            &yes_without_telemetry,
            ldgr_home.path(),
            false,
            &mut input,
            &mut output,
        )?;
        assert_eq!(default.decision, TelemetryConsentDecision::Enabled);
        assert!(!ldgr_home.path().join("telemetry-consent.json").exists());
        save_telemetry_consent(
            ldgr_home.path(),
            &default.with_donation(TelemetryConsentDecision::Enabled),
        )?;

        let explicit_disable =
            install_args_for_telemetry(false, Some(TelemetryInstallChoice::Disable));
        let mut flag_input = std::io::Cursor::new(b"");
        let mut flag_output = Vec::new();
        let disabled = resolve_install_telemetry_consent_with_io(
            &explicit_disable,
            ldgr_home.path(),
            false,
            &mut flag_input,
            &mut flag_output,
        )?;
        assert_eq!(disabled.decision, TelemetryConsentDecision::Disabled);
        assert_eq!(
            disabled.donation_decision,
            TelemetryConsentDecision::Enabled,
            "--telemetry must not change the separate donation choice"
        );
        assert!(String::from_utf8(flag_output)?.is_empty());

        let explicit_enable =
            install_args_for_telemetry(true, Some(TelemetryInstallChoice::Enable));
        let mut enable_input = std::io::Cursor::new(b"");
        let mut enable_output = Vec::new();
        let enabled = resolve_install_telemetry_consent_with_io(
            &explicit_enable,
            ldgr_home.path(),
            false,
            &mut enable_input,
            &mut enable_output,
        )?;
        assert_eq!(enabled.decision, TelemetryConsentDecision::Enabled);
        assert!(String::from_utf8(enable_output)?.is_empty());
        Ok(())
    }

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
            store: None,
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
            ("conduct", "ldgr-conduct"),
            ("evidence", "ldgr-evidence"),
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
    fn standalone_adapter_config_is_transactional_and_uses_bounded_pi_defaults(
    ) -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let failed_temp = home.path().join("failed-transaction");
        let mut failed_transaction = InstallTransaction::new(failed_temp.join("rollback"))?;
        let failed_result: anyhow::Result<()> = (|| {
            let config = ensure_adapter_harness_config(home.path(), &mut failed_transaction, true)?;
            assert_eq!(config.default_harness.as_deref(), Some("pi"));
            assert_eq!(config.selected_harnesses, ["pi"]);
            assert_eq!(config.installed.len(), 1);
            assert_eq!(config.installed[0].harness, "pi");
            assert_eq!(
                config.installed[0].prompt_paths,
                [home.path().join(".ldgr/prompts")]
            );
            assert_eq!(
                config.installed[0].skill_paths,
                [home.path().join(".pi/agent/skills")]
            );
            assert_eq!(
                config.installed[0].extension_paths,
                [home.path().join(".pi/agent/extensions")]
            );
            anyhow::bail!("injected post-config installation failure")
        })();
        assert!(
            finish_ephemeral_installation(failed_transaction, &failed_temp, failed_result).is_err()
        );
        assert!(!failed_temp.exists());
        assert!(!home.path().join(".ldgr/config.toml").exists());
        assert!(!home.path().join(".ldgr/config.json").exists());

        let successful_temp = home.path().join("successful-transaction");
        let mut successful_transaction = InstallTransaction::new(successful_temp.join("rollback"))?;
        ensure_adapter_harness_config(home.path(), &mut successful_transaction, true)?;
        finish_ephemeral_installation(successful_transaction, &successful_temp, Ok(()))?;
        assert!(!successful_temp.exists());
        let toml_config = crate::harness_config::parse_harness_config_toml(&fs::read_to_string(
            home.path().join(".ldgr/config.toml"),
        )?)?;
        let json_config = crate::harness_config::parse_harness_config_json(&fs::read_to_string(
            home.path().join(".ldgr/config.json"),
        )?)?;
        assert_eq!(toml_config.selected_harnesses, ["pi"]);
        assert_eq!(
            json_config.selected_harnesses,
            toml_config.selected_harnesses
        );
        Ok(())
    }

    #[test]
    fn standalone_adapter_config_never_replaces_invalid_existing_config() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let config_path = home.path().join(".ldgr/config.json");
        fs::create_dir_all(config_path.parent().expect("config has parent"))?;
        fs::write(&config_path, "{invalid")?;
        let temp = home.path().join("transaction");
        let mut transaction = InstallTransaction::new(temp.join("rollback"))?;
        let error = ensure_adapter_harness_config(home.path(), &mut transaction, true)
            .expect_err("invalid config must block installation");
        assert!(error.to_string().contains("unreadable or invalid"));
        finish_ephemeral_installation::<()>(transaction, &temp, Err(error))
            .expect_err("invalid config remains an installation error");
        assert_eq!(fs::read_to_string(config_path)?, "{invalid");
        assert!(!home.path().join(".ldgr/config.toml").exists());
        assert!(!temp.exists());
        Ok(())
    }

    #[test]
    fn source_root_install_patches_adapter_argv_to_cargo_runner() -> anyhow::Result<()> {
        let install_root = tempfile::tempdir()?;
        let source_root = tempfile::tempdir()?;
        let cargo_manifest = source_root.path().join("Cargo.toml");
        std::fs::write(&cargo_manifest, "[workspace]\n")?;
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

        patch_adapter_argv_to_source_runner(install_root.path(), "ldgr-conduct", &cargo_manifest)?;
        let manifest = std::fs::read_to_string(install_root.path().join("adapter.toml"))?;
        assert!(manifest.contains("argv = [\"cargo\", \"run\", \"--quiet\", \"--manifest-path\""));
        assert!(manifest.contains("\"--target-dir\""));
        assert!(manifest.contains("\"-p\", \"ldgr-conduct\", \"--\"]"));
        assert!(manifest.contains("\"--\", \"status\"]"));
        let parsed: toml::Value =
            toml::from_str(&manifest).expect("patched manifest should parse as TOML");
        let commands = parsed["commands"]
            .as_array()
            .expect("commands should be an array");
        let argv = commands[0]["argv"]
            .as_array()
            .expect("argv should be an array")
            .iter()
            .map(|value| value.as_str().expect("argv entries should be strings"))
            .collect::<Vec<_>>();
        assert_eq!(argv[4], cargo_manifest.to_string_lossy());
        assert_eq!(
            argv[6],
            install_root.path().join("source-target").to_string_lossy()
        );
        Ok(())
    }

    #[test]
    fn source_install_receipt_tracks_identity_files_invocations_and_boundaries(
    ) -> anyhow::Result<()> {
        let source_root = tempfile::tempdir()?;
        let install_root = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        let source_manifest = r#"[adapter]
slug = "example"

[[commands]]
namespace = "example"
argv = ["ldgr-example-adapter"]

[[tools]]
name = "example-summary"
argv = ["ldgr-example-adapter", "manifest-summary"]
"#;
        std::fs::write(source_root.path().join("adapter.toml"), source_manifest)?;
        std::fs::write(
            source_root.path().join("Cargo.toml"),
            "[package]\nname = \"ldgr-example-adapter\"\nversion = \"0.0.0\"\n",
        )?;
        std::fs::write(
            source_root.path().join("adapter-resources.json"),
            "{\"schema_version\":1,\"resources\":[]}",
        )?;
        std::fs::create_dir_all(install_root.path().join("prompts"))?;
        std::fs::write(install_root.path().join("adapter.toml"), source_manifest)?;
        std::fs::write(
            install_root.path().join("adapter-resources.json"),
            "{\"schema_version\":1,\"resources\":[]}",
        )?;
        std::fs::write(
            install_root.path().join("prompts/example.md"),
            "tracked prompt",
        )?;
        let source = AdapterSourcePackage {
            bundle_root: source_root.path().canonicalize()?,
            cargo_manifest: source_root.path().canonicalize()?.join("Cargo.toml"),
        };
        patch_adapter_argv_to_source_runner(
            install_root.path(),
            "ldgr-example-adapter",
            &source.cargo_manifest,
        )?;
        let plan = source_harness_resource_plan(install_root.path(), home.path())?;
        install_source_harness_resources(&plan, false)?;
        let marker = home.path().join(".ldgr/installed-adapters/example");
        write_file(
            &marker,
            &format!(
                "install_root={}\ninstall_kind=local_source\n",
                install_root.path().display()
            ),
        )?;
        write_source_installation_receipt(
            "example",
            "ldgr-example-adapter",
            &source,
            install_root.path(),
            &marker,
            &plan,
        )?;

        let value: serde_json::Value = serde_json::from_str(&std::fs::read_to_string(
            install_root.path().join("installation-receipt.json"),
        )?)?;
        let AdapterInstallationReceipt::Source(receipt) =
            parse_adapter_installation_receipt(value)?
        else {
            panic!("expected a local source receipt");
        };
        assert_eq!(receipt.install_kind, "local_source");
        assert!(!receipt.verified_release);
        assert!(!receipt.ownership.source_checkout_owned);
        assert_eq!(receipt.ownership.generated_paths, ["source-target"]);
        assert_eq!(receipt.source.package, "ldgr-example-adapter");
        assert_eq!(receipt.executable_invocations.len(), 2);
        assert_eq!(receipt.executable_invocations[0].kind, "namespace");
        assert_eq!(receipt.executable_invocations[0].name, "example");
        assert_eq!(receipt.executable_invocations[0].argv[0], "cargo");
        assert_eq!(receipt.executable_invocations[1].kind, "tool");
        assert_eq!(receipt.executable_invocations[1].name, "example-summary");
        assert!(receipt
            .manifest_digests
            .source_resource_manifest_sha256
            .is_some());
        assert!(receipt
            .manifest_digests
            .installed_resource_manifest_sha256
            .is_some());
        assert!(receipt
            .installed_files
            .iter()
            .any(|file| file.path == "adapter.toml"));
        assert!(receipt
            .installed_files
            .iter()
            .any(|file| file.path == "prompts/example.md"));
        assert_eq!(receipt.owned_resources.len(), 1);
        assert!(source_receipt_drift(install_root.path(), home.path(), &receipt)?.is_empty());

        std::fs::create_dir_all(install_root.path().join("source-target/debug"))?;
        std::fs::write(
            install_root.path().join("source-target/debug/cache"),
            "generated",
        )?;
        assert!(source_receipt_drift(install_root.path(), home.path(), &receipt)?.is_empty());

        std::fs::write(
            install_root.path().join("prompts/example.md"),
            "locally modified",
        )?;
        let drift = source_receipt_drift(install_root.path(), home.path(), &receipt)?;
        assert!(drift
            .iter()
            .any(|path| path.ends_with("prompts/example.md")));
        Ok(())
    }

    #[test]
    fn source_receipt_never_authorizes_external_removal_outside_harness_roots() -> anyhow::Result<()>
    {
        let install_root = tempfile::tempdir()?;
        let source_root = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        std::fs::write(install_root.path().join("adapter.toml"), "[adapter]\n")?;
        let outside = tempfile::NamedTempFile::new()?;
        let receipt = crate::release_index::SourceInstallationReceipt {
            schema_version: SOURCE_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION,
            install_kind: "local_source".to_owned(),
            domain: "example".to_owned(),
            installed_at_unix_seconds: 0,
            source: crate::release_index::SourceInstallIdentity {
                package: "ldgr-example-adapter".to_owned(),
                bundle_root: source_root.path().display().to_string(),
                cargo_manifest: source_root.path().join("Cargo.toml").display().to_string(),
                bundle_sha256: "digest".to_owned(),
            },
            manifest_digests: crate::release_index::SourceManifestDigests {
                source_adapter_manifest_sha256: "digest".to_owned(),
                source_cargo_manifest_sha256: "digest".to_owned(),
                installed_adapter_manifest_sha256: digest_path(
                    &install_root.path().join("adapter.toml"),
                )?,
                source_resource_manifest_sha256: None,
                installed_resource_manifest_sha256: None,
            },
            installer_invocation: vec!["cargo".to_owned()],
            executable_invocations: Vec::new(),
            installed_files: vec![crate::release_index::OwnedResource {
                path: "adapter.toml".to_owned(),
                sha256: digest_path(&install_root.path().join("adapter.toml"))?,
            }],
            owned_resources: vec![crate::release_index::OwnedResource {
                path: outside.path().display().to_string(),
                sha256: digest_path(outside.path())?,
            }],
            ownership: crate::release_index::SourceOwnershipBoundaries {
                install_root: install_root.path().canonicalize()?.display().to_string(),
                marker_path: home
                    .path()
                    .join(".ldgr/installed-adapters/example")
                    .display()
                    .to_string(),
                source_checkout_owned: false,
                generated_paths: vec!["source-target".to_owned()],
                external_resource_roots: vec![outside
                    .path()
                    .parent()
                    .expect("temporary file parent")
                    .display()
                    .to_string()],
            },
            verified_release: false,
        };

        let error = verify_source_receipt_boundaries(install_root.path(), home.path(), &receipt)
            .expect_err("outside resource must be rejected");
        assert!(error
            .to_string()
            .contains("not a currently configured harness boundary"));
        assert!(outside.path().is_file());
        Ok(())
    }

    #[test]
    fn source_identity_digest_ignores_build_caches_but_tracks_source_changes() -> anyhow::Result<()>
    {
        let source = tempfile::tempdir()?;
        std::fs::write(source.path().join("Cargo.toml"), "[package]\nname='demo'\n")?;
        std::fs::create_dir_all(source.path().join("src"))?;
        std::fs::write(source.path().join("src/main.rs"), "fn main() {}\n")?;
        let original = digest_source_bundle(source.path())?;

        std::fs::create_dir_all(source.path().join("target/debug"))?;
        std::fs::write(source.path().join("target/debug/cache"), "generated")?;
        assert_eq!(digest_source_bundle(source.path())?, original);

        std::fs::write(
            source.path().join("src/main.rs"),
            "fn main() { println!(); }\n",
        )?;
        assert_ne!(digest_source_bundle(source.path())?, original);
        Ok(())
    }

    #[test]
    fn source_update_rejects_content_changed_after_planning_before_mutation() -> anyhow::Result<()>
    {
        let source = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        let install_root = home.path().join("installed-fixture");
        std::fs::write(
            source.path().join("Cargo.toml"),
            "[package]\nname='ldgr-fixture-adapter'\nversion='0.0.0'\n",
        )?;
        std::fs::write(
            source.path().join("adapter.toml"),
            "[adapter]\nslug='fixture'\n",
        )?;
        let mut transaction = InstallTransaction::new(home.path().join("rollback"))?;

        let error = apply_source_adapter_update(
            "fixture",
            "ldgr-fixture-adapter",
            source.path(),
            &install_root,
            home.path(),
            Some("stale-planned-digest"),
            &mut transaction,
            false,
        )
        .expect_err("source changed after planning must fail closed");

        assert!(error
            .to_string()
            .contains("local source changed after adapter update planning"));
        assert!(!install_root.exists());
        Ok(())
    }

    #[test]
    fn source_root_resolves_nested_standalone_adapter_workspace() -> anyhow::Result<()> {
        let checkout = tempfile::tempdir()?;
        let adapter_root = checkout.path().join("ldgr-example-adapter");
        std::fs::create_dir(&adapter_root)?;
        std::fs::write(
            adapter_root.join("adapter.toml"),
            "[adapter]\nslug = \"example\"\n",
        )?;
        std::fs::write(
            adapter_root.join("Cargo.toml"),
            "[package]\nname = \"ldgr-example-adapter\"\nversion = \"0.0.0\"\n\n[workspace]\n",
        )?;

        let resolved = resolve_adapter_source_package("ldgr-example-adapter", checkout.path())?;

        assert_eq!(resolved.bundle_root, adapter_root.canonicalize()?);
        assert_eq!(
            resolved.cargo_manifest,
            adapter_root.canonicalize()?.join("Cargo.toml")
        );
        let install_root = checkout.path().join("installed");
        let command =
            source_adapter_install_command("ldgr-example-adapter", &resolved, &install_root);
        let args = command.get_args().collect::<Vec<_>>();
        assert_eq!(
            args,
            [
                "run".as_ref(),
                "--manifest-path".as_ref(),
                resolved.cargo_manifest.as_os_str(),
                "-p".as_ref(),
                "ldgr-example-adapter".as_ref(),
                "--".as_ref(),
                "adapter".as_ref(),
                "install".as_ref(),
                "--install-root".as_ref(),
                install_root.as_os_str(),
                "--print-path".as_ref(),
            ]
        );
        assert_eq!(
            command.get_current_dir(),
            Some(resolved.bundle_root.as_path())
        );
        Ok(())
    }

    #[test]
    fn source_root_accepts_standalone_adapter_checkout() -> anyhow::Result<()> {
        let checkout = tempfile::tempdir()?;
        std::fs::write(
            checkout.path().join("adapter.toml"),
            "[adapter]\nslug = \"example\"\n",
        )?;
        std::fs::write(
            checkout.path().join("Cargo.toml"),
            "[package]\nname = \"ldgr-example-adapter\"\nversion = \"0.0.0\"\n\n[workspace]\n",
        )?;

        let resolved = resolve_adapter_source_package("ldgr-example-adapter", checkout.path())?;

        assert_eq!(resolved.bundle_root, checkout.path().canonicalize()?);
        assert_eq!(
            resolved.cargo_manifest,
            checkout.path().canonicalize()?.join("Cargo.toml")
        );
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

    #[test]
    fn adapter_bundle_contract_preflight_is_exact_and_read_only() -> anyhow::Result<()> {
        let bundle = tempfile::tempdir()?;
        std::fs::write(
            bundle.path().join("adapter.toml"),
            "[adapter]\nslug = \"example\"\ncore_version = \"generated\"\n",
        )?;
        let missing = validate_adapter_bundle_contract(bundle.path(), "example").unwrap_err();
        assert!(format!("{missing:#}").contains("missing generated database contract"));

        let valid = crate::database_contract::generated_adapter_contract_json("example")?;
        std::fs::write(bundle.path().join("adapter-database-contract.json"), &valid)?;
        validate_adapter_bundle_contract(bundle.path(), "example")?;

        let mut tampered: serde_json::Value = serde_json::from_str(&valid)?;
        tampered["contract_hash"] = "sha256:tampered".into();
        std::fs::write(
            bundle.path().join("adapter-database-contract.json"),
            serde_json::to_vec(&tampered)?,
        )?;
        let error = validate_adapter_bundle_contract(bundle.path(), "example").unwrap_err();
        assert!(format!("{error:#}").contains("incompatible with this Core release"));
        Ok(())
    }
}
