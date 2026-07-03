#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{
        create_work_item, get_work_item_by_slug, list_artifacts, list_decisions,
        list_loop_interventions, list_observations, open_store, request_loop_intervention,
        set_work_item_status,
        LoopInterventionAction, WorkItemStatus,
    };

    fn temp_loop_store() -> anyhow::Result<(tempfile::TempDir, rusqlite::Connection)> {
        let temp = tempfile::tempdir()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        Ok((temp, connection))
    }

    fn write_role_prompts(root: &Path) -> anyhow::Result<PathBuf> {
        fs::create_dir_all(root)?;
        let base = root.join("ldgr-core-loop.md");
        fs::write(&base, "BASE {{ldgr_context}}")?;
        for role in LOOP_ROLES {
            fs::write(
                root.join(format!("ldgr-loop-{role}.md")),
                format!("ROLE: {role}\n{{{{ldgr_context}}}}"),
            )?;
        }
        Ok(base)
    }

    fn sequence_options(prompt: PathBuf, agent: LoopAgent, dry_run: bool) -> LoopRuntimeOptions {
        LoopRuntimeOptions {
            prompt: LoopPromptSource::Path(prompt),
            agent,
            audit_argv: None,
            summary_agent: None,
            summary_log: PathBuf::from("summary.md"),
            project_complete_requested: false,
            dry_run,
            stream_agent_output: false,
            live_progress: false,
            progress_heartbeat: Duration::from_secs(0),
            agent_timeout: DEFAULT_LOOP_PROCESS_TIMEOUT,
        }
    }

    fn role_logging_agent(temp: &Path, fail_role: Option<&str>) -> anyhow::Result<(Vec<String>, PathBuf)> {
        let log_path = temp.join("roles.log");
        let script = temp.join("role-agent.sh");
        let fail_check = fail_role
            .map(|role| format!("grep -q 'ROLE: {role}' \"$prompt\" && exit 7\n"))
            .unwrap_or_default();
        fs::write(
            &script,
            format!(
                "#!/bin/sh\nprompt=$(mktemp)\ncat >\"$prompt\"\nfor role in planner worker scryb validator; do\n  if grep -q \"ROLE: $role\" \"$prompt\"; then echo $role >>\"$1\"; fi\ndone\n{fail_check}printf ok\n"
            ),
        )?;
        Ok((
            vec!["sh".to_owned(), script.display().to_string(), log_path.display().to_string()],
            log_path,
        ))
    }

    #[test]
    fn generic_role_sequence_runs_fresh_agent_for_each_role() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "seq", "Sequence", "Run all roles")?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let (argv, log_path) = role_logging_agent(temp.path(), None)?;
        let artifacts = temp.path().join("artifacts");

        let outcome = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::Argv(argv), false),
        )?;

        let LoopRuntimeOutcome::Completed(result) = outcome else { panic!("unexpected outcome") };
        assert_eq!(result.agent_exit_code, Some(0));
        assert_eq!(result.role_results.len(), 4);
        assert_eq!(fs::read_to_string(log_path)?, "planner\nworker\nscryb\nvalidator\n");
        assert_eq!(get_work_item_by_slug(&connection, "seq")?.status, WorkItemStatus::Running);
        Ok(())
    }

    #[test]
    fn generic_role_sequence_stops_on_agent_failure() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "seq-fail", "Sequence fail", "Fail at scryb")?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let (argv, log_path) = role_logging_agent(temp.path(), Some("scryb"))?;
        let artifacts = temp.path().join("artifacts");

        let outcome = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::Argv(argv), false),
        )?;

        let LoopRuntimeOutcome::Completed(result) = outcome else { panic!("unexpected outcome") };
        assert_eq!(result.agent_exit_code, Some(7));
        assert_eq!(result.role_results.len(), 3);
        assert_eq!(fs::read_to_string(log_path)?, "planner\nworker\nscryb\n");
        Ok(())
    }

    #[test]
    fn generic_role_sequence_dry_run_writes_full_role_evidence() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "seq-dry", "Sequence dry", "Dry run roles")?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");

        let outcome = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::DryRun, true),
        )?;

        let LoopRuntimeOutcome::Completed(result) = outcome else { panic!("unexpected outcome") };
        assert_eq!(result.role_results.len(), 4);
        assert_eq!(get_work_item_by_slug(&connection, "seq-dry")?.status, WorkItemStatus::Pending);
        for role in LOOP_ROLES {
            assert!(artifacts.join(format!("loop-run-1-{role}-prompt.md")).is_file());
            assert!(artifacts.join(format!("loop-run-1-{role}-agent-output.md")).is_file());
        }
        Ok(())
    }

    #[test]
    fn generic_role_sequence_exports_role_stop_authority_environment() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "seq-env", "Sequence env", "Export role env")?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let log_path = temp.path().join("role-env.log");
        let script = temp.path().join("role-env-agent.sh");
        fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\nprintf '%s:%s:%s:%s\\n' \"$LDGR_LOOP_ROLE\" \"$LDGR_LOOP_STOP_AUTHORITY\" \"$LDGR_LOOP_ASSIGNED_WORK_SLUG\" \"$LDGR_LOOP_ASSIGNED_RUN_ID\" >>\"$1\"\n",
        )?;
        let artifacts = temp.path().join("artifacts");

        let outcome = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(
                prompt,
                LoopAgent::Argv(vec!["sh".to_owned(), script.display().to_string(), log_path.display().to_string()]),
                false,
            ),
        )?;

        let LoopRuntimeOutcome::Completed(result) = outcome else { panic!("unexpected outcome") };
        assert_eq!(result.agent_exit_code, Some(0));
        assert_eq!(
            fs::read_to_string(log_path)?,
            "planner:planner:seq-env:1\nworker:planner:seq-env:1\nscryb:planner:seq-env:1\nvalidator:planner:seq-env:1\n"
        );
        Ok(())
    }

    #[test]
    fn generic_role_sequence_records_durable_role_results() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(
            &connection,
            None,
            "worker-contract",
            "Worker contract",
            "Record role results",
        )?;
        create_work_item(
            &connection,
            None,
            "other-work",
            "Other work",
            "Must remain outside this run",
        )?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");

        let outcome = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::DryRun, true),
        )?;

        let LoopRuntimeOutcome::Completed(result) = outcome else { panic!("unexpected outcome") };
        assert_eq!(result.work_slug, "worker-contract");
        assert_eq!(get_work_item_by_slug(&connection, "other-work")?.status, WorkItemStatus::Pending);
        let observations = list_observations(&connection, Some(1), 20)?;
        assert!(
            observations.iter().any(|observation| observation.body.contains(
                "Generic loop worker role completed for assigned work item worker-contract"
            )),
            "{observations:#?}"
        );
        assert!(artifacts.join("loop-run-1-worker-agent-output.md").is_file());
        Ok(())
    }

    #[test]
    fn scryb_role_creates_evidence_bounded_reports_and_meta_report() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(
            &connection,
            None,
            "scryb-report",
            "Scryb report",
            "Create report artifacts",
        )?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");

        run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::DryRun, true),
        )?;

        let cycle_summary = artifacts.join("loop-run-1-scryb-cycle-summary.md");
        let human_reference = artifacts.join("loop-run-1-scryb-reference.md");
        let meta_report = artifacts.join("loop-meta-report.md");
        assert!(cycle_summary.is_file());
        assert!(human_reference.is_file());
        assert!(meta_report.is_file());

        let summary = fs::read_to_string(&cycle_summary)?;
        assert!(summary.contains("This report is generated only from LDGR-recorded run metadata"));
        assert!(summary.contains("loop-run-1-scryb-agent-output.md"));
        assert!(summary.contains("loop-meta-report.md"));
        assert!(!summary.contains("validated successfully"), "{summary}");
        assert!(!summary.contains("implementation is correct"), "{summary}");

        let meta = fs::read_to_string(&meta_report)?;
        assert!(meta.contains("## Run 1: Scryb report"));
        assert!(meta.contains("Evidence boundary"));

        let artifact_records = list_artifacts(&connection, Some(1), 100)?;
        assert!(artifact_records
            .iter()
            .any(|artifact| artifact.path == Path::new("loop-run-1-scryb-cycle-summary.md")));
        assert!(artifact_records
            .iter()
            .any(|artifact| artifact.path == Path::new("loop-run-1-scryb-reference.md")));
        assert!(artifact_records
            .iter()
            .any(|artifact| artifact.path == Path::new("loop-meta-report.md")));

        let observations = list_observations(&connection, Some(1), 20)?;
        assert!(observations.iter().any(|observation| {
            observation.body.contains("Scryb report artifacts recorded")
                && observation.body.contains("loop-run-1-scryb-cycle-summary.md")
                && observation.body.contains("loop-meta-report.md")
        }));
        Ok(())
    }

    #[test]
    fn generic_role_sequence_records_role_prompt_provenance() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "seq-prov", "Sequence provenance", "Record provenance")?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");

        run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::DryRun, true),
        )?;

        let provenance = fs::read_to_string(artifacts.join("loop-run-1-planner-prompt-provenance.json"))?;
        assert!(provenance.contains(r#""prompt_role": "planner""#), "{provenance}");
        assert!(provenance.contains("ldgr-loop-planner.md"), "{provenance}");
        let validator_provenance = fs::read_to_string(
            artifacts.join("loop-run-1-validator-prompt-provenance.json"),
        )?;
        assert!(
            validator_provenance.contains(r#""thinking_level_intent": "xhigh where supported"#),
            "{validator_provenance}"
        );
        assert!(validator_provenance.contains("third-party observer"), "{validator_provenance}");
        assert!(list_artifacts(&connection, Some(1), 100)?
            .iter()
            .any(|artifact| artifact.path == Path::new("loop-run-1-validator-prompt-provenance.json")));
        Ok(())
    }

    fn validator_ops_agent(temp: &Path, stdout: &str) -> anyhow::Result<Vec<String>> {
        let script = temp.join("validator-ops-agent.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\nif [ \"$LDGR_LOOP_ROLE\" = validator ]; then cat <<'EOF'\n{}\nEOF\nfi\nexit 0\n",
                stdout
            ),
        )?;
        Ok(vec!["sh".to_owned(), script.display().to_string()])
    }

    fn role_output_agent(temp: &Path, validator_stdout: &str) -> anyhow::Result<Vec<String>> {
        let script = temp.join("role-output-agent.sh");
        fs::write(
            &script,
            format!(
                "#!/bin/sh\ncat >/dev/null\ncase \"$LDGR_LOOP_ROLE\" in\n  planner) printf 'planner saw context\\n' ;;\n  worker) printf 'worker bounded execution\\n' ;;\n  scryb) printf 'scryb concise evidence\\n' ;;\n  validator) cat <<'EOF'\n{}\nEOF\n    ;;\nesac\nexit 0\n",
                validator_stdout
            ),
        )?;
        Ok(vec!["sh".to_owned(), script.display().to_string()])
    }

    #[test]
    fn validator_ops_can_clear_block_with_rationale_and_evidence() -> anyhow::Result<()> {
        let (_temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "validator-clear", "Validator clear", "Clear safe block")?;
        let intervention = request_loop_intervention(
            &connection,
            LoopInterventionAction::Steer,
            "operator requested validator review",
            Some("continue only if evidence is clean"),
            Some("test"),
        )?;
        let outcome = execute_validator_clear_block(
            &connection,
            1,
            intervention.id,
            "validation evidence is clean",
            &["cargo test passed".to_owned()],
        );

        let interventions = list_loop_interventions(&connection, 10)?;
        assert_eq!(interventions[0].status.as_str(), "cleared");
        assert!(outcome.contains("clear_block"), "{outcome}");
        assert!(outcome.contains("validation evidence is clean"), "{outcome}");
        Ok(())
    }

    fn cwd_test_lock() -> &'static std::sync::Mutex<()> {
        static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
        LOCK.get_or_init(|| std::sync::Mutex::new(()))
    }

    fn git(args: &[&str], cwd: &Path) -> anyhow::Result<()> {
        let output = Command::new("git").args(args).current_dir(cwd).output()?;
        if !output.status.success() {
            bail!(
                "git {:?} failed: {}{}",
                args,
                String::from_utf8_lossy(&output.stdout),
                String::from_utf8_lossy(&output.stderr)
            );
        }
        Ok(())
    }

    fn init_merge_repo(temp: &Path, branch_content: &str) -> anyhow::Result<(PathBuf, PathBuf)> {
        let repo = temp.join("repo");
        fs::create_dir(&repo)?;
        git(&["init", "-b", "main"], &repo)?;
        git(&["config", "user.email", "validator@example.test"], &repo)?;
        git(&["config", "user.name", "Validator Test"], &repo)?;
        fs::write(repo.join("file.txt"), "base\n")?;
        git(&["add", "file.txt"], &repo)?;
        git(&["commit", "-m", "base"], &repo)?;
        git(&["branch", "worker"], &repo)?;
        let worktree = temp.join("worker-tree");
        git(&["worktree", "add", worktree.to_str().unwrap(), "worker"], &repo)?;
        fs::write(worktree.join("file.txt"), branch_content)?;
        git(&["add", "file.txt"], &worktree)?;
        git(&["commit", "-m", "worker change"], &worktree)?;
        Ok((repo, worktree))
    }

    #[test]
    fn validator_ops_can_merge_clean_worktree_with_validation_evidence() -> anyhow::Result<()> {
        let _guard = cwd_test_lock().lock().unwrap();
        let original_dir = std::env::current_dir()?;
        let temp = tempfile::tempdir()?;
        let (repo, worktree) = init_merge_repo(temp.path(), "worker\n")?;
        std::env::set_current_dir(&repo)?;
        let outcome = execute_validator_merge_worktree(
            1,
            &worktree,
            "validated worker branch",
            &["cargo test passed".to_owned()],
        );
        std::env::set_current_dir(original_dir)?;

        assert!(outcome.contains("applied"), "{outcome}");
        assert_eq!(fs::read_to_string(repo.join("file.txt"))?, "worker\n");
        Ok(())
    }

    #[test]
    fn validator_ops_denies_unsafe_merge_when_target_dirty() -> anyhow::Result<()> {
        let _guard = cwd_test_lock().lock().unwrap();
        let original_dir = std::env::current_dir()?;
        let temp = tempfile::tempdir()?;
        let (repo, worktree) = init_merge_repo(temp.path(), "worker\n")?;
        fs::write(repo.join("dirty.txt"), "dirty\n")?;
        std::env::set_current_dir(&repo)?;
        let outcome = execute_validator_merge_worktree(
            1,
            &worktree,
            "validated worker branch",
            &["cargo test passed".to_owned()],
        );
        std::env::set_current_dir(original_dir)?;

        assert!(outcome.contains("denied/failed safely"), "{outcome}");
        assert!(outcome.contains("uncommitted changes"), "{outcome}");
        Ok(())
    }

    #[test]
    fn validator_ops_handles_merge_conflict_safely() -> anyhow::Result<()> {
        let _guard = cwd_test_lock().lock().unwrap();
        let original_dir = std::env::current_dir()?;
        let temp = tempfile::tempdir()?;
        let (repo, worktree) = init_merge_repo(temp.path(), "worker\n")?;
        fs::write(repo.join("file.txt"), "main\n")?;
        git(&["add", "file.txt"], &repo)?;
        git(&["commit", "-m", "main change"], &repo)?;
        std::env::set_current_dir(&repo)?;
        let outcome = execute_validator_merge_worktree(
            1,
            &worktree,
            "validated worker branch",
            &["cargo test passed".to_owned()],
        );
        std::env::set_current_dir(original_dir)?;

        assert!(outcome.contains("denied/failed safely"), "{outcome}");
        assert_eq!(git_output(&repo, &["status", "--porcelain=v1"])?.trim(), "");
        assert_eq!(fs::read_to_string(repo.join("file.txt"))?, "main\n");
        Ok(())
    }

    #[test]
    fn validator_ops_denies_merge_without_validation_evidence() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "validator-deny", "Validator deny", "Deny unsafe merge")?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");
        let stdout = "```ldgr-validator-ops json\n{\"actions\":[{\"action\":\"merge_worktree\",\"worktree\":\"/tmp/not-used\",\"rationale\":\"looks fine\",\"validation_evidence\":[]}]}\n```";

        run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::Argv(validator_ops_agent(temp.path(), stdout)?), false),
        )?;

        let report = fs::read_to_string(artifacts.join("loop-run-1-validator-ops.md"))?;
        assert!(report.contains("denied: clean validation evidence is required"), "{report}");
        Ok(())
    }

    #[test]
    fn validator_revision_gate_creates_linked_bounded_revision_work() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        let original = create_work_item(
            &connection,
            None,
            "validator-revise",
            "Validator revise",
            "Produce evidence that validator can refuse",
        )?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");
        let stdout = "```ldgr-validator-revision json\n{\"rationale\":\"tests were claimed but not run\",\"required_corrections\":[\"run cargo test and record output\",\"weaken unsupported correctness claim\"],\"affected_artifacts\":[\"loop-run-1-worker-agent-output.md\"],\"affected_work_items\":[\"validator-revise\"]}\n```";

        let outcome = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt.clone(), LoopAgent::Argv(role_output_agent(temp.path(), stdout)?), false),
        )?;

        let LoopRuntimeOutcome::Completed(result) = outcome else { panic!("unexpected outcome") };
        assert_eq!(result.agent_exit_code, Some(0));
        assert_eq!(get_work_item_by_slug(&connection, "validator-revise")?.status, WorkItemStatus::Done);
        let revision = get_work_item_by_slug(&connection, "validator-revise-revision-run-1")?;
        assert_eq!(revision.parent_work_item_id, Some(original.id));
        assert_eq!(revision.status, WorkItemStatus::Pending);
        assert!(revision.description.contains("run cargo test and record output"), "{}", revision.description);
        assert!(revision.description.contains("Worker instructions: perform only the bounded corrections"), "{}", revision.description);
        let decisions = list_decisions(&connection, Some("validator-revise"), 10)?;
        assert_eq!(decisions[0].next_work_slug.as_deref(), Some("validator-revise-revision-run-1"));
        let report = fs::read_to_string(artifacts.join("loop-run-1-validator-revision.md"))?;
        assert!(report.contains("Refusal accepted"), "{report}");
        assert!(report.contains("loop-run-1-worker-agent-output.md"), "{report}");

        run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::Argv(role_output_agent(temp.path(), "evidence now good enough")?), false),
        )?;
        let planner_prompt = fs::read_to_string(artifacts.join("loop-run-2-planner-prompt.md"))?;
        assert!(planner_prompt.contains("Validator refused validator-revise"), "{planner_prompt}");
        assert!(planner_prompt.contains("loop-run-1-validator-revision.md"), "{planner_prompt}");
        let worker_prompt = fs::read_to_string(artifacts.join("loop-run-2-worker-prompt.md"))?;
        assert!(worker_prompt.contains("run cargo test and record output"), "{worker_prompt}");
        Ok(())
    }

    #[test]
    fn validator_revision_gate_handles_bad_json_without_blocking_good_enough_acceptance() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "bad-revision", "Bad revision", "Malformed refusal")?;
        create_work_item(&connection, None, "good-enough", "Good enough", "No refusal should pass proportionately")?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");
        let stdout = "```ldgr-validator-revision json\n{not json}\n```";

        let first = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt.clone(), LoopAgent::Argv(role_output_agent(temp.path(), stdout)?), false),
        )?;
        let LoopRuntimeOutcome::Completed(first) = first else { panic!("unexpected outcome") };
        assert_eq!(first.agent_exit_code, Some(0));
        assert!(get_work_item_by_slug(&connection, "bad-revision-revision-run-1").is_err());
        let report = fs::read_to_string(artifacts.join("loop-run-1-validator-revision.md"))?;
        assert!(report.contains("denied safely: failed to parse"), "{report}");

        set_work_item_status(&connection, "bad-revision", WorkItemStatus::Done, Some("safe malformed gate did not create revision"))?;
        let second = run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt, LoopAgent::Argv(role_output_agent(temp.path(), "good-enough evidence accepted")?), false),
        )?;
        let LoopRuntimeOutcome::Completed(second) = second else { panic!("unexpected outcome") };
        assert_eq!(second.agent_exit_code, Some(0));
        assert!(!artifacts.join("loop-run-2-validator-revision.md").exists());
        assert!(get_work_item_by_slug(&connection, "good-enough-revision-run-2").is_err());
        Ok(())
    }

    #[test]
    fn validator_role_preserves_advisory_output_for_next_planner_cycle() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(
            &connection,
            None,
            "validator-advice",
            "Validator advice",
            "Preserve validator advisory handoff",
        )?;
        let prompt = write_role_prompts(&temp.path().join("prompts"))?;
        let artifacts = temp.path().join("artifacts");

        run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(prompt.clone(), LoopAgent::DryRun, true),
        )?;

        let advisory_path = artifacts.join("loop-run-1-validator-advisory.md");
        assert!(advisory_path.is_file());
        let advisory = fs::read_to_string(&advisory_path)?;
        assert!(advisory.contains("methodology, interpreted outcomes, claim strength, evidence quality, risks, and next-direction recommendations"), "{advisory}");
        assert!(advisory.contains("not the primary executor"), "{advisory}");
        assert!(advisory.contains("loop-run-1-validator-agent-output.md"), "{advisory}");

        let artifact_records = list_artifacts(&connection, Some(1), 100)?;
        assert!(artifact_records
            .iter()
            .any(|artifact| artifact.path == Path::new("loop-run-1-validator-advisory.md")));
        let observations = list_observations(&connection, Some(1), 20)?;
        assert!(observations.iter().any(|observation| {
            observation.body.contains("Validator advisory perspective recorded")
                && observation.body.contains("planner handoff")
        }));

        set_work_item_status(
            &connection,
            "validator-advice",
            WorkItemStatus::Done,
            Some("advance to next planner cycle"),
        )?;
        create_work_item(
            &connection,
            None,
            "next-planner",
            "Next planner",
            "Planner should see validator perspective",
        )?;
        let script = temp.path().join("planner-only.sh");
        fs::write(
            &script,
            "#!/bin/sh\ncat >/dev/null\n[ \"$LDGR_LOOP_ROLE\" = planner ] && exit 9\nexit 0\n",
        )?;

        run_loop_once(
            &connection,
            &artifacts,
            &sequence_options(
                prompt,
                LoopAgent::Argv(vec!["sh".to_owned(), script.display().to_string()]),
                false,
            ),
        )?;
        let planner_prompt = fs::read_to_string(artifacts.join("loop-run-2-planner-prompt.md"))?;
        assert!(planner_prompt.contains("Validator advisory perspective recorded"), "{planner_prompt}");
        assert!(planner_prompt.contains("loop-run-1-validator-advisory.md"), "{planner_prompt}");
        Ok(())
    }

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
