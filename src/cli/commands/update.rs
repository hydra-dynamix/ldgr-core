use std::collections::BTreeMap;
use std::env;
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use semver::Version;
use serde::Serialize;

use crate::adapter_registry::AdapterRegistry;
use crate::cli::args::UpdateArgs;
use crate::harness_config::UpdateChannel;
use crate::release_index::{AdapterReleaseIndex, ReleaseKeyring};
use crate::update::apply::{
    apply_staged_update_plan, stage_verified_update_plan, PlanStagingOwnership, StagedUpdatePlan,
    VerifiedStagingCatalogs,
};
use crate::update::catalog::{
    fetch_signed_adapter_update_catalog, fetch_signed_core_update_catalog, AdapterCatalogFetch,
    AdapterCatalogSources, CoreCatalogFetch, CoreCatalogSources, CoreUpdateCatalog,
    VerifiedAdapterUpdateCatalog, VerifiedCoreUpdateCatalog,
};
use crate::update::installation::{
    resolve_current_core_installation, CoreInstallationOwnership, LegacyAdoptionAuthorization,
    LegacyAdoptionConsent,
};
use crate::update::network::UpdateNetworkClient;
use crate::update::plan::{
    build_update_plan, AdapterInstallationKind, AdapterInstallationSnapshot, AdapterOrigin,
    CoreInstallationSnapshot, CorePlanOwnership, UpdateAction, UpdateComponentKind,
    UpdateInventory, UpdatePlan, UpdatePlanRequest, UpdateResult, UpdateResultMode,
    UpdateResultStatus, VerifiedCatalogSnapshots,
};
use crate::update::state::{
    CachedCheckResult, ComponentResult, TerminalError, TerminalOutcome, UpdateCache, UpdateLock,
    UpdateMode, UpdateStateStore, SCHEMA_VERSION,
};

const CHECK_LOCK_LEASE: Duration = Duration::from_secs(30);
const APPLY_LOCK_LEASE: Duration = Duration::from_secs(60 * 60);

pub fn handle_update(args: UpdateArgs) -> anyhow::Result<()> {
    if args.check {
        return handle_check(args);
    }
    handle_apply(args)
}

struct ResolvedUpdate {
    plan: UpdatePlan,
    core_etag: Option<String>,
    core_catalog: VerifiedCoreUpdateCatalog,
    adapter_catalog: VerifiedAdapterUpdateCatalog,
    ownership: PlanStagingOwnership,
}

fn handle_check(args: UpdateArgs) -> anyhow::Result<()> {
    let home = user_home()?;
    let state = UpdateStateStore::open(home.join(".ldgr"))?;
    let previous_cache = state.load_cache()?;
    let lock = state.acquire_lock(UpdateMode::Check, None, CHECK_LOCK_LEASE)?;
    let plan = run_check(
        &args,
        &home,
        &state,
        previous_cache.as_ref(),
        lock,
        "explicit update check failed",
    )?;

    let result = plan.check_result();
    render_result(&result, args.json)?;
    if result.status == UpdateResultStatus::Blocked {
        bail!("update.no-compatible-release: the resolved update plan is blocked");
    }
    Ok(())
}

