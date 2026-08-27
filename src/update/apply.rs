use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File};
use std::io::{ErrorKind, Write};
use std::path::{Component, Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, ensure, Context};
use semver::Version;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::release_index::{
    parse_detached_signature, verify_detached_signature_bytes, verify_file_sha256_for,
    AdapterReleaseIndex, InstallationReceipt, SourceInstallationReceipt,
};

use super::catalog::{
    extract_bound_core_archive, verify_resolved_core_archive_signature, ResolvedCoreRelease,
    VerifiedAdapterUpdateCatalog, VerifiedCoreUpdateCatalog,
};
use super::installation::{
    core_installation_receipt_path, validate_receipt, CompatibilityProbe, CoreArchiveProvenance,
    CoreInstallationReceipt, CoreInstallerKind, ProcessCompatibilityProbe,
};
use super::network::{UpdateNetworkClient, MAX_UPDATE_ARTIFACT_BYTES, MAX_UPDATE_SIGNATURE_BYTES};
use super::plan::{AdapterReleaseArtifact, UpdateAction, UpdatePlan, UpdatePlanComponent};
use super::state::{atomic_json, ComponentResult, UpdateLock, UpdateStateStore};

const JOURNAL_SCHEMA_VERSION: u32 = 1;
const JOURNAL_FILE: &str = "journal.json";
const SNAPSHOTS_DIRECTORY: &str = "snapshots";
const STAGING_MANIFEST: &str = "staging-manifest.json";

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnedTarget {
    pub component: String,
    pub role: String,
    pub boundary: PathBuf,
    pub path: PathBuf,
}

impl OwnedTarget {
    pub fn new(
        component: impl Into<String>,
        role: impl Into<String>,
        boundary: impl Into<PathBuf>,
        path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            component: component.into(),
            role: role.into(),
            boundary: boundary.into(),
            path: path.into(),
        }
    }
}

pub struct VerifiedStagingCatalogs<'a> {
    pub core: &'a VerifiedCoreUpdateCatalog,
    pub adapters: &'a VerifiedAdapterUpdateCatalog,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", content = "receipt", rename_all = "snake_case")]
