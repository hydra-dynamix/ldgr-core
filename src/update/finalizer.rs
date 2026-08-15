use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::Duration;

use anyhow::{bail, ensure, Context};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::apply::{
    apply_staged_update_plan, InstallTransaction, PlanStagingOwnership, StagedArtifact,
    StagedUpdatePlan, StagingManifest,
};
use super::catalog::VerifiedAdapterUpdateCatalog;
use super::installation::validate_receipt;
use super::plan::{UpdateAction, UpdatePlan};
use super::state::{
    atomic_json, ComponentResult, RecoveryAction, TerminalError, TerminalOutcome, UpdateLock,
    UpdateMode, UpdateStateStore, MAX_STATE_BYTES, SCHEMA_VERSION,
};

const FINALIZER_PAYLOAD: &str = "finalizer.json";
const STAGING_MANIFEST: &str = "staging-manifest.json";
const PARENT_EXIT_TIMEOUT: Duration = Duration::from_secs(60);
const FINALIZER_LOCK_LEASE: Duration = Duration::from_secs(60 * 60);

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WindowsFinalizerPayload {
    schema_version: u32,
    plan_id: String,
    manifest_sha256: String,
    core_binary_sha256: String,
    agentctl_binary_sha256: String,
    ownership: PlanStagingOwnership,
    adapter_catalog: VerifiedAdapterUpdateCatalog,
}

struct FinalizerContext {
    plan: UpdatePlan,
    manifest: StagingManifest,
    payload: WindowsFinalizerPayload,
    stage_root: PathBuf,
}

pub(crate) fn prepare_foreground_finalizer(
    store: &UpdateStateStore,
    lock: &UpdateLock,
    plan: &UpdatePlan,
    staged: StagedUpdatePlan,
    ownership: &PlanStagingOwnership,
    adapter_catalog: &VerifiedAdapterUpdateCatalog,
) -> anyhow::Result<(PathBuf, String)> {
    let stage_root = store.stage_dir(plan.plan_id())?;
    let (core_binary, agentctl_binary) = staged_core_pair(&staged.manifest)?;
    let payload = WindowsFinalizerPayload {
        schema_version: SCHEMA_VERSION,
        plan_id: plan.plan_id().to_owned(),
        manifest_sha256: file_sha256(&stage_root.join(STAGING_MANIFEST))?,
        core_binary_sha256: file_sha256(core_binary)?,
        agentctl_binary_sha256: file_sha256(agentctl_binary)?,
        ownership: ownership.clone(),
        adapter_catalog: adapter_catalog.clone(),
    };
    let payload_path = stage_root.join(FINALIZER_PAYLOAD);
    atomic_json(&payload_path, &payload)?;
    store.bind_finalizer_payload(lock, plan.plan_id(), &file_sha256(&payload_path)?)?;
    store.mark_applying(lock, plan.plan_id())?;
    staged.transaction.preserve_for_finalizer()?;
    let executable = core_binary.to_path_buf();
    ensure!(
        executable.starts_with(stage_root.join("artifacts")),
        "staged finalizer executable escapes the artifact boundary"
    );
    let token = store.load_staging_state(plan.plan_id())?.internal_token;
    Ok((executable, token))
}

pub(crate) fn launch_foreground_finalizer(
    store: &UpdateStateStore,
    lock: UpdateLock,
    plan_id: &str,
    executable: &Path,
    token: &str,
) -> anyhow::Result<()> {
    launch_finalizer(store, lock, plan_id, executable, token, std::process::id())
}

