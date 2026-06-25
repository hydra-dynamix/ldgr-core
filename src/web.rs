use std::collections::HashMap;
use std::fs;
use std::io::{Read, Seek, SeekFrom, Write};
use std::net::{IpAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;
use std::time::SystemTime;

use anyhow::{bail, Context};
use serde_json::json;

use crate::store::{
    add_global_observation, clear_loop_intervention, get_artifact, get_run, get_work_item_by_slug,
    list_artifacts, list_decisions, list_event_logs, list_observations, list_runs, open_store,
    read_context, read_mission_log, request_loop_intervention, resume_loop, ArtifactKind,
    GlobalObservationKind, LoopInterventionAction,
};

const MAX_HEADER_BYTES: usize = 16 * 1024;
const MAX_BODY_BYTES: usize = 256 * 1024;
const READ_TIMEOUT: Duration = Duration::from_secs(5);
const WORKER_COUNT: usize = 8;
const PENDING_CONNECTION_LIMIT: usize = 64;
const MISSION_LOG_ENTRY_LIMIT: i64 = 30;

#[derive(Clone, Debug, Default)]
pub struct WebOptions {
    pub unsafe_expose: bool,
    pub control_token: String,
}

pub fn serve(
    db_path: &Path,
    artifact_root: &Path,
    host: &str,
    port: u16,
    options: WebOptions,
) -> anyhow::Result<()> {
    validate_exposure_options(host, &options)?;
    let listener = TcpListener::bind((host, port))
        .with_context(|| format!("failed to bind web cockpit to {host}:{port}"))?;
    let address = listener
        .local_addr()
        .context("failed to read web cockpit listener address")?;
    println!("ldgr web cockpit listening on http://{address}");
    println!(
        "open with control token: http://{address}/?control_token={}",
        options.control_token
    );

    let (connection_sender, connection_receiver) =
        mpsc::sync_channel::<TcpStream>(PENDING_CONNECTION_LIMIT);
    let connection_receiver = Arc::new(Mutex::new(connection_receiver));
    for _ in 0..WORKER_COUNT {
        let connection_receiver = Arc::clone(&connection_receiver);
        let db_path = db_path.to_path_buf();
        let artifact_root = artifact_root.to_path_buf();
        let options = options.clone();
        thread::spawn(move || loop {
            let received = connection_receiver
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner())
                .recv();
            let Ok(stream) = received else {
                break;
            };
            if let Err(error) = handle_connection(stream, &db_path, &artifact_root, &options) {
                eprintln!("web cockpit request failed: {error:#}");
            }
        });
    }

    for stream in listener.incoming() {
        match stream {
            Ok(stream) => {
                if let Err(mpsc::TrySendError::Full(mut rejected)) =
                    connection_sender.try_send(stream)
                {
                    let _ = write_response(
                        &mut rejected,
                        "503 Service Unavailable",
                        "text/plain; charset=utf-8",
                        b"server is at capacity; retry shortly",
                    );
                }
            }
            Err(error) => eprintln!("web cockpit connection failed: {error}"),
        }
    }
    Ok(())
}

fn validate_exposure_options(host: &str, options: &WebOptions) -> anyhow::Result<()> {
    if is_loopback_host(host) {
        return Ok(());
    }
    if !options.unsafe_expose {
        bail!(
            "refusing to expose web cockpit on non-loopback host {host}; use --unsafe-expose with --control-token to acknowledge the risk"
        );
    }
    if options.control_token.trim().is_empty() {
        bail!("--control-token is required when --unsafe-expose is used");
    }
    Ok(())
}

fn is_loopback_host(host: &str) -> bool {
    if host.eq_ignore_ascii_case("localhost") {
        return true;
    }
    host.parse::<IpAddr>()
        .map(|address| address.is_loopback())
        .unwrap_or(false)
}

#[derive(Debug)]
struct HttpRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

impl HttpRequest {
    fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|value| value.as_str())
    }
}

#[derive(Debug)]
struct HttpError {
    status: &'static str,
    message: String,
}

impl HttpError {
    fn new(status: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            message: message.into(),
        }
    }
}

#[derive(Debug)]
struct WebApiError {
    status: &'static str,
    code: &'static str,
    message: String,
}

impl WebApiError {
    fn new(status: &'static str, code: &'static str, message: impl Into<String>) -> Self {
        Self {
            status,
            code,
            message: message.into(),
        }
    }

    fn not_found(message: impl Into<String>) -> Self {
        Self::new("404 Not Found", "not_found", message)
    }

    fn from_error(error: anyhow::Error) -> Self {
        let message = format!("{error:#}");
        if message.contains(" not found") {
            Self::not_found(message)
        } else if message.contains("missing or invalid X-LDGR-Control-Token")
            || message.contains("Origin header does not match")
        {
            Self::new("403 Forbidden", "forbidden", message)
        } else if message.contains("POST content type must be") {
            Self::new(
                "415 Unsupported Media Type",
                "unsupported_media_type",
                message,
            )
        } else {
            Self::new("400 Bad Request", "bad_request", message)
        }
    }
}

