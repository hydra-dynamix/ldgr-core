#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::{Arc, Barrier};
    use std::thread;
    use tempfile::TempDir;

    fn temp_store() -> anyhow::Result<(TempDir, Connection)> {
        let temp = TempDir::new()?;
        let connection = open_store(&temp.path().join("ldgr.sqlite3"))?;
        Ok((temp, connection))
    }

    #[test]
    fn add_observation_rolls_back_when_event_recording_fails() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(
            &connection,
            None,
            "atomic-observation",
            "Atomic observation",
            "Observation insert and event recording must commit together.",
        )?;
        let run = start_run(&connection, "atomic-observation", Some("manual"))?;
        connection.execute_batch(
            "CREATE TRIGGER fail_observation_event
             BEFORE INSERT ON event_log
             WHEN NEW.entity_type = 'observation'
             BEGIN
                 SELECT RAISE(ABORT, 'blocked observation event');
             END;",
        )?;

        let error = add_observation(&connection, run.id, "must roll back").unwrap_err();

        assert!(
            format!("{error:#}").contains("blocked observation event"),
            "{error:#}"
        );
        let observation_count: i64 = connection.query_row(
            "SELECT count(*) FROM observation WHERE run_id = ?1",
            params![run.id],
            |row| row.get(0),
        )?;
        assert_eq!(observation_count, 0);
        Ok(())
    }

    #[test]
    fn concurrent_manual_and_loop_claims_create_one_run_for_pending_work() -> anyhow::Result<()> {
        let (temp, connection) = temp_store()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        create_work_item(
            &connection,
            None,
            "claim-race",
            "Claim race",
            "Only one concurrent claimant should start this work.",
        )?;
        drop(connection);

        let claimant_count = 12;
        let barrier = Arc::new(Barrier::new(claimant_count));
        let mut handles = Vec::new();
        for index in 0..claimant_count {
            let db_path = db_path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || -> anyhow::Result<Option<i64>> {
                let connection = open_store(&db_path)?;
                barrier.wait();
                if index % 2 == 0 {
                    match start_run(&connection, "claim-race", Some("manual")) {
                        Ok(run) => Ok(Some(run.id)),
                        Err(_) => Ok(None),
                    }
                } else {
                    Ok(claim_next_pending_run(&connection, Some("loop"))?
                        .map(|claimed| claimed.run.id))
                }
            }));
        }

        let mut claimed_run_ids = Vec::new();
        for handle in handles {
            if let Some(run_id) = handle.join().expect("claim thread panicked")? {
                claimed_run_ids.push(run_id);
            }
        }

        let connection = open_store(&db_path)?;
        let runs = list_runs(&connection, None)?;
        assert_eq!(claimed_run_ids.len(), 1, "{claimed_run_ids:?}");
        assert_eq!(runs.len(), 1);
        assert_eq!(runs[0].run_id, claimed_run_ids[0]);
        assert_eq!(
            get_work_item_by_slug(&connection, "claim-race")?.status,
            WorkItemStatus::Running
        );

        Ok(())
    }

    #[test]
    fn continuing_without_next_work_is_blocked_when_no_other_work_exists() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;

        let error = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "more remains",
            None,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("continuing requires a next work item"));
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Pending
        );

        Ok(())
    }

    #[test]
    fn continuing_with_next_work_finishes_current_and_creates_child() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        let current = create_work_item(&connection, None, "current", "Current", "Current work")?;

        let decision = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "queue next",
            Some(NextWorkSpec {
                slug: "next",
                title: Some("Next"),
                description: Some("Next work"),
            }),
        )?;

        let next = get_work_item_by_slug(&connection, "next")?;
        assert_eq!(decision.next_work_item_id, Some(next.id));
        assert_eq!(next.parent_work_item_id, Some(current.id));
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Done
        );

        Ok(())
    }

    #[test]
    fn concurrent_decisions_share_next_work_without_duplicate_slug_race() -> anyhow::Result<()> {
        let (temp, connection) = temp_store()?;
        let db_path = temp.path().join("ldgr.sqlite3");
        let decider_count = 8;
        for index in 0..decider_count {
            create_work_item(
                &connection,
                None,
                &format!("current-{index}"),
                &format!("Current {index}"),
                "Concurrent current work",
            )?;
        }
        drop(connection);

        let barrier = Arc::new(Barrier::new(decider_count));
        let mut handles = Vec::new();
        for index in 0..decider_count {
            let db_path = db_path.clone();
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || -> anyhow::Result<i64> {
                let connection = open_store(&db_path)?;
                barrier.wait();
                let decision = record_decision(
                    &connection,
                    &format!("current-{index}"),
                    DecisionOutcome::Continue,
                    "share the same next slice",
                    Some(NextWorkSpec {
                        slug: "shared-next",
                        title: Some("Shared next"),
                        description: Some("Only one work item should be created."),
                    }),
                )?;
                decision
                    .next_work_item_id
                    .context("continue decision should link next work")
            }));
        }

        let mut next_ids = Vec::new();
        for handle in handles {
            next_ids.push(handle.join().expect("decision thread panicked")?);
        }

        let connection = open_store(&db_path)?;
        let shared_next = get_work_item_by_slug(&connection, "shared-next")?;
        assert!(next_ids.iter().all(|id| *id == shared_next.id));
        let shared_next_count: i64 = connection.query_row(
            "SELECT count(*) FROM work_item WHERE slug = 'shared-next'",
            [],
            |row| row.get(0),
        )?;
        assert_eq!(shared_next_count, 1);

        Ok(())
    }

    #[test]
    fn continuing_can_link_existing_next_work() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;
        let existing = create_work_item(&connection, None, "next", "Next", "Next work")?;

        let decision = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "link existing next",
            Some(NextWorkSpec {
                slug: "next",
                title: None,
                description: None,
            }),
        )?;

        assert_eq!(decision.next_work_item_id, Some(existing.id));
        assert_eq!(get_work_item_by_slug(&connection, "next")?.id, existing.id);
        assert_eq!(list_work_items(&connection, None)?.len(), 2);

        Ok(())
    }

    #[test]
    fn missing_existing_next_requires_create_details() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;

        let error = record_decision(
            &connection,
            "current",
            DecisionOutcome::Continue,
            "missing details",
            Some(NextWorkSpec {
                slug: "missing",
                title: None,
                description: None,
            }),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("supply --next-title and --next-description"));
        assert_eq!(list_work_items(&connection, None)?.len(), 1);

        Ok(())
    }

    #[test]
    fn close_run_invalid_continue_without_next_leaves_run_and_work_unchanged() -> anyhow::Result<()>
    {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;
        let run = start_run(&connection, "current", Some("cargo test"))?;

        let error = close_run(
            &connection,
            run.id,
            RunStatus::Success,
            Some("should not persist"),
            DecisionOutcome::Continue,
            "more remains",
            None,
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("continuing requires a next work item"));
        let unchanged_run = get_run_by_id(&connection, run.id)?;
        assert_eq!(unchanged_run.status, RunStatus::Running);
        assert_eq!(unchanged_run.finished_at, None);
        assert_eq!(unchanged_run.notes, None);
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Running
        );
        assert!(list_decisions(&connection, None, 10)?.is_empty());

        Ok(())
    }

    #[test]
    fn close_run_invalid_next_work_leaves_run_and_work_unchanged() -> anyhow::Result<()> {
        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "current", "Current", "Current work")?;
        let run = start_run(&connection, "current", Some("cargo test"))?;

        let error = close_run(
            &connection,
            run.id,
            RunStatus::Success,
            Some("should not persist"),
            DecisionOutcome::Continue,
            "queue missing next",
            Some(NextWorkSpec {
                slug: "missing",
                title: None,
                description: None,
            }),
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("supply --next-title and --next-description"));
        let unchanged_run = get_run_by_id(&connection, run.id)?;
        assert_eq!(unchanged_run.status, RunStatus::Running);
        assert_eq!(unchanged_run.finished_at, None);
        assert_eq!(unchanged_run.notes, None);
        assert_eq!(
            get_work_item_by_slug(&connection, "current")?.status,
            WorkItemStatus::Running
        );
        assert!(list_decisions(&connection, None, 10)?.is_empty());
        assert_eq!(list_work_items(&connection, None)?.len(), 1);

        Ok(())
    }

    #[test]
    fn managed_artifact_record_path_keeps_artifacts_inside_root() -> anyhow::Result<()> {
        let (temp, connection) = temp_store()?;
        create_work_item(
            &connection,
            None,
            "artifact-work",
            "Artifacts",
            "Record artifacts",
        )?;
        let run = start_run(&connection, "artifact-work", Some("test"))?;
        let artifact_root = temp.path().join("artifacts");
        fs::create_dir_all(&artifact_root)?;

        let internal_path = artifact_root.join("report.md");
        fs::write(&internal_path, "inside")?;
        let internal = add_artifact(
            &connection,
            &artifact_root,
            run.id,
            ArtifactKind::Report,
            &internal_path,
            "internal",
        )?;
        assert_eq!(internal.path, PathBuf::from("report.md"));

        // `?` is illegal in a Windows filename, so the on-disk fixture uses a
        // portable name. Sanitizing genuinely illegal characters is covered by
        // the unit tests for `sanitize_artifact_file_name`, which need no file.
        let external_path = temp.path().join("external report.md");
        fs::write(&external_path, "outside")?;
        let external = add_artifact(
            &connection,
            &artifact_root,
            run.id,
            ArtifactKind::Report,
            &external_path,
            "external",
        )?;

        assert!(external.path.starts_with("submitted"));
        assert!(external.path.to_string_lossy().contains("submitted"));
        assert!(external
            .path
            .to_string_lossy()
            .ends_with("external_report.md"));
        assert_eq!(
            fs::read_to_string(artifact_root.join(external.path))?,
            "outside"
        );

        Ok(())
    }

    struct TelemetryEnvGuard {
        home: Option<OsString>,
        userprofile: Option<OsString>,
        kill_switch: Option<OsString>,
    }

    impl TelemetryEnvGuard {
        fn install(home: &Path) -> Self {
            let guard = Self {
                home: std::env::var_os("HOME"),
                userprofile: std::env::var_os("USERPROFILE"),
                kill_switch: std::env::var_os("LDGR_TELEMETRY"),
            };
            std::env::set_var("HOME", home);
            std::env::remove_var("USERPROFILE");
            std::env::remove_var("LDGR_TELEMETRY");
            guard
        }
    }

    impl Drop for TelemetryEnvGuard {
        fn drop(&mut self) {
            restore_env("HOME", &self.home);
            restore_env("USERPROFILE", &self.userprofile);
            restore_env("LDGR_TELEMETRY", &self.kill_switch);
        }
    }

    fn restore_env(name: &str, value: &Option<OsString>) {
        match value {
            Some(value) => std::env::set_var(name, value),
            None => std::env::remove_var(name),
        }
    }

    fn enable_core_telemetry(home: &Path) -> anyhow::Result<()> {
        crate::telemetry::save_telemetry_consent(
            &home.join(".ldgr"),
            &crate::telemetry::TelemetryConsent::current(
                crate::telemetry::TelemetryConsentDecision::Enabled,
            ),
        )?;
        Ok(())
    }

    fn core_telemetry_sequences(
        home: &Path,
    ) -> anyhow::Result<Vec<Vec<crate::telemetry::transition::StateCode>>> {
        let route = home.join(".ldgr/telemetry-pending/core-work/v1");
        if !route.exists() {
            return Ok(Vec::new());
        }
        let mut files = fs::read_dir(route)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?;
        files.sort();
        files
            .into_iter()
            .map(|path| {
                let payload = fs::read(path)?;
                Ok(serde_json::from_slice(&payload)?)
            })
            .collect()
    }

    #[test]
    fn core_work_telemetry_maps_completed_result_terminals_without_content() -> anyhow::Result<()> {
        let _lock = crate::telemetry::telemetry_environment_lock()
            .lock()
            .expect("telemetry environment lock poisoned");
        let home = TempDir::new()?;
        let _env = TelemetryEnvGuard::install(home.path());
        enable_core_telemetry(home.path())?;

        let (_temp, connection) = temp_store()?;

        create_work_item(&connection, None, "positive", "Positive", "Positive work")?;
        let positive_run = start_run(&connection, "positive", Some("secret positive command"))?;
        close_run(
            &connection,
            positive_run.id,
            RunStatus::Success,
            Some("secret positive notes"),
            DecisionOutcome::Continue,
            "secret positive rationale",
            Some(NextWorkSpec {
                slug: "after-positive",
                title: Some("After positive"),
                description: Some("Queued follow-up"),
            }),
        )?;

        create_work_item(&connection, None, "negative", "Negative", "Negative work")?;
        let negative_run = start_run(&connection, "negative", Some("secret negative command"))?;
        add_validation_record(
            &connection,
            negative_run.id,
            ValidationOutcome::Fail,
            Some("secret failing validation command"),
            Some("secret failing validation rationale"),
        )?;
        close_run(
            &connection,
            negative_run.id,
            RunStatus::Success,
            None,
            DecisionOutcome::Continue,
            "secret negative rationale",
            Some(NextWorkSpec {
                slug: "after-negative",
                title: Some("After negative"),
                description: Some("Queued follow-up"),
            }),
        )?;

        create_work_item(
            &connection,
            None,
            "inconclusive",
            "Inconclusive",
            "Inconclusive work",
        )?;
        let inconclusive_run = start_run(&connection, "inconclusive", Some("secret command"))?;
        close_run(
            &connection,
            inconclusive_run.id,
            RunStatus::Success,
            None,
            DecisionOutcome::Inconclusive,
            "secret inconclusive rationale",
            Some(NextWorkSpec {
                slug: "after-inconclusive",
                title: Some("After inconclusive"),
                description: Some("Queued follow-up"),
            }),
        )?;

        let mut sequences = core_telemetry_sequences(home.path())?;
        sequences.sort();
        assert_eq!(sequences, vec![vec![0, 1, 3], vec![0, 1, 4], vec![0, 1, 5]]);
        for path in fs::read_dir(home.path().join(".ldgr/telemetry-pending/core-work/v1"))? {
            let payload = fs::read_to_string(path?.path())?;
            assert!(!payload.contains("secret"));
            assert!(payload.starts_with('[') && payload.ends_with(']'));
        }
        Ok(())
    }

    #[test]
    fn core_work_telemetry_maps_failure_cancellation_and_hold_resume_sequences(
    ) -> anyhow::Result<()> {
        let _lock = crate::telemetry::telemetry_environment_lock()
            .lock()
            .expect("telemetry environment lock poisoned");
        let home = TempDir::new()?;
        let _env = TelemetryEnvGuard::install(home.path());
        enable_core_telemetry(home.path())?;

        let (_temp, connection) = temp_store()?;

        create_work_item(&connection, None, "failure", "Failure", "Failure work")?;
        let failed_run = start_run(&connection, "failure", Some("secret failed command"))?;
        finish_run(
            &connection,
            failed_run.id,
            RunStatus::Failed,
            Some("secret operational failure notes"),
        )?;

        create_work_item(&connection, None, "cancel", "Cancel", "Cancel work")?;
        let _cancel_run = start_run(&connection, "cancel", Some("secret cancel command"))?;
        cancel_work_item(&connection, "cancel", Some("secret cancellation reason"))?;

        create_work_item(&connection, None, "held", "Held", "Held work")?;
        let first_run = start_run(&connection, "held", Some("secret first command"))?;
        assert_eq!(first_run.status, RunStatus::Running);
        hold_work_item(&connection, "held", Some("secret hold reason"))?;
        resume_work_item(&connection, "held", Some("secret resume reason"))?;
        let second_run = start_run(&connection, "held", Some("secret second command"))?;
        close_run(
            &connection,
            second_run.id,
            RunStatus::Success,
            None,
            DecisionOutcome::Continue,
            "secret held completion rationale",
            Some(NextWorkSpec {
                slug: "after-held",
                title: Some("After held"),
                description: Some("Queued follow-up"),
            }),
        )?;

        let mut sequences = core_telemetry_sequences(home.path())?;
        sequences.sort();
        assert_eq!(
            sequences,
            vec![vec![0, 1, 2, 1, 3], vec![0, 1, 6], vec![0, 1, 7]]
        );
        Ok(())
    }

    #[test]
    fn core_work_telemetry_respects_consent_off_and_failed_commits() -> anyhow::Result<()> {
        let _lock = crate::telemetry::telemetry_environment_lock()
            .lock()
            .expect("telemetry environment lock poisoned");
        let home = TempDir::new()?;
        let _env = TelemetryEnvGuard::install(home.path());

        let (_temp, connection) = temp_store()?;
        create_work_item(&connection, None, "off", "Off", "Consent off work")?;
        let off_run = start_run(&connection, "off", Some("command"))?;
        close_run(
            &connection,
            off_run.id,
            RunStatus::Success,
            None,
            DecisionOutcome::Stop,
            "done with consent off",
            None,
        )?;
        assert!(core_telemetry_sequences(home.path())?.is_empty());

        enable_core_telemetry(home.path())?;
        create_work_item(&connection, None, "rollback", "Rollback", "Rollback work")?;
        let rollback_run = start_run(&connection, "rollback", Some("command"))?;
        let error = close_run(
            &connection,
            rollback_run.id,
            RunStatus::Success,
            Some("should not commit"),
            DecisionOutcome::Continue,
            "missing next work should roll back",
            None,
        )
        .unwrap_err();
        assert!(error
            .to_string()
            .contains("continuing requires a next work item"));
        assert!(core_telemetry_sequences(home.path())?.is_empty());
        Ok(())
    }
}