fn handle_apply(args: UpdateArgs) -> anyhow::Result<()> {
    let home = user_home()?;
    let state = UpdateStateStore::open(home.join(".ldgr"))?;
    let previous_cache = state.load_cache()?;
    let lock = state.acquire_lock(UpdateMode::Apply, None, APPLY_LOCK_LEASE)?;
    let resolved = match resolve_update(&args, &home) {
        Ok(resolved) => resolved,
        Err(error) => {
            lock.release()?;
            return Err(error);
        }
    };
    state.write_cache(&success_cache(
        previous_cache.as_ref(),
        &resolved.plan,
        resolved.core_etag.clone(),
    )?)?;

    if resolved.plan.blocked() {
        let result = apply_result(&resolved.plan, UpdateResultStatus::Blocked, &[]);
        render_result(&result, args.json)?;
        lock.release()?;
        bail!("update.no-compatible-release: the resolved update plan is blocked");
    }
    #[cfg(not(any(unix, windows)))]
    if resolved.plan.components().iter().any(|component| {
        component.kind() == UpdateComponentKind::CoreBundle
            && component.action() == UpdateAction::Update
    }) {
        lock.release()?;
        bail!(
            "update.apply-platform-unavailable: this build cannot yet activate a selected Core/agentctl bundle; use `--adapters-only` to apply adapter updates"
        );
    }
    if !resolved.plan.update_available() {
        let result = apply_result(&resolved.plan, UpdateResultStatus::Current, &[]);
        render_result(&result, args.json)?;
        lock.release()?;
        return Ok(());
    }

    confirm_application(&args, &resolved.plan)?;
    let client = UpdateNetworkClient::new(args.offline)?;
    let catalogs = VerifiedStagingCatalogs {
        core: &resolved.core_catalog,
        adapters: &resolved.adapter_catalog,
    };
    let staged = match stage_verified_update_plan(
        &state,
        &lock,
        &client,
        &resolved.plan,
        &catalogs,
        &resolved.ownership,
    ) {
        Ok(staged) => staged,
        Err(error) => {
            let components = failed_component_results(&resolved.plan);
            if state.load_staging_state(resolved.plan.plan_id()).is_ok() {
                state.complete_plan(
                    &lock,
                    resolved.plan.plan_id(),
                    TerminalOutcome::Failed,
                    components.clone(),
                    Some(TerminalError {
                        code: "update.staging-failed".to_owned(),
                        summary: format!("{error:#}"),
                    }),
                )?;
            }
            let result = apply_result(&resolved.plan, UpdateResultStatus::Failed, &components);
            render_result(&result, args.json)?;
            lock.release()?;
            return Err(anyhow::anyhow!("update.staging-failed: {error:#}"));
        }
    };
    #[cfg(windows)]
    if resolved.plan.components().iter().any(|component| {
        component.kind() == UpdateComponentKind::CoreBundle
            && component.action() == UpdateAction::Update
    }) {
        let (executable, token) = crate::update::finalizer::prepare_foreground_finalizer(
            &state,
            &lock,
            &resolved.plan,
            staged,
            &resolved.ownership,
            &resolved.adapter_catalog,
        )?;
        crate::update::finalizer::launch_foreground_finalizer(
            &state,
            lock,
            resolved.plan.plan_id(),
            &executable,
            &token,
        )?;
        let result = apply_result(
            &resolved.plan,
            UpdateResultStatus::StagedPendingRestart,
            &[],
        );
        render_result(&result, args.json)?;
        return Ok(());
    }
    state.mark_applying(&lock, resolved.plan.plan_id())?;
    let StagedUpdatePlan {
        manifest,
        mut transaction,
    } = staged;
    match apply_staged_update_plan(
        &resolved.plan,
        &manifest,
        &resolved.adapter_catalog,
        &resolved.ownership,
        &mut transaction,
        args.json,
    ) {
        Ok(components) => {
            transaction.commit()?;
            state.complete_plan(
                &lock,
                resolved.plan.plan_id(),
                TerminalOutcome::Applied,
                components.clone(),
                None,
            )?;
            state.write_cache(&current_cache(
                previous_cache.as_ref(),
                resolved.core_etag.clone(),
            )?)?;
            let result = apply_result(&resolved.plan, UpdateResultStatus::Applied, &components);
            render_result(&result, args.json)?;
            lock.release()?;
            Ok(())
        }
        Err(failure) => {
            drop(transaction);
            state.complete_plan(
                &lock,
                resolved.plan.plan_id(),
                TerminalOutcome::RolledBack,
                failure.components.clone(),
                Some(TerminalError {
                    code: "update.activation-failed".to_owned(),
                    summary: format!("{:#}", failure.source),
                }),
            )?;
            let result = apply_result(
                &resolved.plan,
                UpdateResultStatus::Failed,
                &failure.components,
            );
            render_result(&result, args.json)?;
            lock.release()?;
            Err(anyhow::anyhow!(
                "update.activation-failed: {:#}",
                failure.source
            ))
        }
    }
}

