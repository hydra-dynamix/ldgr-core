#!/usr/bin/env python3
"""Generate separate central, release-set, and adapter compatibility contracts."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path
from typing import Any

try:
    import tomllib
except ModuleNotFoundError as error:  # pragma: no cover - Python < 3.11
    raise SystemExit("Python 3.11 or newer is required") from error


FORMAT = "ldgr.database-contract.v1"
ADAPTER_FORMAT = "ldgr.adapter-database-contract.v1"
ADAPTER_COMPATIBILITY_FORMAT = "ldgr.adapter-compatibility.v2"
CORE_COMPATIBILITY_FORMAT = "ldgr.core-compatibility.v2"
SOURCE_FORMAT = "ldgr.database-contract-sources.v2"
VERSION_PATTERNS = (
    re.compile(r"(?:pub\s+)?const\s+CURRENT_SCHEMA_VERSION\s*:\s*i64\s*=\s*(\d+)\s*;"),
    re.compile(r"(?:pub\s+)?const\s+VERSION\s*:\s*i64\s*=\s*(\d+)\s*;"),
)
IDENTIFIER = re.compile(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*")
CAPABILITY = re.compile(r"[a-z][a-z0-9]*(?:-[a-z0-9]+)*(?:\.[a-z][a-z0-9]*(?:-[a-z0-9]+)*)*\.v[1-9][0-9]*")
DIGEST = re.compile(r"sha256:[0-9a-f]{64}")


def parse_args() -> argparse.Namespace:
    core_root = Path(__file__).resolve().parents[1]
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--workspace-root",
        type=Path,
        default=core_root.parent,
        help="workspace containing Core and adapter directories",
    )
    parser.add_argument(
        "--core-root",
        type=Path,
        default=core_root,
        help="ldgr-core checkout receiving generated output",
    )
    parser.add_argument(
        "--sources",
        type=Path,
        help="reviewed v2 source registry (defaults under the Core schema directory)",
    )
    parser.add_argument(
        "--check",
        action="store_true",
        help="fail when committed output differs instead of writing it",
    )
    return parser.parse_args()


def source_version(path: Path) -> int:
    text = path.read_text(encoding="utf-8")
    for pattern in VERSION_PATTERNS:
        match = pattern.search(text)
        if match:
            version = int(match.group(1))
            if version <= 0:
                raise ValueError(f"schema version in {path} must be positive")
            return version
    raise ValueError(f"could not derive schema version from {path}")


def migration_sources(component_root: Path) -> list[Path]:
    sources = []
    for relative in ("src/migrations.rs", "src/schema.rs"):
        candidate = component_root / relative
        if candidate.is_file() and "CREATE TABLE" in candidate.read_text(encoding="utf-8"):
            sources.append(candidate)
    return sources


def migration_digest(workspace_root: Path, sources: list[Path]) -> str:
    digest = hashlib.sha256()
    for source in sorted(sources):
        relative = source.relative_to(workspace_root).as_posix().encode("utf-8")
        digest.update(len(relative).to_bytes(8, "big"))
        digest.update(relative)
        body = effective_migration_source(source)
        digest.update(len(body).to_bytes(8, "big"))
        digest.update(body)
    return f"sha256:{digest.hexdigest()}"


def effective_migration_source(path: Path) -> bytes:
    """Fingerprint schema/version SQL, excluding tests and implementation code."""
    text = path.read_text(encoding="utf-8").split("#[cfg(test)]", 1)[0]
    versions = []
    for pattern in VERSION_PATTERNS:
        versions.extend(match.group(0) for match in pattern.finditer(text))
    raw_strings = re.findall(r'r#+"(.*?)"#+', text, flags=re.DOTALL)
    sql_markers = (
        "CREATE TABLE",
        "CREATE INDEX",
        "CREATE TRIGGER",
        "ALTER TABLE",
        "DROP TABLE",
        "DROP INDEX",
        "DROP TRIGGER",
        "UPDATE schema_version",
    )
    sql = [
        re.sub(r"\s+", " ", fragment).strip()
        for fragment in raw_strings
        if any(marker in fragment for marker in sql_markers)
    ]
    canonical = json.dumps(
        {"versions": versions, "sql": sql}, sort_keys=True, separators=(",", ":")
    )
    return canonical.encode("utf-8")


def adapter_namespace(component_root: Path) -> str:
    manifest = component_root / "adapter.toml"
    if manifest.is_file():
        value = tomllib.loads(manifest.read_text(encoding="utf-8"))
        namespace = value.get("adapter", {}).get("slug")
        if isinstance(namespace, str) and namespace:
            return namespace.removeprefix("ldgr-")
    return component_root.name.removeprefix("ldgr-")


def discover_adapter_roots(workspace_root: Path) -> list[Path]:
    roots = []
    for path in workspace_root.iterdir():
        if not path.is_dir() or not path.name.startswith("ldgr-"):
            continue
        if path.name == "ldgr-core" or not (path / "Cargo.toml").is_file():
            continue
        if (path / "adapter.toml").is_file() or path.name == "ldgr-private-commercial":
            roots.append(path)
    return sorted(roots)


def exact_fields(value: Any, fields: set[str], subject: str) -> dict[str, Any]:
    if not isinstance(value, dict) or set(value) != fields:
        actual = sorted(value) if isinstance(value, dict) else type(value).__name__
        raise ValueError(f"{subject} fields differ: expected {sorted(fields)}, got {actual}")
    return value


def positive(value: Any, subject: str) -> int:
    if isinstance(value, bool) or not isinstance(value, int) or not 1 <= value <= 2_147_483_647:
        raise ValueError(f"{subject} must be a positive 32-bit integer")
    return value


def identifier(value: Any, subject: str) -> str:
    if not isinstance(value, str) or not IDENTIFIER.fullmatch(value) or value.startswith("ldgr-"):
        raise ValueError(f"{subject} is not a canonical identifier")
    return value


def sorted_unique(values: list[Any], subject: str, key=lambda value: value) -> None:
    if not isinstance(values, list) or values != sorted(values, key=key):
        raise ValueError(f"{subject} must be sorted")
    keys = [key(value) for value in values]
    if len(keys) != len(set(keys)):
        raise ValueError(f"{subject} must be unique")


def load_sources(path: Path) -> dict[str, Any]:
    value = json.loads(path.read_text(encoding="utf-8"))
    exact_fields(value, {"adapters", "central_components", "core", "format"}, "source registry")
    if value["format"] != SOURCE_FORMAT:
        raise ValueError(f"unsupported source registry format {value['format']!r}")
    if not isinstance(value["adapters"], list) or not isinstance(value["central_components"], list):
        raise ValueError("source registry adapters and central_components must be arrays")
    core = exact_fields(
        value["core"],
        {"core_capabilities", "supported_adapter_protocol_epochs"},
        "source core compatibility",
    )
    epochs = core["supported_adapter_protocol_epochs"]
    sorted_unique(epochs, "source supported adapter protocol epochs")
    if not epochs:
        raise ValueError("source supported adapter protocol epochs must not be empty")
    for epoch in epochs:
        positive(epoch, "source adapter protocol epoch")
    capabilities = core["core_capabilities"]
    sorted_unique(capabilities, "source Core capabilities")
    for capability in capabilities:
        if not isinstance(capability, str) or not CAPABILITY.fullmatch(capability):
            raise ValueError(f"invalid source Core capability {capability!r}")
    sorted_unique(value["adapters"], "source adapters", lambda item: item.get("adapter", ""))
    sorted_unique(
        value["central_components"],
        "source central components",
        lambda item: item.get("namespace", ""),
    )
    return value


def resolve_sources(workspace_root: Path, relative_sources: list[Any], subject: str) -> list[Path]:
    if not isinstance(relative_sources, list) or not relative_sources:
        raise ValueError(f"{subject} must contain at least one migration source")
    if relative_sources != sorted(relative_sources) or len(relative_sources) != len(set(relative_sources)):
        raise ValueError(f"{subject} must be sorted and unique")
    paths = []
    for relative in relative_sources:
        if not isinstance(relative, str) or Path(relative).is_absolute() or ".." in Path(relative).parts:
            raise ValueError(f"invalid migration source {relative!r} in {subject}")
        path = (workspace_root / relative).resolve()
        try:
            path.relative_to(workspace_root)
        except ValueError as error:
            raise ValueError(f"migration source escapes workspace: {relative}") from error
        if not path.is_file():
            raise ValueError(f"migration source does not exist: {relative}")
        paths.append(path)
    return paths


def component(namespace: str, core_version: int, workspace_root: Path, sources: list[Path], version: int) -> dict[str, Any]:
    return {
        "namespace": namespace,
        "schema_version": version,
        "minimum_core_schema": core_version,
        "migration_digest": migration_digest(workspace_root, sources),
        "migration_sources": [source.relative_to(workspace_root).as_posix() for source in sorted(sources)],
    }


def hash_contract(contract: dict[str, Any]) -> dict[str, Any]:
    canonical = json.dumps(contract, sort_keys=True, separators=(",", ":")).encode("utf-8")
    return {**contract, "contract_hash": f"sha256:{hashlib.sha256(canonical).hexdigest()}"}


def build_central_contract(workspace_root: Path, core_root: Path, registry: dict[str, Any]) -> dict[str, Any]:
    workspace_root = workspace_root.resolve()
    core_root = core_root.resolve()
    core_source = core_root / "src/store/schema.rs"
    core_version = source_version(core_source)
    components = [component("core", core_version, workspace_root, [core_source], core_version)]
    for index, raw in enumerate(registry["central_components"]):
        subject = f"central_components[{index}]"
        entry = exact_fields(
            raw,
            {"migration_sources", "namespace", "owner_adapter", "schema_epoch", "schema_version"},
            subject,
        )
        namespace = identifier(entry["namespace"], f"{subject}.namespace")
        identifier(entry["owner_adapter"], f"{subject}.owner_adapter")
        positive(entry["schema_epoch"], f"{subject}.schema_epoch")
        version = positive(entry["schema_version"], f"{subject}.schema_version")
        sources = resolve_sources(workspace_root, entry["migration_sources"], f"{subject}.migration_sources")
        for source in sources:
            try:
                source.relative_to(core_root)
            except ValueError as error:
                raise ValueError(f"central migration source must be Core-owned: {source}") from error
        components.append(component(namespace, core_version, workspace_root, sources, version))
    components.sort(key=lambda item: item["namespace"])
    return hash_contract({"format": FORMAT, "core_schema_version": core_version, "components": components})


def build_core_profile(
    workspace_root: Path,
    core_root: Path,
    registry: dict[str, Any],
    contract: dict[str, Any] | None = None,
) -> dict[str, Any]:
    """Build the signed Core protocol/capability/component inventory."""
    contract = contract or build_central_contract(workspace_root, core_root, registry)
    contract_by_namespace = {item["namespace"]: item for item in contract["components"]}
    central_components = []
    for source in registry["central_components"]:
        generated = contract_by_namespace[source["namespace"]]
        central_components.append(
            {
                "lineage": [
                    {
                        "migration_digest": generated["migration_digest"],
                        "schema_version": generated["schema_version"],
                    }
                ],
                "namespace": source["namespace"],
                "owner_adapter": source["owner_adapter"],
                "schema_epoch": source["schema_epoch"],
                "schema_version": generated["schema_version"],
            }
        )
    central_components.sort(key=lambda item: item["namespace"])
    return {
        "central_components": central_components,
        "core_capabilities": registry["core"]["core_capabilities"],
        "core_schema_version": contract["core_schema_version"],
        "format": CORE_COMPATIBILITY_FORMAT,
        "supported_adapter_protocol_epochs": registry["core"]["supported_adapter_protocol_epochs"],
    }


def build_release_set(workspace_root: Path, core_root: Path) -> dict[str, Any]:
    """Build the legacy global fingerprint retained for provenance and v1."""
    workspace_root = workspace_root.resolve()
    core_root = core_root.resolve()
    core_source = core_root / "src/store/schema.rs"
    core_version = source_version(core_source)
    components = [component("core", core_version, workspace_root, [core_source], core_version)]
    namespaces = {"core"}
    for adapter_root in discover_adapter_roots(workspace_root):
        namespace = adapter_namespace(adapter_root)
        identifier(namespace, "adapter namespace")
        if namespace in namespaces:
            raise ValueError(f"duplicate adapter schema namespace {namespace!r}")
        namespaces.add(namespace)
        sources = migration_sources(adapter_root)
        version = source_version(sources[0]) if sources else 1
        components.append(component(namespace, core_version, workspace_root, sources, version))
    components.sort(key=lambda item: item["namespace"])
    return hash_contract({"format": FORMAT, "core_schema_version": core_version, "components": components})


def build_contract(workspace_root: Path, core_root: Path, registry: dict[str, Any] | None = None) -> dict[str, Any]:
    """Compatibility alias for callers: build the central database contract."""
    core_root = core_root.resolve()
    if registry is None:
        registry = load_sources(core_root / "schema/database-contract-sources.json")
    return build_central_contract(workspace_root, core_root, registry)


def build_adapter_sidecar(
    workspace_root: Path,
    adapter_root: Path,
    source: dict[str, Any],
    central_by_namespace: dict[str, dict[str, Any]],
) -> dict[str, Any]:
    subject = f"adapter {source.get('adapter', '<unknown>')}"
    entry = exact_fields(
        source,
        {
            "adapter",
            "adapter_protocol_epoch",
            "central_components",
            "local_stores",
            "minimum_core_schema",
            "required_core_capabilities",
        },
        subject,
    )
    adapter = identifier(entry["adapter"], f"{subject}.adapter")
    if adapter != adapter_namespace(adapter_root):
        raise ValueError(f"source adapter {adapter} does not match {adapter_root}")
    epoch = positive(entry["adapter_protocol_epoch"], f"{subject}.adapter_protocol_epoch")
    minimum = positive(entry["minimum_core_schema"], f"{subject}.minimum_core_schema")
    capabilities = entry["required_core_capabilities"]
    sorted_unique(capabilities, f"{subject}.required_core_capabilities")
    for capability in capabilities:
        if not isinstance(capability, str) or not CAPABILITY.fullmatch(capability):
            raise ValueError(f"invalid Core capability {capability!r} for {adapter}")

    requirements = []
    sorted_unique(entry["central_components"], f"{subject}.central_components", lambda item: item.get("namespace", ""))
    for offset, raw_requirement in enumerate(entry["central_components"]):
        req_subject = f"{subject}.central_components[{offset}]"
        requirement = exact_fields(
            raw_requirement,
            {"accepted_lineage_digests", "minimum_schema_version", "namespace", "schema_epoch"},
            req_subject,
        )
        namespace = identifier(requirement["namespace"], f"{req_subject}.namespace")
        registration = central_by_namespace.get(namespace)
        if registration is None:
            raise ValueError(f"{adapter} requires unregistered central component {namespace}")
        if registration["owner_adapter"] != adapter:
            raise ValueError(f"{adapter} does not own central component {namespace}")
        positive(requirement["schema_epoch"], f"{req_subject}.schema_epoch")
        positive(requirement["minimum_schema_version"], f"{req_subject}.minimum_schema_version")
        digests = requirement["accepted_lineage_digests"]
        sorted_unique(digests, f"{req_subject}.accepted_lineage_digests")
        if not digests or any(not isinstance(value, str) or not DIGEST.fullmatch(value) for value in digests):
            raise ValueError(f"{req_subject}.accepted_lineage_digests contains an invalid digest")
        requirements.append(requirement)

    local_stores = []
    sorted_unique(entry["local_stores"], f"{subject}.local_stores", lambda item: item.get("store_id", ""))
    for offset, raw_store in enumerate(entry["local_stores"]):
        store_subject = f"{subject}.local_stores[{offset}]"
        store = exact_fields(raw_store, {"engine", "migration_sources", "store_id"}, store_subject)
        store_id = identifier(store["store_id"], f"{store_subject}.store_id")
        engine = identifier(store["engine"], f"{store_subject}.engine")
        sources = resolve_sources(workspace_root, store["migration_sources"], f"{store_subject}.migration_sources")
        for path in sources:
            try:
                path.relative_to(adapter_root.resolve())
            except ValueError as error:
                raise ValueError(f"local store source must be owned by {adapter}: {path}") from error
        local_stores.append(
            {
                "engine": engine,
                "migration_digest": migration_digest(workspace_root, sources),
                "schema_version": source_version(sources[0]),
                "store_id": store_id,
            }
        )

    return {
        "adapter": adapter,
        "compatibility": {
            "adapter_protocol_epoch": epoch,
            "central_components": requirements,
            "minimum_core_schema": minimum,
            "required_core_capabilities": capabilities,
        },
        "format": ADAPTER_COMPATIBILITY_FORMAT,
        "local_stores": local_stores,
    }


def json_output(contract: dict[str, Any]) -> str:
    return json.dumps(contract, indent=2, sort_keys=True) + "\n"


def canonical_json_output(value: dict[str, Any]) -> str:
    return json.dumps(value, sort_keys=True, separators=(",", ":"), ensure_ascii=False) + "\n"


def adapter_json_output(release_set: dict[str, Any], namespace: str) -> str:
    component_value = next(item for item in release_set["components"] if item["namespace"] == namespace)
    value = {
        "format": ADAPTER_FORMAT,
        "contract_hash": release_set["contract_hash"],
        "core_schema_version": release_set["core_schema_version"],
        "component": component_value,
    }
    return json_output(value)


def rust_output(contract: dict[str, Any], release_set: dict[str, Any]) -> str:
    rows = []
    for item in contract["components"]:
        rows.append(
            "    DatabaseComponentContract {\n"
            "        namespace: %s,\n"
            "        schema_version: %s,\n"
            "        minimum_core_schema: %s,\n"
            "        migration_digest: %s,\n"
            "    },"
            % (
                json.dumps(item["namespace"]),
                item["schema_version"],
                item["minimum_core_schema"],
                json.dumps(item["migration_digest"]),
            )
        )
    if len(rows) == 1:
        components_output = (
            "pub const GENERATED_DATABASE_COMPONENTS: &[DatabaseComponentContract] =\n"
            "    &[" + rows[0].removeprefix("    ").removesuffix(",") + "];"
        )
    else:
        components_output = (
            "pub const GENERATED_DATABASE_COMPONENTS: &[DatabaseComponentContract] = &[\n"
            + "\n".join(rows)
            + "\n];"
        )
    return """// @generated by scripts/generate-database-contract.py; do not edit.

