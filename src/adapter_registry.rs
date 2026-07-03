use std::collections::{BTreeMap, BTreeSet};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

use crate::manifest_integrity::{diagnose_manifest_digest, AdapterManifestDigestState};

pub const ADAPTER_MANIFEST_FILE: &str = "adapter.toml";
pub const LDGR_ADAPTER_PATH_ENV: &str = "LDGR_ADAPTER_PATH";
pub const LDGR_HOME_ENV: &str = "LDGR_HOME";
pub const HOME_ENV: &str = "HOME";
pub const PROJECT_ADAPTER_ROOT: &str = ".ldgr";
pub const INSTALLED_ADAPTERS_DIR: &str = "adapters";

#[derive(Clone, Debug, Default)]
pub struct AdapterDiscoveryEnvironment {
    pub adapter_path: Option<std::ffi::OsString>,
    pub ldgr_home: Option<std::ffi::OsString>,
    pub home: Option<std::ffi::OsString>,
    pub include_project_root: bool,
}

impl AdapterDiscoveryEnvironment {
    pub fn from_process_env() -> Self {
        Self {
            adapter_path: env::var_os(LDGR_ADAPTER_PATH_ENV),
            ldgr_home: env::var_os(LDGR_HOME_ENV),
            home: env::var_os(HOME_ENV),
            include_project_root: true,
        }
    }

    pub fn adapter_search_roots(&self) -> Vec<PathBuf> {
        let mut roots = Vec::new();
        if let Some(paths) = &self.adapter_path {
            roots.extend(env::split_paths(paths));
        }
        if self.include_project_root {
            roots.push(PathBuf::from(PROJECT_ADAPTER_ROOT));
        }
        if let Some(home) = &self.ldgr_home {
            let home = PathBuf::from(home);
            roots.push(home.join(INSTALLED_ADAPTERS_DIR));
            roots.push(home);
        }
        if let Some(home) = &self.home {
            let home = PathBuf::from(home).join(PROJECT_ADAPTER_ROOT);
            roots.push(home.join(INSTALLED_ADAPTERS_DIR));
            roots.push(home);
        }
        dedup_roots(roots)
    }
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

