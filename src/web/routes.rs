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
    let agent = optional_nonempty_form_value(form, "agent").unwrap_or("agentctl");
    if agent != "agentctl" {
        bail!("agent must be agentctl; use agent_argv for custom agents");
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
