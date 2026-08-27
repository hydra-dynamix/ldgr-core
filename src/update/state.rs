use std::collections::BTreeMap;
use std::fs::{self, File, OpenOptions};
use std::io::{ErrorKind, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};
use thiserror::Error;

use super::plan::UpdatePlan;

pub const SCHEMA_VERSION: u32 = 1;
pub const MAX_STATE_BYTES: u64 = 1024 * 1024;
pub const MAX_HISTORY: usize = 32;
pub const MAX_LOCK_LEASE: Duration = Duration::from_secs(86_400);
const CACHE: &str = "update-state.json";
const LOCK: &str = "update.lock";
const PLAN: &str = "plan.json";
const STATE: &str = "state.json";
const PENDING_REPORT: &str = "pending-report.json";

#[derive(Clone, Debug)]
pub struct UpdateStateStore {
    home: PathBuf,
    updates: PathBuf,
    staging: PathBuf,
    history: PathBuf,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateCache {
    pub schema_version: u32,
    pub checked_at_unix_ms: u64,
    pub result: CachedCheckResult,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_etag: Option<String>,
    #[serde(default)]
    pub consecutive_failures: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_notice: Option<CachedNotice>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub notice_history: Vec<CachedNotice>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum CachedCheckResult {
    Current,
    UpdatesAvailable {
        plan_id: String,
        target_core: String,
        adapter_updates: u32,
    },
    Failed {
        code: String,
        summary: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CachedNotice {
    pub plan_id: String,
    pub notified_at_unix_ms: u64,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateMode {
    Check,
    Apply,
    Finalize,
    Recover,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StagingPhase {
    Staged,
    Applying,
    Applied,
    RolledBack,
    Failed,
}

impl StagingPhase {
    fn terminal(self) -> bool {
        matches!(self, Self::Applied | Self::RolledBack | Self::Failed)
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum TerminalOutcome {
    Applied,
    RolledBack,
    Failed,
}

impl TerminalOutcome {
    fn phase(self) -> StagingPhase {
        match self {
            Self::Applied => StagingPhase::Applied,
            Self::RolledBack => StagingPhase::RolledBack,
            Self::Failed => StagingPhase::Failed,
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TerminalError {
    pub code: String,
    pub summary: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ComponentResult {
    pub kind: String,
    pub name: String,
    pub status: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StagingState {
    pub schema_version: u32,
    pub plan_id: String,
    pub mode: UpdateMode,
    pub phase: StagingPhase,
    pub created_at_unix_ms: u64,
    pub updated_at_unix_ms: u64,
    pub internal_token: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub finalizer_payload_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TerminalError>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct TerminalHistory {
    pub schema_version: u32,
    pub plan_id: String,
    pub mode: UpdateMode,
    pub started_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub outcome: TerminalOutcome,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub components: Vec<ComponentResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TerminalError>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct PendingUpdateReport {
    pub schema_version: u32,
    pub plan_id: String,
    pub outcome: TerminalOutcome,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<TerminalError>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct PlanEnvelope<T> {
    pub schema_version: u32,
    pub plan_id: String,
    pub plan: T,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecoveryAction {
    ResumeStaging,
    RollbackRequired,
    TerminalReceiptRecovered,
    IncompleteStagingDiscarded,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RecoveryRecord {
    pub plan_id: String,
    pub action: RecoveryAction,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateLockRecord {
    pub schema_version: u32,
    pub pid: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_start_identity: Option<String>,
    pub created_at_unix_ms: u64,
    pub lease_expires_at_unix_ms: u64,
    pub mode: UpdateMode,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_id: Option<String>,
    pub owner_token: String,
}

#[derive(Debug)]
pub struct UpdateLock {
    path: PathBuf,
    record: UpdateLockRecord,
    released: bool,
}

#[derive(Debug, Error)]
pub enum UpdateStateError {
    #[error(
        "update.locked: update {mode:?} is owned by pid {pid} until {lease_expires_at_unix_ms}"
    )]
    Locked {
        pid: u32,
        mode: UpdateMode,
        lease_expires_at_unix_ms: u64,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum ProcessIdentity {
    Running(Option<String>),
    Gone,
    Unavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum WriteFault {
    None,
    AfterTempSync,
}

impl UpdateLock {
    pub fn record(&self) -> &UpdateLockRecord {
        &self.record
    }
    pub fn release(mut self) -> anyhow::Result<()> {
        self.release_inner()
    }

    pub(crate) fn owner_token(&self) -> &str {
        &self.record.owner_token
    }

    /// Transfers a startup-check lock to a detached child without opening a
    /// race in which another foreground invocation can schedule a second
    /// worker. The child must prove possession of the owner token before it
    /// can use or release the lock.
    pub(crate) fn handoff_to_pid(mut self, pid: u32) -> anyhow::Result<()> {
        ensure!(pid > 0, "update lock handoff PID must be positive");
        ensure!(
            self.record.mode == UpdateMode::Check && self.record.plan_id.is_none(),
            "only an unbound update-check lock can be handed to a worker"
        );
        let current: UpdateLockRecord = read_json(&self.path, "update lock")?;
        validate_lock(&current)?;
        ensure!(
            current.owner_token == self.record.owner_token,
            "update lock ownership changed before worker handoff"
        );
        ensure!(
            current.pid == std::process::id(),
            "update lock is not owned by the foreground process"
        );
        self.record.pid = pid;
        self.record.process_start_identity = process_start(pid);
        atomic_json(&self.path, &self.record)?;
        self.released = true;
        Ok(())
    }

    /// Transfers a foreground apply lock to the detached Windows finalizer.
    /// The per-plan token becomes the lock token so the child must prove both
    /// possession of durable plan state and ownership of the handed-off PID.
    pub(crate) fn handoff_to_finalizer(
        mut self,
        pid: u32,
        plan_id: &str,
        internal_token: &str,
    ) -> anyhow::Result<()> {
        ensure!(pid > 0, "update finalizer PID must be positive");
        validate_id(plan_id)?;
        ensure!(
            internal_token.len() == 64 && lower_hex(internal_token),
            "invalid internal update finalizer token"
        );
        ensure!(
            matches!(self.record.mode, UpdateMode::Apply | UpdateMode::Recover)
                && self.record.plan_id.is_none(),
            "only an unbound foreground apply or recovery lock can be handed to a finalizer"
        );
        let current: UpdateLockRecord = read_json(&self.path, "update lock")?;
        validate_lock(&current)?;
        ensure!(
            current.owner_token == self.record.owner_token && current.pid == std::process::id(),
            "update lock ownership changed before finalizer handoff"
        );
        self.record.pid = pid;
        self.record.process_start_identity = process_start(pid);
        self.record.mode = UpdateMode::Finalize;
        self.record.plan_id = Some(plan_id.to_owned());
        self.record.owner_token = internal_token.to_owned();
        atomic_json(&self.path, &self.record)?;
        self.released = true;
        Ok(())
    }

    fn release_inner(&mut self) -> anyhow::Result<()> {
        if self.released {
            return Ok(());
        }
        // An expired owner must not unlink a successor's lock.
        if now_ms()? >= self.record.lease_expires_at_unix_ms {
            self.released = true;
            return Ok(());
        }
        match read_json::<UpdateLockRecord>(&self.path, "update lock") {
            Ok(current) if current.owner_token == self.record.owner_token => {
                reject_link(&self.path)?;
                fs::remove_file(&self.path).context("failed to release update lock")?;
                sync_parent(&self.path)?;
            }
            Ok(_) => {}
            Err(error) if not_found(&error) => {}
            Err(error) => return Err(error),
        }
        self.released = true;
        Ok(())
    }
}

impl Drop for UpdateLock {
    fn drop(&mut self) {
        let _ = self.release_inner();
    }
}

impl UpdateStateStore {
    pub fn open(ldgr_home: impl AsRef<Path>) -> anyhow::Result<Self> {
        let home = absolute(ldgr_home.as_ref())?;
        let updates = home.join("updates");
        let store = Self {
            home,
            staging: updates.join("staging"),
            history: updates.join("history"),
            updates,
        };
        store.prepare()?;
        Ok(store)
    }

    pub fn load_cache(&self) -> anyhow::Result<Option<UpdateCache>> {
        self.verify_paths()?;
        match read_json(&self.home.join(CACHE), "update cache") {
            Ok(cache) => {
                let cache: UpdateCache = cache;
                schema(cache.schema_version, "update cache")?;
                validate_cache(&cache)?;
                Ok(Some(cache))
            }
            Err(error) if not_found(&error) => Ok(None),
            Err(error) => Err(error),
        }
    }

    pub fn write_cache(&self, cache: &UpdateCache) -> anyhow::Result<()> {
        self.verify_paths()?;
        schema(cache.schema_version, "update cache")?;
        validate_cache(cache)?;
        atomic_json(&self.home.join(CACHE), cache)
    }

    pub fn acquire_lock(
        &self,
        mode: UpdateMode,
        plan_id: Option<&str>,
        lease: Duration,
    ) -> anyhow::Result<UpdateLock> {
        self.acquire_lock_at(mode, plan_id, lease, now_ms()?)
    }

    /// Claims a check lock that the foreground process atomically handed to
    /// this detached worker. A guessed hidden command is insufficient: both
    /// the unguessable token and the recorded child PID must match.
    pub(crate) fn claim_handed_off_check_lock(
        &self,
        owner_token: &str,
    ) -> anyhow::Result<UpdateLock> {
        self.verify_paths()?;
        ensure!(
            owner_token.len() == 64 && lower_hex(owner_token),
            "invalid internal update worker token"
        );
        let path = self.updates.join(LOCK);
        let record: UpdateLockRecord = read_json(&path, "update lock")?;
        validate_lock(&record)?;
        ensure!(
            record.mode == UpdateMode::Check && record.plan_id.is_none(),
            "internal update worker requires an unbound check lock"
        );
        ensure!(
            record.owner_token == owner_token,
            "internal update worker token does not own the update lock"
        );
        ensure!(
            record.pid == std::process::id(),
            "update lock was not handed to this worker process"
        );
        ensure!(
            now_ms()? < record.lease_expires_at_unix_ms,
            "internal update worker lock lease expired"
        );
        let actual_start = process_start(std::process::id());
        ensure!(
            !matches!(
                (&record.process_start_identity, &actual_start),
                (Some(expected), Some(actual)) if expected != actual
            ),
            "internal update worker process identity changed"
        );
        Ok(UpdateLock {
            path,
            record,
            released: false,
        })
    }

    pub(crate) fn claim_handed_off_finalizer_lock(
        &self,
        owner_token: &str,
        plan_id: &str,
    ) -> anyhow::Result<UpdateLock> {
        self.verify_paths()?;
        validate_id(plan_id)?;
        ensure!(
            owner_token.len() == 64 && lower_hex(owner_token),
            "invalid internal update finalizer token"
        );
        let path = self.updates.join(LOCK);
        let record: UpdateLockRecord = read_json(&path, "update lock")?;
        validate_lock(&record)?;
        ensure!(
            record.mode == UpdateMode::Finalize && record.plan_id.as_deref() == Some(plan_id),
            "internal update finalizer requires the matching plan lock"
        );
        ensure!(
            record.owner_token == owner_token,
            "internal update finalizer token does not own the update lock"
        );
        ensure!(
            record.pid == std::process::id(),
            "update lock was not handed to this finalizer process"
        );
        ensure!(
            now_ms()? < record.lease_expires_at_unix_ms,
            "internal update finalizer lock lease expired"
        );
        let actual_start = process_start(std::process::id());
        ensure!(
            !matches!(
                (&record.process_start_identity, &actual_start),
                (Some(expected), Some(actual)) if expected != actual
            ),
            "internal update finalizer process identity changed"
        );
        Ok(UpdateLock {
            path,
            record,
            released: false,
        })
    }

    fn acquire_lock_at(
        &self,
        mode: UpdateMode,
        plan_id: Option<&str>,
        lease: Duration,
        now: u64,
    ) -> anyhow::Result<UpdateLock> {
        self.verify_paths()?;
        ensure!(
            !lease.is_zero() && lease <= MAX_LOCK_LEASE,
            "update lock lease must be between 1 ms and 24 hours"
        );
        if let Some(id) = plan_id {
            validate_id(id)?;
        }
        let record = UpdateLockRecord {
            schema_version: SCHEMA_VERSION,
            pid: std::process::id(),
            process_start_identity: process_start(std::process::id()),
            created_at_unix_ms: now,
            lease_expires_at_unix_ms: now
                .checked_add(lease.as_millis() as u64)
                .context("update lock lease overflow")?,
            mode,
            plan_id: plan_id.map(str::to_owned),
            owner_token: token()?,
        };
        let path = self.updates.join(LOCK);
        for _ in 0..8 {
            match create_lock(&path, &record) {
                Ok(()) => {
                    return Ok(UpdateLock {
                        path,
                        record,
                        released: false,
                    })
                }
                Err(error) if already_exists(&error) => {
                    let current: UpdateLockRecord = read_json(&path, "update lock")?;
                    validate_lock(&current)?;
                    if !lock_stale(&current, now) {
                        return Err(UpdateStateError::Locked {
                            pid: current.pid,
                            mode: current.mode,
                            lease_expires_at_unix_ms: current.lease_expires_at_unix_ms,
                        }
                        .into());
                    }
                    reclaim_lock(&path, &current.owner_token)?;
                }
                Err(error) => return Err(error),
            }
        }
        bail!("update.locked: ownership changed repeatedly during acquisition")
    }

    pub fn stage_plan<T: Serialize>(&self, lock: &UpdateLock, plan: &T) -> anyhow::Result<String> {
        self.verify_lock(lock)?;
        let canonical = canonical_bytes(plan)?;
        let plan_id = digest(&canonical);
        if let Some(bound) = lock.record.plan_id.as_deref() {
            ensure!(
                bound == plan_id,
                "update lock plan id does not match staged plan"
            );
        }
        let directory = self.stage_dir(&plan_id)?;
        secure_dir(&directory, "update staging plan directory")?;
        sync_parent(&directory)?;
        let plan_path = directory.join(PLAN);
        if plan_path.exists() {
            let existing: PlanEnvelope<Value> = read_json(&plan_path, "staged update plan")?;
            validate_envelope(&existing)?;
            ensure!(
                canonical_bytes(&existing.plan)? == canonical,
                "staged plan digest collision"
            );
        } else {
            atomic_json(
                &plan_path,
                &PlanEnvelope {
                    schema_version: SCHEMA_VERSION,
                    plan_id: plan_id.clone(),
                    plan,
                },
            )?;
        }
        let state_path = directory.join(STATE);
        if state_path.exists() {
            ensure!(
                self.load_staging_state(&plan_id)?.phase == StagingPhase::Staged,
                "staged plan is no longer resumable"
            );
        } else {
            let now = now_ms()?;
            atomic_json(
                &state_path,
                &StagingState {
                    schema_version: SCHEMA_VERSION,
                    plan_id: plan_id.clone(),
                    mode: lock.record.mode,
                    phase: StagingPhase::Staged,
                    created_at_unix_ms: now,
                    updated_at_unix_ms: now,
                    internal_token: token()?,
                    finalizer_payload_sha256: None,
                    components: Vec::new(),
                    error: None,
                },
            )?;
        }
        Ok(plan_id)
    }

    pub(crate) fn stage_update_plan(
        &self,
        lock: &UpdateLock,
        plan: &UpdatePlan,
    ) -> anyhow::Result<String> {
        self.verify_lock(lock)?;
        plan.verify_plan_id()?;
        let plan_id = plan.plan_id().to_owned();
        if let Some(bound) = lock.record.plan_id.as_deref() {
            ensure!(
                bound == plan_id,
                "update lock plan id does not match resolved plan"
            );
        }
        let directory = self.stage_dir(&plan_id)?;
        secure_dir(&directory, "update staging plan directory")?;
        sync_parent(&directory)?;
        let plan_path = directory.join(PLAN);
        if plan_path.exists() {
            let existing: PlanEnvelope<UpdatePlan> =
                read_json(&plan_path, "staged resolved update plan")?;
            schema(existing.schema_version, "staged resolved update plan")?;
            ensure!(
                existing.plan_id == plan_id && existing.plan == *plan,
                "staged resolved plan changed for deterministic plan id"
            );
            existing.plan.verify_plan_id()?;
        } else {
            atomic_json(
                &plan_path,
                &PlanEnvelope {
                    schema_version: SCHEMA_VERSION,
                    plan_id: plan_id.clone(),
                    plan,
                },
            )?;
        }
        let state_path = directory.join(STATE);
        if state_path.exists() {
            ensure!(
                self.load_staging_state(&plan_id)?.phase == StagingPhase::Staged,
                "staged resolved plan is no longer resumable"
            );
        } else {
            let now = now_ms()?;
            atomic_json(
                &state_path,
                &StagingState {
                    schema_version: SCHEMA_VERSION,
                    plan_id: plan_id.clone(),
                    mode: lock.record.mode,
                    phase: StagingPhase::Staged,
                    created_at_unix_ms: now,
                    updated_at_unix_ms: now,
                    internal_token: token()?,
                    finalizer_payload_sha256: None,
                    components: Vec::new(),
                    error: None,
                },
            )?;
        }
        Ok(plan_id)
    }

    pub fn load_staged_plan<T: DeserializeOwned + Serialize>(
        &self,
        plan_id: &str,
    ) -> anyhow::Result<PlanEnvelope<T>> {
        let directory = self.stage_dir(plan_id)?;
        verify_dir(&directory, "update staging plan directory")?;
        let envelope: PlanEnvelope<T> = read_json(&directory.join(PLAN), "staged update plan")?;
        schema(envelope.schema_version, "staged update plan")?;
        ensure!(
            envelope.plan_id == plan_id,
            "staged plan id does not match its directory"
        );
        ensure!(
            deterministic_plan_id(&envelope.plan)? == plan_id,
            "staged plan digest mismatch"
        );
        Ok(envelope)
    }

    pub fn load_staged_update_plan(
        &self,
        plan_id: &str,
    ) -> anyhow::Result<PlanEnvelope<UpdatePlan>> {
        let directory = self.stage_dir(plan_id)?;
        verify_dir(&directory, "update staging plan directory")?;
        let envelope: PlanEnvelope<UpdatePlan> =
            read_json(&directory.join(PLAN), "staged resolved update plan")?;
        schema(envelope.schema_version, "staged resolved update plan")?;
        ensure!(
            envelope.plan_id == plan_id && envelope.plan.plan_id() == plan_id,
            "staged resolved plan id does not match its directory"
        );
        envelope.plan.verify_plan_id()?;
        Ok(envelope)
    }

    pub fn load_staging_state(&self, plan_id: &str) -> anyhow::Result<StagingState> {
        let directory = self.stage_dir(plan_id)?;
        verify_dir(&directory, "update staging plan directory")?;
        let state: StagingState = read_json(&directory.join(STATE), "staged update state")?;
        validate_state(&state, plan_id)?;
        Ok(state)
    }

    pub fn mark_applying(&self, lock: &UpdateLock, plan_id: &str) -> anyhow::Result<()> {
        self.verify_plan_lock(lock, plan_id)?;
        let mut state = self.load_staging_state(plan_id)?;
        ensure!(
            state.phase == StagingPhase::Staged,
            "staged plan cannot enter applying from {:?}",
            state.phase
        );
        state.phase = StagingPhase::Applying;
        advance_state_timestamp(&mut state)?;
        atomic_json(&self.stage_dir(plan_id)?.join(STATE), &state)
    }

    pub(crate) fn bind_finalizer_payload(
        &self,
        lock: &UpdateLock,
        plan_id: &str,
        sha256: &str,
    ) -> anyhow::Result<()> {
        self.verify_plan_lock(lock, plan_id)?;
        ensure!(
            sha256.len() == 64 && lower_hex(sha256),
            "invalid Windows finalizer payload digest"
        );
        let mut state = self.load_staging_state(plan_id)?;
        ensure!(
            state.phase == StagingPhase::Staged,
            "finalizer payload can only be bound to staged plans"
        );
        state.finalizer_payload_sha256 = Some(sha256.to_owned());
        advance_state_timestamp(&mut state)?;
        atomic_json(&self.stage_dir(plan_id)?.join(STATE), &state)
    }

    pub fn complete_plan(
        &self,
        lock: &UpdateLock,
        plan_id: &str,
        outcome: TerminalOutcome,
        components: Vec<ComponentResult>,
        error: Option<TerminalError>,
    ) -> anyhow::Result<TerminalHistory> {
        self.verify_plan_lock(lock, plan_id)?;
        let mut state = self.load_staging_state(plan_id)?;
        ensure!(
            matches!(state.phase, StagingPhase::Staged | StagingPhase::Applying),
            "staged plan is already terminal"
        );
        state.phase = outcome.phase();
        advance_state_timestamp(&mut state)?;
        state.components = components;
        state.error = error;
        atomic_json(&self.stage_dir(plan_id)?.join(STATE), &state)?;
        let history = history_from(&state)?;
        self.persist_history(&history)?;
        self.remove_stage(plan_id)?;
        Ok(history)
    }

    /// Writes terminal state and immutable history while retaining staging.
    /// A Windows finalizer runs from the staging tree, so cleanup must wait
    /// until a later process can remove the exited executable.
    pub(crate) fn complete_plan_deferred_cleanup(
        &self,
        lock: &UpdateLock,
        plan_id: &str,
        outcome: TerminalOutcome,
        components: Vec<ComponentResult>,
        error: Option<TerminalError>,
    ) -> anyhow::Result<TerminalHistory> {
        self.verify_plan_lock(lock, plan_id)?;
        let mut state = self.load_staging_state(plan_id)?;
        ensure!(
            matches!(state.phase, StagingPhase::Staged | StagingPhase::Applying),
            "staged plan is already terminal"
        );
        state.phase = outcome.phase();
        advance_state_timestamp(&mut state)?;
        state.components = components;
        state.error = error;
        atomic_json(&self.stage_dir(plan_id)?.join(STATE), &state)?;
        let history = history_from(&state)?;
        self.persist_history(&history)?;
        Ok(history)
    }

    pub(crate) fn write_pending_report(&self, history: &TerminalHistory) -> anyhow::Result<()> {
        validate_history(history, &history.plan_id)?;
        atomic_json(
            &self.updates.join(PENDING_REPORT),
            &PendingUpdateReport {
                schema_version: SCHEMA_VERSION,
                plan_id: history.plan_id.clone(),
                outcome: history.outcome,
                error: history.error.clone(),
            },
        )
    }

    pub(crate) fn take_pending_report(&self) -> anyhow::Result<Option<PendingUpdateReport>> {
        let path = self.updates.join(PENDING_REPORT);
        let report: PendingUpdateReport = match read_json(&path, "pending update report") {
            Ok(report) => report,
            Err(error) if not_found(&error) => return Ok(None),
            Err(error) => return Err(error),
        };
        schema(report.schema_version, "pending update report")?;
        validate_id(&report.plan_id)?;
        reject_link(&path)?;
        fs::remove_file(&path)?;
        sync_parent(&path)?;
        Ok(Some(report))
    }

    pub(crate) fn plan_path(&self, plan_id: &str) -> anyhow::Result<PathBuf> {
        Ok(self.stage_dir(plan_id)?.join(PLAN))
    }

    pub fn read_history(&self) -> anyhow::Result<Vec<TerminalHistory>> {
        self.verify_paths()?;
        let mut result = Vec::new();
        for entry in fs::read_dir(&self.history).context("failed to read update history")? {
            let entry = entry?;
            reject_link(&entry.path())?;
            ensure!(
                entry.file_type()?.is_file(),
                "update history contains a non-file entry"
            );
            let Some(id) = history_id(&entry.path()) else {
                continue;
            };
            let record: TerminalHistory = read_json(&entry.path(), "update history")?;
            validate_history(&record, &id)?;
            result.push(record);
        }
        result.sort_by(|a, b| {
            b.finished_at_unix_ms
                .cmp(&a.finished_at_unix_ms)
                .then_with(|| b.plan_id.cmp(&a.plan_id))
        });
        Ok(result)
    }

    pub fn recover_interrupted(&self, lock: &UpdateLock) -> anyhow::Result<Vec<RecoveryRecord>> {
        self.verify_lock(lock)?;
        ensure!(
            lock.record.mode == UpdateMode::Recover,
            "recovery requires a recover-mode lock"
        );
        self.clean_temps()?;
        let mut ids = Vec::new();
        for entry in fs::read_dir(&self.staging)? {
            let entry = entry?;
            reject_link(&entry.path())?;
            ensure!(
                entry.file_type()?.is_dir(),
                "update staging contains a non-directory entry"
            );
            let id = entry
                .file_name()
                .into_string()
                .map_err(|_| anyhow::anyhow!("plan id is not UTF-8"))?;
            validate_id(&id)?;
            ids.push(id);
        }
        ids.sort();
        let mut result = Vec::new();
        for plan_id in ids {
            let directory = self.stage_dir(&plan_id)?;
            if !directory.join(PLAN).exists() || !directory.join(STATE).exists() {
                self.remove_stage(&plan_id)?;
                result.push(RecoveryRecord {
                    plan_id,
                    action: RecoveryAction::IncompleteStagingDiscarded,
                });
                continue;
            }
            validate_staged_plan_file(&directory, &plan_id)?;
            let state = self.load_staging_state(&plan_id)?;
            let action = match state.phase {
                StagingPhase::Staged => RecoveryAction::ResumeStaging,
                StagingPhase::Applying => RecoveryAction::RollbackRequired,
                phase if phase.terminal() => {
                    self.persist_history(&history_from(&state)?)?;
                    self.remove_stage(&plan_id)?;
                    RecoveryAction::TerminalReceiptRecovered
                }
                _ => unreachable!(),
            };
            result.push(RecoveryRecord { plan_id, action });
        }
        Ok(result)
    }

    fn prepare(&self) -> anyhow::Result<()> {
        secure_dir(&self.home, "LDGR home")?;
        secure_dir(&self.updates, "update state directory")?;
        secure_dir(&self.staging, "update staging directory")?;
        secure_dir(&self.history, "update history directory")?;
        self.verify_paths()
    }

    fn verify_paths(&self) -> anyhow::Result<()> {
        verify_dir(&self.home, "LDGR home")?;
        verify_dir(&self.updates, "update state directory")?;
        verify_dir(&self.staging, "update staging directory")?;
        verify_dir(&self.history, "update history directory")?;
        boundary(&self.home, &self.updates)?;
        boundary(&self.home, &self.staging)?;
        boundary(&self.home, &self.history)
    }

    pub(crate) fn stage_dir(&self, plan_id: &str) -> anyhow::Result<PathBuf> {
        validate_id(plan_id)?;
        let path = self.staging.join(plan_id);
        boundary(&self.staging, &path)?;
        Ok(path)
    }

    fn verify_lock(&self, lock: &UpdateLock) -> anyhow::Result<()> {
        let path = self.updates.join(LOCK);
        ensure!(
            lock.path == path,
            "update lock belongs to another state store"
        );
        ensure!(
            now_ms()? < lock.record.lease_expires_at_unix_ms,
            "update lock lease expired"
        );
        let current: UpdateLockRecord = read_json(&path, "update lock")?;
        validate_lock(&current)?;
        ensure!(
            current.owner_token == lock.record.owner_token,
            "update lock ownership changed"
        );
        Ok(())
    }

    fn verify_plan_lock(&self, lock: &UpdateLock, plan_id: &str) -> anyhow::Result<()> {
        validate_id(plan_id)?;
        self.verify_lock(lock)?;
        if let Some(bound) = lock.record.plan_id.as_deref() {
            ensure!(bound == plan_id, "update lock is bound to another plan");
        }
        Ok(())
    }

    fn persist_history(&self, record: &TerminalHistory) -> anyhow::Result<()> {
        validate_history(record, &record.plan_id)?;
        let path = self.history.join(format!("{}.json", record.plan_id));
        if path.exists() {
            let old: TerminalHistory = read_json(&path, "update history")?;
            ensure!(old == *record, "terminal update history is immutable");
        } else {
            atomic_json(&path, record)?;
        }
        let entries = self.read_history()?;
        for old in entries.iter().skip(MAX_HISTORY) {
            let path = self.history.join(format!("{}.json", old.plan_id));
            reject_link(&path)?;
            fs::remove_file(path)?;
        }
        if entries.len() > MAX_HISTORY {
            sync_directory(&self.history)?;
        }
        Ok(())
    }

    fn remove_stage(&self, plan_id: &str) -> anyhow::Result<()> {
        let path = self.stage_dir(plan_id)?;
        if path.exists() {
            validate_tree(&path)?;
            fs::remove_dir_all(path)?;
            sync_directory(&self.staging)?;
        }
        Ok(())
    }

    fn clean_temps(&self) -> anyhow::Result<()> {
        for directory in [&self.home, &self.updates, &self.history] {
            for entry in fs::read_dir(directory)? {
                let entry = entry?;
                let name = entry.file_name().to_string_lossy().into_owned();
                let orphan = name.starts_with(".state.tmp-")
                    || name.starts_with(".lock.claim-")
                    || name.starts_with(".lock.stale-");
                if orphan {
                    reject_link(&entry.path())?;
                    ensure!(
                        entry.file_type()?.is_file(),
                        "atomic temporary is not a file"
                    );
                    fs::remove_file(entry.path())?;
                }
            }
        }
        Ok(())
    }
}

pub fn deterministic_plan_id<T: Serialize>(plan: &T) -> anyhow::Result<String> {
    Ok(digest(&canonical_bytes(plan)?))
}

fn validate_cache(cache: &UpdateCache) -> anyhow::Result<()> {
    match &cache.result {
        CachedCheckResult::UpdatesAvailable { plan_id, .. } => validate_id(plan_id)?,
        CachedCheckResult::Failed { code, summary } => {
            safe_text(code, "cached error code")?;
            safe_text(summary, "cached error summary")?;
        }
        CachedCheckResult::Current => {}
    }
    if let Some(notice) = &cache.last_notice {
        validate_id(&notice.plan_id)?;
    }
    ensure!(
        cache.notice_history.len() <= 32,
        "cached update notice history exceeds 32 entries"
    );
    for notice in &cache.notice_history {
        validate_id(&notice.plan_id)?;
    }
    Ok(())
}

fn validate_envelope(envelope: &PlanEnvelope<Value>) -> anyhow::Result<()> {
    schema(envelope.schema_version, "staged plan")?;
    validate_id(&envelope.plan_id)?;
    ensure!(
        deterministic_plan_id(&envelope.plan)? == envelope.plan_id,
        "staged plan digest mismatch"
    );
    Ok(())
}

fn validate_staged_plan_file(directory: &Path, plan_id: &str) -> anyhow::Result<()> {
    let envelope: PlanEnvelope<Value> = read_json(&directory.join(PLAN), "staged update plan")?;
    schema(envelope.schema_version, "staged update plan")?;
    ensure!(
        envelope.plan_id == plan_id,
        "staged plan id does not match its directory"
    );
    if envelope.plan.get("plan_id").is_some() {
        let plan: UpdatePlan = serde_json::from_value(envelope.plan)
            .context("staged resolved update plan is invalid")?;
        ensure!(
            plan.plan_id() == plan_id,
            "staged resolved plan id does not match its directory"
        );
        plan.verify_plan_id()?;
    } else {
        validate_envelope(&PlanEnvelope {
            schema_version: envelope.schema_version,
            plan_id: envelope.plan_id,
            plan: envelope.plan,
        })?;
    }
    Ok(())
}

fn validate_state(state: &StagingState, plan_id: &str) -> anyhow::Result<()> {
    schema(state.schema_version, "staged state")?;
    ensure!(state.plan_id == plan_id, "staged state plan id mismatch");
    ensure!(
        state.updated_at_unix_ms >= state.created_at_unix_ms,
        "staged timestamps are reversed"
    );
    ensure!(
        state.internal_token.len() == 64 && lower_hex(&state.internal_token),
        "invalid internal token"
    );
    if let Some(digest) = &state.finalizer_payload_sha256 {
        ensure!(
            digest.len() == 64 && lower_hex(digest),
            "invalid Windows finalizer payload digest"
        );
    }
    ensure!(
        state.phase.terminal() || (state.components.is_empty() && state.error.is_none()),
        "non-terminal state has terminal fields"
    );
    Ok(())
}

fn validate_history(record: &TerminalHistory, plan_id: &str) -> anyhow::Result<()> {
    schema(record.schema_version, "terminal history")?;
    ensure!(
        record.plan_id == plan_id,
        "terminal history plan id mismatch"
    );
    ensure!(
        record.finished_at_unix_ms >= record.started_at_unix_ms,
        "terminal timestamps are reversed"
    );
    if let Some(error) = &record.error {
        safe_text(&error.code, "terminal error code")?;
        safe_text(&error.summary, "terminal error summary")?;
    }
    Ok(())
}

fn validate_lock(record: &UpdateLockRecord) -> anyhow::Result<()> {
    schema(record.schema_version, "update lock")?;
    ensure!(record.pid > 0, "update lock PID must be positive");
    ensure!(
        record.lease_expires_at_unix_ms > record.created_at_unix_ms,
        "invalid lock lease"
    );
    ensure!(
        record.lease_expires_at_unix_ms - record.created_at_unix_ms
            <= MAX_LOCK_LEASE.as_millis() as u64,
        "lock lease exceeds 24 hours"
    );
    if let Some(id) = &record.plan_id {
        validate_id(id)?;
    }
    ensure!(
        record.owner_token.len() == 64 && lower_hex(&record.owner_token),
        "invalid lock token"
    );
    Ok(())
}

fn history_from(state: &StagingState) -> anyhow::Result<TerminalHistory> {
    let outcome = match state.phase {
        StagingPhase::Applied => TerminalOutcome::Applied,
        StagingPhase::RolledBack => TerminalOutcome::RolledBack,
        StagingPhase::Failed => TerminalOutcome::Failed,
        _ => bail!("staged update state is not terminal"),
    };
    Ok(TerminalHistory {
        schema_version: SCHEMA_VERSION,
        plan_id: state.plan_id.clone(),
        mode: state.mode,
        started_at_unix_ms: state.created_at_unix_ms,
        finished_at_unix_ms: state.updated_at_unix_ms,
        outcome,
        components: state.components.clone(),
        error: state.error.clone(),
    })
}

fn schema(version: u32, label: &str) -> anyhow::Result<()> {
    ensure!(
        version == SCHEMA_VERSION,
        "unsupported {label} schema version {version}"
    );
    Ok(())
}

fn validate_id(id: &str) -> anyhow::Result<()> {
    ensure!(
        id.len() == 64 && lower_hex(id),
        "plan id must be a lowercase SHA-256 digest"
    );
    Ok(())
}

fn lower_hex(value: &str) -> bool {
    value
        .bytes()
        .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

fn safe_text(value: &str, label: &str) -> anyhow::Result<()> {
    ensure!(!value.trim().is_empty(), "{label} must not be empty");
    ensure!(
        !value.contains(['\n', '\r']) && value.len() <= 1024,
        "{label} is not a bounded single line"
    );
    Ok(())
}

fn canonical_bytes<T: Serialize>(value: &T) -> anyhow::Result<Vec<u8>> {
    fn canonical(value: Value) -> Value {
        match value {
            Value::Array(values) => Value::Array(values.into_iter().map(canonical).collect()),
            Value::Object(values) => {
                let sorted = values
                    .into_iter()
                    .map(|(k, v)| (k, canonical(v)))
                    .collect::<BTreeMap<_, _>>();
                Value::Object(Map::from_iter(sorted))
            }
            value => value,
        }
    }
    serde_json::to_vec(&canonical(serde_json::to_value(value)?))
        .context("failed to canonicalize update plan")
}

fn digest(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn now_ms() -> anyhow::Result<u64> {
    Ok(SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("clock before Unix epoch")?
        .as_millis() as u64)
}

fn advance_state_timestamp(state: &mut StagingState) -> anyhow::Result<()> {
    state.updated_at_unix_ms = now_ms()?
        .max(state.created_at_unix_ms)
        .max(state.updated_at_unix_ms);
    Ok(())
}

fn token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow::anyhow!("random token failed: {error}"))?;
    Ok(bytes.iter().map(|b| format!("{b:02x}")).collect())
}

fn absolute(path: &Path) -> anyhow::Result<PathBuf> {
    ensure!(!path.as_os_str().is_empty(), "LDGR home must not be empty");
    let path = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    ensure!(
        path.components()
            .all(|c| !matches!(c, Component::ParentDir)),
        "LDGR home contains parent traversal"
    );
    Ok(path)
}

fn boundary(root: &Path, path: &Path) -> anyhow::Result<()> {
    ensure!(
        path.starts_with(root) && path != root,
        "update state path escapes ownership boundary"
    );
    Ok(())
}

fn secure_dir(path: &Path, label: &str) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => verify_dir(path, label),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            #[cfg(unix)]
            {
                use std::os::unix::fs::DirBuilderExt;
                fs::DirBuilder::new()
                    .mode(0o700)
                    .create(path)
                    .with_context(|| format!("failed to create {label}"))?;
            }
            #[cfg(not(unix))]
            fs::create_dir(path).with_context(|| format!("failed to create {label}"))?;
            owner_only(path)?;
            verify_dir(path, label)
        }
        Err(error) => Err(error.into()),
    }
}

fn verify_dir(path: &Path, label: &str) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        !link_or_reparse(&metadata),
        "{label} must not be a symlink or reparse point"
    );
    ensure!(metadata.is_dir(), "{label} is not a directory");
    Ok(())
}

fn reject_link(path: &Path) -> anyhow::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    ensure!(
        !link_or_reparse(&metadata),
        "update state path must not be a symlink or reparse point"
    );
    Ok(())
}

fn link_or_reparse(metadata: &fs::Metadata) -> bool {
    if metadata.file_type().is_symlink() {
        return true;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        metadata.file_attributes() & 0x400 != 0
    }
    #[cfg(not(windows))]
    false
}

fn validate_tree(root: &Path) -> anyhow::Result<()> {
    reject_link(root)?;
    let mut pending = vec![root.to_owned()];
    let mut count = 0;
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(directory)? {
            let entry = entry?;
            count += 1;
            ensure!(count <= 10_000, "staging tree exceeds recovery bound");
            reject_link(&entry.path())?;
            let kind = entry.file_type()?;
            ensure!(
                kind.is_file() || kind.is_dir(),
                "staging contains a special file"
            );
            if kind.is_dir() {
                pending.push(entry.path());
            }
        }
    }
    Ok(())
}

fn read_json<T: DeserializeOwned>(path: &Path, label: &str) -> anyhow::Result<T> {
    reject_link(path)?;
    let metadata = fs::metadata(path)?;
    ensure!(
        metadata.is_file() && metadata.len() <= MAX_STATE_BYTES,
        "{label} is not a bounded regular file"
    );
    let mut bytes = Vec::with_capacity(metadata.len() as usize);
    Read::by_ref(&mut File::open(path)?)
        .take(MAX_STATE_BYTES + 1)
        .read_to_end(&mut bytes)?;
    ensure!(
        bytes.len() as u64 <= MAX_STATE_BYTES,
        "{label} exceeds 1 MiB"
    );
    serde_json::from_slice(&bytes).with_context(|| format!("failed to parse {label}"))
}

pub(crate) fn atomic_json<T: Serialize>(path: &Path, value: &T) -> anyhow::Result<()> {
    atomic_json_fault(path, value, WriteFault::None)
}

fn atomic_json_fault<T: Serialize>(
    path: &Path,
    value: &T,
    fault: WriteFault,
) -> anyhow::Result<()> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    ensure!(
        bytes.len() as u64 <= MAX_STATE_BYTES,
        "update state exceeds 1 MiB"
    );
    let parent = path.parent().context("state path has no parent")?;
    verify_dir(parent, "update state parent")?;
    if path.exists() {
        reject_link(path)?;
    }
    let temporary = parent.join(format!(".state.tmp-{}-{}", std::process::id(), token()?));
    let result = (|| {
        let mut file = private_file(&temporary)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        if fault == WriteFault::AfterTempSync {
            bail!("injected interruption after temporary fsync");
        }
        drop(file);
        atomic_replace(&temporary, path)?;
        sync_parent(path)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn private_file(path: &Path) -> anyhow::Result<File> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    owner_only(path)?;
    Ok(file)
}

fn create_lock(path: &Path, record: &UpdateLockRecord) -> anyhow::Result<()> {
    if path.exists() {
        reject_link(path)?;
        return Err(std::io::Error::from(ErrorKind::AlreadyExists).into());
    }
    let temporary = path.with_file_name(format!(".lock.claim-{}-{}", std::process::id(), token()?));
    let result = (|| {
        let mut bytes = serde_json::to_vec_pretty(record)?;
        bytes.push(b'\n');
        let mut file = private_file(&temporary)?;
        file.write_all(&bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        fs::hard_link(&temporary, path).map_err(|error| {
            if path.exists() {
                std::io::Error::from(ErrorKind::AlreadyExists)
            } else {
                error
            }
        })?;
        sync_parent(path)
    })();
    let _ = fs::remove_file(temporary);
    result
}

fn reclaim_lock(path: &Path, expected: &str) -> anyhow::Result<()> {
    reject_link(path)?;
    let stale = path.with_file_name(format!(".lock.stale-{}-{}", std::process::id(), token()?));
    match fs::rename(path, &stale) {
        Ok(()) => {
            let moved: UpdateLockRecord = read_json(&stale, "stale update lock")?;
            if moved.owner_token != expected {
                if !path.exists() {
                    fs::rename(&stale, path)?;
                }
                bail!("lock ownership changed during reclamation");
            }
            fs::remove_file(stale)?;
            sync_parent(path)
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn lock_stale(record: &UpdateLockRecord, now: u64) -> bool {
    if now >= record.lease_expires_at_unix_ms {
        return true;
    }
    match process_identity(record.pid) {
        ProcessIdentity::Gone => true,
        ProcessIdentity::Running(actual) => matches!(
            (&record.process_start_identity, actual),
            (Some(expected), Some(actual)) if expected != &actual
        ),
        ProcessIdentity::Unavailable => false,
    }
}

fn process_identity(pid: u32) -> ProcessIdentity {
    match process_start(pid) {
        Some(identity) => ProcessIdentity::Running(Some(identity)),
        None if process_gone(pid) => ProcessIdentity::Gone,
        None => ProcessIdentity::Unavailable,
    }
}

#[cfg(target_os = "linux")]
fn process_start(pid: u32) -> Option<String> {
    let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    let end = stat.rfind(')')?;
    stat.get(end + 2..)?
        .split_whitespace()
        .nth(19)
        .map(|v| format!("linux-proc:{v}"))
}

#[cfg(target_os = "linux")]
fn process_gone(pid: u32) -> bool {
    !Path::new(&format!("/proc/{pid}")).exists()
}

#[cfg(windows)]
fn process_start(pid: u32) -> Option<String> {
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
    };
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut creation = FILETIME::default();
        let mut exit = FILETIME::default();
        let mut kernel = FILETIME::default();
        let mut user = FILETIME::default();
        let ok = GetProcessTimes(process, &mut creation, &mut exit, &mut kernel, &mut user) != 0;
        CloseHandle(process);
        ok.then(|| {
            format!(
                "windows-filetime:{}",
                ((creation.dwHighDateTime as u64) << 32) | creation.dwLowDateTime as u64
            )
        })
    }
}

#[cfg(windows)]
fn process_gone(pid: u32) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, ERROR_INVALID_PARAMETER};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if !process.is_null() {
            CloseHandle(process);
            false
        } else {
            GetLastError() == ERROR_INVALID_PARAMETER
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_start(_pid: u32) -> Option<String> {
    None
}

#[cfg(not(any(target_os = "linux", windows)))]
fn process_gone(pid: u32) -> bool {
    unsafe {
        libc::kill(pid as libc::pid_t, 0) != 0
            && std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH)
    }
}

fn history_id(path: &Path) -> Option<String> {
    let id = path.file_name()?.to_str()?.strip_suffix(".json")?;
    validate_id(id).ok()?;
    Some(id.to_owned())
}

fn not_found(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == ErrorKind::NotFound)
    })
}

fn already_exists(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|e| e.kind() == ErrorKind::AlreadyExists)
    })
}

#[cfg(not(windows))]
fn atomic_replace(source: &Path, destination: &Path) -> anyhow::Result<()> {
    fs::rename(source, destination).context("failed to atomically replace update state")
}

#[cfg(windows)]
fn atomic_replace(source: &Path, destination: &Path) -> anyhow::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{
        MoveFileExW, MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(Some(0))
        .collect::<Vec<_>>();
    let ok = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    ensure!(
        ok != 0,
        "failed to atomically replace update state: {}",
        std::io::Error::last_os_error()
    );
    Ok(())
}

#[cfg(unix)]
fn owner_only(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    let mode = if fs::symlink_metadata(path)?.is_dir() {
        0o700
    } else {
        0o600
    };
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

#[cfg(windows)]
fn owner_only(path: &Path) -> anyhow::Result<()> {
    use std::mem::{size_of, zeroed};
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Foundation::{
        CloseHandle, GetLastError, ERROR_ACCESS_DENIED, ERROR_INVALID_FUNCTION,
        ERROR_NOT_SUPPORTED, GENERIC_ALL,
    };
    use windows_sys::Win32::Security::{
        AddAccessAllowedAce, GetLengthSid, GetTokenInformation, InitializeAcl,
        InitializeSecurityDescriptor, SetFileSecurityW, SetSecurityDescriptorDacl, TokenUser, ACL,
        ACL_REVISION, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
        SECURITY_DESCRIPTOR, TOKEN_QUERY, TOKEN_USER,
    };
    use windows_sys::Win32::System::SystemServices::SECURITY_DESCRIPTOR_REVISION;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut token_handle = std::ptr::null_mut();
        ensure!(
            OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut token_handle) != 0,
            "failed to open ACL token"
        );
        let mut needed = 0;
        GetTokenInformation(
            token_handle,
            TokenUser,
            std::ptr::null_mut(),
            0,
            &mut needed,
        );
        let mut token_data = vec![0_u64; (needed as usize + 7) / 8];
        ensure!(
            GetTokenInformation(
                token_handle,
                TokenUser,
                token_data.as_mut_ptr().cast(),
                needed,
                &mut needed
            ) != 0,
            "failed to read ACL token"
        );
        let user = &*token_data.as_ptr().cast::<TOKEN_USER>();
        let acl_len = size_of::<ACL>()
            + size_of::<windows_sys::Win32::Security::ACCESS_ALLOWED_ACE>()
            + GetLengthSid(user.User.Sid) as usize
            - size_of::<u32>();
        let mut acl_data = vec![0_u32; (acl_len + 3) / 4];
        let acl = acl_data.as_mut_ptr().cast::<ACL>();
        let mut descriptor: SECURITY_DESCRIPTOR = zeroed();
        let ok = InitializeAcl(acl, acl_len as u32, ACL_REVISION) != 0
            && AddAccessAllowedAce(acl, ACL_REVISION, GENERIC_ALL, user.User.Sid) != 0
            && InitializeSecurityDescriptor(
                (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                SECURITY_DESCRIPTOR_REVISION,
            ) != 0
            && SetSecurityDescriptorDacl(
                (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(),
                1,
                acl,
                0,
            ) != 0;
        CloseHandle(token_handle);
        ensure!(ok, "failed to build owner-only ACL");
        let wide = path
            .as_os_str()
            .encode_wide()
            .chain(Some(0))
            .collect::<Vec<_>>();
        if SetFileSecurityW(
            wide.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            (&mut descriptor as *mut SECURITY_DESCRIPTOR).cast(),
        ) == 0
        {
            let code = GetLastError();
            if code != ERROR_NOT_SUPPORTED
                && code != ERROR_INVALID_FUNCTION
                && code != ERROR_ACCESS_DENIED
            {
                bail!(
                    "failed to apply owner-only ACL: {}",
                    std::io::Error::from_raw_os_error(code as i32)
                );
            }
        }
    }
    Ok(())
}

#[cfg(not(any(unix, windows)))]
fn owner_only(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> anyhow::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn sync_parent(path: &Path) -> anyhow::Result<()> {
    sync_directory(path.parent().context("state path has no parent")?)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Barrier};
    use std::thread;

    use super::*;

    fn fixture() -> anyhow::Result<(tempfile::TempDir, UpdateStateStore)> {
        let directory = tempfile::tempdir()?;
        let store = UpdateStateStore::open(directory.path().join(".ldgr"))?;
        Ok((directory, store))
    }

    fn plan(seed: u64) -> Value {
        serde_json::json!({"components": [{"name": format!("adapter-{seed}")}, {"name": "core"}], "channel": "stable"})
    }

    #[test]
    fn plan_ids_are_canonical_and_path_safe() -> anyhow::Result<()> {
        let a = serde_json::json!({"z": [{"b": 2, "a": 1}], "a": true});
        let b = serde_json::json!({"a": true, "z": [{"a": 1, "b": 2}]});
        assert_eq!(deterministic_plan_id(&a)?, deterministic_plan_id(&b)?);
        assert_eq!(deterministic_plan_id(&a)?.len(), 64);
        assert!(validate_id("../escape").is_err());
        Ok(())
    }

    #[test]
    fn atomic_cache_fault_preserves_previous_version() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let old = UpdateCache {
            schema_version: SCHEMA_VERSION,
            checked_at_unix_ms: 10,
            result: CachedCheckResult::Current,
            catalog_etag: Some("v1".to_owned()),
            consecutive_failures: 0,
            last_notice: None,
            notice_history: Vec::new(),
        };
        store.write_cache(&old)?;
        let new = UpdateCache {
            checked_at_unix_ms: 20,
            ..old.clone()
        };
        assert!(
            atomic_json_fault(&store.home.join(CACHE), &new, WriteFault::AfterTempSync).is_err()
        );
        assert_eq!(store.load_cache()?, Some(old));
        fs::write(
            store.home.join(CACHE),
            vec![b'x'; MAX_STATE_BYTES as usize + 1],
        )?;
        assert!(store
            .load_cache()
            .unwrap_err()
            .to_string()
            .contains("bounded regular file"));
        Ok(())
    }

    #[test]
    fn staging_is_idempotent_and_interrupted_apply_requires_rollback() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let plan = plan(1);
        let id = deterministic_plan_id(&plan)?;
        let lock = store.acquire_lock(UpdateMode::Apply, Some(&id), Duration::from_secs(60))?;
        assert_eq!(store.stage_plan(&lock, &plan)?, id);
        assert_eq!(store.stage_plan(&lock, &plan)?, id);
        assert_eq!(store.load_staged_plan::<Value>(&id)?.plan, plan);
        store.mark_applying(&lock, &id)?;
        lock.release()?;
        let recovery = store.acquire_lock(UpdateMode::Recover, None, Duration::from_secs(60))?;
        assert_eq!(
            store.recover_interrupted(&recovery)?,
            vec![RecoveryRecord {
                plan_id: id,
                action: RecoveryAction::RollbackRequired,
            }]
        );
        Ok(())
    }

    #[test]
    fn state_transitions_tolerate_wall_clock_rollback() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let plan = plan(23);
        let id = deterministic_plan_id(&plan)?;
        let lock = store.acquire_lock(UpdateMode::Apply, Some(&id), Duration::from_secs(60))?;
        store.stage_plan(&lock, &plan)?;

        let mut state = store.load_staging_state(&id)?;
        state.created_at_unix_ms += 60_000;
        state.updated_at_unix_ms = state.created_at_unix_ms;
        atomic_json(&store.stage_dir(&id)?.join(STATE), &state)?;

        store.mark_applying(&lock, &id)?;
        let applying = store.load_staging_state(&id)?;
        assert_eq!(applying.updated_at_unix_ms, applying.created_at_unix_ms);
        let history =
            store.complete_plan(&lock, &id, TerminalOutcome::Applied, Vec::new(), None)?;
        assert!(history.finished_at_unix_ms >= history.started_at_unix_ms);
        Ok(())
    }

    #[test]
    fn terminal_state_recovers_receipt_before_staging_cleanup() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let plan = plan(2);
        let id = deterministic_plan_id(&plan)?;
        let lock = store.acquire_lock(UpdateMode::Apply, Some(&id), Duration::from_secs(60))?;
        store.stage_plan(&lock, &plan)?;
        store.mark_applying(&lock, &id)?;
        let mut state = store.load_staging_state(&id)?;
        state.phase = StagingPhase::RolledBack;
        state.updated_at_unix_ms += 1;
        state.error = Some(TerminalError {
            code: "update.activation-failed".to_owned(),
            summary: "fixture failed".to_owned(),
        });
        atomic_json(&store.stage_dir(&id)?.join(STATE), &state)?;
        lock.release()?;
        let recovery = store.acquire_lock(UpdateMode::Recover, None, Duration::from_secs(60))?;
        assert_eq!(
            store.recover_interrupted(&recovery)?[0].action,
            RecoveryAction::TerminalReceiptRecovered
        );
        assert!(!store.stage_dir(&id)?.exists());
        assert_eq!(
            store.read_history()?[0].outcome,
            TerminalOutcome::RolledBack
        );
        Ok(())
    }

    #[test]
    fn deferred_terminal_report_is_durable_and_one_shot() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let plan = plan(22);
        let lock = store.acquire_lock(UpdateMode::Apply, None, Duration::from_secs(60))?;
        let id = store.stage_plan(&lock, &plan)?;
        store.mark_applying(&lock, &id)?;
        let history = store.complete_plan_deferred_cleanup(
            &lock,
            &id,
            TerminalOutcome::RolledBack,
            vec![ComponentResult {
                kind: "core_bundle".to_owned(),
                name: "core".to_owned(),
                status: "rolled_back".to_owned(),
            }],
            Some(TerminalError {
                code: "update.activation-failed".to_owned(),
                summary: "injected failure".to_owned(),
            }),
        )?;
        store.write_pending_report(&history)?;
        let report = store.take_pending_report()?.expect("pending report");
        assert_eq!(report.plan_id, id);
        assert_eq!(report.outcome, TerminalOutcome::RolledBack);
        assert!(store.take_pending_report()?.is_none());
        lock.release()?;
        Ok(())
    }

    #[test]
    fn recovery_discards_incomplete_staging_and_orphaned_claim_files() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let id = deterministic_plan_id(&plan(99))?;
        secure_dir(&store.stage_dir(&id)?, "fixture incomplete staging")?;
        let orphan = store.updates.join(".lock.claim-orphan");
        fs::write(&orphan, b"interrupted claim")?;
        let lock = store.acquire_lock(UpdateMode::Recover, None, Duration::from_secs(60))?;
        assert_eq!(
            store.recover_interrupted(&lock)?,
            vec![RecoveryRecord {
                plan_id: id.clone(),
                action: RecoveryAction::IncompleteStagingDiscarded,
            }]
        );
        assert!(!store.stage_dir(&id)?.exists());
        assert!(!orphan.exists());
        assert!(
            lock.record.process_start_identity.is_some()
                || cfg!(not(any(windows, target_os = "linux")))
        );
        Ok(())
    }

    #[test]
    fn terminal_history_is_bounded() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        for seed in 0..(MAX_HISTORY + 4) as u64 {
            let plan = plan(seed);
            let id = deterministic_plan_id(&plan)?;
            let lock = store.acquire_lock(UpdateMode::Apply, Some(&id), Duration::from_secs(60))?;
            store.stage_plan(&lock, &plan)?;
            store.complete_plan(&lock, &id, TerminalOutcome::Applied, Vec::new(), None)?;
            lock.release()?;
        }
        assert_eq!(store.read_history()?.len(), MAX_HISTORY);
        Ok(())
    }

    #[test]
    fn exactly_one_concurrent_writer_acquires_lock() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let store = Arc::new(store);
        let barrier = Arc::new(Barrier::new(9));
        let mut workers = Vec::new();
        for _ in 0..8 {
            let store = Arc::clone(&store);
            let barrier = Arc::clone(&barrier);
            workers.push(thread::spawn(move || {
                barrier.wait();
                let lock = store.acquire_lock(UpdateMode::Check, None, Duration::from_secs(60));
                barrier.wait();
                lock.is_ok()
            }));
        }
        barrier.wait();
        barrier.wait();
        assert_eq!(
            workers
                .into_iter()
                .map(|w| w.join().unwrap())
                .filter(|won| *won)
                .count(),
            1
        );
        Ok(())
    }

    #[test]
    fn live_lock_blocks_and_expired_lease_is_reclaimed() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let now = now_ms()?;
        let lock = store.acquire_lock_at(UpdateMode::Check, None, Duration::from_secs(60), now)?;
        assert!(store
            .acquire_lock_at(UpdateMode::Apply, None, Duration::from_secs(60), now + 1)
            .unwrap_err()
            .downcast_ref::<UpdateStateError>()
            .is_some());
        std::mem::forget(lock);
        let next = store.acquire_lock_at(
            UpdateMode::Recover,
            None,
            Duration::from_secs(60),
            now + 61_000,
        )?;
        assert_eq!(next.record.mode, UpdateMode::Recover);
        Ok(())
    }

    #[test]
    fn detached_worker_must_claim_the_handed_off_pid_and_token() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let lock = store.acquire_lock(UpdateMode::Check, None, Duration::from_secs(60))?;
        let token = lock.owner_token().to_owned();
        assert!(store.claim_handed_off_check_lock(&"f".repeat(64)).is_err());
        lock.handoff_to_pid(std::process::id())?;
        let worker = store.claim_handed_off_check_lock(&token)?;
        assert_eq!(worker.record().pid, std::process::id());
        worker.release()?;
        assert!(store.claim_handed_off_check_lock(&token).is_err());
        Ok(())
    }

