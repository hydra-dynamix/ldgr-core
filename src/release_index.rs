use std::collections::HashMap;

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};

pub const ADAPTER_RELEASE_INDEX_SCHEMA_VERSION: u32 = 1;

pub fn parse_release_index(json: &str) -> anyhow::Result<AdapterReleaseIndex> {
    let index: AdapterReleaseIndex =
        serde_json::from_str(json).context("failed to parse adapter release index JSON")?;
    validate_release_index(&index)?;
    Ok(index)
}

pub fn validate_release_index(index: &AdapterReleaseIndex) -> anyhow::Result<()> {
    if index.schema_version != ADAPTER_RELEASE_INDEX_SCHEMA_VERSION {
        bail!(
            "unsupported adapter release index schema_version {}; expected {}",
            index.schema_version,
            ADAPTER_RELEASE_INDEX_SCHEMA_VERSION
        );
    }
    if index.adapters.is_empty() {
        bail!("adapter release index must contain at least one adapter");
    }
    let mut identifiers = HashMap::<&str, String>::new();
    for (adapter_index, adapter) in index.adapters.iter().enumerate() {
        require_text(
            &adapter.domain,
            &format!("adapters[{adapter_index}].domain"),
        )?;
        validate_identifier(
            &adapter.domain,
            &format!("adapters[{adapter_index}].domain"),
        )?;
        if adapter.primary_namespace != adapter.domain {
            bail!(
                "adapters[{adapter_index}].primary_namespace must equal canonical domain `{}`",
                adapter.domain
            );
        }
        register_identifier(
            &mut identifiers,
            &adapter.domain,
            &format!("adapters[{adapter_index}].domain"),
        )?;
        for (alias_index, alias) in adapter.aliases.iter().enumerate() {
            let field = format!("adapters[{adapter_index}].aliases[{alias_index}]");
            validate_identifier(alias, &field)?;
            register_identifier(&mut identifiers, alias, &field)?;
        }
        require_text(&adapter.title, &format!("adapters[{adapter_index}].title"))?;
        if adapter.releases.is_empty() {
            bail!("adapters[{adapter_index}].releases must not be empty");
        }
        for (release_index, release) in adapter.releases.iter().enumerate() {
            let path = format!("adapters[{adapter_index}].releases[{release_index}]");
            require_text(&release.version, &format!("{path}.version"))?;
            require_text(
                &release.core_compatibility,
                &format!("{path}.core_compatibility"),
            )?;
            if release.platforms.is_empty() {
                bail!("{path}.platforms must not be empty");
            }
            for (platform_index, platform) in release.platforms.iter().enumerate() {
                let platform_path = format!("{path}.platforms[{platform_index}]");
                require_text(&platform.platform, &format!("{platform_path}.platform"))?;
                require_text(&platform.asset_url, &format!("{platform_path}.asset_url"))?;
                require_text(
                    &platform.archive_root,
                    &format!("{platform_path}.archive_root"),
                )?;
                require_text(&platform.binary, &format!("{platform_path}.binary"))?;
                require_text(&platform.sha256, &format!("{platform_path}.sha256"))?;
                require_text(
                    &platform.signature_url,
                    &format!("{platform_path}.signature_url"),
                )?;
                require_text(
                    &platform.signing_key_id,
                    &format!("{platform_path}.signing_key_id"),
                )?;
                require_text(
                    &platform.resource_manifest,
                    &format!("{platform_path}.resource_manifest"),
                )?;
            }
        }
    }
    Ok(())
}

fn validate_identifier(value: &str, field: &str) -> anyhow::Result<()> {
    let mut chars = value.chars();
    let valid = matches!(chars.next(), Some(first) if first.is_ascii_lowercase())
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if !valid || value.starts_with("ldgr-") || value.ends_with('-') || value.contains("--") {
        bail!("{field} `{value}` is invalid; expected a canonical lowercase domain without an `ldgr-` executable prefix");
    }
    Ok(())
}

fn register_identifier<'a>(
    identifiers: &mut HashMap<&'a str, String>,
    value: &'a str,
    field: &str,
) -> anyhow::Result<()> {
    if let Some(existing) = identifiers.insert(value, field.to_owned()) {
        bail!("{field} `{value}` collides with {existing}");
    }
    Ok(())
}

