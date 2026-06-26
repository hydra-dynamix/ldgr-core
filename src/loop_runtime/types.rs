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
    Agentctl,
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
            Self::Agentctl => "agentctl".to_owned(),
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

