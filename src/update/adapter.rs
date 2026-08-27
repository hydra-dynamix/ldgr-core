use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{bail, Context};
use semver::{Version, VersionReq};

use crate::adapter_registry::{AdapterRegistry, DiscoveredAdapter};
use crate::harness_config::UpdateChannel;
use crate::release_index::{
    AdapterPlatformRelease, AdapterRelease, AdapterReleaseProduct, InstallationReceipt,
    ResolvedAdapterRelease, SourceInstallationReceipt,
};
use crate::update::apply::{
    AdapterOwnershipReceipt, AdapterStagingOwnership, PlanStagingOwnership, StagedArtifact,
    StagingManifest,
};
use crate::update::catalog::VerifiedAdapterUpdateCatalog;
use crate::update::plan::{AdapterInstallationKind, UpdateAction, UpdatePlan, UpdatePlanComponent};
use crate::update::state::ComponentResult;

pub(crate) use crate::update::apply::InstallTransaction;

#[derive(Debug)]
pub(crate) struct AdapterInstallationInspection {
    slug: String,
    install_root: PathBuf,
    home: PathBuf,
    receipt: AdapterInstallationReceipt,
}

#[derive(Debug)]
enum AdapterInstallationReceipt {
    Release(InstallationReceipt),
    Source {
        receipt: SourceInstallationReceipt,
        source_root: PathBuf,
        source_sha256: String,
        source_changed: bool,
    },
}

#[derive(Debug)]
pub(crate) enum AdapterUpdatePlan {
    Release {
        slug: String,
        install_root: PathBuf,
        home: PathBuf,
        installed_version: Version,
        target_version: Version,
        update_available: bool,
        adapter: AdapterReleaseProduct,
        release: AdapterRelease,
        platform: AdapterPlatformRelease,
    },
    LocalSource {
        slug: String,
        install_root: PathBuf,
        home: PathBuf,
        package: String,
        source_root: PathBuf,
        source_sha256: String,
        source_changed: bool,
    },
}

impl AdapterUpdatePlan {
    pub(crate) fn print_status(&self) {
        match self {
            Self::Release {
                slug,
                installed_version,
                target_version,
                update_available,
                ..
            } => println!(
                "adapter={slug} installed={installed_version} latest_compatible={target_version} update_available={update_available}"
            ),
            Self::LocalSource {
                slug,
                source_changed,
                ..
            } => println!(
                "adapter={slug} install_kind=local_source source_changed={source_changed} verified_release=false"
            ),
        }
    }

    pub(crate) fn should_apply_for_single_adapter_command(&self) -> bool {
        match self {
            // Preserve the explicit command's behavior: it reruns the recorded installer even
            // when source content is unchanged. Bulk planning can select only changed sources.
            Self::LocalSource { .. } => true,
            Self::Release { .. } => self.update_available(),
        }
    }

    pub(crate) fn update_available(&self) -> bool {
        match self {
            Self::Release {
                update_available, ..
            } => *update_available,
            Self::LocalSource { source_changed, .. } => *source_changed,
        }
    }
}

pub(crate) fn inspect_adapter_installation(
    name: &str,
) -> anyhow::Result<AdapterInstallationInspection> {
    let registry = AdapterRegistry::discover();
    let installed = registry
        .find(name)
        .with_context(|| format!("adapter `{name}` is not installed"))?;
    let value = installed
        .installation_receipt
        .clone()
        .context("installed adapter has no tracked installation receipt; reinstall it first")?;
    let home = home_dir()?;
    let receipt = match parse_installation_receipt(value)? {
        AdapterInstallationReceipt::Release(receipt) => {
            anyhow::ensure!(
                receipt.domain == installed.slug,
                "release receipt domain `{}` does not match installed adapter `{}`",
                receipt.domain,
                installed.slug
            );
            AdapterInstallationReceipt::Release(receipt)
        }
        AdapterInstallationReceipt::Source { receipt, .. } => {
            let (source_root, source_sha256, source_changed) =
                crate::cli::commands::ops::inspect_source_installation_for_update(
                    &installed.root_path,
                    &home,
                    &receipt,
                )?;
            AdapterInstallationReceipt::Source {
                receipt,
                source_root,
                source_sha256,
                source_changed,
            }
        }
    };
    Ok(AdapterInstallationInspection {
        slug: installed.slug.clone(),
        install_root: installed.root_path.clone(),
        home,
        receipt,
    })
}

