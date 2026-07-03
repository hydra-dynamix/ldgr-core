# Generic multi-role loop

LDGR's autonomous loop is still one bounded work item per cycle, but the default
runtime now executes that cycle as four fresh role invocations over the same
project ledger: planner, worker, scryb, and validator. Each role starts from the
assigned-work section plus LDGR status/context rendered into its prompt. Do not
assume continuity from chat history or a previous process; durable LDGR records
and artifacts are the handoff.

The legacy single-prompt entrypoints remain accepted. The compatibility prompt
name is `ldgr-core-loop.md`, and the loop still writes the historical
`loop-run-<id>-agent-output.md` artifact so existing consumers have a stable
summary/output path.

## Role model

The generic role order is fixed:

1. **planner** - reads current context and chooses a bounded plan for exactly
   the assigned work item. It should identify the next claim, uncertainty, or
   risk to test and give the worker a minimal execution path. It may recommend
   stopping only when no valuable bounded branches remain.
2. **worker** - executes the plan for the assigned work item, records evidence,
   runs practical validation, and queues concrete follow-up work for discovered
   gaps that are outside the current scope.
3. **scryb** - preserves continuity. It records concise summaries and indexes
   to the evidence produced in the cycle without inventing new validation
   claims.
4. **validator** - independently reviews methodology, interpreted outcomes,
   claim strength, evidence quality, risks, and next-direction recommendations.
   It advises the next planner/worker cycle and may request a narrow set of
   guarded operational actions described below.

All four roles receive the same assigned-work guardrail: complete exactly the
selected LDGR work item and do not silently switch to another pending item.

## Prompt customization and provenance

`ldgr init` seeds editable project prompts under `.ldgr/prompts/` and
`ldgr install` seeds global defaults under `~/.ldgr/prompts/` or
`$LDGR_HOME/prompts/`. Existing prompt files are preserved during install/init.

Default generic prompt files:

- `ldgr-loop-planner.md`
- `ldgr-loop-worker.md`
- `ldgr-loop-scryb.md`
- `ldgr-loop-validator.md`
- `ldgr-core-loop.md` (compatibility prompt)

Prompt sources for `ldgr loop run` are mutually exclusive:

```sh
ldgr loop run --prompt .ldgr/prompts/ldgr-core-loop.md --agent agentctl
ldgr loop run --prompt-slug generic-planner --agent agentctl
ldgr loop run --bundle cleanroom --prompt-role planner --agent agentctl
```

For durable prompt provenance, import edited prompt files into the prompt store
and activate/update them rather than relying only on loose files:

```sh
ldgr prompt import generic-planner --role planner --path .ldgr/prompts/ldgr-loop-planner.md
ldgr prompt activate generic-planner
ldgr prompt update generic-planner --path .ldgr/prompts/ldgr-loop-planner.md
```

Every loop run records prompt provenance artifacts with the exact prompt source,
version/hash when available, and bundle hash when a sealed bundle is used.

## CLI flags and defaults

Important `ldgr loop run` flags verified against the implemented help surface:

- `--prompt <PATH>` - render from an editable prompt file.
- `--prompt-slug <SLUG>` - render from an active stored prompt.
- `--bundle <SLUG>` with `--prompt-role <ROLE>` - render from a sealed prompt
  bundle; `--prompt-role` selects the bundle prompt when multiple prompts exist.
- `--agent agentctl` - use the installed `agentctl run ldgr-loop` preset.
- `--agent-argv '["cmd", "arg"]'` - run any command that reads the prompt on
  stdin.
- `--summary-agent agentctl` or `--summary-argv '[...]'` - run a post-cycle
  summarizer.
- `--summary-log <PATH>` - append post-cycle summaries; default:
  `.ldgr/logs/loop-summary.md`.
- `--project-complete-requested` plus `--audit-argv '[...]'` - require a fresh
  completion audit before handling project-completion requests.
- `--dry-run` - render and record artifacts without consuming the pending work.
- `--stream-agent-output` - tee subprocess output while still recording it.
- `--agent-timeout-seconds <N>` - per-role subprocess timeout; `0` disables the
  wall-clock timeout.
