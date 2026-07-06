#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::{create_work_item, open_store};

    fn temp_loop_store() -> anyhow::Result<(tempfile::TempDir, rusqlite::Connection)> {
        let temp = tempfile::tempdir()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        Ok((temp, connection))
    }



    fn loop_options(prompt: LoopPromptSource, agent: LoopAgent, dry_run: bool) -> LoopRuntimeOptions {
        LoopRuntimeOptions {
            prompt,
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


    #[test]
    fn composite_prompt_source_concatenates_fragments_with_provenance() -> anyhow::Result<()> {
        let (temp, connection) = temp_loop_store()?;
        create_work_item(&connection, None, "compose", "Compose", "Render composite prompt")?;
        let base = temp.path().join("base.md");
        let project = temp.path().join("project.md");
        fs::write(&base, "BASE {{ldgr_context}}")?;
        fs::write(&project, "PROJECT RULES")?;
        let artifacts = temp.path().join("artifacts");

        let outcome = run_loop_once(
            &connection,
            &artifacts,
            &loop_options(
                LoopPromptSource::Composite {
                    sources: vec![
                        LoopPromptSource::Path(base.clone()),
                        LoopPromptSource::Path(project.clone()),
                    ],
                },
                LoopAgent::DryRun,
                true,
            ),
        )?;

        let LoopRuntimeOutcome::Completed(result) = outcome else { panic!("unexpected outcome") };
        let rendered = fs::read_to_string(artifacts.join(result.prompt_artifact_path))?;
        assert!(rendered.contains("BASE"), "{rendered}");
        assert!(rendered.contains("PROJECT RULES"), "{rendered}");
        assert!(rendered.contains("ldgr-prompt-fragment 1"), "{rendered}");
        assert!(rendered.contains("ldgr-prompt-fragment 2"), "{rendered}");
        let provenance = fs::read_to_string(artifacts.join("loop-run-1-prompt-provenance.json"))?;
        assert!(provenance.contains(r#""source_type": "composite""#), "{provenance}");
        assert!(provenance.contains(r#""components""#), "{provenance}");
        assert!(provenance.contains(base.to_str().unwrap()), "{provenance}");
        assert!(provenance.contains(project.to_str().unwrap()), "{provenance}");
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