fn handle_connection(
    mut stream: TcpStream,
    db_path: &Path,
    artifact_root: &Path,
    options: &WebOptions,
) -> anyhow::Result<()> {
    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            return write_response(
                &mut stream,
                error.status,
                "text/plain; charset=utf-8",
                error.message.as_bytes(),
            );
        }
    };

    let result = if request.method == "POST" {
        handle_post(&mut stream, db_path, artifact_root, &request, options)
    } else if request.method == "GET" {
        handle_get(&mut stream, db_path, artifact_root, &request.path)
    } else if request.path.starts_with("/api/") {
        write_api_error(
            &mut stream,
            WebApiError::new(
                "405 Method Not Allowed",
                "method_not_allowed",
                "method not allowed",
            ),
        )
    } else {
        write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"method not allowed",
        )
    };

    if let Err(error) = result {
        if request.path.starts_with("/api/") {
            write_api_error(&mut stream, WebApiError::from_error(error))?;
        } else {
            write_response(
                &mut stream,
                "400 Bad Request",
                "text/plain; charset=utf-8",
                format!("{error:#}").as_bytes(),
            )?;
        }
    }
    Ok(())
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpError> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| {
            HttpError::new(
                "400 Bad Request",
                format!("failed to set read timeout: {error}"),
            )
        })?;
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = find_header_end(&bytes) {
            break index;
        }
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(HttpError::new(
                "431 Request Header Fields Too Large",
                "request headers are too large",
            ));
        }
        let mut chunk = [0_u8; 1024];
        let count = stream.read(&mut chunk).map_err(|error| {
            HttpError::new(
                "400 Bad Request",
                format!("failed to read HTTP request: {error}"),
            )
        })?;
        if count == 0 {
            return Err(HttpError::new(
                "400 Bad Request",
                "request ended before headers were complete",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    };

    let header_bytes = &bytes[..header_end];
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| HttpError::new("400 Bad Request", "request headers are not valid UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP target"))?
        .to_string();
    let version = parts
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP version"))?;
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return Err(HttpError::new(
            "400 Bad Request",
            "invalid HTTP request line",
        ));
    }
    if !target.starts_with('/') {
        return Err(HttpError::new(
            "400 Bad Request",
            "HTTP target must be an absolute path",
        ));
    }
    let path = target.split('?').next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| HttpError::new("400 Bad Request", "invalid HTTP header"))?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let body_start = header_end + 4;
    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| HttpError::new("400 Bad Request", "Content-Length must be an integer"))?,
        None => 0,
    };
    if content_length > MAX_BODY_BYTES {
        return Err(HttpError::new(
            "413 Payload Too Large",
            "request body is too large",
        ));
    }
    if method == "POST" && !headers.contains_key("content-length") {
        return Err(HttpError::new(
            "411 Length Required",
            "POST requests must include Content-Length",
        ));
    }

    let mut body = bytes.get(body_start..).unwrap_or_default().to_vec();
    while body.len() < content_length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).map_err(|error| {
            HttpError::new(
                "400 Bad Request",
                format!("failed to read HTTP request body: {error}"),
            )
        })?;
        if count == 0 {
            return Err(HttpError::new(
                "400 Bad Request",
                "request body ended before Content-Length bytes were received",
            ));
        }
        body.extend_from_slice(&chunk[..count]);
        if body.len() > MAX_BODY_BYTES {
            return Err(HttpError::new(
                "413 Payload Too Large",
                "request body is too large",
            ));
        }
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

fn handle_get(
    stream: &mut TcpStream,
    db_path: &Path,
    artifact_root: &Path,
    path: &str,
) -> anyhow::Result<()> {
    match path {
        "/" | "/index.html" => write_response(
            stream,
            "200 OK",
            "text/html; charset=utf-8",
            INDEX_HTML.as_bytes(),
        ),
        "/app.css" => write_response(
            stream,
            "200 OK",
            "text/css; charset=utf-8",
            APP_CSS.as_bytes(),
        ),
        "/app.js" => write_response(
            stream,
            "200 OK",
            "application/javascript; charset=utf-8",
            APP_JS.as_bytes(),
        ),
        "/api/context" => {
            let connection = open_store(db_path)?;
            let context = read_context(&connection)?;
            write_json(stream, &context)
        }
        "/api/mission-log" => {
            let connection = open_store(db_path)?;
            let mission_log = read_mission_log(&connection, MISSION_LOG_ENTRY_LIMIT)?;
            write_json(stream, &mission_log)
        }
        "/api/logs" => serve_logs(stream, db_path),
        "/api/conduct/waves" => serve_conduct_waves(stream, db_path),
        api_path if api_path.starts_with("/api/runs/") => {
            serve_run_detail(stream, db_path, api_path)
        }
        api_path if api_path.starts_with("/api/work/") => {
            serve_work_detail(stream, db_path, api_path)
        }
        artifact_path if artifact_path.starts_with("/api/artifacts/") => {
            serve_artifact(stream, db_path, artifact_root, artifact_path)
        }
        api_path if api_path.starts_with("/api/") => write_api_error(
            stream,
            WebApiError::not_found(format!("{api_path} not found")),
        ),
        page_path
            if page_path.starts_with("/runs/")
                || page_path.starts_with("/work/")
                || page_path.starts_with("/artifacts/")
                || page_path == "/logs" =>
        {
            write_response(
                stream,
                "200 OK",
                "text/html; charset=utf-8",
                INDEX_HTML.as_bytes(),
            )
        }
        _ => write_response(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found",
        ),
    }
}

