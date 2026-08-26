use std::fs::{self, File};
use std::io::{Read, Write};
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{ensure, Context};
use reqwest::blocking::{Client, Response};
use reqwest::header::{ETAG, IF_NONE_MATCH};
use reqwest::redirect::Policy;
use reqwest::{StatusCode, Url};

pub const MAX_UPDATE_CATALOG_BYTES: u64 = 1024 * 1024;
pub const MAX_UPDATE_SIGNATURE_BYTES: u64 = 16 * 1024;
pub const MAX_UPDATE_KEYRING_BYTES: u64 = 64 * 1024;
pub const MAX_UPDATE_ARTIFACT_BYTES: u64 = 512 * 1024 * 1024;
const MAX_REDIRECTS: usize = 10;
const COPY_BUFFER_BYTES: usize = 64 * 1024;

#[derive(Clone, Debug)]
pub struct UpdateClientConfig {
    pub offline: bool,
    pub connect_timeout: Duration,
    pub catalog_timeout: Duration,
    pub artifact_timeout: Duration,
    allow_loopback_http: bool,
}

impl Default for UpdateClientConfig {
    fn default() -> Self {
        Self {
            offline: false,
            connect_timeout: Duration::from_secs(2),
            catalog_timeout: Duration::from_secs(5),
            artifact_timeout: Duration::from_secs(120),
            allow_loopback_http: false,
        }
    }
}

#[derive(Debug)]
pub struct UpdateNetworkClient {
    client: Client,
    config: UpdateClientConfig,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CatalogFetch {
    Modified {
        bytes: Vec<u8>,
        etag: Option<String>,
    },
    NotModified {
        etag: Option<String>,
    },
}

impl UpdateNetworkClient {
    pub fn new(offline: bool) -> anyhow::Result<Self> {
        Self::with_config(UpdateClientConfig {
            offline,
            ..UpdateClientConfig::default()
        })
    }

    pub fn with_config(config: UpdateClientConfig) -> anyhow::Result<Self> {
        ensure!(
            !config.connect_timeout.is_zero(),
            "update connect timeout must be greater than zero"
        );
        ensure!(
            !config.catalog_timeout.is_zero(),
            "update catalog timeout must be greater than zero"
        );
        ensure!(
            !config.artifact_timeout.is_zero(),
            "update artifact timeout must be greater than zero"
        );
        let redirect = Policy::custom(|attempt| {
            let target = attempt.url();
            if attempt.previous().len() >= MAX_REDIRECTS {
                attempt.error("update redirect limit exceeded")
            } else if target.scheme() != "https" {
                attempt.error("update redirect target must use HTTPS")
            } else if !target.username().is_empty() || target.password().is_some() {
                attempt.error("update redirect target must not contain credentials")
            } else {
                attempt.follow()
            }
        });
        let client = Client::builder()
            .use_rustls_tls()
            .https_only(!config.allow_loopback_http)
            .connect_timeout(config.connect_timeout)
            .redirect(redirect)
            .referer(false)
            .no_proxy()
            .user_agent(update_user_agent())
            .build()
            .context("failed to build bounded update network client")?;
        Ok(Self { client, config })
    }

    pub fn fetch_catalog(
        &self,
        source: &str,
        previous_etag: Option<&str>,
    ) -> anyhow::Result<CatalogFetch> {
        match self.classify_source(source)? {
            UpdateSource::Local(path) => Ok(CatalogFetch::Modified {
                bytes: read_local_bounded(&path, MAX_UPDATE_CATALOG_BYTES, "update catalog")?,
                etag: None,
            }),
            UpdateSource::Remote(url) => {
                let mut request = self.client.get(url).timeout(self.config.catalog_timeout);
                if let Some(etag) = previous_etag {
                    request = request.header(IF_NONE_MATCH, etag);
                }
                let response = request.send().context("update catalog request failed")?;
                let response_etag = response_etag(&response)?.or_else(|| {
                    (response.status() == StatusCode::NOT_MODIFIED)
                        .then(|| previous_etag.map(str::to_owned))
                        .flatten()
                });
                if response.status() == StatusCode::NOT_MODIFIED {
                    return Ok(CatalogFetch::NotModified {
                        etag: response_etag,
                    });
                }
                let response = response
                    .error_for_status()
                    .context("update catalog server returned an error")?;
                Ok(CatalogFetch::Modified {
                    bytes: read_response_bounded(
                        response,
                        MAX_UPDATE_CATALOG_BYTES,
                        "update catalog",
                    )?,
                    etag: response_etag,
                })
            }
        }
    }

