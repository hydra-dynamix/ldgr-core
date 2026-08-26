use std::fs;
use std::io::{ErrorKind, Write};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use tempfile::NamedTempFile;

use super::serializer::serialize_sequence;
use super::transition::{
    CommittedSequence, NormalizedTerminal, NumericalProtocol, StateCode, TransitionAcceptance,
};
use super::{anonymous_collection_is_eligible, TELEMETRY_PENDING_DIRECTORY};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum BufferedTransition {
    Intermediate,
    Terminal(NormalizedTerminal),
    Dropped,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum QueuedTerminalSequence {
    Queued,
    Dropped,
}

/// A privacy-bounded sequence for one local unit of work.
///
/// Incomplete states exist only in memory. The buffer writes one unlabelled
/// numerical array after a valid terminal transition and only while current
/// consent remains enabled.
#[derive(Debug)]
pub struct LocalSequenceBuffer<'protocol> {
    ldgr_home: PathBuf,
    sequence: CommittedSequence<'protocol>,
    finalized: bool,
}

impl<'protocol> LocalSequenceBuffer<'protocol> {
    /// Begin after the protocol's initial state has committed locally.
    ///
    /// `Ok(None)` means collection is not currently eligible. Invalid or
    /// unreadable consent also fails closed to `Ok(None)`.
    pub fn begin_after_commit(
        ldgr_home: impl Into<PathBuf>,
        protocol: &'protocol NumericalProtocol,
    ) -> anyhow::Result<Option<Self>> {
        let ldgr_home = ldgr_home.into();
        if !collection_is_eligible(&ldgr_home) {
            return Ok(None);
        }
        Ok(Some(Self {
            ldgr_home,
            sequence: CommittedSequence::begin_after_commit(protocol)?,
            finalized: false,
        }))
    }

    /// Record a numerical state after its corresponding local commit.
    ///
    /// Invalid protocol transitions are rejected. Consent changes, the process
    /// kill switch, or local queue failures drop telemetry without persisting a
    /// partial record.
    pub fn submit_committed(&mut self, state: StateCode) -> anyhow::Result<BufferedTransition> {
        if self.finalized || !collection_is_eligible(&self.ldgr_home) {
            self.finalized = true;
            return Ok(BufferedTransition::Dropped);
        }
        let accepted = self.sequence.submit_committed(state)?;
        match accepted {
            TransitionAcceptance::Intermediate => Ok(BufferedTransition::Intermediate),
            TransitionAcceptance::Terminal(terminal) => {
                self.finalized = true;
                if queue_terminal_sequence(
                    &self.ldgr_home,
                    self.sequence.protocol(),
                    self.sequence.numerical_states(),
                )
                .is_err()
                {
                    return Ok(BufferedTransition::Dropped);
                }
                Ok(BufferedTransition::Terminal(terminal))
            }
        }
    }

    pub fn numerical_states(&self) -> &[StateCode] {
        self.sequence.numerical_states()
    }
}

fn collection_is_eligible(ldgr_home: &Path) -> bool {
    anonymous_collection_is_eligible(ldgr_home)
}

pub(crate) fn queue_committed_terminal_sequence(
    ldgr_home: impl Into<PathBuf>,
    protocol: &NumericalProtocol,
    states: &[StateCode],
) -> anyhow::Result<QueuedTerminalSequence> {
    let ldgr_home = ldgr_home.into();
    if !collection_is_eligible(&ldgr_home) {
        return Ok(QueuedTerminalSequence::Dropped);
    }
    match queue_terminal_sequence(&ldgr_home, protocol, states) {
        Ok(()) => Ok(QueuedTerminalSequence::Queued),
        Err(_) => Ok(QueuedTerminalSequence::Dropped),
    }
}

