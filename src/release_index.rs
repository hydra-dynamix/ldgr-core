use std::collections::{HashMap, HashSet};
use std::fs;
use std::path::Path;

use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::EntryType;

use crate::adapter_compatibility::{
    core_compatibility_inventory, evaluate_requirements_v2, parse_adapter_compatibility_v2,
    CentralComponentDatabaseStateV2, CompatibilityRequirementsV2, CoreCompatibilityProfileV2,
};
use crate::update::network::{CatalogFetch, UpdateNetworkClient};

pub const LEGACY_ADAPTER_RELEASE_INDEX_SCHEMA_VERSION: u32 = 1;
pub const ADAPTER_RELEASE_INDEX_SCHEMA_VERSION: u32 = 2;
pub const LEGACY_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION: u32 =
    LEGACY_ADAPTER_RELEASE_INDEX_SCHEMA_VERSION;
pub const ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION: u32 = ADAPTER_RELEASE_INDEX_SCHEMA_VERSION;
pub const SOURCE_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION: u32 = 1;
pub const ADAPTER_RELEASE_INDEX_ENV: &str = "LDGR_ADAPTER_INDEX";
pub const ADAPTER_RELEASE_KEYRING_ENV: &str = "LDGR_ADAPTER_RELEASE_KEYRING";
pub const DEFAULT_ADAPTER_RELEASE_INDEX_URL: &str =
    "https://raw.githubusercontent.com/hydra-dynamix/ldgr-releases/main/index.json";

pub fn adapter_installation_receipt_schema_version(release: &AdapterRelease) -> u32 {
    if release.compatibility.is_some() {
        ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION
    } else {
        LEGACY_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION
    }
}

pub fn load_configured_release_index() -> anyhow::Result<AdapterReleaseIndex> {
    let source = std::env::var(ADAPTER_RELEASE_INDEX_ENV)
        .unwrap_or_else(|_| DEFAULT_ADAPTER_RELEASE_INDEX_URL.to_owned());
    load_release_index(&source)
}

pub fn load_release_index(source: &str) -> anyhow::Result<AdapterReleaseIndex> {
    let client = UpdateNetworkClient::new(false)?;
    let bytes = match client.fetch_catalog(source, None)? {
        CatalogFetch::Modified { bytes, .. } => bytes,
        CatalogFetch::NotModified { .. } => {
            bail!("adapter release index unexpectedly returned not-modified")
        }
    };
    let text = String::from_utf8(bytes).context("adapter release index is not UTF-8")?;
    parse_release_index(&text)
        .with_context(|| format!("invalid adapter release index from {source}"))
}

pub fn resolve_release<'a>(
    index: &'a AdapterReleaseIndex,
    domain: &str,
    core_version: &Version,
    platform: &str,
    exact_version: Option<&Version>,
    include_prerelease: bool,
) -> anyhow::Result<ResolvedAdapterRelease<'a>> {
    let core = core_compatibility_inventory();
    resolve_release_with_profile(
        index,
        domain,
        core_version,
        &core,
        &[],
        platform,
        exact_version,
        include_prerelease,
    )
}

/// Resolve against an explicit active or candidate Core profile. The package
/// version is consulted only by the bounded schema-v1 bridge; schema v2 uses
/// the protocol, schema, capability, and central-component profile.
#[allow(clippy::too_many_arguments)]
pub fn resolve_release_with_profile<'a>(
    index: &'a AdapterReleaseIndex,
    domain: &str,
    legacy_core_version: &Version,
    core: &CoreCompatibilityProfileV2,
    database_components: &[CentralComponentDatabaseStateV2],
    platform: &str,
    exact_version: Option<&Version>,
    include_prerelease: bool,
) -> anyhow::Result<ResolvedAdapterRelease<'a>> {
    validate_release_index(index)?;
    let adapter = index
        .adapters
        .iter()
        .find(|adapter| {
            adapter.domain == domain || adapter.aliases.iter().any(|alias| alias == domain)
        })
        .with_context(|| format!("adapter `{domain}` is not present in the release index"))?;
    let mut candidates = adapter
        .releases
        .iter()
        .filter_map(|release| {
            let version = Version::parse(&release.version).ok()?;
            let platform_release = release
                .platforms
                .iter()
                .find(|item| item.platform == platform)?;
            let channel_allowed = release.channel == ReleaseChannel::Stable || include_prerelease;
            let exact_allowed = exact_version.is_none_or(|exact| exact == &version);
            if !channel_allowed || !exact_allowed {
                return None;
            }
            let compatible = match index.schema_version {
                LEGACY_ADAPTER_RELEASE_INDEX_SCHEMA_VERSION => {
                    VersionReq::parse(&release.core_compatibility)
                        .ok()
                        .is_some_and(|requirement| requirement.matches(legacy_core_version))
                }
                ADAPTER_RELEASE_INDEX_SCHEMA_VERSION => {
                    release.compatibility.as_ref().is_some_and(|requirements| {
                        evaluate_requirements_v2(
                            requirements,
                            &adapter.domain,
                            core,
                            database_components,
                        )
                        .compatible
                    })
                }
                _ => false,
            };
            compatible.then_some((version, release, platform_release))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| {
        right
            .0
            .cmp(&left.0)
            .then_with(|| {
                right
                    .1
                    .compatibility
                    .as_ref()
                    .map_or(0, |value| value.adapter_protocol_epoch)
                    .cmp(
                        &left
                            .1
                            .compatibility
                            .as_ref()
                            .map_or(0, |value| value.adapter_protocol_epoch),
                    )
            })
            .then_with(|| {
                right
                    .1
                    .compatibility
                    .as_ref()
                    .map_or(0, |value| value.minimum_core_schema)
                    .cmp(
                        &left
                            .1
                            .compatibility
                            .as_ref()
                            .map_or(0, |value| value.minimum_core_schema),
                    )
            })
            .then_with(|| {
                left.1
                    .compatibility_sha256
                    .as_deref()
                    .unwrap_or_default()
                    .cmp(right.1.compatibility_sha256.as_deref().unwrap_or_default())
            })
    });
    let Some((version, release, platform_release)) = candidates.into_iter().next() else {
        let profile = if index.schema_version == ADAPTER_RELEASE_INDEX_SCHEMA_VERSION {
            format!(
                "Core compatibility profile (schema {}, protocols {:?})",
                core.core_schema_version, core.supported_adapter_protocol_epochs
            )
        } else {
            format!("Core {legacy_core_version}")
        };
        bail!(
            "no compatible release for adapter `{}` on platform `{platform}` with {profile}",
            adapter.domain
        );
    };
    Ok(ResolvedAdapterRelease {
        adapter,
        release,
        platform: platform_release,
        version,
    })
}

/// Bind an extracted archive's authoritative v2 sidecar to the signed index
/// variant before activation. Legacy archives retain their original exact
/// database-contract validation path.
pub fn verify_resolved_v2_sidecar(
    extracted_root: &Path,
    resolved: &ResolvedAdapterRelease<'_>,
) -> anyhow::Result<()> {
    verify_indexed_v2_sidecar(extracted_root, &resolved.adapter.domain, resolved.release)
}

