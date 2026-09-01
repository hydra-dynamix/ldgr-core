use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub mod adapter_conformance;
pub mod automation;
pub mod buffer;
pub mod command_experience;
pub mod donation;
pub mod serializer;
pub mod transition;
pub mod transmission;

pub const TELEMETRY_CONSENT_SCHEMA_VERSION: u32 = 1;
pub const TELEMETRY_CONSENT_POLICY_VERSION: u32 = 2;
pub const TELEMETRY_CONSENT_FILE: &str = "telemetry-consent.json";
pub const TELEMETRY_PENDING_DIRECTORY: &str = "telemetry-pending";
pub const NUMERICAL_SEQUENCE_PROTOCOLS_V1: &[&str] = &[
    "core-work/v1",
    "research-workflow/v1",
    "command-experience/v1",
];
pub const DEFAULT_TELEMETRY_COLLECTOR_ORIGIN: &str = "https://ldgr.run";
pub const RELEASED_NUMERICAL_PROTOCOLS_V1: &[&transition::NumericalProtocol] = &[
    &transition::CORE_WORK_V1,
    &transition::RESEARCH_WORKFLOW_V1,
    &command_experience::COMMAND_EXPERIENCE_V1,
];

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryConsentDecision {
    Undecided,
    Enabled,
    Disabled,
}

impl TelemetryConsentDecision {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Undecided => "undecided",
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct TelemetryConsent {
    pub schema_version: u32,
    pub policy_version: u32,
    pub decision: TelemetryConsentDecision,
    #[serde(default = "default_donation_decision")]
    pub donation_decision: TelemetryConsentDecision,
}

const fn default_donation_decision() -> TelemetryConsentDecision {
    TelemetryConsentDecision::Disabled
}

impl Default for TelemetryConsent {
    fn default() -> Self {
        Self::current(TelemetryConsentDecision::Enabled)
    }
}

impl TelemetryConsent {
    pub fn current(decision: TelemetryConsentDecision) -> Self {
        Self {
            schema_version: TELEMETRY_CONSENT_SCHEMA_VERSION,
            policy_version: TELEMETRY_CONSENT_POLICY_VERSION,
            decision,
            donation_decision: TelemetryConsentDecision::Disabled,
        }
    }

    pub fn collection_enabled(&self) -> bool {
        self.schema_version == TELEMETRY_CONSENT_SCHEMA_VERSION
            && self.policy_version == TELEMETRY_CONSENT_POLICY_VERSION
            && self.decision == TelemetryConsentDecision::Enabled
    }

    pub fn donation_enabled(&self) -> bool {
        self.schema_version == TELEMETRY_CONSENT_SCHEMA_VERSION
            && self.policy_version == TELEMETRY_CONSENT_POLICY_VERSION
            && self.donation_decision == TelemetryConsentDecision::Enabled
    }

    pub fn with_donation(mut self, decision: TelemetryConsentDecision) -> Self {
        self.donation_decision = decision;
        self
    }

    fn validate(&self) -> anyhow::Result<()> {
        if self.schema_version != TELEMETRY_CONSENT_SCHEMA_VERSION {
            bail!(
                "unsupported telemetry consent schema_version {}; expected {}",
                self.schema_version,
                TELEMETRY_CONSENT_SCHEMA_VERSION
            );
        }
        if self.policy_version == 0 {
            bail!("telemetry consent policy_version must be greater than zero");
        }
        Ok(())
    }
}

pub fn telemetry_consent_path(ldgr_home: &Path) -> PathBuf {
    ldgr_home.join(TELEMETRY_CONSENT_FILE)
}

pub fn load_telemetry_consent(ldgr_home: &Path) -> anyhow::Result<TelemetryConsent> {
    let path = telemetry_consent_path(ldgr_home);
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(TelemetryConsent::default());
        }
        Err(error) => {
            return Err(error)
                .with_context(|| format!("failed to read telemetry consent {}", path.display()));
        }
    };
    let mut consent: TelemetryConsent = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse telemetry consent {}", path.display()))?;
    consent.validate()?;
    if consent.policy_version == 1 {
        // Policy v1 required an explicit choice. Preserve explicit disables and
        // enables, while treating its never-chosen state as the v2 opt-out
        // default. Donation remains disabled unless separately enabled later.
        if consent.decision == TelemetryConsentDecision::Undecided {
            consent.decision = TelemetryConsentDecision::Enabled;
        }
        consent.policy_version = TELEMETRY_CONSENT_POLICY_VERSION;
        consent.donation_decision = TelemetryConsentDecision::Disabled;
    }
    Ok(consent)
}

