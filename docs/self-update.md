# LDGR self-update and adapter-update specification

Status: implemented operational contract
Target: LDGR Core after 0.1.14 (compatibility v2)
Scope: update discovery, explicit update application, startup notifications, the paired `agentctl` binary, and installed adapters

## Summary

LDGR exposes one top-level update surface:

```text
ldgr update
ldgr update --check
ldgr update --check --json
ldgr update --core-only
ldgr update --adapters-only
ldgr update --adapter research --adapter conduct
ldgr update --prerelease
```

`ldgr update` updates the compatibility-bound Core/`agentctl` release bundle and every eligible receipt-managed adapter. It constructs and verifies the complete plan before changing anything. A normal LDGR command may trigger a throttled, non-blocking check at startup, but startup never installs software, prompts, changes a project database, or fails the requested command.

Adapter compatibility v2 is evaluated from protocol epoch, minimum Core schema,
required Core capabilities, and optional registered central components. Exact
global database-contract/release-set identity and Core package patch ranges are
not ordinary v2 discovery or update gates. Local-store metadata is diagnostic
and stays under adapter-owned migration and recovery.

This is an orchestration feature, not a replacement for the existing adapter updater. `ldgr adapter update <name>` already handles signed releases and tracked local-source adapters. The implementation should extract that logic into a reusable library and have both command surfaces call it.

## Current system

The implemented Core behavior is:

- `src/cli/mod.rs` owns parsing, startup recovery, built-in command precedence,
  compatibility-aware dispatch, and the top-level `update` command/startup hook.
- `ldgr update` and `ldgr adapter update <name>` share signed catalog,
  compatibility-v2 resolution, staging, ownership, and transaction primitives.
- Signed releases resolve against the active or candidate Core compatibility
  profile. Local-source updates verify ownership and source identity before
  rerunning their recorded installer.
- `src/release_index.rs` validates schema-v2 adapter indexes, deterministic
  compatibility variants, SHA-256, Ed25519 signatures, and safe extraction. The
  bounded schema-v1 reader retains exact historical checks only for legacy
  artifacts.
- Whole-plan journals snapshot Core/agentctl, adapter bundles, resources, and
  receipts. Central migrations use verified database backups. Adapter-local
  stores are not opened by Core update preflight.
- Discovery searches `LDGR_ADAPTER_PATH`, project `.ldgr/adapters`,
  `LDGR_HOME/adapters`, and `~/.ldgr/adapters`. Only receipt-managed user
  installations are bulk-update targets; project and environment overrides are
  reported but not mutated.
- Official release archives contain `ldgr`, paired `agentctl`, and
  `RELEASE-METADATA.json`; signed catalogs bind platform, versions,
  compatibility profiles, archive roots, URLs, digests, and signing keys.
- Unix activation validates absolute installed binaries before commit. Windows
  uses a durable detached finalizer and reports `staged_pending_restart` until a
  terminal receipt is available.
- `~/.ldgr/config.toml` is canonical and `config.json` remains a compatibility
  mirror.

## Goals

1. Make the command the user naturally tried, `ldgr update`, valid and useful.
2. Check for updates during normal startup without adding noticeable latency or making command success depend on the network.
3. Update Core and the paired `agentctl` as one compatibility unit.
4. Include eligible installed adapters in the same plan, resolving them against the target Core version rather than the currently running version.
5. Preserve the existing adapter ownership, signature, compatibility, rollback, and local-source rules.
6. Support Windows, Linux x86-64/aarch64, and macOS x86-64/aarch64.
7. Keep machine-readable stdout stable and keep update checks independent of telemetry consent and project data.
8. Fail closed before mutation when authenticity, compatibility, ownership, or the complete plan cannot be established.

## Non-goals

- Installing updates automatically at startup.
- Updating adapters found only through project `.ldgr/adapters` or `LDGR_ADAPTER_PATH` development overrides.
- Acting as a general package-manager updater for Homebrew, Cargo, WinGet, or system-managed installations.
- Updating an untracked adapter that has no valid installation receipt.
- Downgrading components through normal resolution. Explicit rollback or version pinning can be designed separately.
- Recording user-level update checks in whichever project `.ldgr/ldgr.db` happens to be current.

