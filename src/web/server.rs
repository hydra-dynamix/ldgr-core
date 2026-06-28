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
                if !is_broken_pipe_error(&error) {
                    eprintln!("web cockpit request failed: {error:#}");
                }
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

fn is_broken_pipe_error(error: &anyhow::Error) -> bool {
    error.chain().any(|cause| {
        cause
            .downcast_ref::<std::io::Error>()
            .is_some_and(|io_error| io_error.kind() == std::io::ErrorKind::BrokenPipe)
    })
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

