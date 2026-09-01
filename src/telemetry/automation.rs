use std::fs::{self, File, OpenOptions};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::Duration;

use anyhow::Context;
use fs2::FileExt;

use super::command_experience::{construction_store_path, release_eligible_constructions};
use super::donation::{experience_donation_is_eligible, EXPERIENCE_DONATION_PENDING_DIRECTORY};
use super::transmission::TransmissionClient;
use super::{
    anonymous_collection_is_eligible, DEFAULT_TELEMETRY_COLLECTOR_ORIGIN,
    RELEASED_NUMERICAL_PROTOCOLS_V1, TELEMETRY_PENDING_DIRECTORY,
};

pub const INTERNAL_FLUSH_ENV: &str = "LDGR_INTERNAL_TELEMETRY_FLUSH";
pub const NO_AUTOMATIC_TELEMETRY_ENV: &str = "LDGR_NO_AUTOMATIC_TELEMETRY";
pub const AUTOMATIC_ROOT_CA_ENV: &str = "LDGR_AUTOMATIC_TELEMETRY_ROOT_CA_PEM";
pub const AUTOMATIC_MAX_DELAY_ENV: &str = "LDGR_AUTOMATIC_TELEMETRY_MAX_DELAY_MS";
pub const AUTOMATIC_TIMEOUT_ENV: &str = "LDGR_AUTOMATIC_TELEMETRY_TIMEOUT_MS";
const WORKER_LOCK_FILE: &str = "telemetry-flush.lock";

/// Schedules delivery at process startup and again after a normal process exit.
///
/// Pending sequence files are the durable recovery marker. If the process ends
/// before this guard is dropped, the next CLI process sees those files and
/// schedules the same detached worker during startup.
pub struct AutomaticTelemetrySession {
    ldgr_home: Option<PathBuf>,
}

impl AutomaticTelemetrySession {
    pub fn start() -> Self {
        if std::env::var_os(INTERNAL_FLUSH_ENV).is_some()
            || std::env::var_os(NO_AUTOMATIC_TELEMETRY_ENV).is_some()
        {
            return Self { ldgr_home: None };
        }
        let ldgr_home = user_ldgr_home();
        if let Some(home) = &ldgr_home {
            maybe_schedule_flush(home);
        }
        Self { ldgr_home }
    }
}

impl Drop for AutomaticTelemetrySession {
    fn drop(&mut self) {
        if let Some(home) = &self.ldgr_home {
            maybe_schedule_flush(home);
        }
    }
}

/// Runs the hidden detached worker. Delivery remains best-effort, and the
/// existing transmission client retains every request that is not accepted.
pub fn run_flush_worker() -> anyhow::Result<()> {
    if std::env::var_os(NO_AUTOMATIC_TELEMETRY_ENV).is_some() {
        return Ok(());
    }
    let Some(ldgr_home) = user_ldgr_home() else {
        return Ok(());
    };
    if !any_collection_is_eligible(&ldgr_home) {
        return Ok(());
    }

    let Some(_lock) = acquire_delivery_lock(&ldgr_home)? else {
        return Ok(());
    };
    if !any_collection_is_eligible(&ldgr_home) {
        return Ok(());
    }

    if anonymous_collection_is_eligible(&ldgr_home) {
        let _ = release_eligible_constructions(&ldgr_home);
    }
    let collector = std::env::var("LDGR_TELEMETRY_COLLECTOR")
        .unwrap_or_else(|_| DEFAULT_TELEMETRY_COLLECTOR_ORIGIN.to_owned());
    let mut client = TransmissionClient::new(&collector)?;
    if let Some(max_delay) = duration_from_millis_env(AUTOMATIC_MAX_DELAY_ENV) {
        client = client.with_max_delay(max_delay);
    }
    if let Some(timeout) = duration_from_millis_env(AUTOMATIC_TIMEOUT_ENV) {
        client = client.with_timeout(timeout);
    }
    if let Some(path) = std::env::var_os(AUTOMATIC_ROOT_CA_ENV) {
        let path = PathBuf::from(path);
        let certificate = fs::read(&path)
            .with_context(|| format!("failed to read root CA PEM {}", path.display()))?;
        client = client
            .with_root_certificate_pem(&certificate)
            .with_context(|| format!("failed to parse root CA PEM {}", path.display()))?;
    }
    if anonymous_collection_is_eligible(&ldgr_home) {
        for protocol in RELEASED_NUMERICAL_PROTOCOLS_V1 {
            let _ = client.transmit_pending(&ldgr_home, protocol);
        }
    }
    if experience_donation_is_eligible(&ldgr_home) {
        let _ = client.transmit_pending_donations(&ldgr_home);
    }
    Ok(())
}

fn any_collection_is_eligible(ldgr_home: &Path) -> bool {
    anonymous_collection_is_eligible(ldgr_home) || experience_donation_is_eligible(ldgr_home)
}

fn maybe_schedule_flush(ldgr_home: &Path) {
    if !any_collection_is_eligible(ldgr_home) || !has_delivery_work(ldgr_home) {
        return;
    }
    let _ = spawn_detached_worker();
}