## Command contract

### `ldgr update`

Without flags, the command:

1. acquires the global update lock;
2. fetches and verifies current release catalogs;
3. discovers the installed Core/`agentctl` bundle and eligible adapters;
4. resolves a single compatible target plan;
5. prints the plan and requests confirmation when stdin is interactive;
6. requires `--yes` when a confirmation would otherwise be needed in a non-interactive session;
7. downloads, verifies, and stages every selected artifact before mutation;
8. applies the plan, rolls back on failure, validates the resulting installation, and writes a durable receipt.

The default selection is the Core/`agentctl` pair plus all receipt-managed adapters under the user LDGR home. Components already current are reported as no-ops.

### Options

| Option | Meaning |
| --- | --- |
| `--check` | Resolve and report only; never download release archives, execute installers, or mutate installation state. |
| `--json` | Emit exactly one versioned JSON document on stdout. Diagnostics remain on stderr. |
| `--yes` | Accept the printed plan and any safe legacy-install adoption without prompting. |
| `--core-only` | Select only the Core/`agentctl` pair. Conflicts with `--adapters-only` and `--adapter`. |
| `--adapters-only` | Select all eligible adapters and leave Core/`agentctl` unchanged. |
| `--adapter <slug>` | Select one adapter; repeatable. Implies adapters-only unless Core is selected by a future explicit flag. |
| `--prerelease` | Permit prerelease Core and adapter targets. Stable remains the default channel. |
| `--offline` | Use only configured local catalogs/artifacts and cached artifacts. Any required network access is an error. |

`ldgr adapter update <name>` remains supported. It should use the same planner and applier for one adapter, keeping its current output compatible where practical.

### Result and exit behavior

- A successful check exits 0 whether or not updates are available. Automation reads `update_available` from JSON.
- A successful apply exits 0 only after the synchronous platform operation is complete, or, on Windows, after a verified plan has been durably staged and the finalizer has been successfully launched. The output must explicitly distinguish `applied` from `staged_pending_restart`.
- Catalog, verification, compatibility, ownership, staging, application, or validation failures exit nonzero.
- A partial multi-component apply is a failure. The updater attempts rollback and reports the terminal state of every component.
- A second updater exits nonzero with a stable `update.locked` error unless it is only reading a completed cached check.

### JSON shape

The check and apply commands return a common envelope:

```json
{
  "schema_version": 1,
  "mode": "check",
  "status": "updates_available",
  "current_core": "0.1.14",
  "target_core": "0.1.15",
  "platform": "windows-x86_64",
  "channel": "stable",
  "components": [
    {
      "kind": "core_bundle",
      "name": "ldgr-core",
      "current": "0.1.14",
      "target": "0.1.15",
      "action": "update",
      "compatibility": "compatible"
    },
    {
      "kind": "adapter",
      "name": "research",
      "current": "0.1.4",
      "target": "0.1.5",
      "action": "update",
      "compatibility": "compatible"
    }
  ],
  "warnings": []
}
```

Allowed top-level statuses are `current`, `updates_available`, `applied`, `staged_pending_restart`, `blocked`, and `failed`. Each component must have an explicit terminal or planned action: `none`, `update`, `reinstall_local_source`, `skip_unmanaged`, `blocked`, `applied`, `rolled_back`, or `failed`.

## Startup check

### Required behavior

Every normal CLI startup calls `maybe_schedule_update_check` after process-home repair and argument normalization but before command dispatch. The hook must be skipped for:

- the internal update worker/finalizer;
- `ldgr update` itself;
- an explicit opt-out environment variable;
- configuration set to `never`;
- an active, non-stale update lock; and
- invocations whose only purpose is shell completion, help, or version display.

The hook first reads `~/.ldgr/update-state.json`. Reading the cache is local and bounded. If a successful check is younger than the configured interval, no network worker is started. If the check is due, LDGR atomically claims a short-lived lock and spawns a detached copy of the current executable in an internal check-worker mode with stdin/stdout/stderr detached.

The foreground command never waits for this worker. Worker failure never changes the foreground exit code. A default command therefore pays only for reading a small file and, at most once per interval, spawning a child.