pub enum AdapterOwnershipReceipt {
    Release(InstallationReceipt),
    LocalSource(SourceInstallationReceipt),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AdapterStagingOwnership {
    pub install_root: PathBuf,
    pub user_adapter_root: PathBuf,
    pub receipt: AdapterOwnershipReceipt,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PlanStagingOwnership {
    pub home: PathBuf,
    pub core: Option<CoreInstallationReceipt>,
    pub adapters: BTreeMap<String, AdapterStagingOwnership>,
    /// Every discovered install root, including retained, blocked, project,
    /// override, and malformed candidates. Used for candidate-profile race
    /// checks before and after activation.
    #[serde(default)]
    pub adapter_roots: BTreeMap<String, PathBuf>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum StagedArtifact {
    CoreBundle {
        name: String,
        archive: PathBuf,
        signature: PathBuf,
        extracted_root: PathBuf,
        core_binary: PathBuf,
        agentctl_binary: PathBuf,
    },
    AdapterRelease {
        name: String,
        archive: PathBuf,
        signature: PathBuf,
        extracted_root: PathBuf,
    },
    LocalSource {
        name: String,
        source_root: PathBuf,
        source_sha256: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct StagingManifest {
    pub schema_version: u32,
    pub plan_id: String,
    pub platform: String,
    pub artifacts: Vec<StagedArtifact>,
    pub targets: Vec<OwnedTarget>,
}

pub struct StagedUpdatePlan {
    pub manifest: StagingManifest,
    pub transaction: InstallTransaction,
}

#[derive(Debug)]
pub(crate) struct UpdateApplyError {
    pub(crate) source: anyhow::Error,
    pub(crate) components: Vec<ComponentResult>,
}

impl std::fmt::Display for UpdateApplyError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{:#}", self.source)
    }
}

impl std::error::Error for UpdateApplyError {}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ActivationStep {
    DestinationStaged,
    AgentctlBackup,
    CoreBackup,
    AgentctlActivated,
    CoreActivated,
    PairValidated,
    ReceiptActivated,
    AdaptersActivated,
    AdapterDiscoveryValidated,
}

trait ActivationHook {
    fn after(&self, _step: ActivationStep) -> anyhow::Result<()> {
        Ok(())
    }
}

struct NoopActivationHook;

impl ActivationHook for NoopActivationHook {}

pub(crate) fn apply_staged_update_plan(
    plan: &UpdatePlan,
    manifest: &StagingManifest,
    adapter_catalog: &VerifiedAdapterUpdateCatalog,
    ownership: &PlanStagingOwnership,
    transaction: &mut InstallTransaction,
    quiet: bool,
) -> Result<Vec<ComponentResult>, UpdateApplyError> {
    #[cfg(not(any(unix, windows)))]
    let core_selected = selected_core_component(plan).is_some();
    #[cfg(not(any(unix, windows)))]
    if core_selected {
        return Err(rollback_apply_failure(
            transaction,
            anyhow::anyhow!(
                "update.apply-platform-unavailable: synchronous Core activation requires Unix"
            ),
            failed_plan_components(plan),
        ));
    }
    let probe = ProcessCompatibilityProbe;
    apply_staged_update_plan_with_services(
        plan,
        manifest,
        adapter_catalog,
        ownership,
        transaction,
        quiet,
        &probe,
        &NoopActivationHook,
    )
}

#[allow(clippy::too_many_arguments)]
fn apply_staged_update_plan_with_services(
    plan: &UpdatePlan,
    manifest: &StagingManifest,
    adapter_catalog: &VerifiedAdapterUpdateCatalog,
    ownership: &PlanStagingOwnership,
    transaction: &mut InstallTransaction,
    quiet: bool,
    probe: &dyn CompatibilityProbe,
    hook: &dyn ActivationHook,
) -> Result<Vec<ComponentResult>, UpdateApplyError> {
    let mut components = Vec::new();
    if let Some(component) = plan
        .components()
        .iter()
        .find(|component| component.kind() == super::plan::UpdateComponentKind::CoreBundle)
    {
        if component.action() == UpdateAction::Update {
            match apply_core_bundle(plan, manifest, ownership, transaction, probe, hook) {
                Ok(result) => components.push(result),
                Err(error) => {
                    return Err(rollback_apply_failure(
                        transaction,
                        error,
                        failed_plan_components(plan),
                    ));
                }
            }
        } else {
            components.push(component_result(component, action_name(component.action())));
        }
    }

    let adapter_components = match crate::update::adapter::apply_staged_adapter_updates(
        plan,
        manifest,
        adapter_catalog,
        ownership,
        transaction,
        quiet,
    ) {
        Ok(results) => results,
        Err(failure) => {
            for result in &mut components {
                if result.status == "applied" {
                    result.status = "rolled_back".to_owned();
                }
            }
            components.extend(failure.components);
            return Err(UpdateApplyError {
                source: failure.source,
                components,
            });
        }
    };
    components.extend(adapter_components);

    if let Err(error) = hook.after(ActivationStep::AdaptersActivated) {
        return Err(rollback_apply_failure(
            transaction,
            error,
            rolled_back_components(components),
        ));
    }
    if let Err(error) = validate_adapter_discovery(plan, ownership) {
        return Err(rollback_apply_failure(
            transaction,
            error,
            rolled_back_components(components),
        ));
    }
    if let Err(error) = hook.after(ActivationStep::AdapterDiscoveryValidated) {
        return Err(rollback_apply_failure(
            transaction,
            error,
            rolled_back_components(components),
        ));
    }
    Ok(components)
}

fn selected_core_component(
    plan: &UpdatePlan,
) -> Option<(&str, &str, &str, &super::plan::CoreBundleArtifact)> {
    plan.components().iter().find_map(|component| {
        let UpdatePlanComponent::CoreBundle {
            name,
            target,
            target_agentctl,
            action: UpdateAction::Update,
            artifact: Some(artifact),
            ..
        } = component
        else {
            return None;
        };
        Some((
            name.as_str(),
            target.as_str(),
            target_agentctl.as_str(),
            artifact,
        ))
    })
}

fn apply_core_bundle(
    plan: &UpdatePlan,
    manifest: &StagingManifest,
    ownership: &PlanStagingOwnership,
    transaction: &mut InstallTransaction,
    probe: &dyn CompatibilityProbe,
    hook: &dyn ActivationHook,
) -> anyhow::Result<ComponentResult> {
    let (name, target_core, target_agentctl, artifact) =
        selected_core_component(plan).context("selected Core update has incomplete metadata")?;
    let receipt = ownership
        .core
        .as_ref()
        .context("selected Core update has no installation receipt")?;
    validate_receipt(receipt)?;
    ensure!(
        receipt.installer_kind == CoreInstallerKind::Official,
        "only an official receipt-managed Core installation may self-update"
    );
    let staged = manifest
        .artifacts
        .iter()
        .find(|staged| matches!(staged, StagedArtifact::CoreBundle { name: staged_name, .. } if staged_name == name))
        .context("selected Core update has no staged bundle")?;
    let StagedArtifact::CoreBundle {
        core_binary,
        agentctl_binary,
        ..
    } = staged
    else {
        unreachable!();
    };
    let destination_staging = DestinationStagedPair::prepare(
        plan.plan_id(),
        core_binary,
        agentctl_binary,
        &receipt.core_binary_path,
        &receipt.agentctl_binary_path,
    )?;
    hook.after(ActivationStep::DestinationStaged)?;

    let agentctl_backup = previous_path(&receipt.agentctl_binary_path)?;
    let core_backup = previous_path(&receipt.core_binary_path)?;
    transaction.backup_file(&receipt.agentctl_binary_path, &agentctl_backup)?;
    hook.after(ActivationStep::AgentctlBackup)?;
    transaction.backup_file(&receipt.core_binary_path, &core_backup)?;
    hook.after(ActivationStep::CoreBackup)?;
    transaction.activate_file(&destination_staging.agentctl, &receipt.agentctl_binary_path)?;
    hook.after(ActivationStep::AgentctlActivated)?;
    transaction.activate_file(&destination_staging.core, &receipt.core_binary_path)?;
    hook.after(ActivationStep::CoreActivated)?;

    let evidence = probe
        .probe(&receipt.core_binary_path, &receipt.agentctl_binary_path)
        .context("absolute-path Core/agentctl compatibility validation failed")?;
    ensure!(
        evidence.core_version == target_core
            && evidence.core_version == artifact.version
            && evidence.agentctl_version == target_agentctl
            && evidence.agentctl_version == artifact.agentctl_version
            && evidence.compatibility_schema
                == artifact.release.compatibility.launcher_compatibility_schema,
        "installed Core/agentctl evidence differs from the resolved release"
    );
    hook.after(ActivationStep::PairValidated)?;

    let updated_receipt = updated_core_receipt(plan, receipt, artifact)?;
    let receipt_target = core_installation_receipt_path(&ownership.home);
    let receipt_staged = destination_staged_path(&receipt_target, plan.plan_id(), "receipt")?;
    remove_path_if_exists(&receipt_staged)?;
    atomic_json(&receipt_staged, &updated_receipt)?;
    let activation = transaction.activate_file(&receipt_staged, &receipt_target);
    let cleanup = remove_path_if_exists(&receipt_staged);
    activation?;
    cleanup?;
    hook.after(ActivationStep::ReceiptActivated)?;
    Ok(ComponentResult {
        kind: "core_bundle".to_owned(),
        name: name.to_owned(),
        status: "applied".to_owned(),
    })
}

fn updated_core_receipt(
    plan: &UpdatePlan,
    receipt: &CoreInstallationReceipt,
    artifact: &super::plan::CoreBundleArtifact,
) -> anyhow::Result<CoreInstallationReceipt> {
    let updated = CoreInstallationReceipt {
        schema_version: receipt.schema_version,
        installer_kind: CoreInstallerKind::Official,
        managed_by: None,
        core_version: artifact.version.clone(),
        agentctl_version: artifact.agentctl_version.clone(),
        archive: Some(CoreArchiveProvenance {
            url: artifact.platform.archive_url.clone(),
            sha256: artifact.platform.sha256.clone(),
            signing_key_id: artifact.platform.signing_key_id.clone(),
            platform: artifact.platform.platform.clone(),
            release_commit: artifact.release.core_commit.clone(),
        }),
        install_root: receipt.install_root.clone(),
        core_binary_path: receipt.core_binary_path.clone(),
        agentctl_binary_path: receipt.agentctl_binary_path.clone(),
        core_binary_sha256: file_sha256(&receipt.core_binary_path)?,
        agentctl_binary_sha256: file_sha256(&receipt.agentctl_binary_path)?,
        compatibility_schema: artifact
            .release
            .compatibility
            .launcher_compatibility_schema
            .clone(),
        previous_successful_plan_id: Some(plan.plan_id().to_owned()),
        installed_at_unix_seconds: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .context("system clock is before Unix epoch")?
            .as_secs(),
    };
    validate_receipt(&updated)?;
    Ok(updated)
}

struct DestinationStagedPair {
    core: PathBuf,
    agentctl: PathBuf,
}

impl DestinationStagedPair {
    fn prepare(
        plan_id: &str,
        core_source: &Path,
        agentctl_source: &Path,
        core_target: &Path,
        agentctl_target: &Path,
    ) -> anyhow::Result<Self> {
        let core = destination_staged_path(core_target, plan_id, "core")?;
        let agentctl = destination_staged_path(agentctl_target, plan_id, "agentctl")?;
        remove_path_if_exists(&core)?;
        remove_path_if_exists(&agentctl)?;
        copy_executable(core_source, &core)?;
        if let Err(error) = copy_executable(agentctl_source, &agentctl) {
            let cleanup = remove_path_if_exists(&core);
            if let Err(cleanup) = cleanup {
                return Err(anyhow::anyhow!(
                    "{error:#}; destination staging cleanup failed: {cleanup:#}"
                ));
            }
            return Err(error);
        }
        Ok(Self { core, agentctl })
    }
}

impl Drop for DestinationStagedPair {
    fn drop(&mut self) {
        let _ = remove_path_if_exists(&self.core);
        let _ = remove_path_if_exists(&self.agentctl);
    }
}

fn destination_staged_path(target: &Path, plan_id: &str, role: &str) -> anyhow::Result<PathBuf> {
    validate_plan_id(plan_id)?;
    let parent = target.parent().context("activation target has no parent")?;
    let name = target
        .file_name()
        .context("activation target has no file name")?;
    let mut staged = std::ffi::OsString::from(format!(".ldgr-update-{plan_id}-{role}-"));
    staged.push(name);
    staged.push(".staged");
    Ok(parent.join(staged))
}

fn previous_path(target: &Path) -> anyhow::Result<PathBuf> {
    let name = target.file_name().context("Core binary has no file name")?;
    let mut previous = name.to_os_string();
    previous.push(".previous");
    Ok(target
        .parent()
        .context("Core binary has no parent")?
        .join(previous))
}

fn copy_executable(source: &Path, destination: &Path) -> anyhow::Result<()> {
    copy_file_synced(source, destination)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(destination)?.permissions();
        permissions.set_mode(permissions.mode() | 0o111);
        fs::set_permissions(destination, permissions)?;
        File::open(destination)?.sync_all()?;
    }
    sync_directory(
        destination
            .parent()
            .context("destination-staged binary has no parent")?,
    )
}

fn file_sha256(path: &Path) -> anyhow::Result<String> {
    let mut file = File::open(path)?;
    let mut digest = Sha256::new();
    std::io::copy(&mut file, &mut digest)?;
    Ok(format!("{:x}", digest.finalize()))
}

fn validate_adapter_discovery(
    plan: &UpdatePlan,
    ownership: &PlanStagingOwnership,
) -> anyhow::Result<()> {
    let adapters = plan
        .components()
        .iter()
        .filter(|component| {
            matches!(component, UpdatePlanComponent::Adapter { .. })
                && matches!(
                    component.action(),
                    UpdateAction::None | UpdateAction::Update | UpdateAction::ReinstallLocalSource
                )
        })
        .collect::<Vec<_>>();
    if adapters.is_empty() {
        return Ok(());
    }
    let mut roots = BTreeSet::new();
    for component in &adapters {
        if let Some(root) = ownership.adapter_roots.get(component.name()) {
            roots.insert(root.clone());
        } else if let Some(owned) = ownership.adapters.get(component.name()) {
            roots.insert(owned.install_root.clone());
        }
    }
    let registry = if let Some(candidate) = plan.candidate_adapter_compatibility() {
        crate::adapter_registry::AdapterRegistry::discover_from_roots_with_profiles(
            roots,
            &candidate.profile,
            &candidate.projected_database_components,
            &candidate.legacy_profile,
        )
    } else {
        crate::adapter_registry::AdapterRegistry::discover_from_roots(roots)
    };
    for component in adapters {
        let expected_root = ownership
            .adapter_roots
            .get(component.name())
            .or_else(|| {
                ownership
                    .adapters
                    .get(component.name())
                    .map(|owned| &owned.install_root)
            })
            .with_context(|| format!("adapter `{}` lost its inventoried root", component.name()))?;
        ensure!(
            !registry
                .warnings
                .iter()
                .any(|warning| warning.manifest_path.starts_with(expected_root)),
            "adapter `{}` emitted a candidate-profile discovery warning",
            component.name()
        );
        let discovered = registry.find(component.name()).with_context(|| {
            format!(
                "adapter `{}` is not discoverable after candidate activation",
                component.name()
            )
        })?;
        ensure!(
            discovered.state.permits_dispatch(),
            "adapter `{}` is {} against the candidate Core after activation: {}",
            component.name(),
            discovered.state,
            discovered
                .reasons
                .first()
                .map(|reason| reason.message.as_str())
                .unwrap_or("compatibility could not be proved")
        );
        ensure!(
            paths_equal(&discovered.root_path, expected_root),
            "adapter `{}` resolved from a different installation root",
            component.name()
        );
    }
    Ok(())
}

fn component_result(component: &UpdatePlanComponent, status: &str) -> ComponentResult {
    ComponentResult {
        kind: match component {
            UpdatePlanComponent::CoreBundle { .. } => "core_bundle",
            UpdatePlanComponent::Adapter { .. } => "adapter",
        }
        .to_owned(),
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

fn failed_plan_components(plan: &UpdatePlan) -> Vec<ComponentResult> {
    plan.components()
        .iter()
        .map(|component| {
            let status = if matches!(
                component.action(),
                UpdateAction::Update | UpdateAction::ReinstallLocalSource
            ) {
                "failed"
            } else {
                action_name(component.action())
            };
            component_result(component, status)
        })
        .collect()
}

fn rolled_back_components(mut components: Vec<ComponentResult>) -> Vec<ComponentResult> {
    for component in &mut components {
        if component.status == "applied" {
            component.status = "rolled_back".to_owned();
        }
    }
    components
}

fn rollback_apply_failure(
    transaction: &mut InstallTransaction,
    error: anyhow::Error,
    components: Vec<ComponentResult>,
) -> UpdateApplyError {
    let source = match transaction.rollback() {
        Ok(()) => error,
        Err(rollback_error) => {
            anyhow::anyhow!("{error:#}; whole-plan rollback also failed: {rollback_error:#}")
        }
    };
    UpdateApplyError { source, components }
}

/// Downloads and authenticates every selected artifact before taking the first
/// destination snapshot. Catalog membership is checked for the complete plan
/// before network or filesystem artifact work begins. The returned transaction
/// is sealed: activation cannot add a late, unsnapshotted destination.
pub fn stage_verified_update_plan(
    store: &UpdateStateStore,
    lock: &UpdateLock,
    client: &UpdateNetworkClient,
    plan: &UpdatePlan,
    catalogs: &VerifiedStagingCatalogs<'_>,
    ownership: &PlanStagingOwnership,
) -> anyhow::Result<StagedUpdatePlan> {
    plan.verify_plan_id()
        .context("update.artifact-untrusted: resolved plan digest is invalid")?;
    ensure!(
        !plan.blocked(),
        "update.no-compatible-release: blocked plans cannot be staged"
    );
    validate_complete_catalog_binding(plan, catalogs)
        .context("update.artifact-untrusted: resolved plan is not bound to verified catalogs")?;
    let plan_id = store.stage_update_plan(lock, plan)?;
    ensure!(
        plan_id == plan.plan_id(),
        "resolved plan id changed during staging"
    );
    let stage_root = store.stage_dir(&plan_id)?;
    let artifact_root = stage_root.join("artifacts");
    reject_existing_link_ancestors(&artifact_root, Some(&stage_root))?;
    fs::create_dir_all(&artifact_root)?;

    let mut artifacts = Vec::new();
    let mut targets = Vec::new();
    for component in plan.components() {
        match component {
            UpdatePlanComponent::CoreBundle {
                name,
                action: UpdateAction::Update,
                artifact: Some(artifact),
                ..
            } => {
                let receipt = ownership.core.as_ref().with_context(|| {
                    format!("missing receipt-managed ownership for Core component `{name}`")
                })?;
                let staged = stage_core_artifact(
                    client,
                    &artifact_root,
                    name,
                    plan.platform(),
                    artifact,
                    catalogs.core,
                )?;
                targets.extend(core_targets(&ownership.home, receipt)?);
                artifacts.push(staged);
            }
            UpdatePlanComponent::Adapter {
                name,
                action: UpdateAction::Update,
                release: Some(release),
                ..
            } => {
                let owned = ownership.adapters.get(name).with_context(|| {
                    format!("missing receipt-managed ownership for adapter `{name}`")
                })?;
                let staged =
                    stage_adapter_artifact(client, &artifact_root, plan, name, release, catalogs)?;
                let StagedArtifact::AdapterRelease { extracted_root, .. } = &staged else {
                    unreachable!();
                };
                targets.extend(adapter_release_targets(
                    &ownership.home,
                    name,
                    release,
                    extracted_root,
                    owned,
                )?);
                artifacts.push(staged);
            }
            UpdatePlanComponent::Adapter {
                name,
                action: UpdateAction::ReinstallLocalSource,
                local_source: Some(source),
                ..
            } => {
                let owned = ownership.adapters.get(name).with_context(|| {
                    format!("missing receipt-managed ownership for adapter `{name}`")
                })?;
                let (staged, source_targets) =
                    stage_local_source(&ownership.home, name, source, owned)?;
                artifacts.push(staged);
                targets.extend(source_targets);
            }
            component
                if matches!(
                    component.action(),
                    UpdateAction::Update | UpdateAction::ReinstallLocalSource
                ) =>
            {
                bail!(
                    "selected component `{}` has incomplete artifact metadata",
                    component.name()
                );
            }
            _ => {}
        }
    }
    ensure!(
        !artifacts.is_empty(),
        "resolved update plan contains no selected artifacts"
    );
    validate_retained_adapter_preflight(plan, ownership).context(
        "update.compatibility-preflight-failed: retained adapter changed during staging",
    )?;
    targets = coalesce_staging_targets(targets)?;
    targets.sort_by(|left, right| {
        path_key(&left.path)
            .cmp(&path_key(&right.path))
            .then_with(|| left.component.cmp(&right.component))
            .then_with(|| left.role.cmp(&right.role))
    });
    validate_target_manifest(&targets)?;
    verify_staging_capacity(&stage_root, &artifacts, &targets)?;

    let transaction = InstallTransaction::prepare(stage_root.join("rollback"), &plan_id, &targets)?;
    let manifest = StagingManifest {
        schema_version: JOURNAL_SCHEMA_VERSION,
        plan_id,
        platform: plan.platform().to_owned(),
        artifacts,
        targets,
    };
    let manifest_path = stage_root.join(STAGING_MANIFEST);
    if manifest_path.exists() {
        let existing: StagingManifest = serde_json::from_reader(File::open(&manifest_path)?)?;
        ensure!(
            existing == manifest,
            "deterministic staging manifest changed for the same plan id"
        );
    } else {
        atomic_json(&manifest_path, &manifest)?;
    }
    Ok(StagedUpdatePlan {
        manifest,
        transaction,
    })
}

fn validate_retained_adapter_preflight(
    plan: &UpdatePlan,
    ownership: &PlanStagingOwnership,
) -> anyhow::Result<()> {
    let Some(candidate) = plan.candidate_adapter_compatibility() else {
        return Ok(());
    };
    for component in plan.components().iter().filter(|component| {
        matches!(component, UpdatePlanComponent::Adapter { .. })
            && component.action() == UpdateAction::None
    }) {
        let root = ownership
            .adapter_roots
            .get(component.name())
            .with_context(|| {
                format!(
                    "retained adapter `{}` lost its inventoried root",
                    component.name()
                )
            })?;
        let registry = crate::adapter_registry::AdapterRegistry::discover_from_roots_with_profiles(
            [root.clone()],
            &candidate.profile,
            &candidate.projected_database_components,
            &candidate.legacy_profile,
        );
        let discovered = registry.find(component.name()).with_context(|| {
            format!(
                "retained adapter `{}` is no longer discoverable",
                component.name()
            )
        })?;
        ensure!(
            discovered.state.permits_dispatch() && paths_equal(&discovered.root_path, root),
            "retained adapter `{}` no longer passes the candidate Core profile: {}",
            component.name(),
            discovered
                .reasons
                .first()
                .map(|reason| reason.message.as_str())
                .unwrap_or("installation identity changed")
        );
    }
    Ok(())
}

fn verify_staging_capacity(
    stage_root: &Path,
    artifacts: &[StagedArtifact],
    targets: &[OwnedTarget],
) -> anyhow::Result<()> {
    let archive_bytes = artifacts.iter().try_fold(0_u64, |total, artifact| {
        let archive = match artifact {
            StagedArtifact::CoreBundle { archive, .. }
            | StagedArtifact::AdapterRelease { archive, .. } => Some(archive),
            StagedArtifact::LocalSource { .. } => None,
        };
        let bytes = archive
            .map(fs::metadata)
            .transpose()?
            .map_or(0, |value| value.len());
        total
            .checked_add(bytes)
            .context("staged artifact size overflow")
    })?;
    let snapshot_bytes = targets.iter().try_fold(0_u64, |total, target| {
        total
            .checked_add(path_size(&target.path)?)
            .context("update snapshot size overflow")
    })?;
    let required = archive_bytes
        .checked_mul(2)
        .and_then(|bytes| bytes.checked_add(snapshot_bytes))
        .context("update staging space estimate overflow")?;
    if let Some(available) = available_space(stage_root)? {
        ensure!(
            available >= required,
            "update.download-failed: staging requires {required} bytes but only {available} are available"
        );
    }
    let mut checked = BTreeSet::new();
    for target in targets {
        let boundary = absolute_lexical(&target.boundary)?;
        if checked.insert(path_key(&boundary)) {
            if let Some(available) = available_space(&boundary)? {
                ensure!(
                    available >= archive_bytes,
                    "update.activation-failed: destination filesystem for {} lacks space for staged activation",
                    boundary.display()
                );
            }
        }
    }
    Ok(())
}

fn path_size(path: &Path) -> anyhow::Result<u64> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                !link_or_reparse(&metadata),
                "cannot size linked update target {}",
                path.display()
            );
            if metadata.is_file() {
                return Ok(metadata.len());
            }
            ensure!(metadata.is_dir(), "update target has an unsupported type");
            fs::read_dir(path)?.try_fold(0_u64, |total, entry| {
                total
                    .checked_add(path_size(&entry?.path())?)
                    .context("update target size overflow")
            })
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(0),
        Err(error) => Err(error.into()),
    }
}

fn nearest_existing_directory(path: &Path) -> anyhow::Result<PathBuf> {
    let mut candidate = absolute_lexical(path)?;
    loop {
        if candidate.is_dir() {
            reject_link(&candidate, "space-check directory")?;
            return Ok(candidate);
        }
        ensure!(
            candidate.pop(),
            "no existing filesystem root for space check"
        );
    }
}

#[cfg(unix)]
fn checked_space_bytes(
    available_blocks: impl Into<u64>,
    fragment_size: impl Into<u64>,
) -> Option<u64> {
    available_blocks.into().checked_mul(fragment_size.into())
}

#[cfg(unix)]
fn available_space(path: &Path) -> anyhow::Result<Option<u64>> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let path = nearest_existing_directory(path)?;
    let encoded = CString::new(path.as_os_str().as_bytes())?;
    let mut stats = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: `encoded` is a NUL-terminated path and `stats` points to writable
    // storage of the exact type required by statvfs.
    let result = unsafe { libc::statvfs(encoded.as_ptr(), stats.as_mut_ptr()) };
    if result != 0 {
        return Ok(None);
    }
    // SAFETY: statvfs returned success and initialized the output structure.
    let stats = unsafe { stats.assume_init() };
    Ok(checked_space_bytes(stats.f_bavail, stats.f_frsize))
}

#[cfg(windows)]
fn available_space(path: &Path) -> anyhow::Result<Option<u64>> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;

