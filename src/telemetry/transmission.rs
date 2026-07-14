use std::fs;
use std::path::{Path, PathBuf};
use std::thread;
use std::time::Duration;

use reqwest::blocking::{Client, Request};
use reqwest::header::{ACCEPT, CONTENT_TYPE};
use reqwest::redirect::Policy;
use reqwest::Url;

use super::serializer::parse_exact_sequence;
use super::transition::NumericalProtocol;
use super::{load_telemetry_consent, telemetry_kill_switch_active, TELEMETRY_PENDING_DIRECTORY};

pub const DEFAULT_MAX_TRANSMISSION_DELAY: Duration = Duration::from_secs(30);
pub const DEFAULT_TRANSMISSION_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct TransmissionReport {
    pub disabled: bool,
    pub attempted: usize,
    pub accepted: usize,
    pub retained: usize,
    pub invalid_dropped: usize,
}

#[derive(Clone, Debug)]
pub struct TransmissionClient {
    collector_origin: Url,
    max_delay: Duration,
    timeout: Duration,
}

impl TransmissionClient {
    pub fn new(collector_origin: &str) -> anyhow::Result<Self> {
        let origin = Url::parse(collector_origin)?;
        anyhow::ensure!(
            origin.scheme() == "https",
            "telemetry collector must use HTTPS"
        );
        anyhow::ensure!(
            origin.host_str().is_some(),
            "telemetry collector host is missing"
        );
        anyhow::ensure!(
            origin.username().is_empty() && origin.password().is_none(),
            "telemetry collector URL must not contain credentials"
        );
        anyhow::ensure!(
            origin.path() == "/" && origin.query().is_none() && origin.fragment().is_none(),
            "telemetry collector URL must be an origin without path, query, or fragment"
        );
        Ok(Self {
            collector_origin: origin,
            max_delay: DEFAULT_MAX_TRANSMISSION_DELAY,
            timeout: DEFAULT_TRANSMISSION_TIMEOUT,
        })
    }

    pub fn with_max_delay(mut self, max_delay: Duration) -> Self {
        self.max_delay = max_delay;
        self
    }

    pub fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    /// Best-effort delivery of pending sequences for exactly one protocol.
    ///
    /// This method never returns a network or local queue error. Failed sends
    /// remain pending, while malformed local records are deleted rather than
    /// transmitted. Callers must not make normal command outcomes depend on
    /// this report.
    pub fn transmit_pending(
        &self,
        ldgr_home: &Path,
        protocol: &NumericalProtocol,
    ) -> TransmissionReport {
        let transport = HttpSequenceTransport {
            timeout: self.timeout,
        };
        self.transmit_with(ldgr_home, protocol, &transport)
    }

    fn transmit_with<T: SequenceTransport>(
        &self,
        ldgr_home: &Path,
        protocol: &NumericalProtocol,
        transport: &T,
    ) -> TransmissionReport {
        let mut report = TransmissionReport::default();
        if !collection_is_eligible(ldgr_home) {
            report.disabled = true;
            return report;
        }
        if protocol.validate().is_err() {
            return report;
        }
        let Some(route) = protocol.endpoint().strip_prefix("/sequences/") else {
            return report;
        };
        let pending_directory = ldgr_home.join(TELEMETRY_PENDING_DIRECTORY).join(route);
        let mut pending = match real_pending_files(&pending_directory) {
            Ok(pending) => pending,
            Err(_) => return report,
        };
        pending.sort();

        let endpoint = match self.collector_origin.join(protocol.endpoint()) {
            Ok(endpoint) => endpoint,
            Err(_) => return report,
        };
        for path in pending {
            if !collection_is_eligible(ldgr_home) {
                report.disabled = true;
                break;
            }
            let payload = match fs::read(&path) {
                Ok(payload) => payload,
                Err(_) => {
                    report.retained += 1;
                    continue;
                }
            };
            if parse_exact_sequence(protocol, &payload).is_err() {
                if fs::remove_file(&path).is_ok() {
                    report.invalid_dropped += 1;
                } else {
                    report.retained += 1;
                }
                continue;
            }

            random_delay(self.max_delay);
            report.attempted += 1;
            if transport.post(&endpoint, &payload) {
                if fs::remove_file(&path).is_ok() {
                    report.accepted += 1;
                } else {
                    report.retained += 1;
                }
            } else {
                report.retained += 1;
            }
        }
        report
    }
}

