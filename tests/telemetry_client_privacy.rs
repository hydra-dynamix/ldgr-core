use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use anyhow::{anyhow, bail, Context};
use assert_cmd::Command;
use ldgr_core::telemetry::adapter_protocols::EVIDENCE_WORKFLOW_V1;
use ldgr_core::telemetry::buffer::LocalSequenceBuffer;
use ldgr_core::telemetry::transition::{
    NumericalProtocol, COMPLETED_NEGATIVE, COMPLETED_POSITIVE, CORE_WORK_V1, RESEARCH_WORKFLOW_V1,
    RUNNING,
};
use ldgr_core::telemetry::transmission::{preview_pending_sequences, TransmissionClient};
use ldgr_core::telemetry::{
    load_telemetry_consent, save_telemetry_consent, TelemetryConsent, TelemetryConsentDecision,
    TELEMETRY_PENDING_DIRECTORY,
};
use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer};
use rustls::{ServerConfig, ServerConnection, StreamOwned};

const CAPTURE_CA_PEM: &[u8] = include_bytes!("fixtures/telemetry-capture-ca.pem");
const CAPTURE_SERVER_CERTIFICATE_PEM: &[u8] =
    include_bytes!("fixtures/telemetry-capture-server-cert.pem");
const CAPTURE_SERVER_PRIVATE_KEY_PEM: &[u8] =
    include_bytes!("fixtures/telemetry-capture-server-key.pem");

#[test]
fn capture_server_receives_only_exact_raw_arrays_one_per_request_and_no_metadata(
) -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    enable(home.path())?;
    queue(home.path(), COMPLETED_POSITIVE)?;
    queue(home.path(), COMPLETED_NEGATIVE)?;
    write_hostile_pending_fixtures(home.path())?;

    let server = CaptureServer::start(vec![ResponsePlan::status(204), ResponsePlan::status(204)])?;
    let report = trusted_client(&server)?.transmit_pending(home.path(), &CORE_WORK_V1);

    assert_eq!(report.attempted, 2);
    assert_eq!(report.accepted, 2);
    assert_eq!(report.retained, 0);
    assert_eq!(report.invalid_dropped, 2);

    let requests = server.finish()?;
    assert_eq!(requests.len(), 2);
    let mut bodies = requests
        .iter()
        .map(|request| request.body.clone())
        .collect::<Vec<_>>();
    bodies.sort();
    assert_eq!(bodies, [b"[0,1,3]".to_vec(), b"[0,1,4]".to_vec()]);

    for request in requests {
        assert_eq!(
            request.request_line,
            "POST /sequences/core-work/v1 HTTP/1.1"
        );
        assert_privacy_preserving_wire_headers(&request);
        assert_no_prohibited_body_metadata(&request.body);
    }

    let preview = preview_pending_sequences(home.path(), &CORE_WORK_V1)?;
    assert!(preview.payloads.is_empty());
    assert_eq!(preview.invalid, 0);
    assert_eq!(preview.unreadable, 0);
    Ok(())
}

