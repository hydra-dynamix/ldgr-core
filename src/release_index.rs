use std::collections::HashMap;
use std::fs;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signature, Verifier, VerifyingKey};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tar::EntryType;

pub const ADAPTER_RELEASE_INDEX_SCHEMA_VERSION: u32 = 1;
pub const ADAPTER_RELEASE_INDEX_ENV: &str = "LDGR_ADAPTER_INDEX";
pub const ADAPTER_RELEASE_KEYRING_ENV: &str = "LDGR_ADAPTER_RELEASE_KEYRING";
pub const DEFAULT_ADAPTER_RELEASE_INDEX_URL: &str =
    "https://raw.githubusercontent.com/hydra-dynamix/ldgr-releases/main/index.json";

pub fn load_configured_release_index() -> anyhow::Result<AdapterReleaseIndex> {
    let source = std::env::var(ADAPTER_RELEASE_INDEX_ENV)
        .unwrap_or_else(|_| DEFAULT_ADAPTER_RELEASE_INDEX_URL.to_owned());
    load_release_index(&source)
}

pub fn load_release_index(source: &str) -> anyhow::Result<AdapterReleaseIndex> {
    let text = if source.starts_with("https://") {
        let output = Command::new("curl")
            .args(["-fsSL", source])
            .output()
            .with_context(|| format!("failed to execute curl for adapter index {source}"))?;
        if !output.status.success() {
            bail!(
                "failed to download adapter release index {source}: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            );
        }
        String::from_utf8(output.stdout).context("adapter release index is not UTF-8")?
    } else {
        let path = source.strip_prefix("file://").unwrap_or(source);
        fs::read_to_string(Path::new(path))
            .with_context(|| format!("failed to read adapter release index {path}"))?
    };
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
            let requirement = VersionReq::parse(&release.core_compatibility).ok()?;
            let platform_release = release
                .platforms
                .iter()
                .find(|item| item.platform == platform)?;
            let channel_allowed = release.channel == ReleaseChannel::Stable || include_prerelease;
            let exact_allowed = exact_version.is_none_or(|exact| exact == &version);
            (channel_allowed && exact_allowed && requirement.matches(core_version)).then_some((
                version,
                release,
                platform_release,
            ))
        })
        .collect::<Vec<_>>();
    candidates.sort_by(|left, right| right.0.cmp(&left.0));
    let Some((version, release, platform_release)) = candidates.into_iter().next() else {
        bail!("no compatible release for adapter `{}` on platform `{platform}` with Core {core_version}", adapter.domain);
    };
    Ok(ResolvedAdapterRelease {
        adapter,
        release,
        platform: platform_release,
        version,
    })
}

pub fn verify_file_sha256(path: &Path, expected: &str) -> anyhow::Result<()> {
    if expected.len() != 64 || !expected.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        bail!("indexed SHA-256 must contain exactly 64 hexadecimal characters");
    }
    let bytes = fs::read(path)
        .with_context(|| format!("failed to read {} for SHA-256 verification", path.display()))?;
    let actual = format!("{:x}", Sha256::digest(bytes));
    if !actual.eq_ignore_ascii_case(expected) {
        bail!("adapter archive SHA-256 mismatch: expected {expected}, got {actual}");
    }
    Ok(())
}

pub fn verify_detached_release_signature(
    archive_path: &Path,
    signature_path: &Path,
    keyring_path: &Path,
    expected_key_id: &str,
) -> anyhow::Result<()> {
    let keyring: ReleaseKeyring =
        serde_json::from_str(&fs::read_to_string(keyring_path).with_context(|| {
            format!("failed to read release keyring {}", keyring_path.display())
        })?)
        .context("release keyring is not valid JSON")?;
    let envelope: DetachedSignature =
        serde_json::from_str(&fs::read_to_string(signature_path).with_context(|| {
            format!(
                "failed to read detached signature {}",
                signature_path.display()
            )
        })?)
        .context("detached release signature is not valid JSON")?;
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
    let archive = fs::read(archive_path)
        .with_context(|| format!("failed to read signed archive {}", archive_path.display()))?;
    verifier
        .verify(&archive, &Signature::from_bytes(&signature))
        .context("detached adapter release signature did not verify")
}

pub fn extract_safe_tar_gz(
    archive_path: &Path,
    destination: &Path,
    expected_root: &str,
) -> anyhow::Result<()> {
    validate_identifier_like_archive_root(expected_root)?;
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

fn validate_identifier_like_archive_root(root: &str) -> anyhow::Result<()> {
    if root.is_empty() || root == "." || root == ".." || Path::new(root).components().count() != 1 {
        bail!("archive_root must be one relative path component");
    }
    Ok(())
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseKeyring {
    pub keys: Vec<ReleasePublicKey>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePublicKey {
    pub key_id: String,
    pub public_key: String,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
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
    pub core_compatibility: String,
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
                validate_relative_resource_path(
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
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};
    use semver::Version;
    use std::io::Write as _;
    use tar::EntryType;

    use super::{
        extract_safe_tar_gz, load_release_index, parse_release_index, parse_resource_manifest,
        resolve_release, verify_detached_release_signature, verify_file_sha256,
        AdapterClassification, DetachedSignature, ReleaseChannel, ReleaseKeyring, ReleasePublicKey,
    };

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

    #[test]
    fn loads_explicit_local_index_without_network() -> anyhow::Result<()> {
        let index = load_release_index("tests/fixtures/release-index/open-and-commercial.json")?;
        assert_eq!(index.adapters[0].domain, "example");
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
