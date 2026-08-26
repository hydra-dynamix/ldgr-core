use std::collections::{BTreeSet, HashMap};
use std::env;
use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use clap::Parser;
use ed25519_dalek::{Signer, SigningKey};
use ldgr_core::release_index::{
    parse_release_keyring, verify_file_sha256_for, DetachedSignature, ReleaseChannel,
    ReleaseKeyring, ReleasePublicKey,
};
use ldgr_core::update::catalog::{
    canonical_catalog_bytes, extract_bound_core_archive, read_release_metadata,
    verify_resolved_core_archive_signature, verify_signed_core_update_catalog, CorePlatformArchive,
    CoreRelease, CoreReleaseCompatibility, CoreReleaseMetadata, CoreUpdateCatalog,
    PairedAgentctlRelease, ResolvedCoreRelease, CORE_RELEASE_METADATA_SCHEMA_VERSION,
    CORE_UPDATE_CATALOG_SCHEMA_VERSION, ERROR_RECOVERY_SCHEMA_VERSION,
    LAUNCHER_COMPATIBILITY_SCHEMA_V1,
};
use semver::Version;
use serde::Deserialize;
use sha2::{Digest, Sha256};

const PLATFORMS: [&str; 5] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
];

#[derive(Debug, Parser)]
#[command(about = "Prepare and verify one signed LDGR Core catalog release")]
struct Args {
    /// Checked one-time inventory used only when no signed catalog exists.
    #[arg(
        long,
        conflicts_with_all = [
            "existing_catalog",
            "existing_signature",
            "previous_version_output",
            "version",
            "channel",
            "minimum_updater_version",
            "archive_base_url"
        ]
    )]
    bootstrap_inventory: Option<PathBuf>,
    #[arg(long, required_unless_present = "bootstrap_inventory")]
    existing_catalog: Option<PathBuf>,
    #[arg(long, required_unless_present = "bootstrap_inventory")]
    existing_signature: Option<PathBuf>,
    #[arg(long)]
    trusted_keyring: PathBuf,
    #[arg(long)]
    dist: PathBuf,
    #[arg(long)]
    output_catalog: PathBuf,
    #[arg(long)]
    output_signature: PathBuf,
    #[arg(long, required_unless_present = "bootstrap_inventory")]
    previous_version_output: Option<PathBuf>,
    #[arg(long, required_unless_present = "bootstrap_inventory")]
    version: Option<String>,
    #[arg(long, required_unless_present = "bootstrap_inventory", value_parser = ["stable", "prerelease"])]
    channel: Option<String>,
    #[arg(long, required_unless_present = "bootstrap_inventory")]
    minimum_updater_version: Option<String>,
    #[arg(long, required_unless_present = "bootstrap_inventory")]
    archive_base_url: Option<String>,
    #[arg(long)]
    catalog_key_id: String,
    #[arg(long)]
    archive_key_id: String,
    #[arg(long, default_value = "LDGR_CATALOG_SIGNING_KEY")]
    catalog_key_env: String,
    #[arg(long, default_value = "LDGR_ARCHIVE_SIGNING_KEY")]
    archive_key_env: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapInventory {
    schema_version: u32,
    release_repository: String,
    supported_stable_versions: Vec<String>,
    releases: Vec<BootstrapRelease>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapRelease {
    release_id: u64,
    tag: String,
    tag_commit: String,
    release: CoreRelease,
    provenance: Vec<BootstrapPlatformProvenance>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct BootstrapPlatformProvenance {
    platform: String,
    archive_asset_id: u64,
    archive_name: String,
    archive_size: u64,
    archive_sha256: String,
    checksum_asset_id: u64,
    checksum_name: String,
    checksum_size: u64,
    checksum_sha256: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    prepare(&args)
}

fn prepare(args: &Args) -> anyhow::Result<()> {
    if let Some(inventory) = args.bootstrap_inventory.as_deref() {
        return bootstrap(args, inventory);
    }
    append(args)
}

fn bootstrap(args: &Args, inventory_path: &Path) -> anyhow::Result<()> {
    ensure!(
        !args.output_catalog.exists() && !args.output_signature.exists(),
        "bootstrap refuses to replace an existing catalog or catalog signature"
    );
    let catalog_key = signing_key_from_env(&args.catalog_key_env)?;
    let archive_key = signing_key_from_env(&args.archive_key_env)?;
    let trusted = parse_release_keyring(&fs::read_to_string(&args.trusted_keyring)?)
        .context("trusted release keyring is invalid")?;
    require_matching_trusted_key(&trusted, &args.catalog_key_id, &catalog_key)?;
    require_matching_trusted_key(&trusted, &args.archive_key_id, &archive_key)
        .context("bootstrap archive signing key must already be a trusted Core root")?;

    let inventory: BootstrapInventory = serde_json::from_str(
        &fs::read_to_string(inventory_path).context("failed to read bootstrap inventory")?,
    )
    .context("bootstrap inventory is invalid")?;
    ensure!(
        inventory.schema_version == 1,
        "unsupported bootstrap inventory schema_version {}",
        inventory.schema_version
    );
    validate_repository_name(&inventory.release_repository)?;
    ensure!(
        !inventory.releases.is_empty(),
        "bootstrap inventory must contain at least one supported stable release"
    );

    let mut declared_versions = inventory.supported_stable_versions.clone();
    ensure!(
        declared_versions
            .iter()
            .all(|value| Version::parse(value).is_ok()),
        "supported_stable_versions contains an invalid semantic version"
    );
    declared_versions.sort_by(|left, right| {
        Version::parse(left)
            .expect("checked version")
            .cmp(&Version::parse(right).expect("checked version"))
    });
    declared_versions.dedup();
    ensure!(
        declared_versions == inventory.supported_stable_versions,
        "supported_stable_versions must be unique and sorted"
    );

    let expected_platforms = PLATFORMS.into_iter().collect::<BTreeSet<_>>();
    let mut releases = Vec::new();
    let mut inventory_versions = Vec::new();
    let mut release_ids = BTreeSet::new();
    let mut asset_ids = BTreeSet::new();
    for entry in &inventory.releases {
        ensure!(entry.release_id > 0, "release_id must be positive");
        ensure!(
            release_ids.insert(entry.release_id),
            "duplicate release_id {}",
            entry.release_id
        );
        let version = Version::parse(&entry.release.version)
            .context("bootstrap release version is not semantic")?;
        ensure!(
            version.pre.is_empty() && entry.release.channel == ReleaseChannel::Stable,
            "bootstrap accepts stable Core releases only"
        );
        ensure!(
            entry.tag == format!("v{version}"),
            "bootstrap tag must be v{version}"
        );
        ensure!(
            entry.tag_commit == entry.release.core_commit,
            "Core {} catalog commit differs from immutable tag provenance",
            version
        );
        ensure!(
            entry.release.compatibility.adapter_compatibility.is_some(),
            "Core {version} is missing its exact compatibility-v2 profile"
        );
        entry
            .release
            .compatibility
            .adapter_compatibility
            .as_ref()
            .expect("checked profile")
            .validate()
            .with_context(|| format!("Core {version} compatibility-v2 profile is invalid"))?;

        let release_platforms = entry
            .release
            .platforms
            .iter()
            .map(|platform| platform.platform.as_str())
            .collect::<BTreeSet<_>>();
        let provenance_platforms = entry
            .provenance
            .iter()
            .map(|platform| platform.platform.as_str())
            .collect::<BTreeSet<_>>();
        ensure!(
            release_platforms == expected_platforms
                && provenance_platforms == expected_platforms
                && entry.release.platforms.len() == PLATFORMS.len()
                && entry.provenance.len() == PLATFORMS.len(),
            "Core {version} must contain exactly the five supported platforms"
        );

        for platform in &entry.release.platforms {
            let provenance = entry
                .provenance
                .iter()
                .find(|candidate| candidate.platform == platform.platform)
                .expect("complete provenance matrix");
            verify_bootstrap_platform(
                args,
                &inventory,
                entry,
                platform,
                provenance,
                &archive_key,
                &mut asset_ids,
            )?;
        }
        inventory_versions.push(entry.release.version.clone());
        releases.push(entry.release.clone());
    }
    releases.sort_by(|left, right| {
        Version::parse(&left.version)
            .expect("checked version")
            .cmp(&Version::parse(&right.version).expect("checked version"))
    });
    inventory_versions.sort_by(|left, right| {
        Version::parse(left)
            .expect("checked version")
            .cmp(&Version::parse(right).expect("checked version"))
    });
    ensure!(
        inventory_versions == inventory.supported_stable_versions,
        "inventory releases differ from supported_stable_versions"
    );

    let catalog = CoreUpdateCatalog {
        schema_version: CORE_UPDATE_CATALOG_SCHEMA_VERSION,
        release_keys: Vec::new(),
        releases,
    };
    let canonical = canonical_catalog_bytes(&catalog)?;
    let signature = signature_envelope(&args.catalog_key_id, &catalog_key, &canonical);
    let signature_json = format!("{}\n", serde_json::to_string_pretty(&signature)?);
    let verified = verify_signed_core_update_catalog(
        std::str::from_utf8(&canonical)?,
        &signature_json,
        &trusted,
    )
    .context("bootstrap catalog failed independent trusted-keyring verification")?;
    verify_bootstrap_matrix(args, &inventory, &verified)?;
    // A local write failure may leave a detached signature, but never an
    // unsigned catalog file that a later publication step could mistake for a
    // complete candidate.
    fs::write(&args.output_signature, signature_json)?;
    fs::write(&args.output_catalog, canonical)?;
    Ok(())
}

fn append(args: &Args) -> anyhow::Result<()> {
    let version_text = required(args.version.as_deref(), "--version")?;
    let minimum_updater_text = required(
        args.minimum_updater_version.as_deref(),
        "--minimum-updater-version",
    )?;
    let version = Version::parse(version_text).context("release version is not semantic")?;
    let minimum_updater =
        Version::parse(minimum_updater_text).context("minimum updater version is not semantic")?;
    ensure!(
        minimum_updater <= version,
        "minimum updater version cannot exceed the release version"
    );
    let catalog_key = signing_key_from_env(&args.catalog_key_env)?;
    let archive_key = signing_key_from_env(&args.archive_key_env)?;
    let trusted = parse_release_keyring(&fs::read_to_string(&args.trusted_keyring)?)
        .context("trusted release keyring is invalid")?;
    require_matching_trusted_key(&trusted, &args.catalog_key_id, &catalog_key)?;

    let existing_text = fs::read_to_string(required_path(
        args.existing_catalog.as_deref(),
        "--existing-catalog",
    )?)
    .context("failed to read existing Core catalog")?;
    let existing_signature = fs::read_to_string(required_path(
        args.existing_signature.as_deref(),
        "--existing-signature",
    )?)
    .context("failed to read existing Core catalog signature")?;
    let existing = verify_signed_core_update_catalog(&existing_text, &existing_signature, &trusted)
        .context("existing Core catalog is not signed by a configured trust root")?;
    ensure!(
        existing
            .catalog
            .releases
            .iter()
            .all(|release| release.version != version_text),
        "Core catalog already contains version {}",
        version_text
    );

    let previous = existing
        .catalog
        .releases
        .iter()
        .map(|release| {
            Version::parse(&release.version).map(|version| (version, release.version.as_str()))
        })
        .collect::<Result<Vec<_>, _>>()?
        .into_iter()
        .max_by(|left, right| left.0.cmp(&right.0))
        .context("existing Core catalog has no previous supported version")?;
    ensure!(
        previous.0 < version,
        "new Core version {} must be newer than previous supported version {}",
        version,
        previous.0
    );
    fs::write(
        required_path(
            args.previous_version_output.as_deref(),
            "--previous-version-output",
        )?,
        format!("{}\n", previous.1),
    )?;

    let archive_public_key = STANDARD.encode(archive_key.verifying_key().to_bytes());
    let mut release_keys = existing.catalog.release_keys.clone();
    let trusted_keys = trusted
        .keys
        .iter()
        .map(|key| (key.key_id.as_str(), key.public_key.as_str()))
        .collect::<HashMap<_, _>>();
    match trusted_keys.get(args.archive_key_id.as_str()) {
        Some(public_key) => ensure!(
            *public_key == archive_public_key,
            "archive secret does not match trusted key {}",
            args.archive_key_id
        ),
        None => {
            add_or_validate_successor(&mut release_keys, &args.archive_key_id, &archive_public_key)?
        }
    }

    let (metadata, platforms) = inspect_and_sign_matrix(args, &archive_key)?;
    ensure!(
        metadata.version == version_text,
        "embedded Core version differs"
    );
    let release = CoreRelease {
        version: version_text.to_owned(),
        channel: match required(args.channel.as_deref(), "--channel")? {
            "stable" => ReleaseChannel::Stable,
            "prerelease" => ReleaseChannel::Prerelease,
            _ => unreachable!(),
        },
        minimum_updater_version: minimum_updater_text.to_owned(),
        core_commit: metadata.commit.clone(),
        source_repository: metadata.source_repository.clone(),
        agentctl: PairedAgentctlRelease {
            version: metadata.agentctl_version.clone(),
            repository: metadata.agentctl_repository.clone(),
            commit: metadata.agentctl_commit.clone(),
        },
        compatibility: CoreReleaseCompatibility {
            launcher_compatibility_schema: metadata.launcher_compatibility_schema.clone(),
            error_recovery_schema: metadata.error_recovery_schema,
            release_metadata_schema: metadata.schema_version,
            adapter_compatibility: Some(
                ldgr_core::update::catalog::CandidateCoreAdapterCompatibilityV2::generated(),
            ),
        },
        platforms,
    };
    let mut releases = existing.catalog.releases.clone();
    releases.push(release);
    releases.sort_by(|left, right| {
        Version::parse(&left.version)
            .expect("validated release")
            .cmp(&Version::parse(&right.version).expect("validated release"))
    });
    let catalog = CoreUpdateCatalog {
        schema_version: CORE_UPDATE_CATALOG_SCHEMA_VERSION,
        release_keys,
        releases,
    };
    let canonical = canonical_catalog_bytes(&catalog)?;
    let signature = signature_envelope(&args.catalog_key_id, &catalog_key, &canonical);
    fs::write(&args.output_catalog, &canonical)?;
    fs::write(
        &args.output_signature,
        format!("{}\n", serde_json::to_string_pretty(&signature)?),
    )?;

    let verified = verify_signed_core_update_catalog(
        std::str::from_utf8(&canonical)?,
        &serde_json::to_string(&signature)?,
        &trusted,
    )
    .context("generated Core catalog failed independent verification")?;
    verify_generated_matrix(args, &version, &verified)?;
    Ok(())
}

fn verify_bootstrap_platform(
    args: &Args,
    inventory: &BootstrapInventory,
    entry: &BootstrapRelease,
    platform: &CorePlatformArchive,
    provenance: &BootstrapPlatformProvenance,
    signing_key: &SigningKey,
    asset_ids: &mut BTreeSet<u64>,
) -> anyhow::Result<()> {
    ensure!(
        provenance.archive_asset_id > 0 && provenance.checksum_asset_id > 0,
        "Core {} {} provenance asset IDs must be positive",
        entry.release.version,
        platform.platform
    );
    ensure!(
        asset_ids.insert(provenance.archive_asset_id)
            && asset_ids.insert(provenance.checksum_asset_id),
        "bootstrap provenance asset IDs must be globally unique"
    );
    let expected_name = format!(
        "ldgr-core-{}-{}.tar.gz",
        entry.release.version, platform.platform
    );
    ensure!(
        provenance.archive_name == expected_name
            && provenance.checksum_name == format!("{expected_name}.sha256"),
        "Core {} {} provenance asset names are not canonical",
        entry.release.version,
        platform.platform
    );
    ensure!(
        provenance.archive_sha256 == platform.sha256,
        "Core {} {} catalog digest differs from immutable archive provenance",
        entry.release.version,
        platform.platform
    );
    let expected_url = format!(
        "https://github.com/{}/releases/download/{}/{}",
        inventory.release_repository, entry.tag, expected_name
    );
    ensure!(
        platform.archive_url == expected_url
            && platform.signature_url == format!("{expected_url}.sig")
            && platform.archive_root == format!("ldgr-core-{}", entry.release.version)
            && platform.signing_key_id == args.archive_key_id,
        "Core {} {} catalog asset binding differs from reviewed provenance",
        entry.release.version,
        platform.platform
    );

    let archive = args.dist.join(&provenance.archive_name);
    let checksum = args.dist.join(&provenance.checksum_name);
    ensure!(
        archive.is_file(),
        "missing hosted archive {}",
        archive.display()
    );
    ensure!(
        checksum.is_file(),
        "missing hosted checksum {}",
        checksum.display()
    );
    let archive_bytes = fs::read(&archive)?;
    let checksum_bytes = fs::read(&checksum)?;
    ensure!(
        archive_bytes.len() as u64 == provenance.archive_size
            && checksum_bytes.len() as u64 == provenance.checksum_size,
        "Core {} {} hosted asset size differs from immutable provenance",
        entry.release.version,
        platform.platform
    );
    let archive_digest = format!("{:x}", Sha256::digest(&archive_bytes));
    let checksum_digest = format!("{:x}", Sha256::digest(&checksum_bytes));
    ensure!(
        archive_digest == provenance.archive_sha256
            && checksum_digest == provenance.checksum_sha256,
        "Core {} {} hosted asset bytes differ from immutable provenance",
        entry.release.version,
        platform.platform
    );
    verify_checksum_sidecar(&checksum, &archive_digest)?;
    let signature = signature_envelope(&args.archive_key_id, signing_key, &archive_bytes);
    fs::write(
        format!("{}.sig", archive.display()),
        format!("{}\n", serde_json::to_string_pretty(&signature)?),
    )?;
    Ok(())
}

fn verify_bootstrap_matrix(
    args: &Args,
    inventory: &BootstrapInventory,
    verified: &ldgr_core::update::catalog::VerifiedCoreUpdateCatalog,
) -> anyhow::Result<()> {
    ensure!(verified.catalog.releases.len() == inventory.releases.len());
    for release in &verified.catalog.releases {
        let version = Version::parse(&release.version)?;
        for platform in &release.platforms {
            let archive = args.dist.join(format!(
                "ldgr-core-{}-{}.tar.gz",
                release.version, platform.platform
            ));
            let signature = PathBuf::from(format!("{}.sig", archive.display()));
            let resolved = ResolvedCoreRelease {
                version: version.clone(),
                release: release.clone(),
                platform: platform.clone(),
            };
            verify_resolved_core_archive_signature(&archive, &signature, &resolved, verified)
                .with_context(|| {
                    format!(
                        "Core {} {} archive signature failed independent verification",
                        release.version, platform.platform
                    )
                })?;
            let extracted = tempfile::tempdir()?;
            extract_bound_core_archive(&archive, extracted.path(), &resolved).with_context(
                || {
                    format!(
                        "Core {} {} archive metadata failed catalog binding",
                        release.version, platform.platform
                    )
                },
            )?;
        }
    }
    Ok(())
}

fn validate_repository_name(repository: &str) -> anyhow::Result<()> {
    let parts = repository.split('/').collect::<Vec<_>>();
    ensure!(
        parts.len() == 2
            && parts.iter().all(|part| {
                !part.is_empty()
                    && part.bytes().all(|byte| {
                        byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.')
                    })
            }),
        "release_repository must use canonical owner/repository form"
    );
    Ok(())
}

fn required<'a>(value: Option<&'a str>, flag: &str) -> anyhow::Result<&'a str> {
    value.with_context(|| format!("{flag} is required in append mode"))
}

