use std::collections::HashSet;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

pub fn parse_adapter_manifest(manifest_text: &str) -> anyhow::Result<AdapterManifest> {
    let manifest: AdapterManifest =
        toml::from_str(manifest_text).context("failed to parse adapter manifest TOML")?;
    validate_adapter_manifest(&manifest)?;
    Ok(manifest)
}

pub fn validate_adapter_manifest(manifest: &AdapterManifest) -> anyhow::Result<()> {
    let mut seen_command_aliases = HashSet::new();
    for (index, command) in manifest.commands.iter().enumerate() {
        validate_namespace(index, &command.namespace)?;
        validate_argv(index, &command.argv)?;
        for (alias_index, alias) in command.aliases.iter().enumerate() {
            let trimmed = alias.trim();
            if trimmed.is_empty() {
                bail!("commands[{index}].aliases[{alias_index}] must not be empty");
            }
            if !seen_command_aliases.insert(trimmed.to_string()) {
                bail!("duplicate command alias `{trimmed}` in commands[{index}].aliases");
            }
        }
    }
    Ok(())
}

fn validate_namespace(index: usize, namespace: &str) -> anyhow::Result<()> {
    if !is_valid_namespace(namespace) {
        bail!(
            "commands[{index}].namespace `{namespace}` is invalid; expected lowercase dot-separated identifier segments using letters, digits, and hyphens"
        );
    }
    Ok(())
}

fn is_valid_namespace(namespace: &str) -> bool {
    !namespace.is_empty()
        && namespace.split('.').all(|segment| {
            let mut chars = segment.chars();
            matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
                && chars.all(|character| {
                    character.is_ascii_lowercase() || character.is_ascii_digit() || character == '-'
                })
        })
}

fn validate_argv(index: usize, argv: &[String]) -> anyhow::Result<()> {
    if argv.is_empty() {
        bail!("commands[{index}].argv must contain at least one executable entry");
    }
    for (entry_index, entry) in argv.iter().enumerate() {
        if entry.trim().is_empty() {
            bail!("commands[{index}].argv[{entry_index}] must not be empty");
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct AdapterManifest {
    pub adapter: ManifestAdapter,
    pub profile: ManifestProfile,
    #[serde(default)]
    pub tools: Vec<ManifestTool>,
    #[serde(default)]
    pub commands: Vec<ManifestCommandNamespace>,
    #[serde(default)]
    pub target_profiles: Vec<ManifestTargetProfile>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestAdapter {
    pub slug: String,
    pub title: String,
    pub core_version: String,
    #[serde(default)]
    pub aliases: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestProfile {
    pub loop_prompt_path: String,
    pub default_milestone_template: String,
    pub spec_artifact_path: String,
    pub readiness_policy: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestTool {
    pub name: String,
    pub argv: Vec<String>,
    pub description: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestCommandNamespace {
    pub namespace: String,
    pub argv: Vec<String>,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub title: String,
    pub description: String,
    pub help: ManifestCommandHelp,
    #[serde(default)]
    pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestCommandHelp {
    pub usage: String,
    pub summary: String,
    #[serde(default)]
    pub details: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestTargetProfile {
    pub slug: String,
    pub title: String,
    pub target_type: String,
    pub description: String,
    #[serde(default)]
    pub probes: Vec<ManifestProbeFamily>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct ManifestProbeFamily {
    pub slug: String,
    pub title: String,
    pub description: String,
    pub evidence_artifact_kind: Option<String>,
    pub expectation_template: Option<String>,
    pub validation_hint: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::parse_adapter_manifest;

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
