fn live_progress(options: &LoopRuntimeOptions, message: impl AsRef<str>) {
    if options.live_progress {
        eprintln!("[ldgr loop] {}", message.as_ref());
    }
}

fn live_progress_phase(options: &LoopRuntimeOptions, run_id: i64, phase: &str, report: &str) {
    live_progress(options, format!("run={run_id} phase={phase}: {report}"));
}

fn live_progress_artifact(options: &LoopRuntimeOptions, run_id: i64, path: &Path, description: &str) {
    live_progress(
        options,
        format!(
            "run={run_id} artifact={}: {description}",
            path.display()
        ),
    );
}

fn live_progress_latest_context(
    connection: &Connection,
    options: &LoopRuntimeOptions,
    run_id: i64,
) -> anyhow::Result<()> {
    if !options.live_progress {
        return Ok(());
    }
    let context = read_context(connection)?;
    if let Some(observation) = context.latest_observations.first() {
        live_progress(
            options,
            format!(
                "run={run_id} latest_observation={} work={}: {}",
                observation.observation_id, observation.work_slug, observation.body
            ),
        );
    }
    if let Some(decision) = context.latest_decision {
        live_progress(
            options,
            format!(
                "run={run_id} latest_decision={} work={} outcome={}: {}",
                decision.decision_id,
                decision.work_slug,
                decision.outcome.as_str(),
                decision.rationale
            ),
        );
    }
    Ok(())
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
    live_progress(
        options,
        format!(
            "start run={} work={} title={} command={}",
            run.id, work_item.slug, work_item.title, command_text
        ),
    );
    let started_report = format!("Started bounded loop session for {}.", work_item.slug);
    record_run_phase(connection, run.id, "started", &started_report)?;
    live_progress_phase(options, run.id, "started", &started_report);

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
    let rendering_report = format!("Rendering loop prompt for {work_slug}.");
    record_run_phase(connection, run_id, "rendering_prompt", &rendering_report)?;
    live_progress_phase(options, run_id, "rendering_prompt", &rendering_report);
    let resolved_prompt = resolve_prompt_source(connection, &options.prompt)?;
    record_run_phase(connection, run_id, "prompt_source", &resolved_prompt.description)?;
    live_progress_phase(options, run_id, "prompt_source", &resolved_prompt.description);
    let template = resolved_prompt.template.clone();
    let context = read_context(connection)?;
    let status = brief_context(
        &context,
        BriefContextOptions {
            recent: 3,
            width: 240,
        },
    );
    let context_json = serde_json::to_string_pretty(&context)?;
    let status_json = serde_json::to_string_pretty(&status)?;
    let rendered_prompt = append_steering_section(
        prepend_assigned_work_section(
            render_prompt_document(
                &template,
                &context_json,
                &status_json,
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
    live_progress_artifact(
        options,
        run_id,
        &prompt_artifact_path,
        "Rendered autonomous loop prompt with rehydrated LDGR context.",
    );
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
    live_progress_artifact(
        options,
        run_id,
        &provenance_path,
        "Exact prompt source/version/hash provenance for this loop run.",
    );

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

    let role_results = run_role_sequence(
        connection,
        artifact_root,
        options,
        work_item,
        run_id,
        steering_interventions,
        &context_json,
        &status_json,
        &resolved_prompt,
    )?;
    if get_run(connection, run_id)?.status == RunStatus::Running {
        record_run_phase(
            connection,
            run_id,
            "running_agent",
            &format!("Completed generic role agent sequence for {work_slug}."),
        )?;
    }
    let agent_exit_code = role_results
        .iter()
        .find(|result| result.agent_exit_code != Some(0))
        .or_else(|| role_results.last())
        .and_then(|result| result.agent_exit_code);
    let summary_input_path = role_results
        .last()
        .map(|result| result.output_artifact_path.as_path())
        .unwrap_or(prompt_artifact_path.as_path());
    let (summary_artifact_path, summary_exit_code) = run_post_cycle_summary(
        connection,
        artifact_root,
        options,
        work_slug,
        run_id,
        summary_input_path,
        agent_exit_code,
    )?;

    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &prompt_artifact_path,
        "Rendered autonomous loop prompt with rehydrated LDGR context.",
    )?;

    let final_status = if options.dry_run {
        RunStatus::Partial
    } else if role_results.len() == LOOP_ROLES.len()
        && role_results
            .iter()
            .all(|result| result.agent_exit_code == Some(0))
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
        summary_artifact_path,
        summary_exit_code,
        agent_exit_code,
        audit_exit_code,
        role_results,
    })
}

const LOOP_ROLES: [&str; 4] = ["planner", "worker", "scryb", "validator"];

