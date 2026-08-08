use std::env;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context};
use semver::Version;
use serde::Serialize;

use crate::adapter_registry::AdapterRegistry;
use crate::cli::args::UpdateArgs;
use crate::harness_config::UpdateChannel;
use crate::release_index::{AdapterReleaseIndex, ReleaseKeyring};
use crate::update::catalog::{
    fetch_signed_adapter_update_catalog, fetch_signed_core_update_catalog, AdapterCatalogFetch,
    AdapterCatalogSources, CoreCatalogFetch, CoreCatalogSources, CoreUpdateCatalog,
    VerifiedCoreUpdateCatalog,
};
use crate::update::installation::{
    resolve_current_core_installation, CoreInstallationOwnership, LegacyAdoptionAuthorization,
    LegacyAdoptionConsent,
};
use crate::update::network::UpdateNetworkClient;
use crate::update::plan::{
    build_update_plan, AdapterInstallationKind, AdapterInstallationSnapshot, AdapterOrigin,
    CoreInstallationSnapshot, CorePlanOwnership, UpdateAction, UpdateComponentKind,
    UpdateInventory, UpdatePlan, UpdatePlanRequest, UpdateResult, UpdateResultStatus,
    VerifiedCatalogSnapshots,
};
use crate::update::state::{
    CachedCheckResult, UpdateCache, UpdateLock, UpdateMode, UpdateStateStore, SCHEMA_VERSION,
};

const CHECK_LOCK_LEASE: Duration = Duration::from_secs(30);

pub fn handle_update(args: UpdateArgs) -> anyhow::Result<()> {
    if !args.check {
        bail!(
            "update.apply-unavailable: update application is not enabled until verified staging and rollback are available; run `ldgr update --check`"
        );
    }
    handle_check(args)
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
    let resolved = resolve_check(args, home);
    let (plan, core_etag) = match resolved {
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
    state.write_cache(&success_cache(previous_cache, &plan, core_etag)?)?;
    lock.release()?;
    Ok(plan)
}

fn resolve_check(args: &UpdateArgs, home: &Path) -> anyhow::Result<(UpdatePlan, Option<String>)> {
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
    let inventory = inspect_inventory(home, args.yes)
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
            AdapterCatalogFetch::Modified { verified, .. } => verified.catalog,
            AdapterCatalogFetch::NotModified { .. } => {
                bail!("update.catalog-unavailable: adapter catalog returned not-modified without a cached snapshot")
            }
        }
    } else {
        AdapterReleaseIndex {
            schema_version: 1,
            adapters: Vec::new(),
        }
    };
    let platform = platform_tag()?;
    let updater_version = Version::parse(env!("CARGO_PKG_VERSION"))?;
    let plan = build_update_plan(
        &request,
        &VerifiedCatalogSnapshots {
            core: &core,
            adapters: &adapter_catalog,
        },
        &inventory,
        &updater_version,
        &platform,
    )
    .context("update.no-compatible-release: failed to resolve a compatible update plan")?;
    Ok((plan, core_etag))
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

fn inspect_inventory(home: &Path, yes: bool) -> anyhow::Result<UpdateInventory> {
    let ownership =
        resolve_current_core_installation(home, LegacyAdoptionConsent::NonInteractive { yes })?;
    let core = core_snapshot(ownership);
    let registry = AdapterRegistry::discover();
    let mut discovery_warnings = registry
        .warnings
        .iter()
        .map(|warning| format!("adapter discovery warning: {}", warning.message))
        .collect::<Vec<_>>();
    let roots = AdapterRoots::discover(home)?;
    let mut adapters = Vec::new();
    for installed in &registry.adapters {
        let origin = roots.origin(&installed.root_path);
        let installation = if origin == AdapterOrigin::User {
            match crate::update::adapter::snapshot_adapter_installation(installed, home) {
                Ok(snapshot) => snapshot,
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
    Ok(UpdateInventory {
        core,
        adapters,
        discovery_warnings,
    })
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

fn now_ms() -> anyhow::Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before Unix epoch")?
        .as_millis()
        .try_into()
        .context("update check timestamp overflow")
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
        "mode=check status={} current_core={} target_core={} platform={} channel={}",
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