The worker performs the equivalent of `ldgr update --check --json`, writes state through a temporary file plus atomic rename, and releases the lock. It does not open or modify the current project database.

### Notification behavior

The next interactive foreground invocation reads the completed cached result and may print one concise notice to stderr:

```text
update available: ldgr 0.1.15 and 2 adapters; run `ldgr update`
```

Notifications must:

- never appear on stdout;
- be suppressed for non-interactive sessions by default;
- be emitted at most once per target plan digest per 24 hours;
- name check failures only after repeated failures, as a warning, never as a command failure; and
- avoid printing on adapter subprocess stdout/stderr through the internal recursion guard.

“Trigger a check when started” means schedule a due check, not perform an install and not synchronously contact the network on every command.

### Defaults and configuration

Add a typed, defaulted update section to `HarnessConfig` without changing the current schema version:

```toml
[updates]
check = "startup"       # startup | never
interval_hours = 24
channel = "stable"      # stable | prerelease
include_adapters = true
notify = true
```

Expose these through `ldgr config show` and `ldgr config set`:

```text
ldgr config set updates.check never
ldgr config set updates.interval-hours 12
ldgr config set updates.channel prerelease
ldgr config set updates.include-adapters false
ldgr config set updates.notify false
```

Support `LDGR_NO_UPDATE_CHECK=1` as an immediate process-level opt-out. `CI=true` suppresses notices but does not alter an explicit `ldgr update --check`. An explicit update command always overrides the startup interval.

## Release discovery and trust

### Signed update catalog

Publish a dedicated, versioned Core update catalog rather than parsing GitHub’s latest-release API at runtime:

```text
https://raw.githubusercontent.com/hydra-dynamix/ldgr-releases/main/core-index.json
https://raw.githubusercontent.com/hydra-dynamix/ldgr-releases/main/core-index.json.sig
```

The catalog contains Core version, channel, platform, archive URL/root, SHA-256, detached archive signature URL, signing key id, minimum updater version, paired `agentctl` version, launcher compatibility schema, and release metadata schema. The signature covers the canonical catalog bytes so an attacker cannot relabel an older signed archive as a newer version or alter compatibility metadata.

Core update trust roots must be embedded in the binary. Key rotation uses a catalog signed by an already trusted key and may add a successor key; a new key cannot authorize itself. `LDGR_CORE_UPDATE_INDEX` and `LDGR_CORE_RELEASE_KEYRING` may override sources for testing and enterprise mirrors, but non-HTTPS remote URLs are rejected and local files require `file://` or `--offline`.

The existing adapter index should receive the same catalog-signature protection. Archive Ed25519 verification remains mandatory. Until a signed adapter catalog is deployed, explicit adapter update behavior may remain available, but unattended startup results must label adapter availability `untrusted_catalog` and must not stage an update from it.

### Network client

Use the existing Rustls-backed blocking `reqwest` dependency rather than spawning `curl` for the new updater. Centralize fetching so Core and adapters share:

- HTTPS-only redirects;
- connect and total timeouts;
- response-size limits;
- ETag/If-None-Match support;
- a fixed user agent containing only the LDGR version and platform;
- no cookies, credentials, project paths, repository data, or telemetry identifiers; and
- deterministic local-file behavior for tests.

Recommended worker limits are a 2-second connect timeout, a 5-second total catalog timeout, and bounded catalog/signature sizes. Explicit artifact downloads may use a longer configurable timeout and must stream to disk rather than buffering archives in memory.

## Resolution and compatibility

Resolution is one immutable plan built from one verified snapshot of each catalog.

1. Determine the running Core version from `CARGO_PKG_VERSION` and the platform from the existing platform-tag logic.
2. Resolve the newest allowed Core release strictly newer than the running version. Normal update never downgrades.
3. Treat the Core archive’s `ldgr` and `agentctl` as one component. Verify the catalog metadata against `RELEASE-METADATA.json` after extraction.
4. Discover adapters, but select only installations with valid receipts whose install roots are within the configured user adapter root and whose owned resources remain within recorded/configured harness boundaries.
5. Resolve signed-release adapters against the **candidate Core profile** (or
   the active profile when Core is not selected): adapter protocol epoch,
   projected Core schema, required Core capabilities, and projected registered
   central components. Do not compare the global release-set hash or a maximum
   Core package patch for v2 artifacts.
