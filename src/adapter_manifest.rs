//! Public adapter manifest model shared by LDGR core and adapter crates.
//!
//! These types describe the open adapter manifest surface only: adapter
//! identity, prompt/template profile paths, adapter-owned tools and command
//! namespaces, target profiles, probe families, aliases, and optional manifest
//! integrity metadata. Commercial licensing, entitlement, and policy decisions
//! intentionally live outside this model.

use std::{
    collections::HashSet,
    fs,
    ops::Range,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use crate::manifest_integrity::{diagnose_manifest_digest, AdapterManifestIntegrityReport};

/// Load, parse, and validate an adapter manifest file into the public manifest model.
///
/// This is the structured public API for callers that need machine-readable
/// diagnostics. Paths in the manifest remain adapter-relative strings; callers
/// can resolve them against [`AdapterManifestParseReport::manifest_dir`].
pub fn load_adapter_manifest(
    manifest_path: impl AsRef<Path>,
) -> Result<AdapterManifestParseReport, AdapterManifestParseError> {
    let manifest_path = manifest_path.as_ref();
    let manifest_text = fs::read_to_string(manifest_path).map_err(|error| {
        AdapterManifestParseError::single(AdapterManifestDiagnostic {
            code: AdapterManifestDiagnosticCode::Io,
            message: format!(
                "failed to read adapter manifest {}: {error}",
                manifest_path.display()
            ),
            manifest_path: Some(manifest_path.to_path_buf()),
            span: None,
            field: None,
        })
    })?;
    parse_adapter_manifest_with_path(&manifest_text, Some(manifest_path))
}

/// Parse and validate adapter manifest TOML into the public manifest model.
///
/// This compatibility wrapper preserves the historical `anyhow::Result` shape.
/// New integrations should prefer [`parse_adapter_manifest_text`] or
/// [`load_adapter_manifest`] when they need structured diagnostics.
pub fn parse_adapter_manifest(manifest_text: &str) -> anyhow::Result<AdapterManifest> {
    parse_adapter_manifest_text(manifest_text)
        .map(|report| report.manifest)
        .map_err(Into::into)
}

/// Parse and validate adapter manifest TOML with structured diagnostics.
pub fn parse_adapter_manifest_text(
    manifest_text: &str,
) -> Result<AdapterManifestParseReport, AdapterManifestParseError> {
    parse_adapter_manifest_with_path(manifest_text, None)
}

fn parse_adapter_manifest_with_path(
    manifest_text: &str,
    manifest_path: Option<&Path>,
) -> Result<AdapterManifestParseReport, AdapterManifestParseError> {
    let integrity = diagnose_manifest_digest(manifest_text);
    let manifest: AdapterManifest = toml::from_str(manifest_text).map_err(|error| {
        AdapterManifestParseError::single(diagnostic_from_toml_error(error, manifest_path))
    })?;
    let manifest_dir = manifest_path.and_then(Path::parent);
    let validation = validate_adapter_manifest_semantics(&manifest, manifest_dir);
    if !validation.is_valid() {
        return Err(AdapterManifestParseError::new(
            validation
                .diagnostics
                .into_iter()
                .map(|mut diagnostic| {
                    diagnostic.manifest_path = manifest_path.map(Path::to_path_buf);
                    diagnostic
                })
                .collect(),
        ));
    }
    Ok(AdapterManifestParseReport {
        manifest,
        manifest_path: manifest_path.map(Path::to_path_buf),
        manifest_dir: manifest_dir.map(Path::to_path_buf),
        integrity,
    })
}

fn diagnostic_from_toml_error(
    error: toml::de::Error,
    manifest_path: Option<&Path>,
) -> AdapterManifestDiagnostic {
    let message = error.to_string();
    let code = if message.contains("missing field") {
        AdapterManifestDiagnosticCode::MissingRequiredField
    } else {
        AdapterManifestDiagnosticCode::MalformedToml
    };
    let field = missing_field_name(&message);
    AdapterManifestDiagnostic {
        code,
        message: format!("failed to parse adapter manifest TOML: {message}"),
        manifest_path: manifest_path.map(Path::to_path_buf),
        span: error.span(),
        field,
    }
}

fn missing_field_name(message: &str) -> Option<String> {
    let start = message.find("missing field `")? + "missing field `".len();
    let end = message[start..].find('`')? + start;
    Some(message[start..end].to_string())
}

/// Validate cross-field manifest rules that serde/TOML cannot express.
///
/// This validation is intentionally limited to public adapter shape concerns
/// such as slug syntax, referenced profile files, duplicate public names,
/// command argv shape, and manifest compatibility version. It does not enforce
/// license, entitlement, commercial policy, or adapter-specific readiness rules.
pub fn validate_adapter_manifest(manifest: &AdapterManifest) -> anyhow::Result<()> {
    validate_adapter_manifest_semantics(manifest, None).into_result()?;
    Ok(())
}

/// Validate public adapter manifest semantics and collect all diagnostics.
///
/// When `manifest_dir` is provided, profile path fields are resolved relative to
/// that directory and missing files are reported. When it is absent, this still
/// checks that path fields are non-empty relative paths.
pub fn validate_adapter_manifest_semantics(
    manifest: &AdapterManifest,
    manifest_dir: Option<&Path>,
) -> AdapterManifestSemanticValidationReport {
    let mut diagnostics = Vec::new();

    validate_slug_field(&mut diagnostics, "adapter.slug", &manifest.adapter.slug);
    validate_nonempty_field(&mut diagnostics, "adapter.title", &manifest.adapter.title);
    validate_core_version(&mut diagnostics, &manifest.adapter.core_version);

    let mut seen_adapter_aliases = HashSet::new();
    for (index, alias) in manifest.adapter.aliases.iter().enumerate() {
        let field = format!("adapter.aliases[{index}]");
        validate_slug_field(&mut diagnostics, &field, alias);
        let trimmed = alias.trim();
        if !trimmed.is_empty() && !seen_adapter_aliases.insert(trimmed.to_string()) {
            diagnostics.push(duplicate_value(
                field,
                format!("duplicate adapter alias `{trimmed}`"),
            ));
        }
    }

    validate_profile_path(
        &mut diagnostics,
        manifest_dir,
        "profile.loop_prompt_path",
        &manifest.profile.loop_prompt_path,
    );
    validate_profile_path(
        &mut diagnostics,
        manifest_dir,
        "profile.default_milestone_template",
        &manifest.profile.default_milestone_template,
    );
    validate_profile_path(
        &mut diagnostics,
        manifest_dir,
        "profile.spec_artifact_path",
        &manifest.profile.spec_artifact_path,
    );
    validate_nonempty_field(
        &mut diagnostics,
        "profile.readiness_policy",
        &manifest.profile.readiness_policy,
    );

    let mut seen_tools = HashSet::new();
    for (index, tool) in manifest.tools.iter().enumerate() {
        let field = format!("tools[{index}].name");
        validate_slug_field(&mut diagnostics, &field, &tool.name);
        let trimmed = tool.name.trim();
        if !trimmed.is_empty() && !seen_tools.insert(trimmed.to_string()) {
            diagnostics.push(duplicate_value(
                field,
                format!("duplicate tool name `{trimmed}`"),
            ));
        }
        validate_argv_field(
            &mut diagnostics,
            &format!("tools[{index}].argv"),
            &tool.argv,
        );
    }

    let mut seen_command_namespaces = HashSet::new();
    let mut seen_command_aliases = HashSet::new();
    for (index, command) in manifest.commands.iter().enumerate() {
        let namespace_field = format!("commands[{index}].namespace");
        validate_namespace_field(&mut diagnostics, &namespace_field, &command.namespace);
        let namespace = command.namespace.trim();
        if !namespace.is_empty() && !seen_command_namespaces.insert(namespace.to_string()) {
            diagnostics.push(duplicate_value(
                namespace_field,
                format!("duplicate command namespace `{namespace}`"),
            ));
        }
        validate_argv_field(
            &mut diagnostics,
            &format!("commands[{index}].argv"),
            &command.argv,
        );
        validate_nonempty_field(
            &mut diagnostics,
            &format!("commands[{index}].title"),
            &command.title,
        );
        validate_nonempty_field(
            &mut diagnostics,
            &format!("commands[{index}].description"),
            &command.description,
        );
        validate_nonempty_field(
            &mut diagnostics,
            &format!("commands[{index}].help.usage"),
            &command.help.usage,
        );
        validate_nonempty_field(
            &mut diagnostics,
            &format!("commands[{index}].help.summary"),
            &command.help.summary,
        );
        for (alias_index, alias) in command.aliases.iter().enumerate() {
            let field = format!("commands[{index}].aliases[{alias_index}]");
            validate_namespace_field(&mut diagnostics, &field, alias);
            let trimmed = alias.trim();
            if trimmed == namespace {
                diagnostics.push(invalid_value(
                    field.clone(),
                    format!("command alias `{trimmed}` duplicates command namespace"),
                ));
            }
            if !trimmed.is_empty() && !seen_command_aliases.insert(trimmed.to_string()) {
                diagnostics.push(duplicate_value(
                    field,
                    format!("duplicate command alias `{trimmed}`"),
                ));
            }
        }
    }

    let mut seen_target_profiles = HashSet::new();
    for (index, target_profile) in manifest.target_profiles.iter().enumerate() {
        let field = format!("target_profiles[{index}].slug");
        validate_slug_field(&mut diagnostics, &field, &target_profile.slug);
        let trimmed = target_profile.slug.trim();
        if !trimmed.is_empty() && !seen_target_profiles.insert(trimmed.to_string()) {
            diagnostics.push(duplicate_value(
                field,
                format!("duplicate target profile slug `{trimmed}`"),
            ));
        }
        validate_nonempty_field(
            &mut diagnostics,
            &format!("target_profiles[{index}].title"),
            &target_profile.title,
        );
        validate_nonempty_field(
            &mut diagnostics,
            &format!("target_profiles[{index}].target_type"),
            &target_profile.target_type,
        );
        validate_nonempty_field(
            &mut diagnostics,
            &format!("target_profiles[{index}].description"),
            &target_profile.description,
        );
        let mut seen_probes = HashSet::new();
        for (probe_index, probe) in target_profile.probes.iter().enumerate() {
            let field = format!("target_profiles[{index}].probes[{probe_index}].slug");
            validate_slug_field(&mut diagnostics, &field, &probe.slug);
            let trimmed = probe.slug.trim();
            if !trimmed.is_empty() && !seen_probes.insert(trimmed.to_string()) {
                diagnostics.push(duplicate_value(
                    field,
                    format!("duplicate probe slug `{trimmed}` in target_profiles[{index}]"),
                ));
            }
            validate_nonempty_field(
                &mut diagnostics,
                &format!("target_profiles[{index}].probes[{probe_index}].title"),
                &probe.title,
            );
            validate_nonempty_field(
                &mut diagnostics,
                &format!("target_profiles[{index}].probes[{probe_index}].description"),
                &probe.description,
            );
        }
    }

    AdapterManifestSemanticValidationReport { diagnostics }
}

fn validate_slug_field(diagnostics: &mut Vec<AdapterManifestDiagnostic>, field: &str, value: &str) {
    let value = value.trim();
    if !is_valid_slug(value) {
        diagnostics.push(AdapterManifestDiagnostic {
            code: AdapterManifestDiagnosticCode::InvalidSlug,
            message: format!(
                "{field} `{value}` is invalid; expected lowercase identifier using letters, digits, and hyphens"
            ),
            manifest_path: None,
            span: None,
            field: Some(field.to_string()),
        });
    }
}

fn validate_namespace_field(
    diagnostics: &mut Vec<AdapterManifestDiagnostic>,
    field: &str,
    value: &str,
) {
    let value = value.trim();
    if !is_valid_namespace(value) {
        diagnostics.push(invalid_value(
            field.to_string(),
            format!(
                "{field} `{value}` is invalid; expected lowercase dot-separated identifier segments using letters, digits, and hyphens"
            ),
        ));
    }
}

fn is_valid_slug(slug: &str) -> bool {
    !slug.is_empty()
        && !slug.contains("--")
        && slug.split('-').all(|segment| {
            let mut chars = segment.chars();
            matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
                && chars
                    .all(|character| character.is_ascii_lowercase() || character.is_ascii_digit())
        })
}

fn is_valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty() && namespace.split('.').all(is_valid_slug)
}

