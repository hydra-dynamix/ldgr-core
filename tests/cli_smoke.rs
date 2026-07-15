use std::fs;
use std::io::{BufRead, BufReader, ErrorKind, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::Path;
use std::process::{Command as StdCommand, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::Context;
use assert_cmd::Command;
use base64::{engine::general_purpose::STANDARD, Engine as _};
use ed25519_dalek::{Signer, SigningKey};
use predicates::prelude::*;
use rusqlite::Connection;
use sha2::{Digest, Sha256};
use tempfile::TempDir;

#[test]
fn status_and_context_migrate_released_v1_databases_and_report_the_backup() -> anyhow::Result<()> {
    for entrypoint in [["status"], ["context"]] {
        let project = TempDir::new()?;
        let db_path = project.path().join(".ldgr/ldgr.db");
        let artifact_root = project.path().join(".ldgr/artifacts");
        run(project.path(), &db_path, &artifact_root, ["init"])?;
        downgrade_cli_fixture_to_v1(&db_path)?;

        command(project.path(), &db_path, &artifact_root, entrypoint)?
            .assert()
            .success()
            .stderr(predicate::str::contains(
                "migration: LDGR Core upgraded schema v1 -> v2; verified backup:",
            ));

        let connection = Connection::open(&db_path)?;
        let version: i64 = connection.query_row(
            "SELECT version FROM schema_version WHERE id = 1",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(version, 2);
        assert!(fs::read_dir(db_path.parent().unwrap())?.any(|entry| {
            entry.is_ok_and(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .contains("backup-schema-v1-to-v2")
            })
        }));
    }
    Ok(())
}

#[test]
fn explicit_migrate_command_returns_machine_readable_result() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    run(project.path(), &db_path, &artifact_root, ["init"])?;
    downgrade_cli_fixture_to_v1(&db_path)?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["migrate", "--json"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("\"migrated\": true"))
    .stdout(predicate::str::contains("\"from_schema_version\": 1"))
    .stdout(predicate::str::contains("\"to_schema_version\": 2"))
    .stdout(predicate::str::contains("backup-schema-v1-to-v2"));
    Ok(())
}

#[test]
fn init_status_and_context_share_installed_domain_help_projection() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let adapter = project.path().join("adapter");
    write_adapter_namespace_fixture(&adapter, "bench", "fixture", "[\"true\"]")?;
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../ldgr-bench/adapter-database-contract.json"),
        adapter.join("adapter-database-contract.json"),
    )?;
    let manifest = fs::read_to_string(adapter.join("adapter.toml"))?
        .replace("core_version = \"0.1\"", "core_version = \"generated\"")
        .replace(
            "namespace = \"fixture\"",
            "namespace = \"fixture\"\nstatus_args = [\"status\"]",
        );
    fs::write(adapter.join("adapter.toml"), manifest)?;
    let instruction = "Run ldgr fixture --help for the extended command surface.";

    let mut init = isolated_command(project.path())?;
    init.env("LDGR_ADAPTER_PATH", &adapter).arg("init");
    init.assert()
        .success()
        .stdout(predicate::str::contains(instruction));

    let mut status = isolated_command(project.path())?;
    status
        .env("LDGR_ADAPTER_PATH", &adapter)
        .args(["status", "--json"]);
    status
        .assert()
        .success()
        .stdout(predicate::str::contains(instruction))
        .stdout(predicate::str::contains(
            "\"help_command\": \"ldgr fixture --help\"",
        ))
        .stdout(predicate::str::contains(
            "\"status_command\": \"ldgr fixture status\"",
        ));

    let mut context = isolated_command(project.path())?;
    context
        .env("LDGR_ADAPTER_PATH", &adapter)
        .args(["context", "--json"]);
    context
        .assert()
        .success()
        .stdout(predicate::str::contains(instruction))
        .stdout(predicate::str::contains(
            "\"help_command\": \"ldgr fixture --help\"",
        ));
    Ok(())
}

#[test]
fn adapter_namespace_aliases_are_explicit_and_typos_never_execute() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let adapter = project.path().join("adapter");
    write_adapter_namespace_fixture(&adapter, "fixture", "fixture", "[\"true\"]")?;
    let manifest = fs::read_to_string(adapter.join("adapter.toml"))?.replace(
        "namespace = \"fixture\"",
        "namespace = \"fixture\"\naliases = [\"fx\"]",
    );
    fs::write(adapter.join("adapter.toml"), manifest)?;

    let mut alias = isolated_command(project.path())?;
    alias
        .env("LDGR_ADAPTER_PATH", &adapter)
        .args(["fx", "--help"]);
    alias.assert().success();

    let mut typo = isolated_command(project.path())?;
    typo.env("LDGR_ADAPTER_PATH", &adapter).arg("fxture");
    typo.assert().failure();

    let mut exact_help = isolated_command(project.path())?;
    exact_help
        .env("LDGR_ADAPTER_PATH", &adapter)
        .args(["fixture", "--help"]);
    exact_help.assert().success();
    Ok(())
}

#[test]
fn adapter_install_list_reads_explicit_release_index_without_recompile() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let index = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures/release-index/open-and-commercial.json");
    let mut command = isolated_command(project.path())?;
    command
        .env("LDGR_ADAPTER_INDEX", index)
        .args(["adapter", "install", "list"]);
    command
        .assert()
        .success()
        .stdout(predicate::str::contains("evidence — LDGR Evidence adapter"));
    Ok(())
}

#[test]
fn first_install_requires_explicit_telemetry_choice_and_remembers_no() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let home = project.path().join(".ldgr/test-empty-home");
    let consent_path = home.join(".ldgr/telemetry-consent.json");

    let mut missing = isolated_command(project.path())?;
    missing.args(["install", "--harness", "codex", "--yes", "--no-agentctl"]);
    missing
        .assert()
        .failure()
        .stderr(predicate::str::contains("telemetry choice required"))
        .stderr(predicate::str::contains("--yes` is not telemetry consent"));
    assert!(!consent_path.exists());

    let mut decline = isolated_command(project.path())?;
    decline.args([
        "install",
        "--harness",
        "codex",
        "--yes",
        "--no-agentctl",
        "--telemetry",
        "disable",
    ]);
    decline.assert().success();
    let consent: serde_json::Value = serde_json::from_str(&fs::read_to_string(&consent_path)?)?;
    assert_eq!(consent["decision"], "disabled");
    assert!(home.join(".ldgr/config.json").is_file());

    let mut reinstall = isolated_command(project.path())?;
    reinstall.args(["install", "--harness", "codex", "--yes", "--no-agentctl"]);
    reinstall.assert().success();
    let remembered: serde_json::Value = serde_json::from_str(&fs::read_to_string(&consent_path)?)?;
    assert_eq!(remembered["decision"], "disabled");
    Ok(())
}

#[test]
fn explicit_install_opt_in_records_enabled_consent() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let home = project.path().join(".ldgr/test-empty-home");
    let mut install = isolated_command(project.path())?;
    install.args([
        "install",
        "--harness",
        "codex",
        "--yes",
        "--no-agentctl",
        "--telemetry",
        "enable",
    ]);
    install.assert().success();
    let consent: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        home.join(".ldgr/telemetry-consent.json"),
    )?)?;
    assert_eq!(consent["schema_version"], 1);
    assert_eq!(consent["policy_version"], 1);
    assert_eq!(consent["decision"], "enabled");
    Ok(())
}

#[test]
fn telemetry_controls_report_override_and_disable_without_network() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let home = project.path().join(".ldgr/test-empty-home");
    let ldgr_home = home.join(".ldgr");
    let consent_path = ldgr_home.join("telemetry-consent.json");

    let mut initial_status = isolated_command(project.path())?;
    initial_status.args(["telemetry", "status"]);
    initial_status
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "sequence collection decision: undecided",
        ))
        .stdout(predicate::str::contains("effective collection: disabled"))
        .stdout(predicate::str::contains(
            "eligible numerical protocols: core-work/v1",
        ));
    assert!(!consent_path.exists());

    let mut enable = isolated_command(project.path())?;
    enable.args(["telemetry", "enable"]);
    enable
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "does not include project data, names, labels, identifiers, timestamps, environment information, or linkable installation data",
        ))
        .stdout(predicate::str::contains("sequence collection: enabled"));

    let mut killed_status = isolated_command(project.path())?;
    killed_status
        .env("LDGR_TELEMETRY", "off")
        .args(["telemetry", "status"]);
    killed_status
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "sequence collection decision: enabled",
        ))
        .stdout(predicate::str::contains("effective collection: disabled"))
        .stdout(predicate::str::contains("environment kill switch: active"));

    let pending = ldgr_home.join("telemetry-pending");
    fs::create_dir_all(&pending)?;
    fs::write(pending.join("sequence.json"), "[0,1,3]")?;

    let mut disable = isolated_command(project.path())?;
    disable.args(["telemetry", "disable"]);
    disable
        .assert()
        .success()
        .stdout(predicate::str::contains("sequence collection: disabled"));
    assert!(!pending.exists());
    let consent: serde_json::Value = serde_json::from_str(&fs::read_to_string(consent_path)?)?;
    assert_eq!(consent["decision"], "disabled");
    Ok(())
}

