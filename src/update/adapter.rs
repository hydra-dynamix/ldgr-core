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
use crate::update::plan::AdapterInstallationKind;

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

pub(crate) fn snapshot_adapter_installation(
    installed: &DiscoveredAdapter,
    home: &Path,
) -> anyhow::Result<AdapterInstallationKind> {
    let value = installed
        .installation_receipt
        .clone()
        .context("installed adapter has no tracked installation receipt")?;
    match parse_installation_receipt(value)? {
        AdapterInstallationReceipt::Release(receipt) => {
            anyhow::ensure!(
                receipt.schema_version == 1,
                "unsupported release receipt schema"
            );
            anyhow::ensure!(
                receipt.domain == installed.slug,
                "release receipt domain does not match the discovered adapter"
            );
            Version::parse(&receipt.version).context("release receipt version is invalid")?;
            VersionReq::parse(&receipt.core_compatibility)
                .context("release receipt Core compatibility is invalid")?;
            crate::cli::commands::ops::inspect_release_installation_for_update(
                &installed.root_path,
                home,
                &receipt,
            )?;
            Ok(AdapterInstallationKind::Release {
                version: receipt.version,
                core_compatibility: receipt.core_compatibility,
            })
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
            Ok(AdapterInstallationKind::LocalSource {
                package: receipt.source.package,
                installed_source_sha256: receipt.source.bundle_sha256,
                current_source_sha256,
                source_changed,
            })
        }
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
            platform: "test-platform".to_owned(),
            resource_manifest: "adapter-resources.json".to_owned(),
            installed_at_unix_seconds: 0,
            bundle_sha256: "0".repeat(64),
            binary_path: None,
            binary_sha256: None,
            owned_resources: Vec::new(),
        }
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
                        platforms: vec![platform("2.0.0-beta.1")],
                    },
                    AdapterRelease {
                        version: "1.5.0".to_owned(),
                        channel: ReleaseChannel::Stable,
                        core_compatibility: ">=0.2.0, <0.3.0".to_owned(),
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