fn validate_core_version(diagnostics: &mut Vec<AdapterManifestDiagnostic>, core_version: &str) {
    let version = core_version.trim();
    if version.is_empty() {
        diagnostics.push(invalid_value(
            "adapter.core_version".to_string(),
            "adapter.core_version must not be empty".to_string(),
        ));
        return;
    }

    let parts = version.split('.').collect::<Vec<_>>();
    let numeric = (2..=3).contains(&parts.len())
        && parts.iter().all(|part| {
            !part.is_empty() && part.chars().all(|character| character.is_ascii_digit())
        });
    if !numeric {
        diagnostics.push(invalid_value(
            "adapter.core_version".to_string(),
            format!("adapter.core_version `{version}` is invalid; expected major.minor[.patch]"),
        ));
        return;
    }
    if parts[0] != "0" || parts[1] != "1" {
        diagnostics.push(AdapterManifestDiagnostic {
            code: AdapterManifestDiagnosticCode::UnsupportedCoreVersion,
            message: format!(
                "adapter.core_version `{version}` is unsupported by this public manifest model; supported compatibility line is 0.1"
            ),
            manifest_path: None,
            span: None,
            field: Some("adapter.core_version".to_string()),
        });
    }
}

fn validate_profile_path(
    diagnostics: &mut Vec<AdapterManifestDiagnostic>,
    manifest_dir: Option<&Path>,
    field: &str,
    relative: &str,
) {
    let trimmed = relative.trim();
    if trimmed.is_empty() {
        diagnostics.push(invalid_value(
            field.to_string(),
            format!("{field} must not be empty"),
        ));
        return;
    }
    let path = Path::new(trimmed);
    if path.is_absolute() {
        diagnostics.push(invalid_value(
            field.to_string(),
            format!("{field} must be relative to adapter.toml"),
        ));
        return;
    }
    if let Some(manifest_dir) = manifest_dir {
        let resolved = manifest_dir.join(path);
        if !resolved.is_file() {
            diagnostics.push(AdapterManifestDiagnostic {
                code: AdapterManifestDiagnosticCode::MissingReferencedPath,
                message: format!("{field} references missing file {}", resolved.display()),
                manifest_path: None,
                span: None,
                field: Some(field.to_string()),
            });
        }
    }
}

