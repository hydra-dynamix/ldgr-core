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
        "Project completion was requested. A fresh external audit must inspect mocked/incomplete code, maintainability, quality, tests, edge cases, complexity, smells, and risks. Act on findings and decompose extensive findings into queued LDGR work."
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
        "You are a fresh external audit process for LDGR project completion. Inspect for mocked or incomplete code, maintainability, code quality, test coverage, edge cases, complexity, code smells, and other risks. Produce concrete findings and recommended queued work; do not certify completion unless no material risks remain.\n\nOriginal loop prompt:\n\n{rendered_prompt}"
    )
}

