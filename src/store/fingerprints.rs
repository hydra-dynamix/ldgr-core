use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use super::{is_sha256_digest, ErrorClass};

pub const STRUCTURED_FINGERPRINT_V1: &str = "structured-v1";
pub const STRUCTURED_FINGERPRINT_SPLIT_V1: &str = "structured-v1+split-v1";

#[derive(Debug, Clone, Copy)]
pub struct FingerprintRequest<'a> {
    pub version: &'a str,
    pub supplied_fingerprint: Option<&'a str>,
    pub override_rationale: Option<&'a str>,
    pub split_key: Option<&'a str>,
    pub split_rationale: Option<&'a str>,
    pub class: ErrorClass,
    pub domain: &'a str,
    pub code: &'a str,
    pub boundary: Option<&'a str>,
    pub component: Option<&'a str>,
    pub subject: Option<&'a str>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum FingerprintProvenanceKind {
    Computed,
    Override,
    Split,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FingerprintProvenance {
    pub kind: FingerprintProvenanceKind,
    pub rationale: Option<String>,
    pub base_version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedFingerprint {
    pub version: String,
    pub fingerprint: String,
    pub inputs: Value,
    pub provenance: FingerprintProvenance,
}

pub fn resolve_fingerprint(request: FingerprintRequest<'_>) -> anyhow::Result<ResolvedFingerprint> {
    validate_identity_part("domain", request.domain)?;
    validate_identity_part("code", request.code)?;
    for (name, value) in [
        ("boundary", request.boundary),
        ("component", request.component),
        ("subject", request.subject),
    ] {
        if let Some(value) = value {
            validate_identity_part(name, value)?;
        }
    }

    let inputs = structured_inputs(&request);
    if let Some(fingerprint) = request.supplied_fingerprint {
        anyhow::ensure!(
            request.split_key.is_none(),
            "--fingerprint cannot be combined with --fingerprint-split"
        );
        let rationale = required_rationale(
            "--fingerprint-override-rationale",
            request.override_rationale,
        )?;
        anyhow::ensure!(
            is_sha256_digest(fingerprint),
            "fingerprint must be `sha256:` followed by 64 lowercase hexadecimal characters"
        );
        return Ok(ResolvedFingerprint {
            version: request.version.to_owned(),
            fingerprint: fingerprint.to_owned(),
            inputs,
            provenance: FingerprintProvenance {
                kind: FingerprintProvenanceKind::Override,
                rationale: Some(rationale.to_owned()),
                base_version: None,
            },
        });
    }

    anyhow::ensure!(
        request.version == STRUCTURED_FINGERPRINT_V1,
        "automatic fingerprinting does not support version `{}`; provide an explicit --fingerprint and --fingerprint-override-rationale to preserve custom or historical interpretation",
        request.version
    );
    if let Some(split_key) = request.split_key {
        validate_identity_part("fingerprint split key", split_key)?;
        let rationale =
            required_rationale("--fingerprint-split-rationale", request.split_rationale)?;
        let split_inputs = serde_json::json!({
            "identity": inputs,
            "split": split_key,
        });
        return Ok(ResolvedFingerprint {
            version: STRUCTURED_FINGERPRINT_SPLIT_V1.to_owned(),
            fingerprint: digest_json(&split_inputs)?,
            inputs: split_inputs,
            provenance: FingerprintProvenance {
                kind: FingerprintProvenanceKind::Split,
                rationale: Some(rationale.to_owned()),
                base_version: Some(STRUCTURED_FINGERPRINT_V1.to_owned()),
            },
        });
    }
    anyhow::ensure!(
        request.split_rationale.is_none(),
        "--fingerprint-split-rationale requires --fingerprint-split"
    );
    anyhow::ensure!(
        request.override_rationale.is_none(),
        "--fingerprint-override-rationale requires --fingerprint"
    );

    Ok(ResolvedFingerprint {
        version: STRUCTURED_FINGERPRINT_V1.to_owned(),
        fingerprint: digest_json(&inputs)?,
        inputs,
        provenance: FingerprintProvenance {
            kind: FingerprintProvenanceKind::Computed,
            rationale: None,
            base_version: None,
        },
    })
}

fn structured_inputs(request: &FingerprintRequest<'_>) -> Value {
    let mut fields = BTreeMap::new();
    fields.insert("class", Value::String(request.class.as_str().to_owned()));
    fields.insert("code", Value::String(request.code.to_owned()));
    fields.insert("domain", Value::String(request.domain.to_owned()));
    for (name, value) in [
        ("boundary", request.boundary),
        ("component", request.component),
        ("subject", request.subject),
    ] {
        if let Some(value) = value {
            fields.insert(name, Value::String(value.to_owned()));
        }
    }
    serde_json::to_value(fields).expect("string-only fingerprint inputs serialize")
}

fn digest_json(value: &Value) -> anyhow::Result<String> {
    let canonical = serde_json::to_vec(value)?;
    Ok(format!("sha256:{:x}", Sha256::digest(canonical)))
}

fn required_rationale<'a>(name: &str, value: Option<&'a str>) -> anyhow::Result<&'a str> {
    let value = value.map(str::trim).filter(|value| !value.is_empty());
    value.ok_or_else(|| anyhow::anyhow!("{name} is required and must not be empty"))
}