    pub fn discover_from_roots<I>(roots: I) -> Self
    where
        I: IntoIterator<Item = PathBuf>,
    {
        let mut registry = Self::default();
        let mut claimed_names = BTreeMap::<String, String>::new();

        for root in roots {
            for manifest_path in manifest_paths(&root, &mut registry.warnings) {
                match load_adapter_manifest(&manifest_path) {
                    Ok(mut adapter) => {
                        if let Some(owner) = claimed_names.get(&adapter.slug) {
                            registry.warnings.push(AdapterWarning::new(
                                manifest_path,
                                format!(
                                    "duplicate adapter slug `{}` already provided by {}; skipped",
                                    adapter.slug, owner
                                ),
                            ));
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
                    Err(error) => registry
                        .warnings
                        .push(AdapterWarning::new(manifest_path, format!("{error:#}"))),
                }
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
        self.adapters.iter().find_map(|adapter| {
            adapter.command_namespaces.iter().find(|command| {
                command.namespace == namespace
                    || command.aliases.iter().any(|alias| alias == namespace)
            })
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct DiscoveredAdapter {
    pub slug: String,
    pub title: String,
    pub core_version: String,
    pub aliases: Vec<String>,
    pub manifest_path: PathBuf,
    pub root_path: PathBuf,
    pub profile: AdapterProfile,
    pub commands: Vec<AdapterCommand>,
    pub command_namespaces: Vec<AdapterCommandNamespace>,
    pub target_profiles: Vec<AdapterTargetProfile>,
    pub verified_manifest_digest: Option<String>,
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
    pub help_groups: Vec<AdapterCommandHelpGroup>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterCommandHelpGroup {
    pub title: String,
    #[serde(default)]
    pub commands: Vec<AdapterCommandHelpEntry>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterCommandHelpEntry {
    pub usage: String,
    pub summary: Option<String>,
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
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestCommandHelp {
    usage: Option<String>,
    summary: Option<String>,
    details: Option<String>,
    #[serde(default)]
    groups: Vec<AdapterCommandHelpGroup>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestIntegrity {
    manifest_digest: Option<String>,
}

pub fn adapter_search_roots() -> Vec<PathBuf> {
    AdapterDiscoveryEnvironment::from_process_env().adapter_search_roots()
}

fn dedup_roots(roots: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = BTreeSet::new();
    let mut deduped = Vec::new();
    for root in roots {
        let key = root.to_string_lossy().to_string();
        if seen.insert(key) {
            deduped.push(root);
        }
    }
    deduped
}

pub fn adapter_manifest_paths(root: &Path, warnings: &mut Vec<AdapterWarning>) -> Vec<PathBuf> {
    manifest_paths(root, warnings)
}

fn manifest_paths(root: &Path, warnings: &mut Vec<AdapterWarning>) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let direct = root.join(ADAPTER_MANIFEST_FILE);
    if direct.is_file() {
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
        let manifest_path = child.join(ADAPTER_MANIFEST_FILE);
        if manifest_path.is_file() {
            paths.push(manifest_path);
        }
    }
    paths.sort();
    paths.dedup();
    paths
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
    let integrity = diagnose_manifest_digest(&manifest_text);
    if integrity.state == AdapterManifestDigestState::Failed {
        bail!(
            "failed to verify adapter manifest integrity {}: {}",
            manifest_path.display(),
            integrity
                .message
                .as_deref()
                .unwrap_or("manifest digest verification failed")
        );
    }
    let verified_manifest_digest = integrity.verified_manifest_digest;

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
            details: help.as_ref().and_then(|help| help.details.clone()),
            help_groups: help.map(|help| help.groups).unwrap_or_default(),
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
            help_groups: Vec::new(),
        });
    }

    Ok(DiscoveredAdapter {
        slug: adapter_slug,
        title: nonempty("adapter.title", manifest.adapter.title)?,
        core_version: nonempty("adapter.core_version", manifest.adapter.core_version)?,
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
    })
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
    use std::ffi::OsString;
    use std::fs;
    use std::path::Path;

    use tempfile::TempDir;

    use super::{AdapterDiscoveryEnvironment, AdapterRegistry};
    use crate::manifest_integrity::canonical_manifest_digest;

    #[test]
    fn adapter_search_roots_cover_env_ldgr_home_and_default_locations() {
        let path_separator = if cfg!(windows) { ";" } else { ":" };
        let env_paths = ["/env/one", "/env/two"].join(path_separator);
        let roots = AdapterDiscoveryEnvironment {
            adapter_path: Some(OsString::from(env_paths)),
            ldgr_home: Some(OsString::from("/ldgr-home")),
            home: Some(OsString::from("/user-home")),
            include_project_root: true,
        }
        .adapter_search_roots();

        assert_eq!(
            roots,
            vec![
                Path::new("/env/one").to_path_buf(),
                Path::new("/env/two").to_path_buf(),
                Path::new(".ldgr").to_path_buf(),
                Path::new("/ldgr-home/adapters").to_path_buf(),
                Path::new("/ldgr-home").to_path_buf(),
                Path::new("/user-home/.ldgr/adapters").to_path_buf(),
                Path::new("/user-home/.ldgr").to_path_buf(),
            ]
        );
    }

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
        assert_eq!(slugs, vec!["env-adapter", "home-adapter", "user-adapter"]);
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

        assert_eq!(registry.adapters.len(), 1);
        assert_eq!(registry.adapters[0].slug, "valid-adapter");
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

        assert_eq!(registry.adapters.len(), 1);
        assert_eq!(registry.adapters[0].slug, "valid-adapter");
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

        assert_eq!(registry.adapters.len(), 2);
        assert_eq!(registry.adapters[0].slug, "alpha");
        assert_eq!(registry.adapters[0].aliases, vec!["shared", "alpha-alias"]);
        assert_eq!(registry.adapters[1].slug, "charlie");
        assert!(registry.adapters[1].aliases.is_empty());
        assert_eq!(registry.find("shared").unwrap().slug, "alpha");
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
        write_raw_adapter(dir, &manifest)
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
