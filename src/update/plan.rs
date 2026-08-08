use std::collections::{BTreeMap, BTreeSet};

use anyhow::{bail, ensure, Context};
use semver::{Version, VersionReq};
use serde::{Deserialize, Serialize};
use serde_json::{Map as JsonMap, Value as JsonValue};
use sha2::{Digest, Sha256};

use crate::harness_config::UpdateChannel;
use crate::release_index::{
    resolve_release, validate_release_index, AdapterPlatformRelease, AdapterRelease,
    AdapterReleaseIndex,
};

use super::catalog::{
    resolve_newer_core_release, CorePlatformArchive, CoreRelease, VerifiedCoreUpdateCatalog,
};

pub const UPDATE_PLAN_SCHEMA_VERSION: u32 = 1;
pub const UPDATE_RESULT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct UpdatePlanRequest {
    pub core_only: bool,
    pub adapters_only: bool,
    pub adapters: Vec<String>,
    pub channel: UpdateChannel,
    pub offline: bool,
}

/// Catalogs accepted after the caller has authenticated one immutable snapshot of each.
#[derive(Clone, Debug)]
pub struct VerifiedCatalogSnapshots<'a> {
    pub core: &'a VerifiedCoreUpdateCatalog,
    pub adapters: &'a AdapterReleaseIndex,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum CorePlanOwnership {
    ReceiptManaged,
    PackageManager {
        manager: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        update_command: Option<String>,
    },
    Unmanaged {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreInstallationSnapshot {
    pub current_core: String,
    pub current_agentctl: String,
    pub ownership: CorePlanOwnership,
}

impl Default for CoreInstallationSnapshot {
    fn default() -> Self {
        Self {
            current_core: env!("CARGO_PKG_VERSION").to_owned(),
            current_agentctl: "0.0.0".to_owned(),
            ownership: CorePlanOwnership::Unmanaged {
                reason: "installation ownership was not inspected".to_owned(),
            },
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AdapterOrigin {
    User,
    Project,
    EnvironmentOverride,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "install_kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum AdapterInstallationKind {
    Release {
        version: String,
        core_compatibility: String,
    },
    LocalSource {
        package: String,
        installed_source_sha256: String,
        current_source_sha256: String,
        source_changed: bool,
    },
    Untracked {
        reason: String,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdapterInstallationSnapshot {
    pub slug: String,
    pub origin: AdapterOrigin,
    pub installation: AdapterInstallationKind,
}

#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateInventory {
    pub core: CoreInstallationSnapshot,
    #[serde(default)]
    pub adapters: Vec<AdapterInstallationSnapshot>,
    #[serde(default)]
    pub discovery_warnings: Vec<String>,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateComponentKind {
    CoreBundle,
    Adapter,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateAction {
    None,
    Update,
    ReinstallLocalSource,
    SkipUnmanaged,
    Blocked,
    Applied,
    RolledBack,
    Failed,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateCompatibility {
    Compatible,
    Incompatible,
    NotApplicable,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CoreBundleArtifact {
    pub version: String,
    pub agentctl_version: String,
    pub release: CoreRelease,
    pub platform: CorePlatformArchive,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct AdapterReleaseArtifact {
    pub domain: String,
    pub version: String,
    pub release: AdapterRelease,
    pub platform: AdapterPlatformRelease,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct LocalSourceArtifact {
    pub package: String,
    pub installed_source_sha256: String,
    pub current_source_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum UpdatePlanComponent {
    CoreBundle {
        name: String,
        current: String,
        target: String,
        current_agentctl: String,
        target_agentctl: String,
        action: UpdateAction,
        compatibility: UpdateCompatibility,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        artifact: Option<CoreBundleArtifact>,
    },
    Adapter {
        name: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        current: Option<String>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        target: Option<String>,
        action: UpdateAction,
        compatibility: UpdateCompatibility,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        release: Option<AdapterReleaseArtifact>,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        local_source: Option<LocalSourceArtifact>,
    },
}

impl UpdatePlanComponent {
    pub fn kind(&self) -> UpdateComponentKind {
        match self {
            Self::CoreBundle { .. } => UpdateComponentKind::CoreBundle,
            Self::Adapter { .. } => UpdateComponentKind::Adapter,
        }
    }

    pub fn name(&self) -> &str {
        match self {
            Self::CoreBundle { name, .. } | Self::Adapter { name, .. } => name,
        }
    }

    pub fn action(&self) -> UpdateAction {
        match self {
            Self::CoreBundle { action, .. } | Self::Adapter { action, .. } => *action,
        }
    }

    pub fn compatibility(&self) -> UpdateCompatibility {
        match self {
            Self::CoreBundle { compatibility, .. } | Self::Adapter { compatibility, .. } => {
                *compatibility
            }
        }
    }

    pub fn current(&self) -> Option<&str> {
        match self {
            Self::CoreBundle { current, .. } => Some(current),
            Self::Adapter { current, .. } => current.as_deref(),
        }
    }

    pub fn target(&self) -> Option<&str> {
        match self {
            Self::CoreBundle { target, .. } => Some(target),
            Self::Adapter { target, .. } => target.as_deref(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdatePlan {
    schema_version: u32,
    plan_id: String,
    current_core: String,
    target_core: String,
    platform: String,
    channel: UpdateChannel,
    offline: bool,
    blocked: bool,
    components: Vec<UpdatePlanComponent>,
    warnings: Vec<String>,
}

impl UpdatePlan {
    pub fn schema_version(&self) -> u32 {
        self.schema_version
    }
    pub fn plan_id(&self) -> &str {
        &self.plan_id
    }
    pub fn current_core(&self) -> &str {
        &self.current_core
    }
    pub fn target_core(&self) -> &str {
        &self.target_core
    }
    pub fn platform(&self) -> &str {
        &self.platform
    }
    pub fn channel(&self) -> UpdateChannel {
        self.channel
    }
    pub fn offline(&self) -> bool {
        self.offline
    }
    pub fn blocked(&self) -> bool {
        self.blocked
    }
    pub fn components(&self) -> &[UpdatePlanComponent] {
        &self.components
    }
    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn update_available(&self) -> bool {
        self.components.iter().any(|component| {
            matches!(
                component.action(),
                UpdateAction::Update | UpdateAction::ReinstallLocalSource
            )
        })
    }

    pub fn verify_plan_id(&self) -> anyhow::Result<()> {
        let actual = plan_digest(&PlanDigestPayload::from_plan(self))?;
        ensure!(
            actual == self.plan_id,
            "update plan digest mismatch: expected {}, got {actual}",
            self.plan_id
        );
        Ok(())
    }

    pub fn check_result(&self) -> UpdateResult {
        UpdateResult {
            schema_version: UPDATE_RESULT_SCHEMA_VERSION,
            mode: UpdateResultMode::Check,
            status: if self.blocked {
                UpdateResultStatus::Blocked
            } else if self.update_available() {
                UpdateResultStatus::UpdatesAvailable
            } else {
                UpdateResultStatus::Current
            },
            current_core: self.current_core.clone(),
            target_core: self.target_core.clone(),
            platform: self.platform.clone(),
            channel: self.channel,
            components: self
                .components
                .iter()
                .map(UpdateResultComponent::from_plan_component)
                .collect(),
            warnings: self.warnings.clone(),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateResultMode {
    Check,
    Apply,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum UpdateResultStatus {
    Current,
    UpdatesAvailable,
    Applied,
    StagedPendingRestart,
    Blocked,
    Failed,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateResultComponent {
    pub kind: UpdateComponentKind,
    pub name: String,
    pub current: Option<String>,
    pub target: Option<String>,
    pub action: UpdateAction,
    pub compatibility: UpdateCompatibility,
}

impl UpdateResultComponent {
    fn from_plan_component(component: &UpdatePlanComponent) -> Self {
        Self {
            kind: component.kind(),
            name: component.name().to_owned(),
            current: component.current().map(str::to_owned),
            target: component.target().map(str::to_owned),
            action: component.action(),
            compatibility: component.compatibility(),
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct UpdateResult {
    pub schema_version: u32,
    pub mode: UpdateResultMode,
    pub status: UpdateResultStatus,
    pub current_core: String,
    pub target_core: String,
    pub platform: String,
    pub channel: UpdateChannel,
    pub components: Vec<UpdateResultComponent>,
    pub warnings: Vec<String>,
}

#[derive(Clone, Debug, Serialize)]
struct PlanDigestPayload<'a> {
    schema_version: u32,
    current_core: &'a str,
    target_core: &'a str,
    platform: &'a str,
    channel: UpdateChannel,
    offline: bool,
    blocked: bool,
    components: &'a [UpdatePlanComponent],
    warnings: &'a [String],
}

impl<'a> PlanDigestPayload<'a> {
    fn from_plan(plan: &'a UpdatePlan) -> Self {
        Self {
            schema_version: plan.schema_version,
            current_core: &plan.current_core,
            target_core: &plan.target_core,
            platform: &plan.platform,
            channel: plan.channel,
            offline: plan.offline,
            blocked: plan.blocked,
            components: &plan.components,
            warnings: &plan.warnings,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct NormalizedSelection {
    include_core: bool,
    include_all_adapters: bool,
    adapters: BTreeSet<String>,
}

pub fn build_update_plan(
    request: &UpdatePlanRequest,
    catalogs: &VerifiedCatalogSnapshots<'_>,
    inventory: &UpdateInventory,
    updater_version: &Version,
    platform: &str,
) -> anyhow::Result<UpdatePlan> {
    let selection = normalize_selection(request)?;
    if !catalogs.adapters.adapters.is_empty() {
        validate_release_index(catalogs.adapters).context("adapter catalog snapshot is invalid")?;
    }
    let current_core = Version::parse(&inventory.core.current_core)
        .context("installed Core version is not semantic")?;
    Version::parse(&inventory.core.current_agentctl)
        .context("installed agentctl version is not semantic")?;

    let resolved_core = if selection.include_core {
        resolve_newer_core_release(
            catalogs.core,
            &current_core,
            updater_version,
            platform,
            request.channel == UpdateChannel::Prerelease,
        )?
    } else {
        None
    };
    let resolved_target_core = resolved_core
        .as_ref()
        .map_or_else(|| current_core.clone(), |resolved| resolved.version.clone());
    let target_core = resolved_target_core.to_string();
    let mut components = Vec::new();
    let mut warnings = inventory.discovery_warnings.clone();
    let mut blocked = false;

    if selection.include_core {
        let (target_agentctl, artifact) = resolved_core.as_ref().map_or_else(
            || (inventory.core.current_agentctl.clone(), None),
            |resolved| {
                let mut release = resolved.release.clone();
                release
                    .platforms
                    .sort_by(|left, right| left.platform.cmp(&right.platform));
                (
                    resolved.release.agentctl.version.clone(),
                    Some(CoreBundleArtifact {
                        version: resolved.version.to_string(),
                        agentctl_version: resolved.release.agentctl.version.clone(),
                        release,
                        platform: resolved.platform.clone(),
                    }),
                )
            },
        );
        let update_available = artifact.is_some();
        let (action, compatibility) = if !update_available {
            (UpdateAction::None, UpdateCompatibility::Compatible)
        } else if request.offline
            && artifact
                .as_ref()
                .is_some_and(|value| !core_artifact_is_offline(&value.platform))
        {
            blocked = true;
            warnings.push(
                "Core update requires remote artifacts that are unavailable in offline mode"
                    .to_owned(),
            );
            (UpdateAction::Blocked, UpdateCompatibility::Compatible)
        } else {
            match &inventory.core.ownership {
                CorePlanOwnership::ReceiptManaged => {
                    (UpdateAction::Update, UpdateCompatibility::Compatible)
                }
                CorePlanOwnership::PackageManager {
                    manager,
                    update_command,
                } => {
                    blocked = true;
                    let instruction = update_command
                        .as_deref()
                        .map_or_else(String::new, |command| format!("; run `{command}`"));
                    warnings.push(format!(
                        "Core is managed by {manager} and is check-only{instruction}"
                    ));
                    (
                        UpdateAction::SkipUnmanaged,
                        UpdateCompatibility::NotApplicable,
                    )
                }
                CorePlanOwnership::Unmanaged { reason } => {
                    blocked = true;
                    warnings.push(format!(
                        "Core self-update ownership is unavailable: {reason}"
                    ));
                    (
                        UpdateAction::SkipUnmanaged,
                        UpdateCompatibility::NotApplicable,
                    )
                }
            }
        };
        components.push(UpdatePlanComponent::CoreBundle {
            name: "ldgr-core".to_owned(),
            current: inventory.core.current_core.clone(),
            target: target_core.clone(),
            current_agentctl: inventory.core.current_agentctl.clone(),
            target_agentctl,
            action,
            compatibility,
            artifact,
        });
    }

    let adapters_by_slug = adapters_by_slug(&inventory.adapters)?;
    if selection.include_all_adapters || !selection.adapters.is_empty() {
        let selected_slugs = if selection.include_all_adapters {
            adapters_by_slug.keys().cloned().collect::<BTreeSet<_>>()
        } else {
            selection.adapters.clone()
        };
        for slug in selected_slugs {
            let Some(adapter) = adapters_by_slug.get(&slug) else {
                warnings.push(format!(
                    "adapter `{slug}` was requested but was not discovered; skipped"
                ));
                components.push(skipped_adapter(&slug, None));
                continue;
            };
            let (component, component_blocked, mut component_warnings) = plan_adapter(
                adapter,
                catalogs.adapters,
                &resolved_target_core,
                platform,
                request.channel,
                request.offline,
            )?;
            blocked |= component_blocked;
            warnings.append(&mut component_warnings);
            components.push(component);
        }
    } else if selection.include_core && resolved_core.is_some() {
        append_core_only_incompatibility_warnings(
            &mut warnings,
            adapters_by_slug.values().copied(),
            &resolved_target_core,
        )?;
    }

    components.sort_by(component_order);
    warnings = warnings
        .into_iter()
        .map(|warning| redact_absolute_paths(&warning))
        .collect();
    warnings.sort();
    warnings.dedup();
    let mut plan = UpdatePlan {
        schema_version: UPDATE_PLAN_SCHEMA_VERSION,
        plan_id: String::new(),
        current_core: inventory.core.current_core.clone(),
        target_core,
        platform: platform.to_owned(),
        channel: request.channel,
        offline: request.offline,
        blocked,
        components,
        warnings,
    };
    plan.plan_id = plan_digest(&PlanDigestPayload::from_plan(&plan))?;
    Ok(plan)
}

fn normalize_selection(request: &UpdatePlanRequest) -> anyhow::Result<NormalizedSelection> {
    ensure!(
        !(request.core_only && request.adapters_only),
        "--core-only conflicts with --adapters-only"
    );
    ensure!(
        !request.core_only || request.adapters.is_empty(),
        "--core-only conflicts with --adapter"
    );
    let mut adapters = BTreeSet::new();
    for adapter in &request.adapters {
        let adapter = adapter.trim();
        ensure!(!adapter.is_empty(), "--adapter requires a non-empty slug");
        ensure!(
            !adapter.chars().any(char::is_whitespace),
            "adapter selector `{adapter}` must not contain whitespace"
        );
        adapters.insert(adapter.to_owned());
    }
    Ok(if request.core_only {
        NormalizedSelection {
            include_core: true,
            include_all_adapters: false,
            adapters,
        }
    } else if request.adapters_only || !adapters.is_empty() {
        NormalizedSelection {
            include_core: false,
            include_all_adapters: adapters.is_empty(),
            adapters,
        }
    } else {
        NormalizedSelection {
            include_core: true,
            include_all_adapters: true,
            adapters,
        }
    })
}

fn adapters_by_slug(
    adapters: &[AdapterInstallationSnapshot],
) -> anyhow::Result<BTreeMap<String, &AdapterInstallationSnapshot>> {
    let mut by_slug = BTreeMap::new();
    for adapter in adapters {
        let slug = adapter.slug.trim();
        ensure!(
            !slug.is_empty(),
            "discovered adapter slug must not be empty"
        );
        ensure!(
            !slug.chars().any(char::is_whitespace),
            "discovered adapter slug `{slug}` must not contain whitespace"
        );
        if by_slug.insert(slug.to_owned(), adapter).is_some() {
            bail!("duplicate discovered adapter slug `{slug}`");
        }
    }
    Ok(by_slug)
}

fn append_core_only_incompatibility_warnings<'a>(
    warnings: &mut Vec<String>,
    adapters: impl Iterator<Item = &'a AdapterInstallationSnapshot>,
    target_core: &Version,
) -> anyhow::Result<()> {
    for adapter in adapters {
        if adapter.origin != AdapterOrigin::User {
            continue;
        }
        if let AdapterInstallationKind::Release {
            version,
            core_compatibility,
        } = &adapter.installation
        {
            let compatible = VersionReq::parse(core_compatibility)
                .with_context(|| {
                    format!(
                        "adapter `{}` receipt Core compatibility is invalid",
                        adapter.slug
                    )
                })?
                .matches(target_core);
            if !compatible {
                warnings.push(format!(
                    "adapter `{}` at {version} is incompatible with target Core {target_core}; --core-only leaves it unchanged",
                    adapter.slug
                ));
            }
        }
    }
    Ok(())
}

fn plan_adapter(
    adapter: &AdapterInstallationSnapshot,
    catalog: &AdapterReleaseIndex,
    target_core: &Version,
    platform: &str,
    channel: UpdateChannel,
    offline: bool,
) -> anyhow::Result<(UpdatePlanComponent, bool, Vec<String>)> {
    if adapter.origin != AdapterOrigin::User {
        let origin = match adapter.origin {
            AdapterOrigin::User => unreachable!(),
            AdapterOrigin::Project => "project adapter root",
            AdapterOrigin::EnvironmentOverride => "LDGR_ADAPTER_PATH override",
        };
        return Ok((
            skipped_adapter(
                &adapter.slug,
                installed_adapter_identity(&adapter.installation),
            ),
            false,
            vec![format!(
                "adapter `{}` was discovered through {origin}; skipped",
                adapter.slug
            )],
        ));
    }
    match &adapter.installation {
        AdapterInstallationKind::Untracked { reason } => Ok((
            skipped_adapter(&adapter.slug, None),
            false,
            vec![format!(
                "adapter `{}` has no valid installation receipt; skipped: {reason}",
                adapter.slug
            )],
        )),
        AdapterInstallationKind::LocalSource {
            package,
            installed_source_sha256,
            current_source_sha256,
            source_changed,
        } => {
            ensure!(
                !package.trim().is_empty(),
                "adapter `{}` local source package is empty",
                adapter.slug
            );
            ensure!(
                !installed_source_sha256.trim().is_empty()
                    && !current_source_sha256.trim().is_empty(),
                "adapter `{}` local source digest is empty",
                adapter.slug
            );
            Ok((
                UpdatePlanComponent::Adapter {
                    name: adapter.slug.clone(),
                    current: Some(installed_source_sha256.clone()),
                    target: Some(current_source_sha256.clone()),
                    action: if *source_changed {
                        UpdateAction::ReinstallLocalSource
                    } else {
                        UpdateAction::None
                    },
                    compatibility: UpdateCompatibility::Compatible,
                    release: None,
                    local_source: Some(LocalSourceArtifact {
                        package: package.clone(),
                        installed_source_sha256: installed_source_sha256.clone(),
                        current_source_sha256: current_source_sha256.clone(),
                    }),
                },
                false,
                Vec::new(),
            ))
        }
        AdapterInstallationKind::Release {
            version,
            core_compatibility,
        } => plan_release_adapter(
            adapter,
            version,
            core_compatibility,
            catalog,
            target_core,
            platform,
            channel,
            offline,
        ),
    }
}

#[allow(clippy::too_many_arguments)]
fn plan_release_adapter(
    adapter: &AdapterInstallationSnapshot,
    version: &str,
    core_compatibility: &str,
    catalog: &AdapterReleaseIndex,
    target_core: &Version,
    platform: &str,
    channel: UpdateChannel,
    offline: bool,
) -> anyhow::Result<(UpdatePlanComponent, bool, Vec<String>)> {
    let installed_version = Version::parse(version)
        .with_context(|| format!("adapter `{}` receipt version is invalid", adapter.slug))?;
    let installed_compatibility = VersionReq::parse(core_compatibility).with_context(|| {
        format!(
            "adapter `{}` receipt Core compatibility is invalid",
            adapter.slug
        )
    })?;
    let installed_is_compatible = installed_compatibility.matches(target_core);
    let resolved = resolve_release(
        catalog,
        &adapter.slug,
        target_core,
        platform,
        None,
        channel == UpdateChannel::Prerelease,
    );
    match resolved {
        Ok(resolved) if resolved.version > installed_version => {
            let mut release = resolved.release.clone();
            release
                .platforms
                .sort_by(|left, right| left.platform.cmp(&right.platform));
            let artifact = AdapterReleaseArtifact {
                domain: resolved.adapter.domain.clone(),
                version: resolved.version.to_string(),
                release,
                platform: resolved.platform.clone(),
            };
            if offline && !adapter_artifact_is_offline(&artifact.platform) {
                return Ok((
                    blocked_adapter(&adapter.slug, version, Some(&artifact.version)),
                    true,
                    vec![format!(
                        "adapter `{}` update requires remote artifacts that are unavailable in offline mode",
                        adapter.slug
                    )],
                ));
            }
            Ok((
                UpdatePlanComponent::Adapter {
                    name: adapter.slug.clone(),
                    current: Some(version.to_owned()),
                    target: Some(artifact.version.clone()),
                    action: UpdateAction::Update,
                    compatibility: UpdateCompatibility::Compatible,
                    release: Some(artifact),
                    local_source: None,
                },
                false,
                Vec::new(),
            ))
        }
        Ok(_) if installed_is_compatible => Ok((
            current_adapter(&adapter.slug, version),
            false,
            Vec::new(),
        )),
        Ok(resolved) => Ok((
            blocked_adapter(
                &adapter.slug,
                version,
                Some(&resolved.version.to_string()),
            ),
            true,
            vec![format!(
                "adapter `{}` has no upgrade-only release compatible with target Core {target_core}",
                adapter.slug
            )],
        )),
        Err(error) if installed_is_compatible => Ok((
            current_adapter(&adapter.slug, version),
            false,
            vec![format!(
                "adapter `{}` is absent from the compatible catalog snapshot; retaining compatible installed version {version}: {error}",
                adapter.slug
            )],
        )),
        Err(error) => Ok((
            blocked_adapter(&adapter.slug, version, None),
            true,
            vec![format!(
                "adapter `{}` is incompatible with target Core {target_core}: {error}",
                adapter.slug
            )],
        )),
    }
}

fn current_adapter(name: &str, version: &str) -> UpdatePlanComponent {
    UpdatePlanComponent::Adapter {
        name: name.to_owned(),
        current: Some(version.to_owned()),
        target: Some(version.to_owned()),
        action: UpdateAction::None,
        compatibility: UpdateCompatibility::Compatible,
        release: None,
        local_source: None,
    }
}

fn skipped_adapter(name: &str, current: Option<String>) -> UpdatePlanComponent {
    UpdatePlanComponent::Adapter {
        name: name.to_owned(),
        current,
        target: None,
        action: UpdateAction::SkipUnmanaged,
        compatibility: UpdateCompatibility::NotApplicable,
        release: None,
        local_source: None,
    }
}

fn blocked_adapter(name: &str, current: &str, target: Option<&str>) -> UpdatePlanComponent {
    UpdatePlanComponent::Adapter {
        name: name.to_owned(),
        current: Some(current.to_owned()),
        target: target.map(str::to_owned),
        action: UpdateAction::Blocked,
        compatibility: UpdateCompatibility::Incompatible,
        release: None,
        local_source: None,
    }
}

fn installed_adapter_identity(installation: &AdapterInstallationKind) -> Option<String> {
    match installation {
        AdapterInstallationKind::Release { version, .. } => Some(version.clone()),
        AdapterInstallationKind::LocalSource {
            installed_source_sha256,
            ..
        } => Some(installed_source_sha256.clone()),
        AdapterInstallationKind::Untracked { .. } => None,
    }
}

fn core_artifact_is_offline(platform: &CorePlatformArchive) -> bool {
    local_artifact(&platform.archive_url) && local_artifact(&platform.signature_url)
}

fn adapter_artifact_is_offline(platform: &AdapterPlatformRelease) -> bool {
    local_artifact(&platform.asset_url) && local_artifact(&platform.signature_url)
}

fn local_artifact(source: &str) -> bool {
    source.starts_with("file://")
}

fn redact_absolute_paths(text: &str) -> String {
    text.split_whitespace()
        .map(|token| {
            let candidate = token.trim_matches(|character: char| {
                matches!(
                    character,
                    '`' | '\'' | '"' | '(' | ')' | '[' | ']' | ',' | ';'
                )
            });
            let bytes = candidate.as_bytes();
            let windows_absolute = bytes.len() >= 3
                && bytes[0].is_ascii_alphabetic()
                && bytes[1] == b':'
                && matches!(bytes[2], b'/' | b'\\');
            let unix_absolute = candidate.starts_with('/') && !candidate.starts_with("//");
            let unc_absolute = candidate.starts_with("\\");
            if windows_absolute || unix_absolute || unc_absolute {
                "<path>"
            } else {
                token
            }
        })
        .collect::<Vec<_>>()
        .join(" ")
}

fn component_order(left: &UpdatePlanComponent, right: &UpdatePlanComponent) -> std::cmp::Ordering {
    let rank = |component: &UpdatePlanComponent| match component {
        UpdatePlanComponent::CoreBundle { .. } => 0,
        UpdatePlanComponent::Adapter { .. } => 1,
    };
    rank(left)
        .cmp(&rank(right))
        .then_with(|| left.name().cmp(right.name()))
}

fn plan_digest(payload: &PlanDigestPayload<'_>) -> anyhow::Result<String> {
    let value = serde_json::to_value(payload).context("failed to encode update plan")?;
    let canonical = canonical_json_value(value);
    let bytes = serde_json::to_vec(&canonical).context("failed to serialize update plan")?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

fn canonical_json_value(value: JsonValue) -> JsonValue {
    match value {
        JsonValue::Array(values) => {
            JsonValue::Array(values.into_iter().map(canonical_json_value).collect())
        }
        JsonValue::Object(values) => JsonValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, canonical_json_value(value)))
                .collect::<JsonMap<_, _>>(),
        ),
        value => value,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::release_index::{
        AdapterClassification, AdapterReleaseProduct, ReleaseChannel, ReleaseKeyring,
    };
    use crate::update::catalog::{
        CoreReleaseCompatibility, PairedAgentctlRelease, VerifiedCoreUpdateCatalog,
        CORE_RELEASE_METADATA_SCHEMA_VERSION, ERROR_RECOVERY_SCHEMA_VERSION,
        LAUNCHER_COMPATIBILITY_SCHEMA_V1,
    };

    const PLATFORM: &str = "windows-x86_64";

    fn platform(version: &str, local: bool) -> AdapterPlatformRelease {
        let prefix = if local {
            "file:///fixtures"
        } else {
            "https://example.invalid"
        };
        AdapterPlatformRelease {
            platform: PLATFORM.to_owned(),
            asset_url: format!("{prefix}/adapter-{version}.tar.gz"),
            archive_root: format!("adapter-{version}"),
            binary: "ldgr-fixture".to_owned(),
            sha256: "a".repeat(64),
            signature_url: format!("{prefix}/adapter-{version}.tar.gz.sig"),
            signing_key_id: "fixture".to_owned(),
            resource_manifest: "adapter-resources.json".to_owned(),
        }
    }

    fn adapter_release(
        version: &str,
        channel: ReleaseChannel,
        compatibility: &str,
    ) -> AdapterRelease {
        AdapterRelease {
            version: version.to_owned(),
            channel,
            core_compatibility: compatibility.to_owned(),
            platforms: vec![platform(version, true)],
        }
    }

    fn adapter_product(slug: &str, releases: Vec<AdapterRelease>) -> AdapterReleaseProduct {
        AdapterReleaseProduct {
            domain: slug.to_owned(),
            primary_namespace: slug.to_owned(),
            title: slug.to_owned(),
            aliases: Vec::new(),
            classification: AdapterClassification::OpenSource,
            source_url: None,
            releases,
        }
    }

    fn core_platform(version: &str, local: bool) -> CorePlatformArchive {
        let prefix = if local {
            "file:///fixtures"
        } else {
            "https://example.invalid"
        };
        CorePlatformArchive {
            platform: PLATFORM.to_owned(),
            archive_url: format!("{prefix}/ldgr-{version}.tar.gz"),
            archive_root: format!("ldgr-{version}"),
            sha256: "b".repeat(64),
            signature_url: format!("{prefix}/ldgr-{version}.tar.gz.sig"),
            signing_key_id: "fixture".to_owned(),
        }
    }

    fn core_release(version: &str, channel: ReleaseChannel) -> CoreRelease {
        CoreRelease {
            version: version.to_owned(),
            channel,
            minimum_updater_version: "0.1.0".to_owned(),
            core_commit: format!("core-{version}"),
            source_repository: "https://example.invalid/core".to_owned(),
            agentctl: PairedAgentctlRelease {
                version: version.to_owned(),
                repository: "https://example.invalid/agentctl".to_owned(),
                commit: format!("agentctl-{version}"),
            },
            compatibility: CoreReleaseCompatibility {
                launcher_compatibility_schema: LAUNCHER_COMPATIBILITY_SCHEMA_V1.to_owned(),
                error_recovery_schema: ERROR_RECOVERY_SCHEMA_VERSION,
                release_metadata_schema: CORE_RELEASE_METADATA_SCHEMA_VERSION,
            },
            platforms: vec![core_platform(version, true)],
        }
    }

    fn catalogs(
        core_releases: Vec<CoreRelease>,
        adapters: Vec<AdapterReleaseProduct>,
    ) -> (VerifiedCoreUpdateCatalog, AdapterReleaseIndex) {
        (
            VerifiedCoreUpdateCatalog {
                catalog: crate::update::catalog::CoreUpdateCatalog {
                    schema_version: 1,
                    release_keys: Vec::new(),
                    releases: core_releases,
                },
                catalog_signing_key_id: "fixture".to_owned(),
                archive_keyring: ReleaseKeyring { keys: Vec::new() },
            },
            AdapterReleaseIndex {
                schema_version: 1,
                adapters,
            },
        )
    }

    fn inventory(adapters: Vec<AdapterInstallationSnapshot>) -> UpdateInventory {
        UpdateInventory {
            core: CoreInstallationSnapshot {
                current_core: "1.0.0".to_owned(),
                current_agentctl: "1.0.0".to_owned(),
                ownership: CorePlanOwnership::ReceiptManaged,
            },
            adapters,
            discovery_warnings: Vec::new(),
        }
    }

    fn release_install(
        slug: &str,
        version: &str,
        compatibility: &str,
    ) -> AdapterInstallationSnapshot {
        AdapterInstallationSnapshot {
            slug: slug.to_owned(),
            origin: AdapterOrigin::User,
            installation: AdapterInstallationKind::Release {
                version: version.to_owned(),
                core_compatibility: compatibility.to_owned(),
            },
        }
    }

    fn plan(
        request: UpdatePlanRequest,
        core_releases: Vec<CoreRelease>,
        adapter_releases: Vec<AdapterReleaseProduct>,
        inventory: UpdateInventory,
    ) -> anyhow::Result<UpdatePlan> {
        let (core, adapters) = catalogs(core_releases, adapter_releases);
        build_update_plan(
            &request,
            &VerifiedCatalogSnapshots {
                core: &core,
                adapters: &adapters,
            },
            &inventory,
            &Version::parse("1.0.0")?,
            PLATFORM,
        )
    }

    #[test]
    fn current_plan_and_json_result_are_explicit_no_ops() -> anyhow::Result<()> {
        let plan = plan(
            UpdatePlanRequest::default(),
            vec![core_release("1.0.0", ReleaseChannel::Stable)],
            vec![adapter_product(
                "research",
                vec![adapter_release("1.0.0", ReleaseChannel::Stable, ">=1.0.0")],
            )],
            inventory(vec![release_install("research", "1.0.0", ">=1.0.0")]),
        )?;
        assert!(!plan.blocked());
        assert!(!plan.update_available());
        assert_eq!(plan.components()[0].kind(), UpdateComponentKind::CoreBundle);
        assert!(plan
            .components()
            .iter()
            .all(|component| component.action() == UpdateAction::None));
        let result = plan.check_result();
        assert_eq!(result.status, UpdateResultStatus::Current);
        assert_eq!(serde_json::to_value(result)?["schema_version"], 1);
        plan.verify_plan_id()?;
        Ok(())
    }

    #[test]
    fn resolves_core_pair_then_adapters_against_target_core() -> anyhow::Result<()> {
        let plan = plan(
            UpdatePlanRequest::default(),
            vec![core_release("2.0.0", ReleaseChannel::Stable)],
            vec![adapter_product(
                "research",
                vec![
                    adapter_release("1.5.0", ReleaseChannel::Stable, ">=2.0.0"),
                    adapter_release("1.1.0", ReleaseChannel::Stable, ">=1.0.0, <2.0.0"),
                ],
            )],
            inventory(vec![release_install(
                "research",
                "1.0.0",
                ">=1.0.0, <2.0.0",
            )]),
        )?;
        assert_eq!(plan.target_core(), "2.0.0");
        assert_eq!(plan.components()[0].action(), UpdateAction::Update);
        assert_eq!(plan.components()[1].action(), UpdateAction::Update);
        assert_eq!(plan.components()[1].target(), Some("1.5.0"));
        assert_eq!(
            plan.check_result().status,
            UpdateResultStatus::UpdatesAvailable
        );
        Ok(())
    }

    #[test]
    fn selectors_validate_conflicts_and_use_current_core_for_adapter_only() -> anyhow::Result<()> {
        let (core, adapters) = catalogs(
            vec![core_release("2.0.0", ReleaseChannel::Stable)],
            vec![
                adapter_product(
                    "conduct",
                    vec![adapter_release("1.1.0", ReleaseChannel::Stable, ">=1.0.0")],
                ),
                adapter_product(
                    "research",
                    vec![adapter_release("1.1.0", ReleaseChannel::Stable, ">=1.0.0")],
                ),
            ],
        );
        let inventory = inventory(vec![
            release_install("research", "1.0.0", ">=1.0.0"),
            release_install("conduct", "1.0.0", ">=1.0.0"),
        ]);
        let request = UpdatePlanRequest {
            adapters: vec!["research".to_owned(), "research".to_owned()],
            ..UpdatePlanRequest::default()
        };
        let plan = build_update_plan(
            &request,
            &VerifiedCatalogSnapshots {
                core: &core,
                adapters: &adapters,
            },
            &inventory,
            &Version::parse("1.0.0")?,
            PLATFORM,
        )?;
        assert_eq!(plan.target_core(), "1.0.0");
        assert_eq!(plan.components().len(), 1);
        assert_eq!(plan.components()[0].name(), "research");
        let conflict = UpdatePlanRequest {
            core_only: true,
            adapters: vec!["research".to_owned()],
            ..UpdatePlanRequest::default()
        };
        assert!(build_update_plan(
            &conflict,
            &VerifiedCatalogSnapshots {
                core: &core,
                adapters: &adapters,
            },
            &inventory,
            &Version::parse("1.0.0")?,
            PLATFORM,
        )
        .unwrap_err()
        .to_string()
        .contains("conflicts"));
        Ok(())
    }

    #[test]
    fn prerelease_is_opt_in_for_core_and_adapter() -> anyhow::Result<()> {
        let make_plan = |channel| {
            plan(
                UpdatePlanRequest {
                    channel,
                    ..UpdatePlanRequest::default()
                },
                vec![
                    core_release("2.0.0-beta.1", ReleaseChannel::Prerelease),
                    core_release("1.1.0", ReleaseChannel::Stable),
                ],
                vec![adapter_product(
                    "research",
                    vec![
                        adapter_release(
                            "2.0.0-beta.1",
                            ReleaseChannel::Prerelease,
                            ">=2.0.0-beta.1",
                        ),
                        adapter_release("1.1.0", ReleaseChannel::Stable, ">=1.0.0"),
                    ],
                )],
                inventory(vec![release_install("research", "1.0.0", ">=1.0.0")]),
            )
        };
        let stable = make_plan(UpdateChannel::Stable)?;
        assert_eq!(stable.target_core(), "1.1.0");
        let prerelease = make_plan(UpdateChannel::Prerelease)?;
        assert_eq!(prerelease.target_core(), "2.0.0-beta.1");
        assert_eq!(prerelease.components()[1].target(), Some("2.0.0-beta.1"));
        Ok(())
    }

    #[test]
    fn local_source_drift_is_planned_without_paths_or_release_provenance() -> anyhow::Result<()> {
        let source = |changed| AdapterInstallationSnapshot {
            slug: "local".to_owned(),
            origin: AdapterOrigin::User,
            installation: AdapterInstallationKind::LocalSource {
                package: "ldgr-local-adapter".to_owned(),
                installed_source_sha256: "installed-digest".to_owned(),
                current_source_sha256: if changed {
                    "changed-digest".to_owned()
                } else {
                    "installed-digest".to_owned()
                },
                source_changed: changed,
            },
        };
        for (changed, action) in [
            (false, UpdateAction::None),
            (true, UpdateAction::ReinstallLocalSource),
        ] {
            let plan = plan(
                UpdatePlanRequest {
                    adapters_only: true,
                    ..UpdatePlanRequest::default()
                },
                Vec::new(),
                Vec::new(),
                inventory(vec![source(changed)]),
            )?;
            assert_eq!(plan.components()[0].action(), action);
            let encoded = serde_json::to_string(&plan)?;
            assert!(!encoded.contains("source_root"));
            assert!(!encoded.contains("verified_release"));
        }
        Ok(())
    }

    #[test]
    fn project_override_and_untracked_adapters_are_explicit_skips() -> anyhow::Result<()> {
        let plan = plan(
            UpdatePlanRequest {
                adapters_only: true,
                ..UpdatePlanRequest::default()
            },
            Vec::new(),
            Vec::new(),
            inventory(vec![
                AdapterInstallationSnapshot {
                    slug: "override".to_owned(),
                    origin: AdapterOrigin::EnvironmentOverride,
                    installation: AdapterInstallationKind::Untracked {
                        reason: "development checkout".to_owned(),
                    },
                },
                AdapterInstallationSnapshot {
                    slug: "project".to_owned(),
                    origin: AdapterOrigin::Project,
                    installation: AdapterInstallationKind::Untracked {
                        reason: "project checkout".to_owned(),
                    },
                },
                AdapterInstallationSnapshot {
                    slug: "untracked".to_owned(),
                    origin: AdapterOrigin::User,
                    installation: AdapterInstallationKind::Untracked {
                        reason: "receipt missing".to_owned(),
                    },
                },
            ]),
        )?;
        assert!(plan.components().iter().all(|component| {
            component.action() == UpdateAction::SkipUnmanaged
                && component.compatibility() == UpdateCompatibility::NotApplicable
        }));
        assert!(plan
            .warnings()
            .iter()
            .any(|warning| warning.contains("LDGR_ADAPTER_PATH")));
        assert!(plan
            .warnings()
            .iter()
            .any(|warning| warning.contains("project adapter root")));
        assert!(plan
            .warnings()
            .iter()
            .any(|warning| warning.contains("no valid installation receipt")));
        Ok(())
    }

    #[test]
    fn incompatible_adapter_blocks_default_but_core_only_warns() -> anyhow::Result<()> {
        let core_releases = vec![core_release("2.0.0", ReleaseChannel::Stable)];
        let adapter_releases = vec![adapter_product(
            "research",
            vec![adapter_release(
                "1.0.0",
                ReleaseChannel::Stable,
                ">=1.0.0, <2.0.0",
            )],
        )];
        let installed_inventory = inventory(vec![release_install(
            "research",
            "1.0.0",
            ">=1.0.0, <2.0.0",
        )]);
        let default_plan = plan(
            UpdatePlanRequest::default(),
            core_releases.clone(),
            adapter_releases.clone(),
            installed_inventory.clone(),
        )?;
        assert!(default_plan.blocked());
        assert_eq!(
            default_plan.check_result().status,
            UpdateResultStatus::Blocked
        );
        assert_eq!(default_plan.components()[1].action(), UpdateAction::Blocked);
        let core_only = plan(
            UpdatePlanRequest {
                core_only: true,
                ..UpdatePlanRequest::default()
            },
            core_releases,
            adapter_releases,
            installed_inventory,
        )?;
        assert!(!core_only.blocked());
        assert_eq!(core_only.components().len(), 1);
        assert!(core_only
            .warnings()
            .iter()
            .any(|warning| warning.contains("--core-only")));

        let core_only_with_available_adapter = plan(
            UpdatePlanRequest {
                core_only: true,
                ..UpdatePlanRequest::default()
            },
            vec![core_release("2.0.0", ReleaseChannel::Stable)],
            vec![adapter_product(
                "research",
                vec![adapter_release("1.1.0", ReleaseChannel::Stable, ">=2.0.0")],
            )],
            inventory(vec![release_install(
                "research",
                "1.0.0",
                ">=1.0.0, <2.0.0",
            )]),
        )?;
        assert!(core_only_with_available_adapter
            .warnings()
            .iter()
            .any(|warning| warning.contains("--core-only")));
        Ok(())
    }

    #[test]
    fn offline_blocks_remote_updates_but_accepts_local_artifacts() -> anyhow::Result<()> {
        let mut remote_core = core_release("2.0.0", ReleaseChannel::Stable);
        remote_core.platforms = vec![core_platform("2.0.0", false)];
        let remote = plan(
            UpdatePlanRequest {
                core_only: true,
                offline: true,
                ..UpdatePlanRequest::default()
            },
            vec![remote_core],
            Vec::new(),
            inventory(Vec::new()),
        )?;
        assert!(remote.blocked());
        assert_eq!(remote.components()[0].action(), UpdateAction::Blocked);
        let local = plan(
            UpdatePlanRequest {
                core_only: true,
                offline: true,
                ..UpdatePlanRequest::default()
            },
            vec![core_release("2.0.0", ReleaseChannel::Stable)],
            Vec::new(),
            inventory(Vec::new()),
        )?;
        assert!(!local.blocked());
        assert_eq!(local.components()[0].action(), UpdateAction::Update);

        let mut remote_adapter_release =
            adapter_release("1.1.0", ReleaseChannel::Stable, ">=1.0.0");
        remote_adapter_release.platforms = vec![platform("1.1.0", false)];
        let remote_adapter = plan(
            UpdatePlanRequest {
                adapters_only: true,
                offline: true,
                ..UpdatePlanRequest::default()
            },
            Vec::new(),
            vec![adapter_product("research", vec![remote_adapter_release])],
            inventory(vec![release_install("research", "1.0.0", ">=1.0.0")]),
        )?;
        assert!(remote_adapter.blocked());
        assert_eq!(
            remote_adapter.components()[0].action(),
            UpdateAction::Blocked
        );
        Ok(())
    }

    #[test]
    fn requested_missing_adapter_and_package_managed_core_are_explicit() -> anyhow::Result<()> {
        let mut managed = inventory(Vec::new());
        managed.core.ownership = CorePlanOwnership::PackageManager {
            manager: "cargo".to_owned(),
            update_command: Some("cargo install --force ldgr-core".to_owned()),
        };
        let core = plan(
            UpdatePlanRequest {
                core_only: true,
                ..UpdatePlanRequest::default()
            },
            vec![core_release("2.0.0", ReleaseChannel::Stable)],
            Vec::new(),
            managed,
        )?;
        assert!(core.blocked());
        assert_eq!(core.components()[0].action(), UpdateAction::SkipUnmanaged);
        assert!(core
            .warnings()
            .iter()
            .any(|warning| warning.contains("cargo")));

        let missing = plan(
            UpdatePlanRequest {
                adapters: vec!["missing".to_owned()],
                ..UpdatePlanRequest::default()
            },
            Vec::new(),
            Vec::new(),
            inventory(Vec::new()),
        )?;
        assert_eq!(missing.components()[0].name(), "missing");
        assert_eq!(
            missing.components()[0].action(),
            UpdateAction::SkipUnmanaged
        );
        assert!(missing
            .warnings()
            .iter()
            .any(|warning| warning.contains("was not discovered")));
        Ok(())
    }

    #[test]
    fn component_warning_order_and_plan_digest_are_deterministic() -> anyhow::Result<()> {
        let releases = vec![
            adapter_product(
                "alpha",
                vec![adapter_release("1.1.0", ReleaseChannel::Stable, ">=1.0.0")],
            ),
            adapter_product(
                "zeta",
                vec![adapter_release("1.1.0", ReleaseChannel::Stable, ">=1.0.0")],
            ),
        ];
        let alpha = release_install("alpha", "1.0.0", ">=1.0.0");
        let zeta = release_install("zeta", "1.0.0", ">=1.0.0");
        let mut first_inventory = inventory(vec![zeta.clone(), alpha.clone()]);
        first_inventory.discovery_warnings = vec![
            "z-warning".to_owned(),
            "a-warning".to_owned(),
            "failed to inspect C:\\Users\\alice\\project".to_owned(),
        ];
        let mut second_inventory = inventory(vec![alpha, zeta]);
        second_inventory.discovery_warnings = vec![
            "a-warning".to_owned(),
            "failed to inspect /home/bob/project".to_owned(),
            "z-warning".to_owned(),
        ];
        let first = plan(
            UpdatePlanRequest::default(),
            Vec::new(),
            releases.clone(),
            first_inventory,
        )?;
        let second = plan(
            UpdatePlanRequest::default(),
            Vec::new(),
            releases,
            second_inventory,
        )?;
        assert_eq!(first.plan_id(), second.plan_id());
        assert_eq!(first, second);
        assert_eq!(
            first
                .components()
                .iter()
                .map(UpdatePlanComponent::name)
                .collect::<Vec<_>>(),
            vec!["ldgr-core", "alpha", "zeta"]
        );
        assert_eq!(
            first.warnings(),
            &["a-warning", "failed to inspect <path>", "z-warning"]
        );
        assert!(!serde_json::to_string(&first)?.contains("alice"));
        Ok(())
    }

    #[test]
    fn digest_verification_detects_serialized_plan_tampering() -> anyhow::Result<()> {
        let plan = plan(
            UpdatePlanRequest::default(),
            Vec::new(),
            Vec::new(),
            inventory(Vec::new()),
        )?;
        let mut value = serde_json::to_value(&plan)?;
        value["target_core"] = JsonValue::String("9.9.9".to_owned());
        let tampered: UpdatePlan = serde_json::from_value(value)?;
        assert!(tampered.verify_plan_id().is_err());
        Ok(())
    }
}
