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
        let argv = timeout_with_output_argv();

        let error = run_process_with_stdin_timeout(
            &argv,
            "prompt",
            false,
            output_paths.clone(),
            short_timeout(),
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
        let argv = sleeping_child_argv();
        let prompt = "x".repeat(2 * 1024 * 1024);

        let error = run_process_with_stdin_timeout(
            &argv,
            &prompt,
            false,
            output_paths.clone(),
            short_timeout(),
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
        let argv = grandchild_holding_stdout_argv();

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

    #[cfg(unix)]
    fn timeout_with_output_argv() -> Vec<String> {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "printf started; sleep 5".to_owned(),
        ]
    }

    #[cfg(windows)]
    fn timeout_with_output_argv() -> Vec<String> {
        vec![
            "cmd".to_owned(),
            "/D".to_owned(),
            "/C".to_owned(),
            "<nul set /p =started&ping -n 6 127.0.0.1 >nul".to_owned(),
        ]
    }

    #[cfg(unix)]
    fn sleeping_child_argv() -> Vec<String> {
        vec!["sh".to_owned(), "-c".to_owned(), "sleep 5".to_owned()]
    }

    #[cfg(windows)]
    fn sleeping_child_argv() -> Vec<String> {
        vec![
            "cmd".to_owned(),
            "/D".to_owned(),
            "/C".to_owned(),
            "ping -n 6 127.0.0.1 >nul".to_owned(),
        ]
    }

    #[cfg(unix)]
    fn grandchild_holding_stdout_argv() -> Vec<String> {
        vec![
            "sh".to_owned(),
            "-c".to_owned(),
            "(sleep 5) & printf parent-done".to_owned(),
        ]
    }

    #[cfg(windows)]
    fn grandchild_holding_stdout_argv() -> Vec<String> {
        vec![
            "cmd".to_owned(),
            "/D".to_owned(),
            "/C".to_owned(),
            "start /b cmd /D /C ping -n 6 127.0.0.1 ^>nul&<nul set /p =parent-done&exit /b 0"
                .to_owned(),
        ]
    }

    #[cfg(unix)]
    fn short_timeout() -> Duration {
        Duration::from_millis(100)
    }

    #[cfg(windows)]
    fn short_timeout() -> Duration {
        Duration::from_millis(500)
    }
}
