use std::ffi::OsStr;
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use sha2::{Digest, Sha256};

use crate::harness_config::{
    parse_harness_config_json, parse_harness_config_toml, UpdateCheck, UpdateConfig,
};
use crate::update::state::{
    CachedCheckResult, CachedNotice, UpdateCache, UpdateLock, UpdateMode, UpdateStateError,
    UpdateStateStore,
};

pub const NO_UPDATE_CHECK_ENV: &str = "LDGR_NO_UPDATE_CHECK";
pub const CI_ENV: &str = "CI";
pub(crate) const RECURSION_GUARD_ENV: &str = "LDGR_INTERNAL_UPDATE_CHECK";

const CHECK_LOCK_LEASE: Duration = Duration::from_secs(30);
const NOTICE_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);
const FAILURE_NOTICE_THRESHOLD: u32 = 3;
const WORKER_HANDOFF_ATTEMPTS: usize = 40;
const WORKER_HANDOFF_RETRY: Duration = Duration::from_millis(25);

/// Reads bounded local update state, emits at most one cached notice, and
/// atomically schedules a due detached check. This hook is deliberately
/// best-effort: no local-state or process-spawn failure can affect the
/// foreground command.
pub fn maybe_schedule_update_check() {
    if !foreground_is_interactive() {
        return;
    }
    let _ = try_schedule_update_check();
}

fn try_schedule_update_check() -> Result<()> {
    if recursion_guarded() || process_update_check_disabled() {
        return Ok(());
    }
    let home = user_home()?;
    let config = read_update_config(&home)?;
    if !automatic_update_checks_enabled(&config) {
        return Ok(());
    }

    let store = UpdateStateStore::open(home.join(".ldgr"))?;
    let cache = store.load_cache()?;
    let lock = match store.acquire_lock(UpdateMode::Check, None, CHECK_LOCK_LEASE) {
        Ok(lock) => lock,
        Err(error) if error.downcast_ref::<UpdateStateError>().is_some() => return Ok(()),
        Err(error) => return Err(error),
    };
    let now = now_ms()?;
    let notice = claim_cached_notice(&store, cache, &config, now)?;
    let due = store
        .load_cache()?
        .as_ref()
        .is_none_or(|cache| cache_due_at(cache, config.interval_hours, now));

    if let Some(notice) = notice {
        eprintln!("{notice}");
    }
    if due {
        spawn_detached_check_worker(lock)?;
    } else {
        lock.release()?;
    }
    Ok(())
}

fn claim_cached_notice(
    store: &UpdateStateStore,
    cache: Option<UpdateCache>,
    config: &UpdateConfig,
    now: u64,
) -> Result<Option<String>> {
    if !update_notices_enabled(config) {
        return Ok(None);
    }
    let Some(mut cache) = cache else {
        return Ok(None);
    };
    let Some((notice_key, text)) = pending_notice(&cache, now) else {
        return Ok(None);
    };
    cache.last_notice = Some(CachedNotice {
        plan_id: notice_key.clone(),
        notified_at_unix_ms: now,
    });
    cache.notice_history.retain(|notice| {
        now.saturating_sub(notice.notified_at_unix_ms) < NOTICE_INTERVAL.as_millis() as u64
            && notice.plan_id != notice_key
    });
    cache.notice_history.push(CachedNotice {
        plan_id: notice_key,
        notified_at_unix_ms: now,
    });
    if cache.notice_history.len() > 32 {
        let remove = cache.notice_history.len() - 32;
        cache.notice_history.drain(..remove);
    }
    store.write_cache(&cache)?;
    Ok(Some(text))
}

fn pending_notice(cache: &UpdateCache, now: u64) -> Option<(String, String)> {
    let (key, text) = match &cache.result {
        CachedCheckResult::Current => return None,
        CachedCheckResult::UpdatesAvailable {
            plan_id,
            target_core,
            adapter_updates,
        } => {
            let adapters = match adapter_updates {
                0 => String::new(),
                1 => " and 1 adapter".to_owned(),
                count => format!(" and {count} adapters"),
            };
            (
                plan_id.clone(),
                format!("update available: ldgr {target_core}{adapters}; run `ldgr update`"),
            )
        }
        CachedCheckResult::Failed { code, .. }
            if cache.consecutive_failures >= FAILURE_NOTICE_THRESHOLD =>
        {
            let key = hex_digest(format!("failure:{code}").as_bytes());
            (
                key,
                format!(
                    "warning: automatic update checks failed repeatedly ({code}); run `ldgr update --check`"
                ),
            )
        }
        CachedCheckResult::Failed { .. } => return None,
    };
    let recently_notified = cache
        .last_notice
        .iter()
        .chain(&cache.notice_history)
        .any(|notice| {
            notice.plan_id == key
                && now.saturating_sub(notice.notified_at_unix_ms)
                    < NOTICE_INTERVAL.as_millis() as u64
        });
    if recently_notified {
        return None;
    }
    Some((key, text))
}

fn cache_due_at(cache: &UpdateCache, interval_hours: u64, now: u64) -> bool {
    let interval_ms = interval_hours.saturating_mul(60 * 60 * 1_000);
    now >= cache.checked_at_unix_ms.saturating_add(interval_ms)
}

