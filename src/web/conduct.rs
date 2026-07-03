fn serve_conduct_waves(stream: &mut TcpStream, db_path: &Path, artifact_root: &Path) -> anyhow::Result<()> {
    match crate::conduct_status::read_conduct_batch_status_json(db_path, artifact_root, None) {
        Ok(Some(status)) => write_json(stream, &conduct_waves_from_adapter_status(&status)),
        Ok(None) => write_json(
            stream,
            &serde_json::json!({
                "available": false,
                "message": "conduct adapter status API is not installed",
                "batches": [],
                "feeds": []
            }),
        ),
        Err(error) => write_json(
            stream,
            &serde_json::json!({
                "available": false,
                "message": format!("conduct adapter status API failed: {error:#}"),
                "batches": [],
                "feeds": []
            }),
        ),
    }
}

fn conduct_waves_from_adapter_status(status: &serde_json::Value) -> serde_json::Value {
    let Some(state) = status.get("state").and_then(|state| state.as_object()) else {
        return serde_json::json!({
            "available": true,
            "message": "conduct adapter status API returned no batch state",
            "batches": [],
            "feeds": []
        });
    };
    let batch_id = state
        .get("batch_id")
        .and_then(|value| value.as_str())
        .unwrap_or("unknown");
    let workers = state
        .get("workers")
        .and_then(|value| value.as_array())
        .map(|workers| {
            workers
                .iter()
                .map(conduct_worker_from_adapter_status)
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();
    serde_json::json!({
        "available": true,
        "source": "adapter_status_api",
        "batches": [{
            "batch_id": batch_id,
            "status": state.get("status").cloned().unwrap_or(serde_json::Value::Null),
            "current_wave": state.get("current_wave").cloned().unwrap_or(serde_json::Value::Null),
            "workers": workers,
            "worker_count": workers.len(),
            "worker_status": status.get("worker_status").cloned().unwrap_or(serde_json::Value::Null),
        }],
        "feeds": []
    })
}

fn conduct_worker_from_adapter_status(worker: &serde_json::Value) -> serde_json::Value {
    serde_json::json!({
        "worker_id": worker.get("worker_id").cloned().unwrap_or(serde_json::Value::Null),
        "ticket_slug": worker.get("ticket_id").cloned().unwrap_or(serde_json::Value::Null),
        "status": worker.get("status").cloned().unwrap_or(serde_json::Value::Null),
        "worker_db": worker.get("worker_db_path").cloned().unwrap_or(serde_json::Value::Null),
        "artifact_root": worker.get("worker_artifact_root").cloned().unwrap_or(serde_json::Value::Null),
        "worktree": worker.get("worktree_path").cloned().unwrap_or(serde_json::Value::Null),
        "summary": worker.get("summary").cloned().unwrap_or(serde_json::Value::Null),
        "worker_ldgr": {
            "readable": false,
            "summary": "worker lifecycle is owned by ldgr-conduct; inspect via ldgr conduct batch status --json"
        },
        "git": {
            "available": false,
            "summary": "worktree status is owned by ldgr-conduct review/status surfaces"
        },
        "feeds": []
    })
}