fn launch_finalizer(
    store: &UpdateStateStore,
    lock: UpdateLock,
    plan_id: &str,
    executable: &Path,
    token: &str,
    parent_pid: u32,
) -> anyhow::Result<()> {
    let mut command = Command::new(executable);
    command
        .arg("__update-finalizer")
        .arg("--parent-pid")
        .arg(parent_pid.to_string())
        .arg("--plan")
        .arg(store.plan_path(plan_id)?)
        .arg("--token")
        .arg(token)
        .env(super::startup::RECURSION_GUARD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::loop_runtime::configure_child_home(&mut command);
    configure_hidden_detached(&mut command);
    let child = command.spawn().with_context(|| {
        format!(
            "update.activation-failed: failed to launch staged Windows finalizer {}",
            executable.display()
        )
    })?;
    lock.handoff_to_finalizer(child.id(), plan_id, token)
}

pub(crate) fn handle_finalizer(
    parent_pid: u32,
    plan_path: &Path,
    token: &str,
) -> anyhow::Result<()> {
    ensure!(
        parent_pid > 0 && parent_pid != std::process::id(),
        "invalid Windows finalizer parent PID"
    );
    let home = user_home()?;
    let store = UpdateStateStore::open(home.join(".ldgr"))?;
    let initial = load_context(&store, plan_path, token, true)?;
    let wait_result = wait_for_process_exit(parent_pid, PARENT_EXIT_TIMEOUT);
    let lock = claim_finalizer_lock(&store, token, initial.plan.plan_id())?;
    let context = load_context(&store, plan_path, token, true)?;
    let (mut transaction, recovery_only) = InstallTransaction::resume_for_finalizer(
        context.stage_root.join("rollback"),
        context.plan.plan_id(),
        &context.manifest.targets,
    )?;

    let activation_preflight = wait_result.and_then(|()| {
        validate_installed_pair(&context.payload.ownership)
            .context("installed Core/agentctl changed after Windows staging")
    });

    let terminal = if recovery_only || activation_preflight.is_err() {
        match transaction.rollback() {
            Ok(()) => (
                TerminalOutcome::RolledBack,
                terminal_components(&context.plan, "rolled_back"),
                activation_preflight.err().map(|error| TerminalError {
                    code: "update.activation-failed".to_owned(),
                    summary: redact(&format!("{error:#}"), &home),
                }),
            ),
            Err(error) => (
                TerminalOutcome::Failed,
                terminal_components(&context.plan, "failed"),
                Some(TerminalError {
                    code: "update.rollback-failed".to_owned(),
                    summary: redact(&format!("{error:#}"), &home),
                }),
            ),
        }
    } else {
        match apply_staged_update_plan(
            &context.plan,
            &context.manifest,
            &context.payload.adapter_catalog,
            &context.payload.ownership,
            &mut transaction,
            true,
        ) {
            Ok(components) => {
                transaction.commit()?;
                (TerminalOutcome::Applied, components, None)
            }
            Err(failure) => match transaction.rollback() {
                Ok(()) => (
                    TerminalOutcome::RolledBack,
                    terminal_components(&context.plan, "rolled_back"),
                    Some(TerminalError {
                        code: "update.activation-failed".to_owned(),
                        summary: redact(&format!("{:#}", failure.source), &home),
                    }),
                ),
                Err(rollback) => (
                    TerminalOutcome::Failed,
                    terminal_components(&context.plan, "failed"),
                    Some(TerminalError {
                        code: "update.rollback-failed".to_owned(),
                        summary: redact(
                            &format!("{:#}; rollback failed: {rollback:#}", failure.source),
                            &home,
                        ),
                    }),
                ),
            },
        }
    };

    let history = store.complete_plan_deferred_cleanup(
        &lock,
        context.plan.plan_id(),
        terminal.0,
        terminal.1,
        terminal.2,
    )?;
    store.write_pending_report(&history)?;
    if history.outcome == TerminalOutcome::Applied {
        if let Some(mut cache) = store.load_cache()? {
            cache.result = super::state::CachedCheckResult::Current;
            cache.consecutive_failures = 0;
            store.write_cache(&cache)?;
        }
    }
    lock.release()?;
    ensure!(
        history.outcome != TerminalOutcome::Failed,
        "update.rollback-failed: Windows update finalization requires manual recovery"
    );
    Ok(())
}

pub(crate) fn recover_and_report_pending() -> anyhow::Result<bool> {
    let home = user_home()?;
    let store = UpdateStateStore::open(home.join(".ldgr"))?;
    let lock = match store.acquire_lock(UpdateMode::Recover, None, FINALIZER_LOCK_LEASE) {
        Ok(lock) => lock,
        Err(error) if format!("{error:#}").contains("update.locked") => return Ok(false),
        Err(error) => return Err(error),
    };
    let records = store.recover_interrupted(&lock)?;
    if let Some(record) = records
        .iter()
        .find(|record| record.action == RecoveryAction::RollbackRequired)
    {
        let state = store.load_staging_state(&record.plan_id)?;
        let plan_path = store.plan_path(&record.plan_id)?;
        let context = load_context(&store, &plan_path, &state.internal_token, false)?;
        launch_finalizer(
            &store,
            lock,
            &record.plan_id,
            staged_core_pair(&context.manifest)?.0,
            &state.internal_token,
            std::process::id(),
        )?;
        eprintln!("update recovery: rollback finalizer launched; retry the command after it exits");
        return Ok(true);
    }
    lock.release()?;
    if let Some(report) = store.take_pending_report()? {
        let status = match report.outcome {
            TerminalOutcome::Applied => "applied",
            TerminalOutcome::RolledBack => "rolled_back",
            TerminalOutcome::Failed => "failed",
        };
        eprintln!(
            "update result: plan {} status={status}",
            report.plan_id.get(..12).unwrap_or(&report.plan_id)
        );
        if let Some(error) = report.error {
            eprintln!("update result: {}: {}", error.code, error.summary);
        }
    }
    Ok(false)
}

fn load_context(
    store: &UpdateStateStore,
    supplied_plan_path: &Path,
    token: &str,
    require_staged_executable: bool,
) -> anyhow::Result<FinalizerContext> {
    let supplied = fs::canonicalize(supplied_plan_path)
        .context("Windows finalizer plan path is unavailable")?;
    ensure!(
        supplied.file_name().and_then(|name| name.to_str()) == Some("plan.json"),
        "Windows finalizer plan path is not durable plan state"
    );
    let stage_root = supplied
        .parent()
        .context("Windows finalizer plan has no staging root")?
        .to_path_buf();
    let plan_id = stage_root
        .file_name()
        .and_then(|name| name.to_str())
        .context("Windows finalizer staging root has no plan id")?;
    let expected = fs::canonicalize(store.plan_path(plan_id)?)
        .context("recorded Windows finalizer plan path is unavailable")?;
    ensure!(
        paths_equal(&supplied, &expected),
        "Windows finalizer plan path differs from recorded state"
    );
    let envelope = store.load_staged_update_plan(plan_id)?;
    let state = store.load_staging_state(plan_id)?;
    ensure!(
        state.internal_token == token,
        "Windows finalizer token does not match staged plan"
    );
    let payload_path = stage_root.join(FINALIZER_PAYLOAD);
    ensure!(
        state.finalizer_payload_sha256.as_deref() == Some(&file_sha256(&payload_path)?),
        "Windows finalizer payload digest mismatch"
    );
    let payload: WindowsFinalizerPayload = read_json_limited(&payload_path)?;
    ensure!(
        payload.schema_version == SCHEMA_VERSION && payload.plan_id == plan_id,
        "Windows finalizer payload does not match the staged plan"
    );
    let manifest_path = stage_root.join(STAGING_MANIFEST);
    ensure!(
        file_sha256(&manifest_path)? == payload.manifest_sha256,
        "Windows staging manifest digest mismatch"
    );
    let manifest: StagingManifest = read_json_limited(&manifest_path)?;
    ensure!(
        manifest.schema_version == 1
            && manifest.plan_id == plan_id
            && manifest.platform == envelope.plan.platform(),
        "Windows staging manifest does not match the resolved plan"
    );
    let (executable, agentctl) = staged_core_pair(&manifest)?;
    ensure!(
        file_sha256(executable)? == payload.core_binary_sha256,
        "Windows staged Core digest mismatch"
    );
    ensure!(
        file_sha256(agentctl)? == payload.agentctl_binary_sha256,
        "Windows staged agentctl digest mismatch"
    );
    ensure!(
        executable.starts_with(stage_root.join("artifacts")) && executable.is_file(),
        "Windows finalizer executable escapes staged artifacts"
    );
    if require_staged_executable {
        ensure!(
            paths_equal(
                &fs::canonicalize(std::env::current_exe()?)?,
                &fs::canonicalize(executable)?
            ),
            "hidden Windows finalizer was not launched from the recorded staged Core"
        );
    }
    Ok(FinalizerContext {
        plan: envelope.plan,
        manifest,
        payload,
        stage_root,
    })
}

fn staged_core_pair(manifest: &StagingManifest) -> anyhow::Result<(&Path, &Path)> {
    manifest
        .artifacts
        .iter()
        .find_map(|artifact| match artifact {
            StagedArtifact::CoreBundle {
                core_binary,
                agentctl_binary,
                ..
            } => Some((core_binary.as_path(), agentctl_binary.as_path())),
            _ => None,
        })
        .context("Windows update plan has no staged Core/agentctl pair")
}

fn validate_installed_pair(ownership: &PlanStagingOwnership) -> anyhow::Result<()> {
    let receipt = ownership
        .core
        .as_ref()
        .context("Windows finalizer has no Core installation receipt")?;
    validate_receipt(receipt)?;
    ensure!(
        receipt.core_binary_path.is_file() && receipt.agentctl_binary_path.is_file(),
        "installed Core/agentctl pair is incomplete"
    );
    ensure!(
        file_sha256(&receipt.core_binary_path)? == receipt.core_binary_sha256,
        "installed Core digest changed after staging"
    );
    ensure!(
        file_sha256(&receipt.agentctl_binary_path)? == receipt.agentctl_binary_sha256,
        "installed agentctl digest changed after staging"
    );
    Ok(())
}

fn claim_finalizer_lock(
    store: &UpdateStateStore,
    token: &str,
    plan_id: &str,
) -> anyhow::Result<UpdateLock> {
    let mut last = None;
    for _ in 0..50 {
        match store.claim_handed_off_finalizer_lock(token, plan_id) {
            Ok(lock) => return Ok(lock),
            Err(error) => {
                last = Some(error);
                thread::sleep(Duration::from_millis(20));
            }
        }
    }
    Err(last.context("Windows finalizer lock handoff was not completed")?)
}

fn terminal_components(plan: &UpdatePlan, selected_status: &str) -> Vec<ComponentResult> {
    plan.components()
        .iter()
        .map(|component| {
            let status = if matches!(
                component.action(),
                UpdateAction::Update | UpdateAction::ReinstallLocalSource
            ) {
                selected_status
            } else {
                match component.action() {
                    UpdateAction::None => "none",
                    UpdateAction::SkipUnmanaged => "skip_unmanaged",
                    UpdateAction::Blocked => "blocked",
                    UpdateAction::Applied => "applied",
                    UpdateAction::RolledBack => "rolled_back",
                    UpdateAction::Failed => "failed",
                    UpdateAction::Update | UpdateAction::ReinstallLocalSource => unreachable!(),
                }
            };
            ComponentResult {
                kind: if component.kind() == super::plan::UpdateComponentKind::CoreBundle {
                    "core_bundle"
                } else {
                    "adapter"
                }
                .to_owned(),
                name: component.name().to_owned(),
                status: status.to_owned(),
            }
        })
        .collect()
}

fn read_json_limited<T: DeserializeOwned>(path: &Path) -> anyhow::Result<T> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
        "Windows finalizer state is not a regular file"
    );
    ensure!(
        metadata.len() <= MAX_STATE_BYTES,
        "Windows finalizer state exceeds the size limit"
    );
    serde_json::from_reader(File::open(path)?)
        .with_context(|| format!("failed to parse {}", path.display()))
}

fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    let mut buffer = [0_u8; 64 * 1024];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        digest.update(&buffer[..count]);
    }
    Ok(format!("{:x}", digest.finalize()))
}

fn redact(text: &str, home: &Path) -> String {
    text.replace(&home.display().to_string(), "~")
        .chars()
        .take(2_048)
        .collect()
}

fn user_home() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("could not determine home directory from HOME/USERPROFILE")
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    left.as_os_str()
        .to_string_lossy()
        .eq_ignore_ascii_case(&right.as_os_str().to_string_lossy())
}

fn wait_for_process_exit(pid: u32, timeout: Duration) -> anyhow::Result<()> {
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, WAIT_OBJECT_0, WAIT_TIMEOUT,
    };
    use windows_sys::Win32::Storage::FileSystem::SYNCHRONIZE;
    use windows_sys::Win32::System::Threading::{OpenProcess, WaitForSingleObject};
    unsafe {
        let process = OpenProcess(SYNCHRONIZE, 0, pid);
        if process.is_null() {
            if GetLastError() == ERROR_INVALID_PARAMETER {
                return Ok(());
            }
            bail!("failed to open parent process {pid} for bounded wait");
        }
        let result = WaitForSingleObject(process, timeout.as_millis().min(u32::MAX as u128) as u32);
        CloseHandle(process);
        match result {
            WAIT_OBJECT_0 => Ok(()),
            WAIT_TIMEOUT => bail!("timed out waiting for parent process {pid} to exit"),
            other => bail!("parent process wait failed with status {other}"),
        }
    }
}

