# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

## [0.1.18] - 2026-09-01

### Added

- Add signed local release-store support for offline Core and adapter update and installation workflows.
- Add consent-gated experience donation telemetry with bounded automation and local fixture support.
- Register every published adapter telemetry protocol for preview, explicit transmission, and automatic queue delivery.

### Fixed

- Initialize a bounded default Pi harness configuration when installing an adapter into a clean home.
- Preserve valid existing harness configuration and clean ephemeral staging after successful rollback.

## [0.1.17] - 2026-08-27

### Fixed

- Keep staged update timestamps monotonic when the system wall clock moves backward during an update.
- Retain update ownership for compatibility-v2 adapters that write schema-2 installation receipts.

## [0.1.16] - 2026-08-27

### Fixed

- Let plain interactive `ldgr update` safely adopt verified pre-receipt Core installations through the existing plan confirmation.
- Migrate canonical pre-receipt user adapters to verified signed releases without manual directory removal or separate reinstall commands.
- Preserve strict ownership checks for malformed receipts, modified receipt-owned files, package-manager installs, project adapters, and environment overrides.

## [0.1.15] - 2026-08-26

### Added

- Add signed Core and adapter update catalogs with compatibility-bound planning, detached signature verification, and five-platform release resolution.
- Add atomic Core, agentctl, and adapter staging with durable rollback journals, Windows finalization, startup checks, and explicit check-only mode.
- Add compatibility-v2 adapter capabilities, central-component requirements, local-store isolation, canonical Research installation, and actionable repair diagnostics.
- Add the reviewed one-time signed Core catalog bootstrap and append-only release publication workflow.

### Changed

- Narrow agent error handling to substantive durable, blocking, ambiguous, or integrity-relevant failures; transient command/tool mistakes and safely corrected validation failures now stay out of first-class error and disposition workflows.
- Add `ldgr error <command> <type> <message>` as the agent-first recording path, with generated durable metadata and automatic active run/work links.
- Resume the current active run when `ldgr loop run` starts instead of refusing the invocation or claiming a second work item, and document the full loop execution model and operational controls in focused help.

### Fixed

- Refresh the standalone lockfile for release tooling and normalize it for reproducible cross-platform builds.
- Canonicalize transactional temporary roots on macOS and compare Windows receipt paths by filesystem identity.
- Verify both current and reviewed historical paired-Core metadata through the official installer helper.
- Stop Core and adapter resource installs from writing duplicate `~/.agents/skills` copies; selected harness-specific skill roots are now authoritative, including for legacy configs.
- Resolve and probe Windows source-adapter Cargo/Rustup configurations across split harness and user-profile homes, preserve working explicit homes, reject relative overrides, and record redacted first-class infrastructure errors before spawn when fallbacks are unavailable or ambiguous.

## [0.1.13] - 2026-07-30

### Added

- Add one-shot `ldgr rerun` recovery for complete, non-destructive parser corrections, backed by a project-local restricted receipt.
- Add versioned structured error fingerprints, explicit audited override/split controls, collision rejection, recurrence flags, and bounded redacted recurrence context containing prior occurrences and related causal evidence.
- Reconcile project and per-user recovery spools on init/status/context/loop startup, quarantine corrupt envelopes intact, import emergency occurrences transactionally, and restore dead-worker runs behind explicit disposition gates.
- Add `ldgr compatibility --agentctl-version <version> --json` and pair Core 0.1.13 with agentctl 0.1.2 through a versioned launcher/recovery negotiation contract.

### Changed

- Accept arbitrary non-empty priority labels, with stable ordering for common named priorities and continued canonical `P<number>` support.
- Canonicalize harmless command, option, enum, schedule, label, and dependency syntax variants for non-interactive agent use.
- Preserve complete parser intent in corrected command output while keeping fuzzy and destructive corrections from executing silently.

## [0.1.12] - 2026-07-16

### Added

