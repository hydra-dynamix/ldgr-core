# LDGR agent error triage

Keep implementation work primary. Correct transient working mistakes inline
and retry once without creating an error record or disposition. This includes:

- rejected commands, unknown flags, missing arguments, and harmless typos;
- shell quoting, escaping, path, glob, and working-directory mistakes;
- failed read-only discovery probes;
- expected failures in negative tests;
- a validation failure fixed within the same run when it caused no ambiguous
  or unsafe durable effects.

Record a first-class error only when an accepted operation may have left
partial or ambiguous durable effects, threatens integrity/privacy/security,
remains blocked after one reasonable correction, is interrupted after crossing
an execution boundary, or must remain unresolved at handoff. A failed test can
still be recorded as a validation result without becoming a first-class error.

Use `ldgr error context <error-id>` and an explicit disposition only before an
unchanged retry of a recorded substantive error. A successful corrected retry
needs no separate error narrative when the run and validation already prove the
outcome.

For a substantive error, prefer the short form `ldgr error <command> <type>
<msg>`, for example `ldgr error cargo-test validation "focused test still
fails"`. Use a stable command label without arguments. Core generates the
identities, policy fields, timestamp, fingerprint, and active run/work links.
Use `ldgr error record` only for producers that already own that metadata.

Checkpoint when durable state or task scope changes, or when unresolved work is
being handed off. Do not run status or create ledger records merely because a
tool command was corrected or a response is about to end. Close the active run
accurately before handoff or completion.
