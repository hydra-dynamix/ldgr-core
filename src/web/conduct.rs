fn serve_conduct_waves(stream: &mut TcpStream, db_path: &Path) -> anyhow::Result<()> {
    let root = std::env::current_dir()?.join(".ldgr/.conduct");
    if !root.is_dir() {
        return write_json(
            stream,
            &serde_json::json!({
                "available": false,
                "message": ".ldgr/.conduct directory not found",
                "batches": [],
                "feeds": []
            }),
        );
    }
    let worker_root = root.join("workers");
    let worktree_root = root.join("worktrees");
    let mut batches = Vec::new();
    for batch_entry in read_dir_sorted(&worker_root)? {
        let batch_path = batch_entry.path();
        if !batch_path.is_dir() {
            continue;
        }
        let batch_id = batch_entry.file_name().to_string_lossy().to_string();
        let mut workers = Vec::new();
        for worker_entry in read_dir_sorted(&batch_path)? {
            if !worker_entry.path().is_dir() {
                continue;
            }
            let worker_id = worker_entry.file_name().to_string_lossy().to_string();
            workers.push(conduct_worker_summary(
                db_path,
                &worktree_root,
                &batch_id,
                &worker_id,
                &worker_entry.path(),
            ));
        }
        let modified_sort = modified_sort_key(&batch_path);
        batches.push((
            modified_sort,
            serde_json::json!({
                "batch_id": batch_id,
                "workers": workers,
                "worker_count": workers.len(),
                "modified_sort": modified_sort,
            }),
        ));
    }
    batches.sort_by(|left, right| right.0.cmp(&left.0));
    let batches = batches
        .into_iter()
        .take(8)
        .map(|(_, batch)| batch)
        .collect::<Vec<_>>();
    let feeds = collect_conduct_feeds(&root, 16)?;
    write_json(
        stream,
        &serde_json::json!({
            "available": true,
            "batches": batches,
            "feeds": feeds,
        }),
    )
}

fn conduct_worker_summary(
    parent_db_path: &Path,
    worktree_root: &Path,
    batch_id: &str,
    worker_id: &str,
    worker_dir: &Path,
) -> serde_json::Value {
    let worker_db = worker_dir.join("ldgr.db");
    let artifact_root = worker_dir.join("artifacts");
    let worktree = find_worker_worktree(worktree_root, batch_id, worker_id);
    let ticket_slug = worktree
        .as_ref()
        .and_then(|path| path.file_name())
        .map(|name| name.to_string_lossy().to_string())
        .and_then(|name| {
            name.strip_prefix(&format!("{worker_id}-"))
                .map(str::to_string)
        })
        .unwrap_or_else(|| "unknown".to_string());
    let db_summary = worker_db_summary(&worker_db);
    let git_status = serde_json::json!({
        "available": false,
        "summary": "git status skipped in cockpit summary; use worker worktree for full inspection",
    });
    let feeds = collect_worker_feeds_fast(&artifact_root, 4).unwrap_or_default();
    let parent_seen = parent_db_path.exists();
    serde_json::json!({
        "worker_id": worker_id,
        "ticket_slug": ticket_slug,
        "worker_db": worker_db.display().to_string(),
        "artifact_root": artifact_root.display().to_string(),
        "worktree": worktree.map(|path| path.display().to_string()),
        "worker_ldgr": db_summary,
        "git": git_status,
        "feeds": feeds,
        "parent_db_available": parent_seen,
    })
}

fn worker_db_summary(worker_db: &Path) -> serde_json::Value {
    if !worker_db.is_file() {
        return serde_json::json!({"readable": false, "error": "worker DB not found"});
    }
    match open_store(worker_db).and_then(|connection| {
        let active_runs = list_runs(&connection, Some(crate::store::RunStatus::Running))?;
        let runs = list_runs(&connection, None)?;
        Ok((active_runs, runs))
    }) {
        Ok((active_runs, runs)) => {
            let latest = runs.last();
            serde_json::json!({
                "readable": true,
                "phase": if active_runs.is_empty() { "terminal" } else { "started" },
                "run_id": latest.map(|run| run.run_id),
                "work_slug": latest.map(|run| run.work_slug.clone()),
                "terminal_status": latest.and_then(|run| active_runs.is_empty().then(|| run.status.as_str().to_string())),
                "needs_decision": active_runs.is_empty() && latest.is_some(),
                "active_run_count": active_runs.len(),
                "latest_observation": null,
                "latest_decision": null,
            })
        }
        Err(error) => serde_json::json!({"readable": false, "error": format!("{error:#}")}),
    }
}

fn modified_sort_key(path: &Path) -> u64 {
    fs::metadata(path)
        .and_then(|metadata| metadata.modified())
        .ok()
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .map(|duration| duration.as_secs())
        .unwrap_or_default()
}

