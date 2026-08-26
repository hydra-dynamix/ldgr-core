# First-class error domain contract

Status: accepted design contract for the `core-first-class-errors` program.

This document defines the durable error model shared by LDGR Core, `agentctl`,
loop launchers, adapters, and recovery tooling. The terms **MUST**, **SHOULD**,
and **MAY** are normative.

## Goals and boundary

An error is causal state, not just terminal output. Once an operation has been
accepted, the system MUST leave one of these durable outcomes whenever at least
one declared durable sink is writable:

1. a terminal operation result; or
2. a recoverable error occurrence tied to the accepted operation.

The acceptance point is the last point before execution can create external or
project-visible effects. An execution boundary MUST durably record an intent
envelope before crossing that point. The envelope carries an `operation_id` and
an `attempt_id`; a terminal result or error occurrence closes the attempt.

This is an at-least-once durability contract. A crash may cause a record to be
submitted more than once, so all submissions MUST be idempotent. It is not a
claim that a record can survive when no durable medium is writable. If intent
cannot be written to any declared sink, execution MUST fail closed before
crossing the acceptance point and print an unmistakable nonzero diagnostic.

Ordinary CLI usage errors that fail before acceptance remain diagnostics. They
become first-class errors only when they concern an already accepted operation,
are explicitly recorded by an operator, or expose a failure in the execution
boundary itself.

The same boundary excludes routine working noise: shell quoting and path
mistakes, read-only discovery probes, expected negative-test failures, and
validation failures corrected safely within the active run. These may remain in
ephemeral tool output or in a validation record; they MUST NOT require a
first-class occurrence or disposition unless they reveal ambiguous durable
effects, a safety/integrity risk, or a blocker that survives a reasonable
correction attempt.

## Domain model

### Error aggregate

An error aggregate groups occurrences that have the same structured cause in
one project. It contains derived summary state:

| Field | Contract |
| --- | --- |
| `error_id` | Stable database identity. It is never reused. |
| `project_id` | Stable project identity used to prevent cross-project import. |
| `fingerprint_version` | Version of the canonicalization algorithm. |
| `fingerprint` | `sha256:` digest of the canonical structured identity. |
| `class` | One of the error classes defined below. |
| `domain` | Stable producer namespace, such as `core.store` or `agentctl.spawn`. |
| `code` | Stable machine code within the domain. |
| `severity` | `info`, `warning`, `error`, or `critical`. |
| `retryability` | `never`, `after-change`, `transient`, or `unknown`. |
| `state` | `open`, `acknowledged`, `resolved`, or `accepted`. |
| `first_seen_at` | Timestamp of the first imported occurrence. |
| `last_seen_at` | Timestamp of the latest imported occurrence. |
| `occurrence_count` | Count derived from immutable occurrences. |
| `latest_occurrence_id` | Latest occurrence under deterministic ordering. |
| `disposition_pending` | Derived; true when the latest occurrence has no valid disposition. |

The aggregate is scoped by `(project_id, fingerprint_version, fingerprint)`.
Aggregate summaries are projections and MAY be rebuilt from the immutable
ledger. Message text, stack traces, timestamps, process IDs, absolute home
paths, and secrets MUST NOT participate in identity.

### Immutable error occurrence

An occurrence is an append-only fact that a particular attempt observed an
error. It contains:

| Field | Contract |
| --- | --- |
| `occurrence_id` | Caller-generated UUIDv7, generated before the first write. |
| `error_id` | Aggregate selected during database import. |
| `idempotency_key` | Required caller key, unique within its producer scope. |
| `operation_id` | Stable UUID for the accepted operation. |
| `attempt_id` | Stable UUID for this execution attempt. |
| `class`, `domain`, `code` | Snapshot of classification at observation time. |
| `severity`, `retryability` | Snapshot of policy at observation time. |
| `source` | Producer and boundary, for example `agentctl:pre-spawn`. |
| `summary` | Short redacted operator-facing explanation. |
| `details` | Optional bounded, redacted structured JSON. |
| `environment` | Optional allow-listed environment facts, never a raw dump. |
| `observed_at` | Producer timestamp in RFC 3339 UTC. |
| `recorded_at` | Sink/import timestamp in RFC 3339 UTC. |
| `recovery_origin` | `database`, `project-inbox`, or `user-spool`. |

Once inserted, an occurrence MUST NOT be updated or deleted by lifecycle
commands. Corrections, reclassification, links, and dispositions are new
audited records. Retention may compact oversized diagnostic payloads only
under the retention rules below; it MUST preserve occurrence identity,
classification, timestamps, fingerprint inputs, causal links, payload digest,
and an audit record of the compaction.

