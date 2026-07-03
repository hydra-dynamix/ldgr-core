//! Public helpers for materializing adapter bundles into an LDGR adapter root.
//!
//! A bundle is an adapter directory containing `adapter.toml` plus the
//! adapter-relative profile files declared by that manifest. This module copies
//! only the public manifest/profile surface and intentionally does not perform
//! adapter-specific install side effects.

use std::fs;
use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context};
use serde::Serialize;

use crate::adapter_manifest::{load_adapter_manifest, AdapterManifestParseReport};
use crate::adapter_registry::ADAPTER_MANIFEST_FILE;

/// Summary of a completed adapter bundle materialization.
#[derive(Clone, Debug, Serialize)]
pub struct AdapterBundleMaterialization {
    /// Source adapter manifest that was materialized.
    pub source_manifest_path: PathBuf,
    /// Destination adapter root that received the manifest and profile files.
    pub destination_root: PathBuf,
    /// Destination files written by the helper, relative to `destination_root`.
    pub copied_files: Vec<PathBuf>,
}

/// Copy an adapter bundle manifest and declared profile files into `destination_root`.
///
/// The manifest is loaded through the public manifest parser, so missing profile
/// files and malformed manifests fail before any files are copied. All declared
/// profile paths must be adapter-relative, lexical paths that do not contain
/// parent-directory traversal. Files are copied preserving their adapter-relative
/// locations under `destination_root`.
pub fn materialize_adapter_bundle(
    source_manifest_path: impl AsRef<Path>,
    destination_root: impl AsRef<Path>,
) -> anyhow::Result<AdapterBundleMaterialization> {
    let source_manifest_path = source_manifest_path.as_ref();
    let destination_root = destination_root.as_ref();
    let report = load_adapter_manifest(source_manifest_path).map_err(anyhow::Error::from)?;
    materialize_loaded_adapter_bundle(report, destination_root)
}

fn materialize_loaded_adapter_bundle(
    report: AdapterManifestParseReport,
    destination_root: &Path,
) -> anyhow::Result<AdapterBundleMaterialization> {
    let source_manifest_path = report
        .manifest_path
        .clone()
        .context("loaded adapter manifest report did not include source path")?;
    let source_root = report
        .manifest_dir
        .clone()
        .context("loaded adapter manifest report did not include source directory")?;

    let profile_files = [
        (
            "profile.loop_prompt_path",
            report.manifest.profile.loop_prompt_path.as_str(),
        ),
        (
            "profile.default_milestone_template",
            report.manifest.profile.default_milestone_template.as_str(),
        ),
        (
            "profile.spec_artifact_path",
            report.manifest.profile.spec_artifact_path.as_str(),
        ),
    ];

    let mut copy_plan = Vec::new();
    copy_plan.push((
        source_manifest_path.clone(),
        PathBuf::from(ADAPTER_MANIFEST_FILE),
    ));
    for (field, relative) in profile_files {
        let safe_relative = safe_adapter_relative_path(field, relative)?;
        copy_plan.push((source_root.join(&safe_relative), safe_relative));
    }

    fs::create_dir_all(destination_root).with_context(|| {
        format!(
            "failed to create adapter materialization root {}",
            destination_root.display()
        )
    })?;

    let mut copied_files = Vec::new();
    for (source, relative_destination) in copy_plan {
        if !source.is_file() {
            bail!("adapter bundle source file {} is missing", source.display());
        }
        let destination = destination_root.join(&relative_destination);
        if let Some(parent) = destination.parent() {
            fs::create_dir_all(parent).with_context(|| {
                format!(
                    "failed to create adapter materialization directory {}",
                    parent.display()
                )
            })?;
        }
        fs::copy(&source, &destination).with_context(|| {
            format!(
                "failed to copy adapter bundle file {} to {}",
                source.display(),
                destination.display()
            )
        })?;
        copied_files.push(relative_destination);
    }

    copied_files.sort();
    copied_files.dedup();

    Ok(AdapterBundleMaterialization {
        source_manifest_path,
        destination_root: destination_root.to_path_buf(),
        copied_files,
    })
}

