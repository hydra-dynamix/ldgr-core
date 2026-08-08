use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use flate2::write::GzEncoder;
use flate2::Compression;
use ldgr_core::release_index::{
    DetachedSignature, ReleaseChannel, ReleaseKeyring, ReleasePublicKey,
};
use ldgr_core::update::catalog::{
    canonical_catalog_bytes, CorePlatformArchive, CoreRelease, CoreReleaseCompatibility,
    CoreReleaseMetadata, CoreUpdateCatalog, PairedAgentctlRelease,
};
use sha2::{Digest, Sha256};
use tar::{Builder, Header};

fn repository() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn release_workflow_catalog_publication_is_last_and_matrix_gated() -> anyhow::Result<()> {
    let workflow = fs::read_to_string(repository().join(".github/workflows/release.yml"))?;
    let sign = workflow
        .find("name: Sign complete release candidate")
        .context("sign job missing")?;
    let gate = workflow
        .find("name: Previous-version update ${{ matrix.platform }}")
        .context("previous-version matrix missing")?;
    let publish = workflow
        .find("name: Publish signed catalog last")
        .context("publish job missing")?;
    ensure!(sign < gate && gate < publish);
    let publish_job = &workflow[publish..];
    ensure!(publish_job.contains("- release-gate"));
    ensure!(
        publish_job
            .rfind("git push origin HEAD:main")
            .context("catalog push missing")?
            > publish_job
                .find("Verify staged release assets")
                .context("asset verification missing")?
    );
    for required in [
        "LDGR_CATALOG_SIGNING_KEY",
        "LDGR_ARCHIVE_SIGNING_KEY",
        "dist/*.tar.gz.sig",
        "(cd candidate && sha256sum -c \"dist/$archive.sha256\")",
        "previous-version.txt",
        "injected_failure_after_each_paired_activation_checkpoint_restores_every_target",
        "core-index.json.sig",
    ] {
        ensure!(workflow.contains(required), "workflow omits {required}");
    }
    for platform in [
        "linux-x86_64",
        "linux-aarch64",
        "macos-x86_64",
        "macos-aarch64",
        "windows-x86_64",
    ] {
        ensure!(
            workflow.matches(platform).count() >= 2,
            "{platform} is not in both matrices"
        );
    }
    Ok(())
}

#[test]
fn release_workflow_runs_actionlint_and_signed_installer_fixtures() -> anyhow::Result<()> {
    let workflow = fs::read_to_string(repository().join(".github/workflows/release.yml"))?;
    ensure!(workflow.contains("docker://rhysd/actionlint:1.7.7"));
    ensure!(workflow.contains("if [[ \"$version\" == *-* ]]; then prerelease=\"true\"; fi"));
    ensure!(workflow.contains("prerelease input must match the semantic version channel"));
    ensure!(workflow.contains("default: false"));
    ensure!(workflow.matches("dtolnay/rust-toolchain@stable").count() >= 4);
    ensure!(workflow.contains("tests/install_catalog.ps1"));
    let shell = fs::read_to_string(repository().join("scripts/install.sh"))?;
    let powershell = fs::read_to_string(repository().join("scripts/install.ps1"))?;
    for installer in [&shell, &powershell] {
        ensure!(installer.contains("core-catalog.py"));
        ensure!(
            installer.contains("core-index.json.sig") || installer.contains("$catalogSource.sig")
        );
        ensure!(installer.contains("verify-archive"));
        ensure!(installer.contains("LDGR_CORE_RELEASE_KEYRING"));
        ensure!(!installer.contains("api.github.com/repos"));
        ensure!(!installer.contains("falling back to cargo"));
    }
    ensure!(shell.contains("[ \"$OFFLINE\" = \"1\" ] || require curl"));
    ensure!(shell.contains("--proto-redir '=https'"));
    ensure!(powershell.contains("$handler.AllowAutoRedirect = $false"));
    ensure!(powershell.contains("Installer redirects must remain HTTPS"));
    ensure!(shell.contains("agentctl $AGENTCTL_VERSION"));
    ensure!(powershell.contains("agentctl $expectedAgentctlVersion"));
    Ok(())
}