`occurrence_id` is the end-to-end import identity. The database additionally
enforces uniqueness of `(producer, idempotency_key)`. Reusing either identity
with byte-equivalent canonical content returns the original occurrence.
Reusing it with different canonical content is an idempotency conflict and
MUST fail closed without changing either record.

### Classification

Each occurrence has exactly one class:

| Class | Meaning |
| --- | --- |
| `task-failure` | The requested task executed but did not produce its required result. |
| `validation-failure` | A completed validation proves a requirement is not met. |
| `infrastructure-error` | Runtime, configuration, storage, environment, network, harness, or process control prevented trustworthy execution. |
| `interruption` | Execution began but ended without a normal terminal result, including a signal, forced termination, power loss, or unexpected process disappearance. |
| `operator-cancellation` | An authorized operator deliberately canceled accepted work. |

Validation commands that cannot run because their infrastructure failed are
`infrastructure-error`, not `validation-failure`. A known failing assertion is
`validation-failure`. An ordinary nonzero task exit is `task-failure` unless a
more specific boundary proves infrastructure failure. Intentional cancellation
is never inferred solely from a missing process.

Producers MUST emit a stable `domain` and `code`. Unknown legacy failures use a
specific code such as `legacy.unclassified`, not message text as a surrogate
code. Classification changes are audited annotations; history is not rewritten.

### Fingerprint

Fingerprint algorithm `structured-v1` canonicalizes this map using JSON
Canonicalization Scheme ordering and hashes its UTF-8 bytes with SHA-256:

```json
{
  "class": "infrastructure-error",
  "domain": "agentctl.bootstrap",
  "code": "home-unavailable",
  "boundary": "config-discovery",
  "component": "agentctl",
  "subject": "ldgr-config"
}
```

`class`, `domain`, and `code` are required. `boundary`, `component`, and
`subject` are optional stable dimensions selected by the producer's published
fingerprint policy. Producers MUST NOT add volatile values merely to avoid
grouping. Absolute paths are normalized to stable categories or project-relative
paths; executable names are normalized by platform rules; operating system and
harness enter the fingerprint only when the producer declares them causal.

The stored fingerprint is:

```text
sha256:<lowercase hexadecimal digest>
```

Fingerprint policy changes require a new `fingerprint_version`. Existing
aggregates retain their original interpretation. An audited merge/split
override MAY relate aggregates after a collision or policy improvement, but
MUST NOT replace historical fingerprints. Raw-message similarity may assist
retrieval but MUST NOT create identity.

Core's `error record` command computes `structured-v1` when `--fingerprint` is
omitted. `--boundary`, `--component`, and `--subject` supply its optional stable
dimensions; summary and detail text never enter the digest. A producer-supplied
digest is an explicit override and requires
`--fingerprint-override-rationale`. A known collision or distinct causal branch
uses `--fingerprint-split` plus `--fingerprint-split-rationale`; Core stores the
derived identity under `structured-v1+split-v1`, leaving the base aggregate
unchanged. Reusing a digest with different recorded structured inputs fails
closed and directs the caller to the split control.

For agent and operator use, the preferred surface is `ldgr error <command>
<type> <msg>`. It accepts a stable command label and one of `task`, `validation`,
`infrastructure`, `interruption`, or `cancellation`; Core generates the UUIDv7
identities, structured fingerprint, timestamp, default policy, and active
run/work relations. The detailed `error record` surface remains available to
integrations that own explicit occurrence metadata.

### Causal relations

An occurrence or aggregate can have audited, typed relations to:

- accepted operations and attempts;
- Core runs and work items;
- artifacts and validations;
- causal decisions and error dispositions;
- external supervisor or adapter records.

Relations use `(relation_kind, entity_type, entity_id)` and include their
creation source and timestamp. Core-owned entity references MUST be validated
before insertion. External textual references MUST include a namespace.
Deleting a related entity MUST NOT erase error history; normal Core lifecycle
already restricts deletion where needed, and otherwise the relation is retained
as a tombstoned reference.

## Lifecycle and decisions

Aggregate lifecycle is:

```text
open -> acknowledged -> resolved
  |          |             |
  +----------+-----------> accepted
```

- `open` means the cause is active and not yet owned.
- `acknowledged` means an owner has taken responsibility; it is not resolution.
- `resolved` means evidence shows the cause was removed or the affected
  operation reached a trustworthy terminal result.