    let path = nearest_existing_directory(path)?;
    let wide = path
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let mut available = 0_u64;
    // SAFETY: `wide` is NUL terminated and `available` is valid writable
    // storage. The remaining optional result pointers are intentionally null.
    let result = unsafe {
        GetDiskFreeSpaceExW(
            wide.as_ptr(),
            &mut available,
            std::ptr::null_mut(),
            std::ptr::null_mut(),
        )
    };
    Ok((result != 0).then_some(available))
}

#[cfg(not(any(unix, windows)))]
fn available_space(_path: &Path) -> anyhow::Result<Option<u64>> {
    Ok(None)
}

fn coalesce_staging_targets(targets: Vec<OwnedTarget>) -> anyhow::Result<Vec<OwnedTarget>> {
    let mut result = Vec::<OwnedTarget>::new();
    for target in targets {
        let target = validate_owned_target(&target)?;
        if let Some(existing) = result
            .iter()
            .find(|existing| paths_equal(&existing.path, &target.path))
        {
            ensure!(
                existing.component == target.component
                    && paths_equal(&existing.boundary, &target.boundary),
                "update target {} is claimed by multiple ownership boundaries",
                target.path.display()
            );
        } else {
            result.push(target);
        }
    }
    Ok(result)
}

fn validate_complete_catalog_binding(
    plan: &UpdatePlan,
    catalogs: &VerifiedStagingCatalogs<'_>,
) -> anyhow::Result<()> {
    ensure!(
        !catalogs.core.catalog_signing_key_id.trim().is_empty()
            || !plan.components().iter().any(|component| {
                matches!(
                    component,
                    UpdatePlanComponent::CoreBundle {
                        action: UpdateAction::Update,
                        ..
                    }
                )
            }),
        "selected Core artifact has no verified catalog signature"
    );
    ensure!(
        !catalogs.adapters.catalog_signing_key_id.trim().is_empty()
            || !plan.components().iter().any(|component| {
                matches!(
                    component,
                    UpdatePlanComponent::Adapter {
                        action: UpdateAction::Update,
                        ..
                    }
                )
            }),
        "selected adapter artifact has no verified catalog signature"
    );
    for component in plan.components() {
        match component {
            UpdatePlanComponent::CoreBundle {
                action: UpdateAction::Update,
                artifact: Some(artifact),
                ..
            } => {
                let release = catalogs
                    .core
                    .catalog
                    .releases
                    .iter()
                    .find(|release| release.version == artifact.version)
                    .context("Core release is absent from the verified catalog")?;
                ensure!(
                    release == &artifact.release
                        && release
                            .platforms
                            .iter()
                            .any(|platform| platform == &artifact.platform),
                    "Core release metadata differs from the verified catalog"
                );
            }
            UpdatePlanComponent::Adapter {
                action: UpdateAction::Update,
                release: Some(artifact),
                ..
            } => validate_adapter_catalog_binding(artifact, &catalogs.adapters.catalog)?,
            component
                if matches!(
                    component.action(),
                    UpdateAction::Update | UpdateAction::ReinstallLocalSource
                ) && matches!(
                    component,
                    UpdatePlanComponent::CoreBundle { artifact: None, .. }
                        | UpdatePlanComponent::Adapter {
                            release: None,
                            local_source: None,
                            ..
                        }
                ) =>
            {
                bail!("selected component has incomplete artifact metadata")
            }
            _ => {}
        }
    }
    Ok(())
}

fn validate_adapter_catalog_binding(
    artifact: &AdapterReleaseArtifact,
    catalog: &AdapterReleaseIndex,
) -> anyhow::Result<()> {
    let adapter = catalog
        .adapters
        .iter()
        .find(|adapter| adapter.domain == artifact.domain)
        .context("adapter release is absent from the verified catalog")?;
    ensure!(
        adapter.releases.iter().any(|release| {
            release.version == artifact.version
                && release == &artifact.release
                && release
                    .platforms
                    .iter()
                    .any(|platform| platform == &artifact.platform)
        }),
        "adapter compatibility variant metadata differs from the verified catalog"
    );
    Ok(())
}

fn stage_core_artifact(
    client: &UpdateNetworkClient,
    artifact_root: &Path,
    name: &str,
    plan_platform: &str,
    artifact: &super::plan::CoreBundleArtifact,
    catalog: &VerifiedCoreUpdateCatalog,
) -> anyhow::Result<StagedArtifact> {
    ensure!(
        artifact.platform.platform == plan_platform,
        "Core artifact platform does not match resolved plan platform"
    );
    let resolved = ResolvedCoreRelease {
        version: Version::parse(&artifact.version)?,
        release: artifact.release.clone(),
        platform: artifact.platform.clone(),
    };
    let directory = artifact_root.join("core");
    prepare_artifact_directory(&directory)?;
    let archive = directory.join("archive.tar.gz");
    download_once(
        client,
        &artifact.platform.archive_url,
        &archive,
        MAX_UPDATE_ARTIFACT_BYTES,
    )?;
    verify_file_sha256_for(&archive, &artifact.platform.sha256, "Core release archive")?;
    let signature = directory.join("archive.sig");
    download_once(
        client,
        &artifact.platform.signature_url,
        &signature,
        MAX_UPDATE_SIGNATURE_BYTES,
    )?;
    verify_resolved_core_archive_signature(&archive, &signature, &resolved, catalog)?;
    let extraction = directory.join("extracted");
    remove_path_if_exists(&extraction)?;
    fs::create_dir_all(&extraction)?;
    let extracted_root = extract_bound_core_archive(&archive, &extraction, &resolved)?;
    let extension = if plan_platform.starts_with("windows-") {
        ".exe"
    } else {
        ""
    };
    let platform_root = extracted_root.join(plan_platform);
    let core_binary = platform_root.join(format!("ldgr{extension}"));
    let agentctl_binary = platform_root.join(format!("agentctl{extension}"));
    ensure_regular_staged_path(&core_binary, "staged Core binary")?;
    ensure_regular_staged_path(&agentctl_binary, "staged agentctl binary")?;
    Ok(StagedArtifact::CoreBundle {
        name: name.to_owned(),
        archive,
        signature,
        extracted_root,
        core_binary,
        agentctl_binary,
    })
}

fn stage_adapter_artifact(
    client: &UpdateNetworkClient,
    artifact_root: &Path,
    plan: &UpdatePlan,
    name: &str,
    artifact: &AdapterReleaseArtifact,
    catalogs: &VerifiedStagingCatalogs<'_>,
) -> anyhow::Result<StagedArtifact> {
    ensure!(
        artifact.domain == name && artifact.platform.platform == plan.platform(),
        "adapter artifact identity or platform differs from the resolved plan"
    );
    validate_adapter_catalog_binding(artifact, &catalogs.adapters.catalog)?;
    let directory = artifact_root.join(format!("adapter-{name}"));
    prepare_artifact_directory(&directory)?;
    let archive = directory.join("archive.tar.gz");
    download_once(
        client,
        &artifact.platform.asset_url,
        &archive,
        MAX_UPDATE_ARTIFACT_BYTES,
    )?;
    verify_file_sha256_for(
        &archive,
        &artifact.platform.sha256,
        "adapter release archive",
    )?;
    let signature = directory.join("archive.sig");
    download_once(
        client,
        &artifact.platform.signature_url,
        &signature,
        MAX_UPDATE_SIGNATURE_BYTES,
    )?;
    let envelope = parse_detached_signature(&fs::read_to_string(&signature)?)?;
    verify_detached_signature_bytes(
        &fs::read(&archive)?,
        &envelope,
        &catalogs.adapters.archive_keyring,
        &artifact.platform.signing_key_id,
        "adapter release archive",
    )?;
    let extraction = directory.join("extracted");
    remove_path_if_exists(&extraction)?;
    fs::create_dir_all(&extraction)?;
    crate::release_index::extract_safe_tar_gz(
        &archive,
        &extraction,
        &artifact.platform.archive_root,
    )?;
    let extracted_root = extraction.join(&artifact.platform.archive_root);
    if artifact.release.compatibility.is_some() {
        crate::cli::commands::ops::validate_adapter_bundle_contract(&extracted_root, name)?;
        crate::release_index::verify_indexed_v2_sidecar(&extracted_root, name, &artifact.release)?;
    } else {
        crate::cli::commands::ops::validate_legacy_adapter_bundle_contract(&extracted_root, name)?;
    }
    ensure!(
        extracted_root
            .join(&artifact.platform.resource_manifest)
            .is_file(),
        "adapter archive is missing its embedded resource manifest"
    );
    let platform_binary = extracted_root
        .join(&artifact.platform.platform)
        .join(&artifact.platform.binary);
    ensure!(
        platform_binary.is_file()
            || !crate::cli::commands::ops::adapter_manifest_references_binary(
                &extracted_root,
                &artifact.platform.binary,
            )?,
        "adapter archive is missing required executable {}/{}",
        artifact.platform.platform,
        artifact.platform.binary
    );
    Ok(StagedArtifact::AdapterRelease {
        name: name.to_owned(),
        archive,
        signature,
        extracted_root,
    })
}

fn stage_local_source(
    home: &Path,
    name: &str,
    source: &super::plan::LocalSourceArtifact,
    owned: &AdapterStagingOwnership,
) -> anyhow::Result<(StagedArtifact, Vec<OwnedTarget>)> {
    let AdapterOwnershipReceipt::LocalSource(receipt) = &owned.receipt else {
        bail!("adapter `{name}` plan expects local-source ownership");
    };
    ensure!(
        receipt.domain == name
            && receipt.source.package == source.package
            && receipt.source.bundle_sha256 == source.installed_source_sha256,
        "local-source receipt differs from the resolved plan"
    );
    let (source_root, source_sha256, source_changed) =
        crate::cli::commands::ops::inspect_source_installation_for_update(
            &owned.install_root,
            home,
            receipt,
        )?;
    ensure!(
        source_changed && source_sha256 == source.current_source_sha256,
        "local source changed after update planning"
    );
    crate::cli::commands::ops::validate_adapter_bundle_contract(&source_root, name)?;
    let mut targets = adapter_common_targets(home, name, owned)?;
    let resources = crate::cli::commands::ops::source_harness_resource_plan(&source_root, home)?;
    for resource in resources {
        targets.push(OwnedTarget::new(
            name,
            "harness_resource",
            resource.root,
            resource.target,
        ));
    }
    Ok((
        StagedArtifact::LocalSource {
            name: name.to_owned(),
            source_root,
            source_sha256,
        },
        targets,
    ))
}

fn core_targets(
    home: &Path,
    receipt: &CoreInstallationReceipt,
) -> anyhow::Result<Vec<OwnedTarget>> {
    validate_receipt(receipt)?;
    ensure!(
        receipt.core_binary_path.parent() == Some(receipt.install_root.as_path())
            && receipt.agentctl_binary_path.parent() == Some(receipt.install_root.as_path()),
        "Core receipt binary destinations escape install_root"
    );
    let ldgr_home = absolute_lexical(&home.join(".ldgr"))?;
    Ok(vec![
        OwnedTarget::new(
            "ldgr-core",
            "core_binary",
            &receipt.install_root,
            &receipt.core_binary_path,
        ),
        OwnedTarget::new(
            "ldgr-core",
            "agentctl_binary",
            &receipt.install_root,
            &receipt.agentctl_binary_path,
        ),
        OwnedTarget::new(
            "ldgr-core",
            "core_backup",
            &receipt.install_root,
            previous_path(&receipt.core_binary_path)?,
        ),
        OwnedTarget::new(
            "ldgr-core",
            "agentctl_backup",
            &receipt.install_root,
            previous_path(&receipt.agentctl_binary_path)?,
        ),
        OwnedTarget::new(
            "ldgr-core",
            "installation_receipt",
            &ldgr_home,
            core_installation_receipt_path(home),
        ),
    ])
}