fn validate_argv_field(
    diagnostics: &mut Vec<AdapterManifestDiagnostic>,
    field: &str,
    argv: &[String],
) {
    if argv.is_empty() {
        diagnostics.push(invalid_value(
            field.to_string(),
            format!("{field} must contain at least one executable entry"),
        ));
    }
    for (entry_index, entry) in argv.iter().enumerate() {
        if entry.trim().is_empty() {
            diagnostics.push(invalid_value(
                format!("{field}[{entry_index}]"),
                format!("{field}[{entry_index}] must not be empty"),
            ));
        }
    }
}

fn validate_nonempty_field(
    diagnostics: &mut Vec<AdapterManifestDiagnostic>,
    field: &str,
    value: &str,
) {
    if value.trim().is_empty() {
        diagnostics.push(invalid_value(
            field.to_string(),
            format!("{field} must not be empty"),
        ));
    }
}

fn invalid_value(field: String, message: String) -> AdapterManifestDiagnostic {
    AdapterManifestDiagnostic {
        code: AdapterManifestDiagnosticCode::InvalidValue,
        message,
        manifest_path: None,
        span: None,
        field: Some(field),
    }
}

fn duplicate_value(field: String, message: String) -> AdapterManifestDiagnostic {
    AdapterManifestDiagnostic {
        code: AdapterManifestDiagnosticCode::DuplicateValue,
        message,
        manifest_path: None,
        span: None,
        field: Some(field),
    }
}

