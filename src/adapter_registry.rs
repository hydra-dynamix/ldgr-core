use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::adapter_compatibility::{
    core_compatibility_inventory, evaluate_legacy_v1, evaluate_v2,
    legacy_core_compatibility_inventory, parse_adapter_compatibility,
    CentralComponentDatabaseStateV2, CompatibilityReason, CompatibilityReasonCode,
    CoreCompatibilityProfileV2, LegacyCoreProfileV1, ParsedAdapterCompatibility,
    ADAPTER_COMPATIBILITY_FORMAT_V2,
};
use crate::database_contract::ADAPTER_DATABASE_CONTRACT_FORMAT;
use crate::manifest_integrity::verify_manifest_digest;

pub const ADAPTER_MANIFEST_FILE: &str = "adapter.toml";
const ADAPTER_COMPATIBILITY_FILE: &str = "adapter-compatibility.json";
const LEGACY_ADAPTER_CONTRACT_FILE: &str = "adapter-database-contract.json";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AdapterOperationalState {
    Ready,
    Degraded,
    Blocked,
    Invalid,
}

impl AdapterOperationalState {
    pub fn permits_dispatch(self) -> bool {
        matches!(self, Self::Ready | Self::Degraded)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Degraded => "degraded",
            Self::Blocked => "blocked",
            Self::Invalid => "invalid",
        }
    }
}

impl std::fmt::Display for AdapterOperationalState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.as_str())
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdapterRepair {
    pub available: bool,
    pub argv: Vec<String>,
    pub command: String,
}

#[derive(Debug, Default, Serialize)]
pub struct AdapterRegistry {
    pub adapters: Vec<DiscoveredAdapter>,
    pub warnings: Vec<AdapterWarning>,
}

impl AdapterRegistry {
    pub fn discover() -> Self {
        Self::discover_from_roots(adapter_search_roots())
    }

    pub fn discover_for_database(db_path: &Path) -> Self {
        let mut core = core_compatibility_inventory();
        if db_path.is_file() {
            core.core_schema_version = active_database_schema(db_path).unwrap_or(0);
        }
        Self::discover_from_roots_with_profile(adapter_search_roots(), &core, &[])
    }

    pub fn discover_from_roots<I>(roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        Self::discover_from_roots_with_profile(roots, &core_compatibility_inventory(), &[])
    }