pub fn handle_startup_check_worker(owner_token: &str) -> anyhow::Result<()> {
    let (home, config, state, lock) = crate::update::startup::startup_worker_context(owner_token)?;
    let args = UpdateArgs {
        check: true,
        json: true,
        yes: false,
        core_only: !config.include_adapters,
        adapters_only: false,
        adapters: Vec::new(),
        prerelease: config.channel == UpdateChannel::Prerelease,
        offline: false,
    };
    let previous_cache = state.load_cache()?;
    run_check(
        &args,
        &home,
        &state,
        previous_cache.as_ref(),
        lock,
        "automatic update check failed",
    )?;
    Ok(())
}

fn run_check(
    args: &UpdateArgs,
    home: &Path,
    state: &UpdateStateStore,
    previous_cache: Option<&UpdateCache>,
    lock: UpdateLock,
    failure_summary: &str,
) -> anyhow::Result<UpdatePlan> {
    let resolved = resolve_update(args, home);
    let resolved = match resolved {
        Ok(value) => value,
        Err(error) => {
            let code = stable_error_code(&error);
            let cache = failed_cache(previous_cache, code, failure_summary)?;
            state
                .write_cache(&cache)
                .context("failed to persist failed update check state")?;
            lock.release()?;
            return Err(error);
        }
    };
    state.write_cache(&success_cache(
        previous_cache,
        &resolved.plan,
        resolved.core_etag,
    )?)?;
    lock.release()?;
    Ok(resolved.plan)
}

fn resolve_update(args: &UpdateArgs, home: &Path) -> anyhow::Result<ResolvedUpdate> {
    let request = UpdatePlanRequest {
        core_only: args.core_only,
        adapters_only: args.adapters_only,
        adapters: args.adapters.clone(),
        channel: if args.prerelease {
            UpdateChannel::Prerelease
        } else {
            UpdateChannel::Stable
        },
        offline: args.offline,
    };
    let (inventory, ownership) = inspect_inventory(home, args.yes)
        .context("update.unmanaged-installation: failed to inspect installed ownership")?;
    let client = UpdateNetworkClient::new(args.offline)?;
    let include_core = args.core_only || (!args.adapters_only && args.adapters.is_empty());

    let (core, core_etag) = if include_core {
        let sources = CoreCatalogSources::configured(args.offline).map_err(catalog_error)?;
        match fetch_signed_core_update_catalog(&client, &sources, None).map_err(catalog_error)? {
            CoreCatalogFetch::Modified { verified, etag } => (verified, etag),
            CoreCatalogFetch::NotModified { .. } => {
                bail!("update.catalog-unavailable: Core catalog returned not-modified without a cached snapshot")
            }
        }
    } else {
        (empty_core_catalog(), None)
    };

    let adapter_catalog = if adapter_catalog_required(args, &inventory) {
        let sources = AdapterCatalogSources::configured(args.offline).map_err(catalog_error)?;
        match fetch_signed_adapter_update_catalog(&client, &sources, None).map_err(catalog_error)? {
            AdapterCatalogFetch::Modified { verified, .. } => verified,
            AdapterCatalogFetch::NotModified { .. } => {
                bail!("update.catalog-unavailable: adapter catalog returned not-modified without a cached snapshot")
            }
        }
    } else {
        empty_adapter_catalog()
    };
    let platform = platform_tag()?;
    let updater_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let plan = build_update_plan(
        &request,
        &VerifiedCatalogSnapshots {
            core: &core,
            adapters: &adapter_catalog.catalog,
        },
        &inventory,
        &updater_version,
        &platform,
    )
    .context("update.no-compatible-release: failed to resolve a compatible update plan")?;
    Ok(ResolvedUpdate {
        plan,
        core_etag,
        core_catalog: core,
        adapter_catalog,
        ownership,
    })
}

