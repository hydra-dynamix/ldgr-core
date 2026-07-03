//! Public helpers for applying adapter profile prompts to an LDGR ledger.
//!
//! Adapter manifests declare prompt/template paths relative to `adapter.toml`.
//! This module resolves those paths through the public manifest parser and uses
//! the real core prompt store lifecycle to create or update a prompt, activate
//! it, and validate the durable active prompt record.

use std::path::{Component, Path, PathBuf};

use anyhow::{bail, Context};
use serde::Serialize;

use crate::adapter_manifest::load_adapter_manifest;
use crate::store::{
    active_prompt, create_prompt, get_prompt, init_store, open_store, set_prompt_status,
    stable_content_hash, update_prompt, Prompt,
};

/// Options for applying an adapter manifest's loop prompt into a ledger.
#[derive(Clone, Debug)]
pub struct AdapterProfileApplyOptions<'a> {
    /// Adapter manifest whose `[profile].loop_prompt_path` should be applied.
    pub manifest_path: &'a Path,
    /// LDGR ledger database to initialize/open.
    pub db_path: &'a Path,
    /// Artifact root passed to the core ledger initializer.
    pub artifact_root: &'a Path,
    /// Durable prompt slug to create/update and activate.
    pub prompt_slug: &'a str,
    /// Durable prompt role for newly-created prompts. Existing prompts must already match it.
    pub prompt_role: &'a str,
    /// Optional prompt record description.
    pub description: Option<&'a str>,
}

/// Summary of an applied adapter profile prompt.
#[derive(Clone, Debug, Serialize)]
pub struct AdapterProfileApplication {
    /// Manifest used to resolve the prompt path.
    pub manifest_path: PathBuf,
    /// Adapter root containing the manifest.
    pub adapter_root: PathBuf,
    /// Adapter-relative loop prompt path from the manifest.
    pub loop_prompt_relative_path: PathBuf,
    /// Fully resolved loop prompt file path.
    pub loop_prompt_path: PathBuf,
    /// Durable prompt record after activation and validation.
    pub prompt: Prompt,
}

/// Apply an adapter manifest's loop prompt through the core ledger prompt lifecycle.
///
/// The manifest is loaded with the public manifest parser, so malformed manifests
/// and missing referenced profile files are rejected before prompt mutation. The
/// loop prompt path is resolved relative to the manifest directory and must be a
/// lexical adapter-relative file path without parent-directory traversal. The
/// helper initializes the ledger, creates or updates `prompt_slug`, activates it,
/// then re-reads the active prompt and validates slug, role, body hash, status,
/// and source path.
pub fn apply_adapter_profile_prompt(
    options: AdapterProfileApplyOptions<'_>,
) -> anyhow::Result<AdapterProfileApplication> {
    let report = load_adapter_manifest(options.manifest_path).map_err(anyhow::Error::from)?;
    let manifest_path = report
        .manifest_path
        .clone()
        .unwrap_or_else(|| options.manifest_path.to_path_buf());
    let adapter_root = report.manifest_dir.clone().with_context(|| {
        format!(
            "adapter manifest {} has no parent directory",
            manifest_path.display()
        )
    })?;
    let loop_prompt_relative_path = safe_adapter_relative_path(
        "profile.loop_prompt_path",
        &report.manifest.profile.loop_prompt_path,
    )?;
    let loop_prompt_path = adapter_root.join(&loop_prompt_relative_path);
    let prompt_body = std::fs::read_to_string(&loop_prompt_path).with_context(|| {
        format!(
            "failed to read adapter loop prompt {}",
            loop_prompt_path.display()
        )
    })?;
    let source_path = loop_prompt_path.to_string_lossy().to_string();

    init_store(options.db_path, options.artifact_root)?;
    let connection = open_store(options.db_path)?;
    if let Some(existing) = get_prompt(&connection, options.prompt_slug)? {
        if existing.role != options.prompt_role {
            bail!(
                "prompt {} already exists with role {}; expected {}",
                options.prompt_slug,
                existing.role,
                options.prompt_role
            );
        }
        update_prompt(
            &connection,
            options.prompt_slug,
            &prompt_body,
            Some(source_path.as_str()),
            options.description,
        )?;
    } else {
        create_prompt(
            &connection,
            options.prompt_slug,
            options.prompt_role,
            &prompt_body,
            Some(source_path.as_str()),
            options.description,
        )?;
    }
    set_prompt_status(&connection, options.prompt_slug, "active")?;
    let prompt = active_prompt(&connection, options.prompt_slug)?;
    validate_applied_prompt(
        &prompt,
        options.prompt_slug,
        options.prompt_role,
        &prompt_body,
        &source_path,
    )?;

    Ok(AdapterProfileApplication {
        manifest_path,
        adapter_root,
        loop_prompt_relative_path,
        loop_prompt_path,
        prompt,
    })
}