#[test]
fn cli_transmit_captures_actual_tls_wire_headers_without_identifying_metadata() -> anyhow::Result<()>
{
    let project = tempfile::tempdir()?;
    let ldgr_home = cli_ldgr_home(project.path());
    enable(&ldgr_home)?;
    queue_protocol(&ldgr_home, &CORE_WORK_V1, COMPLETED_NEGATIVE)?;
    queue_protocol(&ldgr_home, &RESEARCH_WORKFLOW_V1, COMPLETED_NEGATIVE)?;
    let mut evidence = LocalSequenceBuffer::begin_after_commit(&ldgr_home, &EVIDENCE_WORKFLOW_V1)?
        .expect("enabled telemetry creates an adapter buffer");
    evidence.submit_committed(RUNNING)?;
    evidence.submit_committed(9)?;
    evidence.submit_committed(COMPLETED_NEGATIVE)?;

    let ca_path = project.path().join("telemetry-capture-ca.pem");
    fs::write(&ca_path, CAPTURE_CA_PEM)?;
    let server = CaptureServer::start(vec![
        ResponsePlan::status(204),
        ResponsePlan::status(204),
        ResponsePlan::status(204),
    ])?;

    let mut command = telemetry_command(project.path())?;
    command
        .args(["telemetry", "transmit", "--collector"])
        .arg(&server.origin)
        .args(["--root-ca-pem"])
        .arg(&ca_path)
        .args(["--max-delay-ms", "0", "--timeout-ms", "3000"]);
    let output = command.output()?;
    let requests = server.finish()?;

    assert!(
        output.status.success(),
        "telemetry transmit failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(
            "protocol /sequences/core-work/v1: attempted=1 accepted=1 retained=0 invalid_dropped=0 disabled=false"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "protocol /sequences/research-workflow/v1: attempted=1 accepted=1 retained=0 invalid_dropped=0 disabled=false"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "protocol /sequences/evidence-workflow/v1: attempted=1 accepted=1 retained=0 invalid_dropped=0 disabled=false"
        ),
        "{stdout}"
    );
    assert!(
        stdout.contains(
            "telemetry transmission: attempted=3 accepted=3 retained=0 invalid_dropped=0"
        ),
        "{stdout}"
    );

    assert_eq!(requests.len(), 3);
    assert_eq!(
        requests
            .iter()
            .map(|request| request.request_line.as_str())
            .collect::<Vec<_>>(),
        vec![
            "POST /sequences/core-work/v1 HTTP/1.1",
            "POST /sequences/research-workflow/v1 HTTP/1.1",
            "POST /sequences/evidence-workflow/v1 HTTP/1.1",
        ]
    );
    assert_eq!(requests[0].body, b"[0,1,4]");
    assert_eq!(requests[1].body, b"[0,1,4]");
    assert_eq!(requests[2].body, b"[0,1,9,4]");
    for request in requests {
        assert_privacy_preserving_wire_headers(&request);
    }
    assert_eq!(pending_file_count_for(&ldgr_home, "core-work/v1")?, 0);
    assert_eq!(
        pending_file_count_for(&ldgr_home, "research-workflow/v1")?,
        0
    );
    assert_eq!(
        pending_file_count_for(&ldgr_home, "evidence-workflow/v1")?,
        0
    );
    Ok(())
}

#[test]
fn automatic_worker_recovers_pending_telemetry_on_next_startup() -> anyhow::Result<()> {
    let project = tempfile::tempdir()?;
    let ldgr_home = cli_ldgr_home(project.path());
    enable(&ldgr_home)?;
    queue_protocol(&ldgr_home, &CORE_WORK_V1, COMPLETED_NEGATIVE)?;

    let ca_path = project.path().join("telemetry-capture-ca.pem");
    fs::write(&ca_path, CAPTURE_CA_PEM)?;
    let server = CaptureServer::start(vec![ResponsePlan::status(204)])?;
    let mut command = automatic_telemetry_command(project.path(), &server, &ca_path)?;
    command.args(["compatibility", "--agentctl-version", "0.1.2", "--json"]);
    let output = command.output()?;
    let requests = server.finish()?;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body, b"[0,1,4]");
    wait_for_empty_pending_route(&ldgr_home, "core-work/v1")?;
    Ok(())
}

#[test]
fn automatic_worker_sends_sequence_queued_during_normal_shutdown() -> anyhow::Result<()> {
    let project = tempfile::tempdir()?;
    let ldgr_home = cli_ldgr_home(project.path());
    enable(&ldgr_home)?;

    run_cli(project.path(), ["init"])?;
    run_cli(
        project.path(),
        [
            "work",
            "create",
            "automatic-shutdown",
            "--title",
            "Automatic shutdown",
            "--description",
            "Queue one terminal sequence.",
        ],
    )?;
    let start = run_cli(
        project.path(),
        ["run", "start", "automatic-shutdown", "--command", "fixture"],
    )?;
    let run_id = String::from_utf8(start.stdout)?
        .split_whitespace()
        .nth(2)
        .context("run start did not print a run ID")?
        .to_owned();

    let ca_path = project.path().join("telemetry-capture-ca.pem");
    fs::write(&ca_path, CAPTURE_CA_PEM)?;
    let server = CaptureServer::start(vec![ResponsePlan::status(204)])?;
    let mut close = automatic_telemetry_command(project.path(), &server, &ca_path)?;
    close.args([
        "run",
        "close",
        &run_id,
        "--status",
        "success",
        "--outcome",
        "stop",
        "--rationale",
        "fixture complete",
    ]);
    let output = close.output()?;
    let requests = server.finish()?;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].body, b"[0,1,3]");
    wait_for_empty_pending_route(&ldgr_home, "core-work/v1")?;
    Ok(())
}