fn spawn_detached_check_worker(lock: UpdateLock) -> Result<()> {
    let executable =
        std::env::current_exe().context("failed to resolve current ldgr executable")?;
    let token = lock.owner_token().to_owned();
    let mut command = Command::new(&executable);
    command
        .arg("__update-check-worker")
        .arg("--token")
        .arg(&token)
        .env(RECURSION_GUARD_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::loop_runtime::configure_child_home(&mut command);
    configure_detached_process(&mut command);
    let child = command
        .spawn()
        .with_context(|| format!("failed to start update worker via {}", executable.display()))?;
    lock.handoff_to_pid(child.id())
}

pub(crate) fn startup_worker_context(
    owner_token: &str,
) -> Result<(PathBuf, UpdateConfig, UpdateStateStore, UpdateLock)> {
    let home = user_home()?;
    let config = read_update_config(&home)?;
    let store = UpdateStateStore::open(home.join(".ldgr"))?;
    let mut last_error = None;
    for _ in 0..WORKER_HANDOFF_ATTEMPTS {
        match store.claim_handed_off_check_lock(owner_token) {
            Ok(lock) => return Ok((home, config, store, lock)),
            Err(error) => {
                last_error = Some(error);
                thread::sleep(WORKER_HANDOFF_RETRY);
            }
        }
    }
    Err(last_error.context("update worker lock handoff was not completed")?)
}

fn read_update_config(home: &Path) -> Result<UpdateConfig> {
    let ldgr_home = home.join(".ldgr");
    let toml_path = ldgr_home.join("config.toml");
    if toml_path.is_file() {
        let text = fs::read_to_string(&toml_path)
            .with_context(|| format!("failed to read {}", toml_path.display()))?;
        return Ok(parse_harness_config_toml(&text)?.updates);
    }
    let json_path = ldgr_home.join("config.json");
    if json_path.is_file() {
        let text = fs::read_to_string(&json_path)
            .with_context(|| format!("failed to read {}", json_path.display()))?;
        return Ok(parse_harness_config_json(&text)?.updates);
    }
    Ok(UpdateConfig::default())
}

fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("could not determine home directory from HOME/USERPROFILE")
}

fn now_ms() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis()
        .try_into()
        .context("update startup timestamp overflow")
}

fn foreground_is_interactive() -> bool {
    std::io::stdin().is_terminal() && std::io::stderr().is_terminal()
}

fn recursion_guarded() -> bool {
    std::env::var_os(RECURSION_GUARD_ENV).is_some()
}

fn hex_digest(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    digest.iter().map(|byte| format!("{byte:02x}")).collect()
}

#[cfg(windows)]
fn configure_detached_process(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const DETACHED_PROCESS: u32 = 0x0000_0008;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(DETACHED_PROCESS | CREATE_NEW_PROCESS_GROUP);
}

#[cfg(unix)]
fn configure_detached_process(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(all(not(unix), not(windows)))]
fn configure_detached_process(_command: &mut Command) {}

/// Returns whether this process opted out of automatic update discovery.
/// Explicit update commands do not consult this startup-only override.
pub fn process_update_check_disabled() -> bool {
    no_update_check_value_disables(std::env::var_os(NO_UPDATE_CHECK_ENV).as_deref())
}

/// Resolves persisted startup policy with the immediate process override.
pub fn automatic_update_checks_enabled(config: &UpdateConfig) -> bool {
    automatic_update_checks_enabled_for(config, std::env::var_os(NO_UPDATE_CHECK_ENV).as_deref())
}

/// Returns whether update notices should be hidden for this process.
/// CI only suppresses notices; it does not disable explicit update checks.
pub fn update_notices_suppressed_by_ci() -> bool {
    ci_value_suppresses(std::env::var_os(CI_ENV).as_deref())
}

/// Applies both the persisted notification preference and the process CI guard.
pub fn update_notices_enabled(config: &UpdateConfig) -> bool {
    update_notices_enabled_for(config, std::env::var_os(CI_ENV).as_deref())
}

fn automatic_update_checks_enabled_for(config: &UpdateConfig, value: Option<&OsStr>) -> bool {
    config.check == UpdateCheck::Startup && !no_update_check_value_disables(value)
}

fn update_notices_enabled_for(config: &UpdateConfig, value: Option<&OsStr>) -> bool {
    config.notify && !ci_value_suppresses(value)
}

fn no_update_check_value_disables(value: Option<&OsStr>) -> bool {
    value
        .and_then(OsStr::to_str)
        .is_some_and(|value| value.trim() == "1")
}

fn ci_value_suppresses(value: Option<&OsStr>) -> bool {
    value
        .and_then(OsStr::to_str)
        .is_some_and(|value| value.trim().eq_ignore_ascii_case("true"))
}

#[cfg(test)]
mod tests {
    use std::ffi::OsStr;

    use crate::harness_config::{UpdateCheck, UpdateConfig};
    use crate::update::state::{CachedCheckResult, CachedNotice, UpdateCache, SCHEMA_VERSION};

