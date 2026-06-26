fn handle_connection(
    mut stream: TcpStream,
    db_path: &Path,
    artifact_root: &Path,
    options: &WebOptions,
) -> anyhow::Result<()> {
    let request = match read_http_request(&mut stream) {
        Ok(request) => request,
        Err(error) => {
            return write_response(
                &mut stream,
                error.status,
                "text/plain; charset=utf-8",
                error.message.as_bytes(),
            );
        }
    };

    let result = if request.method == "POST" {
        handle_post(&mut stream, db_path, artifact_root, &request, options)
    } else if request.method == "GET" {
        handle_get(&mut stream, db_path, artifact_root, &request.path)
    } else if request.path.starts_with("/api/") {
        write_api_error(
            &mut stream,
            WebApiError::new(
                "405 Method Not Allowed",
                "method_not_allowed",
                "method not allowed",
            ),
        )
    } else {
        write_response(
            &mut stream,
            "405 Method Not Allowed",
            "text/plain; charset=utf-8",
            b"method not allowed",
        )
    };

    if let Err(error) = result {
        if request.path.starts_with("/api/") {
            write_api_error(&mut stream, WebApiError::from_error(error))?;
        } else {
            write_response(
                &mut stream,
                "400 Bad Request",
                "text/plain; charset=utf-8",
                format!("{error:#}").as_bytes(),
            )?;
        }
    }
    Ok(())
}

fn read_http_request(stream: &mut TcpStream) -> Result<HttpRequest, HttpError> {
    stream
        .set_read_timeout(Some(READ_TIMEOUT))
        .map_err(|error| {
            HttpError::new(
                "400 Bad Request",
                format!("failed to set read timeout: {error}"),
            )
        })?;
    let mut bytes = Vec::new();
    let header_end = loop {
        if let Some(index) = find_header_end(&bytes) {
            break index;
        }
        if bytes.len() >= MAX_HEADER_BYTES {
            return Err(HttpError::new(
                "431 Request Header Fields Too Large",
                "request headers are too large",
            ));
        }
        let mut chunk = [0_u8; 1024];
        let count = stream.read(&mut chunk).map_err(|error| {
            HttpError::new(
                "400 Bad Request",
                format!("failed to read HTTP request: {error}"),
            )
        })?;
        if count == 0 {
            return Err(HttpError::new(
                "400 Bad Request",
                "request ended before headers were complete",
            ));
        }
        bytes.extend_from_slice(&chunk[..count]);
    };

    let header_bytes = &bytes[..header_end];
    let header_text = std::str::from_utf8(header_bytes)
        .map_err(|_| HttpError::new("400 Bad Request", "request headers are not valid UTF-8"))?;
    let mut lines = header_text.split("\r\n");
    let request_line = lines
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP request line"))?;
    let mut parts = request_line.split_whitespace();
    let method = parts
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP method"))?
        .to_string();
    let target = parts
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP target"))?
        .to_string();
    let version = parts
        .next()
        .ok_or_else(|| HttpError::new("400 Bad Request", "missing HTTP version"))?;
    if parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return Err(HttpError::new(
            "400 Bad Request",
            "invalid HTTP request line",
        ));
    }
    if !target.starts_with('/') {
        return Err(HttpError::new(
            "400 Bad Request",
            "HTTP target must be an absolute path",
        ));
    }
    let path = target.split('?').next().unwrap_or("/").to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            continue;
        }
        let (name, value) = line
            .split_once(':')
            .ok_or_else(|| HttpError::new("400 Bad Request", "invalid HTTP header"))?;
        headers.insert(name.trim().to_ascii_lowercase(), value.trim().to_string());
    }

    let body_start = header_end + 4;
    let content_length = match headers.get("content-length") {
        Some(value) => value
            .parse::<usize>()
            .map_err(|_| HttpError::new("400 Bad Request", "Content-Length must be an integer"))?,
        None => 0,
    };
    if content_length > MAX_BODY_BYTES {
        return Err(HttpError::new(
            "413 Payload Too Large",
            "request body is too large",
        ));
    }
    if method == "POST" && !headers.contains_key("content-length") {
        return Err(HttpError::new(
            "411 Length Required",
            "POST requests must include Content-Length",
        ));
    }

    let mut body = bytes.get(body_start..).unwrap_or_default().to_vec();
    while body.len() < content_length {
        let mut chunk = [0_u8; 4096];
        let count = stream.read(&mut chunk).map_err(|error| {
            HttpError::new(
                "400 Bad Request",
                format!("failed to read HTTP request body: {error}"),
            )
        })?;
        if count == 0 {
            return Err(HttpError::new(
                "400 Bad Request",
                "request body ended before Content-Length bytes were received",
            ));
        }
        body.extend_from_slice(&chunk[..count]);
        if body.len() > MAX_BODY_BYTES {
            return Err(HttpError::new(
                "413 Payload Too Large",
                "request body is too large",
            ));
        }
    }
    body.truncate(content_length);

    Ok(HttpRequest {
        method,
        path,
        headers,
        body,
    })
}

fn find_header_end(bytes: &[u8]) -> Option<usize> {
    bytes.windows(4).position(|window| window == b"\r\n\r\n")
}