fn handle_post(
    stream: &mut TcpStream,
    db_path: &Path,
    artifact_root: &Path,
    request: &HttpRequest,
    options: &WebOptions,
) -> anyhow::Result<()> {
    enforce_control_guard(request, options)?;
    let content_type = request.header("content-type").unwrap_or_default();
    if !content_type
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .eq_ignore_ascii_case("application/x-www-form-urlencoded")
    {
        bail!("POST content type must be application/x-www-form-urlencoded");
    }
    let body = std::str::from_utf8(&request.body).context("POST body is not valid UTF-8")?;
    let form = parse_form_body(body)?;
    let connection = open_store(db_path)?;
    match request.path.as_str() {
        "/api/loop/start" => serve_loop_start(stream, db_path, artifact_root, &form),
        "/api/loop/interventions/pause" => {
            let intervention = request_loop_intervention(
                &connection,
                LoopInterventionAction::Pause,
                required_form_value(&form, "reason")?,
                None,
                optional_form_value(&form, "requested_by"),
            )?;
            write_json(stream, &json!({ "intervention": intervention }))
        }
        "/api/loop/interventions/stop" => {
            let intervention = request_loop_intervention(
                &connection,
                LoopInterventionAction::Stop,
                required_form_value(&form, "reason")?,
                None,
                optional_form_value(&form, "requested_by"),
            )?;
            write_json(stream, &json!({ "intervention": intervention }))
        }
        "/api/loop/interventions/steer" => {
            let intervention = request_loop_intervention(
                &connection,
                LoopInterventionAction::Steer,
                required_form_value(&form, "reason")?,
                Some(required_form_value(&form, "instruction")?),
                optional_form_value(&form, "requested_by"),
            )?;
            write_json(stream, &json!({ "intervention": intervention }))
        }
        "/api/loop/interventions/resume" => {
            let interventions = resume_loop(&connection, required_form_value(&form, "reason")?)?;
            write_json(stream, &json!({ "interventions": interventions }))
        }
        clear_path if clear_path.starts_with("/api/loop/interventions/clear/") => {
            let intervention_id: i64 = clear_path
                .trim_start_matches("/api/loop/interventions/clear/")
                .parse()
                .context("intervention id must be an integer")?;
            let intervention = clear_loop_intervention(
                &connection,
                intervention_id,
                optional_form_value(&form, "reason"),
            )?;
            write_json(stream, &json!({ "intervention": intervention }))
        }
        api_path if api_path.starts_with("/api/") => write_api_error(
            stream,
            WebApiError::not_found(format!("{api_path} not found")),
        ),
        _ => write_response(
            stream,
            "404 Not Found",
            "text/plain; charset=utf-8",
            b"not found",
        ),
    }
}

fn enforce_control_guard(request: &HttpRequest, options: &WebOptions) -> anyhow::Result<()> {
    let supplied = request.header("x-ldgr-control-token").unwrap_or_default();
    if supplied != options.control_token {
        bail!("missing or invalid X-LDGR-Control-Token header");
    }

    if let Some(origin) = request.header("origin") {
        let request_host = request.header("host").unwrap_or_default();
        if !origin_matches_host(origin, request_host) {
            bail!("Origin header does not match the web cockpit host");
        }
    }
    Ok(())
}

pub fn generate_control_token() -> anyhow::Result<String> {
    let mut bytes = [0_u8; 32];
    getrandom::getrandom(&mut bytes)
        .map_err(|error| anyhow::anyhow!("failed to generate web control token: {error}"))?;
    let mut token = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        use std::fmt::Write as _;
        write!(&mut token, "{byte:02x}").expect("writing to String cannot fail");
    }
    Ok(token)
}

fn origin_matches_host(origin: &str, request_host: &str) -> bool {
    let Some((_, rest)) = origin.split_once("://") else {
        return false;
    };
    let origin_host = rest.split('/').next().unwrap_or_default();
    !origin_host.is_empty() && origin_host.eq_ignore_ascii_case(request_host)
}