fn queue_terminal_sequence(
    ldgr_home: &Path,
    protocol: &NumericalProtocol,
    states: &[StateCode],
) -> anyhow::Result<()> {
    let route = protocol
        .endpoint()
        .strip_prefix("/sequences/")
        .context("validated protocol endpoint lost /sequences/ prefix")?;
    let pending_root = ldgr_home.join(TELEMETRY_PENDING_DIRECTORY);
    ensure_real_directory(&pending_root)?;
    let mut destination = pending_root;
    for component in route.split('/') {
        destination.push(component);
        ensure_real_directory(&destination)?;
    }

    let mut pending = NamedTempFile::new_in(&destination).with_context(|| {
        format!(
            "failed to create pending numerical sequence in {}",
            destination.display()
        )
    })?;
    let payload = serialize_sequence(protocol, states)?;
    pending
        .write_all(&payload)
        .context("failed to write numerical sequence")?;
    pending
        .flush()
        .context("failed to flush pending numerical sequence")?;
    pending
        .as_file()
        .sync_all()
        .context("failed to sync pending numerical sequence")?;
    pending
        .keep()
        .map_err(|error| error.error)
        .context("failed to preserve pending numerical sequence")?;
    sync_directory(&destination)?;
    Ok(())
}

fn ensure_real_directory(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.file_type().is_dir() && !metadata.file_type().is_symlink() => {
            Ok(())
        }
        Ok(_) => bail!(
            "telemetry queue path {} is not a real directory",
            path.display()
        ),
        Err(error) if error.kind() == ErrorKind::NotFound => {
            fs::create_dir(path).with_context(|| {
                format!(
                    "failed to create telemetry queue directory {}",
                    path.display()
                )
            })
        }
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect telemetry queue path {}", path.display())),
    }
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> anyhow::Result<()> {
    fs::File::open(path)
        .and_then(|directory| directory.sync_all())
        .with_context(|| {
            format!(
                "failed to sync telemetry queue directory {}",
                path.display()
            )
        })
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::telemetry::transition::{
        CANCELLED, COMPLETED_INCONCLUSIVE, COMPLETED_NEGATIVE, COMPLETED_POSITIVE, CORE_WORK_V1,
        OPERATIONAL_FAILURE, PENDING, RUNNING,
    };
    use crate::telemetry::{
        save_telemetry_consent, telemetry_environment_lock, TelemetryConsent,
        TelemetryConsentDecision, TELEMETRY_CONSENT_FILE,
    };

    fn enable(home: &Path) -> anyhow::Result<()> {
        save_telemetry_consent(
            home,
            &TelemetryConsent::current(TelemetryConsentDecision::Enabled),
        )?;
        Ok(())
    }

    fn pending_files(home: &Path) -> anyhow::Result<Vec<PathBuf>> {
        pending_files_for(home, "core-work/v1")
    }

    fn pending_files_for(home: &Path, route: &str) -> anyhow::Result<Vec<PathBuf>> {
        let route = home.join(TELEMETRY_PENDING_DIRECTORY).join(route);
        if !route.exists() {
            return Ok(Vec::new());
        }
        Ok(fs::read_dir(route)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?)
    }

    const ADAPTER_STATES: &[StateCode] = &[
        PENDING,
        RUNNING,
        COMPLETED_POSITIVE,
        COMPLETED_NEGATIVE,
        COMPLETED_INCONCLUSIVE,
        OPERATIONAL_FAILURE,
        CANCELLED,
    ];
    const ADAPTER_TRANSITIONS: &[(StateCode, StateCode)] =
        &[(PENDING, RUNNING), (RUNNING, COMPLETED_POSITIVE)];
    const ADAPTER_V1: NumericalProtocol = NumericalProtocol::new(
        "/sequences/example-adapter-work/v1",
        PENDING,
        ADAPTER_STATES,
        ADAPTER_TRANSITIONS,
        8,
    );

    #[test]
    fn missing_consent_defaults_on_and_explicit_disable_creates_no_files() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        assert!(LocalSequenceBuffer::begin_after_commit(home.path(), &CORE_WORK_V1)?.is_some());
        save_telemetry_consent(
            home.path(),
            &TelemetryConsent::current(TelemetryConsentDecision::Disabled),
        )?;
        assert!(LocalSequenceBuffer::begin_after_commit(home.path(), &CORE_WORK_V1)?.is_none());
        assert!(!home.path().join(TELEMETRY_PENDING_DIRECTORY).exists());
        Ok(())
    }

    #[test]
    fn adapter_protocol_queueing_inherits_core_consent() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        let adapter_path = [PENDING, RUNNING, COMPLETED_POSITIVE];

        save_telemetry_consent(
            home.path(),
            &TelemetryConsent::current(TelemetryConsentDecision::Disabled),
        )?;
        assert_eq!(
            queue_committed_terminal_sequence(home.path(), &ADAPTER_V1, &adapter_path)?,
            QueuedTerminalSequence::Dropped
        );
        assert!(pending_files_for(home.path(), "example-adapter-work/v1")?.is_empty());

        enable(home.path())?;
        assert_eq!(
            queue_committed_terminal_sequence(home.path(), &ADAPTER_V1, &adapter_path)?,
            QueuedTerminalSequence::Queued
        );
        let files = pending_files_for(home.path(), "example-adapter-work/v1")?;
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(&files[0])?, b"[0,1,3]");
        Ok(())
    }

    #[test]
    fn terminal_sequence_is_one_unlabelled_raw_integer_array() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        enable(home.path())?;
        let mut buffer = LocalSequenceBuffer::begin_after_commit(home.path(), &CORE_WORK_V1)?
            .expect("enabled consent creates a buffer");
        assert_eq!(
            buffer.submit_committed(RUNNING)?,
            BufferedTransition::Intermediate
        );
        assert!(pending_files(home.path())?.is_empty());
        assert_eq!(
            buffer.submit_committed(COMPLETED_NEGATIVE)?,
            BufferedTransition::Terminal(NormalizedTerminal::CompletedNegative)
        );
        let files = pending_files(home.path())?;
        assert_eq!(files.len(), 1);
        assert_eq!(fs::read(&files[0])?, b"[0,1,4]");
        assert_eq!(
            serde_json::from_slice::<serde_json::Value>(&fs::read(&files[0])?)?,
            serde_json::json!([0, 1, 4])
        );
        assert!(home.path().join(TELEMETRY_CONSENT_FILE).is_file());
        Ok(())
    }

    #[test]
    fn consent_removed_before_terminal_discards_the_incomplete_sequence() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        enable(home.path())?;
        let mut buffer = LocalSequenceBuffer::begin_after_commit(home.path(), &CORE_WORK_V1)?
            .expect("enabled consent creates a buffer");
        buffer.submit_committed(RUNNING)?;
        save_telemetry_consent(
            home.path(),
            &TelemetryConsent::current(TelemetryConsentDecision::Disabled),
        )?;
        assert_eq!(
            buffer.submit_committed(COMPLETED_POSITIVE)?,
            BufferedTransition::Dropped
        );
        assert!(pending_files(home.path())?.is_empty());
        Ok(())
    }

    #[test]
    fn process_kill_switch_prevents_buffering_and_terminal_queueing() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        enable(home.path())?;
        std::env::set_var("LDGR_TELEMETRY", "off");
        let result = LocalSequenceBuffer::begin_after_commit(home.path(), &CORE_WORK_V1);
        std::env::remove_var("LDGR_TELEMETRY");
        assert!(result?.is_none());
        assert!(pending_files(home.path())?.is_empty());
        Ok(())
    }

    #[test]
    fn malformed_consent_fails_closed_without_creating_queue_state() -> anyhow::Result<()> {
        let home = tempfile::tempdir()?;
        fs::write(
            home.path().join(TELEMETRY_CONSENT_FILE),
            r#"{"decision":"enabled","unexpected":"content"}"#,
        )?;
        assert!(LocalSequenceBuffer::begin_after_commit(home.path(), &CORE_WORK_V1)?.is_none());
        assert!(!home.path().join(TELEMETRY_PENDING_DIRECTORY).exists());
        Ok(())
    }
}
