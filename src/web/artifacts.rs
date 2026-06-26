fn serve_artifact(
    stream: &mut TcpStream,
    db_path: &Path,
    artifact_root: &Path,
    path: &str,
) -> anyhow::Result<()> {
    let suffix = path.trim_start_matches("/api/artifacts/");
    let (id_text, raw) = suffix
        .strip_suffix("/raw")
        .map(|id| (id, true))
        .unwrap_or((suffix, false));
    let artifact_id: i64 = id_text.parse().context("artifact id must be an integer")?;
    let connection = open_store(db_path)?;
    let artifact = get_artifact(&connection, artifact_id)?;
    let artifact_path = checked_artifact_path(artifact_root, &artifact.path)?;

    if raw {
        let bytes = fs::read(&artifact_path)
            .with_context(|| format!("failed to read artifact {}", artifact.path.display()))?;
        return write_response(
            stream,
            "200 OK",
            content_type_for_path(&artifact_path),
            &bytes,
        );
    }

    let content = if matches!(artifact.kind, ArtifactKind::Image) {
        String::new()
    } else {
        fs::read_to_string(&artifact_path).unwrap_or_else(|_| String::new())
    };
    let viewer = match &artifact.kind {
        ArtifactKind::Json => "json",
        ArtifactKind::Csv => "csv",
        ArtifactKind::Report => "markdown",
        ArtifactKind::Image => "image",
        ArtifactKind::Other => viewer_for_artifact_path(&artifact.path),
        ArtifactKind::Custom(kind) => viewer_for_artifact_kind_or_path(kind, &artifact.path),
    };
    let body = serde_json::to_vec_pretty(&json!({
        "artifact": artifact,
        "viewer": viewer,
        "content": content,
        "raw_url": format!("/api/artifacts/{artifact_id}/raw"),
    }))?;
    write_response(stream, "200 OK", "application/json; charset=utf-8", &body)
}

fn checked_artifact_path(artifact_root: &Path, artifact_path: &Path) -> anyhow::Result<PathBuf> {
    let root = artifact_root.canonicalize().with_context(|| {
        format!(
            "failed to resolve artifact root {}",
            artifact_root.display()
        )
    })?;
    if artifact_path.is_absolute() {
        bail!("artifact records must be relative to the artifact root");
    }
    let candidate = root.join(artifact_path);
    let resolved = candidate
        .canonicalize()
        .with_context(|| format!("failed to resolve artifact {}", artifact_path.display()))?;
    if !resolved.starts_with(&root) {
        bail!(
            "artifact path escapes artifact root: {}",
            artifact_path.display()
        );
    }
    Ok(resolved)
}

fn write_json<T: serde::Serialize>(stream: &mut TcpStream, value: &T) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(value)?;
    write_response(stream, "200 OK", "application/json; charset=utf-8", &body)
}

fn write_api_error(stream: &mut TcpStream, error: WebApiError) -> anyhow::Result<()> {
    let body = serde_json::to_vec_pretty(&json!({
        "error": {
            "code": error.code,
            "message": error.message,
        }
    }))?;
    write_response(
        stream,
        error.status,
        "application/json; charset=utf-8",
        &body,
    )
}

fn write_response(
    stream: &mut TcpStream,
    status: &str,
    content_type: &str,
    body: &[u8],
) -> anyhow::Result<()> {
    write!(
        stream,
        "HTTP/1.1 {status}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Connection: close\r\n\
         X-Content-Type-Options: nosniff\r\n\
         X-Frame-Options: DENY\r\n\
         Referrer-Policy: no-referrer\r\n\
         Content-Security-Policy: default-src 'self'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'\r\n\
         \r\n",
        body.len()
    )
    .context("failed to write HTTP headers")?;
    stream
        .write_all(body)
        .context("failed to write HTTP body")?;
    Ok(())
}

fn viewer_for_artifact_kind_or_path(kind: &str, path: &Path) -> &'static str {
    match kind.to_ascii_lowercase().as_str() {
        "json" => "json",
        "csv" => "csv",
        "report" | "markdown" | "md" | "text" | "txt" | "log" | "patch" | "diff" | "toml"
        | "yaml" | "yml" => "markdown",
        "image" | "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
        _ => viewer_for_artifact_path(path),
    }
}

fn viewer_for_artifact_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "json" => "json",
        "csv" => "csv",
        "md" | "markdown" | "txt" | "log" | "patch" | "diff" | "toml" | "yaml" | "yml" => {
            "markdown"
        }
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => "image",
        _ => "metadata",
    }
}

fn content_type_for_path(path: &Path) -> &'static str {
    match path
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "css" => "text/css; charset=utf-8",
        "csv" => "text/csv; charset=utf-8",
        "gif" => "image/gif",
        "htm" | "html" => "text/html; charset=utf-8",
        "jpg" | "jpeg" => "image/jpeg",
        "json" => "application/json; charset=utf-8",
        "md" | "markdown" | "txt" => "text/plain; charset=utf-8",
        "png" => "image/png",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        _ => "application/octet-stream",
    }
}

