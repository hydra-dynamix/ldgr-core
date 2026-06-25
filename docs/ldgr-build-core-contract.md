# LDGR Build Core Contract

This document defines the advanced LDGR research/readiness contract that an
`ldgr-build` style adapter can rely on through `ldgr-research`. Core `ldgr` is
now the durable continuity ledger: work, runs, observations, artifacts,
decisions, notices, context, web, and loop execution. The research layer stays
adapter-neutral: it records evidence, validation, failures, blockers,
readiness, and profile defaults, while adapters define domain-specific prompts,
milestones, expectation names, artifact conventions, and validation tools.

## Boundary

Core `ldgr` owns these behaviors:

- Durable work items, runs, observations, artifacts, decisions, and notices.
- Core control surfaces: `work edit`, `work status set`, `artifact show`, and
  `notice edit` for correcting durable state without inventing new concepts.
- Machine-readable `status --json` and `context --json` output for core
  ledger and loop state only.

`ldgr-research` owns these advanced behaviors:

- Issues.
- First-class failure records with resolution state and links to lifecycle
  records.
- Typed blockers against explicit lifecycle targets.
- Facts, fact evidence, revalidation policies, expectations, validation
  results, and validation-linked fact state.
- Generic milestones with linked facts and expectations.
- Readiness audits that block completion when required evidence, validation,
  failure, blocker, issue, fact, or revalidation conditions are not satisfied.
- Machine-readable research context output, including `adapter_context`.
- Adapter profile records that declare prompts, default milestone templates,
  spec artifact paths, validation tools, and readiness policy text.

Adapters own these behaviors:

- Domain-specific milestone templates such as a runnable skeleton milestone.
- Domain expectation names such as `SKELETON-001` or `ARCH-002`.
- Domain validation commands and artifact formats.
- Domain prompt text and instructions for decomposing adapter work.
- Any domain-specific interpretation of spec artifacts.

Core must not embed adapter-specific product rules. Adapter-specific rules
belong in research/adapter profiles, prompts, artifacts, target profiles, or
tests that exercise the generic ledger through realistic adapter data.

## Failure And Blocker Model

Validation failures and discovered blockers must be durable and traceable.

`ldgr-research` provides:

- `failure create`, `failure show`, `failure resolve`, and `failure waive`.
- Failure links to work items, runs, artifacts, facts, expectations,
  validation results, milestones, blockers, and issues.
- `validation record --failure-slug` to attach an existing failure to a
  validation trace.
- `validation record --create-failure-slug ...` to create an open failure
  directly from a failed validation result.
- `blocker create` and `blocker resolve` with explicit typed targets.
- Readiness warnings for lower-severity open failures and blockers for
  critical open failures and open typed blockers.

Adapters should use failures for observed failed checks and blockers for known
required conditions that prevent readiness. A failed validation can create or
link a failure; a missing domain prerequisite can be represented as a typed
blocker against the relevant milestone, expectation, work item, run, or
artifact target.

## Readiness Contract

Readiness is not a declaration by the agent. It is computed from durable state.

A milestone or completion decision must be blocked when any required condition
is unresolved, including:

- A required fact is draft, unvalidated, stale, or overdue for revalidation.
- A required expectation is draft or has no validation result.
- A required expectation has a critical failed validation result.
- A linked failure is open and readiness-blocking.
- A typed blocker targets the milestone graph.
- An open issue is linked through failure or evidence state.
- Required artifact or evidence coverage is missing.

`ldgr-research readiness audit` renders the current blockers and warnings.
`ldgr-research readiness audit --queue-next` can queue focused follow-up work
for a readiness gap. `ldgr-research milestone transition --status achieved`
must reject blocked milestones with a concrete cause instead of accepting
unsupported readiness.

## Context JSON Contract

Adapters may consume `ldgr context --json` for stable core operational state:
work items, runs, observations, artifacts, decisions, notices, loop state, and
interventions.

Core `ldgr context --json` does not expose advanced research or adapter state.
It must not promise `adapter_context`, readiness, issues, failures, blockers,
expectations, validation results, target profiles, adapter profiles, or tools.

Advanced context belongs to the owning surface: `ldgr-research` for generic
research/readiness state, and adapter-specific commands or artifacts for
domain-shaped payloads. Core `ldgr` does not create or maintain advanced
research tables; adapters that need them must use the research layer's own
storage/schema management. Adapter-oriented consumers should read that
research/adapter context when they need:

- issue state
- failure and blocker state
- milestone readiness, blocked targets, and gaps
- expectation and validation-result summaries
- target profiles
- active and applied adapter profiles
- registered validation tools

Research/adapter context producers should define their own schema versioning
and migration policy. Core should preserve existing core field meanings across
additive schema changes, but it does not version adapter-owned payloads.

## Profile Contract

Adapter profiles are the minimal configuration bridge between the small LDGR
operational loop and domain-specific adapters. They are managed through
`ldgr-research`, not through hidden core `ldgr` commands.

The research layer records and exposes:

- `slug`
- `title`
- `loop_prompt_path`
- `default_milestone_template`
- `spec_artifact_path`
- `validation_tools`
- `readiness_policy`
- active or retired status
- applied timestamp

`ldgr-research` provides `profile create`, `profile list`, `profile show`, and
`profile apply`. Core `ldgr` does not create, read, or migrate profile tables.
Core loop execution stays explicit: pass `--prompt`, `--prompt-slug`, or
`--bundle` to `ldgr loop run`.

Adapters should create profiles in the research layer instead of requiring LDGR
core to know adapter names, storage tables, or hard-coded domain paths.

## Expected End-To-End Smoke

The queued `validate-ldgr-build-core-workflow` item should prove this contract
through one realistic adapter-shaped scenario:

1. Create or apply an adapter profile with prompt, milestone template, spec
   artifact path, validation tools, and readiness policy.
2. Model a runnable skeleton milestone with required expectations such as
   `SKELETON-001` and `ARCH-002`.
3. Record a failing validation result and create a traceable failure from it.
4. Add a targeted blocker against the relevant lifecycle target.
5. Attach artifact evidence for the validation and milestone state.
6. Verify readiness is blocked for the specific failure and blocker causes.
7. Resolve the failure and blocker, then record passing validation.
8. Verify readiness becomes pass and the research/adapter context exposes the
   adapter profile, milestone readiness, validation, failure, blocker, and
   artifact state needed by adapter consumers without adding those fields to
   core `ldgr context --json`.

The smoke may live in Rust CLI smoke coverage or an equivalent dev test, but it
must exercise the generic core contract rather than introducing adapter-specific
logic into core.
