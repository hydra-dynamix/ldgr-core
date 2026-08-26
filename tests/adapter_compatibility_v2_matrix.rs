use std::fs;
use std::path::{Path, PathBuf};

use ldgr_core::adapter_compatibility::{
    evaluate_legacy_v1, evaluate_v2, parse_adapter_compatibility, parse_adapter_compatibility_v2,
    parse_core_compatibility_v2, projected_database_components, AdapterCompatibilitySidecarV2,
    CentralComponentDatabaseStateV2, CompatibilityReasonCode, ComponentLineageEntryV2,
    CoreCompatibilityProfileV2, LegacyCoreProfileV1, LocalStoreDescriptorV2,
    ParsedAdapterCompatibility,
};
use ldgr_core::adapter_registry::{AdapterOperationalState, AdapterRegistry};
use ldgr_core::release_index::{
    resolve_release_with_profile, AdapterClassification, AdapterPlatformRelease, AdapterRelease,
    AdapterReleaseIndex, AdapterReleaseProduct, ReleaseChannel, ReleaseKeyring,
    ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
};
use ldgr_core::update::apply::{InstallTransaction, OwnedTarget};
use ldgr_core::update::catalog::{
    CandidateCoreAdapterCompatibilityV2, CorePlatformArchive, CoreRelease,
    CoreReleaseCompatibility, CoreUpdateCatalog, PairedAgentctlRelease, VerifiedCoreUpdateCatalog,
    CORE_RELEASE_METADATA_SCHEMA_VERSION, CORE_UPDATE_CATALOG_SCHEMA_VERSION,
    ERROR_RECOVERY_SCHEMA_VERSION, LAUNCHER_COMPATIBILITY_SCHEMA_V1,
};
use ldgr_core::update::plan::{
    build_update_plan, AdapterInstallationKind, AdapterInstallationSnapshot, AdapterOrigin,
    CoreInstallationSnapshot, CorePlanOwnership, InstalledAdapterCompatibility, UpdateAction,
    UpdateInventory, UpdatePlan, UpdatePlanRequest, VerifiedCatalogSnapshots,
};
use rusqlite::Connection;
use semver::Version;

const A: &str = "sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const B: &str = "sha256:bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const C: &str = "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";

fn c5() -> CoreCompatibilityProfileV2 {
    parse_core_compatibility_v2(include_str!(
        "fixtures/adapter-compatibility-v2/c5-core.json"
    ))
    .expect("C5 Core fixture")
}

fn adapter_v2() -> AdapterCompatibilitySidecarV2 {
    parse_adapter_compatibility_v2(include_str!(
        "fixtures/adapter-compatibility-v2/example-v2.json"
    ))
    .expect("example v2 fixture")
}

fn database(profile: &CoreCompatibilityProfileV2) -> Vec<CentralComponentDatabaseStateV2> {
    projected_database_components(profile)
}

fn reason_codes(
    result: &ldgr_core::adapter_compatibility::CompatibilityEvaluation,
) -> Vec<CompatibilityReasonCode> {
    result.reasons.iter().map(|reason| reason.code).collect()
}

fn host_platform() -> &'static str {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("linux", "x86_64") => "linux-x86_64",
        ("linux", "aarch64") => "linux-aarch64",
        ("macos", "x86_64") => "macos-x86_64",
        ("macos", "aarch64") => "macos-aarch64",
        ("windows", "x86_64") => "windows-x86_64",
        pair => panic!("unsupported compatibility-matrix host {pair:?}"),
    }
}

fn binary(name: &str) -> String {
    if cfg!(windows) {
        format!("{name}.exe")
    } else {
        name.to_owned()
    }
}

fn adapter_platform() -> AdapterPlatformRelease {
    AdapterPlatformRelease {
        platform: host_platform().to_owned(),
        asset_url: "https://example.invalid/example.tar.gz".to_owned(),
        archive_root: "example-1.0.0".to_owned(),
        binary: binary("ldgr-example"),
        sha256: "1".repeat(64),
        signature_url: "https://example.invalid/example.tar.gz.sig".to_owned(),
        signing_key_id: "fixture-key".to_owned(),
        resource_manifest: "adapter-resources.json".to_owned(),
    }
}