#[test]
fn opted_in_sanitized_work_episode_is_sent_automatically_after_shutdown() -> anyhow::Result<()> {
    let project = tempfile::tempdir()?;
    let ldgr_home = cli_ldgr_home(project.path());
    enable_donation(&ldgr_home)?;

    run_cli(project.path(), ["init"])?;
    run_cli(
        project.path(),
        [
            "work",
            "create",
            "automatic-donation",
            "--title",
            "Validate the sanitized donation boundary",
            "--description",
            "Preserve model-sanitized evidence for research.",
        ],
    )?;
    let start = run_cli(
        project.path(),
        [
            "run",
            "start",
            "automatic-donation",
            "--command",
            "capture sanitized work episode",
        ],
    )?;
    let run_id = String::from_utf8(start.stdout)?
        .split_whitespace()
        .nth(2)
        .context("run start did not print a run ID")?
        .to_owned();
    run_cli(
        project.path(),
        [
            "observation",
            "add",
            &run_id,
            "--body",
            "Sanitized opted-in evidence",
        ],
    )?;

    let ca_path = project.path().join("telemetry-capture-ca.pem");
    fs::write(&ca_path, CAPTURE_CA_PEM)?;
    let server = CaptureServer::start(vec![ResponsePlan::status(204)])?;
    let mut close = automatic_telemetry_command(project.path(), &server, &ca_path)?;
    close.args([
        "run",
        "close",
        &run_id,
        "--status",
        "success",
        "--outcome",
        "stop",
        "--rationale",
        "sanitized fixture complete",
    ]);
    let output = close.output()?;
    let requests = server.finish()?;

    assert!(output.status.success(), "{output:?}");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0].request_line,
        "POST /donations/experiences/v1 HTTP/1.1"
    );
    assert_privacy_preserving_wire_headers(&requests[0]);
    let donation: serde_json::Value = serde_json::from_slice(&requests[0].body)?;
    assert_eq!(donation["consent"]["decision"], "enabled");
    assert_eq!(donation["episode"]["schema"], "ldgr-work-episode/v1");
    assert_eq!(
        donation["episode"]["material"]["work_item"]["title"],
        "Validate the sanitized donation boundary"
    );
    assert_eq!(
        donation["episode"]["material"]["observations"][0]["body"],
        "Sanitized opted-in evidence"
    );
    wait_for_empty_donation_queue(&ldgr_home)?;
    Ok(())
}

#[test]
fn failed_capture_response_retains_exact_payload_for_next_transmit() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    enable(home.path())?;
    queue(home.path(), COMPLETED_NEGATIVE)?;

    let server = CaptureServer::start(vec![ResponsePlan::status(500), ResponsePlan::status(204)])?;
    let client = trusted_client(&server)?;

    let first = client.transmit_pending(home.path(), &CORE_WORK_V1);
    assert_eq!(first.attempted, 1);
    assert_eq!(first.accepted, 0);
    assert_eq!(first.retained, 1);
    assert_eq!(pending_file_count(home.path())?, 1);

    let second = client.transmit_pending(home.path(), &CORE_WORK_V1);
    assert_eq!(second.attempted, 1);
    assert_eq!(second.accepted, 1);
    assert_eq!(second.retained, 0);
    assert_eq!(pending_file_count(home.path())?, 0);

    let requests = server.finish()?;
    assert_eq!(requests.len(), 2);
    assert!(requests.iter().all(|request| request.body == b"[0,1,4]"));
    Ok(())
}