/// Structured result returned by semantic manifest validation.
#[derive(Clone, Debug, Default, Serialize)]
pub struct AdapterManifestSemanticValidationReport {
    /// All semantic diagnostics found during validation.
    pub diagnostics: Vec<AdapterManifestDiagnostic>,
}

impl AdapterManifestSemanticValidationReport {
    /// Whether no semantic diagnostics were reported.
    pub fn is_valid(&self) -> bool {
        self.diagnostics.is_empty()
    }

    /// Convert the report into a conventional result.
    pub fn into_result(self) -> Result<(), AdapterManifestValidationError> {
        if self.diagnostics.is_empty() {
            Ok(())
        } else {
            Err(AdapterManifestValidationError::new(self.diagnostics))
        }
    }
}

/// Machine-readable adapter manifest semantic validation failure.
#[derive(Clone, Debug, thiserror::Error)]
#[error("adapter manifest semantic validation failed: {diagnostics_summary}")]
pub struct AdapterManifestValidationError {
    /// One or more diagnostics describing why semantic validation failed.
    pub diagnostics: Vec<AdapterManifestDiagnostic>,
    diagnostics_summary: String,
}

impl AdapterManifestValidationError {
    fn new(diagnostics: Vec<AdapterManifestDiagnostic>) -> Self {
        let diagnostics_summary = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Self {
            diagnostics,
            diagnostics_summary,
        }
    }
}

/// Structured result returned by the public manifest parser/loader.
#[derive(Clone, Debug, Serialize)]
pub struct AdapterManifestParseReport {
    /// Parsed public manifest model.
    pub manifest: AdapterManifest,
    /// Source manifest path when parsed through [`load_adapter_manifest`].
    pub manifest_path: Option<PathBuf>,
    /// Directory containing the manifest; path-bearing fields are relative to it.
    pub manifest_dir: Option<PathBuf>,
    /// Public digest verification diagnostic for discovery/apply warnings.
    pub integrity: AdapterManifestIntegrityReport,
}

/// Machine-readable adapter manifest parse failure.
#[derive(Clone, Debug, thiserror::Error)]
#[error("adapter manifest parse failed: {diagnostics_summary}")]
pub struct AdapterManifestParseError {
    /// One or more diagnostics describing why parsing/loading failed.
    pub diagnostics: Vec<AdapterManifestDiagnostic>,
    diagnostics_summary: String,
}

impl AdapterManifestParseError {
    fn single(diagnostic: AdapterManifestDiagnostic) -> Self {
        Self::new(vec![diagnostic])
    }

    fn new(diagnostics: Vec<AdapterManifestDiagnostic>) -> Self {
        let diagnostics_summary = diagnostics
            .iter()
            .map(|diagnostic| diagnostic.message.as_str())
            .collect::<Vec<_>>()
            .join("; ");
        Self {
            diagnostics,
            diagnostics_summary,
        }
    }
}