#[test]
fn adapter_install_resolves_and_installs_fixture_from_index() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let bundle = project.path().join("fixture-1.2.3");
    write_adapter_namespace_fixture(&bundle, "fixture", "fixture", "[\"true\"]")?;
    fs::create_dir_all(bundle.join("skills/fixture"))?;
    fs::write(bundle.join("skills/fixture/SKILL.md"), "fixture skill")?;
    fs::create_dir_all(bundle.join("extensions"))?;
    fs::write(bundle.join("extensions/fixture.ts"), "export {}")?;
    fs::create_dir_all(bundle.join("commands"))?;
    fs::write(bundle.join("commands/fixture.md"), "fixture command")?;
    fs::write(
        bundle.join("adapter-resources.json"),
        serde_json::json!({
            "schema_version": 1,
            "resources": [
                {"kind":"prompt","harnesses":["pi","codex"],"source":"prompts/loop.md","destination":"fixture-loop.md"},
                {"kind":"skill","harnesses":["pi","codex","claude","openclaw"],"source":"skills/fixture","destination":"fixture"},
                {"kind":"extension","harnesses":["pi"],"source":"extensions/fixture.ts","destination":"fixture.ts"},
                {"kind":"command","harnesses":["claude","openclaw"],"source":"commands/fixture.md","destination":"fixture.md"}
            ]
        })
        .to_string(),
    )?;
    let isolated_home = project.path().join(".ldgr/test-empty-home");
    fs::create_dir_all(isolated_home.join(".ldgr"))?;
    fs::write(
        isolated_home.join(".ldgr/config.json"),
        serde_json::json!({
            "schema_version": 1,
            "selected_harnesses": ["pi","codex","claude","openclaw"],
            "installed": [
                {"harness":"pi","prompt_paths":[isolated_home.join("pi-prompts")],"skill_paths":[isolated_home.join("pi-skills")],"extension_paths":[isolated_home.join("pi-extensions")]},
                {"harness":"codex","prompt_paths":[isolated_home.join("codex-prompts")],"skill_paths":[isolated_home.join("codex-skills")]},
                {"harness":"claude","skill_paths":[isolated_home.join("claude-skills")],"command_paths":[isolated_home.join("claude-commands")]},
                {"harness":"openclaw","skill_paths":[isolated_home.join("openclaw-skills")],"command_paths":[isolated_home.join("openclaw-commands")]}
            ]
        })
        .to_string(),
    )?;
    let all_harness_config = fs::read_to_string(isolated_home.join(".ldgr/config.json"))?;
    let archive = project.path().join("fixture.tar.gz");
    let status = StdCommand::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(project.path())
        .arg("fixture-1.2.3")
        .status()?;
    assert!(status.success());
    let archive_sha256 = format!("{:x}", Sha256::digest(fs::read(&archive)?));
    let signing_key = SigningKey::from_bytes(&[42; 32]);
    let signature = project.path().join("fixture.sig");
    fs::write(
        &signature,
        serde_json::json!({
            "algorithm": "Ed25519",
            "key_id": "test",
            "signature": STANDARD.encode(signing_key.sign(&fs::read(&archive)?).to_bytes())
        })
        .to_string(),
    )?;
    let keyring = project.path().join("keys.json");
    fs::write(
        &keyring,
        serde_json::json!({
            "keys": [{
                "key_id": "test",
                "public_key": STANDARD.encode(signing_key.verifying_key().to_bytes())
            }]
        })
        .to_string(),
    )?;
    let platform = format!(
        "{}-{}",
        std::env::consts::OS,
        match std::env::consts::ARCH {
            "aarch64" => "aarch64",
            "x86_64" => "x86_64",
            other => other,
        }
    );
    let index = project.path().join("index.json");
    fs::write(
        &index,
        serde_json::json!({
            "schema_version": 1,
            "adapters": [{
                "domain": "fixture",
                "primary_namespace": "fixture",
                "title": "Fixture adapter",
                "classification": "open_source",
                "releases": [{
                    "version": "1.2.3",
                    "channel": "stable",
                    "core_compatibility": ">=0.1.0, <0.2.0",
                    "platforms": [{
                        "platform": platform,
                        "asset_url": format!("file://{}", archive.display()),
                        "archive_root": "fixture-1.2.3",
                        "binary": "ldgr-fixture",
                        "sha256": archive_sha256,
                        "signature_url": format!("file://{}", signature.display()),
                        "signing_key_id": "test",
                        "resource_manifest": "adapter-resources.json"
                    }]
                }]
            }]
        })
        .to_string(),
    )?;
    let install_root = project.path().join("installed-fixture");
    let mut command = isolated_command(project.path())?;
    command
        .env("LDGR_ADAPTER_INDEX", &index)
        .env("LDGR_ADAPTER_RELEASE_KEYRING", &keyring)
        .args([
            "adapter",
            "install",
            "fixture",
            "--version",
            "1.2.3",
            "--yes",
            "--offline",
            "--install-root",
        ])
        .arg(&install_root);
    command
        .assert()
        .success()
        .stdout(predicate::str::contains("Resolved version 1.2.3"));
    assert!(install_root.join("adapter.toml").is_file());
    let receipt: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        install_root.join("installation-receipt.json"),
    )?)?;
    assert_eq!(receipt["version"], "1.2.3");
    assert_eq!(receipt["signing_key_id"], "test");
    for expected in [
        "pi-prompts/fixture-loop.md",
        "codex-prompts/fixture-loop.md",
        "pi-skills/fixture/SKILL.md",
        "codex-skills/fixture/SKILL.md",
        "claude-skills/fixture/SKILL.md",
        "openclaw-skills/fixture/SKILL.md",
        "pi-extensions/fixture.ts",
        "claude-commands/fixture.md",
        "openclaw-commands/fixture.md",
    ] {
        assert!(isolated_home.join(expected).is_file(), "missing {expected}");
    }
    let mut show = isolated_command(project.path())?;
    show.env("LDGR_ADAPTER_PATH", &install_root)
        .args(["adapter", "show", "fixture", "--json"]);
    show.assert()
        .success()
        .stdout(predicate::str::contains("installation_receipt"));
    fs::write(
        isolated_home.join(".ldgr/config.json"),
        serde_json::json!({
            "schema_version": 1,
            "selected_harnesses": ["codex"],
            "installed": [{"harness":"codex","prompt_paths":[isolated_home.join("codex-prompts")],"skill_paths":[isolated_home.join("codex-skills")]}]
        })
        .to_string(),
    )?;
    let mut reconcile = isolated_command(project.path())?;
    reconcile
        .env("LDGR_ADAPTER_PATH", &install_root)
        .args(["adapter", "reconcile", "fixture"]);
    reconcile.assert().success();
    assert!(!isolated_home.join("pi-prompts/fixture-loop.md").exists());
    assert!(isolated_home
        .join("codex-prompts/fixture-loop.md")
        .is_file());
    fs::write(isolated_home.join(".ldgr/config.json"), &all_harness_config)?;
    let mut reconcile_all = isolated_command(project.path())?;
    reconcile_all
        .env("LDGR_ADAPTER_PATH", &install_root)
        .args(["adapter", "reconcile", "fixture"]);
    reconcile_all.assert().success();
    assert!(isolated_home.join("pi-prompts/fixture-loop.md").is_file());
    let mut update_index: serde_json::Value = serde_json::from_str(&fs::read_to_string(&index)?)?;
    let mut newer = update_index["adapters"][0]["releases"][0].clone();
    newer["version"] = serde_json::Value::String("1.2.4".to_owned());
    update_index["adapters"][0]["releases"]
        .as_array_mut()
        .expect("release fixture is an array")
        .push(newer);
    fs::write(&index, update_index.to_string())?;
    let mut update_check = isolated_command(project.path())?;
    update_check
        .env("LDGR_ADAPTER_PATH", &install_root)
        .env("LDGR_ADAPTER_INDEX", &index)
        .env("LDGR_ADAPTER_RELEASE_KEYRING", &keyring)
        .args(["adapter", "update", "fixture", "--check"]);
    update_check
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "latest_compatible=1.2.4 update_available=true",
        ));
    let mut update = isolated_command(project.path())?;
    update
        .env("LDGR_ADAPTER_PATH", &install_root)
        .env("LDGR_ADAPTER_INDEX", &index)
        .env("LDGR_ADAPTER_RELEASE_KEYRING", &keyring)
        .args(["adapter", "update", "fixture"]);
    update.assert().success();
    let updated_receipt: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        install_root.join("installation-receipt.json"),
    )?)?;
    assert_eq!(updated_receipt["version"], "1.2.4");
    let mut no_op = isolated_command(project.path())?;
    no_op
        .env("LDGR_ADAPTER_PATH", &install_root)
        .env("LDGR_ADAPTER_INDEX", &index)
        .args(["adapter", "update", "fixture", "--check"]);
    no_op
        .assert()
        .success()
        .stdout(predicate::str::contains("update_available=false"));
    let mut mutated = fs::OpenOptions::new().append(true).open(&archive)?;
    mutated.write_all(b"x")?;
    let mut retry = isolated_command(project.path())?;
    retry
        .env("LDGR_ADAPTER_INDEX", &index)
        .env("LDGR_ADAPTER_RELEASE_KEYRING", &keyring)
        .args([
            "adapter",
            "install",
            "fixture",
            "--version",
            "1.2.3",
            "--yes",
            "--install-root",
        ])
        .arg(&install_root);
    retry
        .assert()
        .failure()
        .stderr(predicate::str::contains("SHA-256 mismatch"));
    assert!(install_root.join("adapter.toml").is_file());

    let original_manifest = fs::read(install_root.join("adapter.toml"))?;
    fs::create_dir_all(bundle.join("skills/broken"))?;
    fs::write(bundle.join("skills/broken/SKILL.md"), [0xff, 0xfe])?;
    let mut resource_manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(bundle.join("adapter-resources.json"))?)?;
    resource_manifest["resources"]
        .as_array_mut()
        .expect("resource fixture is an array")
        .push(serde_json::json!({
            "kind":"skill",
            "harnesses":["pi"],
            "source":"skills/broken",
            "destination":"collision/child"
        }));
    fs::write(
        bundle.join("adapter-resources.json"),
        resource_manifest.to_string(),
    )?;
    fs::remove_file(&archive)?;
    let status = StdCommand::new("tar")
        .args(["-czf"])
        .arg(&archive)
        .arg("-C")
        .arg(project.path())
        .arg("fixture-1.2.3")
        .status()?;
    assert!(status.success());
    let updated_archive = fs::read(&archive)?;
    fs::write(
        &signature,
        serde_json::json!({
            "algorithm": "Ed25519",
            "key_id": "test",
            "signature": STANDARD.encode(signing_key.sign(&updated_archive).to_bytes())
        })
        .to_string(),
    )?;
    let mut updated_index: serde_json::Value = serde_json::from_str(&fs::read_to_string(&index)?)?;
    updated_index["adapters"][0]["releases"][0]["platforms"][0]["sha256"] =
        serde_json::Value::String(format!("{:x}", Sha256::digest(&updated_archive)));
    fs::write(&index, updated_index.to_string())?;
    fs::write(isolated_home.join("pi-skills/collision"), "collision")?;
    let mut rollback = isolated_command(project.path())?;
    rollback
        .env("LDGR_ADAPTER_INDEX", &index)
        .env("LDGR_ADAPTER_RELEASE_KEYRING", &keyring)
        .args([
            "adapter",
            "install",
            "fixture",
            "--version",
            "1.2.3",
            "--yes",
            "--install-root",
        ])
        .arg(&install_root);
    rollback.assert().failure();
    assert_eq!(
        fs::read(install_root.join("adapter.toml"))?,
        original_manifest
    );
    assert_eq!(
        fs::read_to_string(isolated_home.join("pi-skills/collision"))?,
        "collision"
    );
    fs::OpenOptions::new()
        .append(true)
        .open(install_root.join("adapter.toml"))?
        .write_all(b"\n# user modification\n")?;
    let mut uninstall = isolated_command(project.path())?;
    uninstall
        .env("LDGR_ADAPTER_PATH", &install_root)
        .args(["adapter", "uninstall", "fixture"]);
    uninstall
        .assert()
        .failure()
        .stderr(predicate::str::contains("modified adapter-owned files"));
    assert!(install_root.exists());
    let mut forced = isolated_command(project.path())?;
    forced.env("LDGR_ADAPTER_PATH", &install_root).args([
        "adapter",
        "uninstall",
        "fixture",
        "--force",
    ]);
    forced.assert().success();
    assert!(!install_root.exists());
    assert_eq!(
        fs::read_to_string(isolated_home.join("pi-skills/collision"))?,
        "collision"
    );
    Ok(())
}

#[test]
fn top_level_help_shows_core_loop_and_hides_mature_project_surface() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let mut command = isolated_command(project.path())?;
    command.arg("--help");
    command.assert().success().stdout(
        predicate::str::contains("Core loop:")
            .and(predicate::str::contains(
                "work create <slug> --title <title> --description <description>",
            ))
            .and(predicate::str::contains("run start <work-slug>"))
            .and(predicate::str::contains("observe <run-id-or-work-slug>"))
            .and(predicate::str::contains("decision record <work-slug>"))
            .and(predicate::str::contains(
                "Default help shows the day-one workflow",
            ))
            .and(predicate::str::contains("target-profile").not())
            .and(predicate::str::contains("revalidation").not())
            .and(predicate::str::contains("failure").not())
            .and(predicate::str::contains("profile").not()),
    );
    Ok(())
}

#[test]
fn focused_subcommand_help_omits_adapter_discovery_blocks() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let mut command = isolated_command(project.path())?;
    command.args(["work", "create", "--help"]);
    command.assert().success().stdout(
        predicate::str::contains("Create a pending work item")
            .and(predicate::str::contains("Usage: ldgr work create"))
            .and(predicate::str::contains("Available adapters:").not())
            .and(predicate::str::contains("Installed adapter control surface:").not()),
    );
    Ok(())
}

#[test]
fn adapter_focused_help_keeps_adapter_discovery_blocks() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let mut command = isolated_command(project.path())?;
    command.args(["adapter", "--help"]);
    command.assert().success().stdout(
        predicate::str::contains("Discover installed adapter manifests")
            .and(predicate::str::contains("Available adapters:"))
            .and(predicate::str::contains("ldgr adapter install conduct")),
    );
    Ok(())
}

#[test]
fn adapter_install_without_name_shows_selection_fallback_not_help() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let mut command = isolated_command(project.path())?;
    command.args(["adapter", "install"]);
    command.assert().success().stdout(
        predicate::str::contains("Available adapters:")
            .and(predicate::str::contains("ldgr adapter install conduct"))
            .and(predicate::str::contains(
                "Run `ldgr adapter install <adapter>`",
            ))
            .and(predicate::str::contains("Usage: ldgr adapter install").not()),
    );
    Ok(())
}

#[test]
fn full_help_shows_core_command_tree_and_research_split() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let mut command = isolated_command(project.path())?;
    command.arg("--full");
    command.assert().success().stdout(
        predicate::str::contains("Core command tree:")
            .and(predicate::str::contains(
                "  work\n    list\n    show\n    create\n    edit\n    import\n    export\n    status\n      set\n    delete",
            ))
            .and(predicate::str::contains(
                "  notice\n    list\n    add\n    edit\n    clear",
            ))
            .and(predicate::str::contains(
                "  prompt\n    create\n    import\n    update\n    activate",
            ))
            .and(predicate::str::contains("  bundle\n    create\n    seal"))
            .and(predicate::str::contains(
                "Research/readiness commands moved to `ldgr-research`",
            ))
            .and(predicate::str::contains("  failure\n    list").not())
            .and(predicate::str::contains("    revalidation").not()),
    );
    Ok(())
}

#[test]
fn invalid_command_input_prints_last_parsable_help() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let mut command = isolated_command(project.path())?;
    command.args(["status", "update"]);
    command.assert().failure().stdout(
        predicate::str::contains("Examples:")
            .and(predicate::str::contains("ldgr status --json"))
            .and(predicate::str::contains(
                "ldgr work status set <work> <status>",
            )),
    );

    let mut command = isolated_command(project.path())?;
    command.args(["work", "bogus"]);
    command
        .assert()
        .failure()
        .stdout(predicate::str::contains("ldgr work create fix-login").and(
            predicate::str::contains("ldgr work status set fix-login held"),
        ));
    Ok(())
}

#[test]
fn web_help_documents_default_loopback_port() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let mut command = isolated_command(project.path())?;
    command.args(["web", "--help"]);
    command.assert().success().stdout(
        predicate::str::contains("--host <HOST>")
            .and(predicate::str::contains("[default: 127.0.0.1]"))
            .and(predicate::str::contains("--port <PORT>"))
            .and(predicate::str::contains("[default: 8686]")),
    );
    Ok(())
}