fn serve_loop_start(
    stream: &mut TcpStream,
    db_path: &Path,
    artifact_root: &Path,
    form: &[(String, String)],
) -> anyhow::Result<()> {
    let prompt_source = loop_prompt_source_args(form)?;
    let dry_run = optional_form_value(form, "dry_run") == Some("true");
    let stream_agent_output = optional_form_value(form, "stream_agent_output") == Some("true");
    let project_complete_requested =
        optional_form_value(form, "project_complete_requested") == Some("true");
    let agent = optional_nonempty_form_value(form, "agent").unwrap_or("codex");
    if agent != "codex" {
        bail!("agent must be codex; use agent_argv for custom agents");
    }
    let agent_argv = optional_nonempty_form_value(form, "agent_argv");
    if agent_argv.is_some() && optional_nonempty_form_value(form, "agent").is_some() {
        bail!("agent and agent_argv are mutually exclusive");
    }
    if let Some(agent_argv) = agent_argv {
        validate_argv_json("agent_argv", agent_argv)?;
    }
    let max_iterations = optional_form_value(form, "max_iterations")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("1");
    let max_iterations_value: u32 = max_iterations
        .parse()
        .context("max_iterations must be a positive integer")?;
    if max_iterations_value == 0 {
        bail!("max_iterations must be at least 1");
    }
    let agent_timeout_seconds = optional_form_value(form, "agent_timeout_seconds")
        .filter(|value| !value.trim().is_empty())
        .unwrap_or("43200");
    let agent_timeout_value: u64 = agent_timeout_seconds
        .parse()
        .context("agent_timeout_seconds must be a positive integer")?;
    if agent_timeout_value == 0 {
        bail!("agent_timeout_seconds must be at least 1");
    }
    if project_complete_requested {
        validate_argv_json("audit_argv", required_form_value(form, "audit_argv")?)?;
    } else if let Some(audit_argv) = optional_form_value(form, "audit_argv") {
        if !audit_argv.trim().is_empty() {
            validate_argv_json("audit_argv", audit_argv)?;
        }
    }

    let executable = std::env::current_exe().context("failed to resolve current executable")?;
    let mut command = Command::new(executable);
    command
        .arg("--db")
        .arg(db_path)
        .arg("--artifact-root")
        .arg(artifact_root)
        .arg("loop")
        .arg("run");
    for argument in &prompt_source {
        command.arg(argument);
    }
    if dry_run {
        command.arg("--dry-run");
    } else if let Some(agent_argv) = agent_argv {
        command.arg("--agent-argv").arg(agent_argv);
    } else {
        command.arg("--agent").arg(agent);
    }
    if stream_agent_output {
        command.arg("--stream-agent-output");
    }
    if max_iterations_value != 1 {
        command.arg("--max-iterations").arg(max_iterations);
    }
    if agent_timeout_value != 43_200 {
        command
            .arg("--agent-timeout-seconds")
            .arg(agent_timeout_seconds);
    }
    if project_complete_requested {
        command.arg("--project-complete-requested");
        command
            .arg("--audit-argv")
            .arg(required_form_value(form, "audit_argv")?);
    } else if let Some(audit_argv) = optional_form_value(form, "audit_argv") {
        if !audit_argv.trim().is_empty() {
            command.arg("--audit-argv").arg(audit_argv);
        }
    }
    let rendered_command = render_command_redacted(&command);
    let child = match command
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(error) => {
            let connection = open_store(db_path)?;
            add_global_observation(
                &connection,
                GlobalObservationKind::Notification,
                &format!(
                    "Web cockpit failed to start loop runtime process; dry_run={dry_run}; project_complete_requested={project_complete_requested}; command={rendered_command}; error={error}"
                ),
                Some("web-loop-runtime-start-failure"),
            )?;
            return Err(error).context("failed to start loop runtime process");
        }
    };
    let pid = child.id();
    let connection = open_store(db_path)?;
    let observation = add_global_observation(
        &connection,
        GlobalObservationKind::Notification,
        &format!(
            "Web cockpit started loop runtime process pid={pid}; dry_run={dry_run}; project_complete_requested={project_complete_requested}; command={rendered_command}"
        ),
        Some("web-loop-start"),
    )?;
    write_json(
        stream,
        &json!({
            "pid": pid,
            "status": "spawned",
            "message": "Loop runtime process spawned; watch the loop state and event log for run progress or startup failures.",
            "launch_observation_id": observation.id,
        }),
    )?;
    retain_loop_runtime_child(db_path.to_path_buf(), child, rendered_command);
    Ok(())
}

fn loop_prompt_source_args(form: &[(String, String)]) -> anyhow::Result<Vec<String>> {
    let prompt = optional_nonempty_form_value(form, "prompt");
    let prompt_slug = optional_nonempty_form_value(form, "prompt_slug");
    let bundle = optional_nonempty_form_value(form, "bundle");
    let count = [prompt, prompt_slug, bundle]
        .into_iter()
        .filter(|value| value.is_some())
        .count();
    if count != 1 {
        bail!("exactly one of prompt, prompt_slug, or bundle must be provided");
    }
    if let Some(prompt) = prompt {
        validate_prompt_path(prompt)?;
        return Ok(vec!["--prompt".to_owned(), prompt.to_owned()]);
    }
    if let Some(prompt_slug) = prompt_slug {
        return Ok(vec!["--prompt-slug".to_owned(), prompt_slug.to_owned()]);
    }
    let bundle = bundle.expect("bundle is present when prompt and prompt_slug are absent");
    let mut args = vec!["--bundle".to_owned(), bundle.to_owned()];
    if let Some(prompt_role) = optional_nonempty_form_value(form, "prompt_role") {
        args.push("--prompt-role".to_owned());
        args.push(prompt_role.to_owned());
    }
    Ok(args)
}

fn render_command_redacted(command: &Command) -> String {
    let mut parts = vec![shell_debug(command.get_program())];
    for argument in command.get_args() {
        parts.push(shell_debug(argument));
    }
    parts.join(" ")
}

fn shell_debug(value: &std::ffi::OsStr) -> String {
    format!("{:?}", value)
}

fn retain_loop_runtime_child(db_path: PathBuf, mut child: Child, rendered_command: String) {
    let pid = child.id();
    thread::spawn(move || {
        let status_result = child.wait();
        let connection = match open_store(&db_path) {
            Ok(connection) => connection,
            Err(error) => {
                eprintln!(
                    "web cockpit failed to open store after loop runtime process pid={pid}: {error:#}"
                );
                return;
            }
        };
        let body = match status_result {
            Ok(status) => {
                let status_text = status
                    .code()
                    .map(|code| format!("exit_code={code}"))
                    .unwrap_or_else(|| "terminated_by_signal=true".to_owned());
                format!(
                    "Web cockpit loop runtime process pid={pid} exited with {status_text}; success={}; command={rendered_command}",
                    status.success()
                )
            }
            Err(error) => format!(
                "Web cockpit failed to wait for loop runtime process pid={pid}; command={rendered_command}; error={error}"
            ),
        };
        if let Err(error) = add_global_observation(
            &connection,
            GlobalObservationKind::Notification,
            &body,
            Some("web-loop-runtime-exit"),
        ) {
            eprintln!(
                "web cockpit failed to record loop runtime process pid={pid} exit: {error:#}"
            );
        }
    });
}