fn run_role_sequence(
    connection: &Connection,
    artifact_root: &Path,
    options: &LoopRuntimeOptions,
    work_item: &crate::store::WorkItem,
    run_id: i64,
    steering_interventions: &[LoopIntervention],
    _context_json: &str,
    _status_json: &str,
    base_prompt: &ResolvedLoopPrompt,
) -> anyhow::Result<Vec<LoopRoleResult>> {
    let mut results = Vec::new();
    for role in LOOP_ROLES {
        let resolved_prompt = resolve_role_prompt_source(connection, &options.prompt, base_prompt, role)?;
        let render_role_report = format!("Rendering {role} role prompt for {}.", work_item.slug);
        let render_role_phase = format!("rendering_{role}_prompt");
        record_run_phase(connection, run_id, &render_role_phase, &render_role_report)?;
        live_progress_phase(options, run_id, &render_role_phase, &render_role_report);
        let role_context = read_context(connection)?;
        let role_status = brief_context(
            &role_context,
            BriefContextOptions {
                recent: 3,
                width: 240,
            },
        );
        let role_context_json = serde_json::to_string_pretty(&role_context)?;
        let role_status_json = serde_json::to_string_pretty(&role_status)?;
        let rendered_prompt = append_steering_section(
            prepend_assigned_work_section(
                render_prompt_document(
                    &resolved_prompt.template,
                    &role_context_json,
                    &role_status_json,
                    options.project_complete_requested,
                )?,
                work_item,
                run_id,
            ),
            steering_interventions,
        );
        let prompt_artifact_path = artifact_root.join(format!("loop-run-{run_id}-{role}-prompt.md"));
        fs::write(&prompt_artifact_path, &rendered_prompt).with_context(|| {
            format!(
                "failed to write {role} prompt artifact {}",
                prompt_artifact_path.display()
            )
        })?;
        add_artifact(
            connection,
            artifact_root,
            run_id,
            ArtifactKind::Report,
            &prompt_artifact_path,
            &format!("Rendered {role} role prompt with rehydrated LDGR context."),
        )?;
        let provenance_path = artifact_root.join(format!("loop-run-{run_id}-{role}-prompt-provenance.json"));
        fs::write(
            &provenance_path,
            serde_json::to_string_pretty(&resolved_prompt.provenance)?,
        )
        .with_context(|| {
            format!(
                "failed to write {role} prompt provenance artifact {}",
                provenance_path.display()
            )
        })?;
        add_artifact(
            connection,
            artifact_root,
            run_id,
            ArtifactKind::Json,
            &provenance_path,
            &format!("Exact {role} role prompt source/version/hash provenance for this loop run."),
        )?;

        let running_role_report = format!("Running fresh {role} agent command for {}.", work_item.slug);
        let running_role_phase = format!("running_{role}_agent");
        record_run_phase(connection, run_id, &running_role_phase, &running_role_report)?;
        live_progress_phase(options, run_id, &running_role_phase, &running_role_report);
        let mut agent = run_role_agent(
            artifact_root,
            options,
            run_id,
            role,
            &work_item.slug,
            &rendered_prompt,
        )?;
        if matches!(options.agent, LoopAgent::Agentctl) {
            agent = enrich_agentctl_failure_output(agent);
        }
        if get_run(connection, run_id)?.status == RunStatus::Running {
            record_run_phase(
                connection,
                run_id,
                &format!("capturing_{role}_agent_output"),
                &format!("Capturing {role} agent output for {}.", work_item.slug),
            )?;
        }
        let output_path = artifact_root.join(format!("loop-run-{run_id}-{role}-agent-output.md"));
        let output_argv = agent_output_argv(&options.agent);
        fs::write(
            &output_path,
            agent.to_markdown(&format!("{role} agent output"), &output_argv),
        )
        .with_context(|| {
            format!(
                "failed to write {role} agent output artifact {}",
                output_path.display()
            )
        })?;
        let output_description = format!("Fresh {role} agent stdout/stderr capture.");
        add_artifact(
            connection,
            artifact_root,
            run_id,
            ArtifactKind::Report,
            &output_path,
            &output_description,
        )?;
        live_progress_artifact(options, run_id, &output_path, &output_description);
        let exit_code = agent.exit_code;
        let observation = format!(
            "Generic loop {role} role completed for assigned work item {} with exit_code {:?}; output artifact: {}.",
            work_item.slug,
            exit_code,
            output_path.display()
        );
        add_observation(connection, run_id, &observation)?;
        live_progress(options, format!("run={run_id} observation: {observation}"));
        write_compat_agent_output_artifact(
            connection,
            artifact_root,
            run_id,
            role,
            &agent,
            &output_path,
        )?;
        results.push(LoopRoleResult {
            role: role.to_owned(),
            prompt_artifact_path,
            output_artifact_path: output_path,
            agent_exit_code: exit_code,
        });
        if role == "scryb" {
            write_scryb_reports(connection, artifact_root, work_item, run_id, &results)?;
        }
        if role == "validator" {
            execute_validator_ops_actions(connection, artifact_root, run_id, &agent)?;
            write_validator_advisory(connection, artifact_root, work_item, run_id, results.last().unwrap())?;
            execute_validator_revision_gate(connection, artifact_root, work_item, run_id, &agent)?;
        }
        live_progress_latest_context(connection, options, run_id)?;
        if get_run(connection, run_id)?.status != RunStatus::Running {
            break;
        }
        if !options.dry_run && exit_code != Some(0) {
            record_run_phase(
                connection,
                run_id,
                &format!("{role}_agent_failed"),
                &format!("Stopping role sequence after {role} agent exit code {exit_code:?}."),
            )?;
            break;
        }
    }
    Ok(results)
}