    use super::{
        automatic_update_checks_enabled_for, cache_due_at, ci_value_suppresses,
        no_update_check_value_disables, pending_notice, update_notices_enabled_for,
        FAILURE_NOTICE_THRESHOLD, NOTICE_INTERVAL,
    };

    fn cache(result: CachedCheckResult) -> UpdateCache {
        UpdateCache {
            schema_version: SCHEMA_VERSION,
            checked_at_unix_ms: 1_000,
            result,
            catalog_etag: None,
            consecutive_failures: 0,
            last_notice: None,
            notice_history: Vec::new(),
        }
    }

    #[test]
    fn no_update_check_accepts_only_the_documented_process_override() {
        assert!(no_update_check_value_disables(Some(OsStr::new("1"))));
        assert!(no_update_check_value_disables(Some(OsStr::new(" 1 "))));
        for value in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("0")),
            Some(OsStr::new("true")),
        ] {
            assert!(!no_update_check_value_disables(value));
        }
    }

    #[test]
    fn ci_true_suppresses_notices_without_becoming_a_general_truthy_flag() {
        assert!(ci_value_suppresses(Some(OsStr::new("true"))));
        assert!(ci_value_suppresses(Some(OsStr::new(" TRUE "))));
        for value in [
            None,
            Some(OsStr::new("")),
            Some(OsStr::new("0")),
            Some(OsStr::new("1")),
        ] {
            assert!(!ci_value_suppresses(value));
        }
    }

    #[test]
    fn process_overrides_compose_with_persisted_update_preferences() {
        let mut config = UpdateConfig::default();
        assert!(automatic_update_checks_enabled_for(&config, None));
        assert!(!automatic_update_checks_enabled_for(
            &config,
            Some(OsStr::new("1"))
        ));
        config.check = UpdateCheck::Never;
        assert!(!automatic_update_checks_enabled_for(&config, None));

        assert!(update_notices_enabled_for(&config, None));
        assert!(!update_notices_enabled_for(
            &config,
            Some(OsStr::new("true"))
        ));
        config.notify = false;
        assert!(!update_notices_enabled_for(&config, None));
    }

    #[test]
    fn cache_interval_is_saturating_and_clock_rollback_stays_throttled() {
        let cache = cache(CachedCheckResult::Current);
        assert!(!cache_due_at(&cache, 24, 1_000));
        assert!(!cache_due_at(&cache, 24, 999));
        assert!(cache_due_at(&cache, 0, 1_000));
        assert!(cache_due_at(&cache, 1, 3_601_000));
        assert!(!cache_due_at(&cache, u64::MAX, u64::MAX - 1));
        assert!(cache_due_at(&cache, u64::MAX, u64::MAX));
    }

    #[test]
    fn update_notice_is_concise_and_deduplicated_by_plan_for_24_hours() {
        let plan_id = "a".repeat(64);
        let mut cache = cache(CachedCheckResult::UpdatesAvailable {
            plan_id: plan_id.clone(),
            target_core: "0.2.0".to_owned(),
            adapter_updates: 2,
        });
        let (key, notice) = pending_notice(&cache, 10_000).expect("first notice");
        assert_eq!(key, plan_id);
        assert_eq!(
            notice,
            "update available: ldgr 0.2.0 and 2 adapters; run `ldgr update`"
        );
        cache.last_notice = Some(CachedNotice {
            plan_id: key,
            notified_at_unix_ms: 10_000,
        });
        assert!(pending_notice(&cache, 10_001).is_none());
        assert!(pending_notice(&cache, 10_000 + NOTICE_INTERVAL.as_millis() as u64).is_some());
    }

    #[test]
    fn failures_warn_only_after_repetition_and_are_time_deduplicated() {
        let mut cache = cache(CachedCheckResult::Failed {
            code: "update.catalog-unavailable".to_owned(),
            summary: "catalog timed out".to_owned(),
        });
        cache.consecutive_failures = FAILURE_NOTICE_THRESHOLD - 1;
        assert!(pending_notice(&cache, 20_000).is_none());
        cache.consecutive_failures = FAILURE_NOTICE_THRESHOLD;
        let (key, notice) = pending_notice(&cache, 20_000).expect("repeated failure warning");
        assert!(notice.contains("update.catalog-unavailable"));
        cache.last_notice = Some(CachedNotice {
            plan_id: key,
            notified_at_unix_ms: 20_000,
        });
        assert!(pending_notice(&cache, 20_001).is_none());
    }

    #[test]
    fn plan_digest_history_prevents_alternating_plan_duplicates() {
        let plan_a = "a".repeat(64);
        let plan_b = "b".repeat(64);
        let mut cache = cache(CachedCheckResult::UpdatesAvailable {
            plan_id: plan_a.clone(),
            target_core: "0.2.0".to_owned(),
            adapter_updates: 0,
        });
        cache.last_notice = Some(CachedNotice {
            plan_id: plan_b,
            notified_at_unix_ms: 30_000,
        });
        cache.notice_history.push(CachedNotice {
            plan_id: plan_a,
            notified_at_unix_ms: 29_000,
        });
        assert!(pending_notice(&cache, 31_000).is_none());
    }
}
