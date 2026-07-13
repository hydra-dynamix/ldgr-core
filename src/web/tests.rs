#[cfg(test)]
mod tests {
    use std::fs;

    use super::{
        checked_artifact_path, loop_prompt_source_args, validate_exposure_options, WebOptions,
        APP_JS, INDEX_HTML,
    };

    #[test]
    fn cockpit_asset_omits_adapter_research_surface() {
        assert!(!INDEX_HTML.contains("Due revalidation"));
        assert!(!INDEX_HTML.contains(r#"id="due-revalidation""#));
        assert!(!INDEX_HTML.contains("Failures"));
        assert!(!INDEX_HTML.contains("Evidence"));
        assert!(!INDEX_HTML.contains("Tools"));
        assert!(!APP_JS.contains("due_fact_revalidation_policies"));
        assert!(!APP_JS.contains("renderDueRevalidation"));
        assert!(!APP_JS.contains("revalidation_expectation_slug"));
        assert!(!APP_JS.contains("readinessAudit"));
        assert!(!APP_JS.contains("renderFailure"));
        assert!(!APP_JS.contains("/api/tools/"));
    }

    #[test]
    fn cockpit_asset_route_logic_decodes_before_reencoding() {
        assert!(APP_JS.contains("function encodedRouteSegment"));
        assert!(APP_JS.contains("decodeURIComponent(segment)"));
        assert!(!APP_JS.contains("'/api/work/' + encodeURIComponent(path.slice"));
    }

    #[test]
    fn cockpit_asset_exposes_supported_loop_launch_options() {
        for expected in [
            "loop-prompt-slug",
            "loop-bundle",
            "loop-prompt-role",
            "loop-agent-argv",
            "loop-agent-timeout-seconds",
            "loop-stream-agent-output",
            "loop-max-iterations",
        ] {
            assert!(APP_JS.contains(expected), "{expected}");
        }
    }

    #[test]
    fn cockpit_asset_omits_adapter_specific_wave_management() {
        for excluded in ["data-view=\"waves\"", "/api/conduct/waves", "Conduct feed tail"] {
            assert!(!INDEX_HTML.contains(excluded) && !APP_JS.contains(excluded));
        }
    }

    #[test]
    fn exposure_options_require_explicit_unsafe_token_for_non_loopback() {
        let options = WebOptions {
            unsafe_expose: false,
            control_token: "secret".to_string(),
        };
        assert!(validate_exposure_options("127.0.0.1", &options).is_ok());
        assert!(validate_exposure_options(
            "0.0.0.0",
            &WebOptions {
                unsafe_expose: false,
                control_token: "secret".to_string(),
            },
        )
        .is_err());
        assert!(validate_exposure_options(
            "0.0.0.0",
            &WebOptions {
                unsafe_expose: true,
                control_token: "secret".to_string(),
            },
        )
        .is_ok());
        assert!(validate_exposure_options(
            "0.0.0.0",
            &WebOptions {
                unsafe_expose: true,
                control_token: String::new(),
            },
        )
        .is_err());
    }

    #[test]
    fn generated_control_tokens_are_header_safe() -> anyhow::Result<()> {
        let token = super::generate_control_token()?;
        assert_eq!(token.len(), 64);
        assert!(token.bytes().all(|byte| byte.is_ascii_hexdigit()));
        Ok(())
    }

    #[test]
    fn checked_artifact_path_rejects_escape_attempts() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join("artifacts");
        fs::create_dir_all(&root)?;
        let inside = root.join("inside.txt");
        fs::write(&inside, "inside")?;
        let outside = temp.path().join("outside.txt");
        fs::write(&outside, "outside")?;

        assert!(checked_artifact_path(&root, std::path::Path::new("inside.txt")).is_ok());
        assert!(checked_artifact_path(&root, &outside).is_err());
        Ok(())
    }

    #[test]
    fn checked_artifact_path_accepts_only_current_relative_records() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let root = temp.path().join(".ldgr/artifacts");
        fs::create_dir_all(&root)?;
        let report = root.join("report.md");
        fs::write(&report, "report")?;
        let resolved = report.canonicalize()?;

        assert_eq!(
            checked_artifact_path(&root, std::path::Path::new("report.md"))?,
            resolved
        );
        assert!(checked_artifact_path(&root, &report).is_err());
        assert!(
            checked_artifact_path(&root, std::path::Path::new(".ldgr/artifacts/report.md"))
                .is_err()
        );
        Ok(())
    }

    #[test]
    fn loop_prompt_source_args_accepts_path_prompt_slug_or_bundle() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        let prompt = temp.path().join("loop-prompt.md");
        fs::write(&prompt, "{{ldgr_context}}")?;
        let prompt_text = prompt.display().to_string();

        assert_eq!(
            loop_prompt_source_args(&form(&[("prompt", prompt_text.as_str())]))?,
            vec!["--prompt", prompt_text.as_str()]
        );
        assert_eq!(
            loop_prompt_source_args(&form(&[("prompt_slug", "surface")]))?,
            vec!["--prompt-slug", "surface"]
        );
        assert_eq!(
            loop_prompt_source_args(&form(&[
                ("bundle", "cleanroom"),
                ("prompt_role", "surface-loop"),
            ]))?,
            vec!["--bundle", "cleanroom", "--prompt-role", "surface-loop"]
        );
        assert!(loop_prompt_source_args(&form(&[])).is_err());
        assert!(loop_prompt_source_args(&form(&[
            ("prompt", prompt_text.as_str()),
            ("prompt_slug", "surface"),
        ]))
        .is_err());
        Ok(())
    }

    fn form(values: &[(&str, &str)]) -> Vec<(String, String)> {
        values
            .iter()
            .map(|(key, value)| ((*key).to_owned(), (*value).to_owned()))
            .collect()
    }
}