fn adapter_release_targets(
    home: &Path,
    name: &str,
    artifact: &AdapterReleaseArtifact,
    extracted_root: &Path,
    owned: &AdapterStagingOwnership,
) -> anyhow::Result<Vec<OwnedTarget>> {
    let AdapterOwnershipReceipt::Release(receipt) = &owned.receipt else {
        bail!("adapter `{name}` plan expects signed-release ownership");
    };
    ensure!(
        receipt.domain == name,
        "adapter receipt domain differs from resolved plan"
    );
    crate::cli::commands::ops::inspect_release_installation_for_update(
        &owned.install_root,
        home,
        receipt,
    )?;
    let mut targets = adapter_common_targets(home, name, owned)?;
    let platform_binary = extracted_root
        .join(&artifact.platform.platform)
        .join(&artifact.platform.binary);
    if platform_binary.is_file() {
        let binary_root = absolute_lexical(&home.join(".local/bin"))?;
        let binary_target = binary_root.join(&artifact.platform.binary);
        let previously_owned_binary = receipt
            .binary_path
            .as_deref()
            .map(Path::new)
            .map(absolute_lexical)
            .transpose()?;
        ensure!(
            !binary_target.exists()
                || previously_owned_binary
                    .as_ref()
                    .is_some_and(|owned| paths_equal(owned, &binary_target)),
            "refusing to overwrite unowned adapter binary {}",
            binary_target.display()
        );
        targets.push(OwnedTarget::new(
            name,
            "adapter_binary",
            &binary_root,
            binary_target,
        ));
    }
    let resources = crate::cli::commands::ops::typed_harness_resource_plan(
        extracted_root,
        home,
        &artifact.platform.resource_manifest,
    )?;
    let previously_owned = receipt
        .owned_resources
        .iter()
        .map(|resource| absolute_lexical(Path::new(&resource.path)))
        .collect::<anyhow::Result<Vec<_>>>()?;
    let allowed_roots = crate::cli::commands::ops::source_allowed_resource_roots(home)?;
    for (_, target) in resources {
        let target = absolute_lexical(&target)?;
        let boundary = allowed_roots
            .iter()
            .find(|root| {
                absolute_lexical(root).is_ok_and(|root| target != root && target.starts_with(root))
            })
            .with_context(|| {
                format!(
                    "adapter resource {} is outside configured harness boundaries",
                    target.display()
                )
            })?;
        ensure!(
            !target.exists() || previously_owned.iter().any(|owned| paths_equal(owned, &target)),
            "refusing to overwrite unowned harness resource {}; remove it or choose a different harness resource path",
            target.display()
        );
        targets.push(OwnedTarget::new(name, "harness_resource", boundary, target));
    }
    Ok(targets)
}

fn adapter_common_targets(
    home: &Path,
    name: &str,
    owned: &AdapterStagingOwnership,
) -> anyhow::Result<Vec<OwnedTarget>> {
    let expected = absolute_lexical(&owned.user_adapter_root.join(name))?;
    ensure!(
        paths_equal(&absolute_lexical(&owned.install_root)?, &expected),
        "adapter install root is outside its receipt-managed user boundary"
    );
    let marker_root = home.join(".ldgr/installed-adapters");
    let mut targets = vec![
        OwnedTarget::new(
            name,
            "adapter_bundle_and_receipt",
            &owned.user_adapter_root,
            &owned.install_root,
        ),
        OwnedTarget::new(
            name,
            "installation_marker",
            &marker_root,
            marker_root.join(name),
        ),
    ];
    let resources = match &owned.receipt {
        AdapterOwnershipReceipt::Release(receipt) => &receipt.owned_resources,
        AdapterOwnershipReceipt::LocalSource(receipt) => &receipt.owned_resources,
    };
    if let AdapterOwnershipReceipt::Release(receipt) = &owned.receipt {
        if let Some(binary) = receipt.binary_path.as_deref() {
            let binary_root = absolute_lexical(&home.join(".local/bin"))?;
            let binary = absolute_lexical(Path::new(binary))?;
            ensure!(
                binary
                    .parent()
                    .is_some_and(|parent| paths_equal(parent, &binary_root)),
                "receipt-owned adapter binary is outside the user binary boundary"
            );
            targets.push(OwnedTarget::new(
                name,
                "previous_adapter_binary",
                binary_root,
                binary,
            ));
        }
    }
    let allowed_roots = crate::cli::commands::ops::source_allowed_resource_roots(home)?;
    for resource in resources {
        let target = absolute_lexical(Path::new(&resource.path))?;
        let boundary = allowed_roots
            .iter()
            .find(|root| {
                absolute_lexical(root).is_ok_and(|root| target != root && target.starts_with(root))
            })
            .context("receipt-owned adapter resource is outside configured harness boundaries")?;
        targets.push(OwnedTarget::new(
            name,
            "previous_harness_resource",
            boundary,
            target,
        ));
    }
    Ok(targets)
}

fn prepare_artifact_directory(path: &Path) -> anyhow::Result<()> {
    reject_existing_link_ancestors(path, path.parent())?;
    fs::create_dir_all(path)?;
    reject_link(path, "artifact staging directory")
}

