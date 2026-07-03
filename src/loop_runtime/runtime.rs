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
    let mut agent = match &options.agent {
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
        LoopAgent::Agentctl => {
            let argv = default_agentctl_argv();
            run_process_with_stdin(
                &argv,
                &rendered_prompt,
                options.stream_agent_output,
                process_output_paths(artifact_root, run_id, "agent")?,
                options.agent_timeout,
            )?
        }
    };

    if matches!(options.agent, LoopAgent::Agentctl) {
        agent = enrich_agentctl_failure_output(agent);
    }

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

    let (summary_artifact_path, summary_exit_code) = run_post_cycle_summary(
        connection,
        artifact_root,
        options,
        work_slug,
        run_id,
        &output_path,
        agent.exit_code,
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
        summary_artifact_path,
        summary_exit_code,
        agent_exit_code: agent.exit_code,
        audit_exit_code,
    })
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

