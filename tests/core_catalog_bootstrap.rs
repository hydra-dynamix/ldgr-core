use std::fs;
use std::path::Path;
use std::process::{Command, Output};

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::SigningKey;
use flate2::write::GzEncoder;
use flate2::Compression;
use ldgr_core::release_index::{parse_release_keyring, ReleaseKeyring, ReleasePublicKey};
use ldgr_core::update::catalog::{
    verify_signed_core_update_catalog, CandidateCoreAdapterCompatibilityV2, CorePlatformArchive,
    CoreRelease, CoreReleaseCompatibility, CoreReleaseMetadata, CoreUpdateCatalog,
    PairedAgentctlRelease,
};
use sha2::{Digest, Sha256};
use tar::{Builder, Header};

const PLATFORMS: [&str; 5] = [
    "linux-x86_64",
    "linux-aarch64",
    "macos-x86_64",
    "macos-aarch64",
    "windows-x86_64",
];

#[test]
fn bootstrap_is_key_gated_fail_closed_and_append_compatible() -> anyhow::Result<()> {
    let directory = tempfile::tempdir()?;
    let dist = directory.path().join("dist");
    fs::create_dir(&dist)?;
    let trusted_key = SigningKey::from_bytes(&[41; 32]);
    let wrong_key = SigningKey::from_bytes(&[42; 32]);
    let keyring = ReleaseKeyring {
        keys: vec![ReleasePublicKey {
            key_id: "fixture-root".into(),
            public_key: STANDARD.encode(trusted_key.verifying_key().to_bytes()),
        }],
    };
    let keyring_path = directory.path().join("release-keyring.json");
    fs::write(&keyring_path, serde_json::to_vec_pretty(&keyring)?)?;
    let inventory = write_release_fixture(&dist, "1.0.0", 100, 200)?;
    let inventory_path = directory.path().join("inventory.json");
    fs::write(&inventory_path, serde_json::to_vec_pretty(&inventory)?)?;
    let catalog = directory.path().join("core-index.json");
    let catalog_signature = directory.path().join("core-index.json.sig");

    let missing_key = bootstrap_command(
        &inventory_path,
        &keyring_path,
        &dist,
        &catalog,
        &catalog_signature,
    )
    .output_context("bootstrap catalog without signing keys")?;
    ensure!(!missing_key.status.success());
    ensure!(!catalog.exists() && !catalog_signature.exists());

    let mismatched_key = bootstrap_command(
        &inventory_path,
        &keyring_path,
        &dist,
        &catalog,
        &catalog_signature,
    )
    .env(
        "LDGR_CATALOG_SIGNING_KEY",
        STANDARD.encode(wrong_key.to_bytes()),
    )
    .env(
        "LDGR_ARCHIVE_SIGNING_KEY",
        STANDARD.encode(wrong_key.to_bytes()),
    )
    .output_context("bootstrap catalog with untrusted signing keys")?;
    ensure!(!mismatched_key.status.success());
    ensure!(!catalog.exists() && !catalog_signature.exists());

    assert_inventory_failure(
        directory.path(),
        &keyring_path,
        &dist,
        &trusted_key,
        "missing-profile",
        |value| {
            value["releases"][0]["release"]["compatibility"]["adapter_compatibility"] =
                serde_json::Value::Null
        },
    )?;
    assert_inventory_failure(
        directory.path(),
        &keyring_path,
        &dist,
        &trusted_key,
        "missing-platform",
        |value| {
            value["releases"][0]["release"]["platforms"]
                .as_array_mut()
                .unwrap()
                .pop();
            value["releases"][0]["provenance"]
                .as_array_mut()
                .unwrap()
                .pop();
        },
    )?;

    let checksum = dist.join("ldgr-core-1.0.0-linux-x86_64.tar.gz.sha256");
    let saved_checksum = fs::read(&checksum)?;
    fs::remove_file(&checksum)?;
    let missing_checksum = signed_bootstrap_command(
        &inventory_path,
        &keyring_path,
        &dist,
        &catalog,
        &catalog_signature,
        &trusted_key,
    )
    .output_context("bootstrap catalog with a missing archive checksum")?;
    ensure!(!missing_checksum.status.success());
    ensure!(!catalog.exists() && !catalog_signature.exists());
    fs::write(&checksum, saved_checksum)?;

    let archive = dist.join("ldgr-core-1.0.0-linux-x86_64.tar.gz");
    let saved_archive = fs::read(&archive)?;
    let mut tampered_archive = saved_archive.clone();
    tampered_archive[0] ^= 1;
    fs::write(&archive, tampered_archive)?;
    let mismatched_archive = signed_bootstrap_command(
        &inventory_path,
        &keyring_path,
        &dist,
        &catalog,
        &catalog_signature,
        &trusted_key,
    )
    .output_context("bootstrap catalog with a tampered archive")?;
    ensure!(!mismatched_archive.status.success());
    ensure!(!catalog.exists() && !catalog_signature.exists());
    fs::write(&archive, saved_archive)?;

    let success = signed_bootstrap_command(
        &inventory_path,
        &keyring_path,
        &dist,
        &catalog,
        &catalog_signature,
        &trusted_key,
    )
    .output_context("bootstrap valid signed catalog")?;
    ensure!(success.status.success(), "{}", stderr(&success));
    let verified = verify_catalog(&catalog, &catalog_signature, &keyring_path)?;
    ensure!(verified.releases.len() == 1);
    for platform in PLATFORMS {
        ensure!(dist
            .join(format!("ldgr-core-1.0.0-{platform}.tar.gz.sig"))
            .is_file());
    }

    write_release_fixture(&dist, "1.0.1", 300, 400)?;
    let appended_catalog = directory.path().join("appended-core-index.json");
    let appended_signature = directory.path().join("appended-core-index.json.sig");
    let previous = directory.path().join("previous-version.txt");
    let append = Command::new(env!("CARGO_BIN_EXE_ldgr-release"))
        .args([
            "--existing-catalog",
            path(&catalog),
            "--existing-signature",
            path(&catalog_signature),
            "--trusted-keyring",
            path(&keyring_path),
            "--dist",
            path(&dist),
            "--output-catalog",
            path(&appended_catalog),
            "--output-signature",
            path(&appended_signature),
            "--previous-version-output",
            path(&previous),
            "--version",
            "1.0.1",
            "--channel",
            "stable",
            "--minimum-updater-version",
            "1.0.0",
            "--archive-base-url",
            "https://github.com/example/core/releases/download/v1.0.1",
            "--catalog-key-id",
            "fixture-root",
            "--archive-key-id",
            "fixture-root",
        ])
        .env(
            "LDGR_CATALOG_SIGNING_KEY",
            STANDARD.encode(trusted_key.to_bytes()),
        )
        .env(
            "LDGR_ARCHIVE_SIGNING_KEY",
            STANDARD.encode(trusted_key.to_bytes()),
        )
        .output_context("append a release to the signed catalog")?;
    ensure!(append.status.success(), "{}", stderr(&append));
    ensure!(fs::read_to_string(previous)?.trim() == "1.0.0");
    ensure!(
        verify_catalog(&appended_catalog, &appended_signature, &keyring_path)?
            .releases
            .len()
            == 2
    );
    Ok(())
}

