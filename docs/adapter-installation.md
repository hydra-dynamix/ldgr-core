# Adapter installation and discovery

LDGR adapter profiles are declarative bundles installed under the user LDGR home. A bundle contains `adapter.toml` plus relative files it references, such as prompts, templates, docs, scripts, skills, or extensions.

`ldgr-core` discovers installed bundles and extends the `ldgr` control surface from their manifests.

## User adapter root

Install adapter bundles under:

```text
~/.ldgr/<adapter>/adapter.toml
```

Example:

```text
~/.ldgr/code/adapter.toml
~/.ldgr/code/prompts/ldgr-loop-next-work.md
~/.ldgr/code/templates/task-spec.md
```

Discovery reads `LDGR_ADAPTER_PATH`, project `.ldgr` for explicit development overrides, `LDGR_HOME`, and `~/.ldgr`. Each adapter root should contain `<slug>/adapter.toml`, or a direct `adapter.toml` for single-bundle roots.

## Install

```bash
ldgr install adapter code
ldgr install adapter security
```

Open-source adapter installs currently call the adapter package's own installer and write the bundle to `~/.ldgr/<adapter>`. Adapter-owned skills/extensions are copied into configured harness locations when present.

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