fn validate_prompt_path(prompt: &str) -> anyhow::Result<()> {
    let path = Path::new(prompt);
    if !path.exists() {
        bail!("prompt path does not exist: {prompt}");
    }
    if !path.is_file() {
        bail!("prompt path is not a file: {prompt}");
    }
    Ok(())
}

fn validate_argv_json(field: &str, value: &str) -> anyhow::Result<()> {
    let argv: Vec<String> = serde_json::from_str(value)
        .with_context(|| format!("{field} must be a JSON array of strings"))?;
    if argv.is_empty() {
        bail!("{field} must not be empty");
    }
    Ok(())
}

fn parse_form_body(body: &str) -> anyhow::Result<Vec<(String, String)>> {
    body.split('&')
        .filter(|part| !part.is_empty())
        .map(|part| {
            let (key, value) = part.split_once('=').unwrap_or((part, ""));
            Ok((
                percent_decode_form_value(key)?,
                percent_decode_form_value(value)?,
            ))
        })
        .collect()
}

fn required_form_value<'a>(form: &'a [(String, String)], key: &str) -> anyhow::Result<&'a str> {
    let value = optional_form_value(form, key).unwrap_or_default();
    if value.trim().is_empty() {
        bail!("{key} must not be empty");
    }
    Ok(value)
}

fn optional_form_value<'a>(form: &'a [(String, String)], key: &str) -> Option<&'a str> {
    form.iter()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.as_str())
}

fn optional_nonempty_form_value<'a>(form: &'a [(String, String)], key: &str) -> Option<&'a str> {
    optional_form_value(form, key).filter(|value| !value.trim().is_empty())
}

fn percent_decode_form_value(value: &str) -> anyhow::Result<String> {
    percent_decode_path_component(&value.replace('+', " "))
}

fn serve_logs(stream: &mut TcpStream, db_path: &Path) -> anyhow::Result<()> {
    let connection = open_store(db_path)?;
    let events = list_event_logs(&connection, 100)?;
    write_json(stream, &json!({ "events": events }))
}

fn serve_run_detail(stream: &mut TcpStream, db_path: &Path, path: &str) -> anyhow::Result<()> {
    let run_id: i64 = path
        .trim_start_matches("/api/runs/")
        .parse()
        .context("run id must be an integer")?;
    let connection = open_store(db_path)?;
    let run = get_run(&connection, run_id)?;
    let observations = list_observations(&connection, Some(run_id), 100)?;
    let artifacts = list_artifacts(&connection, Some(run_id), 100)?;
    write_json(
        stream,
        &json!({
            "run": run,
            "observations": observations,
            "artifacts": artifacts,
        }),
    )
}

fn serve_work_detail(stream: &mut TcpStream, db_path: &Path, path: &str) -> anyhow::Result<()> {
    let slug = percent_decode_path_component(path.trim_start_matches("/api/work/"))?;
    let connection = open_store(db_path)?;
    let work_item = get_work_item_by_slug(&connection, &slug)?;
    let runs: Vec<_> = list_runs(&connection, None)?
        .into_iter()
        .filter(|run| run.work_slug == slug.as_str())
        .collect();
    let decisions = list_decisions(&connection, Some(&slug), 100)?;
    write_json(
        stream,
        &json!({
            "work_item": work_item,
            "runs": runs,
            "decisions": decisions,
        }),
    )
}

fn percent_decode_path_component(component: &str) -> anyhow::Result<String> {
    let bytes = component.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            b'%' => {
                if index + 2 >= bytes.len() {
                    bail!("invalid percent-encoded path component");
                }
                let high = hex_value(bytes[index + 1])?;
                let low = hex_value(bytes[index + 2])?;
                decoded.push((high << 4) | low);
                index += 3;
            }
            byte => {
                decoded.push(byte);
                index += 1;
            }
        }
    }
    String::from_utf8(decoded).context("path component is not valid UTF-8")
}

fn hex_value(byte: u8) -> anyhow::Result<u8> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => bail!("invalid percent-encoded path component"),
    }
}

