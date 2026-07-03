use std::fs;
use std::path::{Path, PathBuf};

use ldgr_core::adapter_bundle::materialize_adapter_bundle;
use ldgr_core::adapter_manifest::{
    load_adapter_manifest, parse_adapter_manifest_text, AdapterManifestDiagnosticCode,
};
use ldgr_core::adapter_profile::{apply_adapter_profile_prompt, AdapterProfileApplyOptions};
use ldgr_core::adapter_registry::AdapterRegistry;
use ldgr_core::manifest_integrity::{canonical_manifest_digest, AdapterManifestDigestState};
use tempfile::TempDir;

#[derive(Clone, Copy)]
struct OpenAdapterFixture {
    slug: &'static str,
    namespace: &'static str,
    manifest_path: &'static str,
    expected_alias: &'static str,
}

const OPEN_ADAPTER_FIXTURES: &[OpenAdapterFixture] = &[
    OpenAdapterFixture {
        slug: "example",
        namespace: "example",
        manifest_path: "../ldgr-example-adapter/adapter.toml",
        expected_alias: "reference",
    },
    OpenAdapterFixture {
        slug: "research",
        namespace: "research",
        manifest_path: "../ldgr-research/adapter.toml",
        expected_alias: "ldgr-research",
    },
];

#[test]
fn open_adapter_fixtures_satisfy_discovery_materialization_and_apply_contracts(
) -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let install_root = temp.path().join("installed-adapters");

    for fixture in OPEN_ADAPTER_FIXTURES {
        let source_manifest = fixture_manifest_path(fixture);

        let loaded = load_adapter_manifest(&source_manifest)?;
        assert_eq!(loaded.manifest.adapter.slug, fixture.slug);
        assert!(
            loaded
                .manifest
                .adapter
                .aliases
                .iter()
                .any(|alias| alias == fixture.expected_alias),
            "{} fixture should keep documented alias {}",
            fixture.slug,
            fixture.expected_alias
        );
        assert!(
            loaded
                .manifest
                .commands
                .iter()
                .any(|command| command.namespace == fixture.namespace),
            "{} fixture should advertise its command namespace",
            fixture.slug
        );

        let destination = install_root.join(fixture.slug);
        let materialization = materialize_adapter_bundle(&source_manifest, &destination)?;
        assert_eq!(materialization.destination_root, destination);
        assert!(materialization
            .copied_files
            .contains(&PathBuf::from("adapter.toml")));
        assert!(
            materialization
                .copied_files
                .contains(&PathBuf::from(&loaded.manifest.profile.loop_prompt_path)),
            "{} fixture should materialize its loop prompt",
            fixture.slug
        );

        let db = temp.path().join(format!("{}-ledger/ldgr.db", fixture.slug));
        let artifact_root = temp.path().join(format!("{}-artifacts", fixture.slug));
        let application = apply_adapter_profile_prompt(AdapterProfileApplyOptions {
            manifest_path: &destination.join("adapter.toml"),
            db_path: &db,
            artifact_root: &artifact_root,
            prompt_slug: &format!("{}-loop", fixture.slug),
            prompt_role: &format!("{}-adapter-loop", fixture.slug),
            description: Some("public adapter contract fixture application"),
        })?;
        assert_eq!(application.prompt.status, "active");
        assert_eq!(
            application.prompt.body,
            fs::read_to_string(application.loop_prompt_path)?
        );
    }

    let registry = AdapterRegistry::discover_from_roots([install_root]);
    assert!(registry.warnings.is_empty(), "{:#?}", registry.warnings);
    let slugs = registry
        .adapters
        .iter()
        .map(|adapter| adapter.slug.as_str())
        .collect::<Vec<_>>();
    assert_eq!(slugs, vec!["example", "research"]);
    for fixture in OPEN_ADAPTER_FIXTURES {
        assert_eq!(
            registry.find(fixture.expected_alias).unwrap().slug,
            fixture.slug
        );
        let namespace = registry
            .resolve_namespace(fixture.namespace)
            .expect("fixture namespace should resolve");
        assert_eq!(namespace.adapter_slug, fixture.slug);
    }

    Ok(())
}

#[test]
fn malformed_manifest_contract_returns_structured_public_diagnostics() {
    let error = parse_adapter_manifest_text("[adapter\n")
        .expect_err("malformed public manifest TOML should fail");

    assert_eq!(error.diagnostics.len(), 1);
    assert_eq!(
        error.diagnostics[0].code,
        AdapterManifestDiagnosticCode::MalformedToml
    );
    assert!(error.diagnostics[0]
        .message
        .contains("failed to parse adapter manifest TOML"));
}

