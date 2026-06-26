pub fn add_artifact(
    connection: &Connection,
    artifact_root: &Path,
    run_id: i64,
    kind: ArtifactKind,
    path: &Path,
    description: &str,
) -> anyhow::Result<Artifact> {
    ensure_run_exists(connection, run_id)?;
    let managed_path = managed_artifact_record_path(artifact_root, run_id, path)?;
    in_write_transaction(connection, |connection| {
        connection
            .execute(
                "INSERT INTO artifact (run_id, kind, path, description)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    run_id,
                    kind.as_str(),
                    managed_path.display().to_string(),
                    description
                ],
            )
            .with_context(|| format!("failed to add artifact to run {run_id}"))?;
        let artifact_id = connection.last_insert_rowid();
        record_event(connection, "artifact", artifact_id, "add", "{}")?;
        get_artifact_by_id(connection, artifact_id)
    })
}

fn managed_artifact_record_path(
    artifact_root: &Path,
    run_id: i64,
    submitted_path: &Path,
) -> anyhow::Result<PathBuf> {
    fs::create_dir_all(artifact_root).with_context(|| {
        format!(
            "failed to create artifact root directory {}",
            artifact_root.display()
        )
    })?;
    let root = artifact_root.canonicalize().with_context(|| {
        format!(
            "failed to resolve artifact root {}",
            artifact_root.display()
        )
    })?;
    let submitted_resolved = resolve_submitted_artifact_path(&root, submitted_path)?;
    if submitted_resolved.starts_with(&root) {
        return submitted_resolved
            .strip_prefix(&root)
            .map(PathBuf::from)
            .with_context(|| {
                format!(
                    "failed to normalize artifact path {} against {}",
                    submitted_path.display(),
                    artifact_root.display()
                )
            });
    }

    let submitted_dir = root.join("submitted");
    fs::create_dir_all(&submitted_dir).with_context(|| {
        format!(
            "failed to create submitted artifact directory {}",
            submitted_dir.display()
        )
    })?;
    let file_name = submitted_path
        .file_name()
        .and_then(|value| value.to_str())
        .map(sanitize_artifact_file_name)
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "artifact".to_owned());
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_nanos();
    let managed_path = submitted_dir.join(format!("run-{run_id}-{timestamp}-{file_name}"));
    fs::copy(&submitted_resolved, &managed_path).with_context(|| {
        format!(
            "failed to copy artifact {} to {}",
            submitted_path.display(),
            managed_path.display()
        )
    })?;
    managed_path
        .strip_prefix(&root)
        .map(PathBuf::from)
        .with_context(|| {
            format!(
                "failed to normalize managed artifact path {} against {}",
                managed_path.display(),
                artifact_root.display()
            )
        })
}

fn resolve_submitted_artifact_path(root: &Path, submitted_path: &Path) -> anyhow::Result<PathBuf> {
    if submitted_path.is_absolute() {
        return submitted_path
            .canonicalize()
            .with_context(|| format!("failed to resolve artifact {}", submitted_path.display()));
    }

    let cwd_candidate = std::env::current_dir()
        .context("failed to read current directory")?
        .join(submitted_path);
    if cwd_candidate.exists() {
        return cwd_candidate
            .canonicalize()
            .with_context(|| format!("failed to resolve artifact {}", submitted_path.display()));
    }

    root.join(submitted_path)
        .canonicalize()
        .with_context(|| format!("failed to resolve artifact {}", submitted_path.display()))
}

fn sanitize_artifact_file_name(value: &str) -> String {
    value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_' | '+') {
                character
            } else {
                '_'
            }
        })
        .collect()
}