fn validate_release_receipt_schema(receipt: &InstallationReceipt) -> anyhow::Result<()> {
    match receipt.schema_version {
        1 => anyhow::ensure!(
            receipt.compatibility.is_none() && receipt.compatibility_sha256.is_none(),
            "release receipt schema 1 cannot contain compatibility-v2 metadata"
        ),
        2 => anyhow::ensure!(
            receipt.compatibility.is_some() && receipt.compatibility_sha256.is_some(),
            "release receipt schema 2 requires compatibility-v2 metadata"
        ),
        schema => anyhow::bail!("unsupported release receipt schema {schema}; expected 1 or 2"),
    }
    Ok(())
}

pub(crate) fn inspect_adapter_for_bulk(
    installed: &DiscoveredAdapter,
    home: &Path,
    user_adapter_root: &Path,
) -> anyhow::Result<(AdapterInstallationKind, AdapterStagingOwnership)> {
    let value = installed
        .installation_receipt
        .clone()
        .context("installed adapter has no tracked installation receipt")?;
    match parse_installation_receipt(value)? {
        AdapterInstallationReceipt::Release(receipt) => {
            validate_release_receipt_schema(&receipt)?;
            anyhow::ensure!(
                receipt.domain == installed.slug,
                "release receipt domain does not match the discovered adapter"
            );
            Version::parse(&receipt.version).context("release receipt version is invalid")?;
            if receipt.compatibility.is_none() {
                VersionReq::parse(&receipt.core_compatibility)
                    .context("legacy release receipt Core compatibility is invalid")?;
            } else {
                anyhow::ensure!(
                    receipt.core_compatibility.is_empty(),
                    "v2 release receipt must not contain a legacy Core compatibility range"
                );
            }
            crate::cli::commands::ops::inspect_release_installation_for_update(
                &installed.root_path,
                home,
                &receipt,
            )?;
            let snapshot = AdapterInstallationKind::Release {
                version: receipt.version.clone(),
                core_compatibility: receipt.core_compatibility.clone(),
            };
            Ok((
                snapshot,
                AdapterStagingOwnership {
                    install_root: installed.root_path.clone(),
                    user_adapter_root: user_adapter_root.to_path_buf(),
                    receipt: AdapterOwnershipReceipt::Release(receipt),
                },
            ))
        }
        AdapterInstallationReceipt::Source { receipt, .. } => {
            anyhow::ensure!(
                receipt.domain == installed.slug,
                "source receipt domain does not match the discovered adapter"
            );
            let (_, current_source_sha256, source_changed) =
                crate::cli::commands::ops::inspect_source_installation_for_update(
                    &installed.root_path,
                    home,
                    &receipt,
                )?;
            let snapshot = AdapterInstallationKind::LocalSource {
                package: receipt.source.package.clone(),
                installed_source_sha256: receipt.source.bundle_sha256.clone(),
                current_source_sha256,
                source_changed,
            };
            Ok((
                snapshot,
                AdapterStagingOwnership {
                    install_root: installed.root_path.clone(),
                    user_adapter_root: user_adapter_root.to_path_buf(),
                    receipt: AdapterOwnershipReceipt::LocalSource(receipt),
                },
            ))
        }
    }
}

#[derive(Debug)]
pub(crate) struct AdapterBulkApplyError {
    pub(crate) source: anyhow::Error,
    pub(crate) components: Vec<ComponentResult>,
}

impl std::fmt::Display for AdapterBulkApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.source)
    }
}

impl std::error::Error for AdapterBulkApplyError {}

