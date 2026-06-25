# Multi-agent write story

Status: decided 2026-06-11 (ldgr decision recorded against
`decide-multi-agent-write-story`). One page; revisit only if the chosen model
fails in practice.

## The question

ldgr stores everything in one SQLite file. What happens when more than one
process touches it at once — and what should the supported story be as
adapters and MCP clients multiply the number of writers?

Real concurrency observed during ldgr's own development (2026-06-11): the web
cockpit polling reads every 5 seconds, an autonomous codex loop writing run
records, and an operator session writing observations — all against one
ledger, simultaneously.

## Options considered

**(a) Status quo, documented.** Default rollback journal, no busy timeout.
Concurrent access mostly works because ldgr's write transactions are
milliseconds long, but any overlap surfaces as an immediate
`database is locked` error with no retry. The failures are rare, random, and
unhelpful — the worst kind.

**(b) One ledger, WAL + busy timeout.** Two pragmas at open. `busy_timeout`
makes lock contention wait-and-retry instead of failing instantly. WAL lets
readers proceed while a writer commits, which is exactly the cockpit-polls-
while-loop-writes shape. Caveat: WAL needs shared-memory support and can be
refused on network or 9p filesystems (including some WSL `/mnt/*` mounts), so
it cannot be assumed unconditionally.

**(c) Ledger-per-role profiles.** Separate databases for orchestrator,
implementer, validator, etc. Maximum isolation, but it fragments the causal
chain that is ldgr's entire value, demands cross-ledger linking machinery, and
adds the kind of config surface the distillation mandate forbids. Global
observation 10 already parked this idea: reassess only if shared context
becomes a practical problem.

## Decision: (b), with graceful WAL fallback

`open_store` now:

1. sets `busy_timeout = 5000` unconditionally — pure win, no files added,
   works on every filesystem;
2. attempts `journal_mode = WAL` and accepts whatever mode SQLite actually
   grants. On filesystems that refuse WAL, the database stays in rollback
   mode and the busy timeout still absorbs contention.

No new commands, config, or record types. Option (c) is explicitly deferred,
not designed.

## The supported model

- **One ledger per project.** Any number of readers; writers are serialized
  by SQLite and wait up to five seconds instead of erroring.
- Write transactions must stay short — the existing savepoint discipline in
  `in_write_transaction` already ensures this; keep it that way.
- WAL adds `-wal`/`-shm` siblings next to `ldgr.db` when granted. They are
  state, live in `.ldgr/`, and are already gitignored.
- Sustained multi-second writers (none exist today) would need this decision
  revisited before being built.
