# Adapter installation and discovery

LDGR adapter profiles are declarative bundles for `ldgr-research` or adapter-owned tooling. A bundle contains `adapter.toml` plus any relative files it references, such as loop prompts and templates. The research layer or adapter-owned wrapper resolves those paths relative to the manifest directory.

Current boundary: `ldgr-core` does not own adapter discovery or profile application commands. It only provides narrow reusable helpers such as manifest digest verification; discovery and profile application are implemented by `ldgr-research` and adapter-owned tools.

## User adapter root

Install adapter bundles under:

```text
~/.ldgr/adapters/<slug>/adapter.toml
```

Example:

```text
~/.ldgr/adapters/code/adapter.toml
~/.ldgr/adapters/code/prompts/ldgr-loop-next-work.md
~/.ldgr/adapters/code/templates/task-spec.md
```

`ldgr-research` also reads `LDGR_HOME/adapters` when `LDGR_HOME` is set. For development and tests, set `LDGR_ADAPTER_PATH` to one or more adapter roots separated by the platform path separator. Each root should contain `<slug>/adapter.toml`.

## Discover and apply

```bash
ldgr-research profile discover          # list installed manifests and exact apply commands
ldgr-research profile apply code        # auto-load and apply ~/.ldgr/adapters/code/adapter.toml
```

Explicit manifest paths still work:

```bash
ldgr-research profile create ./adapter.toml
ldgr-research profile apply code
```

## Aliases

Adapters may declare optional aliases for friendlier application names:

```toml
[adapter]
slug = "code"
title = "Coding adapter"
aliases = ["coding", "ldgr-code"]
```

`ldgr-research profile apply` accepts exact slugs, aliases, case-insensitive normalized
matches, and unambiguous title words from discovered manifests. The canonical
stored profile slug remains the manifest `slug`.

## Binary-backed adapters

Adapter tools should invoke binaries by command name, not by source-checkout paths:

```toml
[[tools]]
name = "code-check-all"
argv = ["ldgr-code", "check", "all"]
```

Install the executable through the adapter's normal package channel, for example:

```bash
cargo install ldgr-code
```

Then install/copy the bundle to `~/.ldgr/adapters/code`. A source checkout is not required after the adapter bundle is installed and the binary is on `PATH`.

Adapter templates and domain-specific examples live with the research/adapter
package that owns those records, not in the core `ldgr` crate.