#[derive(Debug, Deserialize)]
struct ValidatorOpsEnvelope {
    actions: Vec<ValidatorOpsAction>,
}

#[derive(Debug, Deserialize)]
struct ValidatorRevisionEnvelope {
    rationale: String,
    required_corrections: Vec<String>,
    #[serde(default)]
    affected_artifacts: Vec<String>,
    #[serde(default)]
    affected_work_items: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "snake_case")]
enum ValidatorOpsAction {
    ClearBlock {
        intervention_id: i64,
        rationale: String,
        evidence: Vec<String>,
    },
    MergeWorktree {
        worktree: PathBuf,
        rationale: String,
        validation_evidence: Vec<String>,
    },
}

fn execute_validator_ops_actions(
    connection: &Connection,
    artifact_root: &Path,
    run_id: i64,
    agent: &ProcessCapture,
) -> anyhow::Result<()> {
    let Some(envelope) = parse_validator_ops_envelope(agent)? else {
        return Ok(());
    };
    let mut report = format!(
        "# Validator operational actions: run {run_id}\n\nValidator operational authority is guarded: actions require validator exit code 0, recorded rationale, recorded evidence, and runtime safety checks. Denials are non-bypassing safe failures.\n"
    );
    if agent.exit_code != Some(0) {
        report.push_str(&format!(
            "\n- denied: validator exit code {:?}; operational actions require successful validator execution.\n",
            agent.exit_code
        ));
    } else {
        for action in envelope.actions {
            let outcome = execute_validator_ops_action(connection, run_id, action);
            report.push_str(&format!("\n- {}\n", outcome.replace('\n', "\n  ")));
        }
    }
    let report_path = artifact_root.join(format!("loop-run-{run_id}-validator-ops.md"));
    fs::write(&report_path, &report).with_context(|| {
        format!(
            "failed to write validator operational action report {}",
            report_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &report_path,
        "Validator operational action audit trail with guarded allow/deny outcomes.",
    )?;
    add_observation(
        connection,
        run_id,
        &format!(
            "Validator operational actions audited for run {run_id}: {}.",
            report_path.display()
        ),
    )?;
    Ok(())
}

fn parse_validator_ops_envelope(agent: &ProcessCapture) -> anyhow::Result<Option<ValidatorOpsEnvelope>> {
    let combined = format!("{}\n{}", agent.stdout, agent.stderr);
    let Some(json) = extract_fenced_block(&combined, "ldgr-validator-ops") else {
        return Ok(None);
    };
    serde_json::from_str(&json)
        .map(Some)
        .context("failed to parse ldgr-validator-ops JSON block")
}

fn execute_validator_revision_gate(
    connection: &Connection,
    artifact_root: &Path,
    work_item: &crate::store::WorkItem,
    run_id: i64,
    agent: &ProcessCapture,
) -> anyhow::Result<()> {
    let combined = format!("{}\n{}", agent.stdout, agent.stderr);
    let Some(json) = extract_fenced_block(&combined, "ldgr-validator-revision") else {
        return Ok(());
    };
    let report_path = artifact_root.join(format!("loop-run-{run_id}-validator-revision.md"));
    let mut report = format!(
        "# Validator revision gate: run {run_id}\n\nValidator revision authority is risk-based and proportionate: it refuses materially inadequate work without requiring perfection, and converts the refusal into bounded revision work visible to the next planner and worker.\n"
    );
    if agent.exit_code != Some(0) {
        report.push_str(&format!(
            "\n- denied: validator exit code {:?}; revision gates require successful validator execution.\n",
            agent.exit_code
        ));
        write_validator_revision_report(connection, artifact_root, run_id, &report_path, &report)?;
        return Ok(());
    }
    let envelope = match serde_json::from_str::<ValidatorRevisionEnvelope>(&json) {
        Ok(envelope) => envelope,
        Err(error) => {
            report.push_str(&format!(
                "\n- denied safely: failed to parse ldgr-validator-revision JSON block: {error}.\n"
            ));
            write_validator_revision_report(connection, artifact_root, run_id, &report_path, &report)?;
            return Ok(());
        }
    };
    let rationale = envelope.rationale.trim();
    let corrections = envelope
        .required_corrections
        .iter()
        .map(|item| item.trim())
        .filter(|item| !item.is_empty())
        .collect::<Vec<_>>();
    if rationale.is_empty() || corrections.is_empty() {
        report.push_str("\n- denied safely: revision refusal requires non-empty rationale and required_corrections.\n");
        write_validator_revision_report(connection, artifact_root, run_id, &report_path, &report)?;
        return Ok(());
    }

    let revision_slug = format!("{}-revision-run-{run_id}", work_item.slug);
    let corrections_md = corrections
        .iter()
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n");
    let affected_artifacts = envelope
        .affected_artifacts
        .iter()
        .filter(|item| !item.trim().is_empty())
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n");
    let affected_work_items = envelope
        .affected_work_items
        .iter()
        .filter(|item| !item.trim().is_empty())
        .map(|item| format!("- {item}"))
        .collect::<Vec<_>>()
        .join("\n");
    let revision_title = format!("Revise {} after validator refusal", work_item.slug);
    let revision_description = format!(
        "Validator refused the previous role cycle for `{}` as materially inadequate.\n\nRationale:\n{}\n\nRequired corrections:\n{}\n\nAffected artifacts:\n{}\n\nAffected work items:\n{}\n\nWorker instructions: perform only the bounded corrections above, preserve useful progress that already has adequate evidence, add/repair validation evidence, and then record concise durable evidence for a follow-up validator acceptance review.",
        work_item.slug,
        rationale,
        corrections_md,
        if affected_artifacts.is_empty() { "- none specified" } else { &affected_artifacts },
        if affected_work_items.is_empty() { "- none specified" } else { &affected_work_items },
    );
    report.push_str(&format!(
        "\n## Refusal accepted\n\n- Revision work: `{revision_slug}`\n- Rationale: {rationale}\n\n## Required corrections\n\n{corrections_md}\n\n## Affected artifacts\n\n{}\n\n## Affected work items\n\n{}\n",
        if affected_artifacts.is_empty() { "- none specified" } else { &affected_artifacts },
        if affected_work_items.is_empty() { "- none specified" } else { &affected_work_items },
    ));
    write_validator_revision_report(connection, artifact_root, run_id, &report_path, &report)?;
    add_observation(
        connection,
        run_id,
        &format!(
            "Validator refused {} and required bounded revision work {}; rationale: {}; required corrections: {}. Planner must inspect {} before choosing next direction.",
            work_item.slug,
            revision_slug,
            rationale,
            corrections.join("; "),
            report_path.display()
        ),
    )?;
    if get_run(connection, run_id)?.status == RunStatus::Running {
        close_run(
            connection,
            run_id,
            RunStatus::Partial,
            Some("validator requested bounded revision before acceptance"),
            DecisionOutcome::Continue,
            &format!(
                "Validator refusal requires revision before accepting {}; see {}.",
                work_item.slug,
                report_path.display()
            ),
            Some(NextWorkSpec {
                slug: &revision_slug,
                title: Some(&revision_title),
                description: Some(&revision_description),
            }),
        )?;
        record_run_phase(
            connection,
            run_id,
            "validator_revision_required",
            &format!("Validator requested revision work {revision_slug} for {}.", work_item.slug),
        )?;
    }
    Ok(())
}

fn write_validator_revision_report(
    connection: &Connection,
    artifact_root: &Path,
    run_id: i64,
    report_path: &Path,
    report: &str,
) -> anyhow::Result<()> {
    fs::write(report_path, report).with_context(|| {
        format!(
            "failed to write validator revision gate report {}",
            report_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        report_path,
        "Validator refusal/revision gate report with rationale, required corrections, and affected evidence.",
    )?;
    Ok(())
}

fn extract_fenced_block(text: &str, info: &str) -> Option<String> {
    let mut in_block = false;
    let mut body = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if !in_block {
            if trimmed == format!("```{info}") || trimmed == format!("```{info} json") {
                in_block = true;
            }
            continue;
        }
        if trimmed == "```" {
            return Some(body.join("\n"));
        }
        body.push(line);
    }
    None
}

fn execute_validator_ops_action(
    connection: &Connection,
    run_id: i64,
    action: ValidatorOpsAction,
) -> String {
    match action {
        ValidatorOpsAction::ClearBlock {
            intervention_id,
            rationale,
            evidence,
        } => execute_validator_clear_block(connection, run_id, intervention_id, &rationale, &evidence),
        ValidatorOpsAction::MergeWorktree {
            worktree,
            rationale,
            validation_evidence,
        } => execute_validator_merge_worktree(run_id, &worktree, &rationale, &validation_evidence),
    }
}

fn execute_validator_clear_block(
    connection: &Connection,
    run_id: i64,
    intervention_id: i64,
    rationale: &str,
    evidence: &[String],
) -> String {
    if rationale.trim().is_empty() {
        return format!("clear_block intervention_id={intervention_id} denied: rationale is required");
    }
    if evidence.iter().all(|item| item.trim().is_empty()) {
        return format!("clear_block intervention_id={intervention_id} denied: evidence is required");
    }
    match clear_loop_intervention(
        connection,
        intervention_id,
        Some(&format!("validator run {run_id}: {rationale}")),
    ) {
        Ok(intervention) => format!(
            "clear_block intervention_id={} status={} rationale={} evidence={:?}",
            intervention.id,
            intervention.status.as_str(),
            rationale,
            evidence
        ),
        Err(error) => format!("clear_block intervention_id={intervention_id} failed safely: {error:#}"),
    }
}

fn execute_validator_merge_worktree(
    run_id: i64,
    worktree: &Path,
    rationale: &str,
    validation_evidence: &[String],
) -> String {
    if rationale.trim().is_empty() {
        return format!("merge_worktree worktree={} denied: rationale is required", worktree.display());
    }
    if validation_evidence.iter().all(|item| item.trim().is_empty()) {
        return format!("merge_worktree worktree={} denied: clean validation evidence is required", worktree.display());
    }
    match merge_worktree_guarded(worktree) {
        Ok(message) => format!(
            "merge_worktree worktree={} applied: {}; rationale={}; validation_evidence={:?}",
            worktree.display(),
            message,
            rationale,
            validation_evidence
        ),
        Err(error) => format!(
            "merge_worktree worktree={} denied/failed safely for validator run {run_id}: {error:#}",
            worktree.display()
        ),
    }
}

fn merge_worktree_guarded(worktree: &Path) -> anyhow::Result<String> {
    if !worktree.is_dir() {
        bail!("worktree path does not exist or is not a directory");
    }
    let target = std::env::current_dir().context("failed to resolve target repository directory")?;
    ensure_clean_git_status(&target, "target repository")?;
    ensure_clean_git_status(worktree, "source worktree")?;
    let target_common = git_output(&target, &["rev-parse", "--git-common-dir"])?;
    let source_common = git_output(worktree, &["rev-parse", "--git-common-dir"])?;
    let target_common = canonical_git_path(&target, target_common.trim())?;
    let source_common = canonical_git_path(worktree, source_common.trim())?;
    if target_common != source_common {
        bail!("source worktree is not registered to the target repository");
    }
    let branch = git_output(worktree, &["branch", "--show-current"])?;
    let branch = branch.trim();
    if branch.is_empty() {
        bail!("source worktree is detached; refusing merge without a branch name");
    }
    let current = git_output(&target, &["branch", "--show-current"])?;
    if current.trim() == branch {
        bail!("source worktree is on the target branch; refusing self-merge");
    }
    let merge = Command::new("git")
        .arg("merge")
        .arg("--no-ff")
        .arg("--no-commit")
        .arg(branch)
        .current_dir(&target)
        .output()
        .context("failed to launch git merge")?;
    if !merge.status.success() {
        let _ = Command::new("git").arg("merge").arg("--abort").current_dir(&target).output();
        bail!(
            "git merge failed and abort was attempted: {}{}",
            String::from_utf8_lossy(&merge.stdout),
            String::from_utf8_lossy(&merge.stderr)
        );
    }
    Ok(format!("merged branch {branch} into {} without committing", target.display()))
}

fn ensure_clean_git_status(repo: &Path, label: &str) -> anyhow::Result<()> {
    let status = git_output(repo, &["status", "--porcelain=v1"])?;
    if !status.trim().is_empty() {
        bail!("{label} has uncommitted changes; refusing validator merge")
    }
    Ok(())
}

fn git_output(repo: &Path, args: &[&str]) -> anyhow::Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .with_context(|| format!("failed to run git {} in {}", args.join(" "), repo.display()))?;
    if !output.status.success() {
        bail!(
            "git {} failed in {}: {}{}",
            args.join(" "),
            repo.display(),
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).to_string())
}

fn canonical_git_path(repo: &Path, path: &str) -> anyhow::Result<PathBuf> {
    let raw = Path::new(path);
    let joined = if raw.is_absolute() { raw.to_path_buf() } else { repo.join(raw) };
    joined
        .canonicalize()
        .with_context(|| format!("failed to canonicalize git path {}", joined.display()))
}

fn write_validator_advisory(
    connection: &Connection,
    artifact_root: &Path,
    work_item: &crate::store::WorkItem,
    run_id: i64,
    validator_result: &LoopRoleResult,
) -> anyhow::Result<()> {
    let advisory_path = artifact_root.join(format!("loop-run-{run_id}-validator-advisory.md"));
    let advisory = format!(
        "# Validator advisory review: run {run_id}\n\n\
         The validator is an independent third-party observer for this bounded cycle. This runtime-maintained handoff preserves where the validator interpretation lives without converting it into executor authority.\n\n\
         ## Assigned work\n\n\
         - Work slug: `{}`\n\
         - Title: {}\n\
         - Description: {}\n\n\
         ## Required review dimensions\n\n\
         The validator output should be read for its advisory interpretation of methodology, interpreted outcomes, claim strength, evidence quality, risks, and next-direction recommendations.\n\n\
         ## Preserved advisory evidence\n\n\
         - Validator prompt artifact: `{}`\n\
         - Validator output artifact: `{}`\n\
         - Validator exit code: {:?}\n\n\
         ## Planner handoff\n\n\
         The next planner cycle should inspect this advisory report and the validator output artifact before selecting follow-up work. The validator advises planner and worker roles but is not the primary executor.\n",
        work_item.slug,
        work_item.title,
        work_item.description,
        validator_result.prompt_artifact_path.display(),
        validator_result.output_artifact_path.display(),
        validator_result.agent_exit_code,
    );
    fs::write(&advisory_path, advisory).with_context(|| {
        format!(
            "failed to write validator advisory report {}",
            advisory_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &advisory_path,
        "Validator advisory handoff preserving independent review perspective for the next planner cycle.",
    )?;
    add_observation(
        connection,
        run_id,
        &format!(
            "Validator advisory perspective recorded for {}: report {}, output {}; review covers methodology, interpreted outcomes, claim strength, evidence quality, risks, and next-direction recommendations for planner handoff.",
            work_item.slug,
            advisory_path.display(),
            validator_result.output_artifact_path.display()
        ),
    )?;
    Ok(())
}

fn write_scryb_reports(
    connection: &Connection,
    artifact_root: &Path,
    work_item: &crate::store::WorkItem,
    run_id: i64,
    role_results: &[LoopRoleResult],
) -> anyhow::Result<()> {
    let report_path = artifact_root.join(format!("loop-run-{run_id}-scryb-cycle-summary.md"));
    let reference_path = artifact_root.join(format!("loop-run-{run_id}-scryb-reference.md"));
    let meta_report_path = artifact_root.join("loop-meta-report.md");

    let role_rows = role_results
        .iter()
        .map(|result| {
            format!(
                "| {} | {:?} | `{}` | `{}` |",
                result.role,
                result.agent_exit_code,
                result.prompt_artifact_path.display(),
                result.output_artifact_path.display()
            )
        })
        .collect::<Vec<_>>()
        .join("\n");
    let report = format!(
        "# Scryb cycle summary: run {run_id}\n\n\
         This report is generated only from LDGR-recorded run metadata and role artifact paths. It does not add claims about correctness beyond the captured evidence.\n\n\
         ## Assigned work\n\n\
         - Work slug: `{}`\n\
         - Title: {}\n\
         - Description: {}\n\n\
         ## Recorded role evidence\n\n\
         | Role | Exit code | Prompt artifact | Output artifact |\n\
         | --- | --- | --- | --- |\n\
         {}\n\n\
         ## Report paths\n\n\
         - Cycle summary: `{}`\n\
         - Human reference: `{}`\n\
         - Meta-report: `{}`\n",
        work_item.slug,
        work_item.title,
        work_item.description,
        role_rows,
        report_path.display(),
        reference_path.display(),
        meta_report_path.display()
    );
    fs::write(&report_path, report)
        .with_context(|| format!("failed to write scryb cycle summary {}", report_path.display()))?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &report_path,
        "Scryb-maintained concise cycle summary from recorded LDGR evidence.",
    )?;

    let reference = format!(
        "# Human reference for run {run_id}\n\n\
         Use this page as an approachable index to the recorded evidence for `{}`.\n\n\
         - Start with the scryb cycle summary: `{}`\n\
         - Inspect role outputs listed in that summary for the actual agent evidence.\n\
         - Treat this document as an index, not as independent validation.\n",
        work_item.slug,
        report_path.display()
    );
    fs::write(&reference_path, reference)
        .with_context(|| format!("failed to write scryb reference {}", reference_path.display()))?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &reference_path,
        "Scryb-maintained human-readable reference index for recorded run evidence.",
    )?;

    let mut meta_file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&meta_report_path)
        .with_context(|| format!("failed to open scryb meta-report {}", meta_report_path.display()))?;
    writeln!(
        meta_file,
        "\n## Run {run_id}: {}\n\n- Work: `{}`\n- Cycle summary: `{}`\n- Human reference: `{}`\n- Evidence boundary: generated from recorded LDGR run metadata and artifact paths only.\n",
        work_item.title,
        work_item.slug,
        report_path.display(),
        reference_path.display()
    )?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &meta_report_path,
        "Append-only scryb meta-report of loop results and interpretations.",
    )?;

    add_observation(
        connection,
        run_id,
        &format!(
            "Scryb report artifacts recorded for {}: cycle summary {}, human reference {}, meta-report {}.",
            work_item.slug,
            report_path.display(),
            reference_path.display(),
            meta_report_path.display()
        ),
    )?;
    Ok(())
}

