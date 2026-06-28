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

Core help, status, and context include installed adapter profiles and commands.