fn assert_inventory_failure(
    root: &Path,
    keyring: &Path,
    dist: &Path,
    key: &SigningKey,
    name: &str,
    mutate: impl FnOnce(&mut serde_json::Value),
) -> anyhow::Result<()> {
    let source: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("inventory.json"))?)?;
    let mut changed = source;
    mutate(&mut changed);
    let inventory = root.join(format!("{name}.json"));
    let catalog = root.join(format!("{name}-catalog.json"));
    let signature = root.join(format!("{name}-catalog.json.sig"));
    fs::write(&inventory, serde_json::to_vec_pretty(&changed)?)?;
    let output = signed_bootstrap_command(&inventory, keyring, dist, &catalog, &signature, key)
        .output_context(&format!("reject invalid {name} bootstrap inventory"))?;
    ensure!(
        !output.status.success(),
        "invalid inventory unexpectedly passed"
    );
    ensure!(!catalog.exists() && !signature.exists());
    Ok(())
}

fn bootstrap_command(
    inventory: &Path,
    keyring: &Path,
    dist: &Path,
    catalog: &Path,
    signature: &Path,
) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ldgr-release"));
    command.args([
        "--bootstrap-inventory",
        path(inventory),
        "--trusted-keyring",
        path(keyring),
        "--dist",
        path(dist),
        "--output-catalog",
        path(catalog),
        "--output-signature",
        path(signature),
        "--catalog-key-id",
        "fixture-root",
        "--archive-key-id",
        "fixture-root",
    ]);
    command
}