    pub fn fetch_bounded(
        &self,
        source: &str,
        maximum: u64,
        label: &str,
    ) -> anyhow::Result<Vec<u8>> {
        ensure!(maximum > 0, "{label} size limit must be greater than zero");
        match self.classify_source(source)? {
            UpdateSource::Local(path) => read_local_bounded(&path, maximum, label),
            UpdateSource::Remote(url) => {
                let response = self
                    .client
                    .get(url)
                    .timeout(self.config.catalog_timeout)
                    .send()
                    .with_context(|| format!("{label} request failed"))?
                    .error_for_status()
                    .with_context(|| format!("{label} server returned an error"))?;
                read_response_bounded(response, maximum, label)
            }
        }
    }

    pub fn download_artifact(
        &self,
        source: &str,
        destination: &Path,
        maximum: u64,
    ) -> anyhow::Result<u64> {
        ensure!(maximum > 0, "artifact size limit must be greater than zero");
        ensure!(
            !destination.exists(),
            "artifact destination already exists: {}",
            destination.display()
        );
        let parent = destination.parent().with_context(|| {
            format!(
                "artifact destination has no parent directory: {}",
                destination.display()
            )
        })?;
        ensure!(
            parent.is_dir(),
            "artifact destination parent is not a directory: {}",
            parent.display()
        );
        let mut temporary = tempfile::NamedTempFile::new_in(parent).with_context(|| {
            format!(
                "failed to create temporary artifact beside {}",
                destination.display()
            )
        })?;
        let copied = match self.classify_source(source)? {
            UpdateSource::Local(path) => {
                let metadata = regular_file_metadata(&path, "update artifact")?;
                ensure!(
                    metadata.len() <= maximum,
                    "update artifact exceeds the {maximum}-byte size limit"
                );
                let mut input = File::open(&path).with_context(|| {
                    format!("failed to open update artifact {}", path.display())
                })?;
                copy_bounded(
                    &mut input,
                    temporary.as_file_mut(),
                    maximum,
                    "update artifact",
                )?
            }
            UpdateSource::Remote(url) => {
                let mut response = self
                    .client
                    .get(url)
                    .timeout(self.config.artifact_timeout)
                    .send()
                    .context("update artifact request failed")?
                    .error_for_status()
                    .context("update artifact server returned an error")?;
                reject_oversized_content_length(&response, maximum, "update artifact")?;
                copy_bounded(
                    &mut response,
                    temporary.as_file_mut(),
                    maximum,
                    "update artifact",
                )?
            }
        };
        temporary
            .as_file_mut()
            .flush()
            .context("failed to flush downloaded update artifact")?;
        temporary
            .persist(destination)
            .map_err(|error| error.error)
            .with_context(|| {
                format!(
                    "failed to persist downloaded update artifact to {}",
                    destination.display()
                )
            })?;
        Ok(copied)
    }

    fn classify_source(&self, source: &str) -> anyhow::Result<UpdateSource> {
        if source.starts_with("file://") {
            return Ok(UpdateSource::Local(file_url_path(source)?));
        }
        if source.contains("://") {
            let url = Url::parse(source).context("update source is not a valid URL")?;
            let loopback_http =
                self.config.allow_loopback_http && url.scheme() == "http" && is_loopback_host(&url);
            ensure!(
                url.scheme() == "https" || loopback_http,
                "update network sources must use HTTPS"
            );
            ensure!(
                url.host_str().is_some(),
                "update source URL must include a host"
            );
            ensure!(
                url.username().is_empty() && url.password().is_none(),
                "update source URL must not contain credentials"
            );
            ensure!(
                url.fragment().is_none(),
                "update source URL must not contain a fragment"
            );
            ensure!(
                !self.config.offline,
                "offline update mode forbids network access"
            );
            return Ok(UpdateSource::Remote(url));
        }
        ensure!(
            !source.trim().is_empty(),
            "update local source path must not be empty"
        );
        Ok(UpdateSource::Local(PathBuf::from(source)))
    }
}

enum UpdateSource {
    Local(PathBuf),
    Remote(Url),
}

fn update_user_agent() -> String {
    format!(
        "ldgr/{} ({}-{})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH
    )
}

fn is_loopback_host(url: &Url) -> bool {
    url.host_str().is_some_and(|host| {
        host.eq_ignore_ascii_case("localhost")
            || host
                .parse::<IpAddr>()
                .map(|address| address.is_loopback())
                .unwrap_or(false)
    })
}

fn file_url_path(source: &str) -> anyhow::Result<PathBuf> {
    let raw = source
        .strip_prefix("file://")
        .expect("file source prefix was checked");
    ensure!(!raw.is_empty(), "update file URL must contain a path");
    if let Ok(url) = Url::parse(source) {
        if url.scheme() == "file" {
            if let Ok(path) = url.to_file_path() {
                return Ok(path);
            }
        }
    }
    Ok(PathBuf::from(raw))
}

fn regular_file_metadata(path: &Path, label: &str) -> anyhow::Result<fs::Metadata> {
    let metadata = fs::metadata(path)
        .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
    ensure!(metadata.is_file(), "{label} must be a regular file");
    Ok(metadata)
}

fn read_local_bounded(path: &Path, maximum: u64, label: &str) -> anyhow::Result<Vec<u8>> {
    let metadata = regular_file_metadata(path, label)?;
    ensure!(
        metadata.len() <= maximum,
        "{label} exceeds the {maximum}-byte size limit"
    );
    let mut file =
        File::open(path).with_context(|| format!("failed to open {label} {}", path.display()))?;
    let mut bytes = Vec::with_capacity(metadata.len().min(maximum) as usize);
    let mut bounded = Read::by_ref(&mut file).take(maximum.saturating_add(1));
    bounded
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label} {}", path.display()))?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "{label} exceeds the {maximum}-byte size limit"
    );
    Ok(bytes)
}