#[test]
fn disabling_consent_during_flush_prevents_the_next_pending_request_immediately(
) -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    enable(home.path())?;
    queue(home.path(), COMPLETED_POSITIVE)?;
    queue(home.path(), COMPLETED_NEGATIVE)?;

    let server = CaptureServer::start(vec![ResponsePlan::disable_before_response(
        204,
        home.path().to_path_buf(),
    )])?;
    let report = trusted_client(&server)?.transmit_pending(home.path(), &CORE_WORK_V1);

    assert!(report.disabled);
    assert_eq!(report.attempted, 1);
    assert_eq!(report.accepted, 1);
    assert_eq!(pending_file_count(home.path())?, 1);
    assert_eq!(
        load_telemetry_consent(home.path())?.decision,
        TelemetryConsentDecision::Disabled
    );

    let requests = server.finish()?;
    assert_eq!(requests.len(), 1);
    assert!(matches!(
        requests[0].body.as_slice(),
        b"[0,1,3]" | b"[0,1,4]"
    ));
    Ok(())
}

#[test]
fn refused_collector_is_harmless_and_leaves_payload_pending() -> anyhow::Result<()> {
    let home = tempfile::tempdir()?;
    enable(home.path())?;
    queue(home.path(), COMPLETED_NEGATIVE)?;
    let origin = unused_loopback_https_origin()?;

    let report = TransmissionClient::new(&origin)?
        .with_max_delay(Duration::ZERO)
        .with_timeout(Duration::from_millis(200))
        .transmit_pending(home.path(), &CORE_WORK_V1);

    assert_eq!(report.attempted, 1);
    assert_eq!(report.accepted, 0);
    assert_eq!(report.retained, 1);
    let preview = preview_pending_sequences(home.path(), &CORE_WORK_V1)?;
    assert_eq!(preview.payloads.len(), 1);
    assert_eq!(preview.payloads[0].raw_array, b"[0,1,4]");
    Ok(())
}

fn trusted_client(server: &CaptureServer) -> anyhow::Result<TransmissionClient> {
    let client = TransmissionClient::new(&server.origin)?
        .with_root_certificate_pem(CAPTURE_CA_PEM)?
        .with_max_delay(Duration::ZERO)
        .with_timeout(Duration::from_secs(3));
    Ok(client)
}

fn enable(home: &Path) -> anyhow::Result<()> {
    save_telemetry_consent(
        home,
        &TelemetryConsent::current(TelemetryConsentDecision::Enabled),
    )?;
    Ok(())
}

fn disable(home: &Path) -> anyhow::Result<()> {
    save_telemetry_consent(
        home,
        &TelemetryConsent::current(TelemetryConsentDecision::Disabled),
    )?;
    Ok(())
}

fn enable_donation(home: &Path) -> anyhow::Result<()> {
    save_telemetry_consent(
        home,
        &TelemetryConsent::current(TelemetryConsentDecision::Disabled)
            .with_donation(TelemetryConsentDecision::Enabled),
    )?;
    Ok(())
}

fn queue(home: &Path, terminal: u16) -> anyhow::Result<()> {
    queue_protocol(home, &CORE_WORK_V1, terminal)
}

fn queue_protocol(home: &Path, protocol: &NumericalProtocol, terminal: u16) -> anyhow::Result<()> {
    let mut buffer = LocalSequenceBuffer::begin_after_commit(home, protocol)?
        .expect("explicitly enabled telemetry should create a buffer");
    buffer.submit_committed(RUNNING)?;
    buffer.submit_committed(terminal)?;
    Ok(())
}

fn write_hostile_pending_fixtures(home: &Path) -> anyhow::Result<()> {
    let route = pending_route(home);
    fs::create_dir_all(&route)?;
    fs::write(
        route.join("metadata-envelope.json"),
        br#"{"project":"secret","repository":"private","command":"ldgr run","path":"/home/me/repo","model":"private-model","source":"fixture","sequence":[0,1,3]}"#,
    )?;
    fs::write(route.join("noncanonical-array.json"), b"[0, 1, 3]")?;
    Ok(())
}

