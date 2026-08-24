# CLI input and error policy

LDGR Core follows the repository-wide
`docs/ldgr-design-principal-command-parsing.md` intent-preserving parsing
principle. The Core CLI is primarily an agent interface, so parsing and error
recovery are non-interactive. The human-driven installer is the only
interactive exception.

This document covers CLI parsing and diagnostics. Durable failures after an
operation is accepted follow the
[first-class error domain contract](first-class-errors.md).

## Accepted normalization

Core accepts deterministic syntax variants when they identify exactly one
canonical input:

- command and enumerated value case differences;
- declared aliases, including common plural top-level command names;
- snake_case in place of kebab-case for command and long-option names;
- common lifecycle synonyms declared by the relevant enum;
- surrounding whitespace on work identifiers and labels;
- empty or whitespace-only entries in comma-separated dependency lists;
- any non-empty priority label.

`P<number>` priorities remain canonicalized to uppercase. Other priority labels
are canonicalized to lowercase. The common labels `critical`, `urgent`,
`highest`, `high`, `medium`, `normal`, `low`, and `lowest` have stable queue
ordering; domain-specific labels are valid and sort after ranked labels.

Portable schedule imports apply the same work-status, hold-kind, priority,
label, and dependency normalization as direct CLI commands.

## Parse failures

Parse failures occur before command acceptance and are not first-class error
occurrences. Agents should correct a unique harmless mistake and retry once;
they should not create observations, error records, dispositions, or handoff
narratives for the rejected command.

Core never fuzzy-executes a correction. A unique typo suggestion fails with
exit code 2, shows help for the deepest valid command, and prints a complete
`Suggested rerun (not executed)` command that preserves the other input. An
ambiguous match lists candidates without selecting one. Adapter namespace
typos use the same non-interactive behavior.

For a unique, syntactically complete, non-destructive correction, Core saves
the corrected argv in the project-local `.ldgr/last-rerun.json` receipt and prints
`Use ldgr rerun to execute this command.` Running `ldgr rerun` explicitly
consumes that receipt before executing the canonical command. The receipt is
restored when execution returns an ordinary runtime error, so the agent may
retry after fixing the external cause. A successful rerun is one-shot.

Corrections to `work delete` and `adapter uninstall` are displayed but never
saved as rerun receipts. Those destructive operations must still be invoked by
their exact command names rather than through generic typo recovery.

## Errors that remain strict

Core continues to reject inputs when accepting them would lose information,
violate a lifecycle invariant, create ambiguity, or weaken a safety boundary.
These include:

- missing required content or references that do not resolve;
- ambiguous or unrelated command names;
- conflicting options and incomplete compound operations;
- invalid lifecycle transitions, dependency cycles, and active-run conflicts;
- unsafe archive, resource, artifact, or web paths;
- malformed or incompatible database, manifest, contract, release, signature,
  telemetry, and ingestion data;
- non-loopback web exposure without its explicit safety options;
- destructive operations that were not named by a canonical command or an
  explicit alias.

These errors protect durable state or execution safety rather than enforcing a
cosmetic input convention.