trait SequenceTransport {
    fn post(&self, endpoint: &Url, payload: &[u8]) -> bool;
}

struct HttpSequenceTransport {
    timeout: Duration,
}

impl SequenceTransport for HttpSequenceTransport {
    fn post(&self, endpoint: &Url, payload: &[u8]) -> bool {
        let Ok(client) = build_http_client(self.timeout) else {
            return false;
        };
        let Ok(request) = build_sequence_request(&client, endpoint.clone(), payload) else {
            return false;
        };
        client
            .execute(request)
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    }
}

fn build_http_client(timeout: Duration) -> reqwest::Result<Client> {
    Client::builder()
        .https_only(true)
        .redirect(Policy::none())
        .referer(false)
        .no_proxy()
        .timeout(timeout)
        .build()
}

fn build_sequence_request(
    client: &Client,
    endpoint: Url,
    payload: &[u8],
) -> reqwest::Result<Request> {
    let mut request = client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/json")
        .body(payload.to_vec())
        .build()?;
    request.headers_mut().remove(ACCEPT);
    Ok(request)
}

fn collection_is_eligible(ldgr_home: &Path) -> bool {
    !telemetry_kill_switch_active()
        && load_telemetry_consent(ldgr_home)
            .map(|consent| consent.collection_enabled())
            .unwrap_or(false)
}

fn real_pending_files(directory: &Path) -> anyhow::Result<Vec<PathBuf>> {
    let metadata = match fs::symlink_metadata(directory) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error.into()),
    };
    anyhow::ensure!(
        metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
        "pending protocol path is not a real directory"
    );
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_file() && !metadata.file_type().is_symlink() {
            files.push(entry.path());
        }
    }
    Ok(files)
}

