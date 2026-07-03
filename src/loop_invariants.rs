use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

pub const LOOP_INVARIANTS_PROMPT_FILE: &str = "ldgr-loop-invariants.md";
pub const LOOP_INVARIANTS_PROMPT: &str = include_str!("../prompts/ldgr-loop-invariants.md");

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LoopInvariantsSummary {
    pub status: String,
    pub path: Option<PathBuf>,
    pub body: String,
    pub note: String,
}

pub fn resolve_loop_invariants() -> LoopInvariantsSummary {
    let candidates = loop_invariants_candidates();
    for candidate in candidates {
        if candidate.is_file() {
            match fs::read_to_string(&candidate) {
                Ok(body) => {
                    return LoopInvariantsSummary {
                        status: "active".to_owned(),
                        path: Some(candidate),
                        body,
                        note: "Loaded active user-editable loop invariants prompt.".to_owned(),
                    };
                }
                Err(error) => {
                    return LoopInvariantsSummary {
                        status: "unreadable".to_owned(),
                        path: Some(candidate),
                        body: String::new(),
                        note: format!(
                            "Loop invariants prompt exists but could not be read: {error}"
                        ),
                    };
                }
            }
        }
    }
    LoopInvariantsSummary {
        status: "missing".to_owned(),
        path: None,
        body: fallback_loop_invariants_message(),
        note: format!(
            "No active loop invariants prompt found. Run `ldgr init` or `ldgr install` to seed {LOOP_INVARIANTS_PROMPT_FILE}; LDGR is using this fallback notice only."
        ),
    }
}

pub fn fallback_loop_invariants_message() -> String {
    format!(
        "Loop invariants prompt `{LOOP_INVARIANTS_PROMPT_FILE}` is missing. Continue with bounded work, durable evidence, proportionate validation, safe mutation, and concise reporting; reseed prompts with `ldgr init` or `ldgr install`."
    )
}

pub fn loop_invariants_candidates() -> Vec<PathBuf> {
    let mut candidates = vec![Path::new(".ldgr")
        .join("prompts")
        .join(LOOP_INVARIANTS_PROMPT_FILE)];
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        candidates.push(
            PathBuf::from(home)
                .join(".ldgr")
                .join("prompts")
                .join(LOOP_INVARIANTS_PROMPT_FILE),
        );
    }
    candidates
}
