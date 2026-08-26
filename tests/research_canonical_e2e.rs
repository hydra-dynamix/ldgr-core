use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use anyhow::{ensure, Context};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use flate2::write::GzEncoder;
use flate2::Compression;
use ldgr_core::adapter_compatibility::parse_adapter_compatibility_v2;
use ldgr_core::release_index::parse_release_index;
use ldgr_core::update::catalog::canonical_adapter_catalog_bytes;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[test]
#[ignore = "requires LDGR_RESEARCH_E2E_BINARY and LDGR_RESEARCH_E2E_SOURCE"]
fn canonical_research_install_workflow_is_clean_room_and_actionable() -> anyhow::Result<()> {
    let research_binary = required_path("LDGR_RESEARCH_E2E_BINARY")?;
    let research_source = required_path("LDGR_RESEARCH_E2E_SOURCE")?;
    ensure!(research_binary.is_file(), "research binary is missing");
    ensure!(
        research_source.join("adapter.toml").is_file(),
        "research source is missing"
    );

    let temp = TempDir::new()?;
    let root = fs::canonicalize(temp.path())?.join("canonical research e2e-Δ");
    let home = root.join("isolated home");
    let project = root.join("project");
    let fixture = root.join("signed release");
    let empty_path = root.join("empty PATH");
    for path in [&home, &project, &fixture, &empty_path] {
        fs::create_dir_all(path)?;
    }
    write_harness_config(&home)?;

    let release = build_signed_release(&fixture, &research_source, &research_binary)?;
    let mut command = core_command(&project, &home, &release, &empty_path);
    command.args(["adapter", "install", "research", "--yes", "--offline"]);

    // The catalog itself is authenticated before Core touches the archive or
    // creates a discoverable adapter installation.
    let valid_catalog_signature = fs::read(&release.catalog_signature)?;
    fs::write(
        &release.catalog_signature,
        serde_json::to_vec(&serde_json::json!({
            "algorithm": "Ed25519",
            "key_id": "research-e2e",
            "signature": STANDARD.encode([0_u8; 64])
        }))?,
    )?;
    let rejected = command.output()?;
    assert_failure_contains(&rejected, "untrusted adapter update catalog")?;
    ensure!(
        !home.join(".ldgr/adapters/research").exists(),
        "untrusted catalog mutated the adapter installation"
    );
    fs::write(&release.catalog_signature, valid_catalog_signature)?;

    let installed = run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["adapter", "install", "research", "--yes", "--offline"],
    )?;
    ensure!(
        installed.contains("Installed adapter `research`"),
        "{installed}"
    );

    let adapter_root = home.join(".ldgr/adapters/research");
    let receipt_path = adapter_root.join("installation-receipt.json");
    let receipt: serde_json::Value = serde_json::from_slice(&fs::read(&receipt_path)?)?;
    ensure!(receipt["schema_version"] == 2, "v2 receipt was not written");
    ensure!(receipt["domain"] == "research", "receipt domain mismatch");
    ensure!(
        receipt["compatibility"].is_object(),
        "receipt omitted compatibility"
    );
    ensure!(
        receipt["compatibility_sha256"] == release.compatibility_sha256,
        "receipt compatibility fingerprint mismatch"
    );

    let show = run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["adapter", "show", "research", "--json"],
    )?;
    let discovered: serde_json::Value = serde_json::from_str(&show)?;
    ensure!(
        discovered["state"] == "ready",
        "adapter was not ready: {show}"
    );
    let dispatched_binary = discovered["command_namespaces"][0]["argv"][0]
        .as_str()
        .context("discovered research argv is missing")?;
    let dispatched_binary = PathBuf::from(dispatched_binary);
    ensure!(
        dispatched_binary.is_absolute() && dispatched_binary.is_file(),
        "Core did not persist an absolute adapter binary argv: {}",
        dispatched_binary.display()
    );
    let receipt_binary = PathBuf::from(
        receipt["binary_path"]
            .as_str()
            .context("receipt binary path is missing")?,
    );
    ensure!(
        fs::canonicalize(&receipt_binary)? == fs::canonicalize(&dispatched_binary)?,
        "receipt and dispatch binary disagree"
    );

    // No ldgr-research executable is on PATH. Every command below must be
    // discovered and launched through Core's absolute manifest argv.
    run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["research", "install"],
    )?;
    let manifest_after_first = fs::read(adapter_root.join("adapter.toml"))?;
    let prompt_path = home.join("configured prompts/research-loop.md");
    let prompt_after_first = fs::read(&prompt_path)?;
    let receipt_after_first = fs::read(&receipt_path)?;
    run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["research", "install"],
    )?;
    ensure!(
        fs::read(adapter_root.join("adapter.toml"))? == manifest_after_first,
        "research install changed the installed manifest on its idempotent rerun"
    );
    ensure!(
        fs::read(&prompt_path)? == prompt_after_first,
        "research install changed the prompt on its idempotent rerun"
    );
    ensure!(
        fs::read(&receipt_path)? == receipt_after_first,
        "research install changed Core's signed-release receipt"
    );
    let manifest: toml::Value = toml::from_str(&String::from_utf8(manifest_after_first)?)?;
    ensure!(
        Path::new(
            manifest["commands"][0]["argv"][0]
                .as_str()
                .context("installed manifest argv is missing")?
        )
        .is_absolute(),
        "research resource installation regressed absolute dispatch"
    );

    let workflow = run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["research", "workflow"],
    )?;
    ensure!(
        workflow.contains("research"),
        "research workflow output was empty"
    );
    let init = run_core(&project, &home, &release, &empty_path, ["research", "init"])?;
    ensure!(
        init.contains("activated LDGR research loop prompt"),
        "research init did not activate its prompt: {init}"
    );
    run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["research", "doctor"],
    )?;

    let research_db = project.join(".ldgr/research/research.db");
    let local = Connection::open(&research_db)?;
    let local_schema: i64 =
        local.query_row("SELECT max(version) FROM schema_migrations", [], |row| {
            row.get(0)
        })?;
    ensure!(
        local_schema == 4,
        "research local migrations stopped at {local_schema}"
    );
    drop(local);

    let core_db = project.join(".ldgr/ldgr.db");
    let central = Connection::open(&core_db)?;
    let prompt_status: String = central.query_row(
        "SELECT status FROM prompt WHERE slug = 'research-loop'",
        [],
        |row| row.get(0),
    )?;
    ensure!(
        prompt_status == "active",
        "research-loop prompt is not active"
    );
    central.execute_batch("CREATE TABLE additive_core_patch_fixture(id INTEGER PRIMARY KEY);")?;
    drop(central);
    run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["research", "doctor"],
    )?;

    // An incompatible installed candidate remains inspectable and gives exact
    // Core-owned repair guidance instead of disappearing or trying PATH.
    let sidecar_path = adapter_root.join("adapter-compatibility.json");
    let valid_sidecar = fs::read(&sidecar_path)?;
    let mut incompatible: serde_json::Value = serde_json::from_slice(&valid_sidecar)?;
    incompatible["compatibility"]["minimum_core_schema"] = serde_json::json!(999);
    fs::write(&sidecar_path, serde_json::to_vec_pretty(&incompatible)?)?;
    let blocked = run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["adapter", "show", "research", "--json"],
    )?;
    let blocked: serde_json::Value = serde_json::from_str(&blocked)?;
    ensure!(
        blocked["state"] == "blocked",
        "incompatible adapter disappeared"
    );
    ensure!(
        blocked["reasons"][0]["code"] == "compatibility.minimum_core_schema_unsatisfied",
        "blocked adapter did not expose its stable reason: {blocked}"
    );
    ensure!(
        blocked["repair"]["command"] == "ldgr update --adapter research",
        "blocked adapter did not expose actionable repair argv: {blocked}"
    );
    let failed_dispatch = core_command(&project, &home, &release, &empty_path)
        .args(["research", "doctor"])
        .output()?;
    assert_failure_contains(
        &failed_dispatch,
        "repair with `ldgr update --adapter research`",
    )?;

    fs::write(sidecar_path, valid_sidecar)?;
    run_core(
        &project,
        &home,
        &release,
        &empty_path,
        ["research", "doctor"],
    )?;
    Ok(())
}