6. For each local-source receipt, validate its ownership boundary and source identity. A startup check may report source drift. An explicit bulk update may rerun the recorded source installer only when drift exists; unchanged local sources are no-ops.
7. Skip untracked, project-local, and environment-override adapters with an explicit warning. Never guess ownership from a manifest path.
8. If any installed adapter cannot be retained or replaced by a release
   compatible with the target Core, block the whole update before downloads.
   `--core-only` leaves adapter bytes unchanged but does not bypass this proof;
   every retained adapter must evaluate successfully against the candidate.
9. Registry discovery warnings are part of the plan and must not be silently discarded.

The planner returns deterministic component ordering: Core bundle first in reports, then adapters sorted by canonical slug. The applier may use a different safe activation order but records both.

## Staging, application, and rollback

### Common staging

Before changing installed files:

- download every required archive and signature to a unique directory under `~/.ldgr/updates/staging/<plan-id>`;
- verify catalog signature, archive digest, archive signature, safe paths, archive root, platform, and embedded release metadata;
- verify available disk space where supported;
- calculate every destination and prove it lies inside the recorded ownership boundary;
- snapshot all files that may change; and
- write and fsync `plan.json` and `state.json` before activation.

The plan id is a SHA-256 digest of the canonical resolved plan, not a random label. This makes concurrent retries idempotent.

### Core/`agentctl` ownership

The official installers must begin writing `~/.ldgr/core-installation-receipt.json` with:

- schema version and installer kind;
- Core and `agentctl` versions;
- archive URL, SHA-256, signing key, platform, and release commit;
- canonical install root and exact binary paths;
- binary digests;
- compatibility schema; and
- previous successful plan id and timestamp.

Package-manager installations must declare `managed_by` and are check-only. The updater prints the package-manager-specific update command when known.

For pre-receipt official installations, v1 may offer one-time adoption only when both binaries are siblings, the directory is user-owned and writable, the platform is supported, the live compatibility handshake succeeds, and the path is not a recognized package-manager root such as Cargo’s bin directory. Interactive adoption requires confirmation; non-interactive adoption requires `--yes`. Otherwise application stops and directs the user to rerun the official installer.

### Unix activation

On Linux and macOS, stage replacements in the destination filesystem, preserve executable permissions, rename the old pair to receipt-owned backups, atomically rename the new pair into place, and run:

```text
<installed-ldgr> --version
<installed-agentctl> --version
<installed-ldgr> compatibility --agentctl-version <version> --json
```

Then activate staged adapter plans and validate adapter discovery. Any failure restores all snapshots in reverse order.

### Windows activation

Windows cannot overwrite the running `ldgr.exe`. The foreground process therefore stages and verifies the whole plan, then launches the staged new Core binary in a hidden internal finalizer mode with the parent PID and durable plan path. The foreground prints `staged_pending_restart` and exits.

The finalizer:

1. waits for the parent process to exit with a bounded timeout;
2. reacquires and validates the plan lock and digest;
3. snapshots destination files;
4. replaces `agentctl.exe` first and `ldgr.exe` second;
5. activates staged adapters and resources;
6. runs the paired version and compatibility smoke tests using absolute paths;
7. rolls back every changed target if validation fails;
8. writes a terminal result receipt atomically; and
9. removes staging only after a terminal receipt exists.

The next LDGR startup reads any pending receipt before scheduling another check and reports `applied`, `rolled_back`, or `failed`. Stale `applying` state is recoverable from the snapshots; it is never treated as success.

Internal worker/finalizer arguments must be hidden from normal help, protected by an unguessable per-plan token stored with owner-only permissions, and accepted only when the executable and plan paths satisfy the recorded boundaries.

### Adapters

Refactor the existing adapter updater into three reusable phases:

```text
inspect_adapter_installation
plan_adapter_update(target_core_version, channel)
stage_and_apply_adapter_update(plan, transaction)
```

The single-adapter and top-level commands call those functions. Preserve all current invariants:

