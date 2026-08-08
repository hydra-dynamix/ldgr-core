use std::collections::{BTreeMap, HashMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, ensure, Context};
use semver::Version;
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};

use crate::release_index::{
    extract_safe_tar_gz, parse_detached_signature, parse_release_index, parse_release_keyring,
    validate_archive_root, validate_release_keyring, validate_sha256,
    verify_detached_signature_bytes, verify_file_sha256_for, AdapterReleaseIndex,
    DetachedSignature, ReleaseChannel, ReleaseKeyring, ReleasePublicKey, ADAPTER_RELEASE_INDEX_ENV,
    ADAPTER_RELEASE_KEYRING_ENV, DEFAULT_ADAPTER_RELEASE_INDEX_URL,
};
use crate::update::network::{
    CatalogFetch, UpdateNetworkClient, MAX_UPDATE_KEYRING_BYTES, MAX_UPDATE_SIGNATURE_BYTES,
};

pub const CORE_UPDATE_CATALOG_SCHEMA_VERSION: u32 = 1;
pub const CORE_RELEASE_METADATA_SCHEMA_VERSION: u32 = 1;
pub const CORE_UPDATE_INDEX_ENV: &str = "LDGR_CORE_UPDATE_INDEX";
pub const CORE_RELEASE_KEYRING_ENV: &str = "LDGR_CORE_RELEASE_KEYRING";
pub const DEFAULT_CORE_UPDATE_INDEX_URL: &str =
    "https://raw.githubusercontent.com/hydra-dynamix/ldgr-releases/main/core-index.json";
pub const LAUNCHER_COMPATIBILITY_SCHEMA_V1: &str = "ldgr.launcher-compatibility.v1";
pub const ERROR_RECOVERY_SCHEMA_VERSION: u32 = 1;

