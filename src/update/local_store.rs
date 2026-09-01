use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{ensure, Context};
use reqwest::Url;

use super::catalog::{AdapterCatalogSources, CoreCatalogSources};

pub const CORE_CATALOG_FILE: &str = "core-index.json";
pub const ADAPTER_CATALOG_FILE: &str = "index.json";
pub const RELEASE_KEYRING_FILE: &str = "release-keyring.json";
pub const ARTIFACTS_DIRECTORY: &str = "artifacts";

#[derive(Clone, Debug)]
pub struct LocalReleaseStore {
    root: PathBuf,
}

impl LocalReleaseStore {
    pub fn open(path: impl AsRef<Path>) -> anyhow::Result<Self> {
        let path = path.as_ref();
        ensure!(
            !path.as_os_str().is_empty(),
            "local release store path must not be empty"
        );
        let absolute = if path.is_absolute() {
            path.to_path_buf()
        } else {
            std::env::current_dir()
                .context("failed to resolve the current directory for the local release store")?
                .join(path)
        };
        let metadata = fs::symlink_metadata(&absolute).with_context(|| {
            format!(
                "failed to inspect local release store {}",
                absolute.display()
            )
        })?;
        ensure!(
            metadata.file_type().is_dir() && !metadata.file_type().is_symlink(),
            "local release store must be a directory and not a symbolic link"
        );
        let root = absolute.canonicalize().with_context(|| {
            format!(
                "failed to canonicalize local release store {}",
                absolute.display()
            )
        })?;
        Ok(Self { root })
    }

    pub fn root(&self) -> &Path {
        &self.root
    }

    pub fn core_catalog_sources(&self) -> anyhow::Result<CoreCatalogSources> {
        self.required_file(
            &format!("{CORE_CATALOG_FILE}.sig"),
            "Core update catalog signature",
        )?;
        CoreCatalogSources::new(
            self.required_file(CORE_CATALOG_FILE, "Core update catalog")?
                .display()
                .to_string(),
            self.optional_keyring_source()?,
            true,
        )
    }

    pub fn adapter_catalog_sources(&self) -> anyhow::Result<AdapterCatalogSources> {
        self.required_file(
            &format!("{ADAPTER_CATALOG_FILE}.sig"),
            "adapter update catalog signature",
        )?;
        AdapterCatalogSources::new(
            self.required_file(ADAPTER_CATALOG_FILE, "adapter update catalog")?
                .display()
                .to_string(),
            self.optional_keyring_source()?,
            true,
        )
    }

    pub fn resolve_artifact_source(&self, source: &str) -> anyhow::Result<PathBuf> {
        let file_name = source_file_name(source)?;
        let candidates = [
            self.root.join(ARTIFACTS_DIRECTORY).join(&file_name),
            self.root.join(&file_name),
        ];
        let existing = candidates
            .iter()
            .filter(|candidate| candidate.exists())
            .collect::<Vec<_>>();
        ensure!(
            existing.len() == 1,
            "local release store must contain exactly one artifact named `{}` under `{}` or `{}`; found {}",
            file_name.to_string_lossy(),
            self.root.join(ARTIFACTS_DIRECTORY).display(),
            self.root.display(),
            existing.len()
        );
        self.validate_regular_file(existing[0], "local release artifact")
    }

    fn optional_keyring_source(&self) -> anyhow::Result<Option<String>> {
        let path = self.root.join(RELEASE_KEYRING_FILE);
        if !path.exists() {
            return Ok(None);
        }
        Ok(Some(
            self.validate_regular_file(&path, "local release keyring")?
                .display()
                .to_string(),
        ))
    }

    fn required_file(&self, relative: &str, label: &str) -> anyhow::Result<PathBuf> {
        self.validate_regular_file(&self.root.join(relative), label)
    }

    fn validate_regular_file(&self, path: &Path, label: &str) -> anyhow::Result<PathBuf> {
        let metadata = fs::symlink_metadata(path)
            .with_context(|| format!("failed to inspect {label} {}", path.display()))?;
        ensure!(
            metadata.file_type().is_file() && !metadata.file_type().is_symlink(),
            "{label} must be a regular file and not a symbolic link: {}",
            path.display()
        );
        let canonical = path
            .canonicalize()
            .with_context(|| format!("failed to canonicalize {label} {}", path.display()))?;
        ensure!(
            canonical.parent().is_some_and(|parent| {
                parent == self.root || parent == self.root.join(ARTIFACTS_DIRECTORY)
            }),
            "{label} escapes the local release store boundary: {}",
            path.display()
        );
        Ok(canonical)
    }
}

fn source_file_name(source: &str) -> anyhow::Result<PathBuf> {
    ensure!(
        !source.trim().is_empty(),
        "release artifact source must not be empty"
    );
    let name = if source.contains("://") {
        let url = Url::parse(source).context("release artifact source is not a valid URL")?;
        ensure!(
            url.query().is_none() && url.fragment().is_none(),
            "release artifact URL must not contain a query or fragment in local store mode"
        );
        url.path_segments()
            .and_then(|mut segments| segments.next_back())
            .filter(|segment| !segment.is_empty())
            .map(str::to_owned)
            .context("release artifact URL has no file name")?
    } else {
        Path::new(source)
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| !name.is_empty())
            .map(str::to_owned)
            .context("release artifact source has no UTF-8 file name")?
    };
    ensure!(
        !name.contains('%') && Path::new(&name).components().count() == 1,
        "release artifact file name is not a canonical path component"
    );
    Ok(PathBuf::from(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolves_only_unambiguous_direct_store_artifacts() -> anyhow::Result<()> {
        let temp = tempfile::tempdir()?;
        fs::create_dir(temp.path().join(ARTIFACTS_DIRECTORY))?;
        fs::write(
            temp.path().join(ARTIFACTS_DIRECTORY).join("adapter.tar.gz"),
            "archive",
        )?;
        let store = LocalReleaseStore::open(temp.path())?;
        assert_eq!(
            store.resolve_artifact_source("https://example.invalid/releases/adapter.tar.gz")?,
            temp.path()
                .join(ARTIFACTS_DIRECTORY)
                .join("adapter.tar.gz")
                .canonicalize()?
        );
        fs::write(temp.path().join("adapter.tar.gz"), "duplicate")?;
        assert!(store
            .resolve_artifact_source("https://example.invalid/adapter.tar.gz")
            .unwrap_err()
            .to_string()
            .contains("exactly one artifact"));
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn rejects_symlinked_store_files() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let temp = tempfile::tempdir()?;
        let outside = tempfile::NamedTempFile::new()?;
        fs::write(
            temp.path().join(format!("{CORE_CATALOG_FILE}.sig")),
            "signature",
        )?;
        symlink(outside.path(), temp.path().join(CORE_CATALOG_FILE))?;
        let store = LocalReleaseStore::open(temp.path())?;
        assert!(store
            .core_catalog_sources()
            .unwrap_err()
            .to_string()
            .contains("not a symbolic link"));
        Ok(())
    }
}
