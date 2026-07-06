# LDGR loop runtime

`ldgr loop run` executes one bounded single-agent cycle for the next pending work item. The runtime starts a run, renders one prompt from the selected prompt source(s), launches one fresh agent process, captures output artifacts, and then finishes the run unless the agent already closed it with a decision.

## Prompt sources

`ldgr install` seeds global defaults under `~/.ldgr/prompts/` or `$LDGR_HOME/prompts/`. Existing prompt files are preserved during install. `ldgr init` may also copy editable project prompt files under `.ldgr/prompts/` for explicit `--prompt <PATH>` use, but `--prompt-slug <SLUG>` looks only in the global prompt directory.

Default core prompt files:

- `ldgr-core-loop.md`
- `ldgr-loop-invariants.md`

Examples:

```sh
ldgr loop run --prompt .ldgr/prompts/ldgr-core-loop.md --agent agentctl
ldgr loop run --prompt .ldgr/prompts/ldgr-core-loop.md --prompt ./prompts/project-rules.md --agent agentctl
ldgr loop run --prompt-slug core-loop --prompt-slug project-rules --agent agentctl
ldgr prompt compose project-loop --source core-loop --source ./prompts/project-rules.md
ldgr loop run --prompt-slug project-loop --agent agentctl
```

Repeated `--prompt` and/or `--prompt-slug` fragments are concatenated in CLI order into one rendered loop prompt. Composite prompt provenance records every component source and hash when available.

## Artifacts

Each run records:

- `loop-run-<id>-prompt.md` - rendered prompt with assigned work and LDGR context.
- `loop-run-<id>-prompt-provenance.json` - prompt source/hash provenance.
- `loop-run-<id>-agent-output.md` - stdout/stderr preview and links to full captures.
- `loop-run-<id>-agent-stdout.txt` and `loop-run-<id>-agent-stderr.txt` - full captured streams when produced.

## Controls

Useful flags:

- `--agent agentctl` or `--agent-argv '["cmd", "arg"]'`.
- `--dry-run` to render artifacts without spawning an agent.
- `--stream-agent-output` to tee stdout/stderr to the terminal while preserving artifacts.
- `--agent-timeout-seconds <N>` to bound child process runtime; `0` disables the wall-clock timeout.
- `--max-iterations <N>` to run up to N bounded cycles.
- `--until-empty` to keep launching fresh cycles until no pending work remains or the loop blocks.
- `--summary-agent agentctl` or `--summary-argv '[...]'` to append compact post-cycle summaries.

Active notices appear in context as `binding_directives`; agents should treat them as binding unless they conflict with safety or explicit system/developer instructions.