fn required_path<'a>(value: Option<&'a Path>, flag: &str) -> anyhow::Result<&'a Path> {
    value.with_context(|| format!("{flag} is required in append mode"))
}

fn signing_key_from_env(name: &str) -> anyhow::Result<SigningKey> {
    let encoded = env::var(name).with_context(|| format!("{name} is not configured"))?;
    let decoded = STANDARD
        .decode(encoded.trim())
        .with_context(|| format!("{name} is not valid base64"))?;
    let seed: [u8; 32] = decoded
        .try_into()
        .map_err(|_| anyhow::anyhow!("{name} must encode exactly one 32-byte Ed25519 seed"))?;
    Ok(SigningKey::from_bytes(&seed))
}

fn require_matching_trusted_key(
    keyring: &ReleaseKeyring,
    key_id: &str,
    signing_key: &SigningKey,
) -> anyhow::Result<()> {
    let expected = STANDARD.encode(signing_key.verifying_key().to_bytes());
    let trusted = keyring
        .keys
        .iter()
        .find(|key| key.key_id == key_id)
        .with_context(|| format!("catalog signing key {key_id} is not an existing trust root"))?;
    ensure!(
        trusted.public_key == expected,
        "catalog signing secret does not match trusted key {key_id}"
    );
    Ok(())
}

