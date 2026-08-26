---
name: ldgr
description: Use ldgr to track work provenance on any durable, multi-step, or resumable task. Trigger on ldgr, ledger, work item, rehydrate, resume the work, what is next, or any long-horizon work worth recording.
license: Apache-2.0
---

# ldgr

Use ldgr to track work provenance.

If no `.ldgr/ldgr.db` exists:

```sh
ldgr init
```

Otherwise:

```sh
ldgr status
```

For deeper hydration:

```sh
ldgr context
```

Then run `ldgr workflow` to learn how to work this project, and
`ldgr <adapter> workflow` for an installed adapter.

Read `.ldgr/agent-errors.md` when present. Errors are first-class causal
records only when they have durable, blocking, ambiguous, or integrity-relevant
impact. Correct transient command, quoting, path, discovery, and expected-test
failures inline without ledger ceremony. Record an error when an accepted
operation may have partial effects, remains blocked after one reasonable
correction, or must survive handoff. Repeated substantive errors require prior
context and an explicit disposition before an unchanged retry.

Record ordinary substantive failures with `ldgr error <command> <type> <msg>`.
Use a stable command label without arguments; Core supplies identities,
fingerprints, timestamps, policy fields, and active run/work links. Use the
detailed `ldgr error record` command only when an integration owns that metadata.

## Do not run these

`ldgr install` and `ldgr adapter install` are interactive and intended for the
human operator. Running them will block waiting for input you cannot provide.
If either is needed, tell the user to run it themselves.