#[test]
fn adapter_list_show_and_dispatch_use_core_registry_metadata() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let adapter_root = project.path().join("adapters");
    write_adapter_fixture(&adapter_root.join("sample"), "community-sample", "sample")?;
    fs::create_dir_all(adapter_root.join("broken"))?;
    fs::write(adapter_root.join("broken/adapter.toml"), "[adapter\n")?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["adapter", "list"],
    )?
    .env("LDGR_ADAPTER_PATH", &adapter_root)
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "adapter=community-sample title=community-sample title",
    ))
    .stdout(predicate::str::contains("command=community-sample-check"))
    .stderr(predicate::str::contains(
        "warning: skipped adapter manifest",
    ))
    .stderr(predicate::str::contains("failed to parse adapter manifest"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["adapter", "show", "sample"],
    )?
    .env("LDGR_ADAPTER_PATH", &adapter_root)
    .assert()
    .success()
    .stdout(predicate::str::contains("adapter: community-sample"))
    .stdout(predicate::str::contains("aliases: sample"))
    .stdout(predicate::str::contains(
        "loop_prompt_path: prompts/loop.md",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["adapter", "dispatch", "community-sample-check"],
    )?
    .env("LDGR_ADAPTER_PATH", &adapter_root)
    .assert()
    .success()
    .stdout(predicate::str::contains("command: community-sample-check"))
    .stdout(predicate::str::contains("argv: community-sample check"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["community-sample-check"],
    )?
    .env("LDGR_ADAPTER_PATH", &adapter_root)
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "Adapter command `community-sample-check` is installed but is not a core command.",
    ))
    .stderr(predicate::str::contains(
        "Inspect it with `ldgr adapter dispatch community-sample-check`.",
    ));

    Ok(())
}

#[test]
fn adapter_namespace_dispatch_preserves_argv_and_exports_context() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let adapter_root = project.path().join("adapters");
    let record_dir = project.path().join("records");
    fs::create_dir_all(&record_dir)?;
    write_adapter_namespace_fixture(
        &adapter_root.join("example"),
        "example",
        "reference",
        r#"["sh", "-c", '''printf "%s\n" "$@" > "$ADAPTER_RECORD_DIR/argv.txt"; env | grep "^LDGR_" | sort > "$ADAPTER_RECORD_DIR/env.txt"; echo adapter-stdout; echo adapter-stderr >&2''', "adapter-fixture"]"#,
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["reference", "--flag", "two words", "tail"],
    )?
    .env("LDGR_ADAPTER_PATH", &adapter_root)
    .env("ADAPTER_RECORD_DIR", &record_dir)
    .assert()
    .success()
    .stdout(predicate::str::contains("adapter-stdout"))
    .stderr(predicate::str::contains("adapter-stderr"));

    assert_eq!(
        fs::read_to_string(record_dir.join("argv.txt"))?,
        "--flag\ntwo words\ntail\n"
    );
    let env = fs::read_to_string(record_dir.join("env.txt"))?;
    assert!(
        env.contains(&format!("LDGR_DB={}", db_path.display())),
        "{env}"
    );
    assert!(
        env.contains(&format!("LDGR_ARTIFACT_ROOT={}", artifact_root.display())),
        "{env}"
    );
    assert!(
        env.contains(&format!("LDGR_WORKING_DIR={}", project.path().display())),
        "{env}"
    );
    assert!(env.contains("LDGR_ADAPTER_SLUG=example"), "{env}");
    assert!(env.contains("LDGR_ADAPTER_NAMESPACE=reference"), "{env}");

    Ok(())
}

#[test]
fn adapter_namespace_dispatch_propagates_nonzero_exit_status_and_stderr() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let adapter_root = project.path().join("adapters");
    write_adapter_namespace_fixture(
        &adapter_root.join("failer"),
        "failer",
        "failer",
        r#"["sh", "-c", '''echo failing-stdout; echo failing-stderr >&2; exit 7''']"#,
    )?;

    command(project.path(), &db_path, &artifact_root, ["failer"])?
        .env("LDGR_ADAPTER_PATH", &adapter_root)
        .assert()
        .code(7)
        .stdout(predicate::str::contains("failing-stdout"))
        .stderr(predicate::str::contains("failing-stderr"));

    Ok(())
}

#[test]
fn adapter_namespace_dispatch_reports_failure_to_execute() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let adapter_root = project.path().join("adapters");
    write_adapter_namespace_fixture(
        &adapter_root.join("missing"),
        "missing",
        "missing",
        r#"["ldgr-definitely-missing-adapter-command-for-test"]"#,
    )?;

    command(project.path(), &db_path, &artifact_root, ["missing"])?
        .env("LDGR_ADAPTER_PATH", &adapter_root)
        .assert()
        .failure()
        .stderr(
            predicate::str::contains("failed to execute adapter `missing` namespace `missing`")
                .and(predicate::str::contains(
                    "ldgr-definitely-missing-adapter-command-for-test",
                )),
        );

    Ok(())
}

#[test]
fn core_builtin_commands_take_precedence_over_adapter_namespaces() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let adapter_root = project.path().join("adapters");
    let marker = project.path().join("adapter-ran.txt");
    run(project.path(), &db_path, &artifact_root, ["init"])?;
    write_adapter_namespace_fixture(
        &adapter_root.join("status"),
        "status-adapter",
        "status",
        r#"["sh", "-c", "touch adapter-ran.txt"]"#,
    )?;

    command(project.path(), &db_path, &artifact_root, ["status"])?
        .env("LDGR_ADAPTER_PATH", &adapter_root)
        .assert()
        .success()
        .stdout(predicate::str::contains("LDGR brief context"));

    assert!(
        !marker.exists(),
        "built-in status should not dispatch adapter"
    );

    Ok(())
}