fn random_delay(max_delay: Duration) {
    let max_millis = max_delay.as_millis().min(u64::MAX as u128) as u64;
    if max_millis == 0 {
        return;
    }
    let mut random = [0_u8; 8];
    if getrandom::getrandom(&mut random).is_err() {
        return;
    }
    let sample = u64::from_ne_bytes(random);
    let delay = sample % max_millis.saturating_add(1);
    thread::sleep(Duration::from_millis(delay));
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use crate::telemetry::buffer::LocalSequenceBuffer;
    use crate::telemetry::transition::{
        StateCode, COMPLETED_NEGATIVE, COMPLETED_POSITIVE, CORE_WORK_V1, RUNNING,
    };
    use crate::telemetry::{
        save_telemetry_consent, telemetry_environment_lock, TelemetryConsent,
        TelemetryConsentDecision,
    };

    #[derive(Default)]
    struct CaptureTransport {
        requests: Mutex<Vec<(Url, Vec<u8>)>>,
        accept: bool,
    }

    impl SequenceTransport for CaptureTransport {
        fn post(&self, endpoint: &Url, payload: &[u8]) -> bool {
            self.requests
                .lock()
                .expect("capture lock poisoned")
                .push((endpoint.clone(), payload.to_vec()));
            self.accept
        }
    }

    fn enable(home: &Path) -> anyhow::Result<()> {
        save_telemetry_consent(
            home,
            &TelemetryConsent::current(TelemetryConsentDecision::Enabled),
        )?;
        Ok(())
    }

    fn queue(home: &Path, terminal: StateCode) -> anyhow::Result<()> {
        let mut buffer = LocalSequenceBuffer::begin_after_commit(home, &CORE_WORK_V1)?
            .expect("consent is enabled");
        buffer.submit_committed(RUNNING)?;
        buffer.submit_committed(terminal)?;
        Ok(())
    }

    fn zero_delay_client() -> anyhow::Result<TransmissionClient> {
        Ok(TransmissionClient::new("https://collector.example")?.with_max_delay(Duration::ZERO))
    }

    #[test]
    fn one_raw_array_is_sent_per_request_and_accepted_files_are_removed() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        enable(home.path())?;
        queue(home.path(), COMPLETED_POSITIVE)?;
        queue(home.path(), COMPLETED_NEGATIVE)?;
        let transport = CaptureTransport {
            accept: true,
            ..CaptureTransport::default()
        };
        let report = zero_delay_client()?.transmit_with(home.path(), &CORE_WORK_V1, &transport);
        assert_eq!(report.attempted, 2);
        assert_eq!(report.accepted, 2);
        assert_eq!(report.retained, 0);
        let requests = transport.requests.lock().expect("capture lock poisoned");
        assert_eq!(requests.len(), 2);
        assert!(requests.iter().all(|(endpoint, payload)| {
            endpoint.as_str() == "https://collector.example/sequences/core-work/v1"
                && (payload == b"[0,1,3]" || payload == b"[0,1,4]")
        }));
        Ok(())
    }

    #[test]
    fn request_has_no_product_identity_cookie_auth_or_redirect_surface() -> anyhow::Result<()> {
        let client = build_http_client(Duration::from_secs(1))?;
        let request = build_sequence_request(
            &client,
            Url::parse("https://collector.example/sequences/core-work/v1")?,
            b"[0,1,4]",
        )?;
        assert_eq!(request.method(), reqwest::Method::POST);
        assert_eq!(request.headers().len(), 1);
        assert_eq!(request.headers()[CONTENT_TYPE], "application/json");
        assert!(request.headers().get("user-agent").is_none());
        assert!(request.headers().get("cookie").is_none());
        assert!(request.headers().get("authorization").is_none());
        assert_eq!(
            request.body().and_then(|body| body.as_bytes()),
            Some(&b"[0,1,4]"[..])
        );
        Ok(())
    }

    #[test]
    fn consent_and_kill_switch_prevent_any_attempt() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        let transport = CaptureTransport::default();
        let disabled = zero_delay_client()?.transmit_with(home.path(), &CORE_WORK_V1, &transport);
        assert!(disabled.disabled);
        assert_eq!(disabled.attempted, 0);

        enable(home.path())?;
        queue(home.path(), COMPLETED_POSITIVE)?;
        std::env::set_var("LDGR_TELEMETRY", "off");
        let killed = zero_delay_client()?.transmit_with(home.path(), &CORE_WORK_V1, &transport);
        std::env::remove_var("LDGR_TELEMETRY");
        assert!(killed.disabled);
        assert_eq!(killed.attempted, 0);
        assert!(transport
            .requests
            .lock()
            .expect("capture lock poisoned")
            .is_empty());
        Ok(())
    }

    #[test]
    fn failure_retains_payload_and_never_returns_an_error() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        enable(home.path())?;
        queue(home.path(), COMPLETED_NEGATIVE)?;
        let report = TransmissionClient::new("https://127.0.0.1:9")?
            .with_max_delay(Duration::ZERO)
            .with_timeout(Duration::from_millis(100))
            .transmit_pending(home.path(), &CORE_WORK_V1);
        assert_eq!(report.attempted, 1);
        assert_eq!(report.accepted, 0);
        assert_eq!(report.retained, 1);
        Ok(())
    }

    #[test]
    fn malformed_pending_payload_is_deleted_without_a_request() -> anyhow::Result<()> {
        let _guard = telemetry_environment_lock()
            .lock()
            .expect("environment lock poisoned");
        let home = tempfile::tempdir()?;
        enable(home.path())?;
        let route = home
            .path()
            .join(TELEMETRY_PENDING_DIRECTORY)
            .join("core-work/v1");
        fs::create_dir_all(&route)?;
        fs::write(
            route.join("invalid"),
            br#"{"project":"secret","sequence":[0,1,3]}"#,
        )?;
        let transport = CaptureTransport::default();
        let report = zero_delay_client()?.transmit_with(home.path(), &CORE_WORK_V1, &transport);
        assert_eq!(report.invalid_dropped, 1);
        assert_eq!(report.attempted, 0);
        assert!(fs::read_dir(route)?.next().is_none());
        Ok(())
    }

    #[test]
    fn collector_origin_must_be_bare_https_without_credentials() {
        for origin in [
            "http://collector.example",
            "https://user@collector.example",
            "https://collector.example/base",
            "https://collector.example/?id=1",
        ] {
            assert!(TransmissionClient::new(origin).is_err(), "{origin}");
        }
    }
}
