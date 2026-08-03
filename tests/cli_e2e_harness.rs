#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use rusqlite::Connection;

#[test]
#[ignore = "completion-grade Windows matrix; run explicitly with --ignored --nocapture"]
fn fresh_project_cli_e2e_harness() -> anyhow::Result<()> {
    let manifest_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let source_root = manifest_root
        .parent()
        .context("ldgr-core checkout must have a workspace parent")?;
    let executable = PathBuf::from(env!("CARGO_BIN_EXE_ldgr"));
    let unique = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let matrix_root = std::env::var_os("LDGR_CLI_E2E_MATRIX_ROOT")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            manifest_root
                .join("target")
                .join("cli-e2e")
                .join(format!("run-{}-{unique}", std::process::id()))
        });
    if matrix_root.exists() {
        bail!(
            "refusing to reuse retained CLI E2E matrix directory {}",
            matrix_root.display()
        );
    }
    let fixture_root = matrix_root.join("legacy-fixtures");
    let test_root = matrix_root.join("matrix");

    fs::create_dir_all(&fixture_root)?;
    for version in 1..=4 {
        create_legacy_fixture(&executable, &fixture_root, version)?;
    }

    let script = manifest_root.join("tests/cli_e2e.ps1");
    let output = Command::new("powershell.exe")
        .args(["-NoLogo", "-NoProfile", "-NonInteractive"])
        .args(["-ExecutionPolicy", "Bypass", "-File"])
        .arg(&script)
        .arg("-Exe")
        .arg(&executable)
        .arg("-TestRoot")
        .arg(&test_root)
        .arg("-LegacyFixtureRoot")
        .arg(&fixture_root)
        .arg("-SourceRoot")
        .arg(source_root)
        .output()
        .with_context(|| format!("failed to launch {}", script.display()))?;

    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    println!("{stdout}");
    if !stderr.trim().is_empty() {
        eprintln!("{stderr}");
    }
    println!("retained CLI E2E matrix at {}", matrix_root.display());

    if !output.status.success() {
        bail!(
            "CLI E2E matrix failed with {}; inspect {}",
            output.status,
            test_root.join("result.json").display()
        );
    }
    Ok(())
}

fn create_legacy_fixture(
    executable: &Path,
    fixture_root: &Path,
    version: i64,
) -> anyhow::Result<()> {
    let project = fixture_root.join(format!("v{version}"));
    let db = project.join(".ldgr/ldgr.db");
    let artifact_root = project.join(".ldgr/artifacts");
    let profile = project.join("profile");
    fs::create_dir_all(&project)?;

    let output = Command::new(executable)
        .current_dir(&project)
        .env_remove("HOME")
        .env("USERPROFILE", &profile)
        .env("LOCALAPPDATA", profile.join("AppData/Local"))
        .env("XDG_STATE_HOME", profile.join(".local/state"))
        .env("LDGR_HOME", profile.join(".ldgr"))
        .env(
            "LDGR_ADAPTER_PATH",
            profile.join(".ldgr/test-empty-adapters"),
        )
        .arg("--db")
        .arg(&db)
        .arg("--artifact-root")
        .arg(&artifact_root)
        .arg("init")
        .output()?;
    if !output.status.success() {
        bail!(
            "failed to initialize v{version} fixture: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let connection = Connection::open(&db)?;
    connection.execute(
        "INSERT INTO work_item (id, slug, title, description)
         VALUES (?1, ?2, ?3, ?4)",
        (
            4500 + version,
            format!("preserved-v{version}"),
            format!("Preserved v{version} work"),
            "Causal ID must survive automatic migration.",
        ),
    )?;
    downgrade_fixture(&connection, version)?;
    Ok(())
}

fn downgrade_fixture(connection: &Connection, version: i64) -> anyhow::Result<()> {
    match version {
        1 => connection.execute_batch(
            r#"
            DROP TABLE component_record;
            DROP TABLE component_ingest;
            DROP TABLE schema_component;
            DROP TABLE error_disposition;
            DROP TABLE error_transition;
            DROP TABLE error_relation;
            DROP TABLE error_occurrence;
            DROP TABLE error_record;
            DROP TABLE project_identity;
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
        )?,
        2 => connection.execute_batch(
            r#"
            DROP TABLE component_record;
            DROP TABLE component_ingest;
            DROP TABLE schema_component;
            DROP TABLE error_disposition;
            DROP TABLE error_transition;
            DROP TABLE error_relation;
            DROP TABLE error_occurrence;
            DROP TABLE error_record;
            DROP TABLE project_identity;
            UPDATE schema_version SET version = 2 WHERE id = 1;
            "#,
        )?,
        3 => connection.execute_batch(
            r#"
            DROP TABLE error_disposition;
            DROP TABLE error_transition;
            DROP TABLE error_relation;
            DROP TABLE error_occurrence;
            DROP TABLE error_record;
            DROP TABLE project_identity;
            DROP TABLE component_record;
            DROP TABLE component_ingest;
            UPDATE schema_component
               SET schema_version = CASE WHEN namespace = 'core' THEN 3 ELSE schema_version END,
                   minimum_core_schema = 3,
                   contract_hash = 'sha256:cli-e2e-v3';
            UPDATE schema_version SET version = 3 WHERE id = 1;
            "#,
        )?,
        4 => connection.execute_batch(
            r#"
            DROP TABLE error_disposition;
            DROP TABLE error_transition;
            DROP TABLE error_relation;
            DROP TABLE error_occurrence;
            DROP TABLE error_record;
            DROP TABLE project_identity;
            UPDATE schema_component
               SET schema_version = CASE WHEN namespace = 'core' THEN 4 ELSE schema_version END,
                   minimum_core_schema = 4,
                   contract_hash = 'sha256:cli-e2e-v4';
            UPDATE schema_version SET version = 4 WHERE id = 1;
            "#,
        )?,
        other => bail!("unsupported legacy fixture version {other}"),
    }
    Ok(())
}
