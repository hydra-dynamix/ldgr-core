# ldgr

A minimal durable investigation loop backed by SQLite.

`ldgr` gives autonomous agents (and the humans steering them) a durable loop for
work items, runs, observations, decisions, and context. Instead of trusting a
model to keep its own task list and memory, `ldgr` keeps them in one SQLite file
that survives restarts, crashes, and context resets.

## Core ideas

1. **Bound the work.** One work item per loop cycle, nothing more.
2. **Externalize memory.** Observations, artifacts, and decisions live in the
   ledger, not in a context window.
3. **Decide what happens next.** Each run ends with a decision: continue with a
   next work item, stop, or record why the work cannot proceed.
4. **Separate discovery from execution.** New problems become queued work items,
   not detours that derail the current run.
5. **Start small.** The core loop is intentionally compact; learn the basic
   work/run/decision rhythm before adding more process.

See `docs/ldgr-loop-philosophy.html` for the longer explanation of the loop.

## Install

```sh
curl -fsSL https://raw.githubusercontent.com/hydra-dynamix/ldgr-core/main/scripts/install.sh | sh
```

The installer detects the current OS/CPU, downloads the matching release archive,
verifies its SHA-256 checksum when checksum tooling is available, and installs
`ldgr` to `~/.local/bin` by default. Override with:

```sh
LDGR_VERSION=0.1.5 LDGR_INSTALL_DIR="$HOME/bin" sh -c "$(curl -fsSL https://raw.githubusercontent.com/hydra-dynamix/ldgr-core/main/scripts/install.sh)"
```

Source install remains available:

```sh
cargo install --git https://github.com/hydra-dynamix/ldgr-core --locked --force --package ldgr-core
# or, from a source checkout:
git clone https://github.com/hydra-dynamix/ldgr-core
cd ldgr-core
cargo install --path .
```

SQLite is bundled; source fallback requires a recent stable Rust toolchain.

## Quick start

```sh
ldgr init                          # create .ldgr/ldgr.db and print the on-ramp
ldgr work create my-first-task \
  --title "Investigate X" \
  --description "Figure out why X happens and record evidence."
ldgr work edit my-first-task --description "Figure out why X happens; record evidence."
ldgr work status set my-first-task pending
ldgr run start my-first-task --command "manual investigation"
ldgr observe my-first-task --body "X happens when Y."
ldgr artifact add my-first-task --kind report --path notes/x-report.md --description "Investigation notes."
ldgr artifact show 1
ldgr run close my-first-task --status success --outcome continue \
  --rationale "Y confirmed as the trigger." \
  --next-slug fix-y --next-title "Fix Y" --next-description "Patch Y handling."
ldgr status                        # compact agent-first status summary
ldgr context --brief               # compact agent on-ramp
ldgr status --json                 # compact machine-readable handoff
ldgr context                       # the operational cockpit, also: ldgr context --json
```

That loop is the day-one model: work, run, observation, artifact, decision,
notice, and context. `ldgr observe` is a shorthand for recording run observations;
`ldgr observation add` remains available. Commands that attach evidence to a run
accept either numeric run IDs or work-item slugs. `ldgr run close` is the
recommended closure path for active runs because it records the terminal run
status and work decision together.

Use `ldgr --help` or `ldgr <command> --help` to explore the command surface.

## Structured queues and portable schedules

Work items can carry priority, program, group, acceptance criteria, and enforced
dependencies. Dependencies form an acyclic graph: an item is not ready and
cannot be started until every prerequisite is done.

```sh
ldgr work create registry --title "Registry" --description "Build it." \
  --priority P0 --program audit --group accounts
ldgr work create atomicity --title "Atomicity audit" --description "Audit updates." \
  --priority P0 --program audit --group accounts \
  --acceptance-criteria "Concurrent update test passes." \
  --depends-on registry
ldgr status --program audit --priority P0
```

Use a JSON schedule to create or back up a large queue in one command. Imports
are transactional, and `--upsert` updates matching slugs.

```sh
ldgr work export --output .ldgr/schedule-backup.json
ldgr work import schedule.json
ldgr work import schedule.json --upsert
```

The portable format is `ldgr.schedule.v1`; exported records include lifecycle
status, structured metadata, hold classification, and dependency slugs. The
SQLite ledger remains the source of truth for run history and evidence, while
the schedule export is suitable for versioned queue backup.

## The autonomous loop

`ldgr loop run` drives an agent through one or more bounded cycles: each cycle
picks the next pending work item, renders a prompt with the current ledger
context, pipes it to the configured agent, and records the output as a run
artifact. Prompts can come from an editable file path, a durable active prompt
record, or a sealed prompt bundle. Use `--max-iterations N` to run multiple
cycles; the loop stops early when work is blocked, no pending work remains, or a
subprocess fails.