    #[test]
    fn finalizer_handoff_binds_plan_pid_and_durable_token() -> anyhow::Result<()> {
        let (_directory, store) = fixture()?;
        let plan = plan(44);
        let lock = store.acquire_lock(UpdateMode::Apply, None, Duration::from_secs(60))?;
        let id = store.stage_plan(&lock, &plan)?;
        let token = store.load_staging_state(&id)?.internal_token;
        store.bind_finalizer_payload(&lock, &id, &"b".repeat(64))?;
        assert!(store.claim_handed_off_finalizer_lock(&token, &id).is_err());
        lock.handoff_to_finalizer(std::process::id(), &id, &token)?;
        assert!(store
            .claim_handed_off_finalizer_lock(&"f".repeat(64), &id)
            .is_err());
        let finalizer = store.claim_handed_off_finalizer_lock(&token, &id)?;
        assert_eq!(finalizer.record().mode, UpdateMode::Finalize);
        assert_eq!(finalizer.record().plan_id.as_deref(), Some(id.as_str()));
        finalizer.release()?;
        Ok(())
    }

    #[test]
    fn process_start_mismatch_marks_pid_reuse_stale() -> anyhow::Result<()> {
        let now = now_ms()?;
        let record = UpdateLockRecord {
            schema_version: SCHEMA_VERSION,
            pid: std::process::id(),
            process_start_identity: Some("different-start".to_owned()),
            created_at_unix_ms: now,
            lease_expires_at_unix_ms: now + 60_000,
            mode: UpdateMode::Apply,
            plan_id: None,
            owner_token: "a".repeat(64),
        };
        assert!(lock_stale(&record, now + 1));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_state_is_rejected_and_permissions_are_owner_only() -> anyhow::Result<()> {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let directory = tempfile::tempdir()?;
        let home = directory.path().join(".ldgr");
        fs::create_dir(&home)?;
        let target = directory.path().join("target");
        fs::create_dir(&target)?;
        symlink(&target, home.join("updates"))?;
        assert!(UpdateStateStore::open(&home)
            .unwrap_err()
            .to_string()
            .contains("symlink"));
        fs::remove_file(home.join("updates"))?;
        let store = UpdateStateStore::open(&home)?;
        let cache = UpdateCache {
            schema_version: SCHEMA_VERSION,
            checked_at_unix_ms: 1,
            result: CachedCheckResult::Current,
            catalog_etag: None,
            consecutive_failures: 0,
            last_notice: None,
            notice_history: Vec::new(),
        };
        store.write_cache(&cache)?;
        assert_eq!(
            fs::metadata(home.join(CACHE))?.permissions().mode() & 0o777,
            0o600
        );
        assert_eq!(
            fs::metadata(&store.updates)?.permissions().mode() & 0o777,
            0o700
        );
        Ok(())
    }
}
