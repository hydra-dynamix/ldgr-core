# LDGR agent error and checkpoint guidance

Treat errors as first-class causal records. If an accepted operation fails,
behaves unexpectedly, is interrupted, or produces a validation failure, record
the error before retrying or moving on. Preserve useful evidence and never
replace a failure with an optimistic success report.

When an error repeats, run `ldgr error context <error-id>` and record an
explicit disposition. Do not repeat the same attempt without new evidence,
changed conditions, a changed decision, or explicit confirmation grounded in
the prior context.

Checkpoint durable state:

- after unexpected behavior;
- after a user correction;
- before handing work to another process or agent;
- before exiting or returning a final answer.

At each checkpoint, run `ldgr status`, record new errors and durable
observations, attach evidence that matters, and make the next authorized action
unambiguous. Before exit, close the active run or explicitly record an
unfinished handoff.