fn download_once(
    client: &UpdateNetworkClient,
    source: &str,
    destination: &Path,
    maximum: u64,
) -> anyhow::Result<()> {
    if destination.exists() {
        ensure_regular_staged_path(destination, "previously downloaded update artifact")?;
        ensure!(
            fs::metadata(destination)?.len() <= maximum,
            "previously downloaded update artifact exceeds its size limit"
        );
        return Ok(());
    }
    client.download_artifact(source, destination, maximum)?;
    Ok(())
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum JournalPhase {
    Snapshotting,
    Ready,
    Applying,
    RollingBack,
    RolledBack,
    Committed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SnapshotKind {
    Missing,
    File,
    Directory,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
enum SnapshotState {
    Captured,
    Activated,
    Restored,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct SnapshotEntry {
    component: String,
    role: String,
    boundary: PathBuf,
    target: PathBuf,
    backup: PathBuf,
    kind: SnapshotKind,
    state: SnapshotState,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
struct InstallJournal {
    schema_version: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    plan_id: Option<String>,
    phase: JournalPhase,
    snapshots: Vec<SnapshotEntry>,
}

/// A durable transaction covering every destination in one resolved update plan.
///
/// `prepare` captures the complete target set before returning. Activation code
/// can then perform component-specific work through `activate_file` and
/// `activate_directory`, or call `begin_activation` before a legacy activation
/// routine that mutates already-snapshotted targets. Dropping an uncommitted
/// transaction restores snapshots in reverse order.
pub struct InstallTransaction {
    backup_root: PathBuf,
    journal_path: PathBuf,
    journal: InstallJournal,
    committed: bool,
}

impl InstallTransaction {
    /// Compatibility constructor for the existing single-adapter updater.
    /// New whole-plan callers should use `prepare` so the complete destination
    /// manifest is known and snapshotted before activation starts.
    pub fn new(backup_root: PathBuf) -> anyhow::Result<Self> {
        prepare_backup_root(&backup_root)?;
        let journal_path = backup_root.join(JOURNAL_FILE);
        ensure!(
            !journal_path.exists(),
            "installation transaction already exists at {}",
            journal_path.display()
        );
        let journal = InstallJournal {
            schema_version: JOURNAL_SCHEMA_VERSION,
            plan_id: None,
            phase: JournalPhase::Snapshotting,
            snapshots: Vec::new(),
        };
        atomic_json(&journal_path, &journal)?;
        Ok(Self {
            backup_root,
            journal_path,
            journal,
            committed: false,
        })
    }

    pub fn prepare(
        backup_root: PathBuf,
        plan_id: &str,
        targets: &[OwnedTarget],
    ) -> anyhow::Result<Self> {
        validate_plan_id(plan_id)?;
        validate_target_manifest(targets)?;
        prepare_backup_root(&backup_root)?;
        let journal_path = backup_root.join(JOURNAL_FILE);
        let mut transaction = if journal_path.exists() {
            let journal = read_journal(&journal_path)?;
            ensure!(
                journal.plan_id.as_deref() == Some(plan_id),
                "transaction journal belongs to another update plan"
            );
            ensure!(
                matches!(
                    journal.phase,
                    JournalPhase::Snapshotting | JournalPhase::Ready
                ),
                "transaction journal is not resumable from {:?}",
                journal.phase
            );
            Self {
                backup_root,
                journal_path,
                journal,
                committed: false,
            }
        } else {
            let journal = InstallJournal {
                schema_version: JOURNAL_SCHEMA_VERSION,
                plan_id: Some(plan_id.to_owned()),
                phase: JournalPhase::Snapshotting,
                snapshots: Vec::new(),
            };
            atomic_json(&journal_path, &journal)?;
            Self {
                backup_root,
                journal_path,
                journal,
                committed: false,
            }
        };

        ensure_manifest_matches(&transaction.journal.snapshots, targets)?;
        if transaction.journal.phase == JournalPhase::Ready {
            ensure!(
                transaction.journal.snapshots.len() == targets.len(),
                "sealed transaction journal is missing resolved update targets"
            );
            return Ok(transaction);
        }
        for target in targets {
            transaction.snapshot_owned(target)?;
        }
        transaction.seal()?;
        Ok(transaction)
    }

    pub fn resume_for_rollback(backup_root: PathBuf) -> anyhow::Result<Self> {
        let journal_path = backup_root.join(JOURNAL_FILE);
        let journal = read_journal(&journal_path)?;
        ensure!(
            matches!(
                journal.phase,
                JournalPhase::Ready
                    | JournalPhase::Applying
                    | JournalPhase::RollingBack
                    | JournalPhase::RolledBack
            ),
            "transaction journal cannot be recovered from {:?}",
            journal.phase
        );
        for entry in &journal.snapshots {
            validate_entry_paths(&backup_root, entry)?;
        }
        Ok(Self {
            backup_root,
            journal_path,
            journal,
            committed: false,
        })
    }

    /// Reopens a Windows finalizer transaction and reports whether activation
    /// had already begun. Ready journals continue application; applying or
    /// rolling-back journals may only be restored.
    pub(crate) fn resume_for_finalizer(
        backup_root: PathBuf,
        plan_id: &str,
        targets: &[OwnedTarget],
    ) -> anyhow::Result<(Self, bool)> {
        validate_plan_id(plan_id)?;
        validate_target_manifest(targets)?;
        let journal = read_journal(&backup_root.join(JOURNAL_FILE))?;
        ensure!(
            journal.plan_id.as_deref() == Some(plan_id),
            "transaction journal belongs to another update plan"
        );
        ensure_manifest_matches(&journal.snapshots, targets)?;
        ensure!(
            journal.snapshots.len() == targets.len(),
            "transaction journal is missing resolved update targets"
        );
        match journal.phase {
            JournalPhase::Ready => Ok((
                Self {
                    journal_path: backup_root.join(JOURNAL_FILE),
                    backup_root,
                    journal,
                    committed: false,
                },
                false,
            )),
            JournalPhase::Applying | JournalPhase::RollingBack | JournalPhase::RolledBack => {
                Ok((Self::resume_for_rollback(backup_root)?, true))
            }
            phase => bail!("transaction journal cannot be finalized from {phase:?}"),
        }
    }

    pub fn snapshot(&mut self, target: &Path) -> anyhow::Result<()> {
        let absolute = absolute_lexical(target)?;
        let boundary = absolute
            .parent()
            .context("installation target has no parent boundary")?
            .to_path_buf();
        self.snapshot_owned(&OwnedTarget::new(
            "legacy_adapter",
            "adapter_owned_target",
            boundary,
            absolute,
        ))
    }

    pub fn snapshot_within(
        &mut self,
        component: &str,
        role: &str,
        boundary: &Path,
        target: &Path,
    ) -> anyhow::Result<()> {
        self.snapshot_owned(&OwnedTarget::new(component, role, boundary, target))
    }

    fn snapshot_owned(&mut self, target: &OwnedTarget) -> anyhow::Result<()> {
        let normalized = validate_owned_target(target)?;
        if let Some(existing) = self
            .journal
            .snapshots
            .iter()
            .find(|snapshot| paths_equal(&snapshot.target, &normalized.path))
        {
            if self.journal.phase == JournalPhase::Snapshotting {
                ensure!(
                    existing.component == normalized.component
                        && existing.role == normalized.role
                        && paths_equal(&existing.boundary, &normalized.boundary),
                    "update target {} has conflicting ownership",
                    normalized.path.display()
                );
            }
            return Ok(());
        }
        ensure!(
            self.journal.phase == JournalPhase::Snapshotting,
            "cannot add snapshots after the transaction is sealed"
        );

        let backup = self
            .backup_root
            .join(SNAPSHOTS_DIRECTORY)
            .join(self.journal.snapshots.len().to_string());
        // A crash after copying a snapshot but before journaling it can leave
        // this deterministic slot orphaned. It is updater-owned and safe to
        // discard before recapturing the still-unmodified target on retry.
        remove_path_if_exists(&backup)?;
        let kind = snapshot_path(&normalized.path, &backup)?;
        let entry = SnapshotEntry {
            component: normalized.component,
            role: normalized.role,
            boundary: normalized.boundary,
            target: normalized.path,
            backup,
            kind,
            state: SnapshotState::Captured,
        };
        self.journal.snapshots.push(entry);
        self.persist()
    }

    pub fn seal(&mut self) -> anyhow::Result<()> {
        match self.journal.phase {
            JournalPhase::Snapshotting => {
                self.journal.phase = JournalPhase::Ready;
                self.persist()
            }
            JournalPhase::Ready => Ok(()),
            phase => bail!("cannot seal transaction from {phase:?}"),
        }
    }

    pub fn begin_activation(&mut self) -> anyhow::Result<()> {
        match self.journal.phase {
            JournalPhase::Snapshotting => {
                self.seal()?;
                self.begin_activation()
            }
            JournalPhase::Ready => {
                self.journal.phase = JournalPhase::Applying;
                self.persist()
            }
            JournalPhase::Applying => Ok(()),
            phase => bail!("cannot activate transaction from {phase:?}"),
        }
    }
    pub fn backup_file(&mut self, target: &Path, backup: &Path) -> anyhow::Result<()> {
        self.begin_activation()?;
        let target_index = self.snapshot_index(target)?;
        let backup_index = self.snapshot_index(backup)?;
        let target = self.journal.snapshots[target_index].target.clone();
        let backup = self.journal.snapshots[backup_index].target.clone();
        ensure!(
            target.parent() == backup.parent(),
            "paired binary backup must share its destination filesystem"
        );
        ensure_regular_staged_path(&target, "installed binary")?;
        remove_path_if_exists(&backup)?;
        fs::rename(&target, &backup).with_context(|| {
            format!(
                "failed to rename installed binary {} to backup {}",
                target.display(),
                backup.display()
            )
        })?;
        sync_directory(
            backup
                .parent()
                .context("paired binary backup has no parent")?,
        )?;
        self.journal.snapshots[target_index].state = SnapshotState::Activated;
        self.journal.snapshots[backup_index].state = SnapshotState::Activated;
        self.persist()
    }

    pub fn activate_file(&mut self, staged: &Path, target: &Path) -> anyhow::Result<()> {
        self.begin_activation()?;
        let index = self.snapshot_index(target)?;
        ensure_regular_staged_path(staged, "staged file")?;
        if same_file_bytes(staged, &self.journal.snapshots[index].target)? {
            return self.mark_activated(index);
        }
        replace_file(staged, &self.journal.snapshots[index].target)?;
        self.mark_activated(index)
    }

    pub fn activate_directory(&mut self, staged: &Path, target: &Path) -> anyhow::Result<()> {
        self.begin_activation()?;
        let index = self.snapshot_index(target)?;
        validate_regular_tree(staged)?;
        replace_directory(staged, &self.journal.snapshots[index].target)?;
        self.mark_activated(index)
    }

    pub fn rollback(&mut self) -> anyhow::Result<()> {
        if self.journal.phase == JournalPhase::RolledBack {
            return Ok(());
        }
        ensure!(
            self.journal.phase != JournalPhase::Committed,
            "committed installation transaction cannot be rolled back"
        );
        self.journal.phase = JournalPhase::RollingBack;
        self.persist()?;
        for index in (0..self.journal.snapshots.len()).rev() {
            restore_snapshot(&self.journal.snapshots[index])?;
            self.journal.snapshots[index].state = SnapshotState::Restored;
            self.persist()?;
        }
        self.journal.phase = JournalPhase::RolledBack;
        self.persist()
    }

    pub fn commit(mut self) -> anyhow::Result<()> {
        if self.journal.phase == JournalPhase::Snapshotting {
            self.seal()?;
        }
        ensure!(
            matches!(
                self.journal.phase,
                JournalPhase::Ready | JournalPhase::Applying
            ),
            "installation transaction cannot commit from {:?}",
            self.journal.phase
        );
        self.journal.phase = JournalPhase::Committed;
        self.persist()?;
        self.committed = true;
        remove_dir_if_exists(&self.backup_root)
    }

    /// Leaves a sealed, unmodified transaction journal for a detached
    /// finalizer. Consuming self prevents Drop from interpreting the
    /// intentional process handoff as an activation failure.
    pub(crate) fn preserve_for_finalizer(mut self) -> anyhow::Result<()> {
        ensure!(
            self.journal.phase == JournalPhase::Ready,
            "only a sealed, unmodified transaction can be handed to a finalizer"
        );
        self.persist()?;
        self.committed = true;
        Ok(())
    }

    fn snapshot_index(&self, target: &Path) -> anyhow::Result<usize> {
        let target = absolute_lexical(target)?;
        self.journal
            .snapshots
            .iter()
            .position(|snapshot| paths_equal(&snapshot.target, &target))
            .with_context(|| {
                format!(
                    "activation target {} was not snapshotted before mutation",
                    target.display()
                )
            })
    }

    fn mark_activated(&mut self, index: usize) -> anyhow::Result<()> {
        self.journal.snapshots[index].state = SnapshotState::Activated;
        self.persist()
    }

    fn persist(&self) -> anyhow::Result<()> {
        atomic_json(&self.journal_path, &self.journal)
            .context("failed to persist installation rollback journal")
    }
}

impl Drop for InstallTransaction {
    fn drop(&mut self) {
        if !self.committed
            && !matches!(
                self.journal.phase,
                JournalPhase::RolledBack | JournalPhase::Committed
            )
        {
            let _ = self.rollback();
        }
    }
}

fn prepare_backup_root(root: &Path) -> anyhow::Result<()> {
    let root = absolute_lexical(root)?;
    reject_existing_link_ancestors(&root, None)?;
    fs::create_dir_all(root.join(SNAPSHOTS_DIRECTORY)).with_context(|| {
        format!(
            "failed to create installation snapshot root {}",
            root.display()
        )
    })
}

fn read_journal(path: &Path) -> anyhow::Result<InstallJournal> {
    reject_link(path, "transaction journal")?;
    let metadata = fs::metadata(path).context("failed to inspect transaction journal")?;
    ensure!(
        metadata.is_file(),
        "transaction journal is not a regular file"
    );
    ensure!(
        metadata.len() <= 1024 * 1024,
        "transaction journal exceeds the 1 MiB limit"
    );
    let journal: InstallJournal =
        serde_json::from_reader(File::open(path).context("failed to open transaction journal")?)
            .context("transaction journal is invalid")?;
    ensure!(
        journal.schema_version == JOURNAL_SCHEMA_VERSION,
        "unsupported transaction journal schema {}",
        journal.schema_version
    );
    Ok(journal)
}

fn validate_plan_id(plan_id: &str) -> anyhow::Result<()> {
    ensure!(
        plan_id.len() == 64
            && plan_id
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase()),
        "update plan id must be a lowercase SHA-256 digest"
    );
    Ok(())
}

fn validate_target_manifest(targets: &[OwnedTarget]) -> anyhow::Result<()> {
    ensure!(!targets.is_empty(), "update plan has no activation targets");
    let mut paths = BTreeSet::new();
    let mut normalized = Vec::new();
    for target in targets {
        let target = validate_owned_target(target)?;
        ensure!(
            paths.insert(path_key(&target.path)),
            "update plan contains duplicate target {}",
            target.path.display()
        );
        normalized.push(target.path);
    }
    for (index, path) in normalized.iter().enumerate() {
        for other in normalized.iter().skip(index + 1) {
            ensure!(
                !path.starts_with(other) && !other.starts_with(path),
                "update targets {} and {} overlap",
                path.display(),
                other.display()
            );
        }
    }
    Ok(())
}

fn ensure_manifest_matches(
    snapshots: &[SnapshotEntry],
    targets: &[OwnedTarget],
) -> anyhow::Result<()> {
    ensure!(
        snapshots.len() <= targets.len(),
        "transaction journal contains more targets than the resolved plan"
    );
    for snapshot in snapshots {
        let Some(target) = targets.iter().find(|target| {
            absolute_lexical(&target.path).is_ok_and(|path| paths_equal(&path, &snapshot.target))
        }) else {
            bail!(
                "transaction journal target {} is absent from the resolved plan",
                snapshot.target.display()
            );
        };
        let target = validate_owned_target(target)?;
        ensure!(
            snapshot.component == target.component
                && snapshot.role == target.role
                && paths_equal(&snapshot.boundary, &target.boundary),
            "transaction journal ownership changed for {}",
            snapshot.target.display()
        );
    }
    Ok(())
}

fn validate_owned_target(target: &OwnedTarget) -> anyhow::Result<OwnedTarget> {
    ensure!(
        !target.component.trim().is_empty() && !target.role.trim().is_empty(),
        "update target component and role are required"
    );
    let boundary = absolute_lexical(&target.boundary)?;
    let path = absolute_lexical(&target.path)?;
    ensure!(
        path != boundary && path.starts_with(&boundary),
        "update target {} escapes ownership boundary {}",
        path.display(),
        boundary.display()
    );
    reject_existing_link_ancestors(&boundary, None)?;
    reject_existing_link_ancestors(&path, Some(&boundary))?;
    Ok(OwnedTarget {
        component: target.component.clone(),
        role: target.role.clone(),
        boundary,
        path,
    })
}

fn validate_entry_paths(root: &Path, entry: &SnapshotEntry) -> anyhow::Result<()> {
    let target = validate_owned_target(&OwnedTarget::new(
        &entry.component,
        &entry.role,
        &entry.boundary,
        &entry.target,
    ))?;
    ensure!(
        paths_equal(&target.path, &entry.target),
        "transaction target normalization changed"
    );
    let snapshots = absolute_lexical(&root.join(SNAPSHOTS_DIRECTORY))?;
    let backup = absolute_lexical(&entry.backup)?;
    ensure!(
        backup != snapshots && backup.starts_with(&snapshots),
        "transaction backup escapes its snapshot root"
    );
    Ok(())
}

fn snapshot_path(target: &Path, backup: &Path) -> anyhow::Result<SnapshotKind> {
    match fs::symlink_metadata(target) {
        Ok(metadata) => {
            ensure!(
                !link_or_reparse(&metadata),
                "refusing to snapshot linked update target {}",
                target.display()
            );
            if metadata.is_file() {
                copy_file_synced(target, backup)?;
                Ok(SnapshotKind::File)
            } else if metadata.is_dir() {
                copy_dir_recursive(target, backup)?;
                Ok(SnapshotKind::Directory)
            } else {
                bail!("update target {} has an unsupported type", target.display())
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(SnapshotKind::Missing),
        Err(error) => Err(error)
            .with_context(|| format!("failed to inspect update target {}", target.display())),
    }
}

fn restore_snapshot(snapshot: &SnapshotEntry) -> anyhow::Result<()> {
    validate_owned_target(&OwnedTarget::new(
        &snapshot.component,
        &snapshot.role,
        &snapshot.boundary,
        &snapshot.target,
    ))?;
    remove_path_if_exists(&snapshot.target)?;
    match snapshot.kind {
        SnapshotKind::Missing => Ok(()),
        SnapshotKind::File => copy_file_synced(&snapshot.backup, &snapshot.target),
        SnapshotKind::Directory => copy_dir_recursive(&snapshot.backup, &snapshot.target),
    }
}

fn replace_file(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let parent = destination
        .parent()
        .context("activation target has no parent")?;
    fs::create_dir_all(parent)?;
    reject_existing_link_ancestors(destination, Some(parent))?;
    let temporary = parent.join(format!(
        ".ldgr-update-file-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    copy_file_synced(source, &temporary)?;
    remove_path_if_exists(destination)?;
    fs::rename(&temporary, destination).with_context(|| {
        format!(
            "failed to activate staged file at {}",
            destination.display()
        )
    })
}

fn replace_directory(source: &Path, destination: &Path) -> anyhow::Result<()> {
    let parent = destination
        .parent()
        .context("activation target has no parent")?;
    fs::create_dir_all(parent)?;
    let temporary = parent.join(format!(
        ".ldgr-update-dir-{}-{}",
        std::process::id(),
        unique_suffix()
    ));
    copy_dir_recursive(source, &temporary)?;
    remove_path_if_exists(destination)?;
    fs::rename(&temporary, destination).with_context(|| {
        format!(
            "failed to activate staged directory at {}",
            destination.display()
        )
    })
}

fn same_file_bytes(left: &Path, right: &Path) -> anyhow::Result<bool> {
    if !right.is_file() {
        return Ok(false);
    }
    Ok(fs::read(left)? == fs::read(right)?)
}

fn ensure_regular_staged_path(path: &Path, label: &str) -> anyhow::Result<()> {
    reject_link(path, label)?;
    ensure!(path.is_file(), "{label} is not a regular file");
    Ok(())
}

fn validate_regular_tree(root: &Path) -> anyhow::Result<()> {
    reject_link(root, "staged directory")?;
    ensure!(root.is_dir(), "staged directory is not a directory");
    for entry in fs::read_dir(root)? {
        let entry = entry?;
        let metadata = fs::symlink_metadata(entry.path())?;
        ensure!(
            !link_or_reparse(&metadata),
            "staged directory contains a link or reparse point"
        );
        if metadata.is_dir() {
            validate_regular_tree(&entry.path())?;
        } else {
            ensure!(
                metadata.is_file(),
                "staged directory contains an unsupported file type"
            );
        }
    }
    Ok(())
}

fn copy_file_synced(source: &Path, destination: &Path) -> anyhow::Result<()> {
    ensure_regular_staged_path(source, "snapshot source")?;
    if let Some(parent) = destination.parent() {
        fs::create_dir_all(parent)?;
    }
    let mut input = File::open(source)?;
    let mut output = File::create(destination)?;
    std::io::copy(&mut input, &mut output)?;
    output.flush()?;
    let permissions = fs::metadata(source)?.permissions();
    fs::set_permissions(destination, permissions)?;
    output.sync_all()?;
    Ok(())
}

fn copy_dir_recursive(source: &Path, destination: &Path) -> anyhow::Result<()> {
    validate_regular_tree(source)?;
    ensure!(!destination.exists(), "snapshot destination already exists");
    fs::create_dir_all(destination)?;
    for entry in fs::read_dir(source)? {
        let entry = entry?;
        let source_path = entry.path();
        let destination_path = destination.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir_recursive(&source_path, &destination_path)?;
        } else {
            copy_file_synced(&source_path, &destination_path)?;
        }
    }
    sync_directory(destination)
}

fn remove_path_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                !link_or_reparse(&metadata),
                "refusing to remove linked update target {}",
                path.display()
            );
            if metadata.is_dir() {
                validate_regular_tree(path)?;
                fs::remove_dir_all(path)?;
            } else if metadata.is_file() {
                fs::remove_file(path)?;
            } else {
                bail!("update target {} has an unsupported type", path.display());
            }
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn remove_dir_if_exists(path: &Path) -> anyhow::Result<()> {
    match fs::remove_dir_all(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

fn absolute_lexical(path: &Path) -> anyhow::Result<PathBuf> {
    ensure!(
        path.is_absolute(),
        "update ownership paths must be absolute"
    );
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
            Component::CurDir => {}
            Component::ParentDir => {
                ensure!(
                    normalized.pop(),
                    "update ownership path escapes its filesystem root"
                );
            }
        }
    }
    Ok(normalized)
}

fn reject_existing_link_ancestors(path: &Path, stop: Option<&Path>) -> anyhow::Result<()> {
    let path = absolute_lexical(path)?;
    let stop = stop.map(absolute_lexical).transpose()?;
    let mut current = Some(path.as_path());
    while let Some(candidate) = current {
        if candidate.exists() {
            reject_link(candidate, "update ownership path")?;
        }
        if stop.as_deref() == Some(candidate) {
            break;
        }
        current = candidate.parent();
    }
    Ok(())
}

fn reject_link(path: &Path, label: &str) -> anyhow::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) => {
            ensure!(
                !link_or_reparse(&metadata),
                "{label} contains a symlink or reparse point at {}",
                path.display()
            );
            Ok(())
        }
        Err(error) if error.kind() == ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}

#[cfg(windows)]
fn link_or_reparse(metadata: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    metadata.file_type().is_symlink() || metadata.file_attributes() & 0x400 != 0
}

#[cfg(not(windows))]
fn link_or_reparse(metadata: &fs::Metadata) -> bool {
    metadata.file_type().is_symlink()
}

fn path_key(path: &Path) -> String {
    let key = path.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        key.to_ascii_lowercase()
    } else {
        key
    }
}

fn paths_equal(left: &Path, right: &Path) -> bool {
    path_key(left) == path_key(right)
}

fn unique_suffix() -> u128 {
    use std::time::{SystemTime, UNIX_EPOCH};
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos())
}

#[cfg(unix)]
fn sync_directory(path: &Path) -> anyhow::Result<()> {
    File::open(path)?.sync_all()?;
    Ok(())
}

#[cfg(not(unix))]
fn sync_directory(_path: &Path) -> anyhow::Result<()> {
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use ed25519_dalek::{Signer, SigningKey};
    use flate2::write::GzEncoder;
    use flate2::Compression;
    use sha2::{Digest, Sha256};

    use crate::harness_config::UpdateChannel;
    use crate::release_index::{
        AdapterClassification, AdapterPlatformRelease, AdapterRelease, AdapterReleaseIndex,
        AdapterReleaseProduct, DetachedSignature, ReleaseChannel, ReleaseKeyring, ReleasePublicKey,
    };
    use crate::update::catalog::{
        canonical_adapter_catalog_bytes, canonical_catalog_bytes,
        verify_signed_adapter_update_catalog, verify_signed_core_update_catalog,
        CorePlatformArchive, CoreRelease, CoreReleaseCompatibility, CoreReleaseMetadata,
        CoreUpdateCatalog, PairedAgentctlRelease, VerifiedAdapterUpdateCatalog,
    };
    #[cfg(unix)]
    use crate::update::installation::ProcessCompatibilityProbe;
    use crate::update::installation::{
        CompatibilityEvidence, CompatibilityProbe, CoreArchiveProvenance, CoreInstallerKind,
        LAUNCHER_COMPATIBILITY_SCHEMA,
    };
    use crate::update::plan::{
        build_update_plan, AdapterInstallationKind, AdapterInstallationSnapshot, AdapterOrigin,
        CoreInstallationSnapshot, CorePlanOwnership, UpdateInventory, UpdatePlanRequest,
        VerifiedCatalogSnapshots,
    };
    use crate::update::state::{RecoveryAction, TerminalError, TerminalOutcome, UpdateMode};

    fn target(root: &Path, component: &str, role: &str, name: &str) -> OwnedTarget {
        OwnedTarget::new(component, role, root, root.join(name))
    }

    #[cfg(unix)]
    #[test]
    fn available_space_multiplication_accepts_mixed_widths_and_checks_overflow() {
        assert_eq!(
            checked_space_bytes(u32::MAX, 4_096_u64),
            Some(u64::from(u32::MAX) * 4_096)
        );
        assert_eq!(checked_space_bytes(u64::MAX, 2_u32), None);
    }

    struct CoreFixture {
        _files: tempfile::TempDir,
        _home: tempfile::TempDir,
        home_path: PathBuf,
        plan: UpdatePlan,
        core_catalog: VerifiedCoreUpdateCatalog,
        adapter_catalog: VerifiedAdapterUpdateCatalog,
        ownership: PlanStagingOwnership,
        installed_core: PathBuf,
        installed_agentctl: PathBuf,
        archive: PathBuf,
        signing_key: SigningKey,
    }

    fn write_core_archive(
        destination: &Path,
        archive_root: &str,
        platform: &str,
        metadata: &CoreReleaseMetadata,
    ) -> anyhow::Result<()> {
        let output = File::create(destination)?;
        let encoder = GzEncoder::new(output, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append_archive_file(
            &mut archive,
            &format!("{archive_root}/RELEASE-METADATA.json"),
            &serde_json::to_vec(metadata)?,
        )?;
        append_archive_file(
            &mut archive,
            &format!("{archive_root}/{platform}/ldgr.exe"),
            b"new-core",
        )?;
        append_archive_file(
            &mut archive,
            &format!("{archive_root}/{platform}/agentctl.exe"),
            b"new-agentctl",
        )?;
        archive.into_inner()?.finish()?;
        Ok(())
    }

    fn write_adapter_archive(destination: &Path, archive_root: &str) -> anyhow::Result<()> {
        let output = File::create(destination)?;
        let encoder = GzEncoder::new(output, Compression::default());
        let mut archive = tar::Builder::new(encoder);
        append_archive_file(
            &mut archive,
            &format!("{archive_root}/adapter.toml"),
            b"[adapter]\nslug='fixture'\n",
        )?;
        append_archive_file(
            &mut archive,
            &format!("{archive_root}/adapter-resources.json"),
            br#"{"schema_version":1,"resources":[]}"#,
        )?;
        archive.into_inner()?.finish()?;
        Ok(())
    }

    fn append_archive_file(
        archive: &mut tar::Builder<GzEncoder<File>>,
        path: &str,
        bytes: &[u8],
    ) -> anyhow::Result<()> {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o755);
        header.set_cksum();
        archive.append_data(&mut header, path, bytes)?;
        Ok(())
    }

    fn file_url(path: &Path) -> String {
        let path = path.to_string_lossy().replace('\\', "/");
        format!("file:///{}", path.trim_start_matches('/'))
    }

    fn core_fixture() -> anyhow::Result<CoreFixture> {
        let files = tempfile::tempdir()?;
        let home = tempfile::tempdir()?;
        let files_path = fs::canonicalize(files.path())?;
        let home_path = fs::canonicalize(home.path())?;
        let platform = "windows-x86_64";
        let archive_root = "ldgr-core-0.2.0";
        let archive = files_path.join("core.tar.gz");
        let signature = files.path().join("core.tar.gz.sig");
        let key = SigningKey::from_bytes(&[23; 32]);
        let keyring = ReleaseKeyring {
            keys: vec![ReleasePublicKey {
                key_id: "fixture-key".to_owned(),
                public_key: STANDARD.encode(key.verifying_key().to_bytes()),
            }],
        };
        let metadata = CoreReleaseMetadata {
            schema_version: 1,
            package: "ldgr-core".to_owned(),
            binary: "ldgr".to_owned(),
            version: "0.2.0".to_owned(),
            agentctl_version: "0.2.0".to_owned(),
            agentctl_repository: "hydra-dynamix/agentctl".to_owned(),
            agentctl_commit: "b".repeat(40),
            launcher_compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA.to_owned(),
            error_recovery_schema: 1,
            platform: platform.to_owned(),
            commit: "a".repeat(40),
            source_repository: "hydra-dynamix/ldgr".to_owned(),
        };
        write_core_archive(&archive, archive_root, platform, &metadata)?;
        let archive_bytes = fs::read(&archive)?;
        fs::write(
            &signature,
            serde_json::to_vec(&DetachedSignature {
                algorithm: "Ed25519".to_owned(),
                key_id: "fixture-key".to_owned(),
                signature: STANDARD.encode(key.sign(&archive_bytes).to_bytes()),
            })?,
        )?;
        let release = CoreRelease {
            version: "0.2.0".to_owned(),
            channel: ReleaseChannel::Stable,
            minimum_updater_version: "0.1.0".to_owned(),
            core_commit: metadata.commit.clone(),
            source_repository: metadata.source_repository.clone(),
            agentctl: PairedAgentctlRelease {
                version: metadata.agentctl_version.clone(),
                repository: metadata.agentctl_repository.clone(),
                commit: metadata.agentctl_commit.clone(),
            },
            compatibility: CoreReleaseCompatibility {
                launcher_compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA.to_owned(),
                error_recovery_schema: 1,
                release_metadata_schema: 1,
                adapter_compatibility: Some(
                    crate::update::catalog::CandidateCoreAdapterCompatibilityV2::generated(),
                ),
            },
            platforms: vec![CorePlatformArchive {
                platform: platform.to_owned(),
                archive_url: file_url(&archive),
                archive_root: archive_root.to_owned(),
                sha256: format!("{:x}", Sha256::digest(&archive_bytes)),
                signature_url: file_url(&signature),
                signing_key_id: "fixture-key".to_owned(),
            }],
        };
        let catalog = CoreUpdateCatalog {
            schema_version: 1,
            release_keys: Vec::new(),
            releases: vec![release],
        };
        let catalog_signature = serde_json::to_string(&DetachedSignature {
            algorithm: "Ed25519".to_owned(),
            key_id: "fixture-key".to_owned(),
            signature: STANDARD.encode(key.sign(&canonical_catalog_bytes(&catalog)?).to_bytes()),
        })?;
        let core_catalog = verify_signed_core_update_catalog(
            &serde_json::to_string(&catalog)?,
            &catalog_signature,
            &keyring,
        )?;
        let adapter_catalog = VerifiedAdapterUpdateCatalog {
            catalog: AdapterReleaseIndex {
                schema_version: 1,
                adapters: Vec::new(),
            },
            catalog_signing_key_id: String::new(),
            archive_keyring: keyring.clone(),
        };
        let install_root = home_path.join("bin");
        fs::create_dir_all(&install_root)?;
        fs::create_dir_all(home_path.join(".ldgr"))?;
        let installed_core = install_root.join("ldgr.exe");
        let installed_agentctl = install_root.join("agentctl.exe");
        fs::write(&installed_core, "old-core")?;
        fs::write(&installed_agentctl, "old-agentctl")?;
        let receipt = CoreInstallationReceipt {
            schema_version: 1,
            installer_kind: CoreInstallerKind::Official,
            managed_by: None,
            core_version: "0.1.0".to_owned(),
            agentctl_version: "0.1.0".to_owned(),
            archive: Some(CoreArchiveProvenance {
                url: "https:".to_owned() + "//example.invalid/old.tar.gz",
                sha256: "0".repeat(64),
                signing_key_id: "old-key".to_owned(),
                platform: platform.to_owned(),
                release_commit: "c".repeat(40),
            }),
            install_root: install_root.clone(),
            core_binary_path: installed_core.clone(),
            agentctl_binary_path: installed_agentctl.clone(),
            core_binary_sha256: "0".repeat(64),
            agentctl_binary_sha256: "0".repeat(64),
            compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA.to_owned(),
            previous_successful_plan_id: None,
            installed_at_unix_seconds: 0,
        };
        let plan = build_update_plan(
            &UpdatePlanRequest {
                core_only: true,
                channel: UpdateChannel::Stable,
                offline: true,
                ..UpdatePlanRequest::default()
            },
            &VerifiedCatalogSnapshots {
                core: &core_catalog,
                adapters: &adapter_catalog.catalog,
            },
            &UpdateInventory {
                core: CoreInstallationSnapshot {
                    current_core: "0.1.0".to_owned(),
                    current_agentctl: "0.1.0".to_owned(),
                    ownership: CorePlanOwnership::ReceiptManaged,
                },
                adapters: Vec::new(),
                discovery_warnings: Vec::new(),
            },
            &Version::parse(env!("CARGO_PKG_VERSION"))?,
            platform,
        )?;
        let ownership = PlanStagingOwnership {
            home: home_path.clone(),
            core: Some(receipt),
            adapters: BTreeMap::new(),
            adapter_roots: BTreeMap::new(),
        };
        Ok(CoreFixture {
            _files: files,
            _home: home,
            home_path,
            plan,
            core_catalog,
            adapter_catalog,
            ownership,
            installed_core,
            installed_agentctl,
            archive,
            signing_key: key,
        })
    }

    struct FixedCoreProbe;

    impl CompatibilityProbe for FixedCoreProbe {
        fn probe(&self, _core: &Path, _agentctl: &Path) -> anyhow::Result<CompatibilityEvidence> {
            Ok(CompatibilityEvidence {
                core_version: "0.2.0".to_owned(),
                agentctl_version: "0.2.0".to_owned(),
                compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA.to_owned(),
            })
        }
    }

    struct FailAfter(ActivationStep);

    impl ActivationHook for FailAfter {
        fn after(&self, step: ActivationStep) -> anyhow::Result<()> {
            ensure!(step != self.0, "injected failure after {step:?}");
            Ok(())
        }
    }

    fn stage_core_fixture(
        fixture: &CoreFixture,
    ) -> anyhow::Result<(UpdateStateStore, UpdateLock, StagedUpdatePlan)> {
        let store = UpdateStateStore::open(fixture.home_path.join(".ldgr"))?;
        let lock = store.acquire_lock(
            UpdateMode::Apply,
            Some(fixture.plan.plan_id()),
            std::time::Duration::from_secs(30),
        )?;
        let client = UpdateNetworkClient::new(true)?;
        let catalogs = VerifiedStagingCatalogs {
            core: &fixture.core_catalog,
            adapters: &fixture.adapter_catalog,
        };
        let staged = stage_verified_update_plan(
            &store,
            &lock,
            &client,
            &fixture.plan,
            &catalogs,
            &fixture.ownership,
        )?;
        Ok((store, lock, staged))
    }

    #[test]
    fn paired_core_apply_writes_backups_receipt_and_terminal_component() -> anyhow::Result<()> {
        let fixture = core_fixture()?;
        let (store, lock, staged) = stage_core_fixture(&fixture)?;
        let StagedUpdatePlan {
            manifest,
            mut transaction,
        } = staged;
        store.mark_applying(&lock, fixture.plan.plan_id())?;
        let components = apply_staged_update_plan_with_services(
            &fixture.plan,
            &manifest,
            &fixture.adapter_catalog,
            &fixture.ownership,
            &mut transaction,
            true,
            &FixedCoreProbe,
            &NoopActivationHook,
        )?;
        assert_eq!(components.len(), 1);
        assert_eq!(components[0].kind, "core_bundle");
        assert_eq!(components[0].status, "applied");
        assert_eq!(fs::read_to_string(&fixture.installed_core)?, "new-core");
        assert_eq!(
            fs::read_to_string(&fixture.installed_agentctl)?,
            "new-agentctl"
        );
        assert_eq!(
            fs::read_to_string(previous_path(&fixture.installed_core)?)?,
            "old-core"
        );
        assert_eq!(
            fs::read_to_string(previous_path(&fixture.installed_agentctl)?)?,
            "old-agentctl"
        );
        let receipt: CoreInstallationReceipt = serde_json::from_slice(&fs::read(
            core_installation_receipt_path(&fixture.home_path),
        )?)?;
        assert_eq!(receipt.core_version, "0.2.0");
        assert_eq!(receipt.agentctl_version, "0.2.0");
        assert_eq!(
            receipt.previous_successful_plan_id.as_deref(),
            Some(fixture.plan.plan_id())
        );
        assert_eq!(
            receipt.core_binary_sha256,
            file_sha256(&fixture.installed_core)?
        );
        transaction.commit()?;
        store.complete_plan(
            &lock,
            fixture.plan.plan_id(),
            TerminalOutcome::Applied,
            components,
            None,
        )?;
        let history = store.read_history()?;
        assert_eq!(history[0].outcome, TerminalOutcome::Applied);
        assert_eq!(history[0].components[0].status, "applied");
        lock.release()?;
        Ok(())
    }

    #[test]
    fn resolved_plan_recovery_validates_its_embedded_digest() -> anyhow::Result<()> {
        let fixture = core_fixture()?;
        let (store, lock, staged) = stage_core_fixture(&fixture)?;
        store.mark_applying(&lock, fixture.plan.plan_id())?;
        staged.transaction.preserve_for_finalizer()?;
        lock.release()?;
        let recovery = store.acquire_lock(
            UpdateMode::Recover,
            None,
            std::time::Duration::from_secs(60),
        )?;
        let records = store.recover_interrupted(&recovery)?;
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].plan_id, fixture.plan.plan_id());
        assert_eq!(records[0].action, RecoveryAction::RollbackRequired);
        recovery.release()?;
        Ok(())
    }

    #[test]
    fn injected_failure_after_each_paired_activation_checkpoint_restores_every_target(
    ) -> anyhow::Result<()> {
        let steps = [
            ActivationStep::DestinationStaged,
            ActivationStep::AgentctlBackup,
            ActivationStep::CoreBackup,
            ActivationStep::AgentctlActivated,
            ActivationStep::CoreActivated,
            ActivationStep::PairValidated,
            ActivationStep::ReceiptActivated,
            ActivationStep::AdaptersActivated,
            ActivationStep::AdapterDiscoveryValidated,
        ];
        for step in steps {
            let fixture = core_fixture()?;
            let (store, lock, staged) = stage_core_fixture(&fixture)?;
            let StagedUpdatePlan {
                manifest,
                mut transaction,
            } = staged;
            store.mark_applying(&lock, fixture.plan.plan_id())?;
            let failure = apply_staged_update_plan_with_services(
                &fixture.plan,
                &manifest,
                &fixture.adapter_catalog,
                &fixture.ownership,
                &mut transaction,
                true,
                &FixedCoreProbe,
                &FailAfter(step),
            )
            .expect_err("injected activation failure should fail the whole plan");
            let summary = format!("{:#}", failure.source);
            assert!(summary.contains("injected failure"));
            assert_eq!(fs::read_to_string(&fixture.installed_core)?, "old-core");
            assert_eq!(
                fs::read_to_string(&fixture.installed_agentctl)?,
                "old-agentctl"
            );
            assert!(!previous_path(&fixture.installed_core)?.exists());
            assert!(!previous_path(&fixture.installed_agentctl)?.exists());
            assert!(!core_installation_receipt_path(&fixture.home_path).exists());
            assert!(!fixture
                .ownership
                .core
                .as_ref()
                .map(|receipt| destination_staged_path(
                    &receipt.core_binary_path,
                    fixture.plan.plan_id(),
                    "core"
                ))
                .transpose()?
                .expect("fixture has Core ownership")
                .exists());
            store.complete_plan(
                &lock,
                fixture.plan.plan_id(),
                TerminalOutcome::RolledBack,
                failure.components,
                Some(TerminalError {
                    code: "update.activation-failed".to_owned(),
                    summary,
                }),
            )?;
            let history = store.read_history()?;
            assert_eq!(history[0].outcome, TerminalOutcome::RolledBack);
            assert!(matches!(
                history[0].components[0].status.as_str(),
                "failed" | "rolled_back"
            ));
            lock.release()?;
        }
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn unix_pair_runs_absolute_smoke_tests_and_preserves_executable_mode() -> anyhow::Result<()> {
        use std::os::unix::fs::PermissionsExt;

        let fixture = core_fixture()?;
        let (_store, lock, staged) = stage_core_fixture(&fixture)?;
        let StagedUpdatePlan {
            manifest,
            mut transaction,
        } = staged;
        let StagedArtifact::CoreBundle {
            core_binary,
            agentctl_binary,
            ..
        } = &manifest.artifacts[0]
        else {
            panic!("expected staged Core bundle");
        };
        let report = serde_json::json!({
            "schema": LAUNCHER_COMPATIBILITY_SCHEMA,
            "compatible": true,
            "core_version": "0.2.0",
            "core_executable": fixture.installed_core,
            "agentctl_version": "0.2.0",
            "agentctl_requirement": ">=0.2.0, <0.3.0",
            "error_recovery_schema": 1
        });
        fs::write(
            core_binary,
            format!(
                "#!/bin/sh\nif [ \"$1\" = \"--version\" ]; then echo 'ldgr 0.2.0'; exit 0; fi\nprintf '%s\\n' '{}'\n",
                serde_json::to_string(&report)?
            ),
        )?;
        fs::write(agentctl_binary, "#!/bin/sh\necho 'agentctl 0.2.0'\n")?;
        for binary in [core_binary, agentctl_binary] {
            let mut permissions = fs::metadata(binary)?.permissions();
            permissions.set_mode(0o750);
            fs::set_permissions(binary, permissions)?;
        }
        let components = apply_staged_update_plan_with_services(
            &fixture.plan,
            &manifest,
            &fixture.adapter_catalog,
            &fixture.ownership,
            &mut transaction,
            true,
            &ProcessCompatibilityProbe,
            &NoopActivationHook,
        )?;
        assert_eq!(components[0].status, "applied");
        assert_ne!(
            fs::metadata(&fixture.installed_core)?.permissions().mode() & 0o111,
            0
        );
        assert_ne!(
            fs::metadata(&fixture.installed_agentctl)?
                .permissions()
                .mode()
                & 0o111,
            0
        );
        transaction.commit()?;
        lock.release()?;
        Ok(())
    }

    #[test]
    fn verified_staging_persists_plan_and_snapshots_before_activation() -> anyhow::Result<()> {
        let fixture = core_fixture()?;
        let store = UpdateStateStore::open(fixture.home_path.join(".ldgr"))?;
        let lock = store.acquire_lock(
            UpdateMode::Apply,
            Some(fixture.plan.plan_id()),
            std::time::Duration::from_secs(30),
        )?;
        let client = UpdateNetworkClient::new(true)?;
        let catalogs = VerifiedStagingCatalogs {
            core: &fixture.core_catalog,
            adapters: &fixture.adapter_catalog,
        };
        let mut staged = stage_verified_update_plan(
            &store,
            &lock,
            &client,
            &fixture.plan,
            &catalogs,
            &fixture.ownership,
        )?;
        let stage_root = store.stage_dir(fixture.plan.plan_id())?;
        assert!(stage_root.join("plan.json").is_file());
        assert!(stage_root.join("state.json").is_file());
        assert!(stage_root.join(STAGING_MANIFEST).is_file());
        assert!(stage_root.join("rollback/journal.json").is_file());
        assert_eq!(
            store.load_staged_update_plan(fixture.plan.plan_id())?.plan,
            fixture.plan
        );
        assert_eq!(staged.manifest.targets.len(), 5);
        let StagedArtifact::CoreBundle {
            core_binary,
            agentctl_binary,
            ..
        } = &staged.manifest.artifacts[0]
        else {
            panic!("expected staged Core bundle");
        };
        staged
            .transaction
            .activate_file(core_binary, &fixture.installed_core)?;
        staged
            .transaction
            .activate_file(agentctl_binary, &fixture.installed_agentctl)?;
        assert_eq!(fs::read_to_string(&fixture.installed_core)?, "new-core");
        staged.transaction.rollback()?;
        assert_eq!(fs::read_to_string(&fixture.installed_core)?, "old-core");
        assert_eq!(
            fs::read_to_string(&fixture.installed_agentctl)?,
            "old-agentctl"
        );
        lock.release()?;
        Ok(())
    }

    #[test]
    fn tampered_artifact_fails_before_first_snapshot_or_target_mutation() -> anyhow::Result<()> {
        let fixture = core_fixture()?;
        fs::write(&fixture.archive, "tampered")?;
        let store = UpdateStateStore::open(fixture.home_path.join(".ldgr"))?;
        let lock = store.acquire_lock(
            UpdateMode::Apply,
            Some(fixture.plan.plan_id()),
            std::time::Duration::from_secs(30),
        )?;
        let client = UpdateNetworkClient::new(true)?;
        let catalogs = VerifiedStagingCatalogs {
            core: &fixture.core_catalog,
            adapters: &fixture.adapter_catalog,
        };
        let error = stage_verified_update_plan(
            &store,
            &lock,
            &client,
            &fixture.plan,
            &catalogs,
            &fixture.ownership,
        )
        .err()
        .context("tampered archive unexpectedly staged")?;
        assert!(format!("{error:#}").contains("SHA-256 mismatch"));
        assert_eq!(fs::read_to_string(&fixture.installed_core)?, "old-core");
        assert_eq!(
            fs::read_to_string(&fixture.installed_agentctl)?,
            "old-agentctl"
        );
        assert!(!store
            .stage_dir(fixture.plan.plan_id())?
            .join("rollback/journal.json")
            .exists());
        lock.release()?;
        Ok(())
    }

    #[test]
    fn later_artifact_failure_occurs_after_downloads_but_before_any_plan_snapshot(
    ) -> anyhow::Result<()> {
        let mut fixture = core_fixture()?;
        let platform = fixture.plan.platform().to_owned();
        let adapter_archive = fixture._files.path().join("adapter.tar.gz");
        let adapter_signature = fixture._files.path().join("adapter.tar.gz.sig");
        write_adapter_archive(&adapter_archive, "fixture-2.0.0")?;
        let adapter_bytes = fs::read(&adapter_archive)?;
        fs::write(
            &adapter_signature,
            serde_json::to_vec(&DetachedSignature {
                algorithm: "Ed25519".to_owned(),
                key_id: "fixture-key".to_owned(),
                signature: STANDARD.encode(fixture.signing_key.sign(&adapter_bytes).to_bytes()),
            })?,
        )?;
        let adapter_catalog = AdapterReleaseIndex {
            schema_version: 1,
            adapters: vec![AdapterReleaseProduct {
                domain: "fixture".to_owned(),
                primary_namespace: "fixture".to_owned(),
                title: "Fixture".to_owned(),
                aliases: Vec::new(),
                classification: AdapterClassification::OpenSource,
                source_url: None,
                releases: vec![AdapterRelease {
                    version: "2.0.0".to_owned(),
                    channel: ReleaseChannel::Stable,
                    core_compatibility: ">=0.2.0, <0.3.0".to_owned(),
                    compatibility: None,
                    compatibility_sha256: None,
                    platforms: vec![AdapterPlatformRelease {
                        platform: platform.clone(),
                        asset_url: file_url(&adapter_archive),
                        archive_root: "fixture-2.0.0".to_owned(),
                        binary: "ldgr-fixture.exe".to_owned(),
                        sha256: format!("{:x}", Sha256::digest(&adapter_bytes)),
                        signature_url: file_url(&adapter_signature),
                        signing_key_id: "fixture-key".to_owned(),
                        resource_manifest: "adapter-resources.json".to_owned(),
                    }],
                }],
            }],
        };
        let adapter_catalog_signature = serde_json::to_string(&DetachedSignature {
            algorithm: "Ed25519".to_owned(),
            key_id: "fixture-key".to_owned(),
            signature: STANDARD.encode(
                fixture
                    .signing_key
                    .sign(&canonical_adapter_catalog_bytes(&adapter_catalog)?)
                    .to_bytes(),
            ),
        })?;
        let verified_adapter = verify_signed_adapter_update_catalog(
            &serde_json::to_string(&adapter_catalog)?,
            &adapter_catalog_signature,
            &fixture.adapter_catalog.archive_keyring,
        )?;
        fs::write(
            fixture.home_path.join(".ldgr/config.json"),
            serde_json::to_vec(&crate::harness_config::HarnessConfig::default())?,
        )?;
        let user_adapter_root = fixture.home_path.join(".ldgr/adapters");
        let install_root = user_adapter_root.join("fixture");
        fs::create_dir_all(&install_root)?;
        let empty_digest = format!("{:x}", Sha256::digest([]));
        let receipt = InstallationReceipt {
            schema_version: 1,
            domain: "fixture".to_owned(),
            version: "1.0.0".to_owned(),
            source_url: "https:".to_owned() + "//example.invalid/fixture.tar.gz",
            sha256: "0".repeat(64),
            signing_key_id: "old-key".to_owned(),
            core_compatibility: ">=0.1.0, <0.3.0".to_owned(),
            compatibility: None,
            compatibility_sha256: None,
            platform: platform.clone(),
            resource_manifest: "adapter-resources.json".to_owned(),
            installed_at_unix_seconds: 0,
            bundle_sha256: empty_digest,
            binary_path: None,
            binary_sha256: None,
            owned_resources: Vec::new(),
        };
        let plan = build_update_plan(
            &UpdatePlanRequest {
                channel: UpdateChannel::Stable,
                offline: true,
                ..UpdatePlanRequest::default()
            },
            &VerifiedCatalogSnapshots {
                core: &fixture.core_catalog,
                adapters: &verified_adapter.catalog,
            },
            &UpdateInventory {
                core: CoreInstallationSnapshot {
                    current_core: "0.1.0".to_owned(),
                    current_agentctl: "0.1.0".to_owned(),
                    ownership: CorePlanOwnership::ReceiptManaged,
                },
                adapters: vec![AdapterInstallationSnapshot {
                    slug: "fixture".to_owned(),
                    origin: AdapterOrigin::User,
                    installation: AdapterInstallationKind::Release {
                        version: "1.0.0".to_owned(),
                        core_compatibility: ">=0.1.0, <0.3.0".to_owned(),
                    },
                    compatibility: crate::update::plan::InstalledAdapterCompatibility::V2 {
                        sidecar: crate::adapter_compatibility::AdapterCompatibilitySidecarV2 {
                            format: crate::adapter_compatibility::ADAPTER_COMPATIBILITY_FORMAT_V2
                                .to_owned(),
                            adapter: "fixture".to_owned(),
                            compatibility:
                                crate::adapter_compatibility::CompatibilityRequirementsV2 {
                                    adapter_protocol_epoch: 2,
                                    minimum_core_schema: 1,
                                    required_core_capabilities: Vec::new(),
                                    central_components: Vec::new(),
                                },
                            local_stores: Vec::new(),
                        },
                    },
                }],
                discovery_warnings: Vec::new(),
            },
            &Version::parse(env!("CARGO_PKG_VERSION"))?,
            &platform,
        )?;
        fixture.ownership.adapters.insert(
            "fixture".to_owned(),
            AdapterStagingOwnership {
                install_root: install_root.clone(),
                user_adapter_root,
                receipt: AdapterOwnershipReceipt::Release(receipt),
            },
        );
        fs::write(&adapter_archive, "tampered-later-artifact")?;
        let store = UpdateStateStore::open(fixture.home_path.join(".ldgr"))?;
        let lock = store.acquire_lock(
            UpdateMode::Apply,
            Some(plan.plan_id()),
            std::time::Duration::from_secs(30),
        )?;
        let client = UpdateNetworkClient::new(true)?;
        let catalogs = VerifiedStagingCatalogs {
            core: &fixture.core_catalog,
            adapters: &verified_adapter,
        };
        assert!(stage_verified_update_plan(
            &store,
            &lock,
            &client,
            &plan,
            &catalogs,
            &fixture.ownership,
        )
        .is_err());
        let stage_root = store.stage_dir(plan.plan_id())?;
        assert!(stage_root.join("artifacts/core/archive.tar.gz").is_file());
        assert!(!stage_root.join("rollback/journal.json").exists());
        assert_eq!(fs::read_to_string(&fixture.installed_core)?, "old-core");
        assert!(fs::read_dir(install_root)?.next().is_none());
        lock.release()?;
        Ok(())
    }

    #[test]
    fn tampered_plan_and_catalog_binding_fail_before_mutation() -> anyhow::Result<()> {
        let fixture = core_fixture()?;
        let mut value = serde_json::to_value(&fixture.plan)?;
        value["platform"] = serde_json::Value::String("linux-x86_64".to_owned());
        let changed: UpdatePlan = serde_json::from_value(value)?;
        assert!(changed.verify_plan_id().is_err());

        let mut changed_catalog = fixture.core_catalog.clone();
        changed_catalog.catalog.releases[0].core_commit = "f".repeat(40);
        let catalogs = VerifiedStagingCatalogs {
            core: &changed_catalog,
            adapters: &fixture.adapter_catalog,
        };
        assert!(validate_complete_catalog_binding(&fixture.plan, &catalogs).is_err());
        assert_eq!(fs::read_to_string(&fixture.installed_core)?, "old-core");
        assert_eq!(
            fs::read_to_string(&fixture.installed_agentctl)?,
            "old-agentctl"
        );
        Ok(())
    }

    #[test]
    fn legacy_transaction_preserves_existing_adapter_rollback() -> anyhow::Result<()> {
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
    fn legacy_transaction_commit_preserves_adapter_activation() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let target = root.path().join("adapter");
        let rollback = root.path().join("rollback");
        fs::write(&target, "before")?;
        let mut transaction = InstallTransaction::new(rollback.clone())?;
        transaction.snapshot(&target)?;
        fs::write(&target, "after")?;
        transaction.commit()?;
        assert_eq!(fs::read_to_string(target)?, "after");
        assert!(!rollback.exists());
        Ok(())
    }

    #[test]
    fn whole_plan_failure_restores_every_target_in_reverse_safe_state() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let owned = root.path().join("owned");
        fs::create_dir_all(&owned)?;
        let targets = vec![
            target(&owned, "core", "core_binary", "ldgr"),
            target(&owned, "core", "agentctl_binary", "agentctl"),
            target(&owned, "fixture", "adapter_bundle", "adapter"),
            target(&owned, "fixture", "adapter_binary", "ldgr-fixture"),
            target(&owned, "fixture", "adapter_receipt", "adapter-receipt.json"),
            target(&owned, "fixture", "harness_resource", "resource"),
            target(&owned, "core", "receipt", "receipt.json"),
        ];
        for (index, target) in targets.iter().enumerate() {
            fs::write(&target.path, format!("before-{index}"))?;
        }
        let mut transaction =
            InstallTransaction::prepare(root.path().join("journal"), &"a".repeat(64), &targets)?;
        transaction.begin_activation()?;
        for (index, target) in targets.iter().enumerate() {
            fs::write(&target.path, format!("after-{index}"))?;
        }
        transaction.rollback()?;
        transaction.rollback()?;
        for (index, target) in targets.iter().enumerate() {
            assert_eq!(fs::read_to_string(&target.path)?, format!("before-{index}"));
        }
        Ok(())
    }

    #[test]
    fn activation_is_idempotent_and_requires_a_predeclared_snapshot() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let owned = root.path().join("owned");
        let staged = root.path().join("staged");
        fs::create_dir_all(&owned)?;
        fs::create_dir_all(&staged)?;
        let destination = owned.join("ldgr");
        let undeclared = owned.join("agentctl");
        let replacement = staged.join("ldgr");
        fs::write(&destination, "old")?;
        fs::write(&replacement, "new")?;
        let mut transaction = InstallTransaction::prepare(
            root.path().join("journal"),
            &"b".repeat(64),
            &[target(&owned, "core", "core_binary", "ldgr")],
        )?;
        transaction.activate_file(&replacement, &destination)?;
        transaction.activate_file(&replacement, &destination)?;
        assert_eq!(fs::read_to_string(&destination)?, "new");
        assert!(transaction
            .activate_file(&replacement, &undeclared)
            .unwrap_err()
            .to_string()
            .contains("was not snapshotted"));
        transaction.rollback()?;
        assert_eq!(fs::read_to_string(destination)?, "old");
        Ok(())
    }

    #[test]
    fn interrupted_applying_journal_can_be_resumed_and_rolled_back_twice() -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let owned = root.path().join("owned");
        fs::create_dir_all(&owned)?;
        let destination = owned.join("ldgr");
        let replacement = root.path().join("replacement");
        fs::write(&destination, "before")?;
        fs::write(&replacement, "after")?;
        let journal = root.path().join("journal");
        let mut transaction = InstallTransaction::prepare(
            journal.clone(),
            &"f".repeat(64),
            &[target(&owned, "core", "core_binary", "ldgr")],
        )?;
        transaction.activate_file(&replacement, &destination)?;
        std::mem::forget(transaction);
        let mut recovered = InstallTransaction::resume_for_rollback(journal)?;
        recovered.rollback()?;
        recovered.rollback()?;
        assert_eq!(fs::read_to_string(destination)?, "before");
        Ok(())
    }

    #[test]
    fn deterministic_retry_reuses_complete_snapshots_and_rejects_manifest_drift(
    ) -> anyhow::Result<()> {
        let root = tempfile::tempdir()?;
        let owned = root.path().join("owned");
        fs::create_dir_all(&owned)?;
        fs::write(owned.join("ldgr"), "before")?;
        let targets = vec![target(&owned, "core", "core_binary", "ldgr")];
        let journal = root.path().join("journal");
        let transaction = InstallTransaction::prepare(journal.clone(), &"c".repeat(64), &targets)?;
        std::mem::forget(transaction);
        let resumed = InstallTransaction::prepare(journal.clone(), &"c".repeat(64), &targets)?;
        std::mem::forget(resumed);
        let changed = vec![target(&owned, "core", "agentctl_binary", "agentctl")];
        assert!(InstallTransaction::prepare(journal, &"c".repeat(64), &changed).is_err());
        Ok(())
    }

    #[cfg(unix)]
    #[test]
    fn symlink_targets_and_escaping_paths_fail_closed() -> anyhow::Result<()> {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir()?;
        let owned = root.path().join("owned");
        let outside = root.path().join("outside");
        fs::create_dir_all(&owned)?;
        fs::create_dir_all(&outside)?;
        symlink(&outside, owned.join("linked"))?;
        let linked = OwnedTarget::new("core", "receipt", &owned, owned.join("linked/receipt"));
        assert!(InstallTransaction::prepare(
            root.path().join("linked-journal"),
            &"d".repeat(64),
            &[linked]
        )
        .is_err());
        let escaping = OwnedTarget::new("core", "receipt", &owned, owned.join("../outside/file"));
        assert!(InstallTransaction::prepare(
            root.path().join("escape-journal"),
            &"e".repeat(64),
            &[escaping]
        )
        .is_err());
        Ok(())
    }
}
