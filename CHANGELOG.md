# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/).

## [Unreleased]

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
