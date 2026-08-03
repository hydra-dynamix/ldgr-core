# LDGR operator error policy

Errors are first-class causal records, not disposable console text. Every
accepted operation must end with either a terminal result or a durable,
recoverable error occurrence.

Record an error when accepted work, validation, infrastructure, or a running
process fails or is interrupted. Do not create an error for input rejected
before acceptance unless the rejection itself reveals unexpected behavior.
Inspect recurrence with `ldgr error context <error-id>` and record an explicit
disposition before repeating an unchanged attempt.

Require a durable checkpoint:

- after unexpected behavior: record the occurrence and relevant evidence;
- after a user correction: preserve the correction in the active run or
  operator notice and update the bounded work item when its scope changed;
- at process handoff: record current state, evidence, unresolved errors, and
  the next authorized action;
- before exit: run `ldgr status`, record any unpersisted error, and close the
  active run or explicitly record the unfinished handoff.

Use `ldgr error --help` for recording and disposition commands, `ldgr status`
for the compact control surface, and `ldgr context` for the bounded handoff.