struct SignedRelease {
    catalog: PathBuf,
    catalog_signature: PathBuf,
    keyring: PathBuf,
    compatibility_sha256: String,
}

fn build_signed_release(
    fixture: &Path,
    source: &Path,
    binary: &Path,
) -> anyhow::Result<SignedRelease> {
    let version = "0.1.6";
    let archive_root_name = format!("ldgr-research-{version}");
    let archive_root = fixture.join(&archive_root_name);
    fs::create_dir_all(&archive_root)?;
    for file in [
        "adapter.toml",
        "adapter-compatibility.json",
        "adapter-database-contract.json",
        "adapter-resources.json",
        "loop-prompt.md",
    ] {
        fs::copy(source.join(file), archive_root.join(file))
            .with_context(|| format!("failed to package research {file}"))?;
    }
    for directory in ["templates", "docs", "scripts", "commands"] {
        let from = source.join(directory);
        if from.is_dir() {
            copy_tree(&from, &archive_root.join(directory))?;
        }
    }
    fs::create_dir_all(archive_root.join("prompts"))?;
    fs::copy(
        source.join("loop-prompt.md"),
        archive_root.join("prompts/research-loop.md"),
    )?;

    let platform = platform_tag();
    let binary_name = if cfg!(windows) {
        "ldgr-research.exe"
    } else {
        "ldgr-research"
    };
    let packaged_binary = archive_root.join(&platform).join(binary_name);
    fs::create_dir_all(packaged_binary.parent().context("binary has no parent")?)?;
    fs::copy(binary, &packaged_binary)?;

    let archive = fixture.join(format!("ldgr-research-{version}-{platform}.tar.gz"));
    let encoder = GzEncoder::new(fs::File::create(&archive)?, Compression::default());
    let mut tar = tar::Builder::new(encoder);
    tar.append_dir_all(&archive_root_name, &archive_root)?;
    tar.into_inner()?.finish()?;
    let archive_bytes = fs::read(&archive)?;

    let signing_key = SigningKey::from_bytes(&[91; 32]);
    let archive_signature = archive.with_extension("tar.gz.sig");
    write_signature(&archive_signature, &signing_key, &archive_bytes)?;
    let keyring = fixture.join("trusted-keyring.json");
    fs::write(
        &keyring,
        serde_json::to_vec_pretty(&serde_json::json!({
            "keys": [{
                "key_id": "research-e2e",
                "public_key": STANDARD.encode(signing_key.verifying_key().to_bytes())
            }]
        }))?,
    )?;

    let sidecar_text = fs::read_to_string(source.join("adapter-compatibility.json"))?;
    let sidecar = parse_adapter_compatibility_v2(&sidecar_text)?;
    let compatibility_sha256 = sidecar.compatibility.compatibility_sha256()?;
    let compatibility: serde_json::Value =
        serde_json::from_str::<serde_json::Value>(&sidecar_text)?["compatibility"].clone();
    let catalog_value = serde_json::json!({
        "schema_version": 2,
        "adapters": [{
            "domain": "research",
            "primary_namespace": "research",
            "title": "Research adapter",
            "aliases": [],
            "classification": "open_source",
            "releases": [{
                "version": version,
                "channel": "stable",
                "compatibility": compatibility,
                "compatibility_sha256": compatibility_sha256.clone(),
                "platforms": [{
                    "platform": platform,
                    "asset_url": file_url(&archive),
                    "archive_root": archive_root_name,
                    "binary": binary_name,
                    "sha256": format!("{:x}", Sha256::digest(&archive_bytes)),
                    "signature_url": file_url(&archive_signature),
                    "signing_key_id": "research-e2e",
                    "resource_manifest": "adapter-resources.json"
                }]
            }]
        }]
    });
    let catalog = fixture.join("index.json");
    let catalog_text = serde_json::to_string_pretty(&catalog_value)?;
    fs::write(&catalog, &catalog_text)?;
    let parsed = parse_release_index(&catalog_text)?;
    let catalog_signature = PathBuf::from(format!("{}.sig", catalog.display()));
    write_signature(
        &catalog_signature,
        &signing_key,
        &canonical_adapter_catalog_bytes(&parsed)?,
    )?;

    Ok(SignedRelease {
        catalog,
        catalog_signature,
        keyring,
        compatibility_sha256,
    })
}