#[test]
fn digest_mismatch_contract_is_reported_by_public_loader_and_skipped_by_discovery(
) -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let root = temp.path().join("adapters");
    let tampered = root.join("tampered");
    let valid = root.join("valid");

    write_contract_adapter(&valid, "valid", None)?;

    let baseline = contract_manifest_text("tampered", None);
    let digest = canonical_manifest_digest(&baseline)?;
    let manifest_with_mismatched_digest = contract_manifest_text("tampered-renamed", Some(&digest));
    write_contract_adapter(
        &tampered,
        "tampered-renamed",
        Some(&manifest_with_mismatched_digest),
    )?;

    let public_report = load_adapter_manifest(tampered.join("adapter.toml"))?;
    assert_eq!(
        public_report.integrity.state,
        AdapterManifestDigestState::Failed
    );
    assert!(public_report
        .integrity
        .message
        .as_deref()
        .unwrap_or_default()
        .contains("adapter manifest digest mismatch"));

    let registry = AdapterRegistry::discover_from_roots([root]);
    assert_eq!(
        registry
            .adapters
            .iter()
            .map(|adapter| adapter.slug.as_str())
            .collect::<Vec<_>>(),
        vec!["valid"]
    );
    assert!(
        registry
            .warnings
            .iter()
            .any(|warning| warning.message.contains("adapter manifest digest mismatch")),
        "{:#?}",
        registry.warnings
    );

    Ok(())
}

#[test]
fn documented_public_example_flow_uses_only_public_adapter_apis() -> anyhow::Result<()> {
    let temp = TempDir::new()?;
    let fixture = OPEN_ADAPTER_FIXTURES[0];
    let source_manifest = fixture_manifest_path(&fixture);
    let installed = temp.path().join("public-example/example");

    let manifest = load_adapter_manifest(&source_manifest)?.manifest;
    let materialized = materialize_adapter_bundle(&source_manifest, &installed)?;
    let registry =
        AdapterRegistry::discover_from_roots([installed.parent().unwrap().to_path_buf()]);
    let applied = apply_adapter_profile_prompt(AdapterProfileApplyOptions {
        manifest_path: &installed.join("adapter.toml"),
        db_path: &temp.path().join("ledger/ldgr.db"),
        artifact_root: &temp.path().join("artifacts"),
        prompt_slug: "example-loop",
        prompt_role: "example-adapter-loop",
        description: Some("documented public adapter API example"),
    })?;

    assert_eq!(manifest.adapter.slug, "example");
    assert!(materialized
        .copied_files
        .contains(&PathBuf::from("adapter.toml")));
    assert_eq!(
        registry.find("example").unwrap().title,
        "LDGR Example adapter"
    );
    assert_eq!(applied.prompt.slug, "example-loop");

    Ok(())
}

fn fixture_manifest_path(fixture: &OpenAdapterFixture) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join(fixture.manifest_path)
}

fn write_contract_adapter(
    dir: &Path,
    slug: &str,
    manifest_override: Option<&str>,
) -> anyhow::Result<()> {
    fs::create_dir_all(dir.join("prompts"))?;
    fs::create_dir_all(dir.join("templates"))?;
    fs::write(dir.join("prompts/loop.md"), format!("{slug} loop"))?;
    fs::write(
        dir.join("templates/milestones.md"),
        format!("{slug} milestones"),
    )?;
    fs::write(dir.join("templates/spec.md"), format!("{slug} spec"))?;
    fs::write(
        dir.join("adapter.toml"),
        manifest_override
            .map(str::to_owned)
            .unwrap_or_else(|| contract_manifest_text(slug, None)),
    )?;
    Ok(())
}

fn contract_manifest_text(slug: &str, digest: Option<&str>) -> String {
    let mut manifest = format!(
        r#"
[adapter]
slug = "{slug}"
title = "{slug} contract adapter"
core_version = "0.1"

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"

[[commands]]
namespace = "{slug}"
argv = ["ldgr-{slug}"]
title = "{slug} commands"
description = "{slug} contract command namespace"

[commands.help]
usage = "ldgr {slug} <command>"
summary = "Run {slug} commands."
"#
    );
    if let Some(digest) = digest {
        manifest.push_str(&format!("\n[integrity]\nmanifest_digest = {digest:?}\n"));
    }
    manifest
}