- `--max-iterations <N>` - run up to N bounded cycles.
- `--until-empty` - keep launching fresh cycles until no pending work remains or
  the loop blocks.
- `--db <PATH>` and `--artifact-root <PATH>` are inherited core flags; defaults
  are `.ldgr/ldgr.db` and `.ldgr/artifacts`.

The loop blocks rather than starting a new cycle when another work item already
has a running run, when a blocking loop intervention exists, or when there is no
pending work.

## Default report and artifact locations

Unless `--artifact-root` is supplied, run artifacts are written under
`.ldgr/artifacts/` and registered in the ledger. The generic multi-role loop
writes these stable paths for run `<id>`:

- `loop-run-<id>-prompt.md` and `loop-run-<id>-prompt-provenance.json` - the
  compatibility/top-level rendered prompt and provenance.
- `loop-run-<id>-<role>-prompt.md` and
  `loop-run-<id>-<role>-prompt-provenance.json` - per-role prompt artifacts.
- `loop-run-<id>-<role>-agent-output.md` - per-role stdout/stderr capture.
- `loop-run-<id>-agent-output.md` plus optional stdout/stderr text files -
  compatibility output path updated from the latest role output.
- `loop-run-<id>-scryb-cycle-summary.md`,
  `loop-run-<id>-scryb-reference.md`, and `loop-meta-report.md` - scryb-maintained
  evidence indexes.
- `loop-run-<id>-validator-advisory.md` - validator handoff report.
- `loop-run-<id>-validator-ops.md` - validator operational action audit report
  when the validator emits an ops request.
- `loop-run-<id>-summary.md` - optional post-cycle summary output.
- `loop-run-<id>-completion-audit.md` - completion audit output when
  `--project-complete-requested` is used.

Treat generated scryb reports as evidence indexes, not as independent proof of
correctness. The validator advisory points to the role output that contains the
substantive review.

## Validator authority boundaries

The validator is primarily advisory. It should not become the primary executor
and should not broaden a bounded work item into whole-project certification.

The validator should favor proportionate, risk-based acceptance over perfectionism:
accept useful good-enough work when the evidence is adequate for the bounded
item, and refuse only materially inadequate methodology, evidence, validation,
safety, or claim strength.

For a material refusal, the runtime recognizes exactly one optional fenced block
named `ldgr-validator-revision json` in validator output:

```json
{
  "rationale": "tests were claimed but not run",
  "required_corrections": [
    "run the stated validation and record output",
    "weaken unsupported correctness claims"
  ],
  "affected_artifacts": ["loop-run-12-worker-agent-output.md"],
  "affected_work_items": ["current-work-slug"]
}
```

When the validator exits successfully and the block is well-formed, LDGR writes a
`loop-run-<id>-validator-revision.md` report, records a planner-visible
observation, closes the current cycle as partial/continue, and creates a linked
pending child work item named `<work-slug>-revision-run-<id>` with bounded worker
instructions. Malformed refusal blocks are denied safely and audited without
creating revision work.

The runtime also recognizes exactly one optional fenced block named
`ldgr-validator-ops json` in validator output. Supported actions are:

```json
{
  "actions": [
    {
      "action": "clear_block",
      "intervention_id": 123,
      "rationale": "why this is safe",
      "evidence": ["observed evidence path or command"]
    },
    {
      "action": "merge_worktree",
      "worktree": "/path/to/isolated/worktree",
      "rationale": "why this merge is safe",
      "validation_evidence": ["validation command/result"]
    }
  ]
}
```

Operational actions are guarded: the validator process must exit successfully,
rationale and evidence must be present, and runtime safety checks must pass.
`clear_block` can clear pending loop interventions. `merge_worktree` only
attempts a non-force merge from a same-repository clean worktree into a clean
target; dirty worktrees, missing validation evidence, unrelated repositories,
merge conflicts, and unsafe states are denied safely and audited. Denial is not a
bypass mechanism; record follow-up work instead of forcing the action.

## Migration from the single-agent loop

Existing usage can keep calling `ldgr loop run` with the same pending-work model.
The migration path is incremental:

1. Keep existing commands working:

   ```sh
   ldgr loop run --prompt .ldgr/prompts/ldgr-core-loop.md --agent agentctl --max-iterations 1
   ```