fn signed_bootstrap_command(
    inventory: &Path,
    keyring: &Path,
    dist: &Path,
    catalog: &Path,
    signature: &Path,
    key: &SigningKey,
) -> Command {
    let mut command = bootstrap_command(inventory, keyring, dist, catalog, signature);
    command
        .env("LDGR_CATALOG_SIGNING_KEY", STANDARD.encode(key.to_bytes()))
        .env("LDGR_ARCHIVE_SIGNING_KEY", STANDARD.encode(key.to_bytes()));
    command
}

fn verify_catalog(
    catalog: &Path,
    signature: &Path,
    keyring: &Path,
) -> anyhow::Result<CoreUpdateCatalog> {
    let trusted = parse_release_keyring(&fs::read_to_string(keyring)?)?;
    Ok(verify_signed_core_update_catalog(
        &fs::read_to_string(catalog)?,
        &fs::read_to_string(signature)?,
        &trusted,
    )?
    .catalog)
}

fn write_release_fixture(
    dist: &Path,
    version: &str,
    release_id: u64,
    first_asset_id: u64,
) -> anyhow::Result<serde_json::Value> {
    let mut platforms = Vec::new();
    let mut provenance = Vec::new();
    for (index, platform) in PLATFORMS.into_iter().enumerate() {
        let name = format!("ldgr-core-{version}-{platform}.tar.gz");
        let archive = dist.join(&name);
        let metadata = metadata(version, platform);
        if version == "1.0.0" {
            write_historical_archive(&archive, &metadata)?;
        } else {
            write_archive(&archive, &metadata)?;
        }
        let bytes = fs::read(&archive)?;
        let digest = format!("{:x}", Sha256::digest(&bytes));
        let checksum_name = format!("{name}.sha256");
        let checksum = dist.join(&checksum_name);
        fs::write(&checksum, format!("{digest}  {name}\n"))?;
        let checksum_bytes = fs::read(&checksum)?;
        let url = format!("https://github.com/example/core/releases/download/v{version}/{name}");
        platforms.push(CorePlatformArchive {
            platform: platform.into(),
            archive_url: url.clone(),
            archive_root: format!("ldgr-core-{version}"),
            sha256: digest.clone(),
            signature_url: format!("{url}.sig"),
            signing_key_id: "fixture-root".into(),
        });
        provenance.push(serde_json::json!({
            "platform": platform,
            "archive_asset_id": first_asset_id + index as u64 * 2,
            "archive_name": name,
            "archive_size": bytes.len(),
            "archive_sha256": digest,
            "checksum_asset_id": first_asset_id + index as u64 * 2 + 1,
            "checksum_name": checksum_name,
            "checksum_size": checksum_bytes.len(),
            "checksum_sha256": format!("{:x}", Sha256::digest(&checksum_bytes)),
        }));
    }
    let release = CoreRelease {
        version: version.into(),
        channel: ldgr_core::release_index::ReleaseChannel::Stable,
        minimum_updater_version: version.into(),
        core_commit: "a".repeat(40),
        source_repository: "example/core".into(),
        agentctl: PairedAgentctlRelease {
            version: "1.0.0".into(),
            repository: "example/agentctl".into(),
            commit: "b".repeat(40),
        },
        compatibility: CoreReleaseCompatibility {
            launcher_compatibility_schema: "ldgr.launcher-compatibility.v1".into(),
            error_recovery_schema: 1,
            release_metadata_schema: 1,
            adapter_compatibility: Some(CandidateCoreAdapterCompatibilityV2::generated()),
        },
        platforms,
    };
    Ok(serde_json::json!({
        "schema_version": 1,
        "release_repository": "example/core",
        "supported_stable_versions": [version],
        "releases": [{"release_id": release_id, "tag": format!("v{version}"), "tag_commit": "a".repeat(40), "release": release, "provenance": provenance}],
    }))
}

