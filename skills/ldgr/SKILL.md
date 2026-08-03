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
records: checkpoint after unexpected behavior, user corrections, process
handoff, and before exit. Record accepted-operation failures before retrying;
repeated errors require prior context and an explicit disposition.

## Do not run these

`ldgr install` and `ldgr adapter install` are interactive and intended for the
human operator. Running them will block waiting for input you cannot provide.
If either is needed, tell the user to run it themselves.
