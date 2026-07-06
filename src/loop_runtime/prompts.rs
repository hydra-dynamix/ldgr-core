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
    thinking_level_intent: Option<String>,
    advisory_intent: Option<String>,
    components: Option<Vec<PromptProvenance>>,
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
                thinking_level_intent: None,
                advisory_intent: None,
                components: None,
            },
        }),
        LoopPromptSource::StoredPrompt { slug } => {
            let path = global_prompt_path(slug)?;
            let body = fs::read_to_string(&path).with_context(|| {
                format!("failed to read global prompt {} at {}", slug, path.display())
            })?;
            let hash = stable_content_hash(&body);
            Ok(ResolvedLoopPrompt {
                template: body,
                description: format!(
                    "Using global prompt {} path={} hash={}.",
                    slug,
                    path.display(),
                    hash
                ),
                provenance: PromptProvenance {
                    source_type: "global_prompt".to_owned(),
                    path: Some(path.display().to_string()),
                    prompt_slug: Some(slug.clone()),
                    prompt_role: None,
                    prompt_version: None,
                    prompt_hash: Some(hash),
                    thinking_level_intent: None,
                    advisory_intent: None,
                    components: None,
                },
            })
        }
        LoopPromptSource::Composite { sources } => {
            if sources.is_empty() {
                bail!("composite loop prompt requires at least one prompt source");
            }
            let mut resolved_parts = Vec::new();
            for source in sources {
                resolved_parts.push(resolve_prompt_source(connection, source)?);
            }
            let template = resolved_parts
                .iter()
                .enumerate()
                .map(|(index, part)| {
                    format!(
                        "<!-- ldgr-prompt-fragment {}: {} -->\n{}\n",
                        index + 1,
                        part.description,
                        part.template.trim_end()
                    )
                })
                .collect::<Vec<_>>()
                .join("\n");
            let prompt_hash = stable_content_hash(&template);
            let description = format!(
                "Using composite loop prompt with {} fragments hash={prompt_hash}.",
                resolved_parts.len()
            );
            Ok(ResolvedLoopPrompt {
                template,
                description,
                provenance: PromptProvenance {
                    source_type: "composite".to_owned(),
                    path: None,
                    prompt_slug: None,
                    prompt_role: None,
                    prompt_version: None,
                    prompt_hash: Some(prompt_hash),
                    thinking_level_intent: None,
                    advisory_intent: None,
                    components: Some(
                        resolved_parts
                            .into_iter()
                            .map(|part| part.provenance)
                            .collect(),
                    ),
                },
            })
        }
    }
}

fn global_prompt_path(slug: &str) -> anyhow::Result<std::path::PathBuf> {
    let root = std::env::var_os("LDGR_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|home| std::path::PathBuf::from(home).join(".ldgr")))
        .context("cannot resolve LDGR_HOME or HOME for global prompt lookup")?
        .join("prompts");
    let direct = root.join(slug);
    if direct.is_file() {
        return Ok(direct);
    }
    let markdown = root.join(format!("{slug}.md"));
    if markdown.is_file() {
        return Ok(markdown);
    }
    anyhow::bail!(
        "unknown global prompt {slug}; expected {} or {}",
        direct.display(),
        markdown.display()
    )
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