pub(crate) fn apply_staged_adapter_updates(
    plan: &UpdatePlan,
    manifest: &StagingManifest,
    catalog: &VerifiedAdapterUpdateCatalog,
    ownership: &PlanStagingOwnership,
    transaction: &mut InstallTransaction,
    quiet: bool,
) -> Result<Vec<ComponentResult>, AdapterBulkApplyError> {
    let mut components = Vec::new();
    for (index, component) in plan.components().iter().enumerate() {
        let UpdatePlanComponent::Adapter {
            name,
            action,
            release,
            local_source,
            ..
        } = component
        else {
            continue;
        };
        let applied = match action {
            UpdateAction::Update => apply_staged_release(
                name,
                release.as_ref(),
                manifest,
                catalog,
                ownership,
                transaction,
                quiet,
            ),
            UpdateAction::ReinstallLocalSource => apply_staged_local_source(
                name,
                local_source.as_ref(),
                manifest,
                ownership,
                transaction,
                quiet,
            ),
            _ => {
                components.push(component_result(component, action_name(*action)));
                continue;
            }
        };
        match applied {
            Ok(()) => components.push(component_result(component, "applied")),
            Err(error) => {
                let rollback = transaction.rollback();
                for result in &mut components {
                    if result.status == "applied" {
                        result.status = "rolled_back".to_owned();
                    }
                }
                components.push(component_result(component, "failed"));
                for pending in plan.components().iter().skip(index + 1) {
                    if matches!(pending, UpdatePlanComponent::Adapter { .. }) {
                        let status = if matches!(
                            pending.action(),
                            UpdateAction::Update | UpdateAction::ReinstallLocalSource
                        ) {
                            "failed"
                        } else {
                            action_name(pending.action())
                        };
                        components.push(component_result(pending, status));
                    }
                }
                let source = match rollback {
                    Ok(()) => error,
                    Err(rollback_error) => anyhow::anyhow!(
                        "{error:#}; whole-plan rollback also failed: {rollback_error:#}"
                    ),
                };
                return Err(AdapterBulkApplyError { source, components });
            }
        }
    }
    Ok(components)
}

fn apply_staged_release(
    name: &str,
    artifact: Option<&crate::update::plan::AdapterReleaseArtifact>,
    manifest: &StagingManifest,
    catalog: &VerifiedAdapterUpdateCatalog,
    ownership: &PlanStagingOwnership,
    transaction: &mut InstallTransaction,
    quiet: bool,
) -> anyhow::Result<()> {
    let artifact = artifact.context("selected signed adapter has no resolved artifact")?;
    let product = catalog
        .catalog
        .adapters
        .iter()
        .find(|adapter| adapter.domain == name)
        .context("selected signed adapter is absent from the verified catalog")?;
    let staged = manifest
        .artifacts
        .iter()
        .find(|staged| matches!(staged, StagedArtifact::AdapterRelease { name: staged_name, .. } if staged_name == name))
        .context("selected signed adapter has no staged artifact")?;
    let StagedArtifact::AdapterRelease { extracted_root, .. } = staged else {
        unreachable!();
    };
    let owned = ownership
        .adapters
        .get(name)
        .context("selected signed adapter has no ownership record")?;
    let resolved = ResolvedAdapterRelease {
        adapter: product,
        release: &artifact.release,
        platform: &artifact.platform,
        version: Version::parse(&artifact.version)?,
    };
    crate::cli::commands::ops::apply_staged_resolved_index_release(
        &resolved,
        extracted_root,
        &owned.install_root,
        &ownership.home,
        transaction,
        quiet,
        matches!(owned.receipt, AdapterOwnershipReceipt::LegacyNoReceipt),
    )
}

fn apply_staged_local_source(
    name: &str,
    artifact: Option<&crate::update::plan::LocalSourceArtifact>,
    manifest: &StagingManifest,
    ownership: &PlanStagingOwnership,
    transaction: &mut InstallTransaction,
    quiet: bool,
) -> anyhow::Result<()> {
    let artifact = artifact.context("selected local-source adapter has no resolved artifact")?;
    let staged = manifest
        .artifacts
        .iter()
        .find(|staged| matches!(staged, StagedArtifact::LocalSource { name: staged_name, .. } if staged_name == name))
        .context("selected local-source adapter has no staged source")?;
    let StagedArtifact::LocalSource {
        source_root,
        source_sha256,
        ..
    } = staged
    else {
        unreachable!();
    };
    anyhow::ensure!(
        source_sha256 == &artifact.current_source_sha256,
        "staged local-source digest differs from the resolved plan"
    );
    let owned = ownership
        .adapters
        .get(name)
        .context("selected local-source adapter has no ownership record")?;
    crate::cli::commands::ops::apply_source_adapter_update(
        name,
        &artifact.package,
        source_root,
        &owned.install_root,
        &ownership.home,
        Some(source_sha256),
        transaction,
        quiet,
    )
}