- `accepted` means an authorized decision explicitly accepts the remaining
  impact or risk.

A new occurrence reopens a `resolved` aggregate. A new occurrence on an
`accepted` aggregate keeps the historical acceptance but creates a new pending
disposition; acceptance is never silently inherited.

Every lifecycle transition is an append-only transition event containing the
old state, new state, occurrence or aggregate, actor/source, rationale,
timestamp, and optional evidence relations. Invalid transitions fail without
mutation.

Every occurrence requires a rationale-backed disposition:

- `retry`
- `workaround`
- `defer`
- `accept`
- `escalate`
- `cancel`
- `resolve`

Record the disposition explicitly:

```text
ldgr error disposition <error-id> --action defer \
  --actor operator --source cli --rationale "Waiting for the dependency"
```

`--decision-id` links the disposition to an existing causal decision. The
append-only disposition event also records the selected action, responsible
occurrence, actor/source, rationale, evidence relation IDs, retry basis, prior
disposition, and the resulting work transition. `ldgr error show` returns the
audited disposition history.

The disposition records the occurrence, aggregate, actor/source, rationale,
related causal decision when one exists, resulting work transition, evidence,
and timestamp. `accept`, `cancel`, and `resolve` are not synonyms: acceptance
waives remaining impact, cancellation stops the affected operation, and
resolution asserts that evidence closes the cause.

An unresolved blocking error or pending disposition MUST prevent the related
work from being reported as honestly ready or successfully complete. Read-only
inspection, recording evidence, importing recovery records, and recording a
disposition remain available. Nonblocking severity alone does not waive a
disposition; an explicit `accept` decision does.

## Recurrence and retry rules

An occurrence is repeated when its aggregate already contains an earlier
occurrence. On the second and later occurrences, Core MUST:

1. visibly flag recurrence and report the occurrence count;
2. retrieve a deterministic, bounded, redacted context packet;
3. include prior occurrences, the latest dispositions and causal decisions,
   related work/run state, relevant artifacts and validations, and meaningful
   allow-listed environment differences;
4. require a new disposition before another attempt.

An unchanged retry after recurrence requires either new evidence, a changed
execution condition, a different disposition, or an explicit confirmation
with rationale that references the prior decision. Silent identical retry
loops are prohibited. The context packet is advisory evidence and does not
mutate the aggregate.

For a repeated occurrence, `retry` requires `--retry-basis` and
`--prior-disposition-id`. New-evidence retries also require an existing
`--evidence-relation-id`; changed-condition retries require a surfaced
environment difference or evidence relation; and changed-decision retries
require a different `--decision-id`. Before another attempt, callers use
`ldgr error retry-check <error-id>` as the fail-closed authorization gate. It
returns the same bounded prior context on success and rejects missing,
non-retry, or incomplete decisions.

Ordering is deterministic: occurrence time, then record time, then
`occurrence_id`. Context bounds and truncation markers are part of the
machine-readable response so callers know what was omitted.

`ldgr error context <error-id>` exposes the same packet independently of
recording. Its per-section bound defaults to five and is capped at 100. The
packet anchors prior-occurrence ordering to the selected occurrence, derives
work, runs, decisions, artifacts, and validations from audited relations, and
reports only differences from an allow-list of meaningful environment keys.
Sensitive-key values, secret-shaped text, and user-home path segments are
redacted again at retrieval even when a producer failed to normalize them.

## Concurrency and consistency

Database recording uses a write transaction and the following logical order:

1. validate the envelope and project identity;
2. claim or verify the occurrence and producer idempotency keys;
3. find or create the fingerprint aggregate;
4. append the occurrence and causal relations;
5. update the aggregate projection;
6. append domain events;
7. commit.

Concurrent writers of the same occurrence converge on one row. Concurrent
writers of different occurrences with the same fingerprint append both and
produce an exact count. Busy/locked exhaustion is an infrastructure error and
uses the next durability sink; it is never treated as successful recording.

At-least-once import means a producer may crash after a successful write but
before observing the acknowledgement. Repeating the same envelope MUST be
safe. Importers process files under an atomic claim/rename protocol and only
archive a recovery file after the database transaction commits.

## Durable recovery sinks

Producers try sinks in this exact order:

1. the project database;
2. the project recovery inbox at `.ldgr/recovery/inbox`;
3. the per-user operating-system state spool;
4. fail closed.

The user spool is:

- Windows: `%LOCALAPPDATA%\ldgr\recovery\inbox`;
- Unix: `$XDG_STATE_HOME/ldgr/recovery/inbox`, or
  `$HOME/.local/state/ldgr/recovery/inbox` when `XDG_STATE_HOME` is unset.

