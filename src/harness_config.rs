use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

pub const HARNESS_CONFIG_SCHEMA_VERSION: u32 = 1;

pub fn parse_harness_config(text: &str) -> anyhow::Result<HarnessConfig> {
    parse_harness_config_json(text)
}

pub fn parse_harness_config_json(text: &str) -> anyhow::Result<HarnessConfig> {
    let config: HarnessConfig =
        serde_json::from_str(text).context("failed to parse LDGR harness config JSON")?;
    validate_harness_config(config)
}

pub fn parse_harness_config_toml(text: &str) -> anyhow::Result<HarnessConfig> {
    let config: HarnessConfig =
        toml::from_str(text).context("failed to parse LDGR harness config TOML")?;
    validate_harness_config(config)
}

fn validate_harness_config(config: HarnessConfig) -> anyhow::Result<HarnessConfig> {
    if config.schema_version != HARNESS_CONFIG_SCHEMA_VERSION {
        bail!(
            "unsupported LDGR harness config schema_version {}; expected {}",
            config.schema_version,
            HARNESS_CONFIG_SCHEMA_VERSION
        );
    }
    Ok(config)
}

/// How much the agent interrogates the operator before writing a spec.
///
/// Requirements elicitation is the one part of the core workflow that depends
/// on operator preference rather than project state, so it is configuration
/// rather than something the agent should infer per session.
#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum InterviewDepth {
    /// Full interview, one question at a time, transcript stored as an artifact.
    High,
    /// Up to ten questions, answers stored as an artifact.
    #[default]
    Medium,
    /// The five most important questions, answers stored as observations.
    Low,
    /// Ask nothing; infer requirements and record the assumptions made.
    None,
}