fn empty_core_catalog() -> VerifiedCoreUpdateCatalog {
    VerifiedCoreUpdateCatalog {
        catalog: CoreUpdateCatalog {
            schema_version: 1,
            release_keys: Vec::new(),
            releases: Vec::new(),
        },
        catalog_signing_key_id: String::new(),
        archive_keyring: ReleaseKeyring { keys: Vec::new() },
    }
}

fn empty_adapter_catalog() -> VerifiedAdapterUpdateCatalog {
    VerifiedAdapterUpdateCatalog {
        catalog: AdapterReleaseIndex {
            schema_version: 1,
            adapters: Vec::new(),
        },
        catalog_signing_key_id: String::new(),
        archive_keyring: ReleaseKeyring { keys: Vec::new() },
    }
}

fn adapter_catalog_required(args: &UpdateArgs, inventory: &UpdateInventory) -> bool {
    if args.core_only {
        return false;
    }
    inventory.adapters.iter().any(|adapter| {
        matches!(
            adapter.installation,
            AdapterInstallationKind::Release { .. }
        ) && (args.adapters.is_empty() || args.adapters.iter().any(|name| name == &adapter.slug))
    })
}

fn inspect_inventory(
    home: &Path,
    yes: bool,
) -> anyhow::Result<(UpdateInventory, PlanStagingOwnership)> {
    let ownership =
        resolve_current_core_installation(home, LegacyAdoptionConsent::NonInteractive { yes })?;
    let core_receipt = match &ownership {
        CoreInstallationOwnership::OfficialInstall(receipt) => Some(receipt.clone()),
        _ => None,
    };
    let core = core_snapshot(ownership);
    let registry = AdapterRegistry::discover();
    let mut discovery_warnings = registry
        .warnings
        .iter()
        .map(|warning| format!("adapter discovery warning: {}", warning.message))
        .collect::<Vec<_>>();
    let roots = AdapterRoots::discover(home)?;
    let mut adapters = Vec::new();
    let mut adapter_ownership = BTreeMap::new();
    for installed in &registry.adapters {
        let origin = roots.origin(&installed.root_path);
        let installation = if origin == AdapterOrigin::User {
            let Some(user_root) = roots.user_root(&installed.root_path) else {
                discovery_warnings.push(format!(
                    "adapter `{}` is not eligible: install root is not a direct child of the user adapter root",
                    installed.slug
                ));
                adapters.push(AdapterInstallationSnapshot {
                    slug: installed.slug.clone(),
                    origin,
                    installation: AdapterInstallationKind::Untracked {
                        reason: "install root is outside its canonical user adapter boundary"
                            .to_owned(),
                    },
                });
                continue;
            };
            match crate::update::adapter::inspect_adapter_for_bulk(installed, home, user_root) {
                Ok((snapshot, owned)) => {
                    adapter_ownership.insert(installed.slug.clone(), owned);
                    snapshot
                }
                Err(error) => {
                    let detail = format!("{error:#}");
                    let reason = if detail.contains("modified") {
                        "receipt-owned files are modified"
                    } else {
                        "receipt ownership validation failed"
                    };
                    discovery_warnings.push(format!(
                        "adapter `{}` is not eligible: {reason}",
                        installed.slug
                    ));
                    AdapterInstallationKind::Untracked {
                        reason: reason.to_owned(),
                    }
                }
            }
        } else {
            AdapterInstallationKind::Untracked {
                reason: "development or project adapter root".to_owned(),
            }
        };
        adapters.push(AdapterInstallationSnapshot {
            slug: installed.slug.clone(),
            origin,
            installation,
        });
    }
    Ok((
        UpdateInventory {
            core,
            adapters,
            discovery_warnings,
        },
        PlanStagingOwnership {
            home: home.to_path_buf(),
            core: core_receipt,
            adapters: adapter_ownership,
        },
    ))
}