fn safe_adapter_relative_path(field: &str, relative: &str) -> anyhow::Result<PathBuf> {
    let trimmed = relative.trim();
    if trimmed.is_empty() {
        bail!("{field} must not be empty");
    }

    let path = Path::new(trimmed);
    if path.is_absolute() {
        bail!("{field} must be relative to adapter.toml");
    }

    let mut safe = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Normal(segment) => safe.push(segment),
            Component::CurDir => {}
            Component::ParentDir => bail!("{field} must not contain parent-directory traversal"),
            Component::RootDir | Component::Prefix(_) => {
                bail!("{field} must be relative to adapter.toml")
            }
        }
    }

    if safe.as_os_str().is_empty() {
        bail!("{field} must reference a file below the adapter root");
    }
    Ok(safe)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};

    use tempfile::TempDir;

    use super::materialize_adapter_bundle;

    #[test]
    fn materializes_example_fixture_bundle_manifest_prompts_and_templates() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let bundle = dir.path().join("example-bundle");
        write_example_bundle(
            &bundle,
            r#"
[adapter]
slug = "example"
title = "Example fixture adapter"
core_version = "0.1"
aliases = ["example-fixture"]

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "Fixture evidence must pass."
"#,
        )?;
        let destination = dir.path().join("installed/example");

        let materialization =
            materialize_adapter_bundle(bundle.join("adapter.toml"), &destination)?;

        assert_eq!(
            materialization.copied_files,
            vec![
                PathBuf::from("adapter.toml"),
                PathBuf::from("prompts/loop.md"),
                PathBuf::from("templates/milestones.md"),
                PathBuf::from("templates/spec.md"),
            ]
        );
        assert_eq!(
            fs::read_to_string(destination.join("adapter.toml"))?.contains("slug = \"example\""),
            true
        );
        assert_eq!(
            fs::read_to_string(destination.join("prompts/loop.md"))?,
            "loop prompt"
        );
        assert_eq!(
            fs::read_to_string(destination.join("templates/milestones.md"))?,
            "milestone template"
        );
        assert_eq!(
            fs::read_to_string(destination.join("templates/spec.md"))?,
            "spec template"
        );
        Ok(())
    }

    #[test]
    fn materialization_rejects_profile_path_traversal() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let bundle = dir.path().join("traversal-bundle");
        fs::create_dir_all(bundle.join("templates"))?;
        fs::write(dir.path().join("outside-loop.md"), "outside")?;
        fs::write(bundle.join("templates/milestones.md"), "milestone")?;
        fs::write(bundle.join("templates/spec.md"), "spec")?;
        fs::write(
            bundle.join("adapter.toml"),
            r#"
[adapter]
slug = "traversal"
title = "Traversal"
core_version = "0.1"

[profile]
loop_prompt_path = "../outside-loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"
"#,
        )?;

        let error = materialize_adapter_bundle(bundle.join("adapter.toml"), dir.path().join("out"))
            .expect_err("path traversal should be rejected");

        assert!(
            error.to_string().contains("parent-directory traversal"),
            "{error:#}"
        );
        assert!(!dir.path().join("out/adapter.toml").exists());
        Ok(())
    }

    #[test]
    fn materialization_rejects_missing_declared_profile_file() -> anyhow::Result<()> {
        let dir = TempDir::new()?;
        let bundle = dir.path().join("missing-file-bundle");
        fs::create_dir_all(bundle.join("prompts"))?;
        fs::create_dir_all(bundle.join("templates"))?;
        fs::write(bundle.join("prompts/loop.md"), "loop")?;
        fs::write(bundle.join("templates/spec.md"), "spec")?;
        fs::write(
            bundle.join("adapter.toml"),
            r#"
[adapter]
slug = "missing-file"
title = "Missing file"
core_version = "0.1"

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/missing.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"
"#,
        )?;

        let error = materialize_adapter_bundle(bundle.join("adapter.toml"), dir.path().join("out"))
            .expect_err("missing profile file should be rejected");

        assert!(
            error
                .to_string()
                .contains("profile.default_milestone_template"),
            "{error:#}"
        );
        assert!(!dir.path().join("out/adapter.toml").exists());
        Ok(())
    }

    fn write_example_bundle(dir: &Path, manifest: &str) -> anyhow::Result<()> {
        fs::create_dir_all(dir.join("prompts"))?;
        fs::create_dir_all(dir.join("templates"))?;
        fs::write(dir.join("prompts/loop.md"), "loop prompt")?;
        fs::write(dir.join("templates/milestones.md"), "milestone template")?;
        fs::write(dir.join("templates/spec.md"), "spec template")?;
        fs::write(dir.join("adapter.toml"), manifest)?;
        Ok(())
    }
}