pub fn save_telemetry_consent(
    ldgr_home: &Path,
    consent: &TelemetryConsent,
) -> anyhow::Result<PathBuf> {
    consent.validate()?;
    fs::create_dir_all(ldgr_home)
        .with_context(|| format!("failed to create LDGR home {}", ldgr_home.display()))?;
    let destination = telemetry_consent_path(ldgr_home);
    let mut temporary = NamedTempFile::new_in(ldgr_home).with_context(|| {
        format!(
            "failed to create temporary telemetry consent in {}",
            ldgr_home.display()
        )
    })?;
    serde_json::to_writer_pretty(&mut temporary, consent)
        .context("failed to serialize telemetry consent")?;
    temporary
        .write_all(b"\n")
        .context("failed to finish telemetry consent")?;
    temporary
        .as_file()
        .sync_all()
        .context("failed to sync telemetry consent")?;
    temporary
        .persist(&destination)
        .map_err(|error| error.error)
        .with_context(|| {
            format!(
                "failed to atomically replace telemetry consent {}",
                destination.display()
            )
        })?;
    sync_parent_directory(ldgr_home)?;
    Ok(destination)
}

pub fn telemetry_kill_switch_active() -> bool {
    std::env::var_os("LDGR_TELEMETRY").is_some_and(|value| {
        value
            .to_str()
            .is_some_and(|value| value.eq_ignore_ascii_case("off"))
    })
}

pub fn anonymous_collection_is_eligible(ldgr_home: &Path) -> bool {
    !telemetry_kill_switch_active()
        && load_telemetry_consent(ldgr_home)
            .map(|consent| consent.collection_enabled())
            .unwrap_or(false)
}

#[cfg(test)]
pub(crate) fn telemetry_environment_lock() -> &'static std::sync::Mutex<()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
}

pub fn clear_unsent_telemetry(ldgr_home: &Path) -> anyhow::Result<()> {
    let pending = ldgr_home.join(TELEMETRY_PENDING_DIRECTORY);
    let metadata = match fs::symlink_metadata(&pending) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => {
            clear_local_construction_store(ldgr_home)?;
            return Ok(());
        }
        Err(error) => {
            return Err(error).with_context(|| {
                format!("failed to inspect unsent telemetry {}", pending.display())
            });
        }
    };
    if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() {
        fs::remove_dir_all(&pending)
            .with_context(|| format!("failed to clear unsent telemetry {}", pending.display()))?;
    } else {
        fs::remove_file(&pending)
            .with_context(|| format!("failed to clear unsent telemetry {}", pending.display()))?;
    }
    clear_local_construction_store(ldgr_home)?;
    Ok(())
}

fn clear_local_construction_store(ldgr_home: &Path) -> anyhow::Result<()> {
    let path = command_experience::construction_store_path(ldgr_home);
    match fs::symlink_metadata(&path) {
        Ok(metadata) if metadata.file_type().is_file() && !metadata.file_type().is_symlink() => {
            fs::remove_file(&path).with_context(|| {
                format!(
                    "failed to clear local telemetry constructions {}",
                    path.display()
                )
            })
        }
        Ok(_) => bail!(
            "telemetry construction path {} is not a real file",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error).with_context(|| {
            format!(
                "failed to inspect local telemetry constructions {}",
                path.display()
            )
        }),
    }
}

#[cfg(unix)]
fn sync_parent_directory(directory: &Path) -> anyhow::Result<()> {
    fs::File::open(directory)
        .with_context(|| format!("failed to open LDGR home {}", directory.display()))?
        .sync_all()
        .with_context(|| format!("failed to sync LDGR home {}", directory.display()))
}