fn write_signature(path: &Path, key: &SigningKey, bytes: &[u8]) -> anyhow::Result<()> {
    fs::write(
        path,
        serde_json::to_vec_pretty(&serde_json::json!({
            "algorithm": "Ed25519",
            "key_id": "research-e2e",
            "signature": STANDARD.encode(key.sign(bytes).to_bytes())
        }))?,
    )?;
    Ok(())
}

fn core_command(project: &Path, home: &Path, release: &SignedRelease, path: &Path) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_ldgr"));
    command
        .current_dir(project)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("LDGR_HOME", home.join(".ldgr"))
        .env("LDGR_ADAPTER_INDEX", &release.catalog)
        .env("LDGR_ADAPTER_RELEASE_KEYRING", &release.keyring)
        .env("LDGR_NO_UPDATE_CHECK", "1")
        .env("PATH", path)
        .env_remove("LDGR_ADAPTER_PATH")
        .env_remove("LDGR_RESEARCH_E2E_BINARY")
        .env_remove("LDGR_RESEARCH_E2E_SOURCE");
    command
}

fn run_core<I, S>(
    project: &Path,
    home: &Path,
    release: &SignedRelease,
    path: &Path,
    args: I,
) -> anyhow::Result<String>
where
    I: IntoIterator<Item = S>,
    S: AsRef<OsStr>,
{
    let output = core_command(project, home, release, path)
        .args(args)
        .output()?;
    ensure!(
        output.status.success(),
        "Core command failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    Ok(String::from_utf8(output.stdout)?)
}

fn assert_failure_contains(output: &Output, needle: &str) -> anyhow::Result<()> {
    ensure!(!output.status.success(), "command unexpectedly succeeded");
    let combined = format!(
        "{}{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    ensure!(
        combined.contains(needle),
        "missing `{needle}` in:\n{combined}"
    );
    Ok(())
}

fn required_path(name: &str) -> anyhow::Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("{name} is required for the ignored canonical Research E2E"))
}

fn write_harness_config(home: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(home.join(".ldgr"))?;
    fs::write(
        home.join(".ldgr/config.json"),
        serde_json::to_vec_pretty(&serde_json::json!({
            "schema_version": 1,
            "selected_harnesses": ["codex"],
            "installed": [{
                "harness": "codex",
                "prompt_paths": [home.join("configured prompts")],
                "skill_paths": [home.join("configured skills")]
            }]
        }))?,
    )?;
    Ok(())
}

fn copy_tree(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let target = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else {
            fs::copy(entry.path(), target)?;
        }
    }
    Ok(())
}

fn platform_tag() -> String {
    format!("{}-{}", std::env::consts::OS, std::env::consts::ARCH)
}

fn file_url(path: &Path) -> String {
    format!("file://{}", path.display())
}