fn add_or_validate_successor(
    keys: &mut Vec<ReleasePublicKey>,
    key_id: &str,
    public_key: &str,
) -> anyhow::Result<()> {
    if let Some(existing) = keys.iter().find(|key| key.key_id == key_id) {
        ensure!(
            existing.public_key == public_key,
            "successor archive key {key_id} conflicts with the existing catalog"
        );
    } else {
        keys.push(ReleasePublicKey {
            key_id: key_id.to_owned(),
            public_key: public_key.to_owned(),
        });
        keys.sort_by(|left, right| left.key_id.cmp(&right.key_id));
    }
    Ok(())
}

fn inspect_and_sign_matrix(
    args: &Args,
    signing_key: &SigningKey,
) -> anyhow::Result<(CoreReleaseMetadata, Vec<CorePlatformArchive>)> {
    let version = required(args.version.as_deref(), "--version")?;
    let archive_base_url = required(args.archive_base_url.as_deref(), "--archive-base-url")?;
    let expected_platforms = PLATFORMS.into_iter().collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut common_metadata: Option<CoreReleaseMetadata> = None;
    let mut platforms = Vec::new();
    for platform in PLATFORMS {
        let name = format!("ldgr-core-{version}-{platform}.tar.gz");
        let archive = args.dist.join(&name);
        let checksum = archive.with_extension("gz.sha256");
        ensure!(
            archive.is_file(),
            "missing platform archive {}",
            archive.display()
        );
        ensure!(
            checksum.is_file(),
            "missing platform checksum {}",
            checksum.display()
        );
        let digest = format!("{:x}", Sha256::digest(fs::read(&archive)?));
        verify_checksum_sidecar(&checksum, &digest)?;
        let root = format!("ldgr-core-{version}");
        let extracted = tempfile::tempdir()?;
        ldgr_core::release_index::extract_safe_tar_gz(&archive, extracted.path(), &root)?;
        let metadata = read_release_metadata(&extracted.path().join(&root))?;
        ensure!(
            metadata.platform == platform,
            "archive {name} embeds wrong platform"
        );
        validate_metadata_contract(&metadata, version)?;
        if let Some(common) = common_metadata.as_ref() {
            ensure!(
                metadata_without_platform(common) == metadata_without_platform(&metadata),
                "archive {name} metadata differs from the other platforms"
            );
        } else {
            common_metadata = Some(metadata.clone());
        }
        let bytes = fs::read(&archive)?;
        let signature = signature_envelope(&args.archive_key_id, signing_key, &bytes);
        fs::write(
            format!("{}.sig", archive.display()),
            format!("{}\n", serde_json::to_string_pretty(&signature)?),
        )?;
        observed.insert(platform);
        let base = archive_base_url.trim_end_matches('/');
        platforms.push(CorePlatformArchive {
            platform: platform.to_owned(),
            archive_url: format!("{base}/{name}"),
            archive_root: root,
            sha256: digest,
            signature_url: format!("{base}/{name}.sig"),
            signing_key_id: args.archive_key_id.clone(),
        });
    }
    ensure!(
        observed == expected_platforms,
        "release platform matrix is incomplete"
    );
    Ok((
        common_metadata.context("release platform matrix is empty")?,
        platforms,
    ))
}