fn release_index(requirements: &AdapterCompatibilitySidecarV2) -> AdapterReleaseIndex {
    let compatibility = requirements.compatibility.clone();
    let compatibility_sha256 = compatibility
        .compatibility_sha256()
        .expect("compatibility fingerprint");
    AdapterReleaseIndex {
        schema_version: ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
        adapters: vec![AdapterReleaseProduct {
            domain: "example".to_owned(),
            primary_namespace: "example".to_owned(),
            title: "Example".to_owned(),
            aliases: Vec::new(),
            classification: AdapterClassification::OpenSource,
            source_url: None,
            releases: vec![AdapterRelease {
                version: "1.0.0".to_owned(),
                channel: ReleaseChannel::Stable,
                core_compatibility: String::new(),
                compatibility: Some(compatibility),
                compatibility_sha256: Some(compatibility_sha256),
                platforms: vec![adapter_platform()],
            }],
        }],
    }
}

fn core_catalog(profile: CoreCompatibilityProfileV2) -> VerifiedCoreUpdateCatalog {
    let candidate = CandidateCoreAdapterCompatibilityV2 {
        projected_database_components: projected_database_components(&profile),
        legacy_profile: LegacyCoreProfileV1 {
            contract_hash: A.to_owned(),
            core_schema_version: i64::from(profile.core_schema_version),
            components: Vec::new(),
        },
        profile,
    };
    VerifiedCoreUpdateCatalog {
        catalog: CoreUpdateCatalog {
            schema_version: CORE_UPDATE_CATALOG_SCHEMA_VERSION,
            release_keys: Vec::new(),
            releases: vec![CoreRelease {
                version: "0.1.15".to_owned(),
                channel: ReleaseChannel::Stable,
                minimum_updater_version: "0.1.0".to_owned(),
                core_commit: "1".repeat(40),
                source_repository: "hydra-dynamix/ldgr-core".to_owned(),
                agentctl: PairedAgentctlRelease {
                    version: "0.1.2".to_owned(),
                    repository: "hydra-dynamix/agentctl".to_owned(),
                    commit: "2".repeat(40),
                },
                compatibility: CoreReleaseCompatibility {
                    launcher_compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA_V1.to_owned(),
                    error_recovery_schema: ERROR_RECOVERY_SCHEMA_VERSION,
                    release_metadata_schema: CORE_RELEASE_METADATA_SCHEMA_VERSION,
                    adapter_compatibility: Some(candidate),
                },
                platforms: vec![CorePlatformArchive {
                    platform: host_platform().to_owned(),
                    archive_url: "file:///fixtures/ldgr-core-0.1.15.tar.gz".to_owned(),
                    archive_root: "ldgr-core-0.1.15".to_owned(),
                    sha256: "3".repeat(64),
                    signature_url: "file:///fixtures/ldgr-core-0.1.15.tar.gz.sig".to_owned(),
                    signing_key_id: "fixture-key".to_owned(),
                }],
            }],
        },
        catalog_signing_key_id: "fixture-key".to_owned(),
        archive_keyring: ReleaseKeyring { keys: Vec::new() },
    }
}

fn installed_adapter(sidecar: AdapterCompatibilitySidecarV2) -> AdapterInstallationSnapshot {
    AdapterInstallationSnapshot {
        slug: "example".to_owned(),
        origin: AdapterOrigin::User,
        installation: AdapterInstallationKind::Release {
            version: "1.0.0".to_owned(),
            core_compatibility: String::new(),
        },
        compatibility: InstalledAdapterCompatibility::V2 { sidecar },
    }
}

fn update_plan(
    profile: CoreCompatibilityProfileV2,
    sidecar: AdapterCompatibilitySidecarV2,
    adapters: &AdapterReleaseIndex,
) -> anyhow::Result<UpdatePlan> {
    let core = core_catalog(profile);
    build_update_plan(
        &UpdatePlanRequest::default(),
        &VerifiedCatalogSnapshots {
            core: &core,
            adapters,
        },
        &UpdateInventory {
            core: CoreInstallationSnapshot {
                current_core: "0.1.14".to_owned(),
                current_agentctl: "0.1.2".to_owned(),
                ownership: CorePlanOwnership::ReceiptManaged,
            },
            adapters: vec![installed_adapter(sidecar)],
            discovery_warnings: Vec::new(),
        },
        &Version::parse("0.1.14")?,
        host_platform(),
    )
}