fn pending_route(home: &Path) -> PathBuf {
    home.join(TELEMETRY_PENDING_DIRECTORY).join("core-work/v1")
}

fn pending_file_count(home: &Path) -> anyhow::Result<usize> {
    pending_file_count_for(home, "core-work/v1")
}

fn pending_file_count_for(home: &Path, route: &str) -> anyhow::Result<usize> {
    let route = home.join(TELEMETRY_PENDING_DIRECTORY).join(route);
    if !route.exists() {
        return Ok(0);
    }
    Ok(fs::read_dir(route)?
        .filter_map(Result::ok)
        .filter(|entry| {
            entry
                .metadata()
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        })
        .count())
}

fn wait_for_empty_donation_queue(home: &Path) -> anyhow::Result<()> {
    let route = home.join("experience-donation-pending/experiences/v1");
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let pending = if route.exists() {
            fs::read_dir(&route)?.count()
        } else {
            0
        };
        if pending == 0 {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("timed out waiting for automatic donation cleanup");
        }
        thread::sleep(Duration::from_millis(10));
    }
}

fn wait_for_empty_pending_route(home: &Path, route: &str) -> anyhow::Result<()> {
    let deadline = Instant::now() + Duration::from_secs(5);
    while pending_file_count_for(home, route)? != 0 {
        if Instant::now() >= deadline {
            bail!("timed out waiting for automatic telemetry cleanup");
        }
        thread::sleep(Duration::from_millis(10));
    }
    Ok(())
}

fn unused_loopback_https_origin() -> anyhow::Result<String> {
    let listener = TcpListener::bind("127.0.0.1:0")?;
    let address = listener.local_addr()?;
    drop(listener);
    Ok(format!("https://{address}"))
}

fn telemetry_command(project: &Path) -> anyhow::Result<Command> {
    let mut command = Command::cargo_bin("ldgr")?;
    command
        .current_dir(project)
        .env("HOME", project.join(".ldgr/test-empty-home"))
        .env("LDGR_HOME", project.join(".ldgr/test-empty-ldgr-home"))
        .env("LDGR_NO_AUTOMATIC_TELEMETRY", "1")
        .env(
            "LDGR_ADAPTER_PATH",
            project.join(".ldgr/test-empty-adapters"),
        )
        .env_remove("LDGR_TELEMETRY")
        .env_remove("LDGR_TELEMETRY_COLLECTOR");
    Ok(command)
}

fn automatic_telemetry_command(
    project: &Path,
    server: &CaptureServer,
    ca_path: &Path,
) -> anyhow::Result<Command> {
    let mut command = telemetry_command(project)?;
    command
        .env_remove("LDGR_NO_AUTOMATIC_TELEMETRY")
        .env("LDGR_TELEMETRY_COLLECTOR", &server.origin)
        .env("LDGR_AUTOMATIC_TELEMETRY_ROOT_CA_PEM", ca_path)
        .env("LDGR_AUTOMATIC_TELEMETRY_MAX_DELAY_MS", "0")
        .env("LDGR_AUTOMATIC_TELEMETRY_TIMEOUT_MS", "3000");
    Ok(command)
}