fn serve_conduct_waves(stream: &mut TcpStream, db_path: &Path) -> anyhow::Result<()> {
    let root = std::env::current_dir()?.join(".ldgr-conduct");
    if !root.is_dir() {
        return write_json(
            stream,
            &serde_json::json!({
                "available": false,
                "message": ".ldgr-conduct directory not found",
                "batches": [],
                "feeds": []
            }),
        );
    }
    let worker_root = root.join("workers");
    let worktree_root = root.join("worktrees");
    let mut batches = Vec::new();
    for batch_entry in read_dir_sorted(&worker_root)? {
        if !batch_entry.path().is_dir() {
            continue;
        }
        let batch_id = batch_entry.file_name().to_string_lossy().to_string();
        let mut workers = Vec::new();
        for worker_entry in read_dir_sorted(&batch_entry.path())? {
            if !worker_entry.path().is_dir() {
                continue;
            }
            let worker_id = worker_entry.file_name().to_string_lossy().to_string();
            workers.push(conduct_worker_summary(
                db_path,
                &worktree_root,
                &batch_id,
                &worker_id,
                &worker_entry.path(),
            ));
        }
        batches.push(serde_json::json!({
            "batch_id": batch_id,
            "workers": workers,
            "worker_count": workers.len(),
        }));
    }
    let feeds = collect_conduct_feeds(&root, 16)?;
    write_json(
        stream,
        &serde_json::json!({
            "available": true,
            "batches": batches,
            "feeds": feeds,
        }),
    )
}

fn conduct_worker_summary(
    parent_db_path: &Path,
    worktree_root: &Path,
    batch_id: &str,
    worker_id: &str,
    worker_dir: &Path,
) -> serde_json::Value {
    let worker_db = worker_dir.join("ldgr.db");
    let artifact_root = worker_dir.join("artifacts");
    let worktree = find_worker_worktree(worktree_root, batch_id, worker_id);
    let ticket_slug = worktree
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| {
            name.strip_prefix(&format!("{worker_id}-"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    let db_summary = worker_db_summary(&worker_db);
    let git_status = serde_json::json!({
        "available": false,
        "summary": "git status skipped in cockpit summary; use worker worktree for full inspection",
    });
    let feeds = collect_worker_feeds_fast(&artifact_root, 4).unwrap_or_default();
    let parent_seen = parent_db_path.exists();
    serde_json::json!({
        "worker_id": worker_id,
        "ticket_slug": ticket_slug,
        "worker_db": worker_db.display().to_string(),
        "artifact_root": artifact_root.display().to_string(),
        "worktree": worktree.map(|path| path.display().to_string()),
        "worker_ldgr": db_summary,
        "git": git_status,
        "feeds": feeds,
        "parent_db_available": parent_seen,
    })
}

fn worker_db_summary(worker_db: &Path) -> serde_json::Value {
    if !worker_db.is_file() {
        return serde_json::json!({"readable": false, "error": "worker DB not found"});
    }
    match open_store(worker_db).and_then(|connection| {
        let active_runs = list_runs(&connection, Some(crate::store::RunStatus::Running))?;
        let runs = list_runs(&connection, None)?;
        Ok((active_runs, runs))
    }) {
        Ok((active_runs, runs)) => {
            let latest = runs.last();
            serde_json::json!({
                "readable": true,
                "phase": if active_runs.is_empty() { "terminal" } else { "started" },
                "run_id": latest.map(|run| run.run_id),
                "work_slug": latest.map(|run| run.work_slug.clone()),
                "terminal_status": latest.and_then(|run| active_runs.is_empty().then(|| run.status.as_str().to_string())),
                "needs_decision": active_runs.is_empty() && latest.is_some(),
                "active_run_count": active_runs.len(),
                "latest_observation": null,
                "latest_decision": null,
            })
        }
        Err(error) => serde_json::json!({"readable": false, "error": format!("{error:#}")}),
    }
}

fn read_dir_sorted(path: &Path) -> anyhow::Result<Vec<std::fs::DirEntry>> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn find_worker_worktree(worktree_root: &Path, batch_id: &str, worker_id: &str) -> Option<PathBuf> {
    let batch_root = worktree_root.join(batch_id);
    read_dir_sorted(&batch_root)
        .ok()?
        .into_iter()
        .find_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            (entry.path().is_dir() && name.starts_with(&format!("{worker_id}-")))
                .then(|| entry.path())
        })
}

fn collect_conduct_feeds(root: &Path, limit: usize) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut candidates = Vec::new();
    collect_feed_candidates(&root.join("logs"), &mut candidates)?;
    candidates.sort_by(|left, right| right.modified.cmp(&left.modified));
    Ok(candidates
        .into_iter()
        .take(limit)
        .map(feed_json)
        .collect::<Vec<_>>())
}

fn collect_worker_feeds_fast(root: &Path, limit: usize) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut candidates = Vec::new();
    collect_shallow_feed_candidates(&root.join("process"), &mut candidates, 2)?;
    collect_shallow_feed_candidates(&root.join("agent-output"), &mut candidates, 2)?;
    candidates.sort_by(|left, right| right.modified.cmp(&left.modified));
    Ok(candidates
        .into_iter()
        .filter(|candidate| {
            candidate
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.ends_with(".log")
                        || matches!(
                            name,
                            "stdout.txt"
                                | "stderr.txt"
                                | "transcript.md"
                                | "diagnostics.jsonl"
                                | "result.toml"
                        )
                })
        })
        .take(limit)
        .map(feed_json)
        .collect::<Vec<_>>())
}

