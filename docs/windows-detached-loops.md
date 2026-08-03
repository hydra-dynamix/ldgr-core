# Detached loops on Windows

Long-running autonomous queues can be started without keeping a terminal or harness
pipe open:

```powershell
ldgr loop run `
  --prompt C:\Users\me\.ldgr\prompts\ldgr-core-loop.md `
  --agent agentctl `
  --until-empty `
  --detach
```

`--detach` starts a background `ldgr` process, prints its process ID and log paths,
then returns immediately. The child runs from the same working directory with the
same CLI arguments except `--detach`. Its stdout and stderr are written beside the
artifact directory under `.ldgr/logs/loop-detached-*.stdout.log` and
`.ldgr/logs/loop-detached-*.stderr.log` by default.

Monitor durable progress rather than the launcher process:

```powershell
ldgr status
ldgr context
```

On Windows, LDGR also supplies a missing `HOME` environment variable to loop child
processes from `USERPROFILE`. This is process-local compatibility for tools such as
`agentctl`; it does not persistently modify the user's environment.

## Upgrade and rollback

The PowerShell installer resolves the first user-owned `ldgr.exe` already on
`PATH` and updates that directory, including common Cargo installs under
`%USERPROFILE%\.cargo\bin`. The paired archive installs `agentctl.exe` first and
`ldgr.exe` second, prepends the directory to the user and current-process PATH,
then verifies both resolved paths and runs compatibility negotiation. Future
detached loops therefore relaunch the binaries that were actually updated.

Stop an already-running detached loop before updating if Windows reports that
its Core executable is locked. Rerun:

```powershell
irm https://ldgr.run/install.ps1 | iex
agentctl discover --json
```

Each replaced executable is retained once as `agentctl.exe.previous` or
`ldgr.exe.previous`. To roll back, stop detached loops, replace both current
executables with the two matching `.previous` files, and verify both versions.
Never roll back only one member of the pair.

Argument and prompt validation happens before the background process is started. A
completion request still requires an explicit audit command:

```powershell
ldgr loop run `
  --prompt .\prompts\loop.md `
  --agent agentctl `
  --until-empty `
  --project-complete-requested `
  --audit-argv '["my-auditor"]' `
  --detach
```
