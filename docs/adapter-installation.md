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

`ldgr adapter install list` shows installable adapters and where they come from. Core installation acquires or runs the adapter binary, then delegates setup to the adapter-owned installer. The adapter writes its bundle to `~/.ldgr/adapters/<adapter>`, installs adapter-owned prompts, skills, commands, and extensions into the paths declared by configured harness entries in canonical `~/.ldgr/config.toml`, and records license paths there when the adapter supports commercial licensing. Core also maintains `~/.ldgr/config.json` as a compatibility mirror for older adapters during migration.

For local development, `--source-root` accepts either the LDGR monorepo checkout
containing an adapter crate or the adapter crate root itself. Core selects the
adapter crate's own Cargo manifest, so nested standalone workspaces do not need to
be members of the parent Cargo workspace:

```bash
git clone --recurse-submodules https://github.com/hydra-dynamix/ldgr <checkout>
ldgr adapter install example --source-root <checkout>

# Selecting the nested adapter crate directly is equivalent:
ldgr adapter install example --source-root <checkout>/ldgr-example-adapter
```

The older `ldgr install adapter <slug>` path remains a compatibility alias for source-checkout installs.

### Local source receipts and lifecycle

A source install writes `installation-receipt.json` beside the installed
`adapter.toml`. This is a `local_source` receipt, deliberately distinct from a
signed-release receipt. It records:

- the canonical source bundle and Cargo manifest, package name, and a source
  bundle digest that excludes build caches;
- source Cargo/adapter-manifest digests, optional source/installed
  `adapter-resources.json` digests, and the digest of the installed,
  Cargo-runner-patched adapter manifest;
- every installed bundle file and every copied harness resource with its
  SHA-256 digest;
- the installer argv and the executable argv recorded for each adapter
  namespace and tool; and
- the exact install root, marker, generated `source-target` cache, and
  configured harness roots Core owns. The source checkout is explicitly
  outside that ownership boundary.

The receipt always reports `verified_release: false`. A local checkout is never
described as signed, verified release provenance.

Lifecycle commands use the receipt as follows:

```bash
ldgr adapter update example --check
ldgr adapter update example
ldgr adapter reconcile example
ldgr adapter uninstall example
```

`update --check` validates the installed ownership boundary and reports whether
the recorded source bundle changed; it does not consult the release index.
`update` reruns the recorded source installer and refreshes the receipt.
`--prerelease` is rejected for source installs. Reinstall refuses to replace a
signed release or any untracked/modified installation.

`reconcile` copies current installed bundle resources only into configured
harness roots and updates their receipt digests. It refuses modified resources
and unowned destination collisions. `uninstall` removes only the tracked
install root, marker, and harness resources; it preserves the source checkout.
Any modified or untracked owned content blocks removal unless `--force` is
given. Even with `--force`, receipt paths outside the current/default harness
boundaries are rejected rather than deleted.

The `source-target` directory is a generated Cargo cache owned by the
installation. It may change during normal dispatch and is excluded from drift
checks; uninstall removes it with the tracked install root.

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

### Windows source-adapter toolchains

A `--source-root` install records a `cargo run` command instead of requiring the
adapter binary on `PATH`. Before LDGR executes that command on Windows, it probes
the resolved `cargo.exe` with `cargo --version` and verifies the Cargo/Rustup
home combination rather than assuming that `HOME` owns the Rust toolchain.

LDGR preserves a working inherited configuration first. If that probe fails, it
checks the executable resolved from `PATH`, an absolute `CARGO_HOME`, and the
documented Windows defaults under `USERPROFILE\.cargo` and `HOME\.cargo`. For a
Rustup proxy it checks an absolute `RUSTUP_HOME`, `rustup show home`, and existing
`USERPROFILE\.rustup` or `HOME\.rustup` defaults. `HOME` and `USERPROFILE` may
therefore point to different directories, as they often do under a harness.
Standalone, non-Rustup Cargo installations remain valid.

Set `CARGO_HOME` and `RUSTUP_HOME` to absolute paths when overriding discovery.
Relative explicit homes are rejected rather than being resolved against an
incidental working directory. If no configuration works, or more than one
fallback works, LDGR stops before adapter spawn and records a redacted
`ldgr.adapter.source-runtime` infrastructure error. Its fallback list identifies
the environment variables or platform-default origins to choose without storing
the user-profile paths.

Core help, status, and context include installed adapter profiles and commands.

## Numerical transition integration

Adapters that participate in opt-in numerical sequence collection use the
Core-owned interface described in
[Adapter numerical transition contract](adapter-telemetry.md). Adapters do not
own consent, buffering, serialization, or transmission.
