# LDGR operator error policy

Substantive errors are first-class causal records, but ordinary working mistakes
are not. Every accepted operation that crosses a durable execution boundary
must end with either a terminal result or a durable, recoverable error
occurrence.

Do not create an error for rejected input, shell quoting/path mistakes,
read-only discovery failures, expected negative-test failures, or a failed
validation corrected safely within the same run. Correct those inline and
continue. Record an error when accepted work may have partial or ambiguous
effects, an infrastructure or integrity failure remains blocking after one
reasonable correction, a running process is interrupted after acceptance, or
the failure must survive handoff. Inspect recurrence with `ldgr error context
<error-id>` and record an explicit disposition only before repeating an
unchanged recorded attempt.

The normal surface accepts plain text. Use `ldgr error "<explanation>"` to
record, add `--resolve <error-id>` when the cause is fixed, or add `--update
<error-id>` for a later occurrence. Core supplies durable metadata and links
the active run and work item. Reserve `error record` and detailed dispositions
for integrations or exceptional audited retry decisions.

Require a durable checkpoint:

- after substantive unexpected behavior with durable or blocking impact;
- after a user correction only when it changes durable task scope or policy;
- at process handoff: record current state, evidence, unresolved errors, and
  the next authorized action;
- before exit only when an active run or unresolved handoff still needs a
  durable terminal state.

Use `ldgr error --help` for recording and disposition commands, `ldgr status`
for the compact control surface, and `ldgr context` for the bounded handoff.