    pub fn discover_from_roots_with_profile<I>(
        roots: I,
        core: &CoreCompatibilityProfileV2,
        database_components: &[CentralComponentDatabaseStateV2],
    ) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut legacy_core = legacy_core_compatibility_inventory();
        legacy_core.core_schema_version = i64::from(core.core_schema_version);
        Self::discover_from_roots_with_profiles(roots, core, database_components, &legacy_core)
    }

    pub fn discover_from_roots_with_profiles<I>(
        roots: I,
        core: &CoreCompatibilityProfileV2,
        database_components: &[CentralComponentDatabaseStateV2],
        legacy_core: &LegacyCoreProfileV1,
    ) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut registry = Self::default();
        let mut claimed_names = BTreeMap::<String, String>::new();
        let mut seen_manifests = BTreeSet::<PathBuf>::new();

        for root in roots {
            for manifest_path in manifest_paths(&root, &mut registry.warnings) {
                let identity = manifest_path
                    .canonicalize()
                    .unwrap_or_else(|_| manifest_path.clone());
                if !seen_manifests.insert(identity) {
                    continue;
                }
                let mut adapter = match load_adapter_manifest(&manifest_path) {
                    Ok(adapter) => adapter,
                    Err(error) => {
                        let adapter = invalid_adapter_entry(&manifest_path, &error);
                        registry.warnings.push(AdapterWarning::new(
                            manifest_path,
                            format!("{error:#}; retained as invalid"),
                        ));
                        registry.adapters.push(adapter);
                        continue;
                    }
                };
                evaluate_installed_compatibility(
                    &mut adapter,
                    core,
                    database_components,
                    legacy_core,
                );

                if let Some(owner) = claimed_names.get(&adapter.slug) {
                    let message = format!(
                        "duplicate adapter slug `{}` already provided by {}; dispatch is ambiguous",
                        adapter.slug, owner
                    );
                    let reason = registry_reason(
                        CompatibilityReasonCode::InvalidMetadata,
                        "adapter.slug",
                        Value::String(adapter.slug.clone()),
                        Value::String(owner.clone()),
                        message.clone(),
                    );
                    if let Some(existing) = registry
                        .adapters
                        .iter_mut()
                        .find(|existing| existing.slug == adapter.slug)
                    {
                        existing.state = AdapterOperationalState::Invalid;
                        existing.reasons = vec![reason.clone()];
                        existing.repair = install_repair(&existing.slug);
                    }
                    adapter.state = AdapterOperationalState::Invalid;
                    adapter.reasons = vec![reason];
                    adapter.repair = install_repair(&adapter.slug);
                    registry
                        .warnings
                        .push(AdapterWarning::new(manifest_path, message));
                    registry.adapters.push(adapter);
                    continue;
                }

                let mut retained_aliases = Vec::new();
                for alias in adapter.aliases {
                    if let Some(owner) = claimed_names.get(&alias) {
                        registry.warnings.push(AdapterWarning::new(
                            adapter.manifest_path.clone(),
                            format!(
                                "adapter alias `{alias}` conflicts with {}; alias ignored",
                                owner
                            ),
                        ));
                    } else {
                        retained_aliases.push(alias);
                    }
                }
                adapter.aliases = retained_aliases;

                claimed_names.insert(adapter.slug.clone(), adapter.slug.clone());
                for alias in &adapter.aliases {
                    claimed_names.insert(alias.clone(), adapter.slug.clone());
                }
                registry.adapters.push(adapter);
            }
        }

        registry.adapters.sort_by(|left, right| {
            left.slug
                .cmp(&right.slug)
                .then_with(|| left.manifest_path.cmp(&right.manifest_path))
        });
        registry
    }

    pub fn find(&self, slug_or_alias: &str) -> Option<&DiscoveredAdapter> {
        self.adapters.iter().find(|adapter| {
            adapter.slug == slug_or_alias
                || adapter.aliases.iter().any(|alias| alias == slug_or_alias)
        })
    }

    pub fn resolve_command(&self, command: &str) -> Vec<&AdapterCommand> {
        let mut commands = self
            .adapters
            .iter()
            .filter(|adapter| adapter.state.permits_dispatch())
            .flat_map(|adapter| {
                adapter
                    .commands
                    .iter()
                    .filter(move |tool| tool.name == command)
            })
            .collect::<Vec<_>>();
        commands.sort_by(|left, right| {
            left.adapter_slug
                .cmp(&right.adapter_slug)
                .then_with(|| left.name.cmp(&right.name))
        });
        commands
    }

    pub fn resolve_namespace(&self, namespace: &str) -> Option<&AdapterCommandNamespace> {
        self.adapters
            .iter()
            .filter(|adapter| adapter.state.permits_dispatch())
            .find_map(|adapter| {
                adapter.command_namespaces.iter().find(|command| {
                    command.namespace == namespace
                        || command.aliases.iter().any(|alias| alias == namespace)
                })
            })
    }

    pub fn find_by_namespace(&self, namespace: &str) -> Option<&DiscoveredAdapter> {
        self.adapters.iter().find(|adapter| {
            adapter.command_namespaces.iter().any(|command| {
                command.namespace == namespace
                    || command.aliases.iter().any(|alias| alias == namespace)
            })
        })
    }

    pub fn installed_domains(&self) -> Vec<InstalledAdapterDomain> {
        self.adapters
            .iter()
            .filter(|adapter| adapter.state.permits_dispatch())
            .flat_map(|adapter| {
                adapter.command_namespaces.iter().map(|namespace| {
                    let command = format!("ldgr {}", namespace.namespace);
                    InstalledAdapterDomain {
                        adapter: adapter.slug.clone(),
                        namespace: namespace.namespace.clone(),
                        command: command.clone(),
                        help_command: format!("{command} --help"),
                        instruction: format!(
                            "Run {command} --help for the extended command surface."
                        ),
                        status_command: (!namespace.status_args.is_empty())
                            .then(|| format!("{command} {}", namespace.status_args.join(" "))),
                        summary: namespace
                            .summary
                            .clone()
                            .or_else(|| namespace.description.clone()),
                    }
                })
            })
            .collect()
    }
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct InstalledAdapterDomain {
    pub adapter: String,
    pub namespace: String,
    pub command: String,
    pub help_command: String,
    pub instruction: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub status_command: Option<String>,
    pub summary: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct DiscoveredAdapter {
    pub slug: String,
    pub title: String,
    pub state: AdapterOperationalState,
    pub format: Option<String>,
    pub reasons: Vec<CompatibilityReason>,
    pub repair: AdapterRepair,
    pub core_version: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_contract_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_schema_version: Option<i64>,
    pub aliases: Vec<String>,
    pub manifest_path: PathBuf,
    pub root_path: PathBuf,
    pub profile: AdapterProfile,
    pub commands: Vec<AdapterCommand>,
    pub command_namespaces: Vec<AdapterCommandNamespace>,
    pub target_profiles: Vec<AdapterTargetProfile>,
    pub verified_manifest_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub installation_receipt: Option<serde_json::Value>,
    #[serde(skip)]
    installation_receipt_error: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterCommand {
    pub adapter_slug: String,
    pub name: String,
    pub argv: Vec<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AdapterCommandNamespace {
    pub adapter_slug: String,
    pub namespace: String,
    pub argv: Vec<String>,
    pub aliases: Vec<String>,
    pub title: Option<String>,
    pub description: Option<String>,
    pub usage: Option<String>,
    pub summary: Option<String>,
    pub details: Option<String>,
    pub status_args: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub database_contract_hash: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub core_schema_version: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub component_schema_version: Option<i64>,
    #[serde(skip)]
    pub adapter_contract_json: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterProfile {
    pub loop_prompt_path: String,
    pub default_milestone_template: String,
    pub spec_artifact_path: String,
    pub readiness_policy: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterTargetProfile {
    pub slug: String,
    pub title: String,
    pub target_type: String,
    pub description: String,
    #[serde(default)]
    pub probes: Vec<AdapterProbeFamily>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterProbeFamily {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub evidence_artifact_kind: Option<String>,
    pub expectation_template: Option<String>,
    pub validation_hint: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct AdapterWarning {
    pub manifest_path: PathBuf,
    pub message: String,
}

impl AdapterWarning {
    fn new(manifest_path: PathBuf, message: String) -> Self {
        Self {
            manifest_path,
            message,
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct AdapterManifest {
    adapter: ManifestAdapter,
    profile: AdapterProfile,
    #[serde(default)]
    tools: Vec<ManifestTool>,
    #[serde(default)]
    commands: Vec<ManifestCommand>,
    #[serde(default)]
    target_profiles: Vec<AdapterTargetProfile>,
    #[serde(default)]
    integrity: Option<ManifestIntegrity>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestAdapter {
    slug: String,
    title: String,
    core_version: String,
    #[serde(default)]
    aliases: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestTool {
    name: String,
    argv: Vec<String>,
    description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
#[allow(dead_code)]
struct ManifestCommand {
    namespace: String,
    argv: Vec<String>,
    #[serde(default)]
    aliases: Vec<String>,
    title: Option<String>,
    description: Option<String>,
    #[serde(default)]
    capabilities: Vec<String>,
    help: Option<ManifestCommandHelp>,
    #[serde(default)]
    status_args: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCommandHelp {
    usage: Option<String>,
    summary: Option<String>,
    details: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestIntegrity {
    manifest_digest: Option<String>,
}

pub fn adapter_search_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(paths) = env::var_os("LDGR_ADAPTER_PATH") {
        roots.extend(env::split_paths(&paths));
    }
    roots.push(PathBuf::from(".ldgr/adapters"));
    if let Some(home) = env::var_os("LDGR_HOME") {
        roots.push(PathBuf::from(home).join("adapters"));
    }
    if let Some(home) = env::var_os("HOME") {
        roots.push(PathBuf::from(home).join(".ldgr/adapters"));
    }
    roots
}

fn manifest_paths(root: &Path, warnings: &mut Vec<AdapterWarning>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let direct = root.join(ADAPTER_MANIFEST_FILE);
    if adapter_candidate_root(root) {
        paths.push(direct);
    }

    let entries = match fs::read_dir(root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return paths,
        Err(error) => {
            warnings.push(AdapterWarning::new(
                root.join(ADAPTER_MANIFEST_FILE),
                format!("failed to read adapter root {}: {error}", root.display()),
            ));
            return paths;
        }
    };

    let mut children = Vec::new();
    for entry in entries {
        match entry {
            Ok(entry) => children.push(entry.path()),
            Err(error) => warnings.push(AdapterWarning::new(
                root.join(ADAPTER_MANIFEST_FILE),
                format!(
                    "failed to read adapter root entry {}: {error}",
                    root.display()
                ),
            )),
        }
    }
    children.sort();
    for child in children {
        if adapter_candidate_root(&child) {
            paths.push(child.join(ADAPTER_MANIFEST_FILE));
        }
    }
    paths.sort();
    paths.dedup();
    paths
}

fn adapter_candidate_root(path: &Path) -> bool {
    path.is_dir()
        && [
            ADAPTER_MANIFEST_FILE,
            ADAPTER_COMPATIBILITY_FILE,
            LEGACY_ADAPTER_CONTRACT_FILE,
            "installation-receipt.json",
        ]
        .iter()
        .any(|name| path.join(name).is_file())
}

fn load_adapter_manifest(manifest_path: &Path) -> anyhow::Result<DiscoveredAdapter> {
    let manifest_text = fs::read_to_string(manifest_path).with_context(|| {
        format!(
            "failed to read adapter manifest {}",
            manifest_path.display()
        )
    })?;
    let manifest: AdapterManifest = toml::from_str(&manifest_text).with_context(|| {
        format!(
            "failed to parse adapter manifest {}",
            manifest_path.display()
        )
    })?;
    let verified_manifest_digest = verify_manifest_digest(&manifest_text).with_context(|| {
        format!(
            "failed to verify adapter manifest integrity {}",
            manifest_path.display()
        )
    })?;

    let manifest_dir = manifest_path
        .parent()
        .context("adapter manifest path has no parent directory")?;
    validate_manifest(manifest_dir, &manifest)?;

    let adapter_slug = clean_identifier("adapter.slug", &manifest.adapter.slug)?;
    let mut aliases = Vec::new();
    let mut seen_aliases = BTreeSet::new();
    for alias in manifest.adapter.aliases {
        let alias = clean_identifier("adapter.aliases", &alias)?;
        if alias == adapter_slug {
            continue;
        }
        if seen_aliases.insert(alias.clone()) {
            aliases.push(alias);
        }
    }

    let mut commands = Vec::new();
    let mut seen_commands = BTreeSet::new();
    for tool in manifest.tools {
        let name = clean_identifier("tools.name", &tool.name)?;
        if !seen_commands.insert(name.clone()) {
            bail!("duplicate adapter command `{name}`");
        }
        commands.push(AdapterCommand {
            adapter_slug: adapter_slug.clone(),
            name,
            argv: tool.argv,
            description: tool.description,
        });
    }

    let mut command_namespaces = Vec::new();
    let mut seen_namespaces = BTreeSet::new();
    for command in manifest.commands {
        let namespace = clean_identifier("commands.namespace", &command.namespace)?;
        if !seen_namespaces.insert(namespace.clone()) {
            bail!("duplicate adapter namespace `{namespace}`");
        }
        let mut aliases = Vec::new();
        let mut seen_aliases = BTreeSet::new();
        for alias in command.aliases {
            let alias = clean_identifier("commands.aliases", &alias)?;
            if alias != namespace && seen_aliases.insert(alias.clone()) {
                aliases.push(alias);
            }
        }
        let help = command.help;
        command_namespaces.push(AdapterCommandNamespace {
            adapter_slug: adapter_slug.clone(),
            namespace,
            argv: command.argv,
            aliases,
            title: command.title,
            description: command.description,
            usage: help.as_ref().and_then(|help| help.usage.clone()),
            summary: help.as_ref().and_then(|help| help.summary.clone()),
            details: help.and_then(|help| help.details),
            status_args: command.status_args,
            database_contract_hash: None,
            core_schema_version: None,
            component_schema_version: None,
            adapter_contract_json: None,
        });
    }
    if command_namespaces.is_empty() {
        command_namespaces.push(AdapterCommandNamespace {
            adapter_slug: adapter_slug.clone(),
            namespace: adapter_slug.clone(),
            argv: vec![format!("ldgr-{adapter_slug}")],
            aliases: aliases.clone(),
            title: None,
            description: Some("Adapter command namespace inferred from adapter slug.".to_owned()),
            usage: Some(format!("ldgr {adapter_slug} <command> [options]")),
            summary: Some(format!("Run {adapter_slug} adapter commands.")),
            details: None,
            status_args: Vec::new(),
            database_contract_hash: None,
            core_schema_version: None,
            component_schema_version: None,
            adapter_contract_json: None,
        });
    }

    let receipt_path = manifest_dir.join("installation-receipt.json");
    let (installation_receipt, installation_receipt_error) = match fs::read_to_string(&receipt_path)
    {
        Ok(text) => match serde_json::from_str(&text) {
            Ok(value) => (Some(value), None),
            Err(error) => (
                None,
                Some(format!(
                    "failed to parse installation receipt {}: {error}",
                    receipt_path.display()
                )),
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => (None, None),
        Err(error) => (
            None,
            Some(format!(
                "failed to read installation receipt {}: {error}",
                receipt_path.display()
            )),
        ),
    };
    Ok(DiscoveredAdapter {
        slug: adapter_slug,
        title: nonempty("adapter.title", manifest.adapter.title)?,
        state: AdapterOperationalState::Invalid,
        format: None,
        reasons: Vec::new(),
        repair: unavailable_repair(),
        core_version: nonempty("adapter.core_version", manifest.adapter.core_version)?,
        database_contract_hash: None,
        component_schema_version: None,
        aliases,
        manifest_path: manifest_path
            .canonicalize()
            .unwrap_or_else(|_| manifest_path.to_path_buf()),
        root_path: manifest_dir
            .canonicalize()
            .unwrap_or_else(|_| manifest_dir.to_path_buf()),
        profile: manifest.profile,
        commands,
        command_namespaces,
        target_profiles: manifest.target_profiles,
        verified_manifest_digest,
        installation_receipt,
        installation_receipt_error,
    })
}

fn evaluate_installed_compatibility(
    adapter: &mut DiscoveredAdapter,
    core: &CoreCompatibilityProfileV2,
    database_components: &[CentralComponentDatabaseStateV2],
    legacy_core: &LegacyCoreProfileV1,
) {
    if let Some(error) = &adapter.installation_receipt_error {
        adapter.state = AdapterOperationalState::Invalid;
        adapter.reasons = vec![registry_reason(
            CompatibilityReasonCode::InvalidMetadata,
            "installation-receipt.json",
            Value::String("valid installation receipt".to_owned()),
            Value::Null,
            error.clone(),
        )];
        adapter.repair = install_repair(&adapter.slug);
        return;
    }

    let v2_text = read_optional_text(&adapter.root_path.join(ADAPTER_COMPATIBILITY_FILE));
    let legacy_text = read_optional_text(&adapter.root_path.join(LEGACY_ADAPTER_CONTRACT_FILE));
    let parsed = match (v2_text, legacy_text) {
        (Ok(v2), Ok(legacy)) => parse_adapter_compatibility(v2.as_deref(), legacy.as_deref()),
        (Err(error), _) | (_, Err(error)) => {
            adapter.state = AdapterOperationalState::Invalid;
            adapter.reasons = vec![registry_reason(
                CompatibilityReasonCode::InvalidMetadata,
                "adapter compatibility sidecar",
                Value::Null,
                Value::Null,
                error,
            )];
            adapter.repair = repair_for_invalid(adapter);
            return;
        }
    };

    match parsed {
        Ok(ParsedAdapterCompatibility::V2 { sidecar }) => {
            adapter.format = Some(ADAPTER_COMPATIBILITY_FORMAT_V2.to_owned());
            adapter.core_version =
                format!("schema-v{}+", sidecar.compatibility.minimum_core_schema);
            let mut evaluation = evaluate_v2(&sidecar, &adapter.slug, core, database_components);
            append_v2_receipt_reasons(adapter, &sidecar, &mut evaluation.reasons);
            adapter.reasons = evaluation.reasons;
            adapter.state = if adapter.reasons.is_empty() {
                AdapterOperationalState::Ready
            } else {
                AdapterOperationalState::Blocked
            };
            adapter.repair = if adapter.state == AdapterOperationalState::Ready {
                unavailable_repair()
            } else {
                update_adapter_repair(&adapter.slug)
            };
        }
        Ok(ParsedAdapterCompatibility::LegacyV1 { contract }) => {
            adapter.format = Some(ADAPTER_DATABASE_CONTRACT_FORMAT.to_owned());
            adapter.core_version = format!("schema-v{}", contract.core_schema_version);
            adapter.database_contract_hash = Some(contract.contract_hash.clone());
            adapter.component_schema_version = Some(contract.component.schema_version);
            let contract_text =
                fs::read_to_string(adapter.root_path.join(LEGACY_ADAPTER_CONTRACT_FILE)).ok();
            for namespace in &mut adapter.command_namespaces {
                namespace.database_contract_hash = Some(contract.contract_hash.clone());
                namespace.core_schema_version = Some(contract.core_schema_version);
                namespace.component_schema_version = Some(contract.component.schema_version);
                namespace.adapter_contract_json = contract_text.clone();
            }
            let mut evaluation = evaluate_legacy_v1(&contract, &adapter.slug, legacy_core);
            append_legacy_receipt_reasons(adapter, &mut evaluation.reasons);
            adapter.reasons = evaluation.reasons;
            adapter.state = if adapter.reasons.is_empty() {
                AdapterOperationalState::Degraded
            } else {
                AdapterOperationalState::Blocked
            };
            adapter.repair = update_adapter_repair(&adapter.slug);
        }
        Err(error) => {
            adapter.format = discovered_format(&adapter.root_path);
            adapter.state = AdapterOperationalState::Invalid;
            adapter.reasons = vec![error.reason];
            adapter.repair = repair_for_invalid(adapter);
        }
    }
}

fn append_v2_receipt_reasons(
    adapter: &DiscoveredAdapter,
    sidecar: &crate::adapter_compatibility::AdapterCompatibilitySidecarV2,
    reasons: &mut Vec<CompatibilityReason>,
) {
    let Some(receipt) = &adapter.installation_receipt else {
        return;
    };
    if let Some(domain) = receipt.get("domain").and_then(Value::as_str) {
        if domain != adapter.slug {
            reasons.push(registry_reason(
                CompatibilityReasonCode::AdapterIdentityMismatch,
                "receipt.domain",
                Value::String(adapter.slug.clone()),
                Value::String(domain.to_owned()),
                "installation receipt identity does not match the manifest".to_owned(),
            ));
        }
    }
    if let Some(indexed) = receipt.get("compatibility") {
        if serde_json::to_value(&sidecar.compatibility).ok().as_ref() != Some(indexed) {
            reasons.push(registry_reason(
                CompatibilityReasonCode::InvalidMetadata,
                "receipt.compatibility",
                serde_json::to_value(&sidecar.compatibility).unwrap_or(Value::Null),
                indexed.clone(),
                "installed sidecar compatibility does not match its receipt".to_owned(),
            ));
        }
    }
    if let Some(indexed) = receipt.get("compatibility_sha256").and_then(Value::as_str) {
        let actual = sidecar.compatibility_sha256().ok();
        if actual.as_deref() != Some(indexed) {
            reasons.push(registry_reason(
                CompatibilityReasonCode::InvalidMetadata,
                "receipt.compatibility_sha256",
                Value::String(indexed.to_owned()),
                actual.map(Value::String).unwrap_or(Value::Null),
                "installed sidecar fingerprint does not match its receipt".to_owned(),
            ));
        }
    }
}

fn append_legacy_receipt_reasons(
    adapter: &DiscoveredAdapter,
    reasons: &mut Vec<CompatibilityReason>,
) {
    let Some(receipt) = &adapter.installation_receipt else {
        return;
    };
    if let Some(domain) = receipt.get("domain").and_then(Value::as_str) {
        if domain != adapter.slug {
            reasons.push(registry_reason(
                CompatibilityReasonCode::AdapterIdentityMismatch,
                "receipt.domain",
                Value::String(adapter.slug.clone()),
                Value::String(domain.to_owned()),
                "installation receipt identity does not match the manifest".to_owned(),
            ));
        }
    }
    let Some(requirement) = receipt.get("core_compatibility").and_then(Value::as_str) else {
        return;
    };
    let current = Version::parse(env!("CARGO_PKG_VERSION")).expect("Core package version");
    match VersionReq::parse(requirement) {
        Ok(requirement) if requirement.matches(&current) => {}
        Ok(_) => reasons.push(registry_reason(
            CompatibilityReasonCode::InvalidMetadata,
            "receipt.core_compatibility",
            Value::String(requirement.to_owned()),
            Value::String(current.to_string()),
            "legacy release receipt does not allow the active Core package version".to_owned(),
        )),
        Err(error) => reasons.push(registry_reason(
            CompatibilityReasonCode::InvalidMetadata,
            "receipt.core_compatibility",
            Value::String("valid SemVer requirement".to_owned()),
            Value::String(requirement.to_owned()),
            format!("legacy release receipt has an invalid Core range: {error}"),
        )),
    }
}

fn read_optional_text(path: &Path) -> Result<Option<String>, String> {
    match fs::read_to_string(path) {
        Ok(text) => Ok(Some(text)),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!("failed to read {}: {error}", path.display())),
    }
}

fn discovered_format(root: &Path) -> Option<String> {
    [ADAPTER_COMPATIBILITY_FILE, LEGACY_ADAPTER_CONTRACT_FILE]
        .iter()
        .find_map(|name| {
            fs::read_to_string(root.join(name))
                .ok()
                .and_then(|text| serde_json::from_str::<Value>(&text).ok())
                .and_then(|value| {
                    value
                        .get("format")
                        .and_then(Value::as_str)
                        .map(str::to_owned)
                })
        })
}

fn active_database_schema(path: &Path) -> Option<i32> {
    let connection = rusqlite::Connection::open_with_flags(
        path,
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    crate::store::current_schema_version(&connection)
        .ok()
        .and_then(|version| i32::try_from(version).ok())
}

fn invalid_adapter_entry(manifest_path: &Path, error: &anyhow::Error) -> DiscoveredAdapter {
    let root = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let slug = candidate_identity(root);
    let format = discovered_format(root);
    DiscoveredAdapter {
        slug: slug.clone(),
        title: format!("Invalid adapter {slug}"),
        state: AdapterOperationalState::Invalid,
        format,
        reasons: vec![registry_reason(
            CompatibilityReasonCode::InvalidMetadata,
            "adapter.toml",
            Value::String("valid adapter manifest".to_owned()),
            Value::Null,
            format!("{error:#}"),
        )],
        repair: install_repair(&slug),
        core_version: "unknown".to_owned(),
        database_contract_hash: None,
        component_schema_version: None,
        aliases: Vec::new(),
        manifest_path: manifest_path.to_path_buf(),
        root_path: root.to_path_buf(),
        profile: AdapterProfile {
            loop_prompt_path: String::new(),
            default_milestone_template: String::new(),
            spec_artifact_path: String::new(),
            readiness_policy: String::new(),
        },
        commands: Vec::new(),
        command_namespaces: Vec::new(),
        target_profiles: Vec::new(),
        verified_manifest_digest: None,
        installation_receipt: fs::read_to_string(root.join("installation-receipt.json"))
            .ok()
            .and_then(|text| serde_json::from_str(&text).ok()),
        installation_receipt_error: None,
    }
}

fn candidate_identity(root: &Path) -> String {
    let receipt = fs::read_to_string(root.join("installation-receipt.json"))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get("domain")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let sidecar = fs::read_to_string(root.join(ADAPTER_COMPATIBILITY_FILE))
        .ok()
        .and_then(|text| serde_json::from_str::<Value>(&text).ok())
        .and_then(|value| {
            value
                .get("adapter")
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
    let manifest = fs::read_to_string(root.join(ADAPTER_MANIFEST_FILE))
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|value| {
            value
                .get("adapter")
                .and_then(|value| value.get("slug"))
                .and_then(toml::Value::as_str)
                .map(str::to_owned)
        });
    receipt
        .or(sidecar)
        .or(manifest)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| {
            root.file_name()
                .and_then(|value| value.to_str())
                .unwrap_or("unknown-adapter")
                .trim_start_matches("ldgr-")
                .to_owned()
        })
}

fn registry_reason(
    code: CompatibilityReasonCode,
    subject: impl Into<String>,
    required: Value,
    actual: Value,
    message: String,
) -> CompatibilityReason {
    CompatibilityReason {
        code,
        subject: subject.into(),
        required,
        actual,
        message,
    }
}

fn unavailable_repair() -> AdapterRepair {
    AdapterRepair {
        available: false,
        argv: Vec::new(),
        command: String::new(),
    }
}

fn install_repair(adapter: &str) -> AdapterRepair {
    repair(["ldgr", "adapter", "install", adapter])
}

fn update_adapter_repair(adapter: &str) -> AdapterRepair {
    repair(["ldgr", "update", "--adapter", adapter])
}

fn repair_for_invalid(adapter: &DiscoveredAdapter) -> AdapterRepair {
    if adapter.installation_receipt.is_some() {
        update_adapter_repair(&adapter.slug)
    } else {
        install_repair(&adapter.slug)
    }
}

fn repair<const N: usize>(argv: [&str; N]) -> AdapterRepair {
    let argv = argv
        .iter()
        .map(|value| (*value).to_owned())
        .collect::<Vec<_>>();
    AdapterRepair {
        available: true,
        command: argv.join(" "),
        argv,
    }
}

fn validate_manifest(manifest_dir: &Path, manifest: &AdapterManifest) -> anyhow::Result<()> {
    let _ = manifest
        .integrity
        .as_ref()
        .and_then(|integrity| integrity.manifest_digest.as_ref());
    validate_referenced_file(
        manifest_dir,
        "profile.loop_prompt_path",
        &manifest.profile.loop_prompt_path,
    )?;
    validate_referenced_file(
        manifest_dir,
        "profile.default_milestone_template",
        &manifest.profile.default_milestone_template,
    )?;
    validate_referenced_file(
        manifest_dir,
        "profile.spec_artifact_path",
        &manifest.profile.spec_artifact_path,
    )?;
    nonempty(
        "profile.readiness_policy",
        manifest.profile.readiness_policy.clone(),
    )?;
    for tool in &manifest.tools {
        clean_identifier("tools.name", &tool.name)?;
        if tool.argv.is_empty() {
            bail!("adapter command `{}` has empty argv", tool.name);
        }
        if tool.argv.iter().any(|arg| arg.trim().is_empty()) {
            bail!("adapter command `{}` has an empty argv segment", tool.name);
        }
    }
    Ok(())
}

fn validate_referenced_file(
    manifest_dir: &Path,
    field: &str,
    relative: &str,
) -> anyhow::Result<()> {
    nonempty(field, relative.to_string())?;
    let path = Path::new(relative);
    if path.is_absolute() {
        bail!("{field} must be relative to adapter.toml");
    }
    let resolved = manifest_dir.join(path);
    if !resolved.is_file() {
        bail!("{field} references missing file {}", resolved.display());
    }
    Ok(())
}

fn clean_identifier(field: &str, value: &str) -> anyhow::Result<String> {
    let value = value.trim();
    if value.is_empty() {
        bail!("{field} must not be empty");
    }
    if value.chars().any(char::is_whitespace) {
        bail!("{field} `{value}` must not contain whitespace");
    }
    Ok(value.to_string())
}

fn nonempty(field: &str, value: String) -> anyhow::Result<String> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{AdapterOperationalState, AdapterRegistry};
    use crate::adapter_compatibility::{core_compatibility_inventory, CompatibilityReasonCode};
    use crate::manifest_integrity::canonical_manifest_digest;

    #[test]
    fn adapter_discovery_roots_include_valid_adapters_and_warnings() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let env_root = dir.path().join("env-adapters");
        let home = dir.path().join("ldgr-home");
        let user_home = dir.path().join("user-home");
        write_adapter(&env_root.join("env"), "env-adapter", &["env"], None)?;
        write_adapter(&home.join("adapters/home"), "home-adapter", &["home"], None)?;
        write_adapter(
            &user_home.join(".ldgr/adapters/user"),
            "user-adapter",
            &["user"],
            None,
        )?;
        fs::create_dir_all(env_root.join("broken"))?;
        fs::write(env_root.join("broken/adapter.toml"), "[adapter\n")?;

        let registry = AdapterRegistry::discover_from_roots([
            env_root,
            home.join("adapters"),
            user_home.join(".ldgr/adapters"),
        ]);

        let slugs = registry
            .adapters
            .iter()
            .map(|adapter| adapter.slug.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            slugs,
            vec!["broken", "env-adapter", "home-adapter", "user-adapter"]
        );
        assert_eq!(
            registry.find("broken").unwrap().state,
            AdapterOperationalState::Invalid
        );
        assert_eq!(registry.find("home").unwrap().slug, "home-adapter");
        assert!(
            registry
                .warnings
                .iter()
                .any(|warning| warning.message.contains("failed to parse adapter manifest")),
            "{:#?}",
            registry.warnings
        );
        Ok(())
    }

    #[test]
    fn adapter_discovery_skips_digest_mismatch_without_hiding_valid_adapter() -> anyhow::Result<()>
    {
        let dir = TempDir::new()?;
        let root = dir.path().join("adapters");
        write_adapter(&root.join("valid"), "valid-adapter", &[], None)?;
        write_adapter(
            &root.join("tampered"),
            "tampered-adapter",
            &[],
            Some("sha256:0000000000000000000000000000000000000000000000000000000000000000"),
        )?;

        let registry = AdapterRegistry::discover_from_roots([root]);

        assert_eq!(registry.adapters.len(), 2);
        assert_eq!(
            registry.find("valid-adapter").unwrap().state,
            AdapterOperationalState::Ready
        );
        assert_eq!(
            registry.find("tampered-adapter").unwrap().state,
            AdapterOperationalState::Invalid
        );
        assert!(
            registry
                .warnings
                .iter()
                .any(|warning| warning.message.contains("adapter manifest digest mismatch")),
            "{:#?}",
            registry.warnings
        );
        Ok(())
    }

    #[test]
    fn adapter_discovery_rejects_missing_profile_files_and_empty_commands() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let root = dir.path().join("adapters");
        write_adapter(&root.join("valid"), "valid-adapter", &[], None)?;
        write_raw_adapter(
            &root.join("missing-file"),
            r#"
[adapter]
slug = "missing-file"
title = "Missing file"
core_version = "0.1"

[profile]
loop_prompt_path = "missing.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"
"#,
        )?;
        write_raw_adapter(
            &root.join("empty-command"),
            r#"
[adapter]
slug = "empty-command"
title = "Empty command"
core_version = "0.1"

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"

[[tools]]
name = "empty-command"
argv = []
"#,
        )?;

        let registry = AdapterRegistry::discover_from_roots([root]);

        assert_eq!(registry.adapters.len(), 3);
        assert_eq!(
            registry.find("valid-adapter").unwrap().state,
            AdapterOperationalState::Ready
        );
        assert_eq!(
            registry.find("missing-file").unwrap().state,
            AdapterOperationalState::Invalid
        );
        assert_eq!(
            registry.find("empty-command").unwrap().state,
            AdapterOperationalState::Invalid
        );
        assert!(
            registry
                .warnings
                .iter()
                .any(|warning| warning.message.contains("profile.loop_prompt_path")),
            "{:#?}",
            registry.warnings
        );
        assert!(
            registry
                .warnings
                .iter()
                .any(|warning| warning.message.contains("empty argv")),
            "{:#?}",
            registry.warnings
        );
        Ok(())
    }

    #[test]
    fn adapter_discovery_duplicate_slugs_and_aliases_are_deterministic() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let root = dir.path().join("adapters");
        write_adapter(&root.join("a"), "alpha", &["shared", "alpha-alias"], None)?;
        write_adapter(&root.join("b"), "alpha", &["later"], None)?;
        write_adapter(&root.join("c"), "charlie", &["shared"], None)?;

        let registry = AdapterRegistry::discover_from_roots([root]);

        assert_eq!(registry.adapters.len(), 3);
        assert_eq!(registry.adapters[0].slug, "alpha");
        assert_eq!(registry.adapters[0].aliases, vec!["shared", "alpha-alias"]);
        assert_eq!(registry.adapters[0].state, AdapterOperationalState::Invalid);
        assert_eq!(registry.adapters[1].slug, "alpha");
        assert_eq!(registry.adapters[1].state, AdapterOperationalState::Invalid);
        assert_eq!(registry.adapters[2].slug, "charlie");
        assert!(registry.adapters[2].aliases.is_empty());
        assert_eq!(registry.find("shared").unwrap().slug, "alpha");
        assert!(registry.resolve_namespace("alpha").is_none());
        assert!(
            registry
                .warnings
                .iter()
                .any(|warning| warning.message.contains("duplicate adapter slug `alpha`")),
            "{:#?}",
            registry.warnings
        );
        assert!(
            registry
                .warnings
                .iter()
                .any(|warning| warning.message.contains("adapter alias `shared` conflicts")),
            "{:#?}",
            registry.warnings
        );
        Ok(())
    }

    #[test]
    fn adapter_registry_resolves_advertised_command_metadata() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let root = dir.path().join("adapters");
        write_adapter(&root.join("valid"), "valid-adapter", &[], None)?;

        let registry = AdapterRegistry::discover_from_roots([root]);
        let commands = registry.resolve_command("valid-adapter-check");

        assert_eq!(commands.len(), 1);
        assert_eq!(commands[0].adapter_slug, "valid-adapter");
        assert_eq!(commands[0].argv, vec!["valid-adapter", "check"]);
        Ok(())
    }

    #[test]
    fn compatibility_states_remain_visible_and_gate_dispatch() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let root = dir.path().join("adapters");

        write_adapter(&root.join("ready"), "ready", &[], None)?;

        write_adapter(&root.join("legacy"), "code", &[], None)?;
        fs::remove_file(root.join("legacy/adapter-compatibility.json"))?;
        fs::write(
            root.join("legacy/adapter-database-contract.json"),
            crate::database_contract::generated_adapter_contract_json("code")?,
        )?;

        write_adapter(&root.join("malformed"), "malformed", &[], None)?;
        fs::write(root.join("malformed/adapter-compatibility.json"), "{")?;

        write_adapter(&root.join("protocol"), "protocol", &[], None)?;
        write_v2_sidecar(&root.join("protocol"), "protocol", 2, &[], &[])?;

        write_adapter(&root.join("capability"), "capability", &[], None)?;
        write_v2_sidecar(&root.join("capability"), "capability", 1, &["work.v2"], &[])?;

        write_adapter(&root.join("component"), "component", &[], None)?;
        write_v2_sidecar(
            &root.join("component"),
            "component",
            1,
            &[],
            &[serde_json::json!({
                "accepted_lineage_digests": ["sha256:e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"],
                "minimum_schema_version": 1,
                "namespace": "component",
                "schema_epoch": 1,
            })],
        )?;

        let registry = AdapterRegistry::discover_from_roots_with_profile(
            [root],
            &core_compatibility_inventory(),
            &[],
        );
        assert_eq!(registry.adapters.len(), 6);
        assert_eq!(
            registry.find("ready").unwrap().state,
            AdapterOperationalState::Ready
        );
        assert_eq!(
            registry.find("code").unwrap().state,
            AdapterOperationalState::Degraded
        );
        assert_eq!(
            registry.find("malformed").unwrap().state,
            AdapterOperationalState::Invalid
        );

        let cases = [
            (
                "protocol",
                CompatibilityReasonCode::ProtocolEpochUnsupported,
            ),
            ("capability", CompatibilityReasonCode::CoreCapabilityMissing),
            (
                "component",
                CompatibilityReasonCode::CentralComponentMissing,
            ),
        ];
        for (slug, code) in cases {
            let adapter = registry.find(slug).unwrap();
            assert_eq!(adapter.state, AdapterOperationalState::Blocked);
            assert_eq!(adapter.reasons[0].code, code);
            assert_eq!(
                adapter.repair.argv,
                vec!["ldgr", "update", "--adapter", slug]
            );
            assert!(registry.resolve_namespace(slug).is_none());
        }
        assert!(registry.resolve_namespace("ready").is_some());
        assert!(registry.resolve_namespace("code").is_some());
        assert_eq!(
            registry.find("code").unwrap().repair.command,
            "ldgr update --adapter code"
        );
        assert_eq!(
            registry.find("malformed").unwrap().repair.command,
            "ldgr adapter install malformed"
        );
        Ok(())
    }

    fn write_adapter(
        dir: &Path,
        slug: &str,
        aliases: &[&str],
        digest: Option<&str>,
    ) -> anyhow::Result<()> {
        let alias_list = aliases
            .iter()
            .map(|alias| format!("\"{alias}\""))
            .collect::<Vec<_>>()
            .join(", ");
        let mut manifest = format!(
            r#"
[adapter]
slug = "{slug}"
title = "{slug} title"
core_version = "0.1"
aliases = [{alias_list}]

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"

[[tools]]
name = "{slug}-check"
argv = ["{slug}", "check"]
description = "Run a check."
"#
        );
        if let Some(digest) = digest {
            manifest.push_str(&format!("\n[integrity]\nmanifest_digest = \"{digest}\"\n"));
        }
        write_raw_adapter(dir, &manifest)?;
        write_v2_sidecar(dir, slug, 1, &[], &[])
    }

    fn write_v2_sidecar(
        dir: &Path,
        slug: &str,
        protocol: i32,
        capabilities: &[&str],
        central_components: &[serde_json::Value],
    ) -> anyhow::Result<()> {
        fs::write(
            dir.join("adapter-compatibility.json"),
            serde_json::to_vec(&serde_json::json!({
                "adapter": slug,
                "compatibility": {
                    "adapter_protocol_epoch": protocol,
                    "central_components": central_components,
                    "minimum_core_schema": 5,
                    "required_core_capabilities": capabilities,
                },
                "format": "ldgr.adapter-compatibility.v2",
                "local_stores": [],
            }))?,
        )?;
        Ok(())
    }

    fn write_raw_adapter(dir: &Path, manifest: &str) -> anyhow::Result<()> {
        fs::create_dir_all(dir.join("prompts"))?;
        fs::create_dir_all(dir.join("templates"))?;
        fs::write(dir.join("prompts/loop.md"), "loop")?;
        fs::write(dir.join("templates/milestones.md"), "milestones")?;
        fs::write(dir.join("templates/spec.md"), "spec")?;
        let manifest_path = dir.join("adapter.toml");
        let mut text = manifest.to_string();
        if text.contains("manifest_digest = \"CALCULATED\"") {
            let digest = canonical_manifest_digest(&text)?;
            text = text.replace("CALCULATED", &digest);
        }
        fs::write(manifest_path, text)?;
        Ok(())
    }
}