```sh
ldgr loop run --prompt prompts/loop-prompt.md --agent agentctl    # use the ldgr-loop agentctl entry from ldgr install
ldgr loop run --prompt-slug surface --agent agentctl       # use an active stored prompt
ldgr loop run --bundle cleanroom --prompt-role surface-loop # use a sealed bundle
ldgr loop run --prompt prompts/loop-prompt.md --agent-argv '["my-agent"]' # any command that reads the prompt on stdin
ldgr loop run --prompt prompts/loop-prompt.md --dry-run            # render artifacts without spawning anything
```

`ldgr install` writes `~/.agentctl/config.toml` entries named `ldgr-loop` and
`ldgr-loop-<harness>` so the built-in `--agent agentctl` runner can call
`agentctl run ldgr-loop` and stream the rendered prompt through stdin.

Prompt records live in the ledger with slug, role, body, hash, status, and
version history. Updating a prompt creates a new version while preserving prior
content. Prompt bundles seal active prompt versions into an immutable manifest
and bundle hash:

```sh
ldgr prompt create surface --role surface-loop --body '... {{ldgr_context}} ...'
ldgr prompt import implementation --role implementation-loop --path prompts/impl.md
ldgr prompt update surface --path prompts/surface-v2.md
ldgr prompt activate surface
ldgr bundle create cleanroom --prompt surface --prompt implementation
ldgr bundle seal cleanroom
```

Loop runs that use stored prompts or bundles write prompt provenance artifacts
with the exact prompt slug, version, content hash, and bundle hash used.

Operator steering outside a run is represented as notices:

```sh
ldgr notice add --kind notification --body "Prefer the simpler fix in module Z."
ldgr notice edit 1 --body "Course correction handled."
ldgr notice clear 1 --reason "Applied."
```

## Daily use

`ldgr` is designed to be used continuously while work is happening:

- Start with one concrete work item.
- Start a run when you or an agent begins that work.
- Record observations as facts become clear.
- Attach artifacts when files, reports, logs, or notes matter.
- Close the run with a decision and, when appropriate, queue the next bounded
  piece of work.

The goal is not to create a large planning database up front. The goal is to
keep a durable handoff that always answers: what is active, what was observed,
what was decided, and what should happen next?

## Web cockpit

```sh
ldgr web            # serves http://127.0.0.1:8686
```

A live dashboard over the ledger: work distribution, execution flow,
decisions, observations, artifacts, and loop controls. Even on loopback, all
mutating routes require `X-LDGR-Control-Token`. When `--control-token` is not
provided, `ldgr web` generates an ephemeral token at startup and prints a local
URL containing `?control_token=...`; the bundled UI stores that value in browser
session storage before posting mutations. Exposing the cockpit beyond loopback
requires `--unsafe-expose` together with an explicit `--control-token`.

## Rust library

The `ldgr-core` crate also exposes Rust modules for applications that want to
build on the same ledger:

- `adapter_manifest` for public adapter manifest parsing and validation,
  including optional command namespace declarations.
- `store` for the SQLite-backed work, run, observation, artifact, decision,
  prompt, notice, event, and context records.
- `loop_runtime` for bounded autonomous loop execution.
- `cli` for the command runner used by the `ldgr` binary.
- `web` for the local cockpit server.
- `tool_runner` for command rendering and argv parsing helpers.

### Adapter command manifests

Open adapter manifests may omit command extensions. When an adapter wants core
to expose an adapter-owned command namespace, declare one or more `[[commands]]`
tables:

```toml
[[commands]]
namespace = "community-sample"
argv = ["community-sample"]
aliases = ["sample", "community"]
title = "Community sample commands"
description = "Commands exposed through the core LDGR command surface."
capabilities = ["dispatch", "help"]

[commands.help]
usage = "ldgr community-sample <command> [options]"
summary = "Run community sample adapter commands."
details = "Arguments after the namespace are forwarded to the adapter executable."
```

`ldgr::adapter_manifest::parse_adapter_manifest` validates namespace syntax,
duplicate command aliases, empty `argv`, and malformed command declarations with
clear errors while preserving existing manifest digest behavior.

## Where data lives

State lives in `.ldgr/` inside the project where you run `ldgr`:

- `.ldgr/ldgr.db` is the SQLite ledger.
- `.ldgr/artifacts/` stores managed artifacts created by LDGR.

The ledger is local-first and survives restarts, crashes, and context resets.
You can inspect the current handoff at any time with `ldgr status` or
`ldgr context`. Released schema-v1 ledgers migrate transactionally to schema v2
when first opened; existing work, runs, evidence, and decisions are preserved.

## License

Licensed under the [Apache License, Version 2.0](LICENSE).

Unless you explicitly state otherwise, any contribution intentionally submitted
for inclusion in this work by you, as defined in the Apache-2.0 license, shall
be licensed as Apache-2.0, without any additional terms or conditions.