fn validate_metadata_contract(metadata: &CoreReleaseMetadata, version: &str) -> anyhow::Result<()> {
    ensure!(metadata.schema_version == CORE_RELEASE_METADATA_SCHEMA_VERSION);
    ensure!(metadata.package == "ldgr-core");
    ensure!(metadata.binary == "ldgr");
    ensure!(metadata.version == version);
    ensure!(metadata.launcher_compatibility_schema == LAUNCHER_COMPATIBILITY_SCHEMA_V1);
    ensure!(metadata.error_recovery_schema == ERROR_RECOVERY_SCHEMA_VERSION);
    Version::parse(&metadata.agentctl_version)?;
    ensure!(!metadata.agentctl_repository.trim().is_empty());
    ensure!(!metadata.agentctl_commit.trim().is_empty());
    ensure!(!metadata.commit.trim().is_empty());
    ensure!(!metadata.source_repository.trim().is_empty());
    Ok(())
}

fn metadata_without_platform(metadata: &CoreReleaseMetadata) -> serde_json::Value {
    let mut value = serde_json::to_value(metadata).expect("metadata serializes");
    value
        .as_object_mut()
        .expect("metadata is an object")
        .remove("platform");
    value
}

fn verify_checksum_sidecar(path: &Path, expected: &str) -> anyhow::Result<()> {
    let text = fs::read_to_string(path)?;
    let token = text
        .split_whitespace()
        .next()
        .context("checksum sidecar is empty")?;
    ensure!(
        token == expected,
        "checksum sidecar {} is stale",
        path.display()
    );
    Ok(())
}