#[test]
fn status_and_next_commands_include_conduct_adapter_suggestions() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let adapter_root = project.path().join("adapters");
    write_adapter_fixture(&adapter_root.join("conduct"), "conduct", "cd")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "conduct-next",
            "--title",
            "Conduct next",
            "--description",
            "Inspect conduct batch_id: batch-042 before launching another wave.",
        ],
    )?;

    command(project.path(), &db_path, &artifact_root, ["--help"])?
        .env("LDGR_ADAPTER_PATH", &adapter_root)
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "Installed adapter control surface:",
        ))
        .stdout(predicate::str::contains("ldgr conduct <args...>"));

    command(project.path(), &db_path, &artifact_root, ["status"])?
        .env("LDGR_ADAPTER_PATH", &adapter_root)
        .assert()
        .success()
        .stdout(predicate::str::contains("installed_adapter_namespaces:"))
        .stdout(predicate::str::contains(
            "adapter=conduct namespace=conduct command=ldgr conduct",
        ))
        .stdout(predicate::str::contains("next_commands:"))
        .stdout(predicate::str::contains("ldgr conduct --help"))
        .stdout(predicate::str::contains(
            "ldgr run start conduct-next --command <what-ran>",
        ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["status", "--json"],
    )?
    .env("LDGR_ADAPTER_PATH", &adapter_root)
    .assert()
    .success()
    .stdout(predicate::str::contains(
        r#""installed_adapter_namespaces""#,
    ))
    .stdout(predicate::str::contains(r#""namespace": "conduct""#))
    .stdout(predicate::str::contains("batch launch").not());

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["next", "--commands"],
    )?
    .env("LDGR_ADAPTER_PATH", &adapter_root)
    .assert()
    .success()
    .stdout(predicate::str::contains("ldgr conduct --help"))
    .stdout(predicate::str::contains(
        "ldgr run start conduct-next --command <what-ran>",
    ));

    command(project.path(), &db_path, &artifact_root, ["next"])?
        .env("LDGR_ADAPTER_PATH", &adapter_root)
        .assert()
        .success()
        .stdout(predicate::str::contains("conduct-next Conduct next"))
        .stdout(predicate::str::contains("ldgr conduct").not());

    Ok(())
}

#[test]
#[ignore = "obsolete Core-owned Conduct lifecycle projection removed by ADP-020"]
fn status_surfaces_referenced_conduct_batch_lifecycle() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "setup",
            "--title",
            "Setup",
            "--description",
            "Record conduct fixture artifacts.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "setup"],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "conduct-lifecycle",
            "--title",
            "Conduct lifecycle",
            "--description",
            "Launch the next conduct wave for batch_id: batch-042.",
        ],
    )?;

    let state_path = project.path().join("batch-state.md");
    fs::write(
        &state_path,
        r#"---
ldgr_doc: 1
kind: batch_state
id: batch-042
schema: ldgr.batch_state.v1
status: accepted
---

# Batch State: batch-042

```ldgr-batch-state yaml
batch_id: batch-042
graph_artifact_id: artifact:41
ticket_index_artifact_id: artifact:42
status: wave_complete
current_wave: wave-001
waves:
  - wave_id: wave-001
    node_ids:
      - ticket.alpha
    worker_ids:
      - worker-001
    status: wave_complete
workers:
  - worker_id: worker-001
    ticket_id: ticket.alpha
    work_item_id: work:alpha
    worktree_path: path:.ldgr/.conduct/worktrees/batch-042/worker-001-alpha
    worker_db_path: db:.ldgr/.conduct/workers/batch-042/worker-001/ldgr.db
    status: complete
blocked:
  - ticket_id: ticket.beta
    reason: waiting for dependencies ticket.alpha
```
"#,
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "artifact",
            "add",
            "1",
            "--kind",
            "report",
            "--path",
            state_path.to_str().expect("artifact path is UTF-8"),
            "--description",
            "LDGR batch_state artifact batch_id=batch-042.",
        ],
    )?;

    command(project.path(), &db_path, &artifact_root, ["status"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "conduct_lifecycle: batch_id=batch-042 status=wave_complete",
        ))
        .stdout(predicate::str::contains(
            "workers=total:1 complete:1 active:0 blocked:0 terminal:1",
        ))
        .stdout(predicate::str::contains(
            "conduct_artifacts: graph=41 ticket_index=42 batch_state=1",
        ))
        .stdout(predicate::str::contains(
            "next_valid_action=ldgr conduct batch launch --batch-id batch-042 --graph <graph.md>",
        ))
        .stdout(predicate::str::contains(
            "conduct_warning: next work conduct-lifecycle references conduct batch batch-042",
        ))
        .stdout(predicate::str::contains(
            "ldgr work status set conduct-lifecycle done --reason \"stale conduct work; batch batch-042 is wave_complete\"",
        ))
        .stdout(predicate::str::contains(
            "ldgr work status set conduct-lifecycle held --reason \"stale conduct work; batch batch-042 is wave_complete\"",
        ))
        .stdout(predicate::str::contains(
            "ldgr conduct batch launch --batch-id batch-042 --graph <graph.md>",
        ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["context", "--json"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(r#""conduct_lifecycle""#))
    .stdout(predicate::str::contains(r#""batch_id": "batch-042""#))
    .stdout(predicate::str::contains(r#""status": "wave_complete""#))
    .stdout(predicate::str::contains(r#""graph_artifact_id": 41"#))
    .stdout(predicate::str::contains(
        r#""ticket_index_artifact_id": 42"#,
    ))
    .stdout(predicate::str::contains(r#""blocked_count": 1"#))
    .stdout(predicate::str::contains(r#""stale_next_work""#))
    .stdout(predicate::str::contains(r#""work_slug": "conduct-lifecycle""#))
    .stdout(predicate::str::contains(
        r#""ldgr work status set conduct-lifecycle done --reason \"stale conduct work; batch batch-042 is wave_complete\"""#,
    ));

    Ok(())
}

#[test]
fn init_prints_setup_prompt_and_command_workflow() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    command(project.path(), &db_path, &artifact_root, ["init"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("initialized "))
        .stdout(predicate::str::contains("# LDGR Init Setup Prompt"))
        .stdout(predicate::str::contains(
            "Initialize LDGR around the smallest useful loop",
        ))
        .stdout(predicate::str::contains("Current directory:"))
        .stdout(predicate::str::contains(
            project.path().display().to_string(),
        ))
        .stdout(predicate::str::contains(
            "Repository outline from `dev walk . --stdout --no-content`",
        ))
        .stdout(predicate::str::contains(
            "one work item, one run, observations/artifacts from that run",
        ))
        .stdout(predicate::str::contains("Keep setup core-only"))
        .stdout(predicate::str::contains(
            "Do not introduce adapter or research-layer records during core setup",
        ))
        .stdout(predicate::str::contains("Core loop:"))
        .stdout(predicate::str::contains(
            "work create <slug> --title <title> --description <description>",
        ))
        .stdout(predicate::str::contains(
            "Use `ldgr <command> --help` for flags, or `ldgr --full` for the core command map.",
        ))
        .stdout(predicate::str::contains("Core command tree:").not());

    command(project.path(), &db_path, &artifact_root, ["init"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("opened existing "))
        .stdout(predicate::str::contains("no data erased"));

    Ok(())
}

#[test]
fn context_exits_cleanly_when_stdout_pipe_is_closed() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;

    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("ldgr"))
        .current_dir(project.path())
        .arg("--db")
        .arg(&db_path)
        .arg("--artifact-root")
        .arg(&artifact_root)
        .arg("context")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    drop(child.stdout.take());

    let output = child.wait_with_output()?;
    assert!(output.status.success(), "{output:?}");
    assert_eq!("", String::from_utf8_lossy(&output.stderr));

    Ok(())
}

#[test]
fn run_close_finishes_run_and_records_decision_from_run_id() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "close-me",
            "--title",
            "Close me",
            "--description",
            "Close this run from its id.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "close-me"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "1",
            "--status",
            "success",
            "--outcome",
            "continue",
            "--rationale",
            "Missing next work is invalid.",
        ],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "continuing requires a next work item",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "show", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"))
    .stdout(predicate::str::contains("finished_at").not())
    .stdout(predicate::str::contains("notes:").not());
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "close-me"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "1",
            "--status",
            "success",
            "--outcome",
            "continue",
            "--rationale",
            "Missing next details are invalid.",
            "--next-slug",
            "partial-next",
        ],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "supply --next-title and --next-description",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "show", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"))
    .stdout(predicate::str::contains("finished_at").not())
    .stdout(predicate::str::contains("notes:").not());
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "close-me"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "1",
            "--status",
            "success",
            "--outcome",
            "stop",
            "--rationale",
            "The close command finished the lifecycle.",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "closed run 1 [success] and recorded decision 1 [stop] for close-me",
    ));

    command(project.path(), &db_path, &artifact_root, ["context"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("done=1"))
        .stdout(predicate::str::contains(
            "latest_decision: id=1 work=close-me outcome=stop",
        ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "1",
            "--status",
            "success",
            "--outcome",
            "continue",
            "--rationale",
            "Missing next details are invalid.",
            "--next-slug",
            "partial-next",
        ],
    )?
    .assert()
    .failure();

    Ok(())
}

#[test]
fn observe_alias_and_slug_run_references_work_for_run_evidence() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let report_path = project.path().join("report.txt");
    fs::write(&report_path, "slug artifact evidence\n")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "slug-ref",
            "--title",
            "Slug ref",
            "--description",
            "Use work slugs where agents used to need numeric run IDs.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "slug-ref"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "observe",
            "slug-ref",
            "--body",
            "direct observe alias resolved slug-ref",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("added observation 1"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "observe",
            "add",
            "slug-ref",
            "--body",
            "subcommand observe alias resolved slug-ref",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("added observation 2"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["observation", "list", "--run-id", "slug-ref"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "subcommand observe alias resolved slug-ref",
    ))
    .stdout(predicate::str::contains(
        "direct observe alias resolved slug-ref",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "validation",
            "record",
            "slug-ref",
            "--outcome",
            "pass",
            "--command",
            "cargo test slug-ref",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("command: cargo test slug-ref"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "artifact",
            "add",
            "slug-ref",
            "--path",
            report_path.to_str().context("report path is UTF-8")?,
            "--description",
            "slug-ref artifact",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("added artifact 1"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "slug-ref",
            "--status",
            "success",
            "--outcome",
            "stop",
            "--rationale",
            "Slug-based run references are supported.",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "closed run 1 [success] and recorded decision 1 [stop] for slug-ref",
    ));

    Ok(())
}

#[test]
fn validation_skip_requires_rationale_and_stays_distinct_from_pass() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "validation-check",
            "--title",
            "Validation check",
            "--description",
            "Exercise validation skip records.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "validation-check"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["validation", "record", "1", "--outcome", "skipped"],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "skipped validation requires --rationale",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "validation",
            "record",
            "1",
            "--outcome",
            "skipped",
            "--rationale",
            "No TypeScript package files changed.",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("outcome: skipped"))
    .stdout(predicate::str::contains(
        "rationale: No TypeScript package files changed.",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "finish", "1", "--status", "success"],
    )?
    .assert()
    .success();

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["validation", "list", "--run-id", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("[skipped]"))
    .stdout(predicate::str::contains("[pass]").not());

    command(project.path(), &db_path, &artifact_root, ["status"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("latest_validations:"))
        .stdout(predicate::str::contains("outcome=skipped"))
        .stdout(predicate::str::contains(
            "rationale: No TypeScript package files changed.",
        ));

    let output = command(
        project.path(),
        &db_path,
        &artifact_root,
        ["context", "--json"],
    )?
    .output()?;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout)?;
    let context: serde_json::Value = serde_json::from_str(&stdout)?;
    let validation = &context["latest_validations"][0];
    assert_eq!(validation["outcome"], "skipped");
    assert_eq!(
        validation["rationale"],
        "No TypeScript package files changed."
    );

    Ok(())
}

#[test]
fn run_close_can_link_existing_next_work_from_cli() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "close-current",
            "--title",
            "Close current",
            "--description",
            "Close this run and link queued work.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "queued-next",
            "--title",
            "Queued next",
            "--description",
            "Already queued follow-up work.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "close-current"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "1",
            "--status",
            "success",
            "--outcome",
            "continue",
            "--rationale",
            "Continue with queued work.",
            "--next-slug",
            "queued-next",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "closed run 1 [success] and recorded decision 1 [continue] for close-current",
    ));

    command(project.path(), &db_path, &artifact_root, ["context"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("done=1"))
        .stdout(predicate::str::contains("pending=1"))
        .stdout(predicate::str::contains(
            "latest_decision_next: queued-next",
        ));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "queued-next"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: pending"));

    Ok(())
}

#[test]
fn run_close_rejects_invalid_next_work_before_finishing_run() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "close-current",
            "--title",
            "Close current",
            "--description",
            "Invalid next work must not finish this run.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "finished-next",
            "--title",
            "Finished next",
            "--description",
            "Terminal work cannot be selected as next work.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "status", "set", "finished-next", "done"],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "close-current"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "1",
            "--status",
            "success",
            "--notes",
            "should not persist",
            "--outcome",
            "continue",
            "--rationale",
            "Try to continue with terminal work.",
            "--next-slug",
            "finished-next",
        ],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "work item finished-next already exists but is done",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "show", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"))
    .stdout(predicate::str::contains("finished_at").not())
    .stdout(predicate::str::contains("notes:").not());
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "close-current"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"));
    command(project.path(), &db_path, &artifact_root, ["context"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("active_runs:"))
        .stdout(predicate::str::contains("latest_decision: none"));

    Ok(())
}

#[test]
fn standalone_decision_refuses_to_close_work_with_active_run() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "active-decision",
            "--title",
            "Active decision",
            "--description",
            "Decision recording must not strand active runs.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "active-decision"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "decision",
            "record",
            "active-decision",
            "--outcome",
            "stop",
            "--rationale",
            "This should use run close instead.",
        ],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains("use `ldgr run close 1`"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "active-decision"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "show", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"));

    Ok(())
}

#[test]
fn direct_done_status_refuses_to_close_work_with_active_run() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "active-status",
            "--title",
            "Active status",
            "--description",
            "Status changes must not strand active runs.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "active-status"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "status", "set", "active-status", "done"],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains("use `ldgr run close 1`"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "active-status"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: running"));

    Ok(())
}

#[test]
fn context_brief_prints_agent_on_ramp_without_full_cockpit_noise() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "brief-next",
            "--title",
            "Brief next",
            "--description",
            "Use the compact context output.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "brief-next", "--command", "brief smoke"],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "observation",
            "add",
            "1",
            "--body",
            "first line\nsecond line with enough extra detail to prove whitespace is normalized",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["context", "--brief"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("LDGR brief context"))
    .stdout(predicate::str::contains(
        "loop: phase=started run=1 work=brief-next",
    ))
    .stdout(predicate::str::contains(
        "- run=1 work=brief-next title=Brief next",
    ))
    .stdout(predicate::str::contains("skills:").not())
    .stdout(predicate::str::contains(
        "handoff: active_run=true next_work=false needs_decision=true",
    ))
    .stdout(predicate::str::contains(
        "signoff: complete the active run with `ldgr run close ...` before signing off",
    ))
    .stdout(predicate::str::contains("next_commands:"))
    .stdout(predicate::str::contains(
        "ldgr observe brief-next --body <evidence>",
    ))
    .stdout(predicate::str::contains(
        "ldgr run close brief-next --status <success|partial|failed> --outcome stop --rationale <why>",
    ))
    .stdout(predicate::str::contains(
        "ldgr run close brief-next --status <success|partial|failed> --outcome continue --rationale <why> --next-slug <slug> --next-title <title> --next-description <description>",
    ))
    .stdout(predicate::str::contains("--outcome <continue|stop>").not())
    .stdout(predicate::str::contains("brief_context: ldgr status"))
    .stdout(predicate::str::contains("full_context: ldgr context"))
    .stdout(predicate::str::contains(
        "first line second line with enough extra detail",
    ))
    .stdout(predicate::str::contains("latest_events:").not());

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["context", "--brief", "--json"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(r#""loop_state""#))
    .stdout(predicate::str::contains(r#""work": "brief-next""#))
    .stdout(predicate::str::contains(r#""next_commands""#))
    .stdout(predicate::str::contains(
        r#""ldgr run close brief-next --status <success|partial|failed> --outcome continue --rationale <why> --next-slug <slug> --next-title <title> --next-description <description>""#,
    ))
    .stdout(predicate::str::contains(
        r#""brief_context_command": "ldgr status""#,
    ))
    .stdout(predicate::str::contains(r#""latest_events""#).not());

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["context", "--brief", "--recent", "0", "--width", "40"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("latest_observations: none"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["status", "--json"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(r#""brief_context_command""#))
    .stdout(predicate::str::contains(r#""latest_events""#).not());

    Ok(())
}

#[test]
fn active_run_with_queued_next_work_prints_explicit_continue_next_slug() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "active-work",
            "--title",
            "Active work",
            "--description",
            "Close this work with a queued next item.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "queued-work",
            "--title",
            "Queued work",
            "--description",
            "Already queued follow-up work.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "active-work"],
    )?;

    command(project.path(), &db_path, &artifact_root, ["status"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "handoff: active_run=true next_work=true needs_decision=true",
        ))
        .stdout(predicate::str::contains(
            "ldgr observe active-work --body <evidence>",
        ))
        .stdout(predicate::str::contains(
            "ldgr run close active-work --status <success|partial|failed> --outcome continue --rationale <why> --next-slug queued-work",
        ))
        .stdout(predicate::str::contains(
            "ldgr run close active-work --status <success|partial|failed> --outcome stop --rationale <why>",
        ))
        .stdout(predicate::str::contains("--outcome <continue|stop>").not());

    Ok(())
}

#[test]
fn context_json_is_core_shaped_and_bounded() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    fs::create_dir_all(&artifact_root)?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;

    for index in 0..8 {
        let slug = format!("context-bound-{index}");
        let title = format!("Context bound {index}");
        let run_id = (index + 1).to_string();
        run(
            project.path(),
            &db_path,
            &artifact_root,
            [
                "work",
                "create",
                slug.as_str(),
                "--title",
                title.as_str(),
                "--description",
                "Exercise bounded context JSON.",
            ],
        )?;
        run(
            project.path(),
            &db_path,
            &artifact_root,
            ["run", "start", slug.as_str()],
        )?;
        run(
            project.path(),
            &db_path,
            &artifact_root,
            [
                "observation",
                "add",
                run_id.as_str(),
                "--body",
                "Bounded context observation.",
            ],
        )?;
    }

    for index in 0..5 {
        let artifact_path = artifact_root.join(format!("bounded-{index}.md"));
        fs::write(&artifact_path, format!("# Bounded {index}\n"))?;
        run(
            project.path(),
            &db_path,
            &artifact_root,
            [
                "artifact",
                "add",
                "1",
                "--kind",
                "report",
                "--path",
                artifact_path.to_str().expect("artifact path is UTF-8"),
                "--description",
                "Bounded context artifact.",
            ],
        )?;
    }

    let output = command(
        project.path(),
        &db_path,
        &artifact_root,
        ["context", "--json"],
    )?
    .output()?;
    assert!(output.status.success(), "{output:?}");
    let stdout = String::from_utf8(output.stdout)?;
    let context: serde_json::Value = serde_json::from_str(&stdout)?;

    assert_eq!(context["running_work_items"], 8);
    assert_eq!(context["active_runs"].as_array().unwrap().len(), 5);
    assert_eq!(context["latest_observations"].as_array().unwrap().len(), 3);
    assert_eq!(context["latest_artifacts"].as_array().unwrap().len(), 3);
    assert!(
        context["loop_state"]["recent_cycle_narrative"]
            .as_array()
            .unwrap()
            .len()
            <= 6
    );
    assert!(context["latest_events"].as_array().unwrap().len() <= 10);
    assert!(context.get("loop_state").is_some());
    assert!(context.get("next_work_item").is_some());
    assert!(context.get("adapter_context").is_none());
    assert!(context.get("due_fact_revalidation_policies").is_none());
    assert!(context.get("open_issues").is_none());
    assert!(context.get("resolved_issues").is_none());
    assert!(context.get("latest_issues").is_none());
    assert!(context.get("latest_expectations").is_none());
    assert!(context.get("latest_validation_results").is_none());
    assert!(context.get("latest_failures").is_none());
    assert!(context.get("latest_target_profiles").is_none());
    assert!(context.get("tools").is_none());

    Ok(())
}

#[test]
fn artifacts_can_be_recorded_outside_artifact_root() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let outside_artifact = project.path().join("external-report.md");
    fs::write(&outside_artifact, "# External report\n")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "outside-artifact",
            "--title",
            "Outside artifact",
            "--description",
            "Record an artifact outside the managed artifact directory.",
        ],
    )?;
    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "edit",
            "outside-artifact",
            "--description",
            "Record and inspect an artifact outside the managed artifact directory.",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "edited work item outside-artifact",
    ));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "outside-artifact"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "Record and inspect an artifact outside the managed artifact directory.",
    ));
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "outside-artifact"],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "artifact",
            "add",
            "1",
            "--kind",
            "report",
            "--path",
            outside_artifact.to_str().expect("artifact path is UTF-8"),
            "--description",
            "External report outside artifact root.",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("added artifact 1"))
    .stdout(predicate::str::contains("external-report.md"));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["artifact", "list"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("submitted/").or(predicate::str::contains("submitted\\")))
    .stdout(predicate::str::contains("external-report.md"));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["artifact", "show", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("Artifact: 1"))
    .stdout(predicate::str::contains("kind: report"))
    .stdout(predicate::str::contains(
        "External report outside artifact root.",
    ));
    let submitted_files = fs::read_dir(artifact_root.join("submitted"))?.count();
    assert_eq!(submitted_files, 1);

    Ok(())
}

#[test]
fn prompt_and_bundle_commands_match_documented_loop_surface() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let surface_path = project.path().join("surface-v2.md");
    let implementation_path = project.path().join("implementation.md");
    fs::write(
        &surface_path,
        "surface prompt v2 {{ldgr_context}} {{ldgr_status}}",
    )?;
    fs::write(
        &implementation_path,
        "implementation prompt {{ldgr_context}}",
    )?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "prompt",
            "create",
            "surface",
            "--role",
            "surface-loop",
            "--body",
            "surface prompt v1 {{ldgr_context}}",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "created prompt surface version=1 status=draft",
    ));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "prompt",
            "update",
            "surface",
            "--path",
            surface_path.to_str().expect("prompt path is UTF-8"),
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "updated prompt surface version=2 status=draft",
    ));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["prompt", "activate", "surface"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "activated prompt surface version=2 status=active",
    ));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "prompt",
            "import",
            "implementation",
            "--role",
            "implementation-loop",
            "--path",
            implementation_path.to_str().expect("prompt path is UTF-8"),
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "imported prompt implementation version=1 status=draft",
    ));
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["prompt", "activate", "implementation"],
    )?;
    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "bundle",
            "create",
            "cleanroom",
            "--prompt",
            "surface",
            "--prompt",
            "implementation",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "created bundle cleanroom status=draft",
    ));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["bundle", "seal", "cleanroom"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "sealed bundle cleanroom status=sealed hash=fnv1a64:",
    ));
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "bundle-loop",
            "--title",
            "Bundle loop",
            "--description",
            "Verify bundle-backed prompt rendering.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--bundle",
            "cleanroom",
            "--prompt-role",
            "surface-loop",
            "--dry-run",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=1 work=bundle-loop"));

    let rendered_prompt = fs::read_to_string(artifact_root.join("loop-run-1-prompt.md"))?;
    assert!(
        rendered_prompt.contains("surface prompt v2"),
        "{rendered_prompt}"
    );
    assert!(
        !rendered_prompt.contains("surface prompt v1"),
        "{rendered_prompt}"
    );
    assert!(
        rendered_prompt.contains(r#""work_slug": "bundle-loop""#),
        "{rendered_prompt}"
    );
    let provenance = fs::read_to_string(artifact_root.join("loop-run-1-prompt-provenance.json"))?;
    assert!(
        provenance.contains(r#""source_type": "bundle""#),
        "{provenance}"
    );
    assert!(
        provenance.contains(r#""bundle_slug": "cleanroom""#),
        "{provenance}"
    );
    assert!(
        provenance.contains(r#""prompt_role": "surface-loop""#),
        "{provenance}"
    );
    assert!(
        provenance.contains(r#""prompt_version": 2"#),
        "{provenance}"
    );

    Ok(())
}

#[test]
fn autonomous_loop_runtime_renders_prompt_and_records_dry_run_session() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("prompts/loop-prompt.md"),
        &prompt_path,
    )?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "loop-check",
            "--title",
            "Loop check",
            "--description",
            "Verify the prompt-driven loop runtime.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--dry-run",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=1 work=loop-check"))
    .stdout(predicate::str::contains("agent_exit_code: unknown"));

    let rendered_prompt = fs::read_to_string(artifact_root.join("loop-run-1-prompt.md"))?;
    assert!(
        rendered_prompt.contains("Job completion policy"),
        "{rendered_prompt}"
    );
    assert!(
        rendered_prompt.contains(r#""work_slug": "loop-check""#),
        "{rendered_prompt}"
    );
    assert!(
        rendered_prompt.contains("## Ldgr status"),
        "{rendered_prompt}"
    );
    assert!(
        rendered_prompt.contains(r#""brief_context_command": "ldgr status""#),
        "{rendered_prompt}"
    );
    assert!(
        rendered_prompt.contains("ldgr work edit"),
        "{rendered_prompt}"
    );
    assert!(
        rendered_prompt.contains("ldgr work status set"),
        "{rendered_prompt}"
    );
    assert!(
        rendered_prompt.contains("ldgr artifact show"),
        "{rendered_prompt}"
    );
    command(project.path(), &db_path, &artifact_root, ["context"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "work_items: pending=1 running=0 held=0",
        ))
        .stdout(predicate::str::contains(
            "loop_state: phase=dry_run_restored_work run=1",
        ))
        .stdout(predicate::str::contains(
            "loop_progress: Dry-run completed without consuming loop-check",
        ))
        .stdout(predicate::str::contains("loop_narrative:"))
        .stdout(predicate::str::contains("phase=running_agent"))
        .stdout(predicate::str::contains("latest_artifacts:"))
        .stdout(predicate::str::contains("loop-run-1-prompt.md"))
        .stdout(predicate::str::contains(
            "Autonomous loop runtime rendered prompt",
        ));

    Ok(())
}

#[test]
fn autonomous_loop_runtime_surfaces_terminal_run_without_decision() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::write(&prompt_path, "{{ldgr_context}}")?;
    let agent_path = project.path().join("agent-does-not-close.sh");
    fs::write(
        &agent_path,
        "#!/bin/sh\ncat >/dev/null\nprintf 'agent exited without decision\\n'\n",
    )?;
    let agent_argv = serde_json::to_string(&vec!["sh", agent_path.to_str().unwrap()])?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "missing-decision-loop",
            "--title",
            "Missing decision loop",
            "--description",
            "Agent exits without closing the work decision.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "other-pending-loop",
            "--title",
            "Other pending loop",
            "--description",
            "Must not be claimed while the prior work still needs a decision.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--agent-argv",
            &agent_argv,
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "loop run=1 work=missing-decision-loop",
    ));

    command(project.path(), &db_path, &artifact_root, ["status"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "loop: phase=needs_decision run=1 work=missing-decision-loop status=success",
        ))
        .stdout(predicate::str::contains(
            "handoff: active_run=false next_work=true needs_decision=true",
        ))
        .stdout(predicate::str::contains(
            "ldgr decision record missing-decision-loop --outcome <continue|stop> --rationale <why>",
        ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--dry-run",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "Loop is blocked by unfinished work item missing-decision-loop",
    ));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "decision",
            "record",
            "missing-decision-loop",
            "--outcome",
            "continue",
            "--rationale",
            "Agent output captured; continue to queued work.",
            "--next-slug",
            "other-pending-loop",
        ],
    )?
    .assert()
    .success();

    command(project.path(), &db_path, &artifact_root, ["work", "list"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "missing-decision-loop [done] Missing decision loop",
        ))
        .stdout(predicate::str::contains(
            "other-pending-loop [pending] Other pending loop",
        ));

    Ok(())
}