- signed-release archives require SHA-256, Ed25519 verification, safe extraction, manifest validation, and typed-resource boundaries;
- local-source installs never claim verified-release provenance;
- modified owned files block replacement;
- unowned destination collisions block replacement;
- release receipts and source receipts remain distinct; and
- harness resources are reconciled from the newly installed bundle.

Extend `InstallTransaction` into a plan-wide transaction or journal. Per-adapter commits cannot provide whole-plan rollback after a later adapter or Core activation fails.

## State and concurrency

Store user-level updater state under `~/.ldgr/updates`:

```text
~/.ldgr/update-state.json
~/.ldgr/updates/update.lock
~/.ldgr/updates/staging/<plan-id>/plan.json
~/.ldgr/updates/staging/<plan-id>/state.json
~/.ldgr/updates/history/<plan-id>.json
```

All state files use versioned schemas, temporary-file writes, fsync where supported, and atomic rename. On Unix, files containing internal finalizer tokens are mode 0600. Windows uses user-only ACLs where available. Symlinks/reparse points in updater-owned state or destination boundaries are rejected.

The lock records PID, process start identity when available, creation time, mode, and plan id. A lock is stale only when the owner is definitively gone or its bounded lease expired. Startup checks skip a live lock; explicit update reports it.

Keep a bounded history of successful and failed attempts. State must not include project paths, command arguments unrelated to updating, or telemetry identifiers.

## Failure model

Use stable codes in human and JSON output:

| Code | Meaning | Retryability |
| --- | --- | --- |
| `update.catalog-unavailable` | Catalog could not be fetched before timeout. | transient |
| `update.catalog-untrusted` | Catalog signature/key validation failed. | after-change |
| `update.no-compatible-release` | No selected release satisfies platform/channel/Core constraints. | after-change |
| `update.unmanaged-installation` | Core or adapter ownership is not established. | after operator choice |
| `update.modified-owned-file` | A receipt-owned target drifted. | after-change |
| `update.locked` | Another update is active. | transient |
| `update.download-failed` | Artifact transfer failed. | transient |
| `update.artifact-untrusted` | Digest, signature, metadata, or archive validation failed. | after-change |
| `update.activation-failed` | A staged target could not be activated. | after-change |
| `update.validation-failed` | Post-activation version/compatibility/discovery checks failed. | after-change |
| `update.rollback-failed` | Automatic restoration did not reach a verified state. | manual recovery required |

Startup workers store these codes in global update state but do not emit project first-class errors. Explicit commands print full causal context and preserve a recoverable terminal receipt. Secrets and home paths are redacted in durable error summaries.

## Code integration map

Recommended modules and edits:

- `src/cli/args/update.rs`: public update flags and hidden worker/finalizer args.
- `src/cli/commands/update.rs`: rendering, confirmation, exit behavior, and calls into the library.
- `src/cli/mod.rs`: add `Command::Update`; invoke the startup scheduler with recursion and display-only guards.
- `src/update/mod.rs`: public planner/applier types.
- `src/update/catalog.rs`: signed Core catalog and signed adapter catalog parsing/validation.
- `src/update/client.rs`: bounded HTTPS/local-file fetches, ETags, and streaming downloads.
- `src/update/state.rs`: cache, lock, plan, history, and atomic persistence.
- `src/update/startup.rs`: due-check scheduling and cached notification rendering.
- `src/update/apply.rs`: plan-wide transaction plus Unix and Windows activation.
- `src/release_index.rs`: retain release parsing/crypto/extraction primitives, but expose them to the new planner and add signed-catalog verification.
- `src/cli/commands/ops.rs`: move adapter lifecycle internals out of the monolithic command module; keep compatibility wrappers.
- `src/harness_config.rs` and config command handling: typed update configuration and preserved TOML/JSON mirroring.
- `scripts/install.ps1` and `scripts/install.sh`: write Core installation receipts and use the same signed catalog semantics.
- `.github/workflows/release.yml`: sign Core archives/catalog, publish the catalog entry only after the complete platform matrix exists, and test update from the previous supported version.
- `docs/adapter-installation.md` and `README.md`: document top-level orchestration, startup notices, opt-out, and package-manager behavior.

No update code should depend on a project database being present.

## Test plan

### Unit tests

