use std::path::Path;
use std::time::Duration;

use ldgr_core::telemetry::buffer::LocalSequenceBuffer;
use ldgr_core::telemetry::transition::{COMPLETED_NEGATIVE, CORE_WORK_V1, RUNNING};
use ldgr_core::telemetry::transmission::TransmissionClient;
use ldgr_core::telemetry::{save_telemetry_consent, TelemetryConsent, TelemetryConsentDecision};

#[test]
#[ignore = "requires an explicitly authorized remote collector"]
fn opted_in_negative_sequence_reaches_remote_collector() -> anyhow::Result<()> {
    let endpoint = std::env::var("LDGR_TELEMETRY_TEST_ENDPOINT")?;
    let certificate = std::fs::read(std::env::var("LDGR_TELEMETRY_TEST_CA")?)?;
    let home = tempfile::tempdir()?;
    enable(home.path())?;

    let mut buffer = LocalSequenceBuffer::begin_after_commit(home.path(), &CORE_WORK_V1)?
        .expect("explicit consent creates a sequence buffer");
    buffer.submit_committed(RUNNING)?;
    buffer.submit_committed(COMPLETED_NEGATIVE)?;

    let report = TransmissionClient::new(&endpoint)?
        .with_root_certificate_pem(&certificate)?
        .with_max_delay(Duration::ZERO)
        .with_timeout(Duration::from_secs(10))
        .transmit_pending(home.path(), &CORE_WORK_V1);
    assert_eq!(report.attempted, 1);
    assert_eq!(report.accepted, 1);
    assert_eq!(report.retained, 0);
    Ok(())
}

fn enable(home: &Path) -> anyhow::Result<()> {
    save_telemetry_consent(
        home,
        &TelemetryConsent::current(TelemetryConsentDecision::Enabled),
    )?;
    Ok(())
}
