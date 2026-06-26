#[cfg(test)]
mod tests {
    use super::*;
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

        let external_path = temp.path().join("external report?.md");
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
            .ends_with("external_report_.md"));
        assert_eq!(
            fs::read_to_string(artifact_root.join(external.path))?,
            "outside"
        );

        Ok(())
    }
}