fn validate_identity_part(name: &str, value: &str) -> anyhow::Result<()> {
    anyhow::ensure!(!value.trim().is_empty(), "{name} must not be empty");
    anyhow::ensure!(value.len() <= 256, "{name} exceeds 256 bytes");
    anyhow::ensure!(
        !value.chars().any(char::is_control),
        "{name} must not contain control characters"
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request<'a>(summary_independent_code: &'a str) -> FingerprintRequest<'a> {
        FingerprintRequest {
            version: STRUCTURED_FINGERPRINT_V1,
            supplied_fingerprint: None,
            override_rationale: None,
            split_key: None,
            split_rationale: None,
            class: ErrorClass::InfrastructureError,
            domain: "agentctl.spawn",
            code: summary_independent_code,
            boundary: Some("worker-spawn"),
            component: Some("agentctl"),
            subject: Some("agent-worker"),
        }
    }

    #[test]
    fn structured_v1_is_deterministic_and_excludes_messages() -> anyhow::Result<()> {
        let first = resolve_fingerprint(request("worker-spawn-failed"))?;
        let second = resolve_fingerprint(request("worker-spawn-failed"))?;
        assert_eq!(first.fingerprint, second.fingerprint);
        assert_eq!(first.version, STRUCTURED_FINGERPRINT_V1);
        assert_eq!(
            first.inputs,
            serde_json::json!({
                "boundary": "worker-spawn",
                "class": "infrastructure-error",
                "code": "worker-spawn-failed",
                "component": "agentctl",
                "domain": "agentctl.spawn",
                "subject": "agent-worker",
            })
        );
        Ok(())
    }

    #[test]
    fn code_and_explicit_split_produce_distinct_versioned_identities() -> anyhow::Result<()> {
        let first = resolve_fingerprint(request("worker-spawn-failed"))?;
        let other = resolve_fingerprint(request("configuration-invalid"))?;
        assert_ne!(first.fingerprint, other.fingerprint);

        let mut split_request = request("worker-spawn-failed");
        split_request.split_key = Some("windows-profile-layout");
        split_request.split_rationale = Some("environment proves a distinct causal branch");
        let split = resolve_fingerprint(split_request)?;
        assert_ne!(first.fingerprint, split.fingerprint);
        assert_eq!(split.version, STRUCTURED_FINGERPRINT_SPLIT_V1);
        assert_eq!(split.provenance.kind, FingerprintProvenanceKind::Split);
        Ok(())
    }

    #[test]
    fn supplied_fingerprints_require_explicit_rationale() {
        let mut supplied = request("worker-spawn-failed");
        let fingerprint = format!("sha256:{}", "a".repeat(64));
        supplied.supplied_fingerprint = Some(&fingerprint);
        assert!(resolve_fingerprint(supplied)
            .unwrap_err()
            .to_string()
            .contains("--fingerprint-override-rationale"));
    }
}
