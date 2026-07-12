use std::collections::BTreeMap;
use std::path::PathBuf;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

pub const HARNESS_CONFIG_SCHEMA_VERSION: u32 = 1;

pub fn parse_harness_config(text: &str) -> anyhow::Result<HarnessConfig> {
    let config: HarnessConfig =
        serde_json::from_str(text).context("failed to parse LDGR harness config JSON")?;
    if config.schema_version != HARNESS_CONFIG_SCHEMA_VERSION {
        bail!(
            "unsupported LDGR harness config schema_version {}; expected {}",
            config.schema_version,
            HARNESS_CONFIG_SCHEMA_VERSION
        );
    }
    Ok(config)
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct HarnessConfig {
    pub schema_version: u32,
    #[serde(default)]
    pub default_harness: Option<String>,
    #[serde(default)]
    pub selected_harnesses: Vec<String>,
    #[serde(default)]
    pub installed: Vec<InstalledHarness>,
    #[serde(flatten)]
    pub extensions: BTreeMap<String, serde_json::Value>,
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
    use super::{parse_harness_config, HarnessResourceKind};

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
}