#[cfg(not(unix))]
fn sync_parent_directory(_directory: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_consent_uses_the_anonymous_opt_out_default() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let consent = load_telemetry_consent(home.path())?;
        assert_eq!(
            consent,
            TelemetryConsent::current(TelemetryConsentDecision::Enabled)
        );
        assert!(consent.collection_enabled());
        assert!(!consent.donation_enabled());
        assert!(!telemetry_consent_path(home.path()).exists());
        Ok(())
    }

    #[test]
    fn decisions_round_trip_and_replace_atomically() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        for decision in [
            TelemetryConsentDecision::Enabled,
            TelemetryConsentDecision::Disabled,
            TelemetryConsentDecision::Undecided,
        ] {
            let expected = TelemetryConsent::current(decision);
            let path = save_telemetry_consent(home.path(), &expected)?;
            assert_eq!(path, telemetry_consent_path(home.path()));
            assert_eq!(load_telemetry_consent(home.path())?, expected);
        }
        Ok(())
    }

    #[test]
    fn only_current_enabled_consent_enables_collection() {
        let enabled = TelemetryConsent::current(TelemetryConsentDecision::Enabled);
        assert!(enabled.collection_enabled());

        let mut stale = enabled.clone();
        stale.policy_version += 1;
        assert!(!stale.collection_enabled());
        assert!(
            !TelemetryConsent::current(TelemetryConsentDecision::Disabled).collection_enabled()
        );
        assert!(
            !TelemetryConsent::current(TelemetryConsentDecision::Undecided).collection_enabled()
        );
    }

    #[test]
    fn invalid_or_expanded_files_fail_closed() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let path = telemetry_consent_path(home.path());
        fs::write(
            &path,
            r#"{"schema_version":99,"policy_version":1,"decision":"enabled"}"#,
        )?;
        assert!(load_telemetry_consent(home.path()).is_err());

        fs::write(
            &path,
            r#"{"schema_version":1,"policy_version":1,"decision":"enabled","identifier":"forbidden"}"#,
        )?;
        assert!(load_telemetry_consent(home.path()).is_err());
        Ok(())
    }

    #[test]
    fn zero_policy_version_is_rejected_before_write() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let consent = TelemetryConsent {
            schema_version: TELEMETRY_CONSENT_SCHEMA_VERSION,
            policy_version: 0,
            decision: TelemetryConsentDecision::Enabled,
            donation_decision: TelemetryConsentDecision::Disabled,
        };
        assert!(save_telemetry_consent(home.path(), &consent).is_err());
        assert!(!telemetry_consent_path(home.path()).exists());
        Ok(())
    }

    #[test]
    fn policy_v1_choices_migrate_without_enabling_donation() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let path = telemetry_consent_path(home.path());
        for (old, expected) in [
            ("disabled", TelemetryConsentDecision::Disabled),
            ("enabled", TelemetryConsentDecision::Enabled),
            ("undecided", TelemetryConsentDecision::Enabled),
        ] {
            fs::write(
                &path,
                format!(r#"{{"schema_version":1,"policy_version":1,"decision":"{old}"}}"#),
            )?;
            let migrated = load_telemetry_consent(home.path())?;
            assert_eq!(migrated.decision, expected);
            assert_eq!(migrated.policy_version, TELEMETRY_CONSENT_POLICY_VERSION);
            assert!(!migrated.donation_enabled());
        }
        Ok(())
    }

    #[test]
    fn clearing_unsent_telemetry_is_immediate_and_idempotent() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let pending = home.path().join(TELEMETRY_PENDING_DIRECTORY);
        fs::create_dir_all(&pending)?;
        fs::write(pending.join("sequence.json"), "[0,1,3]")?;
        fs::write(
            command_experience::construction_store_path(home.path()),
            "local finite projection",
        )?;
        clear_unsent_telemetry(home.path())?;
        assert!(!pending.exists());
        assert!(!command_experience::construction_store_path(home.path()).exists());
        clear_unsent_telemetry(home.path())?;
        Ok(())
    }
}