/// One structured manifest parser diagnostic.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct AdapterManifestDiagnostic {
    /// Stable category for programmatic handling.
    pub code: AdapterManifestDiagnosticCode,
    /// Human-readable diagnostic message.
    pub message: String,
    /// Source manifest path when available.
    pub manifest_path: Option<PathBuf>,
    /// Byte span reported by the TOML parser when available.
    pub span: Option<Range<usize>>,
    /// Required field name when the parser can identify one.
    pub field: Option<String>,
}

/// Stable adapter manifest parser diagnostic categories.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum AdapterManifestDiagnosticCode {
    /// The manifest file could not be read.
    Io,
    /// The manifest text is not valid TOML or has an incompatible TOML shape.
    MalformedToml,
    /// A required public manifest field is absent.
    MissingRequiredField,
    /// TOML parsed successfully but a parser-level value rule failed.
    InvalidValue,
    /// A manifest slug field is not a valid public slug.
    InvalidSlug,
    /// A path-bearing manifest field references a missing file.
    MissingReferencedPath,
    /// A manifest name/slug/alias appears more than once in the same scope.
    DuplicateValue,
    /// The manifest advertises an unsupported LDGR core compatibility version.
    UnsupportedCoreVersion,
}

/// Adapter manifest alias value.
///
/// Aliases are plain strings in TOML and JSON so existing adapters do not need
/// a wrapper object to consume the public model.
pub type ManifestAlias = String;

/// Adapter-relative path value used by profile path fields.
///
/// Paths are stored as strings in the public manifest model and resolved by
/// callers relative to the manifest directory.
pub type ManifestPath = String;

/// Complete public `adapter.toml` shape understood by LDGR core.
///
/// Required sections are [`adapter`](AdapterManifest::adapter) and
/// [`profile`](AdapterManifest::profile). Tool declarations, command
/// namespaces, target profiles, and integrity metadata are optional so older
/// community adapters can adopt the shared model incrementally.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdapterManifest {
    /// Adapter identity and aliases used for discovery and display.
    pub adapter: ManifestAdapter,
    /// Paths and readiness guidance installed by the adapter bundle.
    pub profile: ManifestProfile,
    /// Adapter-owned tools advertised to humans and future automation.
    #[serde(default)]
    pub tools: Vec<ManifestTool>,
    /// Command namespaces that core may dispatch to the adapter executable.
    #[serde(default)]
    pub commands: Vec<ManifestCommandNamespace>,
    /// Target profiles and probe families the adapter knows how to evaluate.
    #[serde(default)]
    pub target_profiles: Vec<ManifestTargetProfile>,
    /// Optional integrity metadata for the manifest itself.
    #[serde(default)]
    pub integrity: Option<ManifestIntegrity>,
}

/// Public adapter identity metadata.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestAdapter {
    /// Stable adapter slug, for example `research` or `example`.
    pub slug: String,
    /// Human-readable adapter title.
    pub title: String,
    /// Minimum/target LDGR core manifest compatibility version advertised by the adapter.
    pub core_version: String,
    /// Alternative names that may be shown in discovery or used by installers.
    #[serde(default)]
    pub aliases: Vec<ManifestAlias>,
}

/// Prompt/template profile paths and completion guidance supplied by an adapter.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestProfile {
    /// Adapter-relative path to the loop prompt file.
    pub loop_prompt_path: ManifestPath,
    /// Adapter-relative path to the default milestone template.
    pub default_milestone_template: ManifestPath,
    /// Adapter-relative path to the default specification artifact template.
    pub spec_artifact_path: ManifestPath,
    /// Adapter-authored readiness guidance; not a commercial policy field.
    pub readiness_policy: String,
}

/// Adapter-owned executable tool declaration.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestTool {
    /// Stable tool name within the adapter manifest.
    pub name: String,
    /// Executable and default arguments used to invoke the tool.
    pub argv: Vec<String>,
    /// Optional human-readable description of the tool.
    pub description: Option<String>,
}

/// Adapter command namespace exposed through the core `ldgr <namespace>` surface.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestCommandNamespace {
    /// Lowercase dot-separated command namespace, for example `research`.
    pub namespace: String,
    /// Executable and fixed arguments that receive forwarded namespace arguments.
    pub argv: Vec<String>,
    /// Alternative command names for discovery/help surfaces.
    #[serde(default)]
    pub aliases: Vec<ManifestAlias>,
    /// Human-readable namespace title.
    pub title: String,
    /// Longer namespace description.
    pub description: String,
    /// Help text shown by core or adapter discovery surfaces.
    pub help: ManifestCommandHelp,
    /// Optional capability labels such as `dispatch` or `help`.
    #[serde(default)]
    pub capabilities: Vec<String>,
}

/// Help text for an adapter command namespace.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestCommandHelp {
    /// Usage line, usually beginning with `ldgr <namespace>`.
    pub usage: String,
    /// Short summary for command listings.
    pub summary: String,
    /// Optional longer help details.
    #[serde(default)]
    pub details: Option<String>,
    /// Optional grouped command tree entries displayed by LDGR core without
    /// executing adapter-owned policy code.
    #[serde(default)]
    pub groups: Vec<ManifestCommandHelpGroup>,
}