pub fn verify_indexed_v2_sidecar(
    extracted_root: &Path,
    adapter: &str,
    release: &AdapterRelease,
) -> anyhow::Result<()> {
    if release.compatibility.is_none() {
        return Ok(());
    }
    let path = extracted_root.join("adapter-compatibility.json");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("adapter archive is missing {}", path.display()))?;
    let sidecar = parse_adapter_compatibility_v2(&text)
        .map_err(anyhow::Error::new)
        .context("staged adapter compatibility sidecar is invalid")?;
    if sidecar.adapter != adapter {
        bail!(
            "staged adapter identity `{}` does not match indexed product `{adapter}`",
            sidecar.adapter
        );
    }
    let expected = release.compatibility.as_ref().expect("checked v2 release");
    if &sidecar.compatibility != expected {
        bail!("staged adapter compatibility object does not match the signed release index");
    }
    let actual = sidecar.compatibility_sha256().map_err(anyhow::Error::new)?;
    let indexed = release
        .compatibility_sha256
        .as_deref()
        .expect("validated v2 release fingerprint");
    if actual != indexed {
        bail!(
            "staged adapter compatibility fingerprint mismatch: expected {indexed}, got {actual}"
        );
    }
    Ok(())
}

pub fn verify_file_sha256(path: &Path, expected: &str) -> anyhow::Result<()> {
    verify_file_sha256_for(path, expected, "adapter archive")
}

pub fn verify_file_sha256_for(path: &Path, expected: &str, subject: &str) -> anyhow::Result<()> {
    validate_sha256(expected)?;
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read {} for SHA-256 verification", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("{subject} SHA-256 mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

pub fn validate_sha256(expected: &str) -> anyhow::Result<()> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("indexed SHA-256 must contain exactly 64 hexadecimal characters");
    }
    Ok(())
}

pub fn verify_detached_release_signature(
    archive_path: &Path,
    signature_path: &Path,
    keyring_path: &Path,
    expected_key_id: &str,
) -> anyhow::Result<()> {
    let keyring =
        parse_release_keyring(&fs::read_to_string(keyring_path).with_context(|| {
            format!("failed to read release keyring {}", keyring_path.display())
        })?)?;
    let envelope =
        parse_detached_signature(&fs::read_to_string(signature_path).with_context(|| {
            format!(
                "failed to read detached signature {}",
                signature_path.display()
            )
        })?)?;
    let archive = fs::read(archive_path)
        .with_context(|| format!("failed to read signed archive {}", archive_path.display()))?;
    verify_detached_signature_bytes(
        &archive,
        &envelope,
        &keyring,
        expected_key_id,
        "adapter release",
    )
}

pub fn parse_release_keyring(text: &str) -> anyhow::Result<ReleaseKeyring> {
    let keyring: ReleaseKeyring =
        serde_json::from_str(text).context("release keyring is not valid JSON")?;
    validate_release_keyring(&keyring)?;
    Ok(keyring)
}

pub fn parse_detached_signature(text: &str) -> anyhow::Result<DetachedSignature> {
    serde_json::from_str(text).context("detached release signature is not valid JSON")
}

pub fn verify_detached_signature_bytes(
    signed_bytes: &[u8],
    envelope: &DetachedSignature,
    keyring: &ReleaseKeyring,
    expected_key_id: &str,
    subject: &str,
) -> anyhow::Result<()> {
    validate_release_keyring(keyring)?;
    if envelope.algorithm != "Ed25519" {
        bail!(
            "unsupported detached signature algorithm `{}`",
            envelope.algorithm
        );
    }
    if envelope.key_id != expected_key_id {
        bail!(
            "detached signature key id `{}` does not match indexed key id `{expected_key_id}`",
            envelope.key_id
        );
    }
    let trusted = keyring
        .keys
        .iter()
        .find(|key| key.key_id == expected_key_id)
        .with_context(|| format!("unknown release signing key id `{expected_key_id}`"))?;
    let public_key: [u8; 32] = STANDARD
        .decode(&trusted.public_key)
        .context("release public key is not valid base64")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("release public key must be 32 bytes"))?;
    let signature: [u8; 64] = STANDARD
        .decode(&envelope.signature)
        .context("detached signature is not valid base64")?
        .try_into()
        .map_err(|_| anyhow::anyhow!("detached signature must be 64 bytes"))?;
    let verifier = VerifyingKey::from_bytes(&public_key)
        .context("release public key is not a valid Ed25519 key")?;
    verifier
        .verify(signed_bytes, &Signature::from_bytes(&signature))
        .with_context(|| format!("detached {subject} signature did not verify"))
}

pub fn validate_release_keyring(keyring: &ReleaseKeyring) -> anyhow::Result<()> {
    if keyring.keys.is_empty() {
        bail!("release keyring must contain at least one trusted key");
    }
    let mut key_ids = HashSet::new();
    for key in &keyring.keys {
        if key.key_id.trim().is_empty() {
            bail!("release key id must not be empty");
        }
        if !key_ids.insert(key.key_id.as_str()) {
            bail!("duplicate release key id `{}`", key.key_id);
        }
        let public_key: [u8; 32] = STANDARD
            .decode(&key.public_key)
            .with_context(|| format!("release public key `{}` is not valid base64", key.key_id))?
            .try_into()
            .map_err(|_| anyhow::anyhow!("release public key `{}` must be 32 bytes", key.key_id))?;
        VerifyingKey::from_bytes(&public_key)
            .with_context(|| format!("release public key `{}` is not valid Ed25519", key.key_id))?;
    }
    Ok(())
}
pub fn extract_safe_tar_gz(
    archive_path: &Path,
    destination: &Path,
    expected_root: &str,
) -> anyhow::Result<()> {
    validate_archive_root(expected_root)?;
    fs::create_dir_all(destination)?;
    let file = fs::File::open(archive_path)
        .with_context(|| format!("failed to open archive {}", archive_path.display()))?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut archive = tar::Archive::new(decoder);
    for entry in archive
        .entries()
        .context("failed to enumerate adapter archive")?
    {
        let mut entry = entry.context("failed to read adapter archive entry")?;
        let path = entry
            .path()
            .context("archive entry path is invalid")?
            .into_owned();
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    std::path::Component::ParentDir
                        | std::path::Component::RootDir
                        | std::path::Component::Prefix(_)
                )
            })
        {
            bail!("unsafe adapter archive path `{}`", path.display());
        }
        if path
            .components()
            .next()
            .and_then(|part| part.as_os_str().to_str())
            != Some(expected_root)
        {
            bail!(
                "adapter archive entry `{}` is outside expected root `{expected_root}`",
                path.display()
            );
        }
        let kind = entry.header().entry_type();
        if matches!(kind, EntryType::Symlink | EntryType::Link) {
            bail!(
                "adapter archive links are not supported: `{}`",
                path.display()
            );
        }
        if !(kind.is_file() || kind.is_dir()) {
            bail!(
                "unsupported adapter archive entry type for `{}`",
                path.display()
            );
        }
        let target = destination.join(&path);
        if kind.is_dir() {
            fs::create_dir_all(&target)?;
        } else {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent)?;
            }
            entry
                .unpack(&target)
                .with_context(|| format!("failed to extract `{}`", path.display()))?;
        }
    }
    if !destination.join(expected_root).is_dir() {
        bail!("adapter archive did not contain expected root `{expected_root}`");
    }
    Ok(())
}