fn require_text(value: &str, field: &str) -> anyhow::Result<()> {
    if value.trim().is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdapterReleaseIndex {
    pub schema_version: u32,
    pub adapters: Vec<AdapterReleaseProduct>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdapterReleaseProduct {
    pub domain: String,
    pub primary_namespace: String,
    pub title: String,
    #[serde(default)]
    pub aliases: Vec<String>,
    pub classification: AdapterClassification,
    #[serde(default)]
    pub source_url: Option<String>,
    pub releases: Vec<AdapterRelease>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterClassification {
    OpenSource,
    Commercial,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdapterRelease {
    pub version: String,
    pub channel: ReleaseChannel,
    pub core_compatibility: String,
    pub platforms: Vec<AdapterPlatformRelease>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ReleaseChannel {
    Stable,
    Prerelease,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdapterPlatformRelease {
    pub platform: String,
    pub asset_url: String,
    pub archive_root: String,
    pub binary: String,
    pub sha256: String,
    pub signature_url: String,
    pub signing_key_id: String,
    pub resource_manifest: String,
}

#[cfg(test)]
mod tests {
    use super::{parse_release_index, AdapterClassification, ReleaseChannel};

    const OPEN_AND_COMMERCIAL: &str =
        include_str!("../tests/fixtures/release-index/open-and-commercial.json");

    #[test]
    fn parses_open_and_commercial_release_entries() -> anyhow::Result<()> {
        let index = parse_release_index(OPEN_AND_COMMERCIAL)?;
        assert_eq!(index.adapters.len(), 2);
        assert_eq!(
            index.adapters[0].classification,
            AdapterClassification::OpenSource
        );
        assert_eq!(
            index.adapters[1].classification,
            AdapterClassification::Commercial
        );
        assert_eq!(
            index.adapters[0].releases[0].channel,
            ReleaseChannel::Stable
        );
        Ok(())
    }

    #[test]
    fn rejects_missing_required_fields() {
        let error = parse_release_index(include_str!(
            "../tests/fixtures/release-index/missing-signature-url.json"
        ))
        .expect_err("missing signature_url must fail");
        assert!(format!("{error:#}").contains("signature_url"));
    }

    #[test]
    fn rejects_empty_required_fields() {
        let invalid = OPEN_AND_COMMERCIAL.replace(
            "https://github.com/hydra-dynamix/ldgr-example-adapter/releases/download/v0.1.4/ldgr-example-adapter-0.1.4-linux-aarch64.tar.gz",
            "",
        );
        let error = parse_release_index(&invalid).expect_err("empty asset URL must fail");
        assert!(error.to_string().contains("asset_url"));
    }

    #[test]
    fn rejects_unknown_schema_versions() {
        let invalid =
            OPEN_AND_COMMERCIAL.replacen("\"schema_version\": 1", "\"schema_version\": 2", 1);
        let error = parse_release_index(&invalid).expect_err("unknown schema must fail");
        assert!(error
            .to_string()
            .contains("unsupported adapter release index"));
    }

    #[test]
    fn rejects_duplicate_domains_and_alias_collisions() {
        let duplicate = OPEN_AND_COMMERCIAL
            .replace("\"domain\": \"evidence\"", "\"domain\": \"example\"")
            .replace(
                "\"primary_namespace\": \"evidence\"",
                "\"primary_namespace\": \"example\"",
            );
        assert!(format!("{:#}", parse_release_index(&duplicate).unwrap_err()).contains("collides"));
        let collision =
            OPEN_AND_COMMERCIAL.replace("\"aliases\": []", "\"aliases\": [\"reference\"]");
        assert!(format!("{:#}", parse_release_index(&collision).unwrap_err()).contains("collides"));
    }

    #[test]
    fn rejects_executable_style_domain_and_namespace_mismatch() {
        let executable = OPEN_AND_COMMERCIAL
            .replace("\"domain\": \"example\"", "\"domain\": \"ldgr-example\"")
            .replace(
                "\"primary_namespace\": \"example\"",
                "\"primary_namespace\": \"ldgr-example\"",
            );
        assert!(
            format!("{:#}", parse_release_index(&executable).unwrap_err())
                .contains("executable prefix")
        );
        let mismatch = OPEN_AND_COMMERCIAL.replacen(
            "\"primary_namespace\": \"example\"",
            "\"primary_namespace\": \"sample\"",
            1,
        );
        assert!(format!("{:#}", parse_release_index(&mismatch).unwrap_err()).contains("must equal"));
    }
}
