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
    #[arg(long)]
    existing_catalog: PathBuf,
    #[arg(long)]
    existing_signature: PathBuf,
    #[arg(long)]
    trusted_keyring: PathBuf,
    #[arg(long)]
    dist: PathBuf,
    #[arg(long)]
    output_catalog: PathBuf,
    #[arg(long)]
    output_signature: PathBuf,
    #[arg(long)]
    previous_version_output: PathBuf,
    #[arg(long)]
    version: String,
    #[arg(long, value_parser = ["stable", "prerelease"])]
    channel: String,
    #[arg(long)]
    minimum_updater_version: String,
    #[arg(long)]
    archive_base_url: String,
    #[arg(long)]
    catalog_key_id: String,
    #[arg(long)]
    archive_key_id: String,
    #[arg(long, default_value = "LDGR_CATALOG_SIGNING_KEY")]
    catalog_key_env: String,
    #[arg(long, default_value = "LDGR_ARCHIVE_SIGNING_KEY")]
    archive_key_env: String,
}

fn main() -> anyhow::Result<()> {
    let args = Args::parse();
    prepare(&args)
}

fn prepare(args: &Args) -> anyhow::Result<()> {
    let version = Version::parse(&args.version).context("release version is not semantic")?;
    let minimum_updater = Version::parse(&args.minimum_updater_version)
        .context("minimum updater version is not semantic")?;
    ensure!(
        minimum_updater <= version,
        "minimum updater version cannot exceed the release version"
    );
    let catalog_key = signing_key_from_env(&args.catalog_key_env)?;
    let archive_key = signing_key_from_env(&args.archive_key_env)?;
    let trusted = parse_release_keyring(&fs::read_to_string(&args.trusted_keyring)?)
        .context("trusted release keyring is invalid")?;
    require_matching_trusted_key(&trusted, &args.catalog_key_id, &catalog_key)?;

    let existing_text = fs::read_to_string(&args.existing_catalog)
        .context("failed to read existing Core catalog")?;
    let existing_signature = fs::read_to_string(&args.existing_signature)
        .context("failed to read existing Core catalog signature")?;
    let existing = verify_signed_core_update_catalog(&existing_text, &existing_signature, &trusted)
        .context("existing Core catalog is not signed by a configured trust root")?;
    ensure!(
        existing
            .catalog
            .releases
            .iter()
            .all(|release| release.version != args.version),
        "Core catalog already contains version {}",
        args.version
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
    fs::write(&args.previous_version_output, format!("{}\n", previous.1))?;

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
        metadata.version == args.version,
        "embedded Core version differs"
    );
    let release = CoreRelease {
        version: args.version.clone(),
        channel: match args.channel.as_str() {
            "stable" => ReleaseChannel::Stable,
            "prerelease" => ReleaseChannel::Prerelease,
            _ => unreachable!(),
        },
        minimum_updater_version: args.minimum_updater_version.clone(),
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
    let expected_platforms = PLATFORMS.into_iter().collect::<BTreeSet<_>>();
    let mut observed = BTreeSet::new();
    let mut common_metadata: Option<CoreReleaseMetadata> = None;
    let mut platforms = Vec::new();
    for platform in PLATFORMS {
        let name = format!("ldgr-core-{}-{platform}.tar.gz", args.version);
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
        let root = format!("ldgr-core-{}", args.version);
        let extracted = tempfile::tempdir()?;
        ldgr_core::release_index::extract_safe_tar_gz(&archive, extracted.path(), &root)?;
        let metadata = read_release_metadata(&extracted.path().join(&root))?;
        ensure!(
            metadata.platform == platform,
            "archive {name} embeds wrong platform"
        );
        validate_metadata_contract(&metadata, &args.version)?;
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
        let base = args.archive_base_url.trim_end_matches('/');
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
    let release = verified
        .catalog
        .releases
        .iter()
        .find(|release| release.version == args.version)
        .context("generated catalog omitted the target release")?;
    ensure!(release.platforms.len() == PLATFORMS.len());
    for platform in &release.platforms {
        let archive = args.dist.join(format!(
            "ldgr-core-{}-{}.tar.gz",
            args.version, platform.platform
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