fn read_dir_sorted(path: &Path) -> anyhow::Result<Vec<std::fs::DirEntry>> {
    if !path.is_dir() {
        return Ok(Vec::new());
    }
    let mut entries = fs::read_dir(path)?.collect::<Result<Vec<_>, _>>()?;
    entries.sort_by_key(|entry| entry.file_name());
    Ok(entries)
}

fn find_worker_worktree(worktree_root: &Path, batch_id: &str, worker_id: &str) -> Option<PathBuf> {
    let batch_root = worktree_root.join(batch_id);
    read_dir_sorted(&batch_root)
        .ok()?
        .into_iter()
        .find_map(|entry| {
            let name = entry.file_name().to_string_lossy().to_string();
            (entry.path().is_dir() && name.starts_with(&format!("{worker_id}-")))
                .then(|| entry.path())
        })
}

fn collect_conduct_feeds(root: &Path, limit: usize) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut candidates = Vec::new();
    collect_feed_candidates(&root.join("logs"), &mut candidates)?;
    candidates.sort_by(|left, right| right.modified.cmp(&left.modified));
    Ok(candidates
        .into_iter()
        .take(limit)
        .map(feed_json)
        .collect::<Vec<_>>())
}

fn collect_worker_feeds_fast(root: &Path, limit: usize) -> anyhow::Result<Vec<serde_json::Value>> {
    let mut candidates = Vec::new();
    collect_shallow_feed_candidates(&root.join("process"), &mut candidates, 2)?;
    collect_shallow_feed_candidates(&root.join("agent-output"), &mut candidates, 2)?;
    candidates.sort_by(|left, right| right.modified.cmp(&left.modified));
    Ok(candidates
        .into_iter()
        .filter(|candidate| {
            candidate
                .path
                .file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.ends_with(".log")
                        || matches!(
                            name,
                            "stdout.txt"
                                | "stderr.txt"
                                | "transcript.md"
                                | "diagnostics.jsonl"
                                | "result.toml"
                        )
                })
        })
        .take(limit)
        .map(feed_json)
        .collect::<Vec<_>>())
}

fn collect_shallow_feed_candidates(
    path: &Path,
    candidates: &mut Vec<FeedCandidate>,
    depth: usize,
) -> anyhow::Result<()> {
    if depth == 0 || !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_shallow_feed_candidates(&path, candidates, depth - 1)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.ends_with(".log")
            || matches!(
                name,
                "stdout.txt" | "stderr.txt" | "transcript.md" | "diagnostics.jsonl" | "result.toml"
            ))
        {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        candidates.push(FeedCandidate {
            path,
            modified: metadata.modified().ok(),
            size: metadata.len(),
        });
    }
    Ok(())
}

#[derive(Debug)]
struct FeedCandidate {
    path: PathBuf,
    modified: Option<SystemTime>,
    size: u64,
}

fn collect_feed_candidates(path: &Path, candidates: &mut Vec<FeedCandidate>) -> anyhow::Result<()> {
    if !path.is_dir() {
        return Ok(());
    }
    for entry in fs::read_dir(path)? {
        let entry = entry?;
        let path = entry.path();
        if path.is_dir() {
            collect_feed_candidates(&path, candidates)?;
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        if !(name.ends_with(".log")
            || matches!(
                name,
                "stdout.txt" | "stderr.txt" | "transcript.md" | "diagnostics.jsonl" | "result.toml"
            ))
        {
            continue;
        }
        let metadata = fs::metadata(&path)?;
        candidates.push(FeedCandidate {
            path,
            modified: metadata.modified().ok(),
            size: metadata.len(),
        });
    }
    Ok(())
}

fn feed_json(candidate: FeedCandidate) -> serde_json::Value {
    serde_json::json!({
        "path": candidate.path.display().to_string(),
        "label": candidate.path.file_name().and_then(|name| name.to_str()).unwrap_or("feed"),
        "size": candidate.size,
        "modified_sort": candidate.modified.and_then(|time| time.duration_since(SystemTime::UNIX_EPOCH).ok()).map(|duration| duration.as_secs()),
        "tail": tail_file(&candidate.path, 12000).unwrap_or_else(|error| format!("failed to read feed: {error:#}")),
    })
}

fn tail_file(path: &Path, max_bytes: usize) -> anyhow::Result<String> {
    let mut file = fs::File::open(path)?;
    let len = file.metadata()?.len();
    let start = len.saturating_sub(max_bytes as u64);
    file.seek(SeekFrom::Start(start))?;
    let mut bytes = Vec::with_capacity((len - start).min(max_bytes as u64) as usize);
    file.take(max_bytes as u64).read_to_end(&mut bytes)?;
    Ok(String::from_utf8_lossy(&bytes).to_string())
}