#[test]
fn core_patch_additive_schema_and_new_capability_retain_the_installed_adapter() -> anyhow::Result<()>
{
    let sidecar = adapter_v2();
    let index = release_index(&sidecar);

    for package_patch in ["0.1.14", "0.1.99", "0.2.0"] {
        let resolved = resolve_release_with_profile(
            &index,
            "example",
            &Version::parse(package_patch)?,
            &c5(),
            &database(&c5()),
            host_platform(),
            None,
            false,
        )?;
        assert_eq!(resolved.version, Version::parse("1.0.0")?);
    }

    let root = tempfile::tempdir()?;
    let db_path = root.path().join("core-c5.db");
    let connection = Connection::open(&db_path)?;
    connection.execute_batch(
        "CREATE TABLE work (id INTEGER PRIMARY KEY, title TEXT NOT NULL);\n         INSERT INTO work (title) VALUES ('still readable');",
    )?;
    connection.execute_batch(
        "CREATE TABLE audit (id INTEGER PRIMARY KEY, work_id INTEGER);\n         CREATE INDEX idx_audit_work_id ON audit(work_id);\n         ALTER TABLE work ADD COLUMN optional_note TEXT;",
    )?;
    assert_eq!(
        connection.query_row("SELECT title FROM work WHERE id = 1", [], |row| row
            .get::<_, String>(0))?,
        "still readable"
    );
    drop(connection);

    let mut c6 = c5();
    c6.core_schema_version = 6;
    c6.core_capabilities.push("work.v2".to_owned());
    c6.core_capabilities.sort();
    let evaluation = evaluate_v2(&sidecar, "example", &c6, &database(&c6));
    assert!(evaluation.compatible, "{:?}", evaluation.reasons);

    for profile in [c5(), c6] {
        let plan = update_plan(
            profile,
            sidecar.clone(),
            &AdapterReleaseIndex {
                schema_version: ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
                adapters: Vec::new(),
            },
        )?;
        assert!(!plan.blocked(), "{:?}", plan.warnings());
        assert_eq!(plan.components()[0].action(), UpdateAction::Update);
        assert_eq!(plan.components()[1].action(), UpdateAction::None);
        assert_eq!(plan.components()[1].current(), Some("1.0.0"));
        assert_eq!(plan.components()[1].target(), Some("1.0.0"));
    }
    Ok(())
}

#[test]
fn local_store_migration_is_isolated_from_core_and_release_eligibility() -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let central_path = root.path().join("ldgr.db");
    let local_path = root.path().join("research.db");
    Connection::open(&central_path)?.execute_batch(
        "CREATE TABLE work (id INTEGER PRIMARY KEY, title TEXT NOT NULL);\n         INSERT INTO work (title) VALUES ('central');",
    )?;
    Connection::open(&local_path)?
        .execute_batch("CREATE TABLE source (id INTEGER PRIMARY KEY, locator TEXT NOT NULL);")?;
    let central_before = fs::read(&central_path)?;

    let mut research = adapter_v2();
    research.adapter = "research".to_owned();
    research.compatibility.central_components.clear();
    research.local_stores = vec![LocalStoreDescriptorV2 {
        store_id: "research".to_owned(),
        engine: "sqlite".to_owned(),
        schema_version: 4,
        migration_digest: B.to_owned(),
    }];
    let fingerprint_before = research.compatibility_sha256()?;

    Connection::open(&local_path)?.execute_batch(
        "ALTER TABLE source ADD COLUMN optional_digest TEXT;\n         CREATE INDEX idx_source_locator ON source(locator);",
    )?;
    research.local_stores[0].schema_version = 5;
    research.local_stores[0].migration_digest = C.to_owned();

    assert_eq!(research.compatibility_sha256()?, fingerprint_before);
    assert_eq!(fs::read(&central_path)?, central_before);
    let evaluation = evaluate_v2(&research, "research", &c5(), &database(&c5()));
    assert!(evaluation.compatible, "{:?}", evaluation.reasons);
    Ok(())
}