#[test]
fn installer_helper_verifies_catalog_checksum_signature_and_embedded_metadata() -> anyhow::Result<()>
{
    let directory = tempfile::tempdir()?;
    let key = SigningKey::from_bytes(&[37; 32]);
    let keyring = ReleaseKeyring {
        keys: vec![ReleasePublicKey {
            key_id: "fixture-root".into(),
            public_key: STANDARD.encode(key.verifying_key().to_bytes()),
        }],
    };
    let archive = directory
        .path()
        .join("ldgr-core-1.2.3-windows-x86_64.tar.gz");
    let metadata = CoreReleaseMetadata {
        schema_version: 1,
        package: "ldgr-core".into(),
        binary: "ldgr".into(),
        version: "1.2.3".into(),
        agentctl_version: "0.1.2".into(),
        agentctl_repository: "hydra-dynamix/agentctl".into(),
        agentctl_commit: "b".repeat(40),
        launcher_compatibility_schema: "ldgr.launcher-compatibility.v1".into(),
        error_recovery_schema: 1,
        platform: "windows-x86_64".into(),
        commit: "a".repeat(40),
        source_repository: "hydra-dynamix/ldgr-core".into(),
    };
    write_archive(&archive, &metadata)?;
    let archive_bytes = fs::read(&archive)?;
    let digest = format!("{:x}", Sha256::digest(&archive_bytes));
    let archive_signature = PathBuf::from(format!("{}.sig", archive.display()));
    let checksum = PathBuf::from(format!("{}.sha256", archive.display()));
    write_signature(&archive_signature, "fixture-root", &key, &archive_bytes)?;
    fs::write(&checksum, format!("{digest}  {}\n", archive.display()))?;
    let archive_url = file_url(&archive);
    let catalog = CoreUpdateCatalog {
        schema_version: 1,
        release_keys: Vec::new(),
        releases: vec![CoreRelease {
            version: "1.2.3".into(),
            channel: ReleaseChannel::Stable,
            minimum_updater_version: "1.0.0".into(),
            core_commit: "a".repeat(40),
            source_repository: "hydra-dynamix/ldgr-core".into(),
            agentctl: PairedAgentctlRelease {
                version: "0.1.2".into(),
                repository: "hydra-dynamix/agentctl".into(),
                commit: "b".repeat(40),
            },
            compatibility: CoreReleaseCompatibility {
                launcher_compatibility_schema: "ldgr.launcher-compatibility.v1".into(),
                error_recovery_schema: 1,
                release_metadata_schema: 1,
            },
            platforms: vec![CorePlatformArchive {
                platform: "windows-x86_64".into(),
                archive_url: archive_url.clone(),
                archive_root: "ldgr-core-1.2.3".into(),
                sha256: digest,
                signature_url: format!("{archive_url}.sig"),
                signing_key_id: "fixture-root".into(),
            }],
        }],
    };
    let canonical = canonical_catalog_bytes(&catalog)?;
    let catalog_path = directory.path().join("core-index.json");
    let catalog_signature = directory.path().join("core-index.json.sig");
    let keyring_path = directory.path().join("release-keyring.json");
    let resolved = directory.path().join("resolved.json");
    fs::write(&catalog_path, &canonical)?;
    write_signature(&catalog_signature, "fixture-root", &key, &canonical)?;
    fs::write(&keyring_path, serde_json::to_vec_pretty(&keyring)?)?;
    let helper = repository().join("scripts/core-catalog.py");

    let output = python(&helper)
        .args([
            "resolve",
            "--catalog",
            path(&catalog_path),
            "--signature",
            path(&catalog_signature),
            "--keyring",
            path(&keyring_path),
            "--platform",
            "windows-x86_64",
            "--version",
            "1.2.3",
            "--offline",
            "--output",
            path(&resolved),
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let output = python(&helper)
        .args([
            "verify-archive",
            "--resolved",
            path(&resolved),
            "--archive",
            path(&archive),
            "--checksum",
            path(&checksum),
            "--signature",
            path(&archive_signature),
        ])
        .output()?;
    ensure!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );

    let mut tampered_catalog = serde_json::to_value(&catalog)?;
    tampered_catalog["releases"][0]["minimum_updater_version"] =
        serde_json::Value::String("1.0.1".into());
    fs::write(&catalog_path, serde_json::to_vec_pretty(&tampered_catalog)?)?;
    let output = python(&helper)
        .args([
            "resolve",
            "--catalog",
            path(&catalog_path),
            "--signature",
            path(&catalog_signature),
            "--keyring",
            path(&keyring_path),
            "--platform",
            "windows-x86_64",
            "--offline",
            "--output",
            path(&resolved),
        ])
        .output()?;
    ensure!(
        !output.status.success(),
        "catalog metadata tampering was accepted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    fs::write(&catalog_path, &canonical)?;
    let mut small_order_public_key = [0_u8; 32];
    small_order_public_key[0] = 1;
    let mut small_order_signature = [0_u8; 64];
    small_order_signature[0] = 1;
    fs::write(
        &keyring_path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "keys": [{
                "key_id": "fixture-small-order",
                "public_key": STANDARD.encode(small_order_public_key),
            }],
        }))?,
    )?;
    fs::write(
        &catalog_signature,
        serde_json::to_vec_pretty(&DetachedSignature {
            algorithm: "Ed25519".into(),
            key_id: "fixture-small-order".into(),
            signature: STANDARD.encode(small_order_signature),
        })?,
    )?;
    let output = python(&helper)
        .args([
            "resolve",
            "--catalog",
            path(&catalog_path),
            "--signature",
            path(&catalog_signature),
            "--keyring",
            path(&keyring_path),
            "--platform",
            "windows-x86_64",
            "--offline",
            "--output",
            path(&resolved),
        ])
        .output()?;
    ensure!(
        !output.status.success(),
        "small-order Ed25519 material was accepted: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(())
}

fn python(helper: &Path) -> Command {
    let mut command = Command::new("python");
    command.arg(helper);
    command
}

fn path(path: &Path) -> &str {
    path.to_str().expect("fixture paths are UTF-8")
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display()).replace('\\', "/")
}

fn write_signature(
    path: &Path,
    key_id: &str,
    key: &SigningKey,
    bytes: &[u8],
) -> anyhow::Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(&DetachedSignature {
            algorithm: "Ed25519".into(),
            key_id: key_id.into(),
            signature: STANDARD.encode(key.sign(bytes).to_bytes()),
        })?,
    )?;
    Ok(())
}

fn write_archive(path: &Path, metadata: &CoreReleaseMetadata) -> anyhow::Result<()> {
    let file = fs::File::create(path)?;
    let mut archive = Builder::new(GzEncoder::new(file, Compression::default()));
    append(
        &mut archive,
        "ldgr-core-1.2.3/RELEASE-METADATA.json",
        &serde_json::to_vec(metadata)?,
    )?;
    append(
        &mut archive,
        "ldgr-core-1.2.3/windows-x86_64/ldgr.exe",
        b"core",
    )?;
    append(
        &mut archive,
        "ldgr-core-1.2.3/windows-x86_64/agentctl.exe",
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
