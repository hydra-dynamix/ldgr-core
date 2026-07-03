# Adapter installation and discovery

LDGR adapter profiles are declarative bundles installed under the user LDGR home. A bundle contains `adapter.toml` plus relative files it references, such as prompts, templates, docs, scripts, skills, or extensions.

`ldgr-core` discovers installed bundles and extends the `ldgr` control surface from their manifests. Core public APIs cover manifest parsing/diagnostics, discovery, namespace dispatch, bundle materialization, and loop-prompt application. Adapter binaries own installer side effects, skills/extensions, workflow commands, validators, and any commercial entitlement checks.

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

Discovery reads `LDGR_ADAPTER_PATH`, project `.ldgr` for explicit development overrides, `LDGR_HOME/adapters`, `LDGR_HOME`, `~/.ldgr/adapters`, and `~/.ldgr`. Each adapter root should contain `<slug>/adapter.toml`, or a direct `adapter.toml` for single-bundle roots.

## Install

```bash
ldgr adapter install list
ldgr adapter install conduct
ldgr adapter install research
```

`ldgr adapter install list` shows installable adapters and where they come from. It keeps open-source/source adapters separate from commercial binary adapters. Core installation acquires or runs the adapter binary, then delegates setup to the adapter-owned installer. The adapter writes its bundle to `~/.ldgr/<adapter>`, installs adapter-owned skills/extensions into configured harness locations, and records license paths in `~/.ldgr/config.json` when the adapter supports commercial licensing.

### Release-backed commercial adapter catalog

Commercial adapter binaries are cataloged by LDGR Core as release assets from `ldgr-releases`. Release assets are produced from the adapter's authoritative source checkout first; `ldgr-releases` is the public binary/metadata catalog, not the commercial source repository. The source checkout authority table lives in `docs/adapter-release-source-layout.md`, and the operator release handoff checklist lives in `docs/adapter-release-checklist.md`:

```text
https://github.com/hydra-dynamix/ldgr-releases
```

The install-list contract is deliberately metadata-only: Core may name a commercial adapter, its title, install command, binary source, and the expected release artifact lookup pattern, but product-specific entitlement policy stays in the adapter or private commercial support crates.

For a commercial adapter `<adapter>` on `<platform>`, Core resolves the GitHub release catalog and selects the newest matching release asset:

| Field | Expected value |
| --- | --- |
| GitHub repository | `hydra-dynamix/ldgr-releases` |
| Release tag | `<adapter>-v<version>` |
| Archive asset | `<adapter>-<version>-<platform>.tar.gz` |
| Checksum asset | `<adapter>-<version>-<platform>.tar.gz.sha256` |
| Metadata asset | `<adapter>-<version>-<platform>.release.json` |
| Archive root | `<adapter>-<version>/` |
| Adapter binary | `<adapter>-<version>/<platform>/ldgr-<adapter>` |
| Platform tag | `linux-x86_64`, `linux-aarch64`, `macos-x86_64`, `macos-aarch64`, or `windows-x86_64`/`windows-aarch64` |

Core no longer assumes adapter releases share `ldgr-core`'s version. It resolves the release catalog by adapter/platform, downloads candidate `.release.json` metadata, and selects only a release whose `adapter_version_family`, `adapter_version`, and `ldgr_core_api_min`/`ldgr_core_api_max_exclusive` fields are compatible with the running `ldgr-core` API. Operators can pin an exact adapter release with `ldgr adapter install <adapter> --adapter-version <version>`. Core then verifies the downloaded archive against the `.sha256` asset and cross-checks `.release.json` adapter/platform/artifact/sha256 metadata. Core copies the platform binary to `~/.local/bin/ldgr-<adapter>`, runs `ldgr-<adapter> adapter install --install-root ~/.ldgr/<adapter> --print-path`, then patches the installed manifest argv to the installed binary path. The adapter-owned installer writes `adapter.toml`, installs adapter bundle files, and performs any license validation/config recording. Commercial adapters may fall back only to an already installed `ldgr-<adapter>` binary or an explicit `--source-root` supplied by the operator.

### Open-source git adapter catalog

Open-source adapters install from git when the operator is not installing from a local source checkout with `--source-root`. Core runs `cargo install --git <repo> --locked --force --root ~/.local <package>`, runs the installed adapter binary's `adapter install` command, then patches the installed manifest argv to the absolute `~/.local/bin/ldgr-<adapter>` path for temp-HOME and non-interactive installs. Current open-source git sources are:

| Adapter | Git repository | Package/binary |
| --- | --- | --- |
| `research` | `https://github.com/hydra-dynamix/ldgr-research` | `ldgr-research` |
| `example` | `https://github.com/hydra-dynamix/ldgr-example-adapter` | `ldgr-example-adapter` |
| `programbench` | `https://github.com/hydra-dynamix/ldgr-programbench` | `ldgr-programbench` |

If the ProgramBench repository is reachable but not yet published as an installable Cargo package, Core reports that clear not-yet-released state instead of presenting it as a generic adapter setup failure.

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

## Public author APIs

Adapter authors should depend on the public `ldgr-core` modules rather than copying private adapter wrapper logic:

| Task | Public API |
| --- | --- |
| Load/parse a manifest with structured diagnostics | `ldgr_core::adapter_manifest::load_adapter_manifest`, `parse_adapter_manifest_text`, `parse_adapter_manifest` |
| Validate public manifest semantics | `validate_adapter_manifest`, `validate_adapter_manifest_semantics`, `AdapterManifestDiagnostic`, `AdapterManifestDiagnosticCode` |
| Discover installed adapters | `ldgr_core::adapter_registry::AdapterRegistry::{discover, discover_from_roots}`, `AdapterDiscoveryEnvironment`, `adapter_search_roots`, `adapter_manifest_paths` |
| Materialize a public bundle | `ldgr_core::adapter_bundle::materialize_adapter_bundle` |
| Apply a manifest loop prompt through the core prompt lifecycle | `ldgr_core::adapter_profile::{apply_adapter_profile_prompt, AdapterProfileApplyOptions}` |
| Parse shared adapter installer flags / pass through to core | `ldgr_core::adapter_command::{default_adapter_root, parse_adapter_install_command, pass_through_ldgr_or_exit}` |
| Verify generic manifest integrity or signed claims | `ldgr_core::manifest_integrity::*`, `ldgr_core::claims::*` |

These APIs intentionally stop at the public adapter boundary. They do not install adapter-owned skills or extensions, create private pack state, decide which validators run, implement readiness policy, or enforce commercial entitlements.

The executable public contract suite is `ldgr-core/tests/adapter_public_contract.rs`.
