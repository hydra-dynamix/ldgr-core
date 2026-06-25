use std::fs;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{bail, Context};
use rusqlite::Connection;

use crate::cli::render::brief_context::{brief_context, BriefContextOptions};
use crate::store::{
    active_prompt, add_artifact, add_observation, apply_loop_intervention, bundled_prompt_version,
    claim_next_pending_run, finish_run, get_run, oldest_running_work_item,
    pending_loop_interventions, read_context, record_run_phase,
    restore_work_item_pending_after_dry_run, sealed_bundle, ArtifactKind, LoopIntervention,
    LoopInterventionAction, RunStatus,
};
use crate::tool_runner::render_command;
use serde::Serialize;

const LOOP_PROCESS_OUTPUT_PREVIEW_BYTES: usize = 64 * 1024;
pub const DEFAULT_LOOP_PROCESS_TIMEOUT: Duration = Duration::from_secs(12 * 60 * 60);
const LOOP_PROCESS_PIPE_DRAIN_TIMEOUT: Duration = Duration::from_secs(30);
const LOOP_PROCESS_TERMINATION_GRACE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopRuntimeOptions {
    pub prompt: LoopPromptSource,
    pub agent: LoopAgent,
    pub audit_argv: Option<Vec<String>>,
    pub project_complete_requested: bool,
    pub dry_run: bool,
    pub stream_agent_output: bool,
    pub agent_timeout: Duration,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopAgent {
    Argv(Vec<String>),
    Codex,
    DryRun,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopPromptSource {
    Path(PathBuf),
    StoredPrompt {
        slug: String,
    },
    Bundle {
        slug: String,
        prompt_role: Option<String>,
    },
}

impl LoopAgent {
    pub fn command_label(&self) -> String {
        match self {
            Self::Argv(argv) => render_command(argv),
            Self::Codex => "codex".to_owned(),
            Self::DryRun => "dry-run".to_owned(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopRuntimeResult {
    pub run_id: i64,
    pub work_slug: String,
    pub prompt_artifact_path: std::path::PathBuf,
    pub audit_artifact_path: Option<std::path::PathBuf>,
    pub agent_exit_code: Option<i32>,
    pub audit_exit_code: Option<i32>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LoopRuntimeOutcome {
    Completed(LoopRuntimeResult),
    BlockedByIntervention,
    BlockedByIncompleteCycle { work_slug: String },
    NoPendingWork,
}

pub fn run_loop_once(
    connection: &Connection,
    artifact_root: &Path,
    options: &LoopRuntimeOptions,
) -> anyhow::Result<LoopRuntimeOutcome> {
    if apply_blocking_intervention_if_present(connection)? {
        return Ok(LoopRuntimeOutcome::BlockedByIntervention);
    }

    if let Some(work_item) = oldest_running_work_item(connection)? {
        return Ok(LoopRuntimeOutcome::BlockedByIncompleteCycle {
            work_slug: work_item.slug,
        });
    }

    let steering_interventions = pending_loop_interventions(connection)?
        .into_iter()
        .filter(|intervention| intervention.action == LoopInterventionAction::Steer)
        .collect::<Vec<_>>();

    let command_text = options.agent.command_label();
    let Some(claimed) = claim_next_pending_run(connection, Some(&command_text))? else {
        return Ok(LoopRuntimeOutcome::NoPendingWork);
    };
    let work_item = claimed.work_item;
    let run = claimed.run;
    record_run_phase(
        connection,
        run.id,
        "started",
        &format!("Started bounded loop session for {}.", work_item.slug),
    )?;

    for intervention in &steering_interventions {
        apply_loop_intervention(connection, intervention.id, Some(run.id))?;
        record_run_phase(
            connection,
            run.id,
            "steered",
            &format!(
                "Applied loop steer intervention {} before agent execution.",
                intervention.id
            ),
        )?;
    }

    match run_loop_after_start(
        connection,
        artifact_root,
        options,
        &work_item,
        run.id,
        &steering_interventions,
    ) {
        Ok(result) => Ok(LoopRuntimeOutcome::Completed(result)),
        Err(error) => {
            let message = format!("Loop runtime failed for {}: {error:#}", work_item.slug);
            let _ = record_run_phase(connection, run.id, "failed", &message);
            let _ = add_observation(connection, run.id, &message);
            let _ = finish_run(connection, run.id, RunStatus::Failed, Some(&message));
            Err(error)
        }
    }
}

fn run_loop_after_start(
    connection: &Connection,
    artifact_root: &Path,
    options: &LoopRuntimeOptions,
    work_item: &crate::store::WorkItem,
    run_id: i64,
    steering_interventions: &[LoopIntervention],
) -> anyhow::Result<LoopRuntimeResult> {
    let work_slug = work_item.slug.as_str();
    record_run_phase(
        connection,
        run_id,
        "rendering_prompt",
        &format!("Rendering loop prompt for {work_slug}."),
    )?;
    let resolved_prompt = resolve_prompt_source(connection, &options.prompt)?;
    record_run_phase(
        connection,
        run_id,
        "prompt_source",
        &resolved_prompt.description,
    )?;
    let template = resolved_prompt.template.clone();
    let context = read_context(connection)?;
    let status = brief_context(
        &context,
        BriefContextOptions {
            recent: 3,
            width: 240,
        },
    );
    let rendered_prompt = append_steering_section(
        prepend_assigned_work_section(
            render_prompt_document(
                &template,
                &serde_json::to_string_pretty(&context)?,
                &serde_json::to_string_pretty(&status)?,
                options.project_complete_requested,
            )?,
            work_item,
            run_id,
        ),
        steering_interventions,
    );

    fs::create_dir_all(artifact_root).with_context(|| {
        format!(
            "failed to create artifact root directory {}",
            artifact_root.display()
        )
    })?;
    let prompt_artifact_path = artifact_root.join(format!("loop-run-{run_id}-prompt.md"));
    fs::write(&prompt_artifact_path, &rendered_prompt).with_context(|| {
        format!(
            "failed to write rendered loop prompt {}",
            prompt_artifact_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &prompt_artifact_path,
        "Rendered autonomous loop prompt with rehydrated LDGR context.",
    )?;
    let provenance_path = artifact_root.join(format!("loop-run-{run_id}-prompt-provenance.json"));
    fs::write(
        &provenance_path,
        serde_json::to_string_pretty(&resolved_prompt.provenance)?,
    )
    .with_context(|| {
        format!(
            "failed to write prompt provenance artifact {}",
            provenance_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Json,
        &provenance_path,
        "Exact prompt source/version/hash provenance for this loop run.",
    )?;

    let mut audit_artifact_path = None;
    let mut audit_exit_code = None;
    if options.project_complete_requested {
        record_run_phase(
            connection,
            run_id,
            "running_completion_audit",
            &format!("Running completion audit before agent execution for {work_slug}."),
        )?;
        let audit_argv = if options.dry_run {
            options.audit_argv.as_deref().unwrap_or(&[])
        } else {
            options
                .audit_argv
                .as_deref()
                .context("--audit-argv is required when --project-complete-requested is supplied")?
        };
        let audit_prompt = render_completion_audit_prompt(&rendered_prompt);
        let audit = if options.dry_run {
            ProcessCapture::from_memory(
                None,
                0,
                "dry-run: completion audit would run here\n".to_owned(),
                String::new(),
            )
        } else {
            run_process_with_stdin(
                audit_argv,
                &audit_prompt,
                false,
                process_output_paths(artifact_root, run_id, "completion-audit")?,
                options.agent_timeout,
            )?
        };
        audit_exit_code = audit.exit_code;
        let audit_path = artifact_root.join(format!("loop-run-{run_id}-completion-audit.md"));
        fs::write(
            &audit_path,
            audit.to_markdown("Completion audit", audit_argv),
        )
        .with_context(|| {
            format!(
                "failed to write completion audit artifact {}",
                audit_path.display()
            )
        })?;
        add_artifact(
            connection,
            artifact_root,
            run_id,
            ArtifactKind::Report,
            &audit_path,
            "Fresh-process completion audit output for project-completion request.",
        )?;
        audit_artifact_path = Some(audit_path);
    }

    record_run_phase(
        connection,
        run_id,
        "running_agent",
        &format!("Running autonomous agent command for {work_slug}."),
    )?;
    let agent = match &options.agent {
        LoopAgent::DryRun => ProcessCapture::from_memory(
            None,
            0,
            "dry-run: autonomous agent would run here\n".to_owned(),
            String::new(),
        ),
        LoopAgent::Argv(argv) => run_process_with_stdin(
            argv,
            &rendered_prompt,
            options.stream_agent_output,
            process_output_paths(artifact_root, run_id, "agent")?,
            options.agent_timeout,
        )?,
        LoopAgent::Codex => {
            let argv = default_codex_argv();
            run_process_with_stdin(
                &argv,
                &rendered_prompt,
                options.stream_agent_output,
                process_output_paths(artifact_root, run_id, "agent")?,
                options.agent_timeout,
            )?
        }
    };

    if get_run(connection, run_id)?.status == RunStatus::Running {
        record_run_phase(
            connection,
            run_id,
            "capturing_agent_output",
            &format!("Capturing autonomous agent output for {work_slug}."),
        )?;
    }
    let output_path = artifact_root.join(format!("loop-run-{run_id}-agent-output.md"));
    let output_argv = agent_output_argv(&options.agent);
    fs::write(
        &output_path,
        agent.to_markdown("Autonomous agent output", &output_argv),
    )
    .with_context(|| {
        format!(
            "failed to write agent output artifact {}",
            output_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &output_path,
        "Autonomous loop agent stdout/stderr capture.",
    )?;

    let final_status = if options.dry_run {
        RunStatus::Partial
    } else if agent.exit_code == Some(0)
        && (!options.project_complete_requested || audit_exit_code == Some(0))
    {
        RunStatus::Success
    } else {
        RunStatus::Failed
    };
    add_observation(
        connection,
        run_id,
        &format!(
            "Autonomous loop runtime rendered prompt and {} agent execution for work item {}.",
            if options.dry_run { "dry-ran" } else { "ran" },
            work_slug
        ),
    )?;
    let current_run = get_run(connection, run_id)?;
    if current_run.status == RunStatus::Running {
        record_run_phase(
            connection,
            run_id,
            "finishing",
            &format!(
                "Finishing loop session for {work_slug} with terminal status {}.",
                final_status.as_str()
            ),
        )?;
        finish_run(
            connection,
            run_id,
            final_status,
            Some("ldgr loop runtime completed one bounded session"),
        )?;
        if options.dry_run {
            restore_work_item_pending_after_dry_run(connection, work_slug, run_id)?;
            record_run_phase(
                connection,
                run_id,
                "dry_run_restored_work",
                &format!(
                    "Dry-run completed without consuming {work_slug}; restored the work item to pending."
                ),
            )?;
        }
    }

    Ok(LoopRuntimeResult {
        run_id,
        work_slug: work_slug.to_owned(),
        prompt_artifact_path,
        audit_artifact_path,
        agent_exit_code: agent.exit_code,
        audit_exit_code,
    })
}

#[derive(Debug, Clone)]
struct ResolvedLoopPrompt {
    template: String,
    description: String,
    provenance: PromptProvenance,
}

#[derive(Debug, Clone, Serialize)]
struct PromptProvenance {
    source_type: String,
    path: Option<String>,
    prompt_slug: Option<String>,
    prompt_role: Option<String>,
    prompt_version: Option<i64>,
    prompt_hash: Option<String>,
    bundle_slug: Option<String>,
    bundle_hash: Option<String>,
}

fn resolve_prompt_source(
    connection: &Connection,
    prompt: &LoopPromptSource,
) -> anyhow::Result<ResolvedLoopPrompt> {
    match prompt {
        LoopPromptSource::Path(path) => Ok(ResolvedLoopPrompt {
            template: fs::read_to_string(path)
                .with_context(|| format!("failed to read prompt document {}", path.display()))?,
            description: format!("Using loop prompt path {}.", path.display()),
            provenance: PromptProvenance {
                source_type: "path".to_owned(),
                path: Some(path.display().to_string()),
                prompt_slug: None,
                prompt_role: None,
                prompt_version: None,
                prompt_hash: None,
                bundle_slug: None,
                bundle_hash: None,
            },
        }),
        LoopPromptSource::StoredPrompt { slug } => {
            let prompt = active_prompt(connection, slug)?;
            Ok(ResolvedLoopPrompt {
                template: prompt.body.clone(),
                description: format!(
                    "Using prompt {} version={} hash={}.",
                    prompt.slug, prompt.current_version, prompt.content_hash
                ),
                provenance: PromptProvenance {
                    source_type: "prompt".to_owned(),
                    path: prompt.source_path.clone(),
                    prompt_slug: Some(prompt.slug),
                    prompt_role: Some(prompt.role),
                    prompt_version: Some(prompt.current_version),
                    prompt_hash: Some(prompt.content_hash),
                    bundle_slug: None,
                    bundle_hash: None,
                },
            })
        }
        LoopPromptSource::Bundle { slug, prompt_role } => {
            let bundle = sealed_bundle(connection, slug)?;
            let (_item, version) =
                bundled_prompt_version(connection, bundle.id, prompt_role.as_deref())?;
            Ok(ResolvedLoopPrompt {
                template: version.body.clone(),
                description: format!(
                    "Using sealed bundle {} hash={} prompt_version={} prompt_hash={}.",
                    bundle.slug, bundle.bundle_hash, version.version, version.content_hash
                ),
                provenance: PromptProvenance {
                    source_type: "bundle".to_owned(),
                    path: version.source_path.clone(),
                    prompt_slug: None,
                    prompt_role: Some(version.role),
                    prompt_version: Some(version.version),
                    prompt_hash: Some(version.content_hash),
                    bundle_slug: Some(bundle.slug),
                    bundle_hash: Some(bundle.bundle_hash),
                },
            })
        }
    }
}

pub fn render_prompt_document(
    template: &str,
    context_json: &str,
    status_json: &str,
    project_complete_requested: bool,
) -> anyhow::Result<String> {
    let job_complete_policy = "Job completion policy: complete exactly one bounded work item, queue concrete follow-up LDGR work when gaps are found, ensure a pending next task exists unless no useful work remains, and never self-certify whole-project completion.";
    let audit_instruction = if project_complete_requested {
        "Project completion was requested. A fresh external Codex audit must inspect mocked/incomplete code, maintainability, quality, tests, edge cases, complexity, smells, and risks. Act on findings and decompose extensive findings into queued LDGR work."
    } else {
        "Project completion was not requested. Do not claim whole-project completion; focus on the next bounded LDGR work item."
    };

    Ok(template
        .replace("{{ldgr_context}}", context_json)
        .replace("{{ldgr_status}}", status_json)
        .replace("{{job_complete_policy}}", job_complete_policy)
        .replace("{{completion_audit_instruction}}", audit_instruction))
}

fn apply_blocking_intervention_if_present(connection: &Connection) -> anyhow::Result<bool> {
    let pending = pending_loop_interventions(connection)?;
    if let Some(intervention) = pending.iter().find(|intervention| {
        matches!(
            intervention.action,
            LoopInterventionAction::Pause | LoopInterventionAction::Stop
        )
    }) {
        apply_loop_intervention(connection, intervention.id, None)?;
        return Ok(true);
    }
    Ok(false)
}

/// Puts the run's assigned work item at the very top of the prompt. The
/// context JSON shows this item as already running (the run starts before
/// rendering), so without this section an agent reading "next pending work
/// item" would land on the following queued item instead.
fn prepend_assigned_work_section(
    rendered_prompt: String,
    work_item: &crate::store::WorkItem,
    run_id: i64,
) -> String {
    format!(
        "# Assigned work (run {run_id})\n\n\
         You are run {run_id}. Complete exactly this work item and no other:\n\n\
         - slug: {slug}\n\
         - title: {title}\n\
         - description: {description}\n\n\
         In the context below this item appears with status `running` because this run was \
         started for it. Other pending items in the context are NOT yours this cycle.\n\n\
         {rendered_prompt}",
        slug = work_item.slug,
        title = work_item.title,
        description = work_item.description,
    )
}

fn append_steering_section(
    mut rendered_prompt: String,
    steering_interventions: &[LoopIntervention],
) -> String {
    if steering_interventions.is_empty() {
        return rendered_prompt;
    }
    rendered_prompt.push_str("\n\n# Explicit LDGR loop steering\n\n");
    for intervention in steering_interventions {
        rendered_prompt.push_str(&format!(
            "- intervention={}: reason={}\n  instruction={}\n",
            intervention.id,
            intervention.reason,
            intervention.instruction.as_deref().unwrap_or("")
        ));
    }
    rendered_prompt
}

fn render_completion_audit_prompt(rendered_prompt: &str) -> String {
    format!(
        "You are a fresh Codex audit process for LDGR project completion. Inspect for mocked or incomplete code, maintainability, code quality, test coverage, edge cases, complexity, code smells, and other risks. Produce concrete findings and recommended queued work; do not certify completion unless no material risks remain.\n\nOriginal loop prompt:\n\n{rendered_prompt}"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessCapture {
    pub exit_code: Option<i32>,
    pub duration_ms: u128,
    pub stdout: String,
    pub stderr: String,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_bytes: u64,
    pub stderr_bytes: u64,
    pub stdout_artifact_path: Option<PathBuf>,
    pub stderr_artifact_path: Option<PathBuf>,
}

impl ProcessCapture {
    pub fn from_memory(
        exit_code: Option<i32>,
        duration_ms: u128,
        stdout: String,
        stderr: String,
    ) -> Self {
        Self {
            stdout_bytes: stdout.len().try_into().unwrap_or(u64::MAX),
            stderr_bytes: stderr.len().try_into().unwrap_or(u64::MAX),
            exit_code,
            duration_ms,
            stdout,
            stderr,
            stdout_truncated: false,
            stderr_truncated: false,
            stdout_artifact_path: None,
            stderr_artifact_path: None,
        }
    }

    fn to_markdown(&self, title: &str, argv: &[String]) -> String {
        format!(
            "# {title}\n\ncommand: `{}`\nexit_code: {}\nduration_ms: {}\n\n## stdout\n\nbytes: {}\npreview_truncated: {}\n{}\n\n```text\n{}\n```\n\n## stderr\n\nbytes: {}\npreview_truncated: {}\n{}\n\n```text\n{}\n```\n",
            if argv.is_empty() { "dry-run".to_owned() } else { render_command(argv) },
            self.exit_code
                .map(|value| value.to_string())
                .unwrap_or_else(|| "unknown".to_owned()),
            self.duration_ms,
            self.stdout_bytes,
            self.stdout_truncated,
            output_artifact_line(self.stdout_artifact_path.as_deref()),
            self.stdout,
            self.stderr_bytes,
            self.stderr_truncated,
            output_artifact_line(self.stderr_artifact_path.as_deref()),
            self.stderr
        )
    }
}

fn default_codex_argv() -> Vec<String> {
    vec![
        "codex".to_owned(),
        "exec".to_owned(),
        "--sandbox".to_owned(),
        "workspace-write".to_owned(),
    ]
}

fn agent_output_argv(agent: &LoopAgent) -> Vec<String> {
    match agent {
        LoopAgent::Argv(argv) => argv.clone(),
        LoopAgent::Codex => default_codex_argv(),
        LoopAgent::DryRun => Vec::new(),
    }
}

#[derive(Clone, Copy)]
enum StreamTarget {
    Stdout,
    Stderr,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ProcessOutputPaths {
    stdin: PathBuf,
    stdout: PathBuf,
    stderr: PathBuf,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct CapturedProcessStream {
    preview: Vec<u8>,
    total_bytes: u64,
    artifact_path: PathBuf,
}

trait ReadSend: Read + Send {}
impl<T: Read + Send> ReadSend for T {}

fn output_artifact_line(path: Option<&Path>) -> String {
    match path {
        Some(path) => format!("full_output: `{}`", path.display()),
        None => "full_output: inline".to_owned(),
    }
}

fn process_output_paths(
    artifact_root: &Path,
    run_id: i64,
    label: &str,
) -> anyhow::Result<ProcessOutputPaths> {
    fs::create_dir_all(artifact_root).with_context(|| {
        format!(
            "failed to create artifact root directory {}",
            artifact_root.display()
        )
    })?;
    let label = sanitize_output_label(label);
    Ok(ProcessOutputPaths {
        stdin: artifact_root.join(format!("loop-run-{run_id}-{label}-stdin.txt")),
        stdout: artifact_root.join(format!("loop-run-{run_id}-{label}-stdout.txt")),
        stderr: artifact_root.join(format!("loop-run-{run_id}-{label}-stderr.txt")),
    })
}

fn sanitize_output_label(value: &str) -> String {
    let label = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    if label.is_empty() {
        "process".to_owned()
    } else {
        label
    }
}

struct ProcessStreamReader {
    stream_name: &'static str,
    artifact_path: PathBuf,
    receiver: mpsc::Receiver<anyhow::Result<CapturedProcessStream>>,
}

fn read_process_stream(
    stream: Box<dyn ReadSend>,
    stream_target: Option<StreamTarget>,
    stream_name: &'static str,
    artifact_path: PathBuf,
) -> ProcessStreamReader {
    let (sender, receiver) = mpsc::sync_channel(1);
    let reader_artifact_path = artifact_path.clone();
    thread::spawn(move || {
        let result = capture_process_stream(stream, stream_target, reader_artifact_path);
        let _ = sender.send(result);
    });
    ProcessStreamReader {
        stream_name,
        artifact_path,
        receiver,
    }
}

fn capture_process_stream(
    mut stream: Box<dyn ReadSend>,
    stream_target: Option<StreamTarget>,
    artifact_path: PathBuf,
) -> anyhow::Result<CapturedProcessStream> {
    let mut artifact = fs::File::create(&artifact_path)
        .with_context(|| format!("failed to create {}", artifact_path.display()))?;
    let mut preview = Vec::new();
    let mut total_bytes: u64 = 0;
    let mut buffer = [0_u8; 8192];
    loop {
        let bytes_read = stream.read(&mut buffer).with_context(|| {
            format!(
                "failed to read process output for {}",
                artifact_path.display()
            )
        })?;
        if bytes_read == 0 {
            break;
        }
        let chunk = &buffer[..bytes_read];
        artifact
            .write_all(chunk)
            .with_context(|| format!("failed to write {}", artifact_path.display()))?;
        if preview.len() < LOOP_PROCESS_OUTPUT_PREVIEW_BYTES {
            let remaining = LOOP_PROCESS_OUTPUT_PREVIEW_BYTES - preview.len();
            preview.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        total_bytes = total_bytes.saturating_add(bytes_read.try_into().unwrap_or(u64::MAX));
        match stream_target {
            Some(StreamTarget::Stdout) => {
                let mut stdout = std::io::stdout().lock();
                stdout.write_all(chunk)?;
                stdout.flush()?;
            }
            Some(StreamTarget::Stderr) => {
                let mut stderr = std::io::stderr().lock();
                stderr.write_all(chunk)?;
                stderr.flush()?;
            }
            None => {}
        }
    }
    artifact
        .flush()
        .with_context(|| format!("failed to flush {}", artifact_path.display()))?;
    Ok(CapturedProcessStream {
        preview,
        total_bytes,
        artifact_path,
    })
}

fn receive_process_stream_with_timeout(
    reader: &ProcessStreamReader,
    command: &str,
    timeout: Duration,
) -> anyhow::Result<Option<CapturedProcessStream>> {
    match reader.receiver.recv_timeout(timeout) {
        Ok(result) => result
            .with_context(|| format!("failed to read {} for `{command}`", reader.stream_name))
            .map(Some),
        Err(mpsc::RecvTimeoutError::Timeout) => Ok(None),
        Err(mpsc::RecvTimeoutError::Disconnected) => {
            bail!(
                "{} reader stopped before returning output for `{command}`",
                reader.stream_name
            )
        }
    }
}

fn run_process_with_stdin(
    argv: &[String],
    stdin_text: &str,
    stream_output: bool,
    output_paths: ProcessOutputPaths,
    timeout: Duration,
) -> anyhow::Result<ProcessCapture> {
    run_process_with_stdin_timeout(argv, stdin_text, stream_output, output_paths, timeout)
}

fn run_process_with_stdin_timeout(
    argv: &[String],
    stdin_text: &str,
    stream_output: bool,
    output_paths: ProcessOutputPaths,
    timeout: Duration,
) -> anyhow::Result<ProcessCapture> {
    run_process_with_stdin_timeouts(
        argv,
        stdin_text,
        stream_output,
        output_paths,
        timeout,
        LOOP_PROCESS_PIPE_DRAIN_TIMEOUT,
        LOOP_PROCESS_PIPE_DRAIN_TIMEOUT,
    )
}

fn run_process_with_stdin_timeouts(
    argv: &[String],
    stdin_text: &str,
    stream_output: bool,
    output_paths: ProcessOutputPaths,
    process_timeout: Duration,
    pipe_drain_timeout: Duration,
    kill_drain_timeout: Duration,
) -> anyhow::Result<ProcessCapture> {
    if argv.is_empty() {
        bail!("process argv must not be empty");
    }
    let started = Instant::now();
    fs::write(&output_paths.stdin, stdin_text).with_context(|| {
        format!(
            "failed to write child stdin file {}",
            output_paths.stdin.display()
        )
    })?;
    let stdin_file = fs::File::open(&output_paths.stdin).with_context(|| {
        format!(
            "failed to open child stdin file {}",
            output_paths.stdin.display()
        )
    })?;

    let command_text = render_command(argv);
    let prepared_process_tree = PreparedProcessTree::new()?;
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .stdin(Stdio::from(stdin_file))
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure_process_group(&mut command);
    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn `{command_text}`"))?;
    let process_tree = match prepared_process_tree.attach(&child) {
        Ok(process_tree) => process_tree,
        Err(error) => {
            let _ = child.kill();
            let _ = child.wait();
            return Err(error);
        }
    };
    let stdout = child.stdout.take().context("failed to open child stdout")?;
    let stderr = child.stderr.take().context("failed to open child stderr")?;
    let stdout_reader = read_process_stream(
        Box::new(stdout),
        stream_output.then_some(StreamTarget::Stdout),
        "stdout",
        output_paths.stdout.clone(),
    );
    let stderr_reader = read_process_stream(
        Box::new(stderr),
        stream_output.then_some(StreamTarget::Stderr),
        "stderr",
        output_paths.stderr.clone(),
    );
    let status_result =
        wait_child_with_timeout(&mut child, &process_tree, process_timeout, &command_text);
    let (stdout, stderr) = collect_process_streams(
        &stdout_reader,
        &stderr_reader,
        &process_tree,
        &command_text,
        pipe_drain_timeout,
        kill_drain_timeout,
    )?;
    let status = match status_result {
        Ok(status) => status,
        Err(error) => {
            bail!(
                "{error}; stdout captured at {}; stderr captured at {}",
                stdout.artifact_path.display(),
                stderr.artifact_path.display()
            );
        }
    };
    Ok(ProcessCapture {
        exit_code: status.code(),
        duration_ms: started.elapsed().as_millis(),
        stdout: String::from_utf8_lossy(&stdout.preview).into_owned(),
        stderr: String::from_utf8_lossy(&stderr.preview).into_owned(),
        stdout_truncated: stdout.total_bytes > stdout.preview.len().try_into().unwrap_or(u64::MAX),
        stderr_truncated: stderr.total_bytes > stderr.preview.len().try_into().unwrap_or(u64::MAX),
        stdout_bytes: stdout.total_bytes,
        stderr_bytes: stderr.total_bytes,
        stdout_artifact_path: Some(stdout.artifact_path),
        stderr_artifact_path: Some(stderr.artifact_path),
    })
}

fn collect_process_streams(
    stdout_reader: &ProcessStreamReader,
    stderr_reader: &ProcessStreamReader,
    process_tree: &ProcessTree,
    command: &str,
    pipe_drain_timeout: Duration,
    kill_drain_timeout: Duration,
) -> anyhow::Result<(CapturedProcessStream, CapturedProcessStream)> {
    let mut stdout =
        receive_process_stream_with_timeout(stdout_reader, command, pipe_drain_timeout)?;
    let mut stderr =
        receive_process_stream_with_timeout(stderr_reader, command, pipe_drain_timeout)?;
    if stdout.is_none() || stderr.is_none() {
        process_tree.terminate();
    }
    if stdout.is_none() {
        stdout = receive_process_stream_with_timeout(stdout_reader, command, kill_drain_timeout)?;
    }
    if stderr.is_none() {
        stderr = receive_process_stream_with_timeout(stderr_reader, command, kill_drain_timeout)?;
    }
    let stdout = stdout.with_context(|| {
        format!(
            "stdout reader did not finish for `{command}` after process group termination; stdout captured at {}",
            stdout_reader.artifact_path.display()
        )
    })?;
    let stderr = stderr.with_context(|| {
        format!(
            "stderr reader did not finish for `{command}` after process group termination; stderr captured at {}",
            stderr_reader.artifact_path.display()
        )
    })?;
    Ok((stdout, stderr))
}

fn wait_child_with_timeout(
    child: &mut Child,
    process_tree: &ProcessTree,
    timeout: Duration,
    command: &str,
) -> anyhow::Result<std::process::ExitStatus> {
    let started = Instant::now();
    loop {
        if let Some(status) = child
            .try_wait()
            .with_context(|| format!("failed to wait for `{command}`"))?
        {
            return Ok(status);
        }
        if started.elapsed() >= timeout {
            terminate_child_process_tree(child, process_tree);
            bail!(
                "process `{command}` timed out after {} seconds",
                timeout.as_secs()
            );
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn terminate_child_process_tree(child: &mut Child, process_tree: &ProcessTree) {
    process_tree.terminate();
    let started = Instant::now();
    while started.elapsed() < LOOP_PROCESS_TERMINATION_GRACE {
        match child.try_wait() {
            Ok(Some(_)) => return,
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(_) => return,
        }
    }
    let _ = child.kill();
    let _ = child.wait();
}

struct PreparedProcessTree {
    #[cfg(windows)]
    job: WindowsJob,
}

impl PreparedProcessTree {
    fn new() -> anyhow::Result<Self> {
        #[cfg(windows)]
        {
            return Ok(Self {
                job: WindowsJob::new()?,
            });
        }
        #[cfg(not(windows))]
        {
            Ok(Self {})
        }
    }

    fn attach(self, child: &Child) -> anyhow::Result<ProcessTree> {
        #[cfg(windows)]
        {
            self.job.assign(child)?;
            return Ok(ProcessTree { job: self.job });
        }
        #[cfg(not(windows))]
        {
            Ok(ProcessTree {
                child_id: child.id(),
            })
        }
    }
}

struct ProcessTree {
    #[cfg(not(windows))]
    child_id: u32,
    #[cfg(windows)]
    job: WindowsJob,
}

impl ProcessTree {
    fn terminate(&self) {
        #[cfg(windows)]
        {
            self.job.terminate();
        }
        #[cfg(not(windows))]
        {
            signal_process_group(self.child_id, TerminationSignal::Terminate);
            thread::sleep(LOOP_PROCESS_TERMINATION_GRACE);
            signal_process_group(self.child_id, TerminationSignal::Kill);
        }
    }
}

#[cfg(not(windows))]
#[derive(Debug, Clone, Copy)]
enum TerminationSignal {
    Terminate,
    Kill,
}

#[cfg(unix)]
fn configure_process_group(command: &mut Command) {
    use std::os::unix::process::CommandExt;
    command.process_group(0);
}

#[cfg(windows)]
fn configure_process_group(command: &mut Command) {
    use std::os::windows::process::CommandExt;
    const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
    command.creation_flags(CREATE_NEW_PROCESS_GROUP);
}

#[cfg(all(not(unix), not(windows)))]
fn configure_process_group(_command: &mut Command) {}

#[cfg(windows)]
struct WindowsJob {
    handle: windows_sys::Win32::Foundation::HANDLE,
}

#[cfg(windows)]
impl WindowsJob {
    fn new() -> anyhow::Result<Self> {
        use windows_sys::Win32::System::JobObjects::{
            CreateJobObjectW, JobObjectExtendedLimitInformation, SetInformationJobObject,
            JOBOBJECT_EXTENDED_LIMIT_INFORMATION, JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE,
        };

        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        if handle.is_null() {
            bail!(
                "failed to create Windows job object for loop subprocess tree: {}",
                std::io::Error::last_os_error()
            );
        }
        let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = unsafe { std::mem::zeroed() };
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        let ok = unsafe {
            SetInformationJobObject(
                handle,
                JobObjectExtendedLimitInformation,
                &limits as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            )
        };
        if ok == 0 {
            let error = std::io::Error::last_os_error();
            unsafe {
                windows_sys::Win32::Foundation::CloseHandle(handle);
            }
            bail!("failed to configure Windows job object: {error}");
        }
        Ok(Self { handle })
    }

    fn assign(&self, child: &Child) -> anyhow::Result<()> {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::System::JobObjects::AssignProcessToJobObject;

        let process = child.as_raw_handle() as windows_sys::Win32::Foundation::HANDLE;
        let ok = unsafe { AssignProcessToJobObject(self.handle, process) };
        if ok == 0 {
            bail!(
                "failed to assign loop subprocess to Windows job object: {}",
                std::io::Error::last_os_error()
            );
        }
        Ok(())
    }

    fn terminate(&self) {
        unsafe {
            windows_sys::Win32::System::JobObjects::TerminateJobObject(self.handle, 1);
        }
    }
}

#[cfg(windows)]
impl Drop for WindowsJob {
    fn drop(&mut self) {
        unsafe {
            windows_sys::Win32::Foundation::CloseHandle(self.handle);
        }
    }
}

#[cfg(unix)]
fn signal_process_group(child_id: u32, signal: TerminationSignal) {
    let signal = match signal {
        TerminationSignal::Terminate => libc::SIGTERM,
        TerminationSignal::Kill => libc::SIGKILL,
    };
    let process_group = -(child_id as libc::pid_t);
    // SAFETY: libc::kill does not retain pointers. The negative PID targets the
    // process group created for this child; errors are intentionally ignored
    // because the process group may already have exited.
    unsafe {
        libc::kill(process_group, signal);
    }
}

#[cfg(all(not(unix), not(windows)))]
fn signal_process_group(_child_id: u32, _signal: TerminationSignal) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loop_process_timeout_kills_child_and_preserves_output_artifacts() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let output_paths = ProcessOutputPaths {
            stdin: temp.path().join("stdin.txt"),
            stdout: temp.path().join("stdout.txt"),
            stderr: temp.path().join("stderr.txt"),
        };
        let argv = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "printf started; sleep 5".to_owned(),
        ];

        let error = run_process_with_stdin_timeout(
            &argv,
            "prompt",
            false,
            output_paths.clone(),
            Duration::from_millis(100),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("timed out after 0 seconds"), "{message}");
        assert!(
            message.contains(output_paths.stdout.to_str().unwrap()),
            "{message}"
        );
        assert_eq!(fs::read_to_string(output_paths.stdout)?, "started");
        Ok(())
    }

    #[test]
    fn loop_process_timeout_is_not_blocked_by_child_that_does_not_read_stdin() -> anyhow::Result<()>
    {
        let temp = tempfile::tempdir()?;
        let output_paths = ProcessOutputPaths {
            stdin: temp.path().join("stdin.txt"),
            stdout: temp.path().join("stdout.txt"),
            stderr: temp.path().join("stderr.txt"),
        };
        let argv = vec!["sh".to_owned(), "-c".to_owned(), "sleep 5".to_owned()];
        let prompt = "x".repeat(2 * 1024 * 1024);

        let error = run_process_with_stdin_timeout(
            &argv,
            &prompt,
            false,
            output_paths.clone(),
            Duration::from_millis(100),
        )
        .unwrap_err();

        let message = format!("{error:#}");
        assert!(message.contains("timed out"), "{message}");
        assert_eq!(fs::read_to_string(output_paths.stdin)?.len(), prompt.len());
        Ok(())
    }

    #[test]
    fn loop_process_pipe_collection_kills_grandchild_holding_stdout_open() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let output_paths = ProcessOutputPaths {
            stdin: temp.path().join("stdin.txt"),
            stdout: temp.path().join("stdout.txt"),
            stderr: temp.path().join("stderr.txt"),
        };
        let argv = vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "(sleep 5) & printf parent-done".to_owned(),
        ];

        let capture = run_process_with_stdin_timeouts(
            &argv,
            "prompt",
            false,
            output_paths,
            Duration::from_secs(5),
            Duration::from_millis(100),
            Duration::from_secs(1),
        )?;

        assert_eq!(capture.exit_code, Some(0));
        assert_eq!(capture.stdout, "parent-done");
        Ok(())
    }
}
