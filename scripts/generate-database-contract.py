#!/usr/bin/env python3
"""Generate LDGR's database contract from Core and adapter schema sources."""

from __future__ import annotations

import argparse
import hashlib
import json
import re
import sys
from pathlib import Path

try:
    import tomllib
except ModuleNotFoundError as error:  # pragma: no cover - Python < 3.11
    raise SystemExit("Python 3.11 or newer is required") from error


FORMAT = "ldgr.database-contract.v1"
ADAPTER_FORMAT = "ldgr.adapter-database-contract.v1"
EMPTY_SHA256 = hashlib.sha256(b"").hexdigest()
VERSION_PATTERNS = (
    re.compile(r"(?:pub\s+)?const\s+CURRENT_SCHEMA_VERSION\s*:\s*i64\s*=\s*(\d+)\s*;"),
    re.compile(r"(?:pub\s+)?const\s+VERSION\s*:\s*i64\s*=\s*(\d+)\s*;"),
)


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
        body = source.read_bytes()
        digest.update(len(body).to_bytes(8, "big"))
        digest.update(body)
    return f"sha256:{digest.hexdigest()}"


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


def build_contract(workspace_root: Path, core_root: Path) -> dict[str, object]:
    workspace_root = workspace_root.resolve()
    core_root = core_root.resolve()
    core_source = core_root / "src/store/schema.rs"
    core_version = source_version(core_source)
    core_relative = core_source.relative_to(workspace_root).as_posix()
    components: list[dict[str, object]] = [
        {
            "namespace": "core",
            "schema_version": core_version,
            "minimum_core_schema": core_version,
            "migration_digest": migration_digest(workspace_root, [core_source]),
            "migration_sources": [core_relative],
        }
    ]

    namespaces = {"core"}
    for adapter_root in discover_adapter_roots(workspace_root):
        namespace = adapter_namespace(adapter_root)
        if not re.fullmatch(r"[a-z][a-z0-9-]*", namespace):
            raise ValueError(f"invalid adapter schema namespace {namespace!r}")
        if namespace in namespaces:
            raise ValueError(f"duplicate adapter schema namespace {namespace!r}")
        namespaces.add(namespace)
        sources = migration_sources(adapter_root)
        version = source_version(sources[0]) if sources else 1
        components.append(
            {
                "namespace": namespace,
                "schema_version": version,
                "minimum_core_schema": core_version,
                "migration_digest": migration_digest(workspace_root, sources),
                "migration_sources": [
                    source.relative_to(workspace_root).as_posix() for source in sorted(sources)
                ],
            }
        )

    components.sort(key=lambda component: str(component["namespace"]))
    contract: dict[str, object] = {
        "format": FORMAT,
        "core_schema_version": core_version,
        "components": components,
    }
    canonical = json.dumps(contract, sort_keys=True, separators=(",", ":")).encode("utf-8")
    contract["contract_hash"] = f"sha256:{hashlib.sha256(canonical).hexdigest()}"
    return contract


def json_output(contract: dict[str, object]) -> str:
    return json.dumps(contract, indent=2, sort_keys=True) + "\n"


def adapter_json_output(contract: dict[str, object], namespace: str) -> str:
    components = contract["components"]
    assert isinstance(components, list)
    component = next(
        component
        for component in components
        if isinstance(component, dict) and component["namespace"] == namespace
    )
    value = {
        "format": ADAPTER_FORMAT,
        "contract_hash": contract["contract_hash"],
        "core_schema_version": contract["core_schema_version"],
        "component": component,
    }
    return json.dumps(value, indent=2, sort_keys=True) + "\n"


def rust_output(contract: dict[str, object]) -> str:
    components = contract["components"]
    assert isinstance(components, list)
    rows = []
    for component in components:
        assert isinstance(component, dict)
        rows.append(
            "    DatabaseComponentContract { namespace: %s, schema_version: %s, "
            "minimum_core_schema: %s, migration_digest: %s },"
            % (
                json.dumps(component["namespace"]),
                component["schema_version"],
                component["minimum_core_schema"],
                json.dumps(component["migration_digest"]),
            )
        )
    return """// @generated by scripts/generate-database-contract.py; do not edit.

pub const DATABASE_CONTRACT_FORMAT: &str = %s;
pub const ADAPTER_DATABASE_CONTRACT_FORMAT: &str = %s;
pub const DATABASE_CONTRACT_HASH: &str = %s;
pub const GENERATED_CORE_SCHEMA_VERSION: i64 = %s;
pub const GENERATED_DATABASE_CONTRACT_JSON: &str = include_str!(\"../schema/database-contract.json\");

pub const GENERATED_DATABASE_COMPONENTS: &[DatabaseComponentContract] = &[
%s
];
""" % (
        json.dumps(contract["format"]),
        json.dumps(ADAPTER_FORMAT),
        json.dumps(contract["contract_hash"]),
        contract["core_schema_version"],
        "\n".join(rows),
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
    contract = build_contract(args.workspace_root, args.core_root)
    stale = update(
        args.core_root / "schema/database-contract.json", json_output(contract), args.check
    )
    stale |= update(
        args.core_root / "src/generated_database_contract.rs",
        rust_output(contract),
        args.check,
    )
    for adapter_root in discover_adapter_roots(args.workspace_root.resolve()):
        stale |= update(
            adapter_root / "adapter-database-contract.json",
            adapter_json_output(contract, adapter_namespace(adapter_root)),
            args.check,
        )
    return 1 if stale else 0


if __name__ == "__main__":
    raise SystemExit(main())