#[test]
fn autonomous_loop_runtime_allows_agent_to_finish_run_before_parent_capture() -> anyhow::Result<()>
{
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::write(&prompt_path, "{{ldgr_context}}")?;
    let agent_path = project.path().join("agent-finishes-run.sh");
    fs::write(
        &agent_path,
        "#!/bin/sh\ncat >/dev/null\nLDGR_BIN=\"$1\"\nDB=\"$2\"\nARTIFACTS=\"$3\"\n\"$LDGR_BIN\" --db \"$DB\" --artifact-root \"$ARTIFACTS\" observation add 1 --body 'agent is finishing this run before parent capture'\n\"$LDGR_BIN\" --db \"$DB\" --artifact-root \"$ARTIFACTS\" run finish 1 --status success --notes 'agent finished run before parent capture'\n\"$LDGR_BIN\" --db \"$DB\" --artifact-root \"$ARTIFACTS\" decision record self-finish-loop --outcome stop --rationale 'agent finished and closed the work'\nprintf 'agent-finished-run\\n'\n",
    )?;
    let ldgr_bin = assert_cmd::cargo::cargo_bin("ldgr");
    let agent_argv = serde_json::to_string(&vec![
        "sh".to_string(),
        agent_path.to_str().unwrap().to_string(),
        ldgr_bin.to_str().unwrap().to_string(),
        db_path.to_str().unwrap().to_string(),
        artifact_root.to_str().unwrap().to_string(),
    ])?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "self-finish-loop",
            "--title",
            "Self finish loop",
            "--description",
            "Agent finishes the run before the parent runtime captures output.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--agent-argv",
            &agent_argv,
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=1 work=self-finish-loop"))
    .stdout(predicate::str::contains("agent_exit_code: 0"));

    command(project.path(), &db_path, &artifact_root, ["context"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("work_items: pending=0 running=0"))
        .stdout(predicate::str::contains(
            "loop_state: phase=completed run=1 work=self-finish-loop status=success",
        ))
        .stdout(predicate::str::contains("latest_decision:"))
        .stdout(predicate::str::contains(
            "agent finished and closed the work",
        ))
        .stdout(predicate::str::contains("loop-run-1-agent-output.md"))
        .stdout(predicate::str::contains("phase=failed").not());

    Ok(())
}

#[test]
fn autonomous_loop_runtime_streams_agent_output_only_when_requested() -> anyhow::Result<()> {
    fn setup_project() -> anyhow::Result<(TempDir, std::path::PathBuf, std::path::PathBuf, String)>
    {
        let project = TempDir::new()?;
        let db_path = project.path().join(".ldgr/ldgr.db");
        let artifact_root = project.path().join(".ldgr/artifacts");
        let prompt_path = project.path().join("loop-prompt.md");
        fs::write(&prompt_path, "{{ldgr_context}}")?;
        let agent_path = project.path().join("agent.sh");
        fs::write(
            &agent_path,
            "#!/bin/sh\ncat >/dev/null\nprintf 'LDGR_STREAM_STDOUT\\n'\nprintf 'LDGR_STREAM_STDERR\\n' >&2\n",
        )?;
        let agent_argv = serde_json::to_string(&vec!["sh", agent_path.to_str().unwrap()])?;

        run(project.path(), &db_path, &artifact_root, ["init"])?;
        run(
            project.path(),
            &db_path,
            &artifact_root,
            [
                "work",
                "create",
                "stream-check",
                "--title",
                "Stream check",
                "--description",
                "Verify optional live loop output streaming.",
            ],
        )?;
        Ok((project, db_path, artifact_root, agent_argv))
    }

    let (quiet_project, quiet_db, quiet_artifacts, quiet_agent_argv) = setup_project()?;
    command(
        quiet_project.path(),
        &quiet_db,
        &quiet_artifacts,
        [
            "loop",
            "run",
            "--prompt",
            "loop-prompt.md",
            "--agent-argv",
            quiet_agent_argv.as_str(),
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=1 work=stream-check"))
    .stdout(predicate::str::contains("LDGR_STREAM_STDOUT").not())
    .stderr(predicate::str::contains("LDGR_STREAM_STDERR").not());
    let quiet_output = fs::read_to_string(quiet_artifacts.join("loop-run-1-agent-output.md"))?;
    assert!(
        quiet_output.contains("LDGR_STREAM_STDOUT"),
        "{quiet_output}"
    );
    assert!(
        quiet_output.contains("LDGR_STREAM_STDERR"),
        "{quiet_output}"
    );

    let (stream_project, stream_db, stream_artifacts, stream_agent_argv) = setup_project()?;
    command(
        stream_project.path(),
        &stream_db,
        &stream_artifacts,
        [
            "loop",
            "run",
            "--prompt",
            "loop-prompt.md",
            "--agent-argv",
            stream_agent_argv.as_str(),
            "--stream-agent-output",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("LDGR_STREAM_STDOUT"))
    .stdout(predicate::str::contains("loop run=1 work=stream-check"))
    .stderr(predicate::str::contains("LDGR_STREAM_STDERR"));
    let streamed_output = fs::read_to_string(stream_artifacts.join("loop-run-1-agent-output.md"))?;
    assert!(
        streamed_output.contains("LDGR_STREAM_STDOUT"),
        "{streamed_output}"
    );
    assert!(
        streamed_output.contains("LDGR_STREAM_STDERR"),
        "{streamed_output}"
    );

    Ok(())
}

#[test]
fn autonomous_loop_runtime_preserves_full_output_in_files() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::write(&prompt_path, "{{ldgr_context}}")?;
    let agent_path = project.path().join("large-output-agent.sh");
    fs::write(
        &agent_path,
        "#!/bin/sh\ncat >/dev/null\nyes A | tr -d '\\n' | head -c 70000\nprintf 'STDOUT_TAIL\\n'\nyes B | tr -d '\\n' | head -c 70000 >&2\nprintf 'STDERR_TAIL\\n' >&2\n",
    )?;
    let agent_argv = serde_json::to_string(&vec!["sh", agent_path.to_str().unwrap()])?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "large-output-check",
            "--title",
            "Large output check",
            "--description",
            "Verify loop output capture preserves full stdout and stderr outside memory previews.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            "loop-prompt.md",
            "--agent-argv",
            agent_argv.as_str(),
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "loop run=1 work=large-output-check",
    ));

    let markdown = fs::read_to_string(artifact_root.join("loop-run-1-agent-output.md"))?;
    assert!(markdown.contains("preview_truncated: true"), "{markdown}");
    assert!(
        markdown.contains("loop-run-1-agent-stdout.txt"),
        "{markdown}"
    );
    assert!(
        markdown.contains("loop-run-1-agent-stderr.txt"),
        "{markdown}"
    );
    assert!(!markdown.contains("STDOUT_TAIL"), "{markdown}");
    assert!(!markdown.contains("STDERR_TAIL"), "{markdown}");

    let stdout = fs::read_to_string(artifact_root.join("loop-run-1-agent-stdout.txt"))?;
    let stderr = fs::read_to_string(artifact_root.join("loop-run-1-agent-stderr.txt"))?;
    assert!(stdout.contains("STDOUT_TAIL"));
    assert!(stderr.contains("STDERR_TAIL"));
    assert!(stdout.len() > 70000);
    assert!(stderr.len() > 70000);

    Ok(())
}

#[test]
fn global_notices_and_held_work_steer_context_without_canceling_work() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::write(&prompt_path, "{{ldgr_context}}")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "research-paused",
            "--title",
            "Research paused",
            "--description",
            "Hold this until dependency work is complete.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "dev-dependency",
            "--title",
            "Dev dependency",
            "--description",
            "Do this before resuming research.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "research-paused"],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "notice",
            "add",
            "--kind",
            "notification",
            "--body",
            "Research is waiting on dev-dependency; do not cancel the research thread.",
            "--source",
            "operator",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "status",
            "set",
            "research-paused",
            "held",
            "--reason",
            "Development work must land first.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "show", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: partial"))
    .stdout(predicate::str::contains("held work item"));
    command(project.path(), &db_path, &artifact_root, ["next"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("dev-dependency Dev dependency"));
    command(project.path(), &db_path, &artifact_root, ["context"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "work_items: pending=1 running=0 held=1 done=0 canceled=0",
        ))
        .stdout(predicate::str::contains("global_observations:"))
        .stdout(predicate::str::contains(
            "Research is waiting on dev-dependency",
        ))
        .stdout(predicate::str::contains("entity=work_item:1 type=hold"));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "list", "--status", "held", "--json"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(r#""slug": "research-paused""#))
    .stdout(predicate::str::contains(r#""status": "held""#));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--dry-run",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=2 work=dev-dependency"));
    let rendered_prompt = fs::read_to_string(artifact_root.join("loop-run-2-prompt.md"))?;
    assert!(
        rendered_prompt.contains("Research is waiting on dev-dependency"),
        "{rendered_prompt}"
    );

    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "notice",
            "edit",
            "1",
            "--body",
            "Dependency completed; research can resume.",
            "--clear-source",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["notice", "clear", "1", "--reason", "Dependency completed."],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "status",
            "set",
            "research-paused",
            "pending",
            "--reason",
            "Dependency completed.",
        ],
    )?;
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "show", "research-paused"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: pending"));

    Ok(())
}

#[test]
fn autonomous_loop_runtime_repeats_until_no_pending_work_remains() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    let agent_path = project.path().join("agent.sh");
    fs::write(&prompt_path, "{{ldgr_context}}")?;
    fs::write(
        &agent_path,
        r#"#!/usr/bin/env bash
set -euo pipefail
prompt="$(cat)"
if grep -q '"work_slug": "second-loop"' <<<"$prompt"; then
  "$LDGR_BIN" --db "$LDGR_DB" --artifact-root "$LDGR_ARTIFACT_ROOT" run close 2 --status success --outcome stop --rationale "All loop work is complete."
elif grep -q '"work_slug": "first-loop"' <<<"$prompt"; then
  "$LDGR_BIN" --db "$LDGR_DB" --artifact-root "$LDGR_ARTIFACT_ROOT" run close 1 --status success --outcome continue --rationale "First loop done; continue to second." --next-slug second-loop
else
  printf 'unexpected prompt\n' >&2
  exit 2
fi
"#,
    )?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "first-loop",
            "--title",
            "First loop",
            "--description",
            "First repeated loop item.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "second-loop",
            "--title",
            "Second loop",
            "--description",
            "Second repeated loop item.",
        ],
    )?;

    let ldgr_bin = assert_cmd::cargo::cargo_bin("ldgr");
    let agent_argv = serde_json::to_string(&vec![
        "bash".to_owned(),
        agent_path.to_str().expect("agent path is UTF-8").to_owned(),
    ])?;
    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--agent-argv",
            agent_argv.as_str(),
            "--max-iterations",
            "3",
        ],
    )?
    .env("LDGR_BIN", &ldgr_bin)
    .env("LDGR_DB", &db_path)
    .env("LDGR_ARTIFACT_ROOT", &artifact_root)
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=1 work=first-loop"))
    .stdout(predicate::str::contains("loop run=2 work=second-loop"))
    .stdout(predicate::str::contains(
        "Loop stopped after 2 iteration(s); no pending work items remain.",
    ));

    command(project.path(), &db_path, &artifact_root, ["work", "list"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("first-loop [done] First loop"))
        .stdout(predicate::str::contains("second-loop [done] Second loop"));

    Ok(())
}

