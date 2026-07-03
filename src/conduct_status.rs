use std::ffi::OsString;
use std::path::Path;
use std::process::Command;

use anyhow::{bail, Context};
use serde_json::Value;

use crate::adapter_registry::{AdapterCommandNamespace, AdapterRegistry};

/// Execute the adapter-owned read-only conduct batch status surface.
///
/// Core must not inspect `.ldgr/.conduct` worker directories or parse conduct
/// batch artifacts as a stable contract. Any core cockpit/context conduct view is
/// a compatibility projection over this adapter-owned JSON status API.
pub fn read_conduct_batch_status_json(
    db: &Path,
    artifact_root: &Path,
    batch_id: Option<&str>,
) -> anyhow::Result<Option<Value>> {
    let registry = AdapterRegistry::discover();
    let Some(namespace) = conduct_namespace(&registry) else {
        return Ok(None);
    };
    if namespace.argv.is_empty() {
        bail!(
            "conduct adapter namespace `{}` has empty argv",
            namespace.namespace
        );
    }

    let working_dir = std::env::current_dir().context("failed to resolve current directory")?;
    let mut args = Vec::<OsString>::new();
    args.extend(namespace.argv.iter().skip(1).map(OsString::from));
    args.push("batch".into());
    args.push("status".into());
    if let Some(batch_id) = batch_id.filter(|value| !value.trim().is_empty()) {
        args.push("--batch-id".into());
        args.push(batch_id.into());
    }
    args.push("--json".into());

    let output = Command::new(&namespace.argv[0])
        .args(args)
        .env("LDGR_DB", db)
        .env("LDGR_ARTIFACT_ROOT", artifact_root)
        .env("LDGR_WORKING_DIR", working_dir)
        .env("LDGR_ADAPTER_SLUG", &namespace.adapter_slug)
        .env("LDGR_ADAPTER_NAMESPACE", &namespace.namespace)
        .output()
        .with_context(|| {
            format!(
                "failed to execute conduct adapter status command `{}`",
                namespace.argv[0]
            )
        })?;

    if !output.status.success() {
        bail!(
            "conduct adapter status command failed with status {}: {}",
            output.status,
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    let value = serde_json::from_slice(&output.stdout).with_context(|| {
        format!(
            "conduct adapter status command emitted invalid JSON: {}",
            String::from_utf8_lossy(&output.stdout).trim()
        )
    })?;
    Ok(Some(value))
}

fn conduct_namespace(registry: &AdapterRegistry) -> Option<&AdapterCommandNamespace> {
    registry
        .resolve_namespace("conduct")
        .or_else(|| {
            registry
                .adapters
                .iter()
                .filter(|adapter| {
                    adapter.slug == "conduct" || adapter.aliases.iter().any(|a| a == "conduct")
                })
                .flat_map(|adapter| &adapter.command_namespaces)
                .find(|namespace| namespace.namespace == "conduct")
        })
        .or_else(|| {
            registry
                .adapters
                .iter()
                .filter(|adapter| adapter.slug.contains("conduct"))
                .flat_map(|adapter| &adapter.command_namespaces)
                .find(|namespace| namespace.namespace.contains("conduct"))
        })
}
