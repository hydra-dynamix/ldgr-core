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