fn signature_envelope(key_id: &str, key: &SigningKey, bytes: &[u8]) -> DetachedSignature {
    DetachedSignature {
        algorithm: "Ed25519".to_owned(),
        key_id: key_id.to_owned(),
        signature: STANDARD.encode(key.sign(bytes).to_bytes()),
    }
}

fn verify_generated_matrix(
    args: &Args,
    version: &Version,
    verified: &ldgr_core::update::catalog::VerifiedCoreUpdateCatalog,
) -> anyhow::Result<()> {
    let version_text = required(args.version.as_deref(), "--version")?;
    let release = verified
        .catalog
        .releases
        .iter()
        .find(|release| release.version == version_text)
        .context("generated catalog omitted the target release")?;
    ensure!(release.platforms.len() == PLATFORMS.len());
    for platform in &release.platforms {
        let archive = args.dist.join(format!(
            "ldgr-core-{}-{}.tar.gz",
            version_text, platform.platform
        ));
        verify_file_sha256_for(&archive, &platform.sha256, "generated Core archive")?;
        let resolved = ResolvedCoreRelease {
            version: version.clone(),
            release: release.clone(),
            platform: platform.clone(),
        };
        let signature = PathBuf::from(format!("{}.sig", archive.display()));
        verify_resolved_core_archive_signature(&archive, &signature, &resolved, verified)?;
        let extracted = tempfile::tempdir()?;
        extract_bound_core_archive(&archive, extracted.path(), &resolved)?;
    }
    Ok(())
}