fn core_snapshot(ownership: CoreInstallationOwnership) -> CoreInstallationSnapshot {
    match ownership {
        CoreInstallationOwnership::OfficialInstall(receipt) => CoreInstallationSnapshot {
            current_core: receipt.core_version,
            current_agentctl: receipt.agentctl_version,
            ownership: CorePlanOwnership::ReceiptManaged,
        },
        CoreInstallationOwnership::PackageManagerCheckOnly {
            managed_by,
            update_command,
            ..
        } => CoreInstallationSnapshot {
            current_core: env!("CARGO_PKG_VERSION").to_owned(),
            current_agentctl: super::ops::AGENTCTL_VERSION.to_owned(),
            ownership: CorePlanOwnership::PackageManager {
                manager: format!("{managed_by:?}").to_ascii_lowercase(),
                update_command,
            },
        },
        CoreInstallationOwnership::LegacyAdoption(candidate) => CoreInstallationSnapshot {
            current_core: candidate.evidence.core_version,
            current_agentctl: candidate.evidence.agentctl_version,
            ownership: if candidate.authorization == LegacyAdoptionAuthorization::Approved {
                CorePlanOwnership::ReceiptManaged
            } else {
                CorePlanOwnership::Unmanaged {
                    reason: "legacy installation adoption requires confirmation".to_owned(),
                }
            },
        },
        CoreInstallationOwnership::Unmanaged { reason } => CoreInstallationSnapshot {
            current_core: env!("CARGO_PKG_VERSION").to_owned(),
            current_agentctl: super::ops::AGENTCTL_VERSION.to_owned(),
            ownership: CorePlanOwnership::Unmanaged { reason },
        },
    }
}

struct AdapterRoots {
    environment: Vec<PathBuf>,
    project: PathBuf,
    user: Vec<PathBuf>,
}

impl AdapterRoots {
    fn discover(home: &Path) -> anyhow::Result<Self> {
        let environment = env::var_os("LDGR_ADAPTER_PATH")
            .map(|paths| env::split_paths(&paths).collect::<Vec<_>>())
            .unwrap_or_default()
            .into_iter()
            .map(|path| absolute_path(&path))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let project = absolute_path(&PathBuf::from(".ldgr/adapters"))?;
        let mut user = Vec::new();
        if let Some(ldgr_home) = env::var_os("LDGR_HOME") {
            user.push(absolute_path(&PathBuf::from(ldgr_home).join("adapters"))?);
        }
        user.push(absolute_path(&home.join(".ldgr/adapters"))?);
        Ok(Self {
            environment,
            project,
            user,
        })
    }

    fn origin(&self, path: &Path) -> AdapterOrigin {
        let path = absolute_path(path).unwrap_or_else(|_| path.to_path_buf());
        if self.environment.iter().any(|root| within(&path, root)) {
            AdapterOrigin::EnvironmentOverride
        } else if within(&path, &self.project) {
            AdapterOrigin::Project
        } else if self.user.iter().any(|root| within(&path, root)) {
            AdapterOrigin::User
        } else {
            AdapterOrigin::EnvironmentOverride
        }
    }

    fn user_root(&self, path: &Path) -> Option<&Path> {
        let path = absolute_path(path).ok()?;
        self.user
            .iter()
            .find(|root| path.parent().is_some_and(|parent| parent == root.as_path()))
            .map(PathBuf::as_path)
    }
}

fn absolute_path(path: &Path) -> anyhow::Result<PathBuf> {
    match path.canonicalize() {
        Ok(path) => Ok(path),
        Err(_) if path.is_absolute() => Ok(path.to_path_buf()),
        Err(_) => Ok(env::current_dir()?.join(path)),
    }
}

fn within(path: &Path, root: &Path) -> bool {
    path == root || path.starts_with(root)
}

fn user_home() -> anyhow::Result<PathBuf> {
    env::var_os("HOME")
        .or_else(|| env::var_os("USERPROFILE"))
        .map(PathBuf::from)
        .context("could not determine home directory from HOME/USERPROFILE")
}