fn run_cli<const ARG_COUNT: usize>(
    project: &Path,
    args: [&str; ARG_COUNT],
) -> anyhow::Result<std::process::Output> {
    let output = telemetry_command(project)?.args(args).output()?;
    if !output.status.success() {
        bail!(
            "ldgr fixture command failed\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(output)
}

fn cli_ldgr_home(project: &Path) -> PathBuf {
    project.join(".ldgr/test-empty-home/.ldgr")
}

fn assert_privacy_preserving_wire_headers(request: &CapturedRequest) {
    assert_eq!(request.header("content-type"), Some("application/json"));
    assert_eq!(
        request
            .header("content-length")
            .and_then(|value| value.parse::<usize>().ok()),
        Some(request.body.len())
    );
    assert!(
        request.header("host").is_some(),
        "HTTP/1.1 host header missing in {:?}",
        request.headers
    );
    // reqwest/hyper currently adds a generic wire-level Accept: */* even though
    // the source Request object does not carry product or SDK headers. That
    // value is non-identifying transport metadata; fail if it ever becomes a
    // more specific fingerprinting surface, while also accepting future
    // transport stacks that successfully suppress the header.
    assert!(
        matches!(request.header("accept"), None | Some("*/*")),
        "unexpected Accept header in telemetry wire request: {:?}",
        request.headers
    );
    assert_no_prohibited_headers(request);
}

fn assert_no_prohibited_headers(request: &CapturedRequest) {
    for (name, _) in &request.headers {
        if let Some(reason) = prohibited_header_reason(name) {
            panic!(
                "prohibited {reason} header {name:?} leaked in {:?}",
                request.headers
            );
        }
    }
}

fn prohibited_header_reason(name: &str) -> Option<&'static str> {
    let name = name.to_ascii_lowercase();
    if name.contains("authorization") {
        return Some("authorization");
    }
    if name.contains("cookie") {
        return Some("cookie");
    }
    if name == "referer" || name == "referrer" {
        return Some("referrer");
    }
    if name.contains("user-agent") {
        return Some("user-agent");
    }
    if name == "forwarded" || name.starts_with("x-forwarded") || name == "x-real-ip" {
        return Some("forwarding/source-address");
    }
    if name.contains("request-id")
        || name.contains("correlation-id")
        || name.contains("trace-id")
        || name == "traceparent"
        || name == "tracestate"
    {
        return Some("request identifier");
    }
    if name.contains("telemetry-sdk") || name == "sentry-trace" || name == "baggage" {
        return Some("telemetry SDK");
    }
    if name.contains("product-version") || name == "ldgr-version" || name == "x-ldgr-version" {
        return Some("product version");
    }
    if name.contains("idempotency") || name.contains("retry") {
        return Some("persistent retry");
    }
    if matches!(
        name.as_str(),
        "x-ldgr-run" | "x-repository" | "x-command" | "x-model" | "x-source"
    ) {
        return Some("LDGR workflow metadata");
    }
    None
}

fn assert_no_prohibited_body_metadata(body: &[u8]) {
    let body = String::from_utf8_lossy(body).to_ascii_lowercase();
    for marker in [
        "project",
        "repo",
        "repository",
        "command",
        "path",
        "model",
        "source",
        "secret",
        "/home",
    ] {
        assert!(
            !body.contains(marker),
            "prohibited body metadata marker {marker:?} leaked in {body:?}"
        );
    }
}

#[derive(Debug)]
struct CaptureServer {
    origin: String,
    handle: JoinHandle<anyhow::Result<Vec<CapturedRequest>>>,
}

impl CaptureServer {
    fn start(plan: Vec<ResponsePlan>) -> anyhow::Result<Self> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        listener.set_nonblocking(true)?;
        let address = listener.local_addr()?;
        let config = Arc::new(capture_tls_config()?);
        let handle = thread::spawn(move || run_capture_server(listener, config, plan));
        Ok(Self {
            origin: format!("https://{address}"),
            handle,
        })
    }

    fn finish(self) -> anyhow::Result<Vec<CapturedRequest>> {
        self.handle
            .join()
            .map_err(|_| anyhow!("capture server thread panicked"))?
    }
}

#[derive(Debug)]
struct ResponsePlan {
    status: u16,
    action: ResponseAction,
}

impl ResponsePlan {
    fn status(status: u16) -> Self {
        Self {
            status,
            action: ResponseAction::None,
        }
    }

    fn disable_before_response(status: u16, home: PathBuf) -> Self {
        Self {
            status,
            action: ResponseAction::DisableTelemetry(home),
        }
    }
}

#[derive(Debug)]
enum ResponseAction {
    None,
    DisableTelemetry(PathBuf),
}

#[derive(Clone, Debug)]
struct CapturedRequest {
    request_line: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
}

impl CapturedRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

