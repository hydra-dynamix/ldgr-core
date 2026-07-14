# Adapter installation and discovery

LDGR adapter profiles are declarative bundles installed under the user LDGR home. A bundle contains `adapter.toml` plus relative files it references, such as prompts, templates, docs, scripts, skills, or extensions.

`ldgr-core` discovers installed bundles and extends the `ldgr` control surface from their manifests.

## User adapter root

Install adapter bundles under:

```text
~/.ldgr/adapters/<adapter>/adapter.toml
```

Example:

```text
~/.ldgr/adapters/code/adapter.toml
~/.ldgr/adapters/code/prompts/ldgr-loop-next-work.md
~/.ldgr/adapters/code/templates/task-spec.md
```

Discovery reads `LDGR_ADAPTER_PATH`, project `.ldgr/adapters` for explicit development overrides, `LDGR_HOME/adapters`, and `~/.ldgr/adapters`. Each adapter root should contain `<slug>/adapter.toml`, or a direct `adapter.toml` for single-bundle roots.

## Install

```bash
ldgr adapter install list
ldgr adapter install conduct
ldgr adapter install research
```

`ldgr adapter install list` shows installable adapters and where they come from. Core installation acquires or runs the adapter binary, then delegates setup to the adapter-owned installer. The adapter writes its bundle to `~/.ldgr/adapters/<adapter>`, installs adapter-owned prompts, skills, commands, and extensions into the paths declared by configured harness entries in `~/.ldgr/config.json`, and records license paths there when the adapter supports commercial licensing.

The older `ldgr install adapter <slug>` path remains a compatibility alias for source-checkout installs.

## Dynamic command surface

Adapters declare namespaces in `adapter.toml`:

```toml
[[commands]]
namespace = "code"
argv = ["ldgr-code"]
aliases = ["coding"]

[commands.help]
usage = "ldgr code <command> [options]"
summary = "Run coding adapter workflows from the LDGR control surface."
```

After install, core dispatches through the namespace:

```bash
ldgr code --help
ldgr code check all
```

Core lifecycle commands keep precedence over adapter namespaces. If a top-level
token is not a built-in command, LDGR matches it against installed namespace
names and aliases, executes the declared `argv`, and appends the remaining user
arguments exactly.

The adapter process inherits stdout and stderr. A nonzero adapter exit status is
returned by `ldgr`, and failure to start the adapter process is reported with the
adapter slug, namespace, and command. LDGR also exports the selected core context
through environment variables:

```text
LDGR_DB
LDGR_ARTIFACT_ROOT
LDGR_WORKING_DIR
LDGR_ADAPTER_SLUG
LDGR_ADAPTER_NAMESPACE
```

Core help, status, and context include installed adapter profiles and commands.

## Numerical transition integration

Adapters that participate in opt-in numerical sequence collection use the
Core-owned interface described in
[Adapter numerical transition contract](adapter-telemetry.md). Adapters do not
own consent, buffering, serialization, or transmission.