impl InterviewDepth {
    pub const VALUES: [Self; 4] = [Self::High, Self::Medium, Self::Low, Self::None];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::High => "high",
            Self::Medium => "medium",
            Self::Low => "low",
            Self::None => "none",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "high" => Some(Self::High),
            "medium" => Some(Self::Medium),
            "low" => Some(Self::Low),
            "none" => Some(Self::None),
            _ => None,
        }
    }

    /// One-line description used by the installer prompt and `ldgr workflow`.
    pub fn describe(self) -> &'static str {
        match self {
            Self::High => "full interview, one question at a time, recorded as an artifact",
            Self::Medium => "up to ten questions, answers recorded as an artifact",
            Self::Low => "five key questions, answers recorded as observations",
            Self::None => "ask nothing; infer requirements and record the assumptions",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HarnessConfig {
    #[serde(default = "default_schema_version")]
    pub schema_version: u32,
    #[serde(default)]
    pub default_harness: Option<String>,
    /// How deep a requirements interview the agent should conduct.
    #[serde(default)]
    pub interview_depth: InterviewDepth,
    /// User-level update discovery and notification preferences.
    #[serde(default)]
    pub updates: UpdateConfig,
    #[serde(default)]
    pub selected_harnesses: Vec<String>,
    #[serde(default)]
    pub installed: Vec<InstalledHarness>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl Default for HarnessConfig {
    fn default() -> Self {
        Self {
            schema_version: HARNESS_CONFIG_SCHEMA_VERSION,
            default_harness: None,
            interview_depth: InterviewDepth::default(),
            updates: UpdateConfig::default(),
            selected_harnesses: Vec::new(),
            installed: Vec::new(),
            extensions: BTreeMap::new(),
        }
    }
}

fn default_schema_version() -> u32 {
    HARNESS_CONFIG_SCHEMA_VERSION
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct InstalledHarness {
    pub harness: String,
    #[serde(default)]
    pub prompt_paths: Vec<PathBuf>,
    #[serde(default)]
    pub skill_paths: Vec<PathBuf>,
    #[serde(default)]
    pub extension_paths: Vec<PathBuf>,
    #[serde(default)]
    pub command_paths: Vec<PathBuf>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateCheck {
    #[default]
    Startup,
    Never,
}

impl UpdateCheck {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Startup => "startup",
            Self::Never => "never",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "startup" => Some(Self::Startup),
            "never" => Some(Self::Never),
            _ => None,
        }
    }
}

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum UpdateChannel {
    #[default]
    Stable,
    Prerelease,
}

impl UpdateChannel {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Stable => "stable",
            Self::Prerelease => "prerelease",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value.trim().to_ascii_lowercase().as_str() {
            "stable" => Some(Self::Stable),
            "prerelease" => Some(Self::Prerelease),
            _ => None,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
pub struct UpdateConfig {
    #[serde(default)]
    pub check: UpdateCheck,
    #[serde(default = "default_update_interval_hours")]
    pub interval_hours: u64,
    #[serde(default)]
    pub channel: UpdateChannel,
    #[serde(default = "default_true")]
    pub include_adapters: bool,
    #[serde(default = "default_true")]
    pub notify: bool,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
}

impl Default for UpdateConfig {
    fn default() -> Self {
        Self {
            check: UpdateCheck::default(),
            interval_hours: default_update_interval_hours(),
            channel: UpdateChannel::default(),
            include_adapters: true,
            notify: true,
            extensions: BTreeMap::new(),
        }
    }
}

const fn default_update_interval_hours() -> u64 {
    24
}

const fn default_true() -> bool {
    true
}

impl HarnessConfig {
    pub fn resource_paths(&self, kind: HarnessResourceKind) -> Vec<&PathBuf> {
        self.installed
            .iter()
            .flat_map(|harness| match kind {
                HarnessResourceKind::Prompt => &harness.prompt_paths,
                HarnessResourceKind::Skill => &harness.skill_paths,
                HarnessResourceKind::Extension => &harness.extension_paths,
                HarnessResourceKind::Command => &harness.command_paths,
            })
            .collect()
    }

    pub fn harness_resource_paths(
        &self,
        harness_name: &str,
        kind: HarnessResourceKind,
    ) -> Vec<&PathBuf> {
        self.installed
            .iter()
            .filter(|harness| harness.harness == harness_name)
            .flat_map(|harness| match kind {
                HarnessResourceKind::Prompt => &harness.prompt_paths,
                HarnessResourceKind::Skill => &harness.skill_paths,
                HarnessResourceKind::Extension => &harness.extension_paths,
                HarnessResourceKind::Command => &harness.command_paths,
            })
            .collect()
    }
}

#[derive(Clone, Copy, Debug)]
pub enum HarnessResourceKind {
    Prompt,
    Skill,
    Extension,
    Command,
}

#[cfg(test)]
mod tests {
    use super::{
        parse_harness_config, parse_harness_config_toml, HarnessResourceKind, UpdateChannel,
        UpdateCheck,
    };

    #[test]
    fn parses_current_schema_without_losing_harnesses_or_paths() -> anyhow::Result<()> {
        let config = parse_harness_config(
            r#"{
              "schema_version": 1,
              "default_harness": "pi",
              "selected_harnesses": ["pi", "codex"],
              "installed": [
                {"harness":"pi","extension_paths":["/tmp/pi.ts"],"skill_paths":["/tmp/pi-skills"],"reload":"ignored extension"},
                {"harness":"codex","prompt_paths":["/tmp/prompts"],"skill_paths":["/tmp/codex-skills"]}
              ],
              "agentctl": {"status":"installed"}
            }"#,
        )?;
        assert_eq!(config.selected_harnesses, ["pi", "codex"]);
        assert_eq!(config.resource_paths(HarnessResourceKind::Skill).len(), 2);
        assert_eq!(config.resource_paths(HarnessResourceKind::Prompt).len(), 1);
        assert_eq!(config.updates.check, UpdateCheck::Startup);
        assert_eq!(config.updates.interval_hours, 24);
        assert_eq!(config.updates.channel, UpdateChannel::Stable);
        assert!(config.updates.include_adapters);
        assert!(config.updates.notify);
        assert_eq!(config.extensions["agentctl"]["status"], "installed");
        Ok(())
    }

    #[test]
    fn rejects_unknown_schema_versions() {
        let error =
            parse_harness_config(r#"{"schema_version":99,"selected_harnesses":[],"installed":[]}"#)
                .expect_err("unknown schema must fail");
        assert!(error
            .to_string()
            .contains("unsupported LDGR harness config"));
    }

    #[test]
    fn parses_canonical_toml_harness_selection() -> anyhow::Result<()> {
        let config = parse_harness_config_toml(
            r#"
schema_version = 1
default_harness = "codex"
selected_harnesses = ["codex", "claude"]
interview_depth = "low"
"#,
        )?;

        assert_eq!(config.default_harness.as_deref(), Some("codex"));
        assert_eq!(config.selected_harnesses, ["codex", "claude"]);
        assert_eq!(config.interview_depth.as_str(), "low");
        assert_eq!(config.updates.check, UpdateCheck::Startup);
        assert_eq!(config.updates.interval_hours, 24);
        assert_eq!(config.updates.channel, UpdateChannel::Stable);
        assert!(config.updates.include_adapters);
        assert!(config.updates.notify);
        Ok(())
    }

    #[test]
    fn parses_update_preferences_and_preserves_nested_extensions() -> anyhow::Result<()> {
        let config = parse_harness_config_toml(
            r#"
schema_version = 1
future_top_level = "preserved"

[updates]
check = "never"
interval_hours = 12
channel = "prerelease"
include_adapters = false
notify = false
enterprise_catalog = "mirror"
"#,
        )?;

        assert_eq!(config.updates.check, UpdateCheck::Never);
        assert_eq!(config.updates.interval_hours, 12);
        assert_eq!(config.updates.channel, UpdateChannel::Prerelease);
        assert!(!config.updates.include_adapters);
        assert!(!config.updates.notify);
        assert_eq!(config.extensions["future_top_level"], "preserved");
        assert_eq!(config.updates.extensions["enterprise_catalog"], "mirror");

        let serialized = toml::to_string_pretty(&config)?;
        let reparsed = parse_harness_config_toml(&serialized)?;
        assert_eq!(reparsed.schema_version, 1);
        assert_eq!(reparsed.extensions["future_top_level"], "preserved");
        assert_eq!(reparsed.updates.extensions["enterprise_catalog"], "mirror");
        Ok(())
    }
}