2. Review generated artifacts after a cycle. Existing tooling may keep reading
   `loop-run-<id>-agent-output.md`; richer tooling should prefer the per-role
   prompt/output artifacts and scryb/validator reports.
3. Move local prompt edits from the old monolithic prompt into the role prompt
   whose responsibility matches the edit.
4. Import role prompts into the prompt store or sealed bundles when prompt
   provenance matters.
5. Use `--dry-run` to check prompt rendering and artifact paths before letting a
   fresh agent consume work.

Compatibility behavior to preserve during migration:

- one pending work item is still claimed per cycle;
- the loop still stops on blocked/running/no-pending states;
- loose prompt files, stored prompts, and sealed bundles remain valid prompt
  sources;
- legacy `agent-output` artifact paths are still produced;
- project-completion handling still requires the explicit completion flag and
  external audit command.

## Generic examples

One bounded generic cycle through agentctl:

```sh
ldgr init
ldgr work create docs-loop-smoke \
  --title "Document one loop behavior" \
  --description "Verify one behavior and record evidence."
ldgr loop run --prompt .ldgr/prompts/ldgr-core-loop.md --agent agentctl --max-iterations 1
ldgr status
```

Render prompts and artifacts without running agents:

```sh
ldgr loop run --prompt .ldgr/prompts/ldgr-core-loop.md --dry-run --max-iterations 1
```

Use a custom command instead of agentctl:

```sh
ldgr loop run \
  --prompt .ldgr/prompts/ldgr-core-loop.md \
  --agent-argv '["python3", "scripts/my-agent.py"]' \
  --summary-argv '["python3", "scripts/summarize-cycle.py"]'
```

## Research-specialized loop

The research adapter keeps the same bounded core loop but adds a semantic
research overlay: programs, branches, questions, options/hypotheses,
experiments, metrics, facts, and research-specific status/context. Install and
initialize the adapter once per project, then use the canonical adapter surface:

```sh
ldgr adapter install research --yes
ldgr research init
ldgr research mode status
ldgr research doctor
ldgr research status
ldgr research context
```

`ldgr-research install` copies the research prompt to
`~/.ldgr/prompts/research-loop.md` (or `$LDGR_HOME/prompts/research-loop.md`).
`ldgr research init` imports and activates the `research-loop` prompt in the
project ledger and enables research mode. When research mode is enabled,
`ldgr research loop run` forwards to the core loop and injects
`--prompt-slug research-loop` unless the operator supplies `--prompt`,
`--prompt-slug`, or `--bundle` explicitly:

```sh
ldgr research loop run --max-iterations 1
# equivalent default forwarding: ldgr loop run --prompt-slug research-loop --max-iterations 1

ldgr research loop run --prompt custom-research-loop.md --max-iterations 1
ldgr research loop run --bundle research-bundle --prompt-role research-loop --max-iterations 1
```

Use the research surface for evidence and work where possible:

```sh
ldgr research observation add <run-id> --body "Evidence from this experiment."
ldgr research validation record <run-id> --outcome pass --command "cargo test" --rationale "Relevant tests passed."
ldgr research work create next-hypothesis --title "Test next hypothesis" --description "One bounded research cycle."
```

For core command names that conflict with research primitives, use the explicit
escape hatch:

```sh
ldgr research core run close <run-id> --status success --outcome continue --rationale "..."
ldgr research core artifact add <run-id> --kind report --path output.txt --description "Transcript"
```

Routine research cycles should stay thin and evidence-linked: compact summaries,
validation records, concise decisions, and one next hypothesis/work item when
needed. Reserve long narrative reports for promotion points such as claim graph
changes, surprising negatives, external-validity shifts, or milestone synthesis.

## Smoke review evidence

This document was checked against the implemented command surfaces with:

```sh
cd ldgr-core && cargo run --quiet -- loop run --help
cd ldgr-research && cargo run --quiet --bin ldgr-research -- loop run --help
```

Both commands expose the loop flags listed above, including prompt source
selection, artifact root, summary/audit options, dry-run, streaming, timeout, and
iteration controls.