fn read_response_bounded(
    mut response: Response,
    maximum: u64,
    label: &str,
) -> anyhow::Result<Vec<u8>> {
    reject_oversized_content_length(&response, maximum, label)?;
    let mut bytes = Vec::new();
    let mut bounded = response.by_ref().take(maximum.saturating_add(1));
    bounded
        .read_to_end(&mut bytes)
        .with_context(|| format!("failed to read {label} response"))?;
    ensure!(
        bytes.len() as u64 <= maximum,
        "{label} exceeds the {maximum}-byte response size limit"
    );
    Ok(bytes)
}

fn copy_bounded(
    input: &mut dyn Read,
    output: &mut dyn Write,
    maximum: u64,
    label: &str,
) -> anyhow::Result<u64> {
    let mut buffer = [0_u8; COPY_BUFFER_BYTES];
    let mut copied = 0_u64;
    loop {
        let remaining = maximum.saturating_sub(copied);
        let requested = usize::try_from(remaining.saturating_add(1))
            .unwrap_or(usize::MAX)
            .min(buffer.len());
        let read = input
            .read(&mut buffer[..requested])
            .with_context(|| format!("failed while reading {label}"))?;
        if read == 0 {
            return Ok(copied);
        }
        ensure!(
            copied.saturating_add(read as u64) <= maximum,
            "{label} exceeds the {maximum}-byte size limit"
        );
        output
            .write_all(&buffer[..read])
            .with_context(|| format!("failed while writing {label}"))?;
        copied += read as u64;
    }
}

fn reject_oversized_content_length(
    response: &Response,
    maximum: u64,
    label: &str,
) -> anyhow::Result<()> {
    if let Some(length) = response.content_length() {
        ensure!(
            length <= maximum,
            "{label} exceeds the {maximum}-byte response size limit"
        );
    }
    Ok(())
}

fn response_etag(response: &Response) -> anyhow::Result<Option<String>> {
    response
        .headers()
        .get(ETAG)
        .map(|value| {
            value
                .to_str()
                .map(str::to_owned)
                .context("update catalog response contains a non-text ETag")
        })
        .transpose()
}

#[cfg(test)]
mod tests {
    use std::io::{Read as _, Write as _};
    use std::net::TcpListener;
    use std::sync::mpsc;
    use std::thread;

    use super::*;

    fn test_client(catalog_timeout: Duration) -> anyhow::Result<UpdateNetworkClient> {
        let mut config = UpdateClientConfig {
            catalog_timeout,
            artifact_timeout: catalog_timeout,
            ..UpdateClientConfig::default()
        };
        config.allow_loopback_http = true;
        UpdateNetworkClient::with_config(config)
    }