If the required home/state root cannot be resolved, that sink is unavailable;
the implementation MUST NOT invent a relative path. `HOME` repair on Windows
uses `USERPROFILE` before configuration or spool resolution.

Each file is one versioned recovery envelope. Writers create a uniquely named
temporary file in the destination directory, flush file content, atomically
rename it to an `.json` inbox name, and flush the directory where supported.
Partial `.tmp` files are quarantined or ignored with a visible diagnostic.
Inbox names are not identities; `occurrence_id` and `idempotency_key` are.

Every project has a persisted random `project_id`. A user-spool envelope also
contains a normalized project locator and, when available, a digest of the
project database identity. Reconciliation imports only an exact `project_id`
match. If a project was moved, an explicit operator-approved rebind records the
old and new locators. Unknown or ambiguous envelopes are quarantined and never
guessed into the current project.

The minimal recovery envelope is:

```json
{
  "format": "ldgr-error-recovery",
  "schema_version": 1,
  "project": {
    "project_id": "0198...",
    "locator": "D:/apps/ldgr",
    "database_identity": "sha256:..."
  },
  "producer": "agentctl",
  "idempotency_key": "0198...:pre-spawn",
  "operation_id": "0198...",
  "attempt_id": "0198...",
  "occurrence_id": "0198...",
  "fingerprint": {
    "version": "structured-v1",
    "value": "sha256:...",
    "inputs": {
      "class": "infrastructure-error",
      "domain": "agentctl.bootstrap",
      "code": "home-unavailable",
      "boundary": "config-discovery",
      "component": "agentctl",
      "subject": "ldgr-config"
    }
  },
  "error": {
    "class": "infrastructure-error",
    "domain": "agentctl.bootstrap",
    "code": "home-unavailable",
    "severity": "error",
    "retryability": "after-change",
    "source": "agentctl:config-discovery",
    "summary": "Home directory was unavailable before configuration discovery.",
    "details": {},
    "environment": {
      "os": "windows",
      "arch": "x86_64"
    }
  },
  "observed_at": "2026-07-31T00:00:00Z"
}
```

Recovery schemas use `deny_unknown_fields` at trust boundaries and publish a
machine-readable JSON Schema. Readers accept only explicitly supported schema
versions. Unknown versions are quarantined with an actionable diagnostic and
left intact.

### Launcher/Core compatibility

Agentctl 0.1.2 negotiates `ldgr.launcher-compatibility.v1` with Core before an
LDGR-owned worker starts. Core 0.1.13 declares agentctl
`>=0.1.2, <0.2.0` and recovery schema 1. A missing negotiation command, rejected
version, invalid report, or unsupported recovery schema produces
`agentctl.compatibility/core-incompatible` with retryability `after-change`.
When the older Core cannot record it directly, agentctl writes the same
schema-v1 recovery envelope to the project/user spool for import after upgrade.

## Privacy, redaction, and retention

Error persistence is allow-list based. Producers MUST NOT record:

- environment dumps;
- command-line arguments known to contain credentials;
- access tokens, cookies, private keys, authorization headers, or connection
  strings;
- arbitrary file contents;
- unredacted user home paths when a stable placeholder is sufficient.

Known secret-shaped fields are rejected or replaced with a typed redaction
marker before any sink write. Redaction happens in the producer, not only at
display time. `details` and `environment` have size and nesting limits. Human
output applies the same policy as JSON output.

Default retention may prune or compact large diagnostic payloads after a
documented period, but occurrence facts, fingerprints, causal links,
dispositions, lifecycle transitions, payload digests, and migration/import
events are durable ledger data. Recovery inbox files are archived or removed
only after verified import; quarantined records require an explicit decision.

## Core database migration plan

The current released Core contract identifies schema version 2 while also
recognizing obsolete historical shapes numbered 3 and 4. The error migration
therefore MUST NOT reuse versions 3 or 4. The implementation target is Core
schema version 5.

Version 5 adds these logical tables:

| Table | Purpose |
| --- | --- |
| `error_record` | Aggregate identity and current projection. |
| `error_occurrence` | Immutable occurrence facts and canonical payload digest. |
| `error_relation` | Audited typed causal links. |
| `error_transition` | Append-only lifecycle state changes. |
| `error_disposition` | Rationale-backed decisions for occurrences. |

Required constraints include:

- unique `(project_id, fingerprint_version, fingerprint)` aggregates;
- unique `occurrence_id`;
- unique `(producer, idempotency_key)`;
- foreign keys from occurrences, transitions, and dispositions to their
  aggregate;
