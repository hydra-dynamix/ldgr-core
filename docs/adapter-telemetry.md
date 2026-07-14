# Adapter numerical transition contract

LDGR Core owns consent, sequence buffering, validation, preview, serialization,
and network transmission. An adapter's only telemetry integration point is the
Core transition API in `ldgr::telemetry::transition`.

An adapter publishes a fixed `NumericalProtocol` containing only:

- its versioned `/sequences/<state-machine>/vN` routing endpoint;
- its declared integer states;
- its allowed integer-to-integer transitions; and
- its sequence-length bound.

The request body never contains the endpoint. It selects the state machine
locally and Core later sends only the validated bare integer array.

Start `CommittedSequence` only after the initial state commits to the adapter's
local ledger. Call `submit_committed` only after each corresponding state change
commits successfully. The API rejects undeclared states, undeclared edges,
self-transitions, transitions after a terminal state, malformed declarations,
and sequences beyond the declared bound without mutating the accepted sequence.

The transition API intentionally has no parameter for a project, repository,
work item, run, user, label, timestamp, command, path, error, or other content.
Do not derive, hash, tokenize, or otherwise encode any such value into a state.
States describe reusable workflow positions only.

Codes `3` through `7` retain their normalized terminal meanings:

```text
3 completed-positive
4 completed-negative
5 completed-inconclusive
6 operational-failure
7 cancelled
```

A completed negative result is an executed investigation and a useful
counterexample. It must use `4`, never operational-failure (`6`).

Adapters must not read the Core consent file, open a telemetry connection,
serialize an upload, add payload fields or headers, or make normal operation
conditional on collection. Core may discard any submitted transition when
consent is absent, the kill switch is active, or validation fails.

Minimal Core-work example:

```rust
use ldgr::telemetry::transition::{
    CommittedSequence, COMPLETED_NEGATIVE, CORE_WORK_V1, RUNNING,
};

let mut sequence = CommittedSequence::begin_after_commit(&CORE_WORK_V1)?;

// Commit the running state to the local ledger first.
sequence.submit_committed(RUNNING)?;

// Commit the negative result to the local ledger first.
sequence.submit_committed(COMPLETED_NEGATIVE)?;
```

The normative privacy, terminal, and wire requirements remain in the LDGR
numerical sequence policy and protocol v1 documents. This API does not authorize
collection and does not give adapters a separate consent or transport surface.