fn platform_tag() -> anyhow::Result<String> {
    let arch = match env::consts::ARCH {
        "x86_64" => "x86_64",
        "aarch64" => "aarch64",
        other => bail!("update.no-compatible-release: unsupported update architecture `{other}`"),
    };
    match env::consts::OS {
        "linux" => Ok(format!("linux-{arch}")),
        "macos" => Ok(format!("macos-{arch}")),
        "windows" => Ok(format!("windows-{arch}")),
        other => bail!("update.no-compatible-release: unsupported update OS `{other}`"),
    }
}

fn success_cache(
    previous: Option<&UpdateCache>,
    plan: &UpdatePlan,
    catalog_etag: Option<String>,
) -> anyhow::Result<UpdateCache> {
    let adapter_updates = plan
        .components()
        .iter()
        .filter(|component| {
            component.kind() == UpdateComponentKind::Adapter
                && matches!(
                    component.action(),
                    UpdateAction::Update | UpdateAction::ReinstallLocalSource
                )
        })
        .count() as u32;
    let result = if plan.blocked() {
        CachedCheckResult::Failed {
            code: "update.no-compatible-release".to_owned(),
            summary: "the resolved update plan is blocked".to_owned(),
        }
    } else if plan.update_available() {
        CachedCheckResult::UpdatesAvailable {
            plan_id: plan.plan_id().to_owned(),
            target_core: plan.target_core().to_owned(),
            adapter_updates,
        }
    } else {
        CachedCheckResult::Current
    };
    Ok(UpdateCache {
        schema_version: SCHEMA_VERSION,
        checked_at_unix_ms: now_ms()?,
        result,
        catalog_etag,
        consecutive_failures: 0,
        last_notice: previous.and_then(|cache| cache.last_notice.clone()),
        notice_history: previous
            .map(|cache| cache.notice_history.clone())
            .unwrap_or_default(),
    })
}

fn failed_cache(
    previous: Option<&UpdateCache>,
    code: &str,
    summary: &str,
) -> anyhow::Result<UpdateCache> {
    Ok(UpdateCache {
        schema_version: SCHEMA_VERSION,
        checked_at_unix_ms: now_ms()?,
        result: CachedCheckResult::Failed {
            code: code.to_owned(),
            summary: summary.to_owned(),
        },
        catalog_etag: previous.and_then(|cache| cache.catalog_etag.clone()),
        consecutive_failures: previous
            .map_or(1, |cache| cache.consecutive_failures.saturating_add(1)),
        last_notice: previous.and_then(|cache| cache.last_notice.clone()),
        notice_history: previous
            .map(|cache| cache.notice_history.clone())
            .unwrap_or_default(),
    })
}

fn current_cache(
    previous: Option<&UpdateCache>,
    catalog_etag: Option<String>,
) -> anyhow::Result<UpdateCache> {
    Ok(UpdateCache {
        schema_version: SCHEMA_VERSION,
        checked_at_unix_ms: now_ms()?,
        result: CachedCheckResult::Current,
        catalog_etag,
        consecutive_failures: 0,
        last_notice: previous.and_then(|cache| cache.last_notice.clone()),
        notice_history: previous
            .map(|cache| cache.notice_history.clone())
            .unwrap_or_default(),
    })
}

fn now_ms() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis()
        .try_into()
        .context("update check timestamp overflow")
}

fn confirm_application(args: &UpdateArgs, plan: &UpdatePlan) -> anyhow::Result<()> {
    if args.yes {
        return Ok(());
    }
    let preview = apply_result(plan, UpdateResultStatus::UpdatesAvailable, &[]);
    if !io::stdin().is_terminal() {
        render_result(&preview, args.json)?;
        bail!("update.confirmation-required: non-interactive update application requires `--yes`");
    }
    if !args.json {
        render_result(&preview, false)?;
    }
    eprint!("Apply this update plan? [y/N] ");
    io::stderr().flush()?;
    let mut answer = String::new();
    io::stdin().read_line(&mut answer)?;
    if !matches!(answer.trim().to_ascii_lowercase().as_str(), "y" | "yes") {
        if args.json {
            render_result(&preview, true)?;
        }
        bail!("update.confirmation-declined: update plan was not applied");
    }
    Ok(())
}