/// Group of adapter command examples shown in core help surfaces.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestCommandHelpGroup {
    /// Human-readable command group title.
    pub title: String,
    /// Commands or usage examples in this group.
    #[serde(default)]
    pub commands: Vec<ManifestCommandHelpEntry>,
}

/// One adapter command help entry shown by core help surfaces.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestCommandHelpEntry {
    /// Usage line, usually beginning with `ldgr <namespace>`.
    pub usage: String,
    /// Optional short explanation.
    #[serde(default)]
    pub summary: Option<String>,
}

/// Target profile that an adapter can install, inspect, or evaluate.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestTargetProfile {
    /// Stable target profile slug within the adapter.
    pub slug: String,
    /// Human-readable target profile title.
    pub title: String,
    /// Broad target type, for example `investigation` or `reference-adapter`.
    pub target_type: String,
    /// Description of when this target profile applies.
    pub description: String,
    /// Probe families associated with the target profile.
    #[serde(default)]
    pub probes: Vec<ManifestProbeFamily>,
}

/// Probe family advertised by a target profile.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestProbeFamily {
    /// Stable probe slug within the target profile.
    pub slug: String,
    /// Human-readable probe title.
    pub title: String,
    /// Description of the evidence or check this probe represents.
    pub description: String,
    /// Optional LDGR artifact kind expected to hold probe evidence.
    pub evidence_artifact_kind: Option<String>,
    /// Optional text template for expected evidence or outcomes.
    pub expectation_template: Option<String>,
    /// Optional command or review hint for validating the probe.
    pub validation_hint: Option<String>,
}

/// Optional manifest integrity metadata.
///
/// The current public integrity field is `manifest_digest`, typically formatted
/// as `sha256:<64 lowercase hex characters>`. Signature, attestation, license,
/// and entitlement data are intentionally not part of this public model.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestIntegrity {
    /// Digest of the canonicalized manifest content with this field removed.
    pub manifest_digest: Option<String>,
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::manifest_integrity::{canonical_manifest_digest, AdapterManifestDigestState};

    use super::{
        load_adapter_manifest, parse_adapter_manifest, parse_adapter_manifest_text,
        validate_adapter_manifest_semantics, AdapterManifestDiagnosticCode,
    };

    const BASE_MANIFEST: &str = r#"
