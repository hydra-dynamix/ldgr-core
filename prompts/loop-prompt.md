# Ldgr Autonomous Loop

{{job_complete_policy}}

{{completion_audit_instruction}}

You are running one bounded Ldgr loop session.

Complete exactly the assigned work item stated at the top of this prompt, record observations/artifacts/decisions as appropriate, and queue follow-up work if needed. The Ldgr status below is the compact control surface; the full context is for deeper situational awareness. Do not pick a different work item from either section.

If you need to correct core state, use the core control surfaces: `ldgr work edit` for title/description fixes, `ldgr work status set` for lifecycle control, `ldgr notice edit` for notice corrections, and `ldgr artifact show` to inspect artifact records. Run `<command> --help` instead of guessing command shapes.

## Ldgr status

```json
{{ldgr_status}}
```

## Full Ldgr context

```json
{{ldgr_context}}
```