pub fn validate_archive_root(root: &str) -> anyhow::Result<()> {
    if root.is_empty() || root == "." || root == ".." || Path::new(root).components().count() != 1 {
        bail!("archive_root must be one relative path component");
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleaseKeyring {
    pub keys: Vec<ReleasePublicKey>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ReleasePublicKey {
    pub key_id: String,
    pub public_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DetachedSignature {
    pub algorithm: String,
    pub key_id: String,
    pub signature: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct InstallationReceipt {
    pub schema_version: u32,
    pub domain: String,
    pub version: String,
    pub source_url: String,
    pub sha256: String,
    pub signing_key_id: String,
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub core_compatibility: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<CompatibilityRequirementsV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility_sha256: Option<String>,
    pub platform: String,
    pub resource_manifest: String,
    pub installed_at_unix_seconds: u64,
    pub bundle_sha256: String,
    pub binary_path: Option<String>,
    pub binary_sha256: Option<String>,
    pub owned_resources: Vec<OwnedResource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInstallationReceipt {
    pub schema_version: u32,
    pub install_kind: String,
    pub domain: String,
    pub installed_at_unix_seconds: u64,
    pub source: SourceInstallIdentity,
    pub manifest_digests: SourceManifestDigests,
    pub installer_invocation: Vec<String>,
    pub executable_invocations: Vec<SourceExecutableInvocation>,
    pub installed_files: Vec<OwnedResource>,
    pub owned_resources: Vec<OwnedResource>,
    pub ownership: SourceOwnershipBoundaries,
    pub verified_release: bool,
}

#[derive(Debug)]
pub enum AdapterInstallationReceipt {
    Release(InstallationReceipt),
    Source(SourceInstallationReceipt),
}

pub fn parse_adapter_installation_receipt(
    value: serde_json::Value,
) -> anyhow::Result<AdapterInstallationReceipt> {
    if value
        .get("install_kind")
        .and_then(serde_json::Value::as_str)
        == Some("local_source")
    {
        let receipt: SourceInstallationReceipt =
            serde_json::from_value(value).context("source installation receipt is invalid")?;
        validate_source_installation_receipt(&receipt)?;
        Ok(AdapterInstallationReceipt::Source(receipt))
    } else {
        let receipt: InstallationReceipt =
            serde_json::from_value(value).context("release installation receipt is invalid")?;
        validate_release_installation_receipt(&receipt)?;
        Ok(AdapterInstallationReceipt::Release(receipt))
    }
}

pub fn validate_release_installation_receipt(receipt: &InstallationReceipt) -> anyhow::Result<()> {
    validate_identifier(&receipt.domain, "installation receipt domain")?;
    Version::parse(&receipt.version).context("release receipt version is invalid")?;
    match receipt.schema_version {
        LEGACY_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION => {
            if receipt.compatibility.is_some() || receipt.compatibility_sha256.is_some() {
                bail!("legacy release receipt contains schema-v2 compatibility fields");
            }
            require_text(
                &receipt.core_compatibility,
                "legacy release receipt core_compatibility",
            )?;
            VersionReq::parse(&receipt.core_compatibility)
                .context("legacy release receipt Core compatibility is invalid")?;
        }
        ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION => {
            if !receipt.core_compatibility.is_empty() {
                bail!("schema-v2 release receipt contains a legacy Core compatibility range");
            }
            let compatibility = receipt
                .compatibility
                .as_ref()
                .context("schema-v2 release receipt requires compatibility metadata")?;
            compatibility
                .validate()
                .map_err(anyhow::Error::new)
                .context("schema-v2 release receipt compatibility is invalid")?;
            let expected = compatibility
                .compatibility_sha256()
                .map_err(anyhow::Error::new)?;
            let actual = receipt
                .compatibility_sha256
                .as_deref()
                .context("schema-v2 release receipt requires compatibility_sha256")?;
            if actual != expected {
                bail!(
                    "schema-v2 release receipt compatibility_sha256 mismatch: expected {expected}, got {actual}"
                );
            }
        }
        schema_version => bail!(
            "unsupported release installation receipt schema {schema_version}; expected {LEGACY_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION} or {ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION}"
        ),
    }
    Ok(())
}

pub fn validate_source_installation_receipt(
    receipt: &SourceInstallationReceipt,
) -> anyhow::Result<()> {
    if receipt.schema_version != SOURCE_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION {
        bail!(
            "unsupported source installation receipt schema {}; expected {}",
            receipt.schema_version,
            SOURCE_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION
        );
    }
    if receipt.install_kind != "local_source" {
        bail!("source installation receipt kind must be `local_source`");
    }
    if receipt.verified_release {
        bail!("local source receipt must not claim verified release provenance");
    }
    if receipt.ownership.source_checkout_owned {
        bail!("local source receipt must not claim ownership of the source checkout");
    }
    if receipt.ownership.generated_paths != ["source-target"] {
        bail!("source receipt generated paths must be exactly `source-target`");
    }
    let namespace = receipt
        .source
        .package
        .strip_prefix("ldgr-")
        .unwrap_or(&receipt.source.package)
        .strip_suffix("-adapter")
        .unwrap_or_else(|| {
            receipt
                .source
                .package
                .strip_prefix("ldgr-")
                .unwrap_or(&receipt.source.package)
        });
    if namespace != receipt.domain {
        bail!(
            "source receipt package `{}` does not own adapter `{}`",
            receipt.source.package,
            receipt.domain
        );
    }
    let installed_manifest = receipt
        .installed_files
        .iter()
        .find(|file| file.path == "adapter.toml")
        .context("source receipt must track installed adapter.toml")?;
    if installed_manifest.sha256 != receipt.manifest_digests.installed_adapter_manifest_sha256 {
        bail!("source receipt installed adapter manifest digests disagree");
    }
    if let Some(expected) = &receipt.manifest_digests.installed_resource_manifest_sha256 {
        let installed_resource_manifest = receipt
            .installed_files
            .iter()
            .find(|file| file.path == "adapter-resources.json")
            .context("source receipt resource digest has no installed resource manifest file")?;
        if &installed_resource_manifest.sha256 != expected {
            bail!("source receipt installed resource manifest digests disagree");
        }
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceInstallIdentity {
    pub package: String,
    pub bundle_root: String,
    pub cargo_manifest: String,
    pub bundle_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceManifestDigests {
    pub source_adapter_manifest_sha256: String,
    pub source_cargo_manifest_sha256: String,
    pub installed_adapter_manifest_sha256: String,
    pub source_resource_manifest_sha256: Option<String>,
    pub installed_resource_manifest_sha256: Option<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceExecutableInvocation {
    pub kind: String,
    pub name: String,
    pub argv: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceOwnershipBoundaries {
    pub install_root: String,
    pub marker_path: String,
    pub source_checkout_owned: bool,
    pub generated_paths: Vec<String>,
    pub external_resource_roots: Vec<String>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedResource {
    pub path: String,
    pub sha256: String,
}

pub fn parse_resource_manifest(text: &str) -> anyhow::Result<AdapterResourceManifest> {
    let manifest: AdapterResourceManifest =
        serde_json::from_str(text).context("failed to parse adapter resource manifest JSON")?;
    if manifest.schema_version != 1 {
        bail!(
            "unsupported adapter resource manifest schema_version {}",
            manifest.schema_version
        );
    }
    if manifest.resources.is_empty() {
        bail!("adapter resource manifest must contain at least one resource");
    }
    for (index, resource) in manifest.resources.iter().enumerate() {
        validate_relative_resource_path(&resource.source, &format!("resources[{index}].source"))?;
        validate_relative_resource_path(
            &resource.destination,
            &format!("resources[{index}].destination"),
        )?;
        if resource.harnesses.is_empty() {
            bail!("resources[{index}].harnesses must not be empty");
        }
        for harness in &resource.harnesses {
            let supported = matches!(
                (harness.as_str(), resource.kind),
                (
                    "pi",
                    AdapterResourceKind::Prompt
                        | AdapterResourceKind::Skill
                        | AdapterResourceKind::Extension
                ) | (
                    "codex",
                    AdapterResourceKind::Prompt | AdapterResourceKind::Skill
                ) | (
                    "claude",
                    AdapterResourceKind::Skill | AdapterResourceKind::Command
                ) | (
                    "openclaw",
                    AdapterResourceKind::Skill | AdapterResourceKind::Command
                )
            );
            if !supported {
                bail!(
                    "resources[{index}] kind {:?} is not supported by harness `{harness}`",
                    resource.kind
                );
            }
        }
    }
    Ok(manifest)
}

fn validate_relative_resource_path(value: &str, field: &str) -> anyhow::Result<()> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|part| {
            matches!(
                part,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
    {
        bail!("{field} must be a non-empty destination-relative path without traversal");
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterResourceManifest {
    pub schema_version: u32,
    pub resources: Vec<AdapterResource>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterResource {
    pub kind: AdapterResourceKind,
    pub harnesses: Vec<String>,
    pub source: String,
    pub destination: String,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterResourceKind {
    Prompt,
    Skill,
    Extension,
    Command,
}

#[derive(Clone, Debug)]
pub struct ResolvedAdapterRelease<'a> {
    pub adapter: &'a AdapterReleaseProduct,
    pub release: &'a AdapterRelease,
    pub platform: &'a AdapterPlatformRelease,
    pub version: Version,
}

pub fn parse_release_index(json: &str) -> anyhow::Result<AdapterReleaseIndex> {
    let index: AdapterReleaseIndex =
        serde_json::from_str(json).context("failed to parse adapter release index JSON")?;
    validate_release_index(&index)?;
    Ok(index)
}

pub fn validate_release_index(index: &AdapterReleaseIndex) -> anyhow::Result<()> {
    if ![
        LEGACY_ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
        ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
    ]
    .contains(&index.schema_version)
    {
        bail!(
            "unsupported adapter release index schema_version {}; expected 1 or 2",
            index.schema_version
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
        let mut precedence_versions = HashMap::<String, String>::new();
        let mut variants = HashSet::<(String, u8, String)>::new();
        for (release_index, release) in adapter.releases.iter().enumerate() {
            let path = format!("adapters[{adapter_index}].releases[{release_index}]");
            require_text(&release.version, &format!("{path}.version"))?;
            let version = Version::parse(&release.version)
                .with_context(|| format!("{path}.version must be a semantic version"))?;
            let precedence = format!(
                "{}.{}.{}-{}",
                version.major, version.minor, version.patch, version.pre
            );
            if let Some(existing) = precedence_versions.get(&precedence) {
                if existing != &release.version {
                    bail!(
                        "{path}.version `{}` collides in SemVer precedence with `{existing}`; build metadata cannot select a release",
                        release.version
                    );
                }
            } else {
                precedence_versions.insert(precedence, release.version.clone());
            }

            let variant_id = match index.schema_version {
                LEGACY_ADAPTER_RELEASE_INDEX_SCHEMA_VERSION => {
                    require_text(
                        &release.core_compatibility,
                        &format!("{path}.core_compatibility"),
                    )?;
                    if release.compatibility.is_some() || release.compatibility_sha256.is_some() {
                        bail!("{path} mixes schema-v2 compatibility fields into a legacy index");
                    }
                    VersionReq::parse(&release.core_compatibility).with_context(|| {
                        format!("{path}.core_compatibility must be a semantic version requirement")
                    })?;
                    release.core_compatibility.clone()
                }
                ADAPTER_RELEASE_INDEX_SCHEMA_VERSION => {
                    if !release.core_compatibility.is_empty() {
                        bail!("{path}.core_compatibility is forbidden in schema v2");
                    }
                    let compatibility = release.compatibility.as_ref().with_context(|| {
                        format!("{path}.compatibility is required in schema v2")
                    })?;
                    compatibility
                        .validate()
                        .map_err(anyhow::Error::new)
                        .with_context(|| format!("{path}.compatibility is invalid"))?;
                    let expected = compatibility
                        .compatibility_sha256()
                        .map_err(anyhow::Error::new)?;
                    let indexed = release.compatibility_sha256.as_deref().with_context(|| {
                        format!("{path}.compatibility_sha256 is required in schema v2")
                    })?;
                    if indexed != expected {
                        bail!(
                            "{path}.compatibility_sha256 mismatch: expected {expected}, got {indexed}"
                        );
                    }
                    indexed.to_owned()
                }
                _ => unreachable!("schema checked above"),
            };
            let channel = match release.channel {
                ReleaseChannel::Stable => 0,
                ReleaseChannel::Prerelease => 1,
            };
            if !variants.insert((release.version.clone(), channel, variant_id)) {
                bail!("{path} duplicates release tuple (version, channel, compatibility variant)");
            }

            if release.platforms.is_empty() {
                bail!("{path}.platforms must not be empty");
            }
            let mut platforms = HashSet::new();
            for (platform_index, platform) in release.platforms.iter().enumerate() {
                let platform_path = format!("{path}.platforms[{platform_index}]");
                require_text(&platform.platform, &format!("{platform_path}.platform"))?;
                if !platforms.insert(platform.platform.as_str()) {
                    bail!(
                        "{platform_path}.platform duplicates `{}`",
                        platform.platform
                    );
                }
                require_text(&platform.asset_url, &format!("{platform_path}.asset_url"))?;
                require_text(
                    &platform.archive_root,
                    &format!("{platform_path}.archive_root"),
                )?;
                validate_archive_root(&platform.archive_root)
                    .with_context(|| format!("invalid {platform_path}.archive_root"))?;
                require_text(&platform.binary, &format!("{platform_path}.binary"))?;
                validate_sha256(&platform.sha256)
                    .with_context(|| format!("invalid {platform_path}.sha256"))?;
                if platform
                    .sha256
                    .bytes()
                    .any(|byte| byte.is_ascii_uppercase())
                {
                    bail!("{platform_path}.sha256 must use lowercase hexadecimal");
                }
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
                validate_relative_resource_path(
                    &platform.resource_manifest,
                    &format!("{platform_path}.resource_manifest"),
                )?;
            }
        }
    }
    Ok(())
}

/// One signed stable Core capability/contract inventory used by catalog CI.
/// Package versions identify diagnostics only; compatibility evaluation never
/// compares them with adapter package ranges.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReleasedCoreCompatibilityV2 {
    pub version: String,
    pub profile: CoreCompatibilityProfileV2,
    pub projected_database_components: Vec<CentralComponentDatabaseStateV2>,
}

/// Apply publication policy after structural index validation. Every stable
/// artifact variant must work on at least one released stable Core inventory,
/// the current (last) inventory must resolve each product, and same-version
/// variants may not overlap on any released inventory.
pub fn validate_release_index_against_core_profiles(
    index: &AdapterReleaseIndex,
    released_cores: &[ReleasedCoreCompatibilityV2],
) -> anyhow::Result<()> {
    validate_release_index(index)?;
    ensure_schema_v2(index)?;
    if released_cores.is_empty() {
        bail!("catalog compatibility gate requires at least one released stable Core profile");
    }
    for (offset, core) in released_cores.iter().enumerate() {
        Version::parse(&core.version)
            .with_context(|| format!("released Core profiles[{offset}].version is not semantic"))?;
        core.profile
            .validate()
            .map_err(anyhow::Error::new)
            .with_context(|| format!("released Core profiles[{offset}].profile is invalid"))?;
    }
    let current = released_cores
        .iter()
        .max_by(|left, right| {
            Version::parse(&left.version)
                .expect("validated Core version")
                .cmp(&Version::parse(&right.version).expect("validated Core version"))
        })
        .expect("non-empty profiles");
    for (adapter_index, adapter) in index.adapters.iter().enumerate() {
        let stable = adapter
            .releases
            .iter()
            .filter(|release| release.channel == ReleaseChannel::Stable)
            .collect::<Vec<_>>();
        if stable.is_empty() {
            bail!("adapters[{adapter_index}] has no stable compatibility-v2 release");
        }
        for release in &stable {
            let requirements = release.compatibility.as_ref().expect("validated schema v2");
            if !released_cores.iter().any(|core| {
                evaluate_requirements_v2(
                    requirements,
                    &adapter.domain,
                    &core.profile,
                    &core.projected_database_components,
                )
                .compatible
            }) {
                bail!(
                    "stable adapter {} {} variant {} is incompatible with every released stable Core profile",
                    adapter.domain,
                    release.version,
                    release.compatibility_sha256.as_deref().unwrap_or("missing")
                );
            }
        }
        if !stable.iter().any(|release| {
            evaluate_requirements_v2(
                release.compatibility.as_ref().expect("validated schema v2"),
                &adapter.domain,
                &current.profile,
                &current.projected_database_components,
            )
            .compatible
        }) {
            bail!(
                "adapter {} has no stable release compatible with current stable Core {}",
                adapter.domain,
                current.version
            );
        }
        for left in 0..stable.len() {
            for right in (left + 1)..stable.len() {
                if stable[left].version != stable[right].version {
                    continue;
                }
                for core in released_cores {
                    let left_matches = evaluate_requirements_v2(
                        stable[left]
                            .compatibility
                            .as_ref()
                            .expect("validated schema v2"),
                        &adapter.domain,
                        &core.profile,
                        &core.projected_database_components,
                    )
                    .compatible;
                    let right_matches = evaluate_requirements_v2(
                        stable[right]
                            .compatibility
                            .as_ref()
                            .expect("validated schema v2"),
                        &adapter.domain,
                        &core.profile,
                        &core.projected_database_components,
                    )
                    .compatible;
                    if left_matches && right_matches {
                        bail!(
                            "adapter {} version {} has compatibility variants that overlap on Core {}",
                            adapter.domain,
                            stable[left].version,
                            core.version
                        );
                    }
                }
            }
        }
    }
    Ok(())
}

fn ensure_schema_v2(index: &AdapterReleaseIndex) -> anyhow::Result<()> {
    if index.schema_version != ADAPTER_RELEASE_INDEX_SCHEMA_VERSION {
        bail!("catalog publication requires adapter index schema_version 2");
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
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub core_compatibility: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility: Option<CompatibilityRequirementsV2>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compatibility_sha256: Option<String>,
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
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};
    use semver::Version;
    use std::io::Write as _;
    use tar::EntryType;

    use crate::adapter_compatibility::{
        CentralComponentDescriptorV2, CentralComponentRequirementV2, CompatibilityRequirementsV2,
        CoreCompatibilityProfileV2, CORE_COMPATIBILITY_FORMAT_V2,
    };

    use super::{
        extract_safe_tar_gz, load_release_index, parse_adapter_installation_receipt,
        parse_release_index, parse_resource_manifest, resolve_release,
        resolve_release_with_profile, validate_release_index_against_core_profiles,
        validate_release_installation_receipt, verify_detached_release_signature,
        verify_file_sha256, verify_resolved_v2_sidecar, AdapterClassification,
        AdapterInstallationReceipt, AdapterPlatformRelease, AdapterRelease, AdapterReleaseIndex,
        AdapterReleaseProduct, DetachedSignature, InstallationReceipt, ReleaseChannel,
        ReleaseKeyring, ReleasePublicKey, ReleasedCoreCompatibilityV2,
        ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION, ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
        LEGACY_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION,
    };

    const OPEN_AND_COMMERCIAL: &str =
        include_str!("../tests/fixtures/release-index/open-and-commercial.json");
    const A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";

    fn v2_requirements(
        protocol: i32,
        minimum_core_schema: i32,
        capabilities: &[&str],
        central_components: Vec<CentralComponentRequirementV2>,
    ) -> CompatibilityRequirementsV2 {
        CompatibilityRequirementsV2 {
            adapter_protocol_epoch: protocol,
            minimum_core_schema,
            required_core_capabilities: capabilities
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            central_components,
        }
    }

    fn fixture_platform() -> AdapterPlatformRelease {
        AdapterPlatformRelease {
            platform: "linux-aarch64".to_owned(),
            asset_url: "https://example.invalid/example.tar.gz".to_owned(),
            archive_root: "example-1.0.0".to_owned(),
            binary: "ldgr-example".to_owned(),
            sha256: "0".repeat(64),
            signature_url: "https://example.invalid/example.tar.gz.sig".to_owned(),
            signing_key_id: "fixture-key".to_owned(),
            resource_manifest: "adapter-resources.json".to_owned(),
        }
    }

    fn v2_release(
        version: &str,
        channel: ReleaseChannel,
        compatibility: CompatibilityRequirementsV2,
    ) -> AdapterRelease {
        let compatibility_sha256 = compatibility.compatibility_sha256().unwrap();
        AdapterRelease {
            version: version.to_owned(),
            channel,
            core_compatibility: String::new(),
            compatibility: Some(compatibility),
            compatibility_sha256: Some(compatibility_sha256),
            platforms: vec![fixture_platform()],
        }
    }

    fn v2_index(releases: Vec<AdapterRelease>) -> AdapterReleaseIndex {
        AdapterReleaseIndex {
            schema_version: ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
            adapters: vec![AdapterReleaseProduct {
                domain: "example".to_owned(),
                primary_namespace: "example".to_owned(),
                title: "Example".to_owned(),
                aliases: vec!["reference".to_owned()],
                classification: AdapterClassification::OpenSource,
                source_url: None,
                releases,
            }],
        }
    }

    fn release_installation_receipt(schema_version: u32) -> InstallationReceipt {
        let compatibility = v2_requirements(1, 5, &["work.v1"], Vec::new());
        let compatibility_sha256 = compatibility.compatibility_sha256().unwrap();
        let legacy = schema_version == LEGACY_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION;
        InstallationReceipt {
            schema_version,
            domain: "example".to_owned(),
            version: "1.2.3".to_owned(),
            source_url: "https://example.invalid/example.tar.gz".to_owned(),
            sha256: "0".repeat(64),
            signing_key_id: "fixture-key".to_owned(),
            core_compatibility: if legacy {
                ">=0.1.0, <0.2.0".to_owned()
            } else {
                String::new()
            },
            compatibility: (!legacy).then_some(compatibility),
            compatibility_sha256: (!legacy).then_some(compatibility_sha256),
            platform: "linux-x86_64".to_owned(),
            resource_manifest: "adapter-resources.json".to_owned(),
            installed_at_unix_seconds: 1,
            bundle_sha256: "1".repeat(64),
            binary_path: None,
            binary_sha256: None,
            owned_resources: Vec::new(),
        }
    }

    fn core_profile(schema: i32) -> CoreCompatibilityProfileV2 {
        CoreCompatibilityProfileV2 {
            format: CORE_COMPATIBILITY_FORMAT_V2.to_owned(),
            core_schema_version: schema,
            supported_adapter_protocol_epochs: vec![1],
            core_capabilities: vec!["prompt.v1".to_owned(), "work.v1".to_owned()],
            central_components: Vec::<CentralComponentDescriptorV2>::new(),
        }
    }

    #[test]
    fn installation_receipt_schema_contract_is_shared_and_fail_closed() -> anyhow::Result<()> {
        let legacy =
            release_installation_receipt(LEGACY_ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION);
        validate_release_installation_receipt(&legacy)?;
        let canonical = release_installation_receipt(ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION);
        validate_release_installation_receipt(&canonical)?;
        assert!(matches!(
            parse_adapter_installation_receipt(serde_json::to_value(&canonical)?)?,
            AdapterInstallationReceipt::Release(_)
        ));

        let mut mixed = canonical.clone();
        mixed.core_compatibility = ">=0.1.0".to_owned();
        assert!(validate_release_installation_receipt(&mixed)
            .unwrap_err()
            .to_string()
            .contains("legacy Core compatibility"));

        let mut stale_fingerprint = canonical.clone();
        stale_fingerprint.compatibility_sha256 = Some(format!("sha256:{}", "0".repeat(64)));
        assert!(validate_release_installation_receipt(&stale_fingerprint)
            .unwrap_err()
            .to_string()
            .contains("compatibility_sha256 mismatch"));

        let unknown = release_installation_receipt(ADAPTER_INSTALLATION_RECEIPT_SCHEMA_VERSION + 1);
        assert!(validate_release_installation_receipt(&unknown)
            .unwrap_err()
            .to_string()
            .contains("unsupported release installation receipt schema"));
        Ok(())
    }

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
            OPEN_AND_COMMERCIAL.replacen("\"schema_version\": 1", "\"schema_version\": 3", 1);
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

    #[test]
    fn loads_explicit_local_index_without_network() -> anyhow::Result<()> {
        let index = load_release_index("tests/fixtures/release-index/open-and-commercial.json")?;
        assert_eq!(index.adapters[0].domain, "example");
        Ok(())
    }

    #[test]
    fn v2_resolution_ignores_core_patch_and_accepts_additive_schema() -> anyhow::Result<()> {
        let index = v2_index(vec![
            v2_release(
                "2.0.0",
                ReleaseChannel::Stable,
                v2_requirements(1, 5, &["work.v2"], Vec::new()),
            ),
            v2_release(
                "1.9.0",
                ReleaseChannel::Stable,
                v2_requirements(1, 5, &["work.v1"], Vec::new()),
            ),
        ]);
        let encoded = serde_json::to_string(&index)?;
        assert!(encoded.contains("\"compatibility\""));
        assert!(!encoded.contains("core_compatibility"));
        let index = parse_release_index(&encoded)?;
        let core5 = core_profile(5);
        let core6 = core_profile(6);
        for (package_patch, profile) in [
            (Version::parse("0.1.14")?, &core5),
            (Version::parse("0.1.99")?, &core5),
            (Version::parse("0.2.0")?, &core6),
        ] {
            let resolved = resolve_release_with_profile(
                &index,
                "reference",
                &package_patch,
                profile,
                &[],
                "linux-aarch64",
                None,
                false,
            )?;
            assert_eq!(resolved.version, Version::parse("1.9.0")?);
        }
        Ok(())
    }

    #[test]
    fn publication_gate_uses_released_profiles_and_rejects_overlapping_variants() {
        let compatible = v2_release(
            "1.0.0",
            ReleaseChannel::Stable,
            v2_requirements(1, 5, &["work.v1"], Vec::new()),
        );
        let cores = vec![
            ReleasedCoreCompatibilityV2 {
                version: "0.1.14".to_owned(),
                profile: core_profile(5),
                projected_database_components: Vec::new(),
            },
            ReleasedCoreCompatibilityV2 {
                version: "0.1.15".to_owned(),
                profile: core_profile(6),
                projected_database_components: Vec::new(),
            },
        ];
        validate_release_index_against_core_profiles(&v2_index(vec![compatible.clone()]), &cores)
            .unwrap();

        let incompatible = v2_release(
            "2.0.0",
            ReleaseChannel::Stable,
            v2_requirements(1, 5, &["work.v2"], Vec::new()),
        );
        let error =
            validate_release_index_against_core_profiles(&v2_index(vec![incompatible]), &cores)
                .unwrap_err();
        assert!(format!("{error:#}").contains("every released stable Core"));

        let overlap = v2_release(
            "1.0.0",
            ReleaseChannel::Stable,
            v2_requirements(1, 4, &[], Vec::new()),
        );
        let error = validate_release_index_against_core_profiles(
            &v2_index(vec![compatible, overlap]),
            &cores,
        )
        .unwrap_err();
        assert!(format!("{error:#}").contains("overlap"));
    }

    #[test]
    fn v2_resolver_rejects_protocol_and_component_incompatibility_before_selection(
    ) -> anyhow::Result<()> {
        let component = CentralComponentRequirementV2 {
            namespace: "notes".to_owned(),
            schema_epoch: 1,
            minimum_schema_version: 1,
            accepted_lineage_digests: vec![A.to_owned()],
        };
        let index = v2_index(vec![
            v2_release(
                "3.0.0",
                ReleaseChannel::Stable,
                v2_requirements(2, 5, &[], Vec::new()),
            ),
            v2_release(
                "2.0.0",
                ReleaseChannel::Stable,
                v2_requirements(1, 5, &[], vec![component]),
            ),
            v2_release(
                "1.0.0",
                ReleaseChannel::Stable,
                v2_requirements(1, 5, &[], Vec::new()),
            ),
        ]);
        let core = core_profile(5);
        let resolved = resolve_release_with_profile(
            &index,
            "example",
            &Version::parse("9.9.9")?,
            &core,
            &[],
            "linux-aarch64",
            None,
            false,
        )?;
        assert_eq!(resolved.version, Version::parse("1.0.0")?);

        let incompatible_only = v2_index(index.adapters[0].releases[..2].to_vec());
        assert!(resolve_release_with_profile(
            &incompatible_only,
            "example",
            &Version::parse("9.9.9")?,
            &core,
            &[],
            "linux-aarch64",
            None,
            false,
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn same_version_variants_have_deterministic_generation_order() -> anyhow::Result<()> {
        let older_generation = v2_release(
            "1.2.3",
            ReleaseChannel::Stable,
            v2_requirements(1, 4, &[], Vec::new()),
        );
        let newer_generation = v2_release(
            "1.2.3",
            ReleaseChannel::Stable,
            v2_requirements(1, 5, &[], Vec::new()),
        );
        let expected = newer_generation.compatibility_sha256.clone();
        let core = core_profile(6);
        for releases in [
            vec![older_generation.clone(), newer_generation.clone()],
            vec![newer_generation.clone(), older_generation.clone()],
        ] {
            let index = v2_index(releases);
            let resolved = resolve_release_with_profile(
                &index,
                "example",
                &Version::parse("0.1.14")?,
                &core,
                &[],
                "linux-aarch64",
                Some(&Version::parse("1.2.3")?),
                false,
            )?;
            assert_eq!(resolved.release.compatibility_sha256, expected);
        }

        let epoch1 = v2_release(
            "1.5.0",
            ReleaseChannel::Stable,
            v2_requirements(1, 5, &[], Vec::new()),
        );
        let epoch2 = v2_release(
            "1.5.0",
            ReleaseChannel::Stable,
            v2_requirements(2, 5, &[], Vec::new()),
        );
        let mut transition_core = core.clone();
        transition_core.supported_adapter_protocol_epochs = vec![1, 2];
        let transition_index = v2_index(vec![epoch1, epoch2]);
        let resolved = resolve_release_with_profile(
            &transition_index,
            "example",
            &Version::parse("0.1.14")?,
            &transition_core,
            &[],
            "linux-aarch64",
            None,
            false,
        )?;
        assert_eq!(
            resolved
                .release
                .compatibility
                .as_ref()
                .unwrap()
                .adapter_protocol_epoch,
            2
        );

        let left = v2_release(
            "2.0.0",
            ReleaseChannel::Stable,
            v2_requirements(1, 5, &[], Vec::new()),
        );
        let right = v2_release(
            "2.0.0",
            ReleaseChannel::Stable,
            v2_requirements(1, 5, &["prompt.v1"], Vec::new()),
        );
        let expected_digest = std::cmp::min(
            left.compatibility_sha256.clone().unwrap(),
            right.compatibility_sha256.clone().unwrap(),
        );
        for releases in [vec![left.clone(), right.clone()], vec![right, left]] {
            let index = v2_index(releases);
            let resolved = resolve_release_with_profile(
                &index,
                "example",
                &Version::parse("0.1.14")?,
                &core,
                &[],
                "linux-aarch64",
                None,
                false,
            )?;
            assert_eq!(
                resolved.release.compatibility_sha256.as_deref(),
                Some(expected_digest.as_str())
            );
        }
        Ok(())
    }

    #[test]
    fn v2_catalog_rejects_mixed_fields_bad_fingerprints_and_ambiguous_versions() {
        let release = v2_release(
            "1.0.0",
            ReleaseChannel::Stable,
            v2_requirements(1, 5, &[], Vec::new()),
        );
        let mut mixed = v2_index(vec![release.clone()]);
        mixed.adapters[0].releases[0].core_compatibility = ">=0.1.0".to_owned();
        assert!(
            format!("{:#}", super::validate_release_index(&mixed).unwrap_err())
                .contains("forbidden")
        );

        let mut bad_digest = v2_index(vec![release.clone()]);
        bad_digest.adapters[0].releases[0].compatibility_sha256 = Some(A.to_owned());
        let error = format!(
            "{:#}",
            super::validate_release_index(&bad_digest).unwrap_err()
        );
        assert!(error.contains("compatibility_sha256"), "{error}");

        let duplicate = v2_index(vec![release.clone(), release.clone()]);
        assert!(format!(
            "{:#}",
            super::validate_release_index(&duplicate).unwrap_err()
        )
        .contains("duplicates release tuple"));

        let mut build_collision = release;
        build_collision.version = "1.0.0+rebuilt".to_owned();
        let collision = v2_index(vec![
            v2_release(
                "1.0.0+original",
                ReleaseChannel::Stable,
                v2_requirements(1, 5, &[], Vec::new()),
            ),
            build_collision,
        ]);
        assert!(format!(
            "{:#}",
            super::validate_release_index(&collision).unwrap_err()
        )
        .contains("collides in SemVer precedence"));
    }

    #[test]
    fn staged_v2_sidecar_must_match_the_signed_variant() -> anyhow::Result<()> {
        use crate::adapter_compatibility::{
            AdapterCompatibilitySidecarV2, ADAPTER_COMPATIBILITY_FORMAT_V2,
        };

        let compatibility = v2_requirements(1, 5, &["work.v1"], Vec::new());
        let index = v2_index(vec![v2_release(
            "1.0.0",
            ReleaseChannel::Stable,
            compatibility.clone(),
        )]);
        let resolved = resolve_release(
            &index,
            "example",
            &Version::parse("0.1.14")?,
            "linux-aarch64",
            None,
            false,
        )?;
        let root = tempfile::tempdir()?;
        let sidecar = AdapterCompatibilitySidecarV2 {
            format: ADAPTER_COMPATIBILITY_FORMAT_V2.to_owned(),
            adapter: "example".to_owned(),
            compatibility,
            local_stores: Vec::new(),
        };
        std::fs::write(
            root.path().join("adapter-compatibility.json"),
            sidecar.canonical_file_json().unwrap(),
        )?;
        verify_resolved_v2_sidecar(root.path(), &resolved)?;

        let mut changed = sidecar;
        changed.compatibility.minimum_core_schema = 4;
        std::fs::write(
            root.path().join("adapter-compatibility.json"),
            changed.canonical_file_json().unwrap(),
        )?;
        assert!(verify_resolved_v2_sidecar(root.path(), &resolved).is_err());
        Ok(())
    }

    #[test]
    fn resolves_latest_compatible_stable_platform_release() -> anyhow::Result<()> {
        let mut index = parse_release_index(OPEN_AND_COMMERCIAL)?;
        let mut newer = index.adapters[0].releases[0].clone();
        newer.version = "0.1.5".to_owned();
        index.adapters[0].releases.push(newer);
        let resolved = resolve_release(
            &index,
            "reference",
            &Version::parse("0.1.4")?,
            "linux-aarch64",
            None,
            false,
        )?;
        assert_eq!(resolved.version, Version::parse("0.1.5")?);
        Ok(())
    }

    #[test]
    fn resolver_honors_exact_prerelease_platform_and_compatibility() -> anyhow::Result<()> {
        let index = parse_release_index(OPEN_AND_COMMERCIAL)?;
        let prerelease = resolve_release(
            &index,
            "evidence",
            &Version::parse("0.1.4")?,
            "linux-aarch64",
            Some(&Version::parse("0.1.0")?),
            true,
        )?;
        assert_eq!(prerelease.release.channel, ReleaseChannel::Prerelease);
        assert!(resolve_release(
            &index,
            "evidence",
            &Version::parse("0.1.4")?,
            "linux-x86_64",
            None,
            true
        )
        .is_err());
        assert!(resolve_release(
            &index,
            "example",
            &Version::parse("0.2.0")?,
            "linux-aarch64",
            None,
            false
        )
        .is_err());
        assert!(resolve_release(
            &index,
            "evidence",
            &Version::parse("0.1.4")?,
            "linux-aarch64",
            None,
            false
        )
        .is_err());
        Ok(())
    }

    #[test]
    fn verifies_sha256_and_rejects_one_byte_mutation() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let archive = directory.path().join("adapter.tar.gz");
        std::fs::write(&archive, b"original")?;
        verify_file_sha256(
            &archive,
            "0682c5f2076f099c34cfdd15a9e063849ed437a49677e6fcc5b4198c76575be5",
        )?;
        std::fs::write(&archive, b"originaL")?;
        let error = verify_file_sha256(
            &archive,
            "0682c5f2076f099c34cfdd15a9e063849ed437a49677e6fcc5b4198c76575be5",
        )
        .expect_err("mutation must fail");
        assert!(error.to_string().contains("SHA-256 mismatch"));
        Ok(())
    }

    #[test]
    fn detached_signature_verification_fails_closed() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let archive = directory.path().join("adapter.tar.gz");
        let signature = directory.path().join("adapter.sig");
        let keyring = directory.path().join("keys.json");
        let signing_key = SigningKey::from_bytes(&[42; 32]);
        std::fs::write(&archive, b"signed archive")?;
        let write_signature = |bytes: &[u8], key_id: &str| -> anyhow::Result<()> {
            std::fs::write(
                &signature,
                serde_json::to_vec(&DetachedSignature {
                    algorithm: "Ed25519".to_owned(),
                    key_id: key_id.to_owned(),
                    signature: STANDARD.encode(signing_key.sign(bytes).to_bytes()),
                })?,
            )?;
            Ok(())
        };
        std::fs::write(
            &keyring,
            serde_json::to_vec(&ReleaseKeyring {
                keys: vec![ReleasePublicKey {
                    key_id: "release-2026".to_owned(),
                    public_key: STANDARD.encode(signing_key.verifying_key().to_bytes()),
                }],
            })?,
        )?;
        write_signature(b"signed archive", "release-2026")?;
        verify_detached_release_signature(&archive, &signature, &keyring, "release-2026")?;

        assert!(
            verify_detached_release_signature(&archive, &signature, &keyring, "unknown").is_err()
        );
        std::fs::write(&archive, b"changed archive")?;
        assert!(
            verify_detached_release_signature(&archive, &signature, &keyring, "release-2026")
                .is_err()
        );
        std::fs::write(&archive, b"signed archive")?;
        write_signature(b"different bytes", "release-2026")?;
        assert!(
            verify_detached_release_signature(&archive, &signature, &keyring, "release-2026")
                .is_err()
        );
        write_signature(b"signed archive", "wrong-key")?;
        assert!(
            verify_detached_release_signature(&archive, &signature, &keyring, "release-2026")
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn safe_extractor_rejects_traversal_and_links() -> anyhow::Result<()> {
        fn archive_with(path: &str, kind: tar::EntryType) -> anyhow::Result<Vec<u8>> {
            let mut encoded = Vec::new();
            {
                let encoder =
                    flate2::write::GzEncoder::new(&mut encoded, flate2::Compression::default());
                let mut builder = tar::Builder::new(encoder);
                let mut header = tar::Header::new_gnu();
                header.set_entry_type(kind);
                header.set_mode(0o644);
                header.set_size(if kind.is_file() { 1 } else { 0 });
                if kind.is_file() {
                    if path.contains("..") {
                        let bytes = header.as_mut_bytes();
                        bytes[..100].fill(0);
                        bytes[..path.len()].copy_from_slice(path.as_bytes());
                        header.set_cksum();
                        builder.append(&header, &b"x"[..])?;
                    } else {
                        header.set_cksum();
                        builder.append_data(&mut header, path, &b"x"[..])?;
                    }
                } else {
                    header.set_link_name("target")?;
                    header.set_cksum();
                    builder.append_data(&mut header, path, std::io::empty())?;
                }
                builder.into_inner()?.finish()?;
            }
            Ok(encoded)
        }
        let directory = tempfile::tempdir()?;
        let archive = directory.path().join("bad.tar.gz");
        std::fs::File::create(&archive)?
            .write_all(&archive_with("../escape", EntryType::Regular)?)?;
        assert!(extract_safe_tar_gz(&archive, &directory.path().join("out"), "fixture").is_err());
        std::fs::File::create(&archive)?
            .write_all(&archive_with("fixture/link", EntryType::Symlink)?)?;
        assert!(extract_safe_tar_gz(&archive, &directory.path().join("out2"), "fixture").is_err());
        Ok(())
    }

    #[test]
    fn typed_resource_manifest_validates_paths_harnesses_and_kinds() -> anyhow::Result<()> {
        let valid = r#"{
          "schema_version":1,
          "resources":[
            {"kind":"skill","harnesses":["codex","claude"],"source":"skills/research","destination":"research"},
            {"kind":"extension","harnesses":["pi"],"source":"extensions/research.ts","destination":"research.ts"}
          ]
        }"#;
        assert_eq!(parse_resource_manifest(valid)?.resources.len(), 2);
        for invalid in [
            valid.replace("research.ts", "../escape"),
            valid.replace("skills/research", "/absolute"),
            valid.replace("[\"codex\",\"claude\"]", "[]"),
            valid.replace("\"skill\"", "\"unknown\""),
            valid.replace("[\"pi\"]", "[\"codex\"]"),
        ] {
            assert!(parse_resource_manifest(&invalid).is_err());
        }
        Ok(())
    }
}