fn run_capture_server(
    listener: TcpListener,
    config: Arc<ServerConfig>,
    plan: Vec<ResponsePlan>,
) -> anyhow::Result<Vec<CapturedRequest>> {
    let mut requests = Vec::new();
    for response in plan {
        let stream = accept_with_timeout(&listener, Duration::from_secs(5))?;
        // Windows accepted sockets inherit the listener's nonblocking mode.
        // Return the connected socket to blocking mode before rustls performs
        // its handshake; the explicit read/write timeouts still bound every
        // operation.
        stream.set_nonblocking(false)?;
        stream.set_read_timeout(Some(Duration::from_secs(5)))?;
        stream.set_write_timeout(Some(Duration::from_secs(5)))?;
        let connection = ServerConnection::new(config.clone())?;
        let mut stream = StreamOwned::new(connection, stream);
        let request = read_http_request(&mut stream)?;
        match response.action {
            ResponseAction::None => {}
            ResponseAction::DisableTelemetry(home) => disable(&home)?,
        }
        write_http_response(&mut stream, response.status)?;
        requests.push(request);
    }
    Ok(requests)
}

fn accept_with_timeout(listener: &TcpListener, timeout: Duration) -> anyhow::Result<TcpStream> {
    let deadline = Instant::now() + timeout;
    loop {
        match listener.accept() {
            Ok((stream, _)) => return Ok(stream),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                if Instant::now() >= deadline {
                    bail!("timed out waiting for telemetry request");
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error).context("failed to accept telemetry request"),
        }
    }
}

fn capture_tls_config() -> anyhow::Result<ServerConfig> {
    let certificate = CertificateDer::from_pem_slice(CAPTURE_SERVER_CERTIFICATE_PEM)?;
    let private_key = PrivateKeyDer::from_pem_slice(CAPTURE_SERVER_PRIVATE_KEY_PEM)?;
    ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(vec![certificate], private_key)
        .context("failed to build capture TLS config")
}

fn read_http_request(
    stream: &mut StreamOwned<ServerConnection, TcpStream>,
) -> anyhow::Result<CapturedRequest> {
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = find_header_end(&bytes) {
            break index;
        }
        read_more(stream, &mut bytes)?;
        if bytes.len() > 16 * 1024 {
            bail!("telemetry request headers exceeded test limit");
        }
    };

    let headers_text = std::str::from_utf8(&bytes[..header_end])?;
    let mut lines = headers_text.split("\r\n");
    let request_line = lines
        .next()
        .context("missing HTTP request line")?
        .to_string();
    let headers = lines
        .filter(|line| !line.is_empty())
        .map(|line| {
            let (name, value) = line
                .split_once(':')
                .ok_or_else(|| anyhow!("malformed HTTP header {line:?}"))?;
            Ok((name.to_ascii_lowercase(), value.trim().to_string()))
        })
        .collect::<anyhow::Result<Vec<_>>>()?;

    let content_length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .map(|(_, value)| value.parse::<usize>())
        .transpose()?
        .context("missing content-length")?;
    let body_start = header_end + 4;
    while bytes.len().saturating_sub(body_start) < content_length {
        read_more(stream, &mut bytes)?;
        if bytes.len() - body_start > ldgr_core::telemetry::donation::MAX_EXPERIENCE_DONATION_BYTES
        {
            bail!("telemetry request body exceeded test limit");
        }
    }

    let body = bytes[body_start..body_start + content_length].to_vec();
    Ok(CapturedRequest {
        request_line,
        headers,
        body,
    })
}

fn read_more(
    stream: &mut StreamOwned<ServerConnection, TcpStream>,
    bytes: &mut Vec<u8>,
) -> anyhow::Result<()> {
    let mut buffer = [0_u8; 1024];
    let read = stream.read(&mut buffer)?;
    if read == 0 {
        bail!("connection closed before complete telemetry request");
    }
    bytes.extend_from_slice(&buffer[..read]);
    Ok(())
}

fn write_http_response(
    stream: &mut StreamOwned<ServerConnection, TcpStream>,
    status: u16,
) -> anyhow::Result<()> {
    let reason = match status {
        204 => "No Content",
        500 => "Internal Server Error",
        _ => "Test Status",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
    )?;
    stream.flush()?;
    Ok(())
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}
