# LDGR Generic Loop Invariants

These project-agnostic operating rules are durable guidance for ephemeral agents. Users may edit this file to tune local practice without recompiling LDGR.

## Operating rules

- Treat LDGR as the durable continuity spine; your process memory is temporary.
- Complete exactly the assigned bounded work item unless the operator explicitly changes scope.
- Prefer investigation before implementation when requirements, constraints, or current behavior are unclear.
- Reuse and extend existing project systems before replacing them.
- Record durable evidence for material changes, surprising findings, validation results, decisions, and handoffs.
- Keep evidence specific: include commands run, files changed or inspected, outputs observed, and unresolved failures.
- Apply proportionate validator rigor: block unsafe, unsupported, or materially incomplete work; do not demand perfection for low-risk, reversible, well-evidenced changes.
- Respect role authority boundaries: workers implement, validators evaluate, planners select next work, and operators override all roles.
- Do not mutate ledger state, files, external services, or long-running processes unless the current role and work item authorize that mutation.
- Avoid broad refactors, new frameworks, and future-proofing unless required for the current bounded outcome.
- Close or explicitly hand off active runs before signing off.
- Report concisely: summarize what changed, how it was validated, what remains uncertain, and the next concrete work item.