fn metadata(version: &str, platform: &str) -> CoreReleaseMetadata {
    CoreReleaseMetadata {
        schema_version: 1,
        package: "ldgr-core".into(),
        binary: "ldgr".into(),
        version: version.into(),
        agentctl_version: "1.0.0".into(),
        agentctl_repository: "example/agentctl".into(),
        agentctl_commit: "b".repeat(40),
        launcher_compatibility_schema: "ldgr.launcher-compatibility.v1".into(),
        error_recovery_schema: 1,
        platform: platform.into(),
        commit: "a".repeat(40),
        source_repository: "example/core".into(),
    }
}

fn write_historical_archive(path: &Path, metadata: &CoreReleaseMetadata) -> anyhow::Result<()> {
    let root = format!("ldgr-core-{}", metadata.version);
    let file = fs::File::create(path)?;
    let mut archive = Builder::new(GzEncoder::new(file, Compression::default()));
    let historical = serde_json::json!({
        "schema_version": metadata.schema_version,
        "package": metadata.package,
        "binary": metadata.binary,
        "version": metadata.version,
        "agentctl_version": metadata.agentctl_version,
        "agentctl_repository": metadata.agentctl_repository,
        "agentctl_commit": metadata.agentctl_commit,
        "component": "ldgr-core",
        "component_commit": metadata.commit,
        "root_commit": "c".repeat(40),
        "platform": metadata.platform,
        "source_repository": metadata.source_repository,
    });
    append(
        &mut archive,
        &format!("{root}/RELEASE-METADATA.json"),
        &serde_json::to_vec(&historical)?,
    )?;
    let extension = if metadata.platform.starts_with("windows-") {
        ".exe"
    } else {
        ""
    };
    append(
        &mut archive,
        &format!("{root}/{}/ldgr{extension}", metadata.platform),
        b"core",
    )?;
    append(
        &mut archive,
        &format!("{root}/{}/agentctl{extension}", metadata.platform),
        b"agentctl",
    )?;
    archive.finish()?;
    Ok(())
}

fn write_archive(path: &Path, metadata: &CoreReleaseMetadata) -> anyhow::Result<()> {
    let root = format!("ldgr-core-{}", metadata.version);
    let file = fs::File::create(path)?;
    let mut archive = Builder::new(GzEncoder::new(file, Compression::default()));
    append(
        &mut archive,
        &format!("{root}/RELEASE-METADATA.json"),
        &serde_json::to_vec(metadata)?,
    )?;
    let extension = if metadata.platform.starts_with("windows-") {
        ".exe"
    } else {
        ""
    };
    append(
        &mut archive,
        &format!("{root}/{}/ldgr{extension}", metadata.platform),
        b"core",
    )?;
    append(
        &mut archive,
        &format!("{root}/{}/agentctl{extension}", metadata.platform),
        b"agentctl",
    )?;
    archive.finish()?;
    Ok(())
}

fn append<W: std::io::Write>(
    archive: &mut Builder<W>,
    name: &str,
    bytes: &[u8],
) -> anyhow::Result<()> {
    let mut header = Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o755);
    header.set_cksum();
    archive.append_data(&mut header, name, bytes)?;
    Ok(())
}

trait CommandOutputContext {
    fn output_context(&mut self, operation: &str) -> anyhow::Result<Output>;
}

impl CommandOutputContext for Command {
    fn output_context(&mut self, operation: &str) -> anyhow::Result<Output> {
        let program = self.get_program().to_string_lossy().into_owned();
        self.output()
            .with_context(|| format!("{operation}: spawn child process `{program}`"))
    }
}

fn path(path: &Path) -> &str {
    path.to_str().expect("fixture path is UTF-8")
}
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