const EMBEDDED_CORE_RELEASE_KEYRING: &str = include_str!("../../release-keyring.json");
const MAX_RELEASE_METADATA_BYTES: u64 = 64 * 1024;
const SUPPORTED_PLATFORMS: [&str; 5] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
];

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreUpdateCatalog {
    pub schema_version: u32,
    #[serde(default)]
    pub release_keys: Vec<ReleasePublicKey>,
    pub releases: Vec<CoreRelease>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreRelease {
    pub version: String,
    pub channel: ReleaseChannel,
    pub minimum_updater_version: String,
    pub core_commit: String,
    pub source_repository: String,
    pub agentctl: PairedAgentctlRelease,
    pub compatibility: CoreReleaseCompatibility,
    pub platforms: Vec<CorePlatformArchive>,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PairedAgentctlRelease {
    pub version: String,
    pub repository: String,
    pub commit: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreReleaseCompatibility {
    pub launcher_compatibility_schema: String,
    pub error_recovery_schema: u32,
    pub release_metadata_schema: u32,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CorePlatformArchive {
    pub platform: String,
    pub archive_url: String,
    pub archive_root: String,
    pub sha256: String,
    pub signature_url: String,
    pub signing_key_id: String,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedCoreUpdateCatalog {
    pub catalog: CoreUpdateCatalog,
    pub catalog_signing_key_id: String,
    pub archive_keyring: ReleaseKeyring,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CoreCatalogFetch {
    Modified {
        verified: VerifiedCoreUpdateCatalog,
        etag: Option<String>,
    },
    NotModified {
        etag: Option<String>,
    },
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct VerifiedAdapterUpdateCatalog {
    pub catalog: AdapterReleaseIndex,
    pub catalog_signing_key_id: String,
    pub archive_keyring: ReleaseKeyring,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AdapterCatalogFetch {
    Modified {
        verified: VerifiedAdapterUpdateCatalog,
        etag: Option<String>,
    },
    NotModified {
        etag: Option<String>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedCoreRelease {
    pub version: Version,
    pub release: CoreRelease,
    pub platform: CorePlatformArchive,
}

#[derive(Clone, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreReleaseMetadata {
    pub schema_version: u32,
    pub package: String,
    pub binary: String,
    pub version: String,
    pub agentctl_version: String,
    pub agentctl_repository: String,
    pub agentctl_commit: String,
    pub launcher_compatibility_schema: String,
    pub error_recovery_schema: u32,
    pub platform: String,
    pub commit: String,
    pub source_repository: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CoreCatalogSources {
    pub index: String,
    pub signature: String,
    pub keyring: Option<String>,
    pub offline: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AdapterCatalogSources {
    pub index: String,
    pub signature: String,
    pub keyring: Option<String>,
    pub offline: bool,
}

impl CoreCatalogSources {
    pub fn new(
        index: impl Into<String>,
        keyring: Option<String>,
        offline: bool,
    ) -> anyhow::Result<Self> {
        let index = index.into();
        validate_source_location(&index, offline, "Core update index")?;
        let signature = format!("{index}.sig");
        validate_source_location(&signature, offline, "Core update index signature")?;
        if let Some(source) = keyring.as_deref() {
            validate_source_location(source, offline, "Core release keyring")?;
        }
        Ok(Self {
            index,
            signature,
            keyring,
            offline,
        })
    }

    pub fn configured(offline: bool) -> anyhow::Result<Self> {
        let index = std::env::var(CORE_UPDATE_INDEX_ENV)
            .unwrap_or_else(|_| DEFAULT_CORE_UPDATE_INDEX_URL.to_owned());
        let keyring = std::env::var(CORE_RELEASE_KEYRING_ENV).ok();
        Self::new(index, keyring, offline)
    }
}

impl AdapterCatalogSources {
    pub fn new(
        index: impl Into<String>,
        keyring: Option<String>,
        offline: bool,
    ) -> anyhow::Result<Self> {
        let index = index.into();
        validate_source_location(&index, offline, "adapter update index")?;
        let signature = format!("{index}.sig");
        validate_source_location(&signature, offline, "adapter update index signature")?;
        if let Some(source) = keyring.as_deref() {
            validate_source_location(source, offline, "adapter release keyring")?;
        }
        Ok(Self {
            index,
            signature,
            keyring,
            offline,
        })
    }

    pub fn configured(offline: bool) -> anyhow::Result<Self> {
        let index = std::env::var(ADAPTER_RELEASE_INDEX_ENV)
            .unwrap_or_else(|_| DEFAULT_ADAPTER_RELEASE_INDEX_URL.to_owned());
        let keyring = std::env::var(ADAPTER_RELEASE_KEYRING_ENV).ok();
        Self::new(index, keyring, offline)
    }
}

pub fn embedded_core_release_keyring() -> anyhow::Result<ReleaseKeyring> {
    parse_release_keyring(EMBEDDED_CORE_RELEASE_KEYRING)
        .context("embedded Core release keyring is invalid")
}

pub fn parse_core_update_catalog(json: &str) -> anyhow::Result<CoreUpdateCatalog> {
    let catalog: CoreUpdateCatalog =
        serde_json::from_str(json).context("failed to parse Core update catalog JSON")?;
    validate_core_update_catalog(&catalog)?;
    Ok(catalog)
}

pub fn canonical_catalog_bytes(catalog: &CoreUpdateCatalog) -> anyhow::Result<Vec<u8>> {
    let value = serde_json::to_value(catalog).context("failed to encode Core update catalog")?;
    let canonical = canonical_json_value(value);
    serde_json::to_vec(&canonical).context("failed to serialize canonical Core update catalog")
}

pub fn canonical_adapter_catalog_bytes(catalog: &AdapterReleaseIndex) -> anyhow::Result<Vec<u8>> {
    let value = serde_json::to_value(catalog).context("failed to encode adapter update catalog")?;
    let canonical = canonical_json_value(value);
    serde_json::to_vec(&canonical).context("failed to serialize canonical adapter update catalog")
}

pub fn verify_signed_adapter_update_catalog(
    catalog_json: &str,
    signature_json: &str,
    trusted_keyring: &ReleaseKeyring,
) -> anyhow::Result<VerifiedAdapterUpdateCatalog> {
    validate_release_keyring(trusted_keyring)
        .context("trusted adapter release keyring is invalid")?;
    let catalog = parse_release_index(catalog_json)?;
    let signature = parse_detached_signature(signature_json)?;
    let canonical = canonical_adapter_catalog_bytes(&catalog)?;
    verify_detached_signature_bytes(
        &canonical,
        &signature,
        trusted_keyring,
        &signature.key_id,
        "adapter update catalog",
    )?;
    Ok(VerifiedAdapterUpdateCatalog {
        catalog,
        catalog_signing_key_id: signature.key_id,
        archive_keyring: trusted_keyring.clone(),
    })
}

pub fn verify_signed_core_update_catalog(
    catalog_json: &str,
    signature_json: &str,
    trusted_keyring: &ReleaseKeyring,
) -> anyhow::Result<VerifiedCoreUpdateCatalog> {
    validate_release_keyring(trusted_keyring).context("trusted Core release keyring is invalid")?;
    let catalog = parse_core_update_catalog(catalog_json)?;
    let signature = parse_detached_signature(signature_json)?;
    let canonical = canonical_catalog_bytes(&catalog)?;
    verify_detached_signature_bytes(
        &canonical,
        &signature,
        trusted_keyring,
        &signature.key_id,
        "Core update catalog",
    )?;
    let archive_keyring = merge_archive_keyring(trusted_keyring, &catalog.release_keys)?;
    validate_archive_signing_keys(&catalog, &archive_keyring)?;
    Ok(VerifiedCoreUpdateCatalog {
        catalog,
        catalog_signing_key_id: signature.key_id,
        archive_keyring,
    })
}

pub fn load_local_signed_core_update_catalog(
    sources: &CoreCatalogSources,
) -> anyhow::Result<VerifiedCoreUpdateCatalog> {
    ensure!(
        !sources.index.starts_with("https://")
            && !sources.signature.starts_with("https://")
            && sources
                .keyring
                .as_deref()
                .is_none_or(|source| !source.starts_with("https://")),
        "local Core update catalog loader does not perform network access"
    );
    let client = UpdateNetworkClient::new(sources.offline)?;
    match fetch_signed_core_update_catalog(&client, sources, None)? {
        CoreCatalogFetch::Modified { verified, .. } => Ok(verified),
        CoreCatalogFetch::NotModified { .. } => {
            bail!("local Core update catalog unexpectedly returned not-modified")
        }
    }
}

pub fn fetch_signed_core_update_catalog(
    client: &UpdateNetworkClient,
    sources: &CoreCatalogSources,
    previous_etag: Option<&str>,
) -> anyhow::Result<CoreCatalogFetch> {
    let (catalog_bytes, etag) = match client.fetch_catalog(&sources.index, previous_etag)? {
        CatalogFetch::Modified { bytes, etag } => (bytes, etag),
        CatalogFetch::NotModified { etag } => {
            return Ok(CoreCatalogFetch::NotModified { etag });
        }
    };
    let signature_bytes = client.fetch_bounded(
        &sources.signature,
        MAX_UPDATE_SIGNATURE_BYTES,
        "Core update index signature",
    )?;
    let catalog_json =
        String::from_utf8(catalog_bytes).context("Core update catalog response is not UTF-8")?;
    let signature_json = String::from_utf8(signature_bytes)
        .context("Core update catalog signature response is not UTF-8")?;
    let trusted_keyring = match sources.keyring.as_deref() {
        Some(source) => {
            let bytes =
                client.fetch_bounded(source, MAX_UPDATE_KEYRING_BYTES, "Core release keyring")?;
            let text = String::from_utf8(bytes).context("Core release keyring is not UTF-8")?;
            parse_release_keyring(&text)?
        }
        None => embedded_core_release_keyring()?,
    };
    let verified =
        verify_signed_core_update_catalog(&catalog_json, &signature_json, &trusted_keyring)
            .with_context(|| format!("untrusted Core update catalog from {}", sources.index))?;
    Ok(CoreCatalogFetch::Modified { verified, etag })
}

pub fn fetch_signed_adapter_update_catalog(
    client: &UpdateNetworkClient,
    sources: &AdapterCatalogSources,
    previous_etag: Option<&str>,
) -> anyhow::Result<AdapterCatalogFetch> {
    let (catalog_bytes, etag) = match client.fetch_catalog(&sources.index, previous_etag)? {
        CatalogFetch::Modified { bytes, etag } => (bytes, etag),
        CatalogFetch::NotModified { etag } => {
            return Ok(AdapterCatalogFetch::NotModified { etag });
        }
    };
    let signature_bytes = client.fetch_bounded(
        &sources.signature,
        MAX_UPDATE_SIGNATURE_BYTES,
        "adapter update index signature",
    )?;
    let catalog_json =
        String::from_utf8(catalog_bytes).context("adapter update catalog is not UTF-8")?;
    let signature_json = String::from_utf8(signature_bytes)
        .context("adapter update catalog signature is not UTF-8")?;
    let trusted_keyring = match sources.keyring.as_deref() {
        Some(source) => {
            let bytes = client.fetch_bounded(
                source,
                MAX_UPDATE_KEYRING_BYTES,
                "adapter release keyring",
            )?;
            let text = String::from_utf8(bytes).context("adapter release keyring is not UTF-8")?;
            parse_release_keyring(&text)?
        }
        None => embedded_core_release_keyring()?,
    };
    let verified =
        verify_signed_adapter_update_catalog(&catalog_json, &signature_json, &trusted_keyring)
            .with_context(|| format!("untrusted adapter update catalog from {}", sources.index))?;
    Ok(AdapterCatalogFetch::Modified { verified, etag })
}

pub fn resolve_newer_core_release(
    verified: &VerifiedCoreUpdateCatalog,
    current_core: &Version,
    updater_version: &Version,
    platform: &str,
    include_prerelease: bool,
) -> anyhow::Result<Option<ResolvedCoreRelease>> {
    validate_platform(platform, "requested platform")?;
    let mut candidates = verified
        .catalog
        .releases
        .iter()
        .filter_map(|release| {
            let version = Version::parse(&release.version).ok()?;
            let minimum_updater = Version::parse(&release.minimum_updater_version).ok()?;
            let channel_allowed = release.channel == ReleaseChannel::Stable || include_prerelease;
            let compatibility_supported = release.compatibility.launcher_compatibility_schema
                == LAUNCHER_COMPATIBILITY_SCHEMA_V1
                && release.compatibility.error_recovery_schema == ERROR_RECOVERY_SCHEMA_VERSION
                && release.compatibility.release_metadata_schema
                    == CORE_RELEASE_METADATA_SCHEMA_VERSION;
            let platform = release
                .platforms
                .iter()
                .find(|archive| archive.platform == platform)?;
            (version > *current_core
                && minimum_updater <= *updater_version
                && channel_allowed
                && compatibility_supported)
                .then_some((version, release, platform))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    Ok(candidates
        .into_iter()
        .next()
        .map(|(version, release, platform)| ResolvedCoreRelease {
            version,
            release: release.clone(),
            platform: platform.clone(),
        }))
}

pub fn verify_resolved_core_archive_signature(
    archive_path: &Path,
    signature_path: &Path,
    resolved: &ResolvedCoreRelease,
    verified: &VerifiedCoreUpdateCatalog,
) -> anyhow::Result<()> {
    let archive = fs::read(archive_path)
        .with_context(|| format!("failed to read Core archive {}", archive_path.display()))?;
    let signature_text = fs::read_to_string(signature_path).with_context(|| {
        format!(
            "failed to read Core archive signature {}",
            signature_path.display()
        )
    })?;
    let signature: DetachedSignature = parse_detached_signature(&signature_text)?;
    verify_detached_signature_bytes(
        &archive,
        &signature,
        &verified.archive_keyring,
        &resolved.platform.signing_key_id,
        "Core release archive",
    )
}

pub fn extract_bound_core_archive(
    archive_path: &Path,
    destination: &Path,
    resolved: &ResolvedCoreRelease,
) -> anyhow::Result<PathBuf> {
    verify_file_sha256_for(
        archive_path,
        &resolved.platform.sha256,
        "Core release archive",
    )?;
    extract_safe_tar_gz(archive_path, destination, &resolved.platform.archive_root)?;
    let extracted_root = destination.join(&resolved.platform.archive_root);
    verify_release_metadata_binding(&extracted_root, resolved)?;
    Ok(extracted_root)
}

pub fn read_release_metadata(extracted_root: &Path) -> anyhow::Result<CoreReleaseMetadata> {
    let metadata_path = extracted_root.join("RELEASE-METADATA.json");
    let metadata = fs::symlink_metadata(&metadata_path).with_context(|| {
        format!(
            "Core archive is missing release metadata {}",
            metadata_path.display()
        )
    })?;
    ensure!(
        metadata.file_type().is_file(),
        "Core release metadata must be a regular file"
    );
    let text = read_bounded_utf8(
        &metadata_path,
        MAX_RELEASE_METADATA_BYTES,
        "Core release metadata",
    )?;
    serde_json::from_str(&text).context("Core release metadata is not valid schema-v1 JSON")
}

pub fn verify_release_metadata_binding(
    extracted_root: &Path,
    resolved: &ResolvedCoreRelease,
) -> anyhow::Result<CoreReleaseMetadata> {
    ensure!(
        extracted_root.file_name().and_then(|name| name.to_str())
            == Some(resolved.platform.archive_root.as_str()),
        "extracted Core archive root does not match catalog archive_root `{}`",
        resolved.platform.archive_root
    );
    let metadata = read_release_metadata(extracted_root)?;
    ensure_equal(
        metadata.schema_version,
        resolved.release.compatibility.release_metadata_schema,
        "release metadata schema_version",
    )?;
    ensure_equal(
        metadata.package.as_str(),
        "ldgr-core",
        "release metadata package",
    )?;
    ensure_equal(metadata.binary.as_str(), "ldgr", "release metadata binary")?;
    ensure_equal(
        metadata.version.as_str(),
        resolved.release.version.as_str(),
        "release metadata Core version",
    )?;
    ensure_equal(
        metadata.agentctl_version.as_str(),
        resolved.release.agentctl.version.as_str(),
        "release metadata agentctl version",
    )?;
    ensure_equal(
        metadata.agentctl_repository.as_str(),
        resolved.release.agentctl.repository.as_str(),
        "release metadata agentctl repository",
    )?;
    ensure_equal(
        metadata.agentctl_commit.as_str(),
        resolved.release.agentctl.commit.as_str(),
        "release metadata agentctl commit",
    )?;
    ensure_equal(
        metadata.launcher_compatibility_schema.as_str(),
        resolved
            .release
            .compatibility
            .launcher_compatibility_schema
            .as_str(),
        "release metadata launcher compatibility schema",
    )?;
    ensure_equal(
        metadata.error_recovery_schema,
        resolved.release.compatibility.error_recovery_schema,
        "release metadata error recovery schema",
    )?;
    ensure_equal(
        metadata.platform.as_str(),
        resolved.platform.platform.as_str(),
        "release metadata platform",
    )?;
    ensure_equal(
        metadata.commit.as_str(),
        resolved.release.core_commit.as_str(),
        "release metadata Core commit",
    )?;
    ensure_equal(
        metadata.source_repository.as_str(),
        resolved.release.source_repository.as_str(),
        "release metadata source repository",
    )?;

    let platform_root = extracted_root.join(&resolved.platform.platform);
    ensure!(
        platform_root.is_dir(),
        "Core archive is missing platform directory `{}`",
        resolved.platform.platform
    );
    let extension = if resolved.platform.platform.starts_with("windows-") {
        ".exe"
    } else {
        ""
    };
    for binary in [format!("ldgr{extension}"), format!("agentctl{extension}")] {
        ensure!(
            platform_root.join(&binary).is_file(),
            "Core archive is missing paired binary `{binary}`"
        );
    }
    Ok(metadata)
}

pub fn validate_core_update_catalog(catalog: &CoreUpdateCatalog) -> anyhow::Result<()> {
    ensure!(
        catalog.schema_version == CORE_UPDATE_CATALOG_SCHEMA_VERSION,
        "unsupported Core update catalog schema_version {}; expected {}",
        catalog.schema_version,
        CORE_UPDATE_CATALOG_SCHEMA_VERSION
    );
    ensure!(
        !catalog.releases.is_empty(),
        "Core update catalog must contain at least one release"
    );
    if !catalog.release_keys.is_empty() {
        validate_release_keyring(&ReleaseKeyring {
            keys: catalog.release_keys.clone(),
        })
        .context("catalog release_keys are invalid")?;
    }

    let mut versions = HashSet::new();
    for (release_index, release) in catalog.releases.iter().enumerate() {
        let path = format!("releases[{release_index}]");
        let version = parse_version(&release.version, &format!("{path}.version"))?;
        ensure!(
            versions.insert(version.clone()),
            "duplicate Core release version `{version}`"
        );
        match release.channel {
            ReleaseChannel::Stable => ensure!(
                version.pre.is_empty(),
                "{path}.channel stable cannot relabel prerelease version `{version}`"
            ),
            ReleaseChannel::Prerelease => ensure!(
                !version.pre.is_empty(),
                "{path}.channel prerelease cannot relabel stable version `{version}`"
            ),
        }
        let minimum = parse_version(
            &release.minimum_updater_version,
            &format!("{path}.minimum_updater_version"),
        )?;
        ensure!(
            minimum <= version,
            "{path}.minimum_updater_version cannot exceed the target Core version"
        );
        validate_commit(&release.core_commit, &format!("{path}.core_commit"))?;
        validate_repository(
            &release.source_repository,
            &format!("{path}.source_repository"),
        )?;
        parse_version(
            &release.agentctl.version,
            &format!("{path}.agentctl.version"),
        )?;
        validate_repository(
            &release.agentctl.repository,
            &format!("{path}.agentctl.repository"),
        )?;
        validate_commit(&release.agentctl.commit, &format!("{path}.agentctl.commit"))?;
        require_text(
            &release.compatibility.launcher_compatibility_schema,
            &format!("{path}.compatibility.launcher_compatibility_schema"),
        )?;
        ensure!(
            release.compatibility.error_recovery_schema > 0,
            "{path}.compatibility.error_recovery_schema must be positive"
        );
        ensure!(
            release.compatibility.release_metadata_schema > 0,
            "{path}.compatibility.release_metadata_schema must be positive"
        );
        ensure!(
            !release.platforms.is_empty(),
            "{path}.platforms must not be empty"
        );
        let expected_root = format!("ldgr-core-{version}");
        let mut platforms = HashSet::new();
        for (platform_index, archive) in release.platforms.iter().enumerate() {
            let archive_path = format!("{path}.platforms[{platform_index}]");
            validate_platform(&archive.platform, &format!("{archive_path}.platform"))?;
            ensure!(
                platforms.insert(archive.platform.as_str()),
                "duplicate platform `{}` for Core {version}",
                archive.platform
            );
            validate_archive_root(&archive.archive_root)?;
            ensure!(
                archive.archive_root == expected_root,
                "{archive_path}.archive_root must be `{expected_root}` to bind the Core version"
            );
            validate_artifact_url(&archive.archive_url, &format!("{archive_path}.archive_url"))?;
            validate_sha256(&archive.sha256)
                .with_context(|| format!("invalid {archive_path}.sha256"))?;
            ensure!(
                archive
                    .sha256
                    .bytes()
                    .all(|byte| !byte.is_ascii_uppercase()),
                "{archive_path}.sha256 must use canonical lowercase hexadecimal"
            );
            validate_artifact_url(
                &archive.signature_url,
                &format!("{archive_path}.signature_url"),
            )?;
            require_text(
                &archive.signing_key_id,
                &format!("{archive_path}.signing_key_id"),
            )?;
        }
    }
    Ok(())
}

fn merge_archive_keyring(
    trusted: &ReleaseKeyring,
    successors: &[ReleasePublicKey],
) -> anyhow::Result<ReleaseKeyring> {
    let mut keys = trusted.keys.clone();
    let mut by_id = keys
        .iter()
        .map(|key| (key.key_id.clone(), key.public_key.clone()))
        .collect::<HashMap<_, _>>();
    for successor in successors {
        match by_id.get(&successor.key_id) {
            Some(existing) => ensure!(
                existing == &successor.public_key,
                "catalog release key `{}` conflicts with an embedded trusted key",
                successor.key_id
            ),
            None => {
                by_id.insert(successor.key_id.clone(), successor.public_key.clone());
                keys.push(successor.clone());
            }
        }
    }
    let keyring = ReleaseKeyring { keys };
    validate_release_keyring(&keyring)?;
    Ok(keyring)
}

fn validate_archive_signing_keys(
    catalog: &CoreUpdateCatalog,
    keyring: &ReleaseKeyring,
) -> anyhow::Result<()> {
    let trusted = keyring
        .keys
        .iter()
        .map(|key| key.key_id.as_str())
        .collect::<HashSet<_>>();
    for release in &catalog.releases {
        for archive in &release.platforms {
            ensure!(
                trusted.contains(archive.signing_key_id.as_str()),
                "Core {} archive for {} names unknown signing key `{}`",
                release.version,
                archive.platform,
                archive.signing_key_id
            );
        }
    }
    Ok(())
}

fn canonical_json_value(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(values) => {
            JsonValue::Array(values.into_iter().map(canonical_json_value).collect())
        }
        JsonValue::Object(values) => {
            let sorted = values
                .into_iter()
                .map(|(key, value)| (key, canonical_json_value(value)))
                .collect::<BTreeMap<_, _>>();
            JsonValue::Object(JsonMap::from_iter(sorted))
        }
        value => value,
    }
}

fn validate_source_location(source: &str, offline: bool, label: &str) -> anyhow::Result<()> {
    if source.starts_with("https://") {
        ensure!(!offline, "offline mode forbids remote {label} sources");
        validate_https_url(source, label)
    } else if let Some(path) = source.strip_prefix("file://") {
        ensure!(!path.is_empty(), "{label} file URL must contain a path");
        Ok(())
    } else if source.contains("://") {
        bail!("{label} must use HTTPS; unsupported source `{source}`")
    } else if offline {
        ensure!(!source.trim().is_empty(), "{label} path must not be empty");
        Ok(())
    } else {
        bail!("local {label} paths require file:// or --offline")
    }
}

fn validate_artifact_url(url: &str, field: &str) -> anyhow::Result<()> {
    if url.starts_with("https://") {
        validate_https_url(url, field)
    } else if let Some(path) = url.strip_prefix("file://") {
        ensure!(!path.is_empty(), "{field} file URL must contain a path");
        Ok(())
    } else {
        bail!("{field} must use https:// or an explicit file:// fixture URL")
    }
}

fn validate_https_url(url: &str, field: &str) -> anyhow::Result<()> {
    let parsed = reqwest::Url::parse(url).with_context(|| format!("{field} is not a valid URL"))?;
    ensure!(parsed.scheme() == "https", "{field} must use HTTPS");
    ensure!(parsed.host_str().is_some(), "{field} must include a host");
    ensure!(
        parsed.username().is_empty() && parsed.password().is_none(),
        "{field} must not contain credentials"
    );
    ensure!(
        parsed.fragment().is_none(),
        "{field} must not contain a fragment"
    );
    Ok(())
}

fn validate_platform(platform: &str, field: &str) -> anyhow::Result<()> {
    ensure!(
        SUPPORTED_PLATFORMS.contains(&platform),
        "{field} `{platform}` is not a supported Core update platform"
    );
    Ok(())
}

fn validate_commit(commit: &str, field: &str) -> anyhow::Result<()> {
    ensure!(
        commit.len() == 40
            && commit
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)),
        "{field} must be a 40-character lowercase hexadecimal Git commit"
    );
    Ok(())
}

fn validate_repository(repository: &str, field: &str) -> anyhow::Result<()> {
    let mut parts = repository.split('/');
    let owner = parts.next().unwrap_or_default();
    let name = parts.next().unwrap_or_default();
    ensure!(
        !owner.is_empty() && !name.is_empty() && parts.next().is_none(),
        "{field} must use canonical owner/repository form"
    );
    ensure!(
        owner
            .bytes()
            .chain(name.bytes())
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')),
        "{field} contains unsupported repository characters"
    );
    Ok(())
}

fn parse_version(value: &str, field: &str) -> anyhow::Result<Version> {
    Version::parse(value).with_context(|| format!("{field} must be a semantic version"))
}

fn require_text(value: &str, field: &str) -> anyhow::Result<()> {
    ensure!(!value.trim().is_empty(), "{field} must not be empty");
    Ok(())
}

fn read_bounded_utf8(path: &Path, maximum: u64, label: &str) -> anyhow::Result<String> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(metadata.is_file(), "{label} must be a regular file");
    ensure!(
        metadata.len() <= maximum,
        "{label} exceeds the {maximum}-byte local size limit"
    );
    fs::read_to_string(path)
        .with_context(|| format!("failed to read {label} {} as UTF-8", path.display()))
}

fn ensure_equal<T>(actual: T, expected: T, field: &str) -> anyhow::Result<()>
where
    T: PartialEq + std::fmt::Debug,
{
    ensure!(
        actual == expected,
        "{field} does not match signed catalog: expected {expected:?}, got {actual:?}"
    );
    Ok(())
}
#[cfg(test)]
mod tests {
    use std::fs;

    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};
    use semver::Version;
    use sha2::{Digest, Sha256};

    use super::*;

    const CATALOG_FIXTURE: &str = include_str!("../../tests/fixtures/core-update/catalog.json");
    const METADATA_FIXTURE: &str =
        include_str!("../../tests/fixtures/core-update/RELEASE-METADATA.json");
    static ENVIRONMENT_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    fn signing_key(byte: u8) -> SigningKey {
        SigningKey::from_bytes(&[byte; 32])
    }

    fn keyring(key_id: &str, key: &SigningKey) -> ReleaseKeyring {
        ReleaseKeyring {
            keys: vec![ReleasePublicKey {
                key_id: key_id.to_owned(),
                public_key: STANDARD.encode(key.verifying_key().to_bytes()),
            }],
        }
    }

    fn signature(catalog: &CoreUpdateCatalog, key_id: &str, key: &SigningKey) -> String {
        serde_json::to_string(&DetachedSignature {
            algorithm: "Ed25519".to_owned(),
            key_id: key_id.to_owned(),
            signature: STANDARD.encode(
                key.sign(&canonical_catalog_bytes(catalog).unwrap())
                    .to_bytes(),
            ),
        })
        .unwrap()
    }

    fn adapter_signature(catalog: &AdapterReleaseIndex, key_id: &str, key: &SigningKey) -> String {
        serde_json::to_string(&DetachedSignature {
            algorithm: "Ed25519".to_owned(),
            key_id: key_id.to_owned(),
            signature: STANDARD.encode(
                key.sign(&canonical_adapter_catalog_bytes(catalog).unwrap())
                    .to_bytes(),
            ),
        })
        .unwrap()
    }

    fn verified_fixture() -> anyhow::Result<VerifiedCoreUpdateCatalog> {
        let catalog = parse_core_update_catalog(CATALOG_FIXTURE)?;
        let anchor = signing_key(42);
        verify_signed_core_update_catalog(
            CATALOG_FIXTURE,
            &signature(&catalog, "fixture-anchor", &anchor),
            &keyring("fixture-anchor", &anchor),
        )
    }

    #[test]
    fn parses_complete_core_catalog_metadata() -> anyhow::Result<()> {
        let catalog = parse_core_update_catalog(CATALOG_FIXTURE)?;
        let release = &catalog.releases[0];
        assert_eq!(release.version, "0.1.15");
        assert_eq!(release.channel, ReleaseChannel::Stable);
        assert_eq!(release.agentctl.version, "0.1.2");
        assert_eq!(
            release.compatibility.launcher_compatibility_schema,
            LAUNCHER_COMPATIBILITY_SCHEMA_V1
        );
        assert_eq!(release.platforms[0].platform, "linux-x86_64");
        assert_eq!(release.platforms[0].archive_root, "ldgr-core-0.1.15");
        Ok(())
    }

    #[test]
    fn adapter_catalog_snapshot_requires_a_trusted_canonical_signature() -> anyhow::Result<()> {
        let text = include_str!("../../tests/fixtures/release-index/open-and-commercial.json");
        let catalog = parse_release_index(text)?;
        let anchor = signing_key(19);
        let signature = adapter_signature(&catalog, "adapter-anchor", &anchor);
        let verified = verify_signed_adapter_update_catalog(
            text,
            &signature,
            &keyring("adapter-anchor", &anchor),
        )?;
        assert_eq!(verified.catalog, catalog);
        assert_eq!(verified.catalog_signing_key_id, "adapter-anchor");

        let mut tampered = catalog;
        tampered.adapters[0].title.push_str(" tampered");
        assert!(verify_signed_adapter_update_catalog(
            &serde_json::to_string(&tampered)?,
            &signature,
            &keyring("adapter-anchor", &anchor),
        )
        .unwrap_err()
        .to_string()
        .contains("did not verify"));
        Ok(())
    }

    #[test]
    fn verifies_canonical_catalog_and_rejects_signed_field_tampering() -> anyhow::Result<()> {
        let catalog = parse_core_update_catalog(CATALOG_FIXTURE)?;
        let anchor = signing_key(42);
        let signature = signature(&catalog, "fixture-anchor", &anchor);
        let trusted = keyring("fixture-anchor", &anchor);
        let reformatted = serde_json::to_string_pretty(&catalog)?;
        let verified = verify_signed_core_update_catalog(&reformatted, &signature, &trusted)?;
        assert_eq!(verified.catalog_signing_key_id, "fixture-anchor");

        let tampered = CATALOG_FIXTURE.replace(
            "1111111111111111111111111111111111111111",
            "3333333333333333333333333333333333333333",
        );
        let error = verify_signed_core_update_catalog(&tampered, &signature, &trusted)
            .expect_err("signed Core commit mutation must fail");
        assert!(format!("{error:#}").contains("signature did not verify"));
        Ok(())
    }

    #[test]
    fn anchored_catalog_can_add_successor_but_successor_cannot_authorize_itself(
    ) -> anyhow::Result<()> {
        let anchor = signing_key(42);
        let successor = signing_key(43);
        let mut catalog = parse_core_update_catalog(CATALOG_FIXTURE)?;
        catalog.release_keys.push(ReleasePublicKey {
            key_id: "fixture-successor".to_owned(),
            public_key: STANDARD.encode(successor.verifying_key().to_bytes()),
        });
        catalog.releases[0].platforms[0].signing_key_id = "fixture-successor".to_owned();
        let catalog_json = serde_json::to_string_pretty(&catalog)?;
        let trusted = keyring("fixture-anchor", &anchor);
        let verified = verify_signed_core_update_catalog(
            &catalog_json,
            &signature(&catalog, "fixture-anchor", &anchor),
            &trusted,
        )?;
        assert!(verified
            .archive_keyring
            .keys
            .iter()
            .any(|key| key.key_id == "fixture-successor"));

        let error = verify_signed_core_update_catalog(
            &catalog_json,
            &signature(&catalog, "fixture-successor", &successor),
            &trusted,
        )
        .expect_err("a catalog key must not authorize its own introduction");
        assert!(format!("{error:#}").contains("unknown release signing key"));
        Ok(())
    }

    #[test]
    fn rejects_relabels_duplicates_unknown_fields_and_unsafe_metadata() -> anyhow::Result<()> {
        let catalog = parse_core_update_catalog(CATALOG_FIXTURE)?;

        let mut duplicate_version = catalog.clone();
        duplicate_version
            .releases
            .push(duplicate_version.releases[0].clone());
        assert!(validate_core_update_catalog(&duplicate_version)
            .unwrap_err()
            .to_string()
            .contains("duplicate Core release version"));

        let mut duplicate_platform = catalog.clone();
        let platform = duplicate_platform.releases[0].platforms[0].clone();
        duplicate_platform.releases[0].platforms.push(platform);
        assert!(validate_core_update_catalog(&duplicate_platform)
            .unwrap_err()
            .to_string()
            .contains("duplicate platform"));

        let mut relabeled = catalog.clone();
        relabeled.releases[0].channel = ReleaseChannel::Prerelease;
        assert!(validate_core_update_catalog(&relabeled)
            .unwrap_err()
            .to_string()
            .contains("cannot relabel stable"));

        let mut wrong_root = catalog.clone();
        wrong_root.releases[0].platforms[0].archive_root = "ldgr-core-9.9.9".to_owned();
        assert!(validate_core_update_catalog(&wrong_root)
            .unwrap_err()
            .to_string()
            .contains("bind the Core version"));

        let mut insecure = catalog.clone();
        insecure.releases[0].platforms[0].archive_url =
            "http://example.invalid/core.tar.gz".to_owned();
        assert!(validate_core_update_catalog(&insecure)
            .unwrap_err()
            .to_string()
            .contains("https://"));

        let mut invalid_platform = catalog.clone();
        invalid_platform.releases[0].platforms[0].platform = "freebsd-x86_64".to_owned();
        assert!(validate_core_update_catalog(&invalid_platform)
            .unwrap_err()
            .to_string()
            .contains("not a supported"));

        let unknown = CATALOG_FIXTURE.replacen(
            "\"schema_version\": 1,",
            "\"schema_version\": 1, \"untrusted_extension\": true,",
            1,
        );
        let error = parse_core_update_catalog(&unknown).unwrap_err();
        assert!(format!("{error:#}").contains("unknown field"));
        Ok(())
    }

    #[test]
    fn resolution_is_strictly_newer_and_channel_aware() -> anyhow::Result<()> {
        let base = verified_fixture()?;
        let current = Version::parse("0.1.14")?;
        let resolved =
            resolve_newer_core_release(&base, &current, &current, "linux-x86_64", false)?
                .expect("fixture is newer");
        assert_eq!(resolved.version, Version::parse("0.1.15")?);
        assert!(resolve_newer_core_release(
            &base,
            &current,
            &Version::parse("0.1.13")?,
            "linux-x86_64",
            true,
        )?
        .is_none());
        assert!(resolve_newer_core_release(
            &base,
            &Version::parse("0.1.15")?,
            &current,
            "linux-x86_64",
            true,
        )?
        .is_none());

        let mut catalog = base.catalog.clone();
        let mut prerelease = catalog.releases[0].clone();
        prerelease.version = "0.2.0-alpha.1".to_owned();
        prerelease.channel = ReleaseChannel::Prerelease;
        prerelease.platforms[0].archive_root = "ldgr-core-0.2.0-alpha.1".to_owned();
        catalog.releases.push(prerelease);
        let anchor = signing_key(42);
        let verified = verify_signed_core_update_catalog(
            &serde_json::to_string(&catalog)?,
            &signature(&catalog, "fixture-anchor", &anchor),
            &keyring("fixture-anchor", &anchor),
        )?;
        assert_eq!(
            resolve_newer_core_release(&verified, &current, &current, "linux-x86_64", false,)?
                .unwrap()
                .version,
            Version::parse("0.1.15")?
        );
        assert_eq!(
            resolve_newer_core_release(&verified, &current, &current, "linux-x86_64", true,)?
                .unwrap()
                .version,
            Version::parse("0.2.0-alpha.1")?
        );
        Ok(())
    }

    #[test]
    fn local_fixture_loader_requires_explicit_control_and_verifies_signature() -> anyhow::Result<()>
    {
        let directory = tempfile::tempdir()?;
        let catalog_path = directory.path().join("core-index.json");
        let signature_path = directory.path().join("core-index.json.sig");
        let keyring_path = directory.path().join("keys.json");
        let catalog = parse_core_update_catalog(CATALOG_FIXTURE)?;
        let anchor = signing_key(42);
        fs::write(&catalog_path, CATALOG_FIXTURE)?;
        fs::write(
            &signature_path,
            signature(&catalog, "fixture-anchor", &anchor),
        )?;
        fs::write(
            &keyring_path,
            serde_json::to_vec(&keyring("fixture-anchor", &anchor))?,
        )?;

        let sources = CoreCatalogSources::new(
            format!("file://{}", catalog_path.display()),
            Some(format!("file://{}", keyring_path.display())),
            false,
        )?;
        let verified = load_local_signed_core_update_catalog(&sources)?;
        assert_eq!(verified.catalog.releases[0].version, "0.1.15");

        let configured = {
            let _guard = ENVIRONMENT_LOCK.lock().expect("environment lock");
            let previous_index = std::env::var_os(CORE_UPDATE_INDEX_ENV);
            let previous_keyring = std::env::var_os(CORE_RELEASE_KEYRING_ENV);
            std::env::set_var(CORE_UPDATE_INDEX_ENV, &sources.index);
            std::env::set_var(
                CORE_RELEASE_KEYRING_ENV,
                sources.keyring.as_deref().expect("fixture keyring"),
            );
            let configured = CoreCatalogSources::configured(false);
            match previous_index {
                Some(value) => std::env::set_var(CORE_UPDATE_INDEX_ENV, value),
                None => std::env::remove_var(CORE_UPDATE_INDEX_ENV),
            }
            match previous_keyring {
                Some(value) => std::env::set_var(CORE_RELEASE_KEYRING_ENV, value),
                None => std::env::remove_var(CORE_RELEASE_KEYRING_ENV),
            }
            configured?
        };
        assert_eq!(configured, sources);

        assert!(
            CoreCatalogSources::new(catalog_path.display().to_string(), None, false)
                .unwrap_err()
                .to_string()
                .contains("file:// or --offline")
        );
        assert!(
            CoreCatalogSources::new("http://example.invalid/core-index.json", None, false)
                .unwrap_err()
                .to_string()
                .contains("must use HTTPS")
        );
        assert!(
            CoreCatalogSources::new("https://example.invalid/core-index.json", None, true)
                .unwrap_err()
                .to_string()
                .contains("offline mode")
        );
        let remote =
            CoreCatalogSources::new("https://example.invalid/core-index.json", None, false)?;
        assert!(load_local_signed_core_update_catalog(&remote)
            .unwrap_err()
            .to_string()
            .contains("does not perform network access"));
        let offline = CoreCatalogSources::new(
            catalog_path.display().to_string(),
            Some(keyring_path.display().to_string()),
            true,
        )?;
        assert_eq!(
            load_local_signed_core_update_catalog(&offline)?
                .catalog
                .releases[0]
                .version,
            "0.1.15"
        );
        Ok(())
    }

    #[test]
    fn archive_signature_extraction_and_release_metadata_are_catalog_bound() -> anyhow::Result<()> {
        let verified = verified_fixture()?;
        let mut resolved = resolve_newer_core_release(
            &verified,
            &Version::parse("0.1.14")?,
            &Version::parse("0.1.14")?,
            "linux-x86_64",
            false,
        )?
        .unwrap();
        let directory = tempfile::tempdir()?;
        let source_root = directory.path().join(&resolved.platform.archive_root);
        let platform_root = source_root.join("linux-x86_64");
        fs::create_dir_all(&platform_root)?;
        fs::write(source_root.join("RELEASE-METADATA.json"), METADATA_FIXTURE)?;
        fs::write(platform_root.join("ldgr"), b"core")?;
        fs::write(platform_root.join("agentctl"), b"agentctl")?;

        let archive_path = directory.path().join("core.tar.gz");
        let archive_file = fs::File::create(&archive_path)?;
        let encoder = flate2::write::GzEncoder::new(archive_file, flate2::Compression::default());
        let mut builder = tar::Builder::new(encoder);
        builder.append_dir_all(&resolved.platform.archive_root, &source_root)?;
        builder.into_inner()?.finish()?;
        let archive_bytes = fs::read(&archive_path)?;
        resolved.platform.sha256 = format!("{:x}", Sha256::digest(&archive_bytes));

        let anchor = signing_key(42);
        let signature_path = directory.path().join("core.tar.gz.sig");
        fs::write(
            &signature_path,
            serde_json::to_vec(&DetachedSignature {
                algorithm: "Ed25519".to_owned(),
                key_id: "fixture-anchor".to_owned(),
                signature: STANDARD.encode(anchor.sign(&archive_bytes).to_bytes()),
            })?,
        )?;
        verify_resolved_core_archive_signature(
            &archive_path,
            &signature_path,
            &resolved,
            &verified,
        )?;
        let extracted = extract_bound_core_archive(
            &archive_path,
            &directory.path().join("extracted"),
            &resolved,
        )?;
        assert_eq!(
            verify_release_metadata_binding(&extracted, &resolved)?.version,
            "0.1.15"
        );

        let mut metadata: serde_json::Value = serde_json::from_str(METADATA_FIXTURE)?;
        metadata["version"] = "9.9.9".into();
        fs::write(
            extracted.join("RELEASE-METADATA.json"),
            serde_json::to_vec(&metadata)?,
        )?;
        assert!(verify_release_metadata_binding(&extracted, &resolved)
            .unwrap_err()
            .to_string()
            .contains("Core version"));
        Ok(())
    }

    #[test]
    fn embedded_trust_roots_are_valid_and_nonempty() -> anyhow::Result<()> {
        let keyring = embedded_core_release_keyring()?;
        assert!(!keyring.keys.is_empty());
        Ok(())
    }
}