#[test]
fn autonomous_loop_runtime_dry_run_does_not_consume_work_item() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::write(&prompt_path, "{{ldgr_context}}")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "first-cycle",
            "--title",
            "First cycle",
            "--description",
            "Verify loop completion gating on unresolved work.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "second-cycle",
            "--title",
            "Second cycle",
            "--description",
            "Should stay pending while first cycle is unresolved.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--dry-run",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=1 work=first-cycle"));

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--dry-run",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("loop run=2 work=first-cycle"));

    command(project.path(), &db_path, &artifact_root, ["run", "list"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("run=1 [partial] work=first-cycle"))
        .stdout(predicate::str::contains("run=2 [partial] work=first-cycle"));
    command(project.path(), &db_path, &artifact_root, ["work", "list"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "first-cycle [pending] First cycle",
        ))
        .stdout(predicate::str::contains(
            "second-cycle [pending] Second cycle",
        ));

    Ok(())
}

#[test]
fn autonomous_loop_runtime_blocks_on_active_run_even_when_work_status_drifted() -> anyhow::Result<()>
{
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::write(&prompt_path, "{{ldgr_context}}")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "drifted-cycle",
            "--title",
            "Drifted cycle",
            "--description",
            "The active run remains authoritative for loop blocking.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "next-cycle",
            "--title",
            "Next cycle",
            "--description",
            "Must not start while any active run exists.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "drifted-cycle"],
    )?;

    let connection = rusqlite::Connection::open(&db_path)?;
    connection.execute(
        "UPDATE work_item SET status = 'done' WHERE slug = 'drifted-cycle'",
        [],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--dry-run",
        ],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "Loop is blocked by unfinished work item drifted-cycle",
    ));

    command(project.path(), &db_path, &artifact_root, ["run", "list"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "run=1 [running] work=drifted-cycle",
        ))
        .stdout(predicate::str::contains("work=next-cycle").not());

    Ok(())
}

#[test]
fn autonomous_loop_runtime_marks_spawn_failure_failed() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt_path = project.path().join("loop-prompt.md");
    fs::write(&prompt_path, "{{ldgr_context}}")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "spawn-fails",
            "--title",
            "Spawn fails",
            "--description",
            "Verify failed agent spawn cleanup.",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "loop",
            "run",
            "--prompt",
            prompt_path.to_str().expect("prompt path is UTF-8"),
            "--agent-argv",
            r#"["definitely-not-a-real-ldgr-agent-command"]"#,
        ],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains("failed to spawn"));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "show", "1"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("status: failed"));
    command(project.path(), &db_path, &artifact_root, ["context"])?
        .assert()
        .success()
        .stdout(predicate::str::contains(
            "loop_state: phase=needs_decision run=1 work=spawn-fails status=failed",
        ))
        .stdout(predicate::str::contains(
            "record a decision to close the work item",
        ))
        .stdout(predicate::str::contains(
            "Loop runtime failed for spawn-fails",
        ));

    Ok(())
}

