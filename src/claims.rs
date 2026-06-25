use std::{fs, path::Path};

use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use thiserror::Error;

pub const CLAIM_SIGNATURE_ALGORITHM_ED25519: &str = "Ed25519";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimSigningKey {
    pub key_id: Option<String>,
    pub public_key: [u8; 32],
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SignedClaimsFile {
    pub alg: String,
    #[serde(default)]
    pub key_id: Option<String>,
    pub claims: Value,
    pub signature: String,
}

#[derive(Debug, Clone, PartialEq)]
pub struct VerifiedClaims {
    pub key_id: Option<String>,
    pub claims: Value,
}

#[derive(Debug, Error)]
pub enum ClaimVerificationError {
    #[error("failed to read signed claims file {path}: {source}")]
    Read {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("signed claims file is not valid JSON: {0}")]
    Parse(#[from] serde_json::Error),
    #[error("unsupported claims signature algorithm {0}")]
    UnsupportedAlgorithm(String),
    #[error("no verification key matched key_id {0}")]
    UnknownKey(String),
    #[error("verification key is not a valid Ed25519 public key")]
    InvalidPublicKey,
    #[error("signature is not valid base64: {0}")]
    InvalidSignatureEncoding(#[from] base64::DecodeError),
    #[error("signature must be 64 bytes")]
    InvalidSignatureLength,
    #[error("claims signature did not verify")]
    InvalidSignature,
}

#[derive(Debug, Serialize)]
struct SignedClaimsPayload<'a> {
    alg: &'a str,
    #[serde(skip_serializing_if = "Option::is_none")]
    key_id: Option<&'a str>,
    claims: &'a Value,
}

pub fn verify_signed_claims_file(
    path: impl AsRef<Path>,
    trusted_keys: &[ClaimSigningKey],
) -> Result<VerifiedClaims, ClaimVerificationError> {
    let path = path.as_ref();
    let contents = fs::read_to_string(path).map_err(|source| ClaimVerificationError::Read {
        path: path.display().to_string(),
        source,
    })?;
    verify_signed_claims_str(&contents, trusted_keys)
}

pub fn verify_signed_claims_str(
    contents: &str,
    trusted_keys: &[ClaimSigningKey],
) -> Result<VerifiedClaims, ClaimVerificationError> {
    let signed: SignedClaimsFile = serde_json::from_str(contents)?;
    verify_signed_claims(signed, trusted_keys)
}

pub fn verify_signed_claims(
    signed: SignedClaimsFile,
    trusted_keys: &[ClaimSigningKey],
) -> Result<VerifiedClaims, ClaimVerificationError> {
    if signed.alg != CLAIM_SIGNATURE_ALGORITHM_ED25519 {
        return Err(ClaimVerificationError::UnsupportedAlgorithm(signed.alg));
    }

    let signature_bytes = STANDARD.decode(&signed.signature)?;
    let signature: [u8; 64] = signature_bytes
        .try_into()
        .map_err(|_| ClaimVerificationError::InvalidSignatureLength)?;
    let signature = Signature::from_bytes(&signature);
    let payload = signed_payload_bytes(&signed)?;
    let candidate_keys = matching_keys(&signed, trusted_keys)?;

    let mut saw_invalid_public_key = false;
    for key in candidate_keys {
        match VerifyingKey::from_bytes(&key.public_key) {
            Ok(verifying_key) if verifying_key.verify(&payload, &signature).is_ok() => {
                return Ok(VerifiedClaims {
                    key_id: signed.key_id,
                    claims: signed.claims,
                });
            }
            Ok(_) => {}
            Err(_) => saw_invalid_public_key = true,
        }
    }

    if saw_invalid_public_key {
        Err(ClaimVerificationError::InvalidPublicKey)
    } else {
        Err(ClaimVerificationError::InvalidSignature)
    }
}

fn matching_keys<'a>(
    signed: &SignedClaimsFile,
    trusted_keys: &'a [ClaimSigningKey],
) -> Result<Vec<&'a ClaimSigningKey>, ClaimVerificationError> {
    match signed.key_id.as_deref() {
        Some(key_id) => {
            let keys = trusted_keys
                .iter()
                .filter(|key| key.key_id.as_deref() == Some(key_id))
                .collect::<Vec<_>>();
            if keys.is_empty() {
                Err(ClaimVerificationError::UnknownKey(key_id.to_owned()))
            } else {
                Ok(keys)
            }
        }
        None if trusted_keys.is_empty() => Err(ClaimVerificationError::UnknownKey(
            "<unkeyed claims>".to_owned(),
        )),
        None => Ok(trusted_keys.iter().collect()),
    }
}