fn validate_applied_prompt(
    prompt: &Prompt,
    expected_slug: &str,
    expected_role: &str,
    expected_body: &str,
    expected_source_path: &str,
) -> anyhow::Result<()> {
    if prompt.slug != expected_slug {
        bail!(
            "activated prompt slug mismatch: got {}, expected {expected_slug}",
            prompt.slug
        );
    }
    if prompt.role != expected_role {
        bail!(
            "activated prompt role mismatch: got {}, expected {expected_role}",
            prompt.role
        );
    }
    if prompt.status != "active" {
        bail!("activated prompt {} is not active", prompt.slug);
    }
    let expected_hash = stable_content_hash(expected_body);
    if prompt.content_hash != expected_hash {
        bail!(
            "activated prompt {} hash mismatch: got {}, expected {}",
            prompt.slug,
            prompt.content_hash,
            expected_hash
        );
    }
    if prompt.source_path.as_deref() != Some(expected_source_path) {
        bail!(
            "activated prompt {} source path mismatch: got {:?}, expected {}",
            prompt.slug,
            prompt.source_path,
            expected_source_path
        );
    }
    Ok(())
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

    use tempfile::TempDir;

    use super::{apply_adapter_profile_prompt, AdapterProfileApplyOptions};
    use crate::store::{active_prompt, create_prompt, init_store, open_store};

    #[test]
    fn applies_manifest_loop_prompt_and_validates_active_store_record() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let adapter = write_adapter(temp.path(), "prompts/loop.md", "first prompt")?;
        let db = temp.path().join("ledger/ldgr.db");
        let artifacts = temp.path().join("artifacts");

        let application = apply_adapter_profile_prompt(AdapterProfileApplyOptions {
            manifest_path: &adapter.join("adapter.toml"),
            db_path: &db,
            artifact_root: &artifacts,
            prompt_slug: "example-loop",
            prompt_role: "example-adapter-loop",
            description: Some("applied by test"),
        })?;

        assert_eq!(
            application.loop_prompt_relative_path,
            std::path::PathBuf::from("prompts/loop.md")
        );
        assert_eq!(application.prompt.slug, "example-loop");
        assert_eq!(application.prompt.role, "example-adapter-loop");
        assert_eq!(application.prompt.status, "active");
        let connection = open_store(&db)?;
        let prompt = active_prompt(&connection, "example-loop")?;
        assert_eq!(prompt.body, "first prompt");
        assert_eq!(
            prompt.source_path.as_deref(),
            Some(application.loop_prompt_path.to_string_lossy().as_ref())
        );
        Ok(())
    }

    #[test]
    fn updates_existing_matching_prompt_and_rejects_role_mismatch() -> anyhow::Result<()> {
        let temp = TempDir::new()?;
        let adapter = write_adapter(temp.path(), "prompts/loop.md", "updated prompt")?;
        let db = temp.path().join("ledger/ldgr.db");
        let artifacts = temp.path().join("artifacts");
        init_store(&db, &artifacts)?;
        let connection = open_store(&db)?;
        create_prompt(
            &connection,
            "adapter-loop",
            "adapter-role",
            "old",
            None,
            None,
        )?;

        let application = apply_adapter_profile_prompt(AdapterProfileApplyOptions {
            manifest_path: &adapter.join("adapter.toml"),
            db_path: &db,
            artifact_root: &artifacts,
            prompt_slug: "adapter-loop",
            prompt_role: "adapter-role",
            description: None,
        })?;
        assert_eq!(application.prompt.current_version, 2);
        assert_eq!(application.prompt.body, "updated prompt");

        let error = apply_adapter_profile_prompt(AdapterProfileApplyOptions {
            manifest_path: &adapter.join("adapter.toml"),
            db_path: &db,
            artifact_root: &artifacts,
            prompt_slug: "adapter-loop",
            prompt_role: "different-role",
            description: None,
        })
        .expect_err("role mismatch should be rejected");
        assert!(
            error.to_string().contains("already exists with role"),
            "{error:#}"
        );
        Ok(())
    }

    #[test]
    fn rejects_loop_prompt_parent_directory_traversal_before_store_mutation() -> anyhow::Result<()>
    {
        let temp = TempDir::new()?;
        fs::write(temp.path().join("outside.md"), "outside")?;
        let adapter = write_adapter(temp.path(), "../outside.md", "unused")?;
        let db = temp.path().join("ledger/ldgr.db");
        let artifacts = temp.path().join("artifacts");

        let error = apply_adapter_profile_prompt(AdapterProfileApplyOptions {
            manifest_path: &adapter.join("adapter.toml"),
            db_path: &db,
            artifact_root: &artifacts,
            prompt_slug: "adapter-loop",
            prompt_role: "adapter-role",
            description: None,
        })
        .expect_err("traversal should be rejected");

        assert!(
            error.to_string().contains("parent-directory traversal"),
            "{error:#}"
        );
        assert!(!db.exists());
        Ok(())
    }

    fn write_adapter(
        root: &std::path::Path,
        loop_prompt_path: &str,
        prompt_body: &str,
    ) -> anyhow::Result<std::path::PathBuf> {
        let adapter = root.join("adapter");
        fs::create_dir_all(adapter.join("prompts"))?;
        fs::create_dir_all(adapter.join("templates"))?;
        fs::write(adapter.join("prompts/loop.md"), prompt_body)?;
        fs::write(adapter.join("templates/milestone.md"), "milestone")?;
        fs::write(adapter.join("templates/spec.md"), "spec")?;
        fs::write(
            adapter.join("adapter.toml"),
            format!(
                r#"
[adapter]
slug = "example"
title = "Example"
core_version = "0.1"

[profile]
loop_prompt_path = {loop_prompt_path:?}
default_milestone_template = "templates/milestone.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"
"#
            ),
        )?;
        Ok(adapter)
    }
}
