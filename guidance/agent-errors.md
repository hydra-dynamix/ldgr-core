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

For a substantive error, record only the explanation:

```text
ldgr error "focused test still fails"
```

Core generates all metadata and active run/work links. Do not classify the
error or supply IDs. When the cause is fixed, resolve it with one extra flag:

```text
ldgr error "the corrected test now passes" --resolve <error-id>
```

Use `--update <error-id>` to append a later occurrence. Inspect context or use
the detailed disposition commands only when an unchanged retry of a repeated
error genuinely needs that audit trail. `ldgr error record` is for integrations
that already own structured metadata.

Checkpoint when durable state or task scope changes, or when unresolved work is
being handed off. Do not run status or create ledger records merely because a
tool command was corrected or a response is about to end. Close the active run
accurately before handoff or completion.