- Core catalog schema, canonical signature, key rotation, unknown fields, duplicate versions, invalid URLs, invalid platforms, and malformed semantic versions.
- Resolution across stable/prerelease channels, exact current/no-op, upgrade-only ordering, target-Core adapter compatibility, and no-compatible-adapter blocking.
- Cache TTL, clock skew, ETag 304, notification deduplication, lock ownership/staleness, and atomic state recovery.
- Install receipt ownership, legacy adoption, package-manager detection, symlink/reparse-point rejection, and path containment.
- Plan digest determinism and redaction.

### CLI integration tests

- `ldgr update`, `--check`, `--json`, selectors, conflicts, `--yes`, `--offline`, and prerelease behavior.
- Startup foreground command succeeds immediately while a fake slow/offline catalog worker times out independently.
- Startup notices use stderr only and never corrupt JSON stdout.
- Startup hook recursion is impossible for adapter dispatch, check workers, and finalizers.
- All receipt-managed adapters are included; project, environment override, untracked, and modified adapters are skipped or blocked as specified.
- The existing `ldgr adapter update <name>` fixtures pass through the refactored library without behavioral regression.

### Artifact and security tests

- Tampered archive, checksum, signature, signed-catalog metadata, embedded release metadata, wrong platform, traversal, links, oversized responses, HTTPS downgrade redirects, and rollback snapshots all fail closed.
- A malicious catalog cannot relabel an old signed archive as a newer version.
- All artifacts are fully verified before the first destination mutation.

### Platform end-to-end tests

- Update from the previous supported release on all five release platforms.
- Verify `ldgr`, `agentctl`, their compatibility handshake, adapter discovery, and harness resources after update.
- Inject failure after each activation step and prove complete rollback.
- On Windows, prove the foreground process exits, the hidden finalizer replaces the formerly locked binary pair, and the next invocation reports the terminal receipt.
- Prove interrupted Windows finalization is recovered or rolled back on the next invocation.

### Release gate

The release workflow may publish a signed catalog entry only after every platform archive, signature, checksum, embedded metadata check, previous-version update test, and rollback test passes. Catalog publication is the final release step so startup checks never observe an incomplete platform matrix.

## Compatibility-v2 and legacy rollout

The rollout keeps readers broader than writers:

1. signed Core and schema-v2 adapter catalogs publish generated compatibility
   metadata and installation receipts;
2. catalog writers reject newly added v1 releases, handwritten Core patch
   ranges, partial platform sets, and stale packaged sidecars;
3. Core continues reading already signed schema-v1 catalogs and
   `adapter-database-contract.json` during protocol epoch 1;
4. an exact legacy match is visible as `degraded`; a stale readable legacy
   install is visible as `blocked` with
   `compatibility.legacy_global_contract_mismatch` or
   `compatibility.legacy_core_schema_mismatch` and an exact repair command;
5. v2 candidates are preferred, and a valid legacy candidate is considered only
   when no compatible v2 candidate exists; and
6. the v1 reader can be removed only with protocol epoch 1, after every supported
   adapter has a published v2 repair artifact.

Never translate a v1 global hash into a v2 schema minimum or ignore a stale
hash. When both sidecars exist, v2 is authoritative; malformed v2 metadata does
not downgrade to v1. Check-only and apply also remain distinct: startup checks
may report a signed plan, but only an explicit update performs staged mutation
and platform-specific rollback.

## Acceptance criteria

The feature is complete when:

- `ldgr update` updates an official receipt-managed Core/`agentctl` pair and all eligible adapters on every supported platform;
- `ldgr update --check --json` deterministically reports the same resolved plan without mutation;
- a due normal startup schedules a background check, adds no network latency to the foreground command, and later prints a deduplicated stderr notice;
- offline/unavailable update infrastructure never changes the requested command’s exit status;
- adapters are resolved against the target Core version and incompatibility blocks the default plan before mutation;
- project/dev override and untracked adapters are never bulk-mutated;
- Core archives, catalogs, adapter archives, and release metadata are cryptographically verified and version-bound;
- Windows updates finalize after the running binary exits and leave a durable terminal receipt;
- injected failures restore the prior Core/`agentctl` pair, adapters, harness resources, and receipts; and
- all existing adapter update, installer, CLI help, JSON-output, and compatibility tests continue to pass.