fn collect_shallow_feed_candidates(
    path: &Path,
    candidates: &mut Vec<FeedCandidate>,
    depth: usize,
) -> anyhow::Result<()> {
    if depth == 0 || !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_shallow_feed_candidates(&path, candidates, depth - 1)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.ends_with(".log")
            || matches!(
                name,
                "stdout.txt" | "stderr.txt" | "transcript.md" | "diagnostics.jsonl" | "result.toml"
            ))
        {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        candidates.push(FeedCandidate {
            path,
            modified: metadata.modified().ok(),
            size: metadata.len(),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct FeedCandidate {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
}

fn collect_feed_candidates(path: &Path, candidates: &mut Vec<FeedCandidate>) -> anyhow::Result<()> {
    if !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_feed_candidates(&path, candidates)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.ends_with(".log")
            || matches!(
                name,
                "stdout.txt" | "stderr.txt" | "transcript.md" | "diagnostics.jsonl" | "result.toml"
            ))
        {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        candidates.push(FeedCandidate {
            path,
            modified: metadata.modified().ok(),
            size: metadata.len(),
        });
    }
    Ok(())
}

fn feed_json(candidate: FeedCandidate) -> serde_json::Value {
    serde_json::json!({
        "path": candidate.path.display().to_string(),
        "label": candidate.path.file_name().and_then(|name| name.to_str()).unwrap_or("feed"),
        "size": candidate.size,
        "modified_sort": candidate.modified.and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|duration| duration.as_secs()),
        "tail": tail_file(&candidate.path, 12000).unwrap_or_else(|error| format!("failed to read feed: {error:#}")),
    })
}

fn tail_file(path: &Path, max_bytes: usize) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start).min(max_bytes as u64) as usize);
    file.take(max_bytes as u64).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

fn serve_artifact(
    stream: &mut TcpStream,
    db_path: &Path,
    artifact_root: &Path,
    path: &str,
) -> anyhow::Result<()> {
    let suffix = path.trim_start_matches("/api/artifacts/");
    let (id_text, raw) = suffix
        .strip_suffix("/raw")
        .map(|id| (id, true))
        .unwrap_or((suffix, false));
    let artifact_id: i64 = id_text.parse().context("artifact id must be an integer")?;
    let connection = open_store(db_path)?;
    let artifact = get_artifact(&connection, artifact_id)?;
    let artifact_path = checked_artifact_path(artifact_root, &artifact.path)?;

    if raw {
        let bytes = fs::read(&artifact_path)
            .with_context(|| format!("failed to read artifact {}", artifact.path.display()))?;
        return write_response(
            stream,
            "200 OK",
            content_type_for_path(&artifact_path),
            &bytes,
        );
    }

    let content = if matches!(artifact.kind, ArtifactKind::Image) {
        String::new()
    } else {
        fs::read_to_string(&artifact_path).unwrap_or_else(|_| String::new())
    };
    let viewer = match &artifact.kind {
        ArtifactKind::Json => "json",
        ArtifactKind::Csv => "csv",
        ArtifactKind::Report => "markdown",
        ArtifactKind::Image => "image",
        ArtifactKind::Other => viewer_for_artifact_path(&artifact.path),
        ArtifactKind::Custom(kind) => viewer_for_artifact_kind_or_path(kind, &artifact.path),
    };
    let body = serde_json::to_vec_pretty(&json!({
        "artifact": artifact,
        "viewer": viewer,
        "content": content,
        "raw_url": format!("/api/artifacts/{artifact_id}/raw"),
    }))?;
    write_response(stream, "200 OK", "application/json; charset=utf-8", &body)
}

fn checked_artifact_path(artifact_root: &Path, artifact_path: &Path) -> anyhow::Result<PathBuf> {
    let root = artifact_root.canonicalize().with_context(|| {
        format!(
            "failed to resolve artifact root {}",
            artifact_root.display()
        )
    })?;
    if artifact_path.is_absolute() {
        bail!("artifact records must be relative to the artifact root");
    }
    let candidate = root.join(artifact_path);
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve artifact {}", artifact_path.display()))?;
    if !resolved.starts_with(&root) {
        bail!(
            "artifact path escapes artifact root: {}",
            artifact_path.display()
        );
    }
    Ok(resolved)
}

fn write_json<T: serde::Serialize>(stream: &mut TcpStream, value: &T) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(value)?;
    write_response(stream, "200 OK", "application/json; charset=utf-8", &body)
}

fn write_api_error(stream: &mut TcpStream, error: WebApiError) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(&json!({
        "error": {
            "code": error.code,
            "message": error.message,
        }
    }))?;
    write_response(
        stream,
        error.status,
        "application/json; charset=utf-8",
        &body,
    )
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         X-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'\r\n\
         \r\n",
        body.len()
    )
    .context("failed to write HTTP headers")?;
    stream
        .write_all(body)
        .context("failed to write HTTP body")?;
    Ok(())
}

fn viewer_for_artifact_kind_or_path(kind: &str, path: &Path) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "json" => "json",
        "csv" => "csv",
        "report" | "markdown" | "md" | "text" | "txt" | "log" | "patch" | "diff" | "toml"
        | "yaml" | "yml" => "markdown",
        "image" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
        _ => viewer_for_artifact_path(path),
    }
}

fn viewer_for_artifact_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => "json",
        "csv" => "csv",
        "md" | "markdown" | "txt" | "log" | "patch" | "diff" | "toml" | "yaml" | "yml" => {
            "markdown"
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
        _ => "metadata",
    }
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "css" => "text/css; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "gif" => "image/gif",
        "htm" | "html" => "text/html; charset=utf-8",
        "jpg" | "jpeg" => "image/jpeg",
        "json" => "application/json; charset=utf-8",
        "md" | "markdown" | "txt" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

const INDEX_HTML: &str = include_str!("web_assets/index.html");

