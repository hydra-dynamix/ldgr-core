#![cfg(windows)]

use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use tempfile::TempDir;

fn ldgr_bin() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_ldgr"))
}

fn run_ldgr(
    project: &Path,
    profile: &Path,
    db: &Path,
    artifact_root: &Path,
    args: &[&str],
) -> anyhow::Result<Output> {
    Command::new(ldgr_bin())
        .current_dir(project)
        .env_remove("HOME")
        .env("USERPROFILE", profile)
        .arg("--db")
        .arg(db)
        .arg("--artifact-root")
        .arg(artifact_root)
        .args(args)
        .output()
        .context("failed to run ldgr")
}

fn powershell_literal(path: &Path) -> String {
    format!("'{}'", path.display().to_string().replace('\'', "''"))
}

fn initialize_project(
    project: &Path,
    profile: &Path,
    db: &Path,
    artifact_root: &Path,
) -> anyhow::Result<()> {
    let init = run_ldgr(project, profile, db, artifact_root, &["init"])?;
    if !init.status.success() {
        bail!(
            "ldgr init failed: {}",
            String::from_utf8_lossy(&init.stderr)
        );
    }
    Ok(())
}

#[test]
fn detached_loop_survives_launcher_exit_and_maps_userprofile_to_home() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let profile = project.path().join("windows-profile");
    let db = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt = project.path().join("loop.md");
    let observed_home = project.path().join("observed-home.txt");
    fs::create_dir_all(&profile)?;
    fs::write(&prompt, "{{ldgr_context}}\n")?;

    initialize_project(project.path(), &profile, &db, &artifact_root)?;
    let work = run_ldgr(
        project.path(),
        &profile,
        &db,
        &artifact_root,
        &[
            "work",
            "create",
            "detached-check",
            "--title",
            "Detached check",
            "--description",
            "Verify detached Windows loop execution.",
        ],
    )?;
    if !work.status.success() {
        bail!(
            "work creation failed: {}",
            String::from_utf8_lossy(&work.stderr)
        );
    }

    let script = format!(
        "$value = [Environment]::GetEnvironmentVariable('HOME', 'Process'); \
         [IO.File]::WriteAllText({}, $value); \
         & {} --db {} --artifact-root {} run close detached-check \
         --status success --outcome stop --rationale 'detached Windows test'; \
         exit $LASTEXITCODE",
        powershell_literal(&observed_home),
        powershell_literal(&ldgr_bin()),
        powershell_literal(&db),
        powershell_literal(&artifact_root),
    );
    let agent_argv =
        serde_json::to_string(&vec!["powershell.exe", "-NoProfile", "-Command", &script])?;
    let detached = run_ldgr(
        project.path(),
        &profile,
        &db,
        &artifact_root,
        &[
            "loop",
            "run",
            "--prompt",
            prompt.to_str().context("prompt path was not UTF-8")?,
            "--agent-argv",
            &agent_argv,
            "--detach",
        ],
    )?;
    if !detached.status.success() {
        bail!(
            "detached launch failed: {}",
            String::from_utf8_lossy(&detached.stderr)
        );
    }
    let launch_output = String::from_utf8_lossy(&detached.stdout);
    assert!(launch_output.contains("detached loop pid="));
    assert!(launch_output.contains("status: ldgr context"));

    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline && !observed_home.exists() {
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        observed_home.exists(),
        "detached loop did not execute its child agent"
    );
    assert_eq!(
        fs::read_to_string(&observed_home)?,
        profile.display().to_string()
    );

    let completion_deadline = Instant::now() + Duration::from_secs(15);
    let mut work_output = String::new();
    while Instant::now() < completion_deadline {
        let work = run_ldgr(
            project.path(),
            &profile,
            &db,
            &artifact_root,
            &["work", "show", "detached-check"],
        )?;
        work_output = String::from_utf8_lossy(&work.stdout).into_owned();
        if work.status.success() && work_output.contains("status: done") {
            break;
        }
        thread::sleep(Duration::from_millis(50));
    }
    assert!(
        work_output.contains("status: done"),
        "detached work item did not finish: {work_output}"
    );

    let logs = project.path().join(".ldgr/logs");
    assert!(fs::read_dir(&logs)?.any(|entry| {
        entry.is_ok_and(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .contains("loop-detached-")
        })
    }));
    Ok(())
}

#[test]
fn detached_completion_request_without_audit_fails_before_starting_work() -> anyhow::Result<()> {
    let project = TempDir::new()?;
    let profile = project.path().join("windows-profile");
    let db = project.path().join(".ldgr/ldgr.db");
    let artifact_root = project.path().join(".ldgr/artifacts");
    let prompt = project.path().join("loop.md");
    fs::create_dir_all(&profile)?;
    fs::write(&prompt, "{{ldgr_context}}\n")?;
    initialize_project(project.path(), &profile, &db, &artifact_root)?;

    let work = run_ldgr(
        project.path(),
        &profile,
        &db,
        &artifact_root,
        &[
            "work",
            "create",
            "audit-check",
            "--title",
            "Audit check",
            "--description",
            "Verify detached audit preflight.",
        ],
    )?;
    assert!(work.status.success());

    let launch = run_ldgr(
        project.path(),
        &profile,
        &db,
        &artifact_root,
        &[
            "loop",
            "run",
            "--prompt",
            prompt.to_str().context("prompt path was not UTF-8")?,
            "--agent-argv",
            "[\"powershell.exe\"]",
            "--project-complete-requested",
            "--detach",
        ],
    )?;
    assert!(!launch.status.success());
    assert!(String::from_utf8_lossy(&launch.stderr)
        .contains("--audit-argv is required when --project-complete-requested is supplied"));

    let status = run_ldgr(
        project.path(),
        &profile,
        &db,
        &artifact_root,
        &["work", "show", "audit-check"],
    )?;
    let status_output = String::from_utf8_lossy(&status.stdout);
    assert!(status_output.contains("status: pending"));
    assert!(!project.path().join(".ldgr/logs").exists());
    Ok(())
}