fn component_result(component: &UpdatePlanComponent, status: &str) -> ComponentResult {
    ComponentResult {
        kind: "adapter".to_owned(),
        name: component.name().to_owned(),
        status: status.to_owned(),
    }
}

fn action_name(action: UpdateAction) -> &'static str {
    match action {
        UpdateAction::None => "none",
        UpdateAction::Update => "update",
        UpdateAction::ReinstallLocalSource => "reinstall_local_source",
        UpdateAction::SkipUnmanaged => "skip_unmanaged",
        UpdateAction::Blocked => "blocked",
        UpdateAction::Applied => "applied",
        UpdateAction::RolledBack => "rolled_back",
        UpdateAction::Failed => "failed",
    }
}

pub(crate) fn plan_adapter_update(
    inspection: AdapterInstallationInspection,
    target_core_version: &Version,
    channel: UpdateChannel,
) -> anyhow::Result<AdapterUpdatePlan> {
    match inspection.receipt {
        AdapterInstallationReceipt::Source {
            receipt,
            source_root,
            source_sha256,
            source_changed,
        } => {
            if channel == UpdateChannel::Prerelease {
                bail!(
                    "--prerelease applies only to signed release adapters, not local source installs"
                );
            }
            Ok(AdapterUpdatePlan::LocalSource {
                slug: inspection.slug,
                install_root: inspection.install_root,
                home: inspection.home,
                package: receipt.source.package,
                source_root,
                source_sha256,
                source_changed,
            })
        }
        AdapterInstallationReceipt::Release(receipt) => {
            let index = crate::release_index::load_configured_release_index()?;
            let platform_tag = platform_tag()?;
            plan_release_update(
                inspection.slug,
                inspection.install_root,
                inspection.home,
                receipt,
                target_core_version,
                channel,
                &index,
                &platform_tag,
            )
        }
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_release_update(
    slug: String,
    install_root: PathBuf,
    home: PathBuf,
    receipt: InstallationReceipt,
    target_core_version: &Version,
    channel: UpdateChannel,
    index: &crate::release_index::AdapterReleaseIndex,
    platform_tag: &str,
) -> anyhow::Result<AdapterUpdatePlan> {
    let installed_version =
        Version::parse(&receipt.version).context("installed receipt version is invalid")?;
    let resolved = crate::release_index::resolve_release(
        index,
        &slug,
        target_core_version,
        platform_tag,
        None,
        channel == UpdateChannel::Prerelease,
    )?;
    let target_version = resolved.version.clone();
    Ok(AdapterUpdatePlan::Release {
        slug,
        install_root,
        home,
        update_available: target_version > installed_version,
        installed_version,
        target_version,
        adapter: resolved.adapter.clone(),
        release: resolved.release.clone(),
        platform: resolved.platform.clone(),
    })
}

pub(crate) fn stage_and_apply_adapter_update(
    plan: &AdapterUpdatePlan,
    transaction: &mut InstallTransaction,
) -> anyhow::Result<()> {
    match plan {
        AdapterUpdatePlan::Release {
            slug,
            install_root,
            home,
            target_version,
            adapter,
            release,
            platform,
            ..
        } => {
            let resolved = ResolvedAdapterRelease {
                adapter,
                release,
                platform,
                version: target_version.clone(),
            };
            let staging_root = std::env::temp_dir().join(format!(
                "ldgr-adapter-index-install-{slug}-{}",
                std::process::id()
            ));
            let _ = fs::remove_dir_all(&staging_root);
            fs::create_dir_all(&staging_root)?;
            println!("\u{25c7} Installing LDGR adapter `{slug}`");
            println!(
                "\u{251c}\u{2500} Resolved version {} for {}",
                target_version, platform.platform
            );
            println!("\u{251c}\u{2500} Install root {}", install_root.display());
            crate::cli::commands::ops::stage_and_apply_resolved_index_release(
                &resolved,
                install_root,
                home,
                false,
                &staging_root,
                transaction,
            )?;
            let _ = fs::remove_dir_all(&staging_root);
            println!(
                "\u{2514}\u{2500} Installed adapter `{slug}`. Try `ldgr {slug} --help` or `ldgr adapter show {slug}`."
            );
            Ok(())
        }
        AdapterUpdatePlan::LocalSource {
            slug,
            install_root,
            home,
            package,
            source_root,
            source_sha256,
            ..
        } => crate::cli::commands::ops::apply_source_adapter_update(
            slug,
            package,
            source_root,
            install_root,
            home,
            Some(source_sha256),
            transaction,
            false,
        ),
    }
}

fn parse_installation_receipt(
    value: serde_json::Value,
) -> anyhow::Result<AdapterInstallationReceipt> {
    if value
        .get("install_kind")
        .and_then(serde_json::Value::as_str)
        == Some("local_source")
    {
        let receipt: SourceInstallationReceipt =
            serde_json::from_value(value).context("source installation receipt is invalid")?;
        validate_source_receipt_shape(&receipt)?;
        Ok(AdapterInstallationReceipt::Source {
            receipt,
            source_root: PathBuf::new(),
            source_sha256: String::new(),
            source_changed: false,
        })
    } else {
        Ok(AdapterInstallationReceipt::Release(
            serde_json::from_value(value).context("release installation receipt is invalid")?,
        ))
    }
}

fn validate_source_receipt_shape(receipt: &SourceInstallationReceipt) -> anyhow::Result<()> {
    anyhow::ensure!(
        receipt.schema_version == 1,
        "unsupported source installation receipt schema {}; expected 1",
        receipt.schema_version
    );
    anyhow::ensure!(
        receipt.install_kind == "local_source",
        "source installation receipt kind must be `local_source`"
    );
    anyhow::ensure!(
        !receipt.verified_release,
        "local source receipt must not claim verified release provenance"
    );
    anyhow::ensure!(
        !receipt.ownership.source_checkout_owned,
        "local source receipt must not claim ownership of the source checkout"
    );
    anyhow::ensure!(
        receipt.ownership.generated_paths == ["source-target"],
        "source receipt generated paths must be exactly `source-target`"
    );
    let namespace = receipt
        .source
        .package
        .strip_prefix("ldgr-")
        .unwrap_or(&receipt.source.package)
        .strip_suffix("-adapter")
        .unwrap_or_else(|| {
            receipt
                .source
                .package
                .strip_prefix("ldgr-")
                .unwrap_or(&receipt.source.package)
        });
    anyhow::ensure!(
        namespace == receipt.domain,
        "source receipt package `{}` does not own adapter `{}`",
        receipt.source.package,
        receipt.domain
    );
    let installed_manifest = receipt
        .installed_files
        .iter()
        .find(|file| file.path == "adapter.toml")
        .context("source receipt must track installed adapter.toml")?;
    anyhow::ensure!(
        installed_manifest.sha256 == receipt.manifest_digests.installed_adapter_manifest_sha256,
        "source receipt installed adapter manifest digests disagree"
    );
    if let Some(expected) = &receipt.manifest_digests.installed_resource_manifest_sha256 {
        let installed_resource_manifest = receipt
            .installed_files
            .iter()
            .find(|file| file.path == "adapter-resources.json")
            .context("source receipt resource digest has no installed resource manifest file")?;
        anyhow::ensure!(
            &installed_resource_manifest.sha256 == expected,
            "source receipt installed resource manifest digests disagree"
        );
    }
    Ok(())
}

fn platform_tag() -> anyhow::Result<String> {
    let arch = match std::env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("unsupported adapter release architecture `{other}`"),
    };
    match std::env::consts::OS {
        "linux" => Ok(format!("linux-{arch}")),
        "macos" => Ok(format!("macos-{arch}")),
        "windows" => Ok(format!("windows-{arch}")),
        other => bail!("unsupported adapter release OS `{other}`"),
    }
}

fn home_dir() -> anyhow::Result<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .ok_or_else(|| anyhow::anyhow!("could not determine home directory from HOME/USERPROFILE"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_index::{
        AdapterClassification, AdapterReleaseIndex, OwnedResource, ReleaseChannel,
        SourceInstallIdentity, SourceManifestDigests, SourceOwnershipBoundaries,
    };

    fn release_receipt(version: &str) -> InstallationReceipt {
        InstallationReceipt {
            schema_version: 1,
            domain: "fixture".to_owned(),
            version: version.to_owned(),
            source_url: "file:///old.tar.gz".to_owned(),
            sha256: "0".repeat(64),
            signing_key_id: "test".to_owned(),
            core_compatibility: ">=0.1.0, <0.3.0".to_owned(),
            compatibility: None,
            compatibility_sha256: None,
            platform: "test-platform".to_owned(),
            resource_manifest: "adapter-resources.json".to_owned(),
            installed_at_unix_seconds: 0,
            bundle_sha256: "0".repeat(64),
            binary_path: None,
            binary_sha256: None,
            owned_resources: Vec::new(),
        }
    }

    #[test]
    fn release_receipt_schema_matches_compatibility_generation() -> anyhow::Result<()> {
        let legacy = release_receipt("1.0.0");
        validate_release_receipt_schema(&legacy)?;

        let mut compatibility_v2 = legacy.clone();
        compatibility_v2.schema_version = 2;
        compatibility_v2.core_compatibility.clear();
        compatibility_v2.compatibility = Some(serde_json::from_value(serde_json::json!({
            "adapter_protocol_epoch": 1,
            "minimum_core_schema": 5,
            "required_core_capabilities": ["work.v1"],
            "central_components": []
        }))?);
        compatibility_v2.compatibility_sha256 = Some(format!("sha256:{}", "0".repeat(64)));
        validate_release_receipt_schema(&compatibility_v2)?;

        compatibility_v2.schema_version = 1;
        assert!(validate_release_receipt_schema(&compatibility_v2).is_err());
        Ok(())
    }

    fn release_index() -> AdapterReleaseIndex {
        let platform = |version: &str| AdapterPlatformRelease {
            platform: "test-platform".to_owned(),
            asset_url: format!("file:///fixture-{version}.tar.gz"),
            archive_root: format!("fixture-{version}"),
            binary: "ldgr-fixture".to_owned(),
            sha256: "0".repeat(64),
            signature_url: format!("file:///fixture-{version}.sig"),
            signing_key_id: "test".to_owned(),
            resource_manifest: "adapter-resources.json".to_owned(),
        };
        AdapterReleaseIndex {
            schema_version: 1,
            adapters: vec![AdapterReleaseProduct {
                domain: "fixture".to_owned(),
                primary_namespace: "fixture".to_owned(),
                title: "Fixture".to_owned(),
                aliases: Vec::new(),
                classification: AdapterClassification::OpenSource,
                source_url: None,
                releases: vec![
                    AdapterRelease {
                        version: "2.0.0-beta.1".to_owned(),
                        channel: ReleaseChannel::Prerelease,
                        core_compatibility: ">=0.2.0, <0.3.0".to_owned(),
                        compatibility: None,
                        compatibility_sha256: None,
                        platforms: vec![platform("2.0.0-beta.1")],
                    },
                    AdapterRelease {
                        version: "1.5.0".to_owned(),
                        channel: ReleaseChannel::Stable,
                        core_compatibility: ">=0.2.0, <0.3.0".to_owned(),
                        compatibility: None,
                        compatibility_sha256: None,
                        platforms: vec![platform("1.5.0")],
                    },
                ],
            }],
        }
    }

    #[test]
    fn transaction_rolls_back_every_snapshot_when_not_committed() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let existing = root.path().join("existing");
        let created = root.path().join("created");
        fs::write(&existing, "before")?;
        {
            let mut transaction = InstallTransaction::new(root.path().join("rollback"))?;
            transaction.snapshot(&existing)?;
            transaction.snapshot(&created)?;
            fs::write(&existing, "after")?;
            fs::write(&created, "created")?;
        }
        assert_eq!(fs::read_to_string(existing)?, "before");
        assert!(!created.exists());
        Ok(())
    }

    #[test]
    fn planner_resolves_channel_against_target_core_version() -> anyhow::Result<()> {
        let index = release_index();
        let current_core = Version::parse("0.1.9")?;
        let target_core = Version::parse("0.2.0")?;
        assert!(plan_release_update(
            "fixture".to_owned(),
            PathBuf::from("fixture-install"),
            PathBuf::from("fixture-home"),
            release_receipt("1.0.0"),
            &current_core,
            UpdateChannel::Stable,
            &index,
            "test-platform",
        )
        .is_err());

        let stable = plan_release_update(
            "fixture".to_owned(),
            PathBuf::from("fixture-install"),
            PathBuf::from("fixture-home"),
            release_receipt("1.0.0"),
            &target_core,
            UpdateChannel::Stable,
            &index,
            "test-platform",
        )?;
        assert!(stable.update_available());
        assert!(matches!(
            stable,
            AdapterUpdatePlan::Release { target_version, .. }
                if target_version == Version::parse("1.5.0")?
        ));

        let prerelease = plan_release_update(
            "fixture".to_owned(),
            PathBuf::from("fixture-install"),
            PathBuf::from("fixture-home"),
            release_receipt("1.0.0"),
            &target_core,
            UpdateChannel::Prerelease,
            &index,
            "test-platform",
        )?;
        assert!(matches!(
            prerelease,
            AdapterUpdatePlan::Release { target_version, .. }
                if target_version == Version::parse("2.0.0-beta.1")?
        ));
        Ok(())
    }

    fn source_receipt(verified_release: bool) -> SourceInstallationReceipt {
        SourceInstallationReceipt {
            schema_version: 1,
            install_kind: "local_source".to_owned(),
            domain: "fixture".to_owned(),
            installed_at_unix_seconds: 0,
            source: SourceInstallIdentity {
                package: "ldgr-fixture-adapter".to_owned(),
                bundle_root: "C:/source".to_owned(),
                cargo_manifest: "C:/source/Cargo.toml".to_owned(),
                bundle_sha256: "digest".to_owned(),
            },
            manifest_digests: SourceManifestDigests {
                source_adapter_manifest_sha256: "source".to_owned(),
                source_cargo_manifest_sha256: "cargo".to_owned(),
                installed_adapter_manifest_sha256: "installed".to_owned(),
                source_resource_manifest_sha256: None,
                installed_resource_manifest_sha256: None,
            },
            installer_invocation: Vec::new(),
            executable_invocations: Vec::new(),
            installed_files: vec![OwnedResource {
                path: "adapter.toml".to_owned(),
                sha256: "installed".to_owned(),
            }],
            owned_resources: Vec::new(),
            ownership: SourceOwnershipBoundaries {
                install_root: "C:/installed".to_owned(),
                marker_path: "C:/home/.ldgr/installed-adapters/fixture".to_owned(),
                source_checkout_owned: false,
                generated_paths: vec!["source-target".to_owned()],
                external_resource_roots: Vec::new(),
            },
            verified_release,
        }
    }

    #[test]
    fn local_source_plan_preserves_provenance_and_channel_rules() -> anyhow::Result<()> {
        assert!(validate_source_receipt_shape(&source_receipt(true))
            .unwrap_err()
            .to_string()
            .contains("must not claim verified release provenance"));
        let inspection = |changed| AdapterInstallationInspection {
            slug: "fixture".to_owned(),
            install_root: PathBuf::from("C:/installed"),
            home: PathBuf::from("C:/home"),
            receipt: AdapterInstallationReceipt::Source {
                receipt: source_receipt(false),
                source_root: PathBuf::from("C:/source"),
                source_sha256: "planned".to_owned(),
                source_changed: changed,
            },
        };
        let stable = plan_adapter_update(
            inspection(false),
            &Version::parse("0.2.0")?,
            UpdateChannel::Stable,
        )?;
        assert!(!stable.update_available());
        assert!(stable.should_apply_for_single_adapter_command());
        assert!(plan_adapter_update(
            inspection(true),
            &Version::parse("0.2.0")?,
            UpdateChannel::Prerelease,
        )
        .unwrap_err()
        .to_string()
        .contains("applies only to signed release adapters"));
        Ok(())
    }
}