    fn serve_once(response: Vec<u8>) -> anyhow::Result<(String, mpsc::Receiver<String>)> {
        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        let (sender, receiver) = mpsc::channel();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("test server accepts request");
            stream
                .set_read_timeout(Some(Duration::from_secs(2)))
                .expect("test server read timeout");
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];
            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = stream.read(&mut buffer).expect("test server reads request");
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
            }
            let _ = sender.send(String::from_utf8_lossy(&request).into_owned());
            stream
                .write_all(&response)
                .expect("test server writes response");
        });
        Ok((format!("http://{address}/fixture"), receiver))
    }

    #[test]
    fn file_sources_are_bounded_deterministic_and_offline_safe() -> anyhow::Result<()> {
        let directory = tempfile::tempdir()?;
        let catalog = directory.path().join("catalog.json");
        fs::write(&catalog, b"fixture catalog")?;
        let client = UpdateNetworkClient::new(true)?;
        assert_eq!(
            client.fetch_catalog(&format!("file://{}", catalog.display()), Some("ignored"))?,
            CatalogFetch::Modified {
                bytes: b"fixture catalog".to_vec(),
                etag: None,
            }
        );
        assert_eq!(
            client.fetch_bounded(catalog.to_str().unwrap(), 15, "fixture")?,
            b"fixture catalog"
        );
        assert!(client
            .fetch_bounded(catalog.to_str().unwrap(), 14, "fixture")
            .unwrap_err()
            .to_string()
            .contains("size limit"));
        assert!(client
            .fetch_catalog("https://example.invalid/catalog.json", None)
            .unwrap_err()
            .to_string()
            .contains("offline"));
        Ok(())
    }

    #[test]
    fn local_server_catalog_requests_use_etags_and_no_identity_headers() -> anyhow::Result<()> {
        let body = b"catalog";
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nETag: \"v1\"\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes()
        .into_iter()
        .chain(body.iter().copied())
        .collect();
        let (url, request) = serve_once(response)?;
        let client = test_client(Duration::from_secs(2))?;
        assert_eq!(
            client.fetch_catalog(&url, None)?,
            CatalogFetch::Modified {
                bytes: body.to_vec(),
                etag: Some("\"v1\"".to_owned()),
            }
        );
        let request = request.recv_timeout(Duration::from_secs(2))?;
        let lower = request.to_ascii_lowercase();
        assert!(lower.contains(&format!("user-agent: {}", update_user_agent())));
        assert!(!lower.contains("cookie:"));
        assert!(!lower.contains("authorization:"));
        assert!(!lower.contains("telemetry"));

        let response = b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n".to_vec();
        let (url, request) = serve_once(response)?;
        assert_eq!(
            client.fetch_catalog(&url, Some("\"v1\""))?,
            CatalogFetch::NotModified {
                etag: Some("\"v1\"".to_owned()),
            }
        );
        let request = request.recv_timeout(Duration::from_secs(2))?;
        assert!(request
            .to_ascii_lowercase()
            .contains("if-none-match: \"v1\""));
        Ok(())
    }

    #[test]
    fn response_limits_timeouts_and_https_redirects_fail_closed() -> anyhow::Result<()> {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 1048577\r\nConnection: close\r\n\r\n".to_vec();
        let (url, _) = serve_once(response)?;
        let client = test_client(Duration::from_secs(2))?;
        assert!(client
            .fetch_catalog(&url, None)
            .unwrap_err()
            .to_string()
            .contains("response size limit"));

        let response = b"HTTP/1.1 302 Found\r\nLocation: http://127.0.0.1:9/downgrade\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
            .to_vec();
        let (url, _) = serve_once(response)?;
        let error = client.fetch_catalog(&url, None).unwrap_err();
        assert!(format!("{error:#}").contains("redirect target must use HTTPS"));

        let listener = TcpListener::bind(("127.0.0.1", 0))?;
        let address = listener.local_addr()?;
        thread::spawn(move || {
            let (_stream, _) = listener.accept().expect("timeout server accepts request");
            thread::sleep(Duration::from_millis(250));
        });
        let client = test_client(Duration::from_millis(50))?;
        let error = client
            .fetch_catalog(&format!("http://{address}/slow"), None)
            .unwrap_err();
        assert!(format!("{error:#}").contains("timed out"));
        Ok(())
    }

    #[test]
    fn artifacts_stream_to_disk_and_remove_oversized_partials() -> anyhow::Result<()> {
        let body = vec![b'x'; 128 * 1024];
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes()
        .into_iter()
        .chain(body.iter().copied())
        .collect();
        let (url, _) = serve_once(response)?;
        let directory = tempfile::tempdir()?;
        let destination = directory.path().join("artifact.tar.gz");
        let client = test_client(Duration::from_secs(2))?;
        assert_eq!(
            client.download_artifact(&url, &destination, body.len() as u64)?,
            body.len() as u64
        );
        assert_eq!(fs::read(&destination)?, body);

        let response = b"HTTP/1.1 200 OK\r\nConnection: close\r\n\r\ntoolarge".to_vec();
        let (url, _) = serve_once(response)?;
        let remote_rejected = directory.path().join("remote-rejected.tar.gz");
        assert!(client
            .download_artifact(&url, &remote_rejected, 3)
            .unwrap_err()
            .to_string()
            .contains("size limit"));
        assert!(!remote_rejected.exists());

        let source = directory.path().join("oversized.tar.gz");
        fs::write(&source, b"too large")?;
        let rejected = directory.path().join("rejected.tar.gz");
        assert!(client
            .download_artifact(source.to_str().unwrap(), &rejected, 3)
            .unwrap_err()
            .to_string()
            .contains("size limit"));
        assert!(!rejected.exists());
        Ok(())
    }
}
