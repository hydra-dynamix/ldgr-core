# LDGR Init Setup Prompt

## Mission

Initialize LDGR around the smallest useful loop: one work item, one run, observations/artifacts from that run, and one decision about what happens next. Capture only enough context for the next agent or human to continue without guessing.

## Required setup steps

### 1. Use the captured project context

`ldgr init` has already captured the current directory and repository outline so the model does not need to spend calls discovering them.

Current directory:

```text
{{PWD}}
```

Repository outline from `dev walk . --stdout --no-content`:

```text
{{DEV_WALK}}
```

Read additional files only when this outline is insufficient to choose the first bounded work item. Run `ldgr status` for the agent on-ramp; expand to `ldgr context` only when full cockpit detail is needed.

`ldgr init` also installs `.pi/extensions/ldgr-context.ts` for Pi-compatible harnesses. Identify the current harness. If it is Pi, run `/reload` when appropriate and use `/ldgr <args>` to run LDGR CLI commands and pipe stdout/stderr into the conversation; `/ldgr` with no args and `/ldgr-context` both capture `ldgr context --brief`. If the harness cannot load Pi extensions, read `.ldgr/harness-setup.md`; extension commands will not work, but `ldgr ...` remains available from the shell.

### 2. Identify the first loop

Create a concise summary that answers:

- What is the immediate objective?
- What result means the first work item is complete?
- What constraints, policies, or boundaries must be followed?
- What files, docs, or facts should the next run see?

Keep the summary short. It should reduce ambiguity, not become a second planning system.

### 3. Create one work item and one run

- Create one pending work item with `ldgr work create`.
- Start it with `ldgr run start`.
- Use `ldgr work edit` only to correct title/description, and `ldgr work status set` only for explicit lifecycle control.
- Record observations only when something durable changed or was learned.
- Add artifacts for durable files or evidence the next run should inspect; use `ldgr artifact show` to verify the record.
- Close the active run with `ldgr run close` once the bounded work is complete; it records the terminal run status and decision together.
- Before signing off or returning a final answer, complete/close the active run unless you are explicitly handing off unfinished work and have recorded that handoff.
- Include the next work item in that close command if one is known.

### 4. Queue only the next useful slice

If the task is larger than one run, queue the next focused work item. Do not prebuild a large backlog unless the current project already demands it.

Good work items should:

- have one clear outcome;
- be independently understandable from title and description;
- reference relevant requirements or source documents;
- avoid bundling unrelated implementation, checking, and documentation work;
- create follow-up items only when new scope is discovered.

If broad placeholder items already exist, replace them with smaller items and record why.

### 5. Keep setup core-only

Do not introduce adapter or research-layer records during core setup. If a
project already uses external research tooling, handle that separately after the
core ledger has one clear work item, run, observation/artifact trail, and close
decision.

### 6. Create or confirm a reusable loop prompt

Create a prompt file or LDGR artifact for future autonomous runs. It should instruct the model to:

- run `ldgr status` first and expand to `ldgr context` when needed;
- take exactly one pending work item;
- start one LDGR run;
- complete only that work item;
- record observations, artifacts, and decisions;
- queue follow-up work only when new scope is found;
- finish the LDGR run with an accurate status before signing off;
- report the next pending work item.

### 7. Validate the on-ramp

Before finishing setup:

- Run `ldgr status`.
- Confirm there is at most one obvious next pending work item.
- Confirm the next run can begin from the recorded context.
- Confirm no setup run is left active unintentionally.
- Run `ldgr --help` when you need the supported core command map.
- Run `<command> --help` rather than guessing; on invalid input LDGR prints help for the last command level it could parse.

## Completion report

In the final response, include:

- setup work item slug and final status;
- concise task summary;
- observations or decisions recorded;
- current next work item;
- any unresolved ambiguity that blocks the next run.