pub const DATABASE_CONTRACT_FORMAT: &str = %s;
pub const ADAPTER_DATABASE_CONTRACT_FORMAT: &str = %s;
pub const DATABASE_CONTRACT_HASH: &str =
    %s;
pub const DATABASE_RELEASE_SET_HASH: &str =
    %s;
pub const GENERATED_CORE_SCHEMA_VERSION: i64 = %s;
pub const GENERATED_DATABASE_CONTRACT_JSON: &str = include_str!(\"../schema/database-contract.json\");
pub const GENERATED_DATABASE_RELEASE_SET_JSON: &str =
    include_str!(\"../schema/database-release-set.json\");
pub const GENERATED_CORE_COMPATIBILITY_JSON: &str =
    include_str!(\"../schema/core-compatibility.json\");

%s
""" % (
        json.dumps(contract["format"]),
        json.dumps(ADAPTER_FORMAT),
        json.dumps(contract["contract_hash"]),
        json.dumps(release_set["contract_hash"]),
        contract["core_schema_version"],
        components_output,
    )


def update(path: Path, content: str, check: bool) -> bool:
    existing = path.read_text(encoding="utf-8") if path.is_file() else None
    if existing == content:
        return False
    if check:
        print(f"stale generated database contract: {path}", file=sys.stderr)
        return True
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(content, encoding="utf-8")
    print(f"generated {path}")
    return False


def main() -> int:
    args = parse_args()
    workspace_root = args.workspace_root.resolve()
    core_root = args.core_root.resolve()
    source_path = args.sources.resolve() if args.sources else core_root / "schema/database-contract-sources.json"
    registry = load_sources(source_path)
    adapter_roots = {adapter_namespace(root): root for root in discover_adapter_roots(workspace_root)}
    configured = {entry["adapter"] for entry in registry["adapters"]}
    if configured != set(adapter_roots):
        raise ValueError(
            "source registry adapters differ from workspace adapters: "
            f"configured={sorted(configured)} discovered={sorted(adapter_roots)}"
        )

    contract = build_central_contract(workspace_root, core_root, registry)
    release_set = build_release_set(workspace_root, core_root)
    core_profile = build_core_profile(workspace_root, core_root, registry, contract)
    stale = update(core_root / "schema/database-contract.json", json_output(contract), args.check)
    stale |= update(core_root / "schema/database-release-set.json", json_output(release_set), args.check)
    stale |= update(
        core_root / "schema/core-compatibility.json",
        canonical_json_output(core_profile),
        args.check,
    )
    stale |= update(
        core_root / "src/generated_database_contract.rs",
        rust_output(contract, release_set),
        args.check,
    )

    central_by_namespace = {entry["namespace"]: entry for entry in registry["central_components"]}
    for source in registry["adapters"]:
        namespace = source["adapter"]
        adapter_root = adapter_roots[namespace]
        sidecar = build_adapter_sidecar(workspace_root, adapter_root, source, central_by_namespace)
        stale |= update(
            adapter_root / "adapter-compatibility.json",
            canonical_json_output(sidecar),
            args.check,
        )
        # Keep generating the exact v1 global contract during the bounded reader
        # migration window. V2 never evaluates this release-set hash.
        stale |= update(
            adapter_root / "adapter-database-contract.json",
            adapter_json_output(release_set, namespace),
            args.check,
        )
    return 1 if stale else 0


if __name__ == "__main__":
    try:
        raise SystemExit(main())
    except (KeyError, OSError, UnicodeError, json.JSONDecodeError, ValueError) as error:
        raise SystemExit(f"database contract generation failed: {error}") from error