- checks for all closed enums and `sha256:` digest shapes;
- indexes for latest errors, unresolved errors, pending dispositions,
  fingerprint recurrence, work/run relations, and recovery import.

The generated database contract, contract hash, schema component catalog, table
shape validators, fixtures, and schema doctor output all advance together.
Migration follows the existing Core path:

1. open and inspect without mutation;
2. reject unknown, newer, corrupt, or shape-incompatible databases;
3. acquire the migration write lock;
4. create and verify the existing backup/recovery metadata;
5. migrate in one transaction;
6. run shape, component-catalog, and foreign-key validation;
7. set version 5 only as the final transaction step;
8. commit, or roll back to the fully usable prior schema.

Recognized versions 1 and 2, plus the explicitly supported obsolete version 3
and 4 shapes, normalize without changing existing causal IDs or payloads.
`ldgr init`, `ldgr status`, and `ldgr context` all open through the same
`ensure_schema` path, so each automatically applies the migration. Concurrent
openers either observe the old complete schema and wait/retry, or the new
complete schema; no caller may observe a partially installed error contract.

Injected failures are tested after backup, after table creation, before version
update, and before commit. An unknown future schema is reported read-only and
never normalized downward.

No historical terminal CLI failure is synthesized into an occurrence during
migration because there is insufficient structured identity. Existing runs,
validations, observations, decisions, events, and artifacts remain unchanged.
Recovery reconciliation may subsequently import envelopes with their original
occurrence identities.

### Startup reconciliation

`ldgr init`, `ldgr status`, `ldgr context`, and `ldgr loop run` reconcile
recovery state after opening the active Core database. Reconciliation scans the
project inbox first and the per-user spool second, in stable path/name order.
Per-user records are imported only when `project.project_id` exactly matches
the open database. A project-inbox record written before the database was
available may instead bind by its normalized project locator.
Schema-v1 records from the original agentctl writer may carry its historical
declaration-order fingerprint digest; the importer verifies that exact legacy
encoding as well as the canonical encoding without weakening classification or
project checks.

Each importer claims a file with a same-directory atomic rename. The database
import, causal relation creation, and any interrupted-run repair commit in one
write transaction; only then is the claimed file moved to `recovery/archive`.
Concurrent importers converge through the file claim and occurrence
idempotency identities. Abandoned claims are retried only after their owner
process is no longer live.

Malformed, partial, unknown-version, and ambiguous records move intact to
`recovery/quarantine`. Project-inbox quarantine also creates a redacted
first-class infrastructure error containing the payload digest and bounded
record name, never the rejected payload. Valid records for a different project
remain in the user spool for that project and are not claimed.

Execution intents include the producer process ID. A live producer keeps its
intent and active run authoritative. A dead producer or loop supervisor without
a terminal attempt transition creates an interruption occurrence. If it owns a
running Core run, reconciliation atomically finishes that run as `partial`,
restores its work item to `pending`, links the interruption, and emits run/work
repair events. The related work remains unclaimable until the blocking error has
an explicit disposition. Status and context remain available throughout this
gate; loop startup fails before worker launch and prints the error IDs requiring
disposition.

## Machine-readable API stability

Human and JSON command surfaces share the same domain values. JSON responses
include:

- `format: "ldgr-error"`;
- `schema_version`;
- complete IDs and lifecycle state;
- `repeated` and `occurrence_count`;
- `disposition_pending`;
- causal relations;
- explicit truncation/redaction metadata.

Fields are additive within a schema version only when older readers can safely
ignore them. Renames, meaning changes, enum changes, or fingerprint changes
require a new schema or fingerprint version as appropriate. Persisted unknown
values fail closed at Core trust boundaries rather than degrading to free text.

## Readiness and recovery invariants

An implementation conforms only if all of the following remain true:

- an accepted operation has a durable intent before effects begin;
- each attempt ends with a terminal result or a durable error whenever any
  declared sink is writable;
- no durable sink means execution aborts before untracked work;
- occurrences and state transitions are append-only audit facts;
- retries and recovery imports are idempotent under crashes and concurrency;
- repeated errors retrieve prior evidence and require a recorded decision;
- unresolved blocking errors and pending dispositions cannot be hidden by
  readiness or success summaries;
- inspection and recovery remain possible while progress is blocked;
- raw error messages and secrets are never identities;
- project identity prevents cross-project spool contamination;
- migrations are automatic for recognized databases and fail closed for
  unrecognized ones.