- Expose direct dependencies, dependents, dependency satisfaction, effective readiness, and blocker reasons from work-item list and detail views.
- Add individual dependency edge editing with `ldgr work dependency add` and `ldgr work dependency remove`.
- Add human-readable, JSON, filtered, and Mermaid dependency graph inspection with `ldgr work graph`.
- Add `ldgr work audit` findings for graph structure, canceled dependencies, priority inversions, terminal reachability, and missing validation records.
- Add transactional import dry-runs and an exported example schedule document.
- Add the released numerical-sequence telemetry transmit path: `ldgr telemetry status` and `preview` cover `core-work/v1` and `research-workflow/v1`, while `ldgr telemetry transmit` sends one raw array per HTTPS request with root-CA, random-delay, and timeout controls.
- Add `ldgr loop run --detach` for background queues with durable logs, including Windows child-process `HOME` compatibility through `USERPROFILE`.
- Publish agentctl beside `ldgr` in every Core release archive and make Windows updates replace the user-owned binary directory already resolved on `PATH`, preserving `*.previous` rollback copies.

### Changed

- Include the enriched nonterminal queue and scoped pending-decision identity in full machine-readable status output.
- Explain dependency-list syntax and the distinction between `run finish` and `run close` in CLI help.
- Print the required work-decision command after `run finish` leaves a completed run awaiting its decision.
- Document the telemetry deletion boundary plainly: disabling removes unsent local payloads and prevents future collection, but already-ingested sequences cannot be individually located for deletion because no user, installation, request, timestamp, or join identifier is stored.
- Validate completion-audit requirements before starting or detaching a loop run.

## [0.1.11] - 2026-07-15

### Fixed

- Automatically migrate recognized schema-v1 ledgers when opening `ldgr status`, `ldgr context`, or `ldgr init`, with a verified backup reported before mutation.
- Preserve adapter-owned tables and data while validating and upgrading the Core-owned schema, including older v1 ledgers that predate optional Core prompt tables.

## [0.1.8] - 2026-07-14

### Added

- Add explicit one-time, opt-in telemetry consent during installation, visible collection status, local buffering, and privacy-preserving numeric state-transition sequences with an interpretable success or unsuccessful terminal outcome.

### Fixed

- Build Linux ARM64 and macOS Intel archives on current GitHub-hosted runners.
- Publish binary releases only after every supported platform build and checksum succeeds, preventing incomplete releases from appearing installable.
- Refresh the standalone package lockfile so release checkouts build reproducibly with `--locked` after adding telemetry and installer dependencies.

## [0.1.6] - 2026-07-13

### Changed

- Report work with no structured dependency edges as `dependencies: none declared` instead of implying that prose dependencies were satisfied.
- Keep `ldgr status --full` focused on global history without repeating adapter, handoff, and next-command sections, and report idle loop state as idle rather than running.

### Fixed

- Fall back to the built-in release/Git installer for ordinary online adapter installs when the default release index is unavailable, while keeping explicit, offline, version-pinned, and prerelease index requests fail-closed.
- Allow Code, Security, Explore, and Bench adapters to recover from a local workspace with `ldgr adapter install <adapter> --source-root <workspace>`.

## [0.1.5] - 2026-07-13

### Added

- Add structured work-item priority, program, group, acceptance criteria, hold classification, and dependency fields.
- Enforce an acyclic dependency graph and prevent manual or autonomous runs from claiming work with unfinished prerequisites.
- Add transactional JSON schedule import and portable schedule export for bulk queue creation and backup.
- Add actionable status filters, priority/program queue summaries, held-reason grouping, readiness, blockers, and downstream-unblock context.

### Changed

- Migrate released schema-v1 ledgers transactionally to schema v2 while preserving existing ledger data.
- Scope default status observations, validations, and decisions to the running or next item; move global history and stale terminal loop detail behind `ldgr status --full`.

### Fixed

- Reject and roll back adapter releases whose manifest requires an executable that is absent from the archive, preventing a successful-looking `code` install with no `ldgr-code` command.

## [0.1.4] - 2026-07-06

### Changed