[adapter]
slug = "example"
title = "Example adapter"
core_version = "0.1"
aliases = ["example"]

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "Evidence must pass."
"#;

    #[test]
    fn adapter_manifest_text_parser_returns_structured_report_for_valid_manifest(
    ) -> anyhow::Result<()> {
        let report = parse_adapter_manifest_text(BASE_MANIFEST)?;

        assert_eq!(report.manifest.adapter.slug, "example");
        assert!(report.manifest_path.is_none());
        assert!(report.manifest_dir.is_none());
        Ok(())
    }

    #[test]
    fn adapter_manifest_text_parser_reports_malformed_toml_diagnostic() {
        let error = parse_adapter_manifest_text("[adapter\nslug = \"example\"")
            .expect_err("malformed TOML should fail");

        assert_eq!(error.diagnostics.len(), 1);
        assert_eq!(
            error.diagnostics[0].code,
            AdapterManifestDiagnosticCode::MalformedToml
        );
        assert!(error.diagnostics[0]
            .message
            .contains("failed to parse adapter manifest TOML"));
        assert!(error.diagnostics[0].span.is_some());
    }

    #[test]
    fn adapter_manifest_text_parser_reports_missing_required_field_diagnostic() {
        let error = parse_adapter_manifest_text(
            r#"
[adapter]
slug = "example"
title = "Example adapter"

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "Evidence must pass."
"#,
        )
        .expect_err("missing core_version should fail");

        assert_eq!(error.diagnostics.len(), 1);
        assert_eq!(
            error.diagnostics[0].code,
            AdapterManifestDiagnosticCode::MissingRequiredField
        );
        assert_eq!(error.diagnostics[0].field.as_deref(), Some("core_version"));
    }

    #[test]
    fn adapter_manifest_loader_preserves_path_bearing_fields_and_source_paths() -> anyhow::Result<()>
    {
        let tempdir = tempfile::tempdir()?;
        let manifest_path = tempdir.path().join("adapter.toml");
        fs::create_dir_all(tempdir.path().join("prompts"))?;
        fs::create_dir_all(tempdir.path().join("templates"))?;
        fs::write(tempdir.path().join("prompts/loop.md"), "loop")?;
        fs::write(tempdir.path().join("templates/milestones.md"), "milestones")?;
        fs::write(tempdir.path().join("templates/spec.md"), "spec")?;
        fs::write(&manifest_path, BASE_MANIFEST)?;

        let report = load_adapter_manifest(&manifest_path)?;

        assert_eq!(
            report.manifest_path.as_deref(),
            Some(manifest_path.as_path())
        );
        assert_eq!(report.manifest_dir.as_deref(), Some(tempdir.path()));
        assert_eq!(report.manifest.profile.loop_prompt_path, "prompts/loop.md");
        assert_eq!(
            report.manifest.profile.default_milestone_template,
            "templates/milestones.md"
        );
        assert_eq!(
            report.manifest.profile.spec_artifact_path,
            "templates/spec.md"
        );
        Ok(())
    }

    #[test]
    fn adapter_manifest_semantic_validation_reports_invalid_slug() {
        let error = parse_adapter_manifest_text(
            &BASE_MANIFEST.replace("slug = \"example\"", "slug = \"Example Adapter\""),
        )
        .expect_err("invalid adapter slug should fail");

        assert!(
            error.diagnostics.iter().any(|diagnostic| diagnostic.code
                == AdapterManifestDiagnosticCode::InvalidSlug
                && diagnostic.field.as_deref() == Some("adapter.slug")),
            "{:#?}",
            error.diagnostics
        );
    }

    #[test]
    fn adapter_manifest_semantic_validation_reports_missing_profile_paths() -> anyhow::Result<()> {
        let tempdir = tempfile::tempdir()?;
        let manifest_path = tempdir.path().join("adapter.toml");
        fs::write(&manifest_path, BASE_MANIFEST)?;

        let error = load_adapter_manifest(&manifest_path)
            .expect_err("missing profile files should fail semantic validation");

        assert!(
            error.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == AdapterManifestDiagnosticCode::MissingReferencedPath
                    && diagnostic.field.as_deref() == Some("profile.loop_prompt_path")
            }),
            "{:#?}",
            error.diagnostics
        );
        Ok(())
    }

    #[test]
    fn adapter_manifest_semantic_validation_reports_duplicates() -> anyhow::Result<()> {
        let manifest: super::AdapterManifest = toml::from_str(&format!(
            r#"{BASE_MANIFEST}
[[tools]]
name = "check"
argv = ["adapter", "check"]

[[tools]]
name = "check"
argv = ["adapter", "check-again"]
"#
        ))?;

        let validation = validate_adapter_manifest_semantics(&manifest, None);

        assert!(
            validation.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == AdapterManifestDiagnosticCode::DuplicateValue
                    && diagnostic.field.as_deref() == Some("tools[1].name")
            }),
            "{:#?}",
            validation.diagnostics
        );
        Ok(())
    }

    #[test]
    fn adapter_manifest_semantic_validation_reports_unsupported_core_version() {
        let error = parse_adapter_manifest_text(
            &BASE_MANIFEST.replace("core_version = \"0.1\"", "core_version = \"9.0\""),
        )
        .expect_err("unsupported core version should fail");

        assert!(
            error.diagnostics.iter().any(|diagnostic| {
                diagnostic.code == AdapterManifestDiagnosticCode::UnsupportedCoreVersion
                    && diagnostic.field.as_deref() == Some("adapter.core_version")
            }),
            "{:#?}",
            error.diagnostics
        );
    }

    #[test]
    fn adapter_command_manifest_parses_namespaces() -> anyhow::Result<()> {
        let manifest = parse_adapter_manifest(&format!(
            r#"{BASE_MANIFEST}
[[commands]]
namespace = "example"
argv = ["ldgr-example-adapter"]
aliases = ["sample", "community"]
title = "Example commands"
description = "Adapter-owned commands exposed through the core command surface."
capabilities = ["dispatch", "help"]

[commands.help]
usage = "ldgr example <command>"
summary = "Run example adapter commands."
details = "Arguments after the namespace are forwarded to the adapter executable."

[[commands]]
namespace = "example.admin"
argv = ["ldgr-example-adapter", "admin"]
aliases = ["sample-admin"]
title = "Example admin commands"
description = "Administrative adapter commands."
capabilities = ["dispatch"]

[commands.help]
usage = "ldgr example.admin <command>"
summary = "Run example adapter admin commands."
"#
        ))?;

        assert_eq!(manifest.commands.len(), 2);
        assert_eq!(manifest.commands[0].namespace, "example");
        assert_eq!(manifest.commands[0].argv, vec!["ldgr-example-adapter"]);
        assert_eq!(manifest.commands[0].aliases, vec!["sample", "community"]);
        assert_eq!(
            manifest.commands[0].help.summary,
            "Run example adapter commands."
        );
        assert_eq!(manifest.commands[1].namespace, "example.admin");
        Ok(())
    }

    #[test]
    fn adapter_command_manifest_omits_commands_for_backward_compatibility() -> anyhow::Result<()> {
        let manifest = parse_adapter_manifest(BASE_MANIFEST)?;
        assert!(manifest.commands.is_empty());
        assert!(manifest.integrity.is_none());
        Ok(())
    }

    #[test]
    fn adapter_manifest_reports_absent_digest_state() -> anyhow::Result<()> {
        let report = parse_adapter_manifest_text(BASE_MANIFEST)?;
        assert_eq!(report.integrity.state, AdapterManifestDigestState::Absent);
        assert!(report.integrity.verified_manifest_digest.is_none());
        assert!(report.integrity.message.is_none());
        Ok(())
    }

    #[test]
    fn adapter_manifest_reports_passed_digest_state() -> anyhow::Result<()> {
        let digest = canonical_manifest_digest(BASE_MANIFEST)?;
        let report = parse_adapter_manifest_text(&format!(
            "{BASE_MANIFEST}\n[integrity]\nmanifest_digest = \"{digest}\"\n"
        ))?;
        assert_eq!(report.integrity.state, AdapterManifestDigestState::Passed);
        assert_eq!(report.integrity.verified_manifest_digest, Some(digest));
        assert!(report.integrity.message.is_none());
        Ok(())
    }

    #[test]
    fn adapter_manifest_reports_failed_digest_state_without_policy_leakage() -> anyhow::Result<()> {
        let report = parse_adapter_manifest_text(&format!(
            "{BASE_MANIFEST}\n[integrity]\nmanifest_digest = \"sha256:0000000000000000000000000000000000000000000000000000000000000000\"\n"
        ))?;
        assert_eq!(report.integrity.state, AdapterManifestDigestState::Failed);
        assert!(report.integrity.verified_manifest_digest.is_none());
        let message = report.integrity.message.unwrap_or_default();
        assert!(
            message.contains("adapter manifest digest mismatch"),
            "{message}"
        );
        assert!(!message.contains("license"), "{message}");
        assert!(!message.contains("commercial"), "{message}");
        assert!(!message.contains("entitlement"), "{message}");
        Ok(())
    }

    #[test]
    fn adapter_manifest_parses_optional_integrity_metadata() -> anyhow::Result<()> {
        let manifest = parse_adapter_manifest(&format!(
            r#"{BASE_MANIFEST}
[integrity]
manifest_digest = "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
"#
        ))?;

        assert_eq!(
            manifest
                .integrity
                .and_then(|integrity| integrity.manifest_digest),
            Some(
                "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef"
                    .to_string()
            )
        );
        Ok(())
    }

    #[test]
    fn adapter_command_manifest_rejects_invalid_namespace() {
        let error = parse_adapter_manifest(&format!(
            r#"{BASE_MANIFEST}
[[commands]]
namespace = "Example"
argv = ["adapter"]
title = "Example commands"
description = "Bad namespace."

[commands.help]
usage = "ldgr Example"
summary = "Bad namespace."
"#
        ))
        .expect_err("invalid namespace should fail");

        assert!(
            error
                .to_string()
                .contains("commands[0].namespace `Example` is invalid"),
            "{error}"
        );
    }

    #[test]
    fn adapter_command_manifest_rejects_duplicate_command_aliases() {
        let error = parse_adapter_manifest(&format!(
            r#"{BASE_MANIFEST}
[[commands]]
namespace = "example"
argv = ["adapter"]
aliases = ["same"]
title = "Example commands"
description = "First namespace."

[commands.help]
usage = "ldgr example"
summary = "First namespace."

[[commands]]
namespace = "other"
argv = ["adapter", "other"]
aliases = ["same"]
title = "Other commands"
description = "Second namespace."

[commands.help]
usage = "ldgr other"
summary = "Second namespace."
"#
        ))
        .expect_err("duplicate aliases should fail");

        assert!(
            error.to_string().contains("duplicate command alias `same`"),
            "{error}"
        );
    }

    #[test]
    fn adapter_command_manifest_rejects_missing_argv_entries() {
        let error = parse_adapter_manifest(&format!(
            r#"{BASE_MANIFEST}
[[commands]]
namespace = "example"
argv = []
title = "Example commands"
description = "No executable."

[commands.help]
usage = "ldgr example"
summary = "No executable."
"#
        ))
        .expect_err("missing argv should fail");

        assert!(
            error
                .to_string()
                .contains("commands[0].argv must contain at least one executable entry"),
            "{error}"
        );
    }

    #[test]
    fn adapter_command_manifest_rejects_malformed_declarations() {
        let error = parse_adapter_manifest(&format!(
            r#"{BASE_MANIFEST}
[[commands]]
namespace = "example"
argv = "adapter"
title = "Example commands"
description = "Malformed argv."

[commands.help]
usage = "ldgr example"
summary = "Malformed argv."
"#
        ))
        .expect_err("malformed command declaration should fail");

        assert!(
            error
                .to_string()
                .contains("failed to parse adapter manifest TOML"),
            "{error}"
        );
    }
}