fn write_compat_agent_output_artifact(
    connection: &Connection,
    artifact_root: &Path,
    run_id: i64,
    role: &str,
    agent: &ProcessCapture,
    source_path: &Path,
) -> anyhow::Result<()> {
    let compat_path = artifact_root.join(format!("loop-run-{run_id}-agent-output.md"));
    if let Some(stdout_path) = &agent.stdout_artifact_path {
        fs::copy(
            stdout_path,
            artifact_root.join(format!("loop-run-{run_id}-agent-stdout.txt")),
        )?;
    }
    if let Some(stderr_path) = &agent.stderr_artifact_path {
        fs::copy(
            stderr_path,
            artifact_root.join(format!("loop-run-{run_id}-agent-stderr.txt")),
        )?;
    }
    let markdown = fs::read_to_string(source_path)?
        .replace(
            &format!("loop-run-{run_id}-{role}-agent-stdout.txt"),
            &format!("loop-run-{run_id}-agent-stdout.txt"),
        )
        .replace(
            &format!("loop-run-{run_id}-{role}-agent-stderr.txt"),
            &format!("loop-run-{run_id}-agent-stderr.txt"),
        );
    fs::write(&compat_path, markdown).with_context(|| {
        format!(
            "failed to write compatibility agent output artifact {}",
            compat_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &compat_path,
        "Autonomous loop agent stdout/stderr capture.",
    )?;
    Ok(())
}

fn run_role_agent(
    artifact_root: &Path,
    options: &LoopRuntimeOptions,
    run_id: i64,
    role: &str,
    work_slug: &str,
    rendered_prompt: &str,
) -> anyhow::Result<ProcessCapture> {
    let run_id_text = run_id.to_string();
    // Only the final role (validator) is authorized to close the assigned run
    // with the cycle decision, so the sequence completes before closure.
    let may_close_flag = if role == "validator" { "1" } else { "0" };
    let role_env = [
        ("LDGR_LOOP_ROLE", role),
        ("LDGR_LOOP_STOP_AUTHORITY", "planner"),
        ("LDGR_LOOP_ASSIGNED_RUN_ID", run_id_text.as_str()),
        ("LDGR_LOOP_ASSIGNED_WORK_SLUG", work_slug),
        ("LDGR_LOOP_MAY_CLOSE_RUN", may_close_flag),
    ];
    match &options.agent {
        LoopAgent::DryRun => Ok(ProcessCapture::from_memory(
            None,
            0,
            format!("dry-run: fresh {role} agent would run here\n"),
            String::new(),
        )),
        LoopAgent::Argv(argv) => run_process_with_stdin_env(
            argv,
            rendered_prompt,
            options.stream_agent_output,
            process_output_paths(artifact_root, run_id, &format!("{role}-agent"))?,
            options.agent_timeout,
            &role_env,
            options.live_progress.then_some(options.progress_heartbeat),
            Some(format!("run={run_id} work={work_slug} role={role}")),
        ),
        LoopAgent::Agentctl => {
            let argv = default_agentctl_argv();
            run_process_with_stdin_env(
                &argv,
                rendered_prompt,
                options.stream_agent_output,
                process_output_paths(artifact_root, run_id, &format!("{role}-agent"))?,
                options.agent_timeout,
                &role_env,
                options.live_progress.then_some(options.progress_heartbeat),
                Some(format!("run={run_id} work={work_slug} role={role}")),
            )
        }
    }
}

fn resolve_role_prompt_source(
    connection: &Connection,
    source: &LoopPromptSource,
    base_prompt: &ResolvedLoopPrompt,
    role: &str,
) -> anyhow::Result<ResolvedLoopPrompt> {
    match source {
        LoopPromptSource::Path(path) => {
            let role_path = path
                .parent()
                .unwrap_or_else(|| Path::new("."))
                .join(format!("ldgr-loop-{role}.md"));
            if role_path.is_file() {
                let mut resolved = resolve_prompt_source(connection, &LoopPromptSource::Path(role_path))?;
                resolved.provenance.prompt_role = Some(role.to_owned());
                apply_role_prompt_metadata(&mut resolved, role);
                Ok(resolved)
            } else {
                Ok(base_role_prompt(base_prompt, role))
            }
        }
        LoopPromptSource::Bundle { slug, .. } => {
            let mut resolved = match resolve_prompt_source(
                connection,
                &LoopPromptSource::Bundle {
                    slug: slug.clone(),
                    prompt_role: Some(role.to_owned()),
                },
            ) {
                Ok(resolved) => resolved,
                Err(_) => base_role_prompt(base_prompt, role),
            };
            apply_role_prompt_metadata(&mut resolved, role);
            Ok(resolved)
        },
        LoopPromptSource::StoredPrompt { .. } => Ok(base_role_prompt(base_prompt, role)),
    }
}

fn base_role_prompt(base_prompt: &ResolvedLoopPrompt, role: &str) -> ResolvedLoopPrompt {
    let mut resolved = base_prompt.clone();
    resolved.description = format!("Using base loop prompt for {role} role.");
    resolved.provenance.prompt_role = Some(role.to_owned());
    apply_role_prompt_metadata(&mut resolved, role);
    resolved
}

fn apply_role_prompt_metadata(resolved: &mut ResolvedLoopPrompt, role: &str) {
    if role == "validator" {
        resolved.provenance.thinking_level_intent = Some(
            "xhigh where supported for independent advisory review".to_owned(),
        );
        resolved.provenance.advisory_intent = Some(
            "third-party observer only; reviews methodology, interpreted outcomes, claim strength, evidence quality, risks, and next-direction recommendations; advises planner/worker without primary executor authority".to_owned(),
        );
    }
}

fn run_post_cycle_summary(
    connection: &Connection,
    artifact_root: &Path,
    options: &LoopRuntimeOptions,
    work_slug: &str,
    run_id: i64,
    agent_output_path: &Path,
    agent_exit_code: Option<i32>,
) -> anyhow::Result<(Option<PathBuf>, Option<i32>)> {
    let Some(summary_agent) = &options.summary_agent else {
        return Ok((None, None));
    };
    record_run_phase(
        connection,
        run_id,
        "running_summary_agent",
        &format!("Running one-shot post-cycle summary agent for {work_slug}."),
    )?;
    let context = read_context(connection)?;
    let prompt = format!(
        "You are a one-shot LDGR summary agent. Do not continue the work. Summarize only the completed run for UI/log consumption.\n\nRun: {run_id}\nWork: {work_slug}\nAgent exit code: {}\nAgent output artifact: {}\n\nUse compact markdown with: outcome, evidence, notable files/artifacts, next work/blocker if visible. Do not invent results.\n\nLDGR context:\n```json\n{}\n```\n",
        agent_exit_code
            .map(|code| code.to_string())
            .unwrap_or_else(|| "unknown".to_owned()),
        agent_output_path.display(),
        serde_json::to_string_pretty(&context)?
    );
    let argv = agent_output_argv(summary_agent);
    let mut summary = match summary_agent {
        LoopAgent::DryRun => ProcessCapture::from_memory(
            None,
            0,
            "dry-run: summary agent would run here\n".to_owned(),
            String::new(),
        ),
        LoopAgent::Argv(argv) => run_process_with_stdin(
            argv,
            &prompt,
            options.stream_agent_output,
            process_output_paths(artifact_root, run_id, "summary-agent")?,
            options.agent_timeout,
        )?,
        LoopAgent::Agentctl => {
            let argv = default_agentctl_argv();
            run_process_with_stdin(
                &argv,
                &prompt,
                options.stream_agent_output,
                process_output_paths(artifact_root, run_id, "summary-agent")?,
                options.agent_timeout,
            )?
        }
    };
    if matches!(summary_agent, LoopAgent::Agentctl) {
        summary = enrich_agentctl_failure_output(summary);
    }
    let summary_path = artifact_root.join(format!("loop-run-{run_id}-summary.md"));
    fs::write(
        &summary_path,
        summary.to_markdown("Post-cycle summary agent output", &argv),
    )
    .with_context(|| {
        format!(
            "failed to write post-cycle summary artifact {}",
            summary_path.display()
        )
    })?;
    add_artifact(
        connection,
        artifact_root,
        run_id,
        ArtifactKind::Report,
        &summary_path,
        "One-shot post-cycle summary agent output for UI/log consumption.",
    )?;
    append_summary_log(&options.summary_log, run_id, work_slug, &summary)?;
    Ok((Some(summary_path), summary.exit_code))
}

fn append_summary_log(
    path: &Path,
    run_id: i64,
    work_slug: &str,
    summary: &ProcessCapture,
) -> anyhow::Result<()> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .with_context(|| format!("failed to create summary log dir {}", parent.display()))?;
    }
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
        .with_context(|| format!("failed to open summary log {}", path.display()))?;
    writeln!(file, "\n## Run {run_id}: {work_slug}\n")?;
    if summary.exit_code != Some(0) {
        writeln!(file, "> summary agent exit: {:?}\n", summary.exit_code)?;
    }
    if summary.stdout.trim().is_empty() {
        writeln!(file, "_No summary output._")?;
    } else {
        writeln!(file, "{}", summary.stdout.trim_end())?;
    }
    if !summary.stderr.trim().is_empty() {
        writeln!(
            file,
            "\n<details><summary>summary stderr</summary>\n\n```text\n{}\n```\n</details>",
            summary.stderr.trim_end()
        )?;
    }
    Ok(())
}