- Install adapter bundles under the single global `~/.ldgr/adapters/<adapter>` root and remove direct `~/.ldgr/<adapter>` discovery fallbacks.
- Route adapter-owned prompts, skills, commands, and extensions through configured harness paths in `~/.ldgr/config.json`, preserving Pi setup while supporting Codex prompt/skill paths.
- Update adapter install docs and smoke coverage for harness-aware resource installation.

### Added

- Add `ldgr loop run --until-empty` to keep launching fresh single-agent loop cycles until no pending work remains or the loop blocks.
- Add optional one-shot post-cycle summaries via `--summary-agent agentctl` / `--summary-argv`, appended to `.ldgr/logs/loop-summary.md` without making the worker agent write narrative reports.
- Install the core loop prompt and include `core` alongside installed adapter loops such as conduct/research in the Pi `/run-loop` selector.
- Add routine-cycle guidance to prefer compact machine-summarizable run summaries and reserve long narrative reports for promotion points.
- Add `scripts/install.sh`, an OS/architecture-aware release installer for clean `curl | sh` installation of `ldgr`.
- Add `ldgr observe` as an observation shorthand, including `ldgr observe <run-id-or-work-slug> --body ...`.
- Allow run references in run/evidence commands to use either numeric run IDs or work-item slugs.

### Fixed

- Keep focused subcommand help concise by limiting adapter discovery blocks to top-level and adapter-focused help.
- Report an actionable `ldgr init` hint when the ledger parent directory is missing instead of surfacing only a low-level SQLite open error.
- Append the latest matching agentctl raw log to failed `ldgr loop run --agent agentctl` output artifacts so child-agent auth/config errors are visible in LDGR evidence.
- Make source-root adapter installs patch adapter command argv to a cargo source runner so `ldgr <adapter>` works immediately without requiring the adapter binary on `PATH`.
- Use the current `agentctl run <agent>` CLI and merge `ldgr-loop` entries into `~/.agentctl/config.toml` so `ldgr loop run --agent agentctl` works after install without dropping existing agentctl agents.
- Use Cargo's positional crate argument for git adapter installs so release fallback can install open adapters such as `ldgr-research`.
- Suggest likely adapter names for `ldgr adapter install <adapter>` typos without silently executing fuzzy matches.
- Install adapter skills only into Pi's configured global skill directory instead of also writing duplicate global `~/.agents/skills` copies.

## [0.1.0] - 2026-06-11

Initial open-source release.

### Added

- Durable SQLite ledger of work items, runs, observations, artifacts,
  decisions, global notices, prompt records, prompt bundles, validation records,
  event logs, and loop interventions.
- Core bounded loop runtime (`ldgr loop run`) with the built-in `codex` preset,
  custom `--agent-argv` processes, dry runs, streamed output, prompt provenance,
  and adjustable agent timeouts.
- Web cockpit (`ldgr web`) with live dashboard, context/artifact viewer, loop
  controls, conduct wave visibility, and token-gated mutating routes.
- Core CLI workflow for init, work, runs, observations, artifacts, decisions,
  validation records, notices, context, status, prompts, bundles, loop control,
  and audit/status rendering.
- Bundled SQLite schema version 1 for the production core ledger shape.

### Changed

- Research/readiness surfaces such as facts, expectations, failures, blockers,
  milestones, tools, skills, chat, profiles, coverage, and evidence live outside
  this crate in the research/adapter layer.
- OpenAI-compatible REST agent integration is no longer part of `ldgr-core`; use
  `--agent-argv` to run agentctl or another external agent process.

## [0.1.1] - 2026-06-30

### Changed

- Make `agentctl` the canonical LDGR loop agent control plane via the global `~/.ldgr/agentctl/harness.toml` configuration generated by `ldgr install`.
- Configure selected harnesses (Pi, Codex, Claude Code, OpenClaw/OpenCode) as global agentctl tasks during `ldgr install`.
- Allow `ldgr loop run --agent agentctl` to run without a default wall-clock timeout; operators may still set `--agent-timeout-seconds` explicitly.

### Fixed

- Avoid requiring per-project `.graph-worker/harness.toml` files for the built-in `--agent agentctl` loop runner.
