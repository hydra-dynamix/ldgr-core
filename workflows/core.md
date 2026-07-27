# ldgr core workflow

You are the primary agent on this project. This is the workflow ldgr expects you
to follow. Read it once, then work it top to bottom.

## Why this workflow is shaped this way

Models resolve a task by the least costly action that appears to satisfy it.
Given a large or vague work item, the cheapest satisfying moves are to narrow
the scope, defer part of the work, substitute a mock or stub, or report that the
task is too complex to complete. Each of those is rational under the prompt and
fatal to the project.

Decomposition removes those options. When a work item is already sized to a
single model output, there is no smaller scope to retreat to, nothing left to
defer, and no interpretation under which a stub is less work than the real
implementation. Completing the item honestly becomes the path of least action.

The dependency graph carries the ordering and prevents collisions between items,
so no agent needs to hold the project in its head. That is the trade: you spend
effort once, up front, on decomposition, and in exchange every downstream cycle
is a narrow task performed by a fresh, uncluttered instance. When the queue
drains, the project is built.

## 1. Establish requirements

Work from the strongest available input, in this order:

1. a written spec;
2. a detailed prompt;
3. an ambiguous prompt.

If you are at (3), do not start building. Move up the list first.

## 2. Interview the user to the configured depth

Check the project's requirements-inquiry setting and respect it:

| Setting | Behavior |
| --- | --- |
| `high` | Conduct a full interview, one question at a time. Record the transcript as a document and attach it to ldgr as an artifact. |
| `medium` | Ask up to ten questions. Record the answers as an artifact. |
| `low` | Ask the five most important questions. Record the answers as observations. |
| `none` | Ask nothing. Infer requirements as accurately as you can and record the assumptions you made. |

Ask one question per turn when interviewing. Do not batch questions into a wall
of text. Whatever the setting, the answers must end up in the ledger — an
interview that lives only in the conversation is lost at the next context reset.

## 3. Write the spec

If no spec exists, write one. Be thorough. Cover every component and every
dependency.

The spec describes a complete, operational, production-quality application.
It does not describe:

- mocked or simulated behavior standing in for real behavior;
- simplified implementations to be revisited later;
- deferred work or phase-two placeholders;
- partially functional MVPs.

If a requirement is genuinely out of scope, say so explicitly in the spec. Do
not encode it as a stub and let a later agent discover the gap.

## 4. Decompose and schedule

Break the spec into atomic work items and schedule them in ldgr with an explicit
dependency graph.

- **Sizing.** Make each item as small as it can usefully be. The baseline
  primitive is a single model output. Keep a larger idea intact only when
  splitting it would break its logic or leave an inconsistent intermediate
  state.
- **Self-sufficiency.** Every item must carry the context needed to complete it,
  plus pointers to anything further. The test: an agent holding only
  `ldgr status` and the work item description should be able to finish the task
  without hunting for answers. If it would have to go digging, the description
  is incomplete.
- **Dependencies.** Encode real prerequisites as edges. Readiness is then a
  property of the graph, not a judgment call made in a context window.
- **Acceptance criteria.** State what "done" means for the item before any agent
  starts it.

## 5. Run the loop

Match the loop to how the user is engaging:

- If the user is actively working alongside you, run **one iteration at a time**
  so they stay in the loop and can redirect.
- Otherwise run the loop with **`--until-empty`** and let it work the queue.

## 6. Validate what lands

Landed work is not finished work. For each completed item, confirm that:

- test coverage exists and passes;
- the acceptance criteria are actually met;
- the code follows *one file, one job*.

Schedule follow-up work items for every gap you find. Clear blockers rather than
routing around them, make the executive decisions the project needs, and record
those decisions in ldgr with their rationale.

## 7. Close out the project

When the queue is empty, the project should do what was asked. Verify that
before saying so: run smoke tests, exercise the real paths, and confirm the
application operates end to end.

**An empty work queue is not a completed project.** Completion is a validated
claim, and the validation is part of the work.
