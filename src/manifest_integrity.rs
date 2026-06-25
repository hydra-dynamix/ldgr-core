use std::collections::BTreeMap;

use anyhow::{bail, Context};
use serde_json::{Map as JsonMap, Number as JsonNumber, Value as JsonValue};
use sha2::{Digest, Sha256};
use toml::Value as TomlValue;

const DIGEST_ALGORITHM: &str = "sha256";
const INTEGRITY_TABLE: &str = "integrity";
const MANIFEST_DIGEST_KEY: &str = "manifest_digest";

pub fn canonical_manifest_digest(manifest_text: &str) -> anyhow::Result<String> {
    let value = canonical_manifest_value(manifest_text)?;
    let canonical = serde_json::to_vec(&value).context("failed to serialize canonical manifest")?;
    Ok(format!("{DIGEST_ALGORITHM}:{}", hex_sha256(&canonical)))
}

pub fn verify_manifest_digest(manifest_text: &str) -> anyhow::Result<Option<String>> {
    let value: TomlValue = manifest_text
        .parse()
        .context("failed to parse manifest for digest verification")?;
    let Some(expected) = manifest_digest_metadata(&value)? else {
        return Ok(None);
    };
    let calculated = canonical_manifest_digest(manifest_text)?;
    if normalize_expected_digest(&expected)? != calculated {
        bail!("adapter manifest digest mismatch: expected {expected}, calculated {calculated}");
    }
    Ok(Some(calculated))
}

fn canonical_manifest_value(manifest_text: &str) -> anyhow::Result<JsonValue> {
    let mut value: TomlValue = manifest_text
        .parse()
        .context("failed to parse manifest for canonical digest")?;
    remove_manifest_digest_metadata(&mut value);
    canonical_json_value(&value)
}

fn manifest_digest_metadata(value: &TomlValue) -> anyhow::Result<Option<String>> {
    let Some(integrity) = value.get(INTEGRITY_TABLE) else {
        return Ok(None);
    };
    let Some(digest) = integrity.get(MANIFEST_DIGEST_KEY) else {
        return Ok(None);
    };
    digest
        .as_str()
        .map(|digest| Some(digest.to_string()))
        .with_context(|| format!("{INTEGRITY_TABLE}.{MANIFEST_DIGEST_KEY} must be a string"))
}

fn remove_manifest_digest_metadata(value: &mut TomlValue) {
    let TomlValue::Table(table) = value else {
        return;
    };
    let remove_integrity = table
        .get_mut(INTEGRITY_TABLE)
        .and_then(TomlValue::as_table_mut)
        .map(|integrity| {
            integrity.remove(MANIFEST_DIGEST_KEY);
            integrity.is_empty()
        })
        .unwrap_or(false);
    if remove_integrity {
        table.remove(INTEGRITY_TABLE);
    }
}

fn canonical_json_value(value: &TomlValue) -> anyhow::Result<JsonValue> {
    match value {
        TomlValue::String(value) => Ok(JsonValue::String(value.clone())),
        TomlValue::Integer(value) => Ok(JsonValue::Number(JsonNumber::from(*value))),
        TomlValue::Float(value) => JsonNumber::from_f64(*value)
            .map(JsonValue::Number)
            .context("manifest contains a non-finite float"),
        TomlValue::Boolean(value) => Ok(JsonValue::Bool(*value)),
        TomlValue::Datetime(value) => Ok(JsonValue::String(value.to_string())),
        TomlValue::Array(values) => values
            .iter()
            .map(canonical_json_value)
            .collect::<anyhow::Result<Vec<_>>>()
            .map(JsonValue::Array),
        TomlValue::Table(values) => {
            let sorted = values
                .iter()
                .map(|(key, value)| Ok((key.clone(), canonical_json_value(value)?)))
                .collect::<anyhow::Result<BTreeMap<_, _>>>()?;
            Ok(JsonValue::Object(JsonMap::from_iter(sorted)))
        }
    }
}

fn normalize_expected_digest(expected: &str) -> anyhow::Result<String> {
    let trimmed = expected.trim();
    if let Some((algorithm, digest)) = trimmed.split_once(':') {
        if algorithm != DIGEST_ALGORITHM {
            bail!(
                "unsupported adapter manifest digest algorithm {algorithm}; expected {DIGEST_ALGORITHM}"
            );
        }
        return Ok(format!(
            "{DIGEST_ALGORITHM}:{}",
            digest.to_ascii_lowercase()
        ));
    }
    if trimmed.len() == 64
        && trimmed
            .chars()
            .all(|character| character.is_ascii_hexdigit())
    {
        return Ok(format!(
            "{DIGEST_ALGORITHM}:{}",
            trimmed.to_ascii_lowercase()
        ));
    }
    bail!(
        "invalid adapter manifest digest {expected}; expected {DIGEST_ALGORITHM}:<64 hex characters>"
    )
}

fn hex_sha256(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

#[cfg(test)]
mod tests {
    use super::{canonical_manifest_digest, verify_manifest_digest};

    const MANIFEST_A: &str = r#"
[adapter]
slug = "example"
title = "Example adapter"
core_version = "0.1"
aliases = ["example", "reference"]

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "Evidence must pass."
"#;

    const MANIFEST_B: &str = r#"
# Same manifest with comments and reordered keys.
[profile]
readiness_policy = "Evidence must pass."
spec_artifact_path = "templates/spec.md"
default_milestone_template = "templates/milestones.md"
loop_prompt_path = "prompts/loop.md"

[adapter]
aliases = [
  "example",
  "reference",
]
core_version = "0.1"
title = "Example adapter"
slug = "example"
"#;

    #[test]
    fn canonical_digest_is_stable_for_equivalent_manifest_toml() -> anyhow::Result<()> {
        assert_eq!(
            canonical_manifest_digest(MANIFEST_A)?,
            canonical_manifest_digest(MANIFEST_B)?
        );
        Ok(())
    }

    #[test]
    fn digest_metadata_is_excluded_from_canonical_digest() -> anyhow::Result<()> {
        let digest = canonical_manifest_digest(MANIFEST_A)?;
        let signed = format!("{MANIFEST_A}\n[integrity]\nmanifest_digest = \"{digest}\"\n");
        assert_eq!(digest, canonical_manifest_digest(&signed)?);
        assert_eq!(Some(digest), verify_manifest_digest(&signed)?);
        Ok(())
    }

    #[test]
    fn tampered_manifest_content_is_rejected_with_clear_error() -> anyhow::Result<()> {
        let digest = canonical_manifest_digest(MANIFEST_A)?;
        let tampered = format!(
            "{}\n[integrity]\nmanifest_digest = \"{digest}\"\n",
            MANIFEST_A.replace("Example adapter", "Tampered adapter")
        );
        let error = verify_manifest_digest(&tampered).expect_err("tamper should fail");
        let message = error.to_string();
        assert!(
            message.contains("adapter manifest digest mismatch"),
            "{message}"
        );
        assert!(message.contains("expected sha256:"), "{message}");
        assert!(message.contains("calculated sha256:"), "{message}");
        Ok(())
    }
}