#[test]
fn registered_components_are_additive_but_pending_forks_and_breaks_are_blocked(
) -> anyhow::Result<()> {
    let sidecar = adapter_v2();
    let mut additive = c5();
    additive.central_components[0].schema_version = 3;
    additive.central_components[0]
        .lineage
        .push(ComponentLineageEntryV2 {
            schema_version: 3,
            migration_digest: C.to_owned(),
        });
    let mut unrelated = additive.central_components[0].clone();
    unrelated.namespace = "other".to_owned();
    unrelated.owner_adapter = "other".to_owned();
    unrelated.lineage = vec![ComponentLineageEntryV2 {
        schema_version: 1,
        migration_digest: C.to_owned(),
    }];
    unrelated.schema_version = 1;
    additive.central_components.push(unrelated);
    let ready = evaluate_v2(&sidecar, "example", &additive, &database(&additive));
    assert!(ready.compatible, "{:?}", ready.reasons);

    let pending = evaluate_v2(&sidecar, "example", &additive, &[]);
    assert_eq!(
        reason_codes(&pending),
        vec![CompatibilityReasonCode::CentralComponentDatabaseState]
    );

    let mut fork = c5();
    fork.central_components[0].lineage[0].migration_digest = C.to_owned();
    let forked = evaluate_v2(&sidecar, "example", &fork, &database(&fork));
    assert_eq!(
        reason_codes(&forked),
        vec![CompatibilityReasonCode::CentralComponentLineageMismatch]
    );

    let mut destructive = c5();
    destructive.central_components[0].schema_epoch = 2;
    let blocked = evaluate_v2(&sidecar, "example", &destructive, &database(&destructive));
    assert_eq!(
        reason_codes(&blocked),
        vec![CompatibilityReasonCode::CentralComponentEpochMismatch]
    );

    let empty_catalog = AdapterReleaseIndex {
        schema_version: ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
        adapters: Vec::new(),
    };
    let plan = update_plan(destructive, sidecar, &empty_catalog)?;
    assert!(plan.blocked());
    assert_eq!(plan.components()[1].action(), UpdateAction::Blocked);
    assert!(plan
        .warnings()
        .iter()
        .any(|warning| warning.contains("compatibility.central_component_epoch_mismatch")));
    Ok(())
}

#[test]
fn legacy_v1_is_degraded_only_on_an_exact_match_and_stale_metadata_never_falls_back(
) -> anyhow::Result<()> {
    let legacy_text = include_str!("fixtures/adapter-compatibility-v2/example-v1.json");
    let ParsedAdapterCompatibility::LegacyV1 { contract } =
        parse_adapter_compatibility(None, Some(legacy_text))?
    else {
        panic!("expected legacy fixture");
    };
    let exact = LegacyCoreProfileV1 {
        contract_hash: A.to_owned(),
        core_schema_version: 5,
        components: vec![contract.component.clone()],
    };
    assert!(evaluate_legacy_v1(&contract, "example", &exact).compatible);

    let installed = tempfile::tempdir()?;
    write_adapter_manifest(installed.path(), "example")?;
    fs::write(
        installed.path().join("adapter-database-contract.json"),
        legacy_text,
    )?;
    let registry = AdapterRegistry::discover_from_roots_with_profiles(
        [installed.path().to_path_buf()],
        &c5(),
        &database(&c5()),
        &exact,
    );
    let discovered = registry.find("example").expect("legacy install visible");
    assert_eq!(discovered.state, AdapterOperationalState::Degraded);
    assert!(discovered.state.permits_dispatch());
    assert!(registry.resolve_namespace("example").is_some());

    let stale = LegacyCoreProfileV1 {
        contract_hash: C.to_owned(),
        core_schema_version: 6,
        components: exact.components.clone(),
    };
    assert_eq!(
        reason_codes(&evaluate_legacy_v1(&contract, "example", &stale)),
        vec![
            CompatibilityReasonCode::LegacyGlobalContractMismatch,
            CompatibilityReasonCode::LegacyCoreSchemaMismatch,
        ]
    );
    let stale_registry = AdapterRegistry::discover_from_roots_with_profiles(
        [installed.path().to_path_buf()],
        &c5(),
        &database(&c5()),
        &stale,
    );
    let stale_install = stale_registry
        .find("example")
        .expect("stale install visible");
    assert_eq!(stale_install.state, AdapterOperationalState::Blocked);
    assert!(!stale_install.state.permits_dispatch());
    assert!(stale_registry.resolve_namespace("example").is_none());

    let malformed_v2 = r#"{"format":"ldgr.adapter-compatibility.v2","unknown":true}"#;
    let error = parse_adapter_compatibility(Some(malformed_v2), Some(legacy_text))
        .expect_err("a stale/malformed v2 manifest must not downgrade to v1");
    assert_eq!(error.reason.code, CompatibilityReasonCode::InvalidMetadata);
    fs::write(
        installed.path().join("adapter-compatibility.json"),
        malformed_v2,
    )?;
    let malformed_registry = AdapterRegistry::discover_from_roots_with_profiles(
        [installed.path().to_path_buf()],
        &c5(),
        &database(&c5()),
        &exact,
    );
    assert_eq!(
        malformed_registry
            .find("example")
            .expect("malformed install visible")
            .state,
        AdapterOperationalState::Invalid
    );
    assert!(malformed_registry.resolve_namespace("example").is_none());
    Ok(())
}