fn configure_hidden_detached(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    const CREATE_NO_WINDOW: u32 = 0x0800_0000;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP | CREATE_NO_WINDOW);
}

#[cfg(test)]
mod tests {
    use std::fs::{self, OpenOptions};
    use std::os::windows::fs::OpenOptionsExt;
    use std::process::Command;
    use std::time::{Duration, Instant};

    use super::wait_for_process_exit;
    use crate::update::apply::{InstallTransaction, OwnedTarget};

    #[test]
    fn parent_wait_is_bounded_and_observes_process_exit() -> anyhow::Result<()> {
        let mut child = Command::new("powershell.exe")
            .args(["-NoProfile", "-Command", "Start-Sleep -Milliseconds 100"])
            .spawn()?;
        wait_for_process_exit(child.id(), Duration::from_secs(5))?;
        let _ = child.wait();
        let started = Instant::now();
        let error = wait_for_process_exit(std::process::id(), Duration::from_millis(25))
            .expect_err("live process should hit the bounded timeout");
        assert!(error.to_string().contains("timed out"));
        assert!(started.elapsed() < Duration::from_secs(2));
        Ok(())
    }

    #[test]
    fn windows_file_lock_blocks_activation_without_losing_snapshot() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let owned = root.path().join("owned");
        fs::create_dir_all(&owned)?;
        let target = owned.join("ldgr.exe");
        let staged = root.path().join("staged.exe");
        fs::write(&target, b"old")?;
        fs::write(&staged, b"new")?;
        let targets = vec![OwnedTarget::new("core", "core_binary", &owned, &target)];
        let journal = root.path().join("rollback");
        let mut transaction =
            InstallTransaction::prepare(journal.clone(), &"a".repeat(64), &targets)?;
        let locked = OpenOptions::new().read(true).share_mode(0).open(&target)?;
        assert!(transaction.activate_file(&staged, &target).is_err());
        drop(locked);
        transaction.rollback()?;
        assert_eq!(fs::read(&target)?, b"old");
        let mut retry =
            InstallTransaction::prepare(root.path().join("retry"), &"a".repeat(64), &targets)?;
        retry.activate_file(&staged, &target)?;
        assert_eq!(fs::read(&target)?, b"new");
        retry.rollback()?;
        assert_eq!(fs::read(&target)?, b"old");
        Ok(())
    }
}
