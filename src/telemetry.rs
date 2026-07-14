use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

pub mod buffer;
pub mod serializer;
pub mod transition;

pub const TELEMETRY_CONSENT_SCHEMA_VERSION: u32 = 1;
pub const TELEMETRY_CONSENT_POLICY_VERSION: u32 = 1;
pub const TELEMETRY_CONSENT_FILE: &str = "telemetry-consent.json";
pub const TELEMETRY_PENDING_DIRECTORY: &str = "telemetry-pending";
pub const NUMERICAL_SEQUENCE_PROTOCOLS_V1: &[&str] = &["core-work/v1"];

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
}

impl Default for TelemetryConsent {
    fn default() -> Self {
        Self::current(TelemetryConsentDecision::Undecided)
    }
}

impl TelemetryConsent {
    pub fn current(decision: TelemetryConsentDecision) -> Self {
        Self {
            schema_version: TELEMETRY_CONSENT_SCHEMA_VERSION,
            policy_version: TELEMETRY_CONSENT_POLICY_VERSION,
            decision,
        }
    }

    pub fn collection_enabled(&self) -> bool {
        self.schema_version == TELEMETRY_CONSENT_SCHEMA_VERSION
            && self.policy_version == TELEMETRY_CONSENT_POLICY_VERSION
            && self.decision == TelemetryConsentDecision::Enabled
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
    let consent: TelemetryConsent = serde_json::from_str(&text)
        .with_context(|| format!("failed to parse telemetry consent {}", path.display()))?;
    consent.validate()?;
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

pub fn clear_unsent_telemetry(ldgr_home: &Path) -> anyhow::Result<()> {
    let pending = ldgr_home.join(TELEMETRY_PENDING_DIRECTORY);
    let metadata = match fs::symlink_metadata(&pending) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
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
    Ok(())
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
    fn missing_consent_is_undecided_and_off() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let consent = load_telemetry_consent(home.path())?;
        assert_eq!(
            consent,
            TelemetryConsent::current(TelemetryConsentDecision::Undecided)
        );
        assert!(!consent.collection_enabled());
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
        };
        assert!(save_telemetry_consent(home.path(), &consent).is_err());
        assert!(!telemetry_consent_path(home.path()).exists());
        Ok(())
    }

    #[test]
    fn clearing_unsent_telemetry_is_immediate_and_idempotent() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        let pending = home.path().join(TELEMETRY_PENDING_DIRECTORY);
        fs::create_dir_all(&pending)?;
        fs::write(pending.join("sequence.json"), "[0,1,3]")?;
        clear_unsent_telemetry(home.path())?;
        assert!(!pending.exists());
        clear_unsent_telemetry(home.path())?;
        Ok(())
    }
}