#[test]
fn protocol_break_and_missing_repair_release_block_before_mutation_with_diagnostics(
) -> anyhow::Result<()> {
    let root = tempfile::tempdir()?;
    let active = root.path().join("active-adapter.txt");
    fs::write(&active, "working-v1")?;

    let mut protocol2 = c5();
    protocol2.supported_adapter_protocol_epochs = vec![2];
    let empty_catalog = AdapterReleaseIndex {
        schema_version: ADAPTER_RELEASE_INDEX_SCHEMA_VERSION,
        adapters: Vec::new(),
    };
    let plan = update_plan(protocol2, adapter_v2(), &empty_catalog)?;

    assert!(plan.blocked());
    assert_eq!(plan.components()[0].action(), UpdateAction::Update);
    assert_eq!(plan.components()[1].action(), UpdateAction::Blocked);
    assert!(plan.warnings().iter().any(|warning| {
        warning.contains("compatibility.protocol_epoch_unsupported")
            && warning.contains("no matching staged repair")
    }));
    assert_eq!(fs::read_to_string(active)?, "working-v1");
    assert!(!root.path().join("rollback").exists());
    Ok(())
}

#[test]
fn interrupted_native_activation_restores_core_adapters_receipts_resources_and_database(
) -> anyhow::Result<()> {
    if let Ok(expected) = std::env::var("LDGR_EXPECTED_TEST_PLATFORM") {
        assert_eq!(host_platform(), expected);
    }

    let root = tempfile::tempdir()?;
    let installation = root.path().join("Program Files").join("LDGR matrix μ");
    let staging = root.path().join("staging");
    fs::create_dir_all(&installation)?;
    fs::create_dir_all(&staging)?;

    let core = installation.join(binary("ldgr"));
    let agentctl = installation.join(binary("agentctl"));
    let adapter_binary = installation.join(binary("ldgr-example"));
    let adapter_bundle = installation.join("adapters").join("example");
    let receipt = installation.join("receipts").join("example.json");
    let resource = installation.join("harness resources").join("example.md");
    let central_db = installation.join("ldgr.db");
    for parent in [adapter_bundle.parent(), receipt.parent(), resource.parent()]
        .into_iter()
        .flatten()
    {
        fs::create_dir_all(parent)?;
    }
    fs::create_dir_all(&adapter_bundle)?;
    fs::write(adapter_bundle.join("adapter.toml"), "old bundle")?;
    for (path, contents) in [
        (&core, "old core"),
        (&agentctl, "old agentctl"),
        (&adapter_binary, "old adapter binary"),
        (&receipt, "old receipt"),
        (&resource, "old resource"),
        (&central_db, "old database bytes"),
    ] {
        fs::write(path, contents)?;
    }

    let staged_core = staging.join(binary("ldgr"));
    let staged_agentctl = staging.join(binary("agentctl"));
    let staged_adapter_binary = staging.join(binary("ldgr-example"));
    let staged_bundle = staging.join("example-bundle");
    let staged_receipt = staging.join("example.json");
    let staged_resource = staging.join("example.md");
    let staged_db = staging.join("ldgr.db");
    fs::create_dir_all(&staged_bundle)?;
    fs::write(staged_bundle.join("adapter.toml"), "new bundle")?;
    for (path, contents) in [
        (&staged_core, "new core"),
        (&staged_agentctl, "new agentctl"),
        (&staged_adapter_binary, "new adapter binary"),
        (&staged_receipt, "new receipt"),
        (&staged_resource, "new resource"),
        (&staged_db, "new database bytes"),
    ] {
        fs::write(path, contents)?;
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        for path in [&core, &agentctl, &adapter_binary] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o750))?;
        }
        for path in [&staged_core, &staged_agentctl, &staged_adapter_binary] {
            fs::set_permissions(path, fs::Permissions::from_mode(0o700))?;
        }
    }

    let targets = [
        target(&installation, "core", "core_binary", &core),
        target(&installation, "core", "agentctl_binary", &agentctl),
        target(&installation, "example", "adapter_binary", &adapter_binary),
        target(&installation, "example", "adapter_bundle", &adapter_bundle),
        target(&installation, "example", "receipt", &receipt),
        target(&installation, "example", "harness_resource", &resource),
        target(&installation, "core", "central_database", &central_db),
    ];
    let journal = root.path().join("rollback journal");
    let mut transaction = InstallTransaction::prepare(journal.clone(), &"d".repeat(64), &targets)?;
    transaction.activate_file(&staged_core, &core)?;
    transaction.activate_file(&staged_agentctl, &agentctl)?;
    transaction.activate_file(&staged_adapter_binary, &adapter_binary)?;
    transaction.activate_directory(&staged_bundle, &adapter_bundle)?;
    transaction.activate_file(&staged_receipt, &receipt)?;
    transaction.activate_file(&staged_resource, &resource)?;
    transaction.activate_file(&staged_db, &central_db)?;
    assert_eq!(fs::read_to_string(&core)?, "new core");
    std::mem::forget(transaction);

    let mut recovered = InstallTransaction::resume_for_rollback(journal)?;
    recovered.rollback()?;
    recovered.rollback()?;

    for (path, contents) in [
        (&core, "old core"),
        (&agentctl, "old agentctl"),
        (&adapter_binary, "old adapter binary"),
        (&receipt, "old receipt"),
        (&resource, "old resource"),
        (&central_db, "old database bytes"),
    ] {
        assert_eq!(fs::read_to_string(path)?, contents);
    }
    assert_eq!(
        fs::read_to_string(adapter_bundle.join("adapter.toml"))?,
        "old bundle"
    );
    assert_eq!(
        core.file_name().and_then(|name| name.to_str()),
        Some(binary("ldgr").as_str())
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(fs::metadata(core)?.permissions().mode() & 0o777, 0o750);
    }
    Ok(())
}

fn write_adapter_manifest(dir: &Path, slug: &str) -> anyhow::Result<()> {
    fs::create_dir_all(dir.join("prompts"))?;
    fs::create_dir_all(dir.join("templates"))?;
    fs::write(dir.join("prompts/loop.md"), "loop")?;
    fs::write(dir.join("templates/milestones.md"), "milestones")?;
    fs::write(dir.join("templates/spec.md"), "spec")?;
    fs::write(
        dir.join("adapter.toml"),
        format!(
            r#"[adapter]
slug = "{slug}"
title = "Compatibility fixture"
core_version = "0.1"

[profile]
loop_prompt_path = "prompts/loop.md"
default_milestone_template = "templates/milestones.md"
spec_artifact_path = "templates/spec.md"
readiness_policy = "ready"

[[tools]]
name = "{slug}-check"
argv = ["{slug}", "check"]
description = "Fixture command."
"#,
        ),
    )?;
    Ok(())
}

fn target(boundary: &Path, component: &str, role: &str, path: &Path) -> OwnedTarget {
    OwnedTarget::new(
        component,
        role,
        PathBuf::from(boundary),
        PathBuf::from(path),
    )
}