fn apply_result(
    plan: &UpdatePlan,
    status: UpdateResultStatus,
    terminal: &[ComponentResult],
) -> UpdateResult {
    let mut result = plan.check_result();
    result.mode = UpdateResultMode::Apply;
    result.status = status;
    for component in &mut result.components {
        if let Some(recorded) = terminal
            .iter()
            .find(|recorded| recorded.name == component.name)
        {
            component.action = action_from_status(&recorded.status);
        }
    }
    result
}

fn action_from_status(status: &str) -> UpdateAction {
    match status {
        "none" => UpdateAction::None,
        "update" => UpdateAction::Update,
        "reinstall_local_source" => UpdateAction::ReinstallLocalSource,
        "skip_unmanaged" => UpdateAction::SkipUnmanaged,
        "blocked" => UpdateAction::Blocked,
        "applied" => UpdateAction::Applied,
        "rolled_back" => UpdateAction::RolledBack,
        _ => UpdateAction::Failed,
    }
}

fn failed_component_results(plan: &UpdatePlan) -> Vec<ComponentResult> {
    plan.components()
        .iter()
        .filter(|component| component.kind() == UpdateComponentKind::Adapter)
        .map(|component| ComponentResult {
            kind: "adapter".to_owned(),
            name: component.name().to_owned(),
            status: if matches!(
                component.action(),
                UpdateAction::Update | UpdateAction::ReinstallLocalSource
            ) {
                "failed"
            } else {
                match component.action() {
                    UpdateAction::None => "none",
                    UpdateAction::SkipUnmanaged => "skip_unmanaged",
                    UpdateAction::Blocked => "blocked",
                    UpdateAction::Applied => "applied",
                    UpdateAction::RolledBack => "rolled_back",
                    UpdateAction::Failed => "failed",
                    UpdateAction::Update | UpdateAction::ReinstallLocalSource => unreachable!(),
                }
            }
            .to_owned(),
        })
        .collect()
}

fn render_result(result: &UpdateResult, json: bool) -> anyhow::Result<()> {
    if json {
        let stdout = io::stdout();
        let mut output = stdout.lock();
        serde_json::to_writer(&mut output, result)?;
        writeln!(output)?;
        return Ok(());
    }
    println!(
        "mode={} status={} current_core={} target_core={} platform={} channel={}",
        json_name(&result.mode)?,
        json_name(&result.status)?,
        result.current_core,
        result.target_core,
        result.platform,
        json_name(&result.channel)?,
    );
    for component in &result.components {
        println!(
            "component kind={} name={} current={} target={} action={} compatibility={}",
            json_name(&component.kind)?,
            component.name,
            component.current.as_deref().unwrap_or("-"),
            component.target.as_deref().unwrap_or("-"),
            json_name(&component.action)?,
            json_name(&component.compatibility)?,
        );
    }
    for warning in &result.warnings {
        eprintln!("warning: {warning}");
    }
    Ok(())
}

fn json_name<T: Serialize>(value: &T) -> anyhow::Result<String> {
    let value = serde_json::to_value(value)?;
    value
        .as_str()
        .map(str::to_owned)
        .context("update result enum did not serialize as a string")
}

fn catalog_error(error: anyhow::Error) -> anyhow::Error {
    let detail = format!("{error:#}");
    let untrusted = [
        "untrusted",
        "did not verify",
        "unknown release signing key",
        "not valid",
        "invalid",
        "unsupported detached signature",
        "does not match indexed key",
    ]
    .iter()
    .any(|marker| detail.contains(marker));
    let code = if untrusted {
        "update.catalog-untrusted"
    } else {
        "update.catalog-unavailable"
    };
    anyhow::anyhow!("{code}: {detail}")
}

fn stable_error_code(error: &anyhow::Error) -> &'static str {
    let detail = format!("{error:#}");
    for code in [
        "update.catalog-unavailable",
        "update.catalog-untrusted",
        "update.no-compatible-release",
        "update.unmanaged-installation",
        "update.locked",
    ] {
        if detail.contains(code) {
            return code;
        }
    }
    "update.failed"
}