fn has_delivery_work(ldgr_home: &Path) -> bool {
    if anonymous_collection_is_eligible(ldgr_home) {
        let pending = ldgr_home.join(TELEMETRY_PENDING_DIRECTORY);
        if directory_tree_has_regular_file(&pending)
            || path_is_regular_file(&construction_store_path(ldgr_home))
        {
            return true;
        }
    }
    experience_donation_is_eligible(ldgr_home)
        && directory_tree_has_regular_file(&ldgr_home.join(EXPERIENCE_DONATION_PENDING_DIRECTORY))
}

fn directory_tree_has_regular_file(root: &Path) -> bool {
    let Ok(metadata) = fs::symlink_metadata(root) else {
        return false;
    };
    if !metadata.file_type().is_dir() || metadata.file_type().is_symlink() {
        return false;
    }
    let Ok(entries) = fs::read_dir(root) else {
        return false;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let Ok(metadata) = fs::symlink_metadata(&path) else {
            continue;
        };
        if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
            return true;
        }
        if metadata.file_type().is_dir()
            && !metadata.file_type().is_symlink()
            && directory_tree_has_regular_file(&path)
        {
            return true;
        }
    }
    false
}

fn path_is_regular_file(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .is_ok_and(|metadata| metadata.file_type().is_file() && !metadata.file_type().is_symlink())
}

fn spawn_detached_worker() -> anyhow::Result<()> {
    let executable =
        std::env::current_exe().context("failed to resolve current ldgr executable")?;
    let mut command = Command::new(executable);
    command
        .arg("__telemetry-flush-worker")
        .env(INTERNAL_FLUSH_ENV, "1")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    crate::loop_runtime::configure_child_home(&mut command);
    configure_detached_process(&mut command);
    command
        .spawn()
        .context("failed to start detached telemetry flush worker")?;
    Ok(())
}

pub(crate) fn acquire_delivery_lock(ldgr_home: &Path) -> anyhow::Result<Option<File>> {
    fs::create_dir_all(ldgr_home)
        .with_context(|| format!("failed to create LDGR home {}", ldgr_home.display()))?;
    let path = ldgr_home.join(WORKER_LOCK_FILE);
    if fs::symlink_metadata(&path)
        .is_ok_and(|metadata| !metadata.file_type().is_file() || metadata.file_type().is_symlink())
    {
        anyhow::bail!(
            "telemetry worker lock path is not a real file: {}",
            path.display()
        );
    }
    let mut options = OpenOptions::new();
    options.create(true).read(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;

        options.mode(0o600).custom_flags(libc::O_NOFOLLOW);
    }
    let file = options
        .open(&path)
        .with_context(|| format!("failed to open telemetry worker lock {}", path.display()))?;
    restrict_lock_permissions(&path)?;
    match FileExt::try_lock_exclusive(&file) {
        Ok(()) => Ok(Some(file)),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => Ok(None),
        Err(error) => Err(error)
            .with_context(|| format!("failed to lock telemetry worker state {}", path.display())),
    }
}

#[cfg(unix)]
fn restrict_lock_permissions(path: &Path) -> anyhow::Result<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o600)).with_context(|| {
        format!(
            "failed to restrict telemetry worker lock {}",
            path.display()
        )
    })
}

#[cfg(not(unix))]
fn restrict_lock_permissions(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

fn duration_from_millis_env(name: &str) -> Option<Duration> {
    std::env::var(name)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .map(Duration::from_millis)
}

fn user_ldgr_home() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .map(|home| home.join(".ldgr"))
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

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    #[test]
    fn pending_files_and_construction_state_request_delivery() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        assert!(!has_delivery_work(home.path()));

        let route = home.path().join("telemetry-pending/core-work/v1");
        fs::create_dir_all(&route)?;
        fs::write(route.join("sequence"), "[0,1,4]")?;
        assert!(has_delivery_work(home.path()));

        fs::remove_dir_all(home.path().join("telemetry-pending"))?;
        assert!(!has_delivery_work(home.path()));
        fs::write(construction_store_path(home.path()), "{}")?;
        assert!(has_delivery_work(home.path()));
        fs::remove_file(construction_store_path(home.path()))?;

        crate::telemetry::save_telemetry_consent(
            home.path(),
            &crate::telemetry::TelemetryConsent::current(
                crate::telemetry::TelemetryConsentDecision::Disabled,
            )
            .with_donation(crate::telemetry::TelemetryConsentDecision::Enabled),
        )?;
        let donation = home
            .path()
            .join(EXPERIENCE_DONATION_PENDING_DIRECTORY)
            .join("experiences/v1");
        fs::create_dir_all(&donation)?;
        fs::write(donation.join("episode"), "{}")?;
        assert!(has_delivery_work(home.path()));
        Ok(())
    }

    #[test]
    fn only_one_automatic_worker_holds_the_delivery_lock() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let first = acquire_delivery_lock(home.path())?.expect("first worker lock");
        assert!(acquire_delivery_lock(home.path())?.is_none());
        drop(first);
        assert!(acquire_delivery_lock(home.path())?.is_some());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_not_delivery_work_or_worker_locks() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let home = tempfile::tempdir()?;
        let external = tempfile::tempdir()?;
        fs::write(external.path().join("sequence"), "[0,1,4]")?;
        symlink(external.path(), home.path().join("telemetry-pending"))?;
        assert!(!has_delivery_work(home.path()));

        symlink(
            external.path().join("sequence"),
            home.path().join(WORKER_LOCK_FILE),
        )?;
        assert!(acquire_delivery_lock(home.path()).is_err());
        Ok(())
    }
}