const APP_CSS: &str = include_str!("web_assets/app.css");

const APP_JS: &str = include_str!("web_assets/app.js");

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        checked_artifact_path, loop_prompt_source_args, validate_exposure_options, WebOptions,
        APP_JS, INDEX_HTML,
    };

    #[test]
    fn cockpit_asset_omits_adapter_research_surface() {
        assert!(!INDEX_HTML.contains("Due revalidation"));
        assert!(!INDEX_HTML.contains(r#"id="due-revalidation""#));
        assert!(!INDEX_HTML.contains("Failures"));
        assert!(!INDEX_HTML.contains("Evidence"));
        assert!(!INDEX_HTML.contains("Tools"));
        assert!(!APP_JS.contains("due_fact_revalidation_policies"));
        assert!(!APP_JS.contains("renderDueRevalidation"));
        assert!(!APP_JS.contains("revalidation_expectation_slug"));
        assert!(!APP_JS.contains("readinessAudit"));
        assert!(!APP_JS.contains("renderFailure"));
        assert!(!APP_JS.contains("/api/tools/"));
    }

    #[test]
    fn cockpit_asset_route_logic_decodes_before_reencoding() {
        assert!(APP_JS.contains("function encodedRouteSegment"));
        assert!(APP_JS.contains("decodeURIComponent(segment)"));
        assert!(!APP_JS.contains("'/api/work/' + encodeURIComponent(path.slice"));
    }

    #[test]
    fn cockpit_asset_exposes_supported_loop_launch_options() {
        for expected in [
            "loop-prompt-slug",
            "loop-bundle",
            "loop-prompt-role",
            "loop-agent-argv",
            "loop-agent-timeout-seconds",
            "loop-stream-agent-output",
            "loop-max-iterations",
        ] {
            assert!(APP_JS.contains(expected), "{expected}");
        }
    }

    #[test]
    fn cockpit_asset_exposes_conduct_wave_management() {
        for expected in [
            "data-view=\"waves\"",
            "wave-management",
            "/api/conduct/waves",
            "Conduct feed tail",
            "worker DB",
        ] {
            assert!(
                INDEX_HTML.contains(expected) || APP_JS.contains(expected),
                "{expected}"
            );
        }
    }

    #[test]
    fn exposure_options_require_explicit_unsafe_token_for_non_loopback() {
        let options = WebOptions {
            unsafe_expose: false,
            control_token: "secret".to_string(),
        };
        assert!(validate_exposure_options("127.0.0.1", &options).is_ok());
        assert!(validate_exposure_options(
            "0.0.0.0",
            &WebOptions {
                unsafe_expose: false,
                control_token: "secret".to_string(),
            },
        )
        .is_err());
        assert!(validate_exposure_options(
            "0.0.0.0",
            &WebOptions {
                unsafe_expose: true,
                control_token: "secret".to_string(),
            },
        )
        .is_ok());
        assert!(validate_exposure_options(
            "0.0.0.0",
            &WebOptions {
                unsafe_expose: true,
                control_token: String::new(),
            },
        )
        .is_err());
    }

    #[test]
    fn generated_control_tokens_are_header_safe() -> anyhow::Result<()> {
        let token = super::generate_control_token()?;
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        Ok(())
    }

    #[test]
    fn checked_artifact_path_rejects_escape_attempts() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("artifacts");
        fs::create_dir_all(&root)?;
        let inside = root.join("inside.txt");
        fs::write(&inside, "inside")?;
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "outside")?;

        assert!(checked_artifact_path(&root, std::path::Path::new("inside.txt")).is_ok());
        assert!(checked_artifact_path(&root, &outside).is_err());
        Ok(())
    }

    #[test]
    fn checked_artifact_path_accepts_only_current_relative_records() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join(".ldgr/artifacts");
        fs::create_dir_all(&root)?;
        let report = root.join("report.md");
        fs::write(&report, "report")?;
        let resolved = report.canonicalize()?;

        assert_eq!(
            checked_artifact_path(&root, std::path::Path::new("report.md"))?,
            resolved
        );
        assert!(checked_artifact_path(&root, &report).is_err());
        assert!(
            checked_artifact_path(&root, std::path::Path::new(".ldgr/artifacts/report.md"))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn loop_prompt_source_args_accepts_path_prompt_slug_or_bundle() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let prompt = temp.path().join("loop-prompt.md");
        fs::write(&prompt, "{{ldgr_context}}")?;
        let prompt_text = prompt.display().to_string();

        assert_eq!(
            loop_prompt_source_args(&form(&[("prompt", prompt_text.as_str())]))?,
            vec!["--prompt", prompt_text.as_str()]
        );
        assert_eq!(
            loop_prompt_source_args(&form(&[("prompt_slug", "surface")]))?,
            vec!["--prompt-slug", "surface"]
        );
        assert_eq!(
            loop_prompt_source_args(&form(&[
                ("bundle", "cleanroom"),
                ("prompt_role", "surface-loop"),
            ]))?,
            vec!["--bundle", "cleanroom", "--prompt-role", "surface-loop"]
        );
        assert!(loop_prompt_source_args(&form(&[])).is_err());
        assert!(loop_prompt_source_args(&form(&[
            ("prompt", prompt_text.as_str()),
            ("prompt_slug", "surface"),
        ]))
        .is_err());
        Ok(())
    }

    fn form(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }
}