#[test]
fn web_cockpit_serves_context_artifact_viewer_and_loop_controls() -> anyhow::Result<()> {
    if !loopback_sockets_available("web_cockpit_serves_context_artifact_viewer_and_loop_controls") {
        return Ok(());
    }

    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let artifact_path = artifact_root.join("report.md");
    fs::create_dir_all(&artifact_root)?;
    fs::write(
        &artifact_path,
        "# Progress\n\nThe cockpit can render reports.",
    )?;
    fs::write(
        project.path().join("loop-prompt.md"),
        "Use the supplied LDGR context.\n\n{{CONTEXT_JSON}}",
    )?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "web-check",
            "--title",
            "Web check",
            "--description",
            "Verify the cockpit.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "encoded work",
            "--title",
            "Encoded work",
            "--description",
            "Verify percent-decoded detail routes.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "web-check"],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "artifact",
            "add",
            "1",
            "--kind",
            "report",
            "--path",
            artifact_path.to_str().expect("artifact path is UTF-8"),
            "--description",
            "Cockpit report.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "observation",
            "add",
            "1",
            "--body",
            "Cockpit smoke observation: the report artifact renders.",
        ],
    )?;

    let port = free_local_port()?;
    let port_text = port.to_string();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("ldgr"))
        .current_dir(project.path())
        .arg("--db")
        .arg(&db_path)
        .arg("--artifact-root")
        .arg(&artifact_root)
        .args([
            "web",
            "--host",
            "127.0.0.1",
            "--port",
            port_text.as_str(),
            "--control-token",
            "secret-token",
        ])
        .spawn()?;

    let index = http_get(port, "/")?;
    assert!(index.contains("Operations cockpit"), "{index}");
    assert!(index.contains("Operator controls"), "{index}");
    assert!(!index.contains("Due revalidation"), "{index}");
    assert!(!index.contains("Failures"), "{index}");
    assert!(!index.contains("Evidence"), "{index}");
    assert!(!index.contains("Tools"), "{index}");
    assert!(index.contains("/app.js"));
    let app_js = http_get(port, "/app.js")?;
    assert!(app_js.contains("/api/context"), "{app_js}");
    assert!(app_js.contains("/api/mission-log"), "{app_js}");
    assert!(app_js.contains("renderMissionLog"), "{app_js}");
    assert!(app_js.contains("set-mission-filter"), "{app_js}");
    assert!(
        app_js.contains(r#"data-action="open-artifact""#),
        "{app_js}"
    );
    assert!(app_js.contains("DECISION"), "{app_js}");
    assert!(app_js.contains("/api/runs/"), "{app_js}");
    assert!(app_js.contains("/api/loop/interventions/"), "{app_js}");
    assert!(app_js.contains("/api/loop/start"), "{app_js}");
    assert!(!app_js.contains("openai-rest"), "{app_js}");
    assert!(app_js.contains("loop-agent-timeout-seconds"), "{app_js}");
    assert!(app_js.contains("loop-agent"), "{app_js}");
    assert!(app_js.contains("Start loop cycle"), "{app_js}");
    assert!(app_js.contains("startLoop"), "{app_js}");
    assert!(app_js.contains("eventTitle"), "{app_js}");
    assert!(app_js.contains("renderEventPayload"), "{app_js}");
    assert!(app_js.contains("Loop intervention"), "{app_js}");
    assert!(app_js.contains("Pause next cycle"), "{app_js}");
    assert!(app_js.contains("Resume paused loop"), "{app_js}");
    assert!(app_js.contains("Stop next cycle"), "{app_js}");
    assert!(app_js.contains("openArtifact"), "{app_js}");
    assert!(app_js.contains("function apiJson"), "{app_js}");
    assert!(app_js.contains("function apiErrorMessage"), "{app_js}");
    assert!(app_js.contains("isEditingControl"), "{app_js}");
    assert!(app_js.contains("Recent cycle narrative"), "{app_js}");
    assert!(app_js.contains("Durable event log"), "{app_js}");
    assert!(!app_js.contains("renderDueRevalidation"), "{app_js}");
    assert!(!app_js.contains("readinessAudit"), "{app_js}");
    assert!(!app_js.contains("renderFailure"), "{app_js}");
    assert!(!app_js.contains("/api/tools/"), "{app_js}");
    assert!(app_js.contains("function encodedRouteSegment"), "{app_js}");
    assert!(
        !app_js.contains("'/api/work/' + encodeURIComponent(path.slice"),
        "{app_js}"
    );

    let pause = http_post_with_token(
        port,
        "/api/loop/interventions/pause",
        "reason=Operator+needs+visibility",
        "secret-token",
    )?;
    assert!(pause.contains(r#""action": "pause""#), "{pause}");
    assert!(pause.contains(r#""status": "pending""#), "{pause}");

    let resume = http_post_with_token(
        port,
        "/api/loop/interventions/resume",
        "reason=Operator+has+visibility",
        "secret-token",
    )?;
    assert!(resume.contains(r#""action": "pause""#), "{resume}");
    assert!(resume.contains(r#""status": "cleared""#), "{resume}");

    let steer = http_post_with_token(
        port,
        "/api/loop/interventions/steer",
        "reason=Use+the+web+cockpit&instruction=Record+progress+clearly",
        "secret-token",
    )?;
    assert!(steer.contains(r#""action": "steer""#), "{steer}");
    assert!(steer.contains("Record progress clearly"), "{steer}");

    fs::create_dir_all(project.path().join("prompts"))?;
    fs::write(
        project.path().join("prompts/loop-prompt.md"),
        "{{ldgr_context}}",
    )?;
    let start = http_post_with_token(
        port,
        "/api/loop/start",
        "prompt=prompts%2Floop-prompt.md&dry_run=true&agent_argv=&audit_argv=&project_complete_requested=false",
        "secret-token",
    )?;
    assert!(start.contains(r#""pid""#), "{start}");
    assert!(start.contains(r#""status": "spawned""#), "{start}");
    assert!(start.contains(r#""launch_observation_id""#), "{start}");

    let context = http_get(port, "/api/context")?;
    assert!(context.contains(r#""running_work_items": 1"#));
    assert!(context.contains(r#""loop_state""#));
    assert!(context.contains(r#""current_phase": "started""#));
    assert!(context.contains(r#""work_slug": "web-check""#));
    assert!(context.contains(r#""latest_events""#));
    assert!(context.contains(r#""loop_interventions""#));
    assert!(context.contains(r#""action": "steer""#));
    assert!(context.contains(r#""event_type": "resume""#));
    assert!(!context.contains(r#""due_fact_revalidation_policies""#));
    assert!(!context.contains(r#""adapter_context""#));
    assert!(!context.contains(r#""latest_expectations""#));
    assert!(!context.contains(r#""latest_validation_results""#));
    assert!(!context.contains(r#""latest_failures""#));
    assert!(!context.contains(r#""latest_target_profiles""#));
    assert!(!context.contains(r#""tools""#));

    let loop_exit_context = wait_for_context_containing(port, "web-loop-runtime-exit")?;
    assert!(
        loop_exit_context.contains("Web cockpit loop runtime process"),
        "{loop_exit_context}"
    );
    assert!(
        loop_exit_context.contains("exit_code=0"),
        "{loop_exit_context}"
    );

    let mission_log = http_get(port, "/api/mission-log")?;
    assert!(mission_log.contains(r#""totals""#), "{mission_log}");
    assert!(mission_log.contains(r#""work_done""#), "{mission_log}");
    assert!(mission_log.contains(r#""runs_succeeded""#), "{mission_log}");
    assert!(mission_log.contains(r#""runs_failed""#), "{mission_log}");
    assert!(
        mission_log.contains(r#""observations_recorded""#),
        "{mission_log}"
    );
    assert!(
        mission_log.contains(r#""artifacts_recorded""#),
        "{mission_log}"
    );
    assert!(
        !mission_log.contains(r#""facts_validated""#),
        "{mission_log}"
    );
    assert!(
        !mission_log.contains(r#""validations_passed""#),
        "{mission_log}"
    );
    assert!(
        !mission_log.contains(r#""milestones_achieved""#),
        "{mission_log}"
    );
    assert!(mission_log.contains(r#""entries""#), "{mission_log}");
    assert!(
        mission_log.contains(r#""slug": "web-check""#),
        "{mission_log}"
    );
    assert!(
        mission_log.contains(r#""title": "Web check""#),
        "{mission_log}"
    );
    assert!(mission_log.contains(r#""run_id": 1"#), "{mission_log}");
    assert!(mission_log.contains(r#""observations""#), "{mission_log}");
    assert!(
        mission_log.contains("Cockpit smoke observation: the report artifact renders."),
        "{mission_log}"
    );
    assert!(mission_log.contains(r#""artifacts""#), "{mission_log}");
    assert!(mission_log.contains("Cockpit report."), "{mission_log}");
    assert!(!mission_log.contains(r#""validations""#), "{mission_log}");

    let artifact = http_get(port, "/api/artifacts/1")?;
    assert!(artifact.contains(r#""viewer": "markdown""#));
    assert!(artifact.contains("The cockpit can render reports."));
    let run_detail = http_get(port, "/api/runs/1")?;
    assert!(run_detail.contains(r#""run""#));
    assert!(!run_detail.contains(r#""tool_executions""#));

    let work_detail = http_get(port, "/api/work/web-check")?;
    assert!(work_detail.contains(r#""work_item""#));
    assert!(work_detail.contains(r#""slug": "web-check""#));

    let encoded_work_detail = http_get(port, "/api/work/encoded%20work")?;
    assert!(encoded_work_detail.contains(r#""slug": "encoded work""#));

    let tool_detail = http_get(port, "/api/tools/echo-web")?;
    assert!(
        tool_detail.contains(r#""code": "not_found""#),
        "{tool_detail}"
    );

    let logs = http_get(port, "/api/logs")?;
    assert!(logs.contains(r#""events""#));
    assert!(logs.contains(r#""entity_type": "artifact""#), "{logs}");
    assert!(logs.contains(r#""event_type": "add""#), "{logs}");

    let run_page = http_get(port, "/runs/1")?;
    assert!(run_page.contains("Operations cockpit"), "{run_page}");

    child.kill().ok();
    child.wait().ok();
    Ok(())
}

#[test]
fn web_cockpit_prints_ephemeral_token_and_requires_it_for_loopback_posts() -> anyhow::Result<()> {
    if !loopback_sockets_available(
        "web_cockpit_prints_ephemeral_token_and_requires_it_for_loopback_posts",
    ) {
        return Ok(());
    }

    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    run(project.path(), &db_path, &artifact_root, ["init"])?;

    let port = free_local_port()?;
    let port_text = port.to_string();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("ldgr"))
        .current_dir(project.path())
        .arg("--db")
        .arg(&db_path)
        .arg("--artifact-root")
        .arg(&artifact_root)
        .args(["web", "--host", "127.0.0.1", "--port", port_text.as_str()])
        .stdout(Stdio::piped())
        .spawn()?;

    let stdout = child.stdout.take().context("failed to open web stdout")?;
    let mut lines = BufReader::new(stdout).lines();
    let listening = lines
        .next()
        .transpose()?
        .context("web server did not print listening line")?;
    assert!(
        listening.contains(&format!("http://127.0.0.1:{port}")),
        "{listening}"
    );
    let token_line = lines
        .next()
        .transpose()?
        .context("web server did not print control-token URL")?;
    let prefix = format!("open with control token: http://127.0.0.1:{port}/?control_token=");
    assert!(token_line.starts_with(&prefix), "{token_line}");
    let token = token_line.trim_start_matches(&prefix);
    assert_eq!(token.len(), 64, "{token_line}");
    assert!(
        token.bytes().all(|byte| byte.is_ascii_hexdigit()),
        "{token_line}"
    );

    let missing_token = http_post(port, "/api/loop/interventions/pause", "reason=missing")?;
    assert!(
        missing_token.contains("invalid X-LDGR-Control-Token"),
        "{missing_token}"
    );

    let accepted = http_post_with_token(
        port,
        "/api/loop/interventions/pause",
        "reason=ephemeral-token",
        token,
    )?;
    assert!(accepted.contains(r#""action": "pause""#), "{accepted}");

    child.kill()?;
    let _ = child.wait();
    Ok(())
}

#[test]
fn artifact_paths_are_current_relative_and_web_raw_is_cwd_independent() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let external_cwd = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let artifact_path = artifact_root.join("report.md");
    fs::create_dir_all(&artifact_root)?;
    fs::write(&artifact_path, "cwd-independent raw artifact")?;

    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "artifact-paths",
            "--title",
            "Artifact paths",
            "--description",
            "Verify path normalization.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "artifact-paths"],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "artifact",
            "add",
            "1",
            "--kind",
            "report",
            "--path",
            artifact_path.to_str().expect("artifact path is UTF-8"),
            "--description",
            "Absolute in-root path.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "artifact",
            "add",
            "1",
            "--kind",
            "report",
            "--path",
            "report.md",
            "--description",
            "Artifact-root-relative path.",
        ],
    )?;

    for artifact_id in [1, 2] {
        let artifact_id = artifact_id.to_string();
        let output = command(
            project.path(),
            &db_path,
            &artifact_root,
            ["artifact", "show", artifact_id.as_str(), "--json"],
        )?
        .output()?;
        assert!(output.status.success(), "{output:?}");
        let body: serde_json::Value = serde_json::from_slice(&output.stdout)?;
        assert_eq!(body["path"], "report.md");
    }

    if !loopback_sockets_available(
        "artifact_paths_are_current_relative_and_web_raw_is_cwd_independent",
    ) {
        return Ok(());
    }

    let port = free_local_port()?;
    let port_text = port.to_string();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("ldgr"))
        .current_dir(external_cwd.path())
        .arg("--db")
        .arg(&db_path)
        .arg("--artifact-root")
        .arg(&artifact_root)
        .args(["web", "--host", "127.0.0.1", "--port", port_text.as_str()])
        .spawn()?;

    for artifact_id in [1, 2] {
        let raw = http_get(port, &format!("/api/artifacts/{artifact_id}/raw"))?;
        assert_eq!(raw, "cwd-independent raw artifact");
    }

    child.kill().ok();
    child.wait().ok();
    Ok(())
}

#[test]
fn web_cockpit_rejects_fragile_or_unsafe_requests() -> anyhow::Result<()> {
    if !loopback_sockets_available("web_cockpit_rejects_fragile_or_unsafe_requests") {
        return Ok(());
    }

    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    fs::write(project.path().join("loop-prompt.md"), "{{ldgr_context}}")?;
    run(project.path(), &db_path, &artifact_root, ["init"])?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["web", "--host", "0.0.0.0", "--port", "0"],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains("refusing to expose web cockpit"));

    let port = free_local_port()?;
    let port_text = port.to_string();
    let mut child = StdCommand::new(assert_cmd::cargo::cargo_bin("ldgr"))
        .current_dir(project.path())
        .arg("--db")
        .arg(&db_path)
        .arg("--artifact-root")
        .arg(&artifact_root)
        .args([
            "web",
            "--host",
            "127.0.0.1",
            "--port",
            port_text.as_str(),
            "--control-token",
            "secret-token",
        ])
        .spawn()?;

    let missing_length = http_raw_request(
        port,
        "POST /api/loop/interventions/pause HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nConnection: close\r\n\r\nreason=test",
    )?;
    assert!(
        missing_length.starts_with("HTTP/1.1 411 Length Required"),
        "{missing_length}"
    );

    let oversized = http_raw_request(
        port,
        &format!(
            "POST /api/loop/interventions/pause HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            300 * 1024
        ),
    )?;
    assert!(
        oversized.starts_with("HTTP/1.1 413 Payload Too Large"),
        "{oversized}"
    );

    let missing_token = http_request_with_headers(
        port,
        "POST",
        "/api/loop/interventions/pause",
        "Content-Type: application/x-www-form-urlencoded\r\n",
        "reason=token-required",
    )?;
    assert!(
        missing_token.contains("invalid X-LDGR-Control-Token"),
        "{missing_token}"
    );

    let wrong_content_type = http_request_with_headers(
        port,
        "POST",
        "/api/loop/interventions/pause",
        "Content-Type: application/json\r\nX-LDGR-Control-Token: secret-token\r\n",
        "{}",
    )?;
    assert!(
        wrong_content_type.contains("application/x-www-form-urlencoded"),
        "{wrong_content_type}"
    );

    let bad_origin = http_request_with_headers(
        port,
        "POST",
        "/api/loop/interventions/pause",
        "Content-Type: application/x-www-form-urlencoded\r\nX-LDGR-Control-Token: secret-token\r\nOrigin: http://evil.example\r\n",
        "reason=bad-origin",
    )?;
    assert!(
        bad_origin.contains("Origin header does not match"),
        "{bad_origin}"
    );

    let missing_prompt = http_request_with_headers(
        port,
        "POST",
        "/api/loop/start",
        "Content-Type: application/x-www-form-urlencoded\r\nX-LDGR-Control-Token: secret-token\r\n",
        "prompt=missing-prompt.md&dry_run=true&agent_argv=&audit_argv=&project_complete_requested=false",
    )?;
    assert!(
        missing_prompt.contains("prompt path does not exist"),
        "{missing_prompt}"
    );

    let unknown_artifact = http_get(port, "/api/artifacts/9999")?;
    assert!(
        unknown_artifact.contains(r#""error""#)
            && unknown_artifact.contains("artifact 9999 not found"),
        "{unknown_artifact}"
    );

    let missing_run = http_raw_request(
        port,
        "GET /api/runs/9999 HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )?;
    assert!(
        missing_run.starts_with("HTTP/1.1 404 Not Found"),
        "{missing_run}"
    );
    assert!(
        missing_run.contains("Content-Type: application/json; charset=utf-8"),
        "{missing_run}"
    );
    assert!(
        missing_run.contains(r#""code": "not_found""#),
        "{missing_run}"
    );
    assert!(missing_run.contains("run 9999 not found"), "{missing_run}");

    let bad_run_id = http_raw_request(
        port,
        "GET /api/runs/not-a-number HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )?;
    assert!(
        bad_run_id.starts_with("HTTP/1.1 400 Bad Request"),
        "{bad_run_id}"
    );
    assert!(
        bad_run_id.contains(r#""code": "bad_request""#),
        "{bad_run_id}"
    );

    let unknown_api = http_raw_request(
        port,
        "GET /api/no-such-route HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )?;
    assert!(
        unknown_api.starts_with("HTTP/1.1 404 Not Found"),
        "{unknown_api}"
    );
    assert!(
        unknown_api.contains(r#""code": "not_found""#),
        "{unknown_api}"
    );

    let wrong_method = http_raw_request(
        port,
        "PUT /api/context HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
    )?;
    assert!(
        wrong_method.starts_with("HTTP/1.1 405 Method Not Allowed"),
        "{wrong_method}"
    );
    assert!(
        wrong_method.contains(r#""code": "method_not_allowed""#),
        "{wrong_method}"
    );

    child.kill().ok();
    child.wait().ok();
    Ok(())
}

fn free_local_port() -> anyhow::Result<u16> {
    let listener = TcpListener::bind(("127.0.0.1", 0))?;
    Ok(listener.local_addr()?.port())
}

#[test]
fn structured_dependencies_gate_readiness_and_reject_cycles() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    run(project.path(), &db_path, &artifact_root, ["init"])?;
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["status", "--full"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "loop: phase=idle run=none work=none status=idle",
    ))
    .stdout(predicate::str::contains("loop: phase=idle run=none work=none status=running").not());
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "registry",
            "--title",
            "Registry",
            "--description",
            "Build the registry.",
            "--priority",
            "P1",
            "--program",
            "audit",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "atomicity",
            "--title",
            "Atomicity",
            "--description",
            "Audit atomic updates.",
            "--acceptance-criteria",
            "Concurrent update test passes.",
            "--priority",
            "P0",
            "--program",
            "audit",
            "--depends-on",
            "registry",
        ],
    )?;

    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["status", "--program", "audit"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("next: registry [P1]"))
    .stdout(predicate::str::contains("next_dependencies: none declared"))
    .stdout(predicate::str::contains("queue: P0=1 P1=1"))
    .stdout(predicate::str::contains("unblocks: atomicity"));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["status", "--program", "audit", "--priority", "P0", "--json"],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains(
        "\"state\": \"idle, work blocked\"",
    ))
    .stdout(predicate::str::contains("\"ldgr work show atomicity\""))
    .stdout(predicate::str::contains("\"ldgr work show registry\""))
    .stdout(predicate::str::contains("\"ldgr run start registry\"").not());
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "atomicity"],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains("blocked by: registry"));
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "status", "set", "registry", "done"],
    )?;
    command(project.path(), &db_path, &artifact_root, ["next"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("atomicity Atomicity"));
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "edit", "registry", "--depends-on", "atomicity"],
    )?
    .assert()
    .failure()
    .stderr(predicate::str::contains(
        "dependency graph must remain acyclic",
    ));
    Ok(())
}

#[test]
fn schedule_import_export_round_trips_structured_queue() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let input = project.path().join("schedule.json");
    let output = project.path().join("backup.json");
    fs::write(
        &input,
        r#"{
  "format": "ldgr.schedule.v1",
  "work_items": [
    {"slug":"base","title":"Base","description":"Base work","priority":"P0","program":"audit"},
    {"slug":"follow","title":"Follow","description":"Follow-up","priority":"P1","program":"audit","group":"registry","acceptance_criteria":"Evidence recorded","dependencies":["base"]},
    {"slug":"external","title":"External","description":"Await validation","status":"held","hold_kind":"external-validation","hold_reason":"Lab review"}
  ]
}"#,
    )?;
    run(project.path(), &db_path, &artifact_root, ["init"])?;
    command(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "import", input.to_str().unwrap()],
    )?
    .assert()
    .success()
    .stdout(predicate::str::contains("created=3"))
    .stdout(predicate::str::contains("dependencies=1"));
    command(project.path(), &db_path, &artifact_root, ["status"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("held: external-validation=1"));
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["work", "export", "--output", output.to_str().unwrap()],
    )?;
    let backup: serde_json::Value = serde_json::from_str(&fs::read_to_string(output)?)?;
    assert_eq!(backup["format"], "ldgr.schedule.v1");
    assert_eq!(backup["work_items"].as_array().unwrap().len(), 3);
    assert_eq!(backup["work_items"][1]["dependencies"][0], "base");
    assert_eq!(backup["work_items"][2]["hold_kind"], "external-validation");
    Ok(())
}

#[test]
fn status_scopes_history_and_hides_stale_terminal_loop_when_new_work_exists() -> anyhow::Result<()>
{
    let project = TempDir::new()?;
    let db_path = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    run(project.path(), &db_path, &artifact_root, ["init"])?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "old-program",
            "--title",
            "Old program",
            "--description",
            "Previously completed work.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        ["run", "start", "old-program"],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "observe",
            "old-program",
            "--body",
            "stale packaging observation",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "run",
            "close",
            "old-program",
            "--status",
            "success",
            "--outcome",
            "stop",
            "--rationale",
            "Old program complete.",
        ],
    )?;
    run(
        project.path(),
        &db_path,
        &artifact_root,
        [
            "work",
            "create",
            "new-audit",
            "--title",
            "New audit",
            "--description",
            "Current work.",
            "--priority",
            "P0",
        ],
    )?;

    command(project.path(), &db_path, &artifact_root, ["status"])?
        .assert()
        .success()
        .stdout(predicate::str::contains("state: idle, work available"))
        .stdout(predicate::str::contains("next: new-audit [P0]"))
        .stdout(predicate::str::contains("phase=completed").not())
        .stdout(predicate::str::contains("stale packaging observation").not());
    let full_output = command(
        project.path(),
        &db_path,
        &artifact_root,
        ["status", "--full"],
    )?
    .output()?;
    assert!(full_output.status.success());
    let full_stdout = String::from_utf8(full_output.stdout)?;
    assert!(full_stdout.contains("global_history:"), "{full_stdout}");
    assert!(
        full_stdout.contains("stale packaging observation"),
        "{full_stdout}"
    );
    assert_eq!(full_stdout.matches("handoff:").count(), 1, "{full_stdout}");
    assert_eq!(
        full_stdout.matches("next_commands:").count(),
        1,
        "{full_stdout}"
    );
    Ok(())
}

fn loopback_sockets_available(test_name: &str) -> bool {
    match TcpListener::bind(("127.0.0.1", 0)) {
        Ok(listener) => {
            drop(listener);
            true
        }
        Err(error) => {
            eprintln!(
                "skipping {test_name}: loopback sockets are unavailable in this environment: {error}"
            );
            false
        }
    }
}

fn http_get(port: u16, path: &str) -> anyhow::Result<String> {
    http_request(port, "GET", path, "")
}

fn http_post(port: u16, path: &str, body: &str) -> anyhow::Result<String> {
    http_request(port, "POST", path, body)
}

fn http_post_with_token(port: u16, path: &str, body: &str, token: &str) -> anyhow::Result<String> {
    http_request_with_headers(
        port,
        "POST",
        path,
        &format!(
            "Content-Type: application/x-www-form-urlencoded\r\nX-LDGR-Control-Token: {token}\r\n"
        ),
        body,
    )
}

fn wait_for_context_containing(port: u16, expected: &str) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let latest = http_get(port, "/api/context")?;
        if latest.contains(expected) {
            return Ok(latest);
        }
        if Instant::now() >= deadline {
            anyhow::bail!("context did not contain {expected:?}; latest response: {latest}");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

fn http_request_with_headers(
    port: u16,
    method: &str,
    path: &str,
    extra_headers: &str,
    body: &str,
) -> anyhow::Result<String> {
    let raw = http_raw_request(
        port,
        &format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra_headers}Content-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        ),
    )?;
    if let Some((_, body)) = raw.split_once("\r\n\r\n") {
        return Ok(body.to_owned());
    }
    Ok(raw)
}

fn http_raw_request(port: u16, request: &str) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                stream.write_all(request.as_bytes())?;
                stream.flush()?;
                let mut bytes = Vec::new();
                match stream.read_to_end(&mut bytes) {
                    Ok(_) => {}
                    Err(error)
                        if error.kind() == ErrorKind::ConnectionReset && !bytes.is_empty() => {}
                    Err(error) => return Err(error.into()),
                }
                return Ok(String::from_utf8_lossy(&bytes).into_owned());
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn http_request(port: u16, method: &str, path: &str, body: &str) -> anyhow::Result<String> {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        match TcpStream::connect(("127.0.0.1", port)) {
            Ok(mut stream) => {
                let headers = if method == "POST" {
                    format!(
                        "Content-Type: application/x-www-form-urlencoded\r\nContent-Length: {}\r\n",
                        body.len()
                    )
                } else {
                    String::new()
                };
                stream.write_all(
                    format!(
                        "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{headers}Connection: close\r\n\r\n{body}",
                    )
                    .as_bytes(),
                )?;
                stream.flush()?;
                let mut bytes = Vec::new();
                match stream.read_to_end(&mut bytes) {
                    Ok(_) => {}
                    Err(error)
                        if error.kind() == ErrorKind::ConnectionReset && !bytes.is_empty() => {}
                    Err(error) => return Err(error.into()),
                }
                let response = String::from_utf8_lossy(&bytes).into_owned();
                if let Some((_, body)) = response.split_once("\r\n\r\n") {
                    return Ok(body.to_owned());
                }
                return Ok(response);
            }
            Err(error) if Instant::now() < deadline => {
                let _ = error;
                thread::sleep(Duration::from_millis(25));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn run<const ARG_COUNT: usize>(
    project: &Path,
    db_path: &Path,
    artifact_root: &Path,
    args: [&str; ARG_COUNT],
) -> anyhow::Result<()> {
    command(project, db_path, artifact_root, args)?
        .assert()
        .success();
    Ok(())
}

fn command<const ARG_COUNT: usize>(
    project: &Path,
    db_path: &Path,
    artifact_root: &Path,
    args: [&str; ARG_COUNT],
) -> anyhow::Result<Command> {
    let mut command = isolated_command(project)?;
    command
        .current_dir(project)
        .arg("--db")
        .arg(db_path)
        .arg("--artifact-root")
        .arg(artifact_root)
        .args(args);
    Ok(command)
}

fn isolated_command(project: &Path) -> anyhow::Result<Command> {
    let mut command = Command::cargo_bin("ldgr")?;
    command
        .current_dir(project)
        .env(
            "LDGR_ADAPTER_PATH",
            project.join(".ldgr/test-empty-adapters"),
        )
        .env("LDGR_HOME", project.join(".ldgr/test-empty-ldgr-home"))
        .env("HOME", project.join(".ldgr/test-empty-home"));
    Ok(command)
}

fn downgrade_cli_fixture_to_v1(db_path: &Path) -> anyhow::Result<()> {
    let connection = Connection::open(db_path)?;
    connection.execute_batch(
        r#"
        DROP TABLE component_record;
        DROP TABLE component_ingest;
        DROP TABLE schema_component;
        DROP TRIGGER IF EXISTS trg_work_dependency_no_cycle;
        DROP INDEX IF EXISTS idx_work_dependency_depends_on;
        DROP INDEX IF EXISTS idx_work_item_priority_program;
        DROP TABLE work_dependency;
        ALTER TABLE work_item DROP COLUMN hold_reason;
        ALTER TABLE work_item DROP COLUMN hold_kind;
        ALTER TABLE work_item DROP COLUMN acceptance_criteria;
        ALTER TABLE work_item DROP COLUMN work_group;
        ALTER TABLE work_item DROP COLUMN program;
        ALTER TABLE work_item DROP COLUMN priority;
        UPDATE schema_version SET version = 1 WHERE id = 1;
        "#,
    )?;
    Ok(())
}

fn write_adapter_fixture(dir: &Path, slug: &str, alias: &str) -> anyhow::Result<()> {
    fs::create_dir_all(dir.join("prompts"))?;
    fs::create_dir_all(dir.join("templates"))?;
    fs::write(dir.join("prompts/loop.md"), "loop")?;
    fs::write(dir.join("templates/milestones.md"), "milestones")?;
    fs::write(dir.join("templates/spec.md"), "spec")?;
    fs::write(
        dir.join("adapter.toml"),
        format!(
            r#"
[adapter]
slug = "{slug}"
title = "{slug} title"
core_version = "0.1"
aliases = ["{alias}"]

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"

[[tools]]
name = "{slug}-check"
argv = ["{slug}", "check"]
description = "Run a check."
"#
        ),
    )?;
    Ok(())
}

fn write_adapter_namespace_fixture(
    dir: &Path,
    slug: &str,
    namespace: &str,
    argv: &str,
) -> anyhow::Result<()> {
    fs::create_dir_all(dir.join("prompts"))?;
    fs::create_dir_all(dir.join("templates"))?;
    fs::write(dir.join("prompts/loop.md"), "loop")?;
    fs::write(dir.join("templates/milestones.md"), "milestones")?;
    fs::write(dir.join("templates/spec.md"), "spec")?;
    fs::write(
        dir.join("adapter.toml"),
        format!(
            r#"
[adapter]
slug = "{slug}"
title = "{slug} title"
core_version = "0.1"

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"

[[commands]]
namespace = "{namespace}"
argv = {argv}

[commands.help]
usage = "ldgr {namespace} <args...>"
summary = "Run {namespace} adapter commands."
"#
        ),
    )?;
    Ok(())
}