fn signed_payload_bytes(signed: &SignedClaimsFile) -> Result<Vec<u8>, serde_json::Error> {
    let payload = SignedClaimsPayload {
        alg: &signed.alg,
        key_id: signed.key_id.as_deref(),
        claims: &signed.claims,
    };
    serde_json::to_vec(&payload)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ed25519_dalek::{Signer, SigningKey};

    fn signed_claims(claims: Value) -> (SignedClaimsFile, ClaimSigningKey) {
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        let mut signed = SignedClaimsFile {
            alg: CLAIM_SIGNATURE_ALGORITHM_ED25519.to_owned(),
            key_id: Some("test-key".to_owned()),
            claims,
            signature: String::new(),
        };
        let signature = signing_key.sign(&signed_payload_bytes(&signed).unwrap());
        signed.signature = STANDARD.encode(signature.to_bytes());
        let key = ClaimSigningKey {
            key_id: signed.key_id.clone(),
            public_key: signing_key.verifying_key().to_bytes(),
        };
        (signed, key)
    }

    #[test]
    fn verifies_offline_signed_claims_and_returns_claim_json() {
        let claims = serde_json::json!({
            "subject": "adapter-owned-subject",
            "features": ["adapter-decides-this"],
            "product": "adapter-decides-this-too",
            "version_family": "2026"
        });
        let (signed, key) = signed_claims(claims.clone());

        let verified = verify_signed_claims(signed, &[key]).unwrap();

        assert_eq!(verified.key_id.as_deref(), Some("test-key"));
        assert_eq!(verified.claims, claims);
        assert_eq!(
            verified.claims["features"][0], "adapter-decides-this",
            "core returns claims without entitlement decisions"
        );
    }

    #[test]
    fn rejects_tampered_claims() {
        let (mut signed, key) = signed_claims(serde_json::json!({
            "subject": "adapter-owned-subject",
            "allowed": false
        }));
        signed.claims["allowed"] = Value::Bool(true);

        let error = verify_signed_claims(signed, &[key]).unwrap_err();

        assert!(matches!(error, ClaimVerificationError::InvalidSignature));
    }

    #[test]
    fn parses_and_verifies_claims_file() {
        let (signed, key) = signed_claims(serde_json::json!({
            "custom_entitlement_shape": {
                "adapter": "owns-policy"
            }
        }));
        let temp_dir = tempfile::tempdir().unwrap();
        let path = temp_dir.path().join("license.claims.json");
        fs::write(&path, serde_json::to_string_pretty(&signed).unwrap()).unwrap();

        let verified = verify_signed_claims_file(&path, &[key]).unwrap();

        assert_eq!(
            verified.claims["custom_entitlement_shape"]["adapter"],
            "owns-policy"
        );
    }

    #[test]
    fn unknown_key_id_fails_before_policy_interpretation() {
        let (signed, key) = signed_claims(serde_json::json!({
            "feature": "adapter-specific"
        }));
        let trusted_key = ClaimSigningKey {
            key_id: Some("different-key".to_owned()),
            public_key: key.public_key,
        };

        let error = verify_signed_claims(signed, &[trusted_key]).unwrap_err();

        assert!(matches!(error, ClaimVerificationError::UnknownKey(_)));
    }

    #[test]
    fn unkeyed_claims_can_match_any_trusted_key() {
        let (mut signed, key) = signed_claims(serde_json::json!({
            "shape": "adapter-owned"
        }));
        signed.key_id = None;
        let signing_key = SigningKey::from_bytes(&[7; 32]);
        signed.signature = STANDARD.encode(
            signing_key
                .sign(&signed_payload_bytes(&signed).unwrap())
                .to_bytes(),
        );
        let wrong_key = ClaimSigningKey {
            key_id: Some("wrong".to_owned()),
            public_key: SigningKey::from_bytes(&[8; 32]).verifying_key().to_bytes(),
        };

        let verified = verify_signed_claims(signed, &[wrong_key, key]).unwrap();

        assert_eq!(verified.key_id, None);
        assert_eq!(verified.claims["shape"], "adapter-owned");
    }
}
