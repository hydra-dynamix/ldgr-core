#!/usr/bin/env python3
"""Isolation tests for database contract and compatibility generation."""

from __future__ import annotations

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).with_name("generate-database-contract.py")
SPEC = importlib.util.spec_from_file_location("database_contract_generator", SCRIPT)
assert SPEC is not None and SPEC.loader is not None
GENERATOR = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(GENERATOR)


def schema(version: int, table: str) -> str:
    return f'''pub const CURRENT_SCHEMA_VERSION: i64 = {version};
const SQL: &str = r#"CREATE TABLE {table}(id INTEGER PRIMARY KEY);"#;
'''


class ContractGenerationIsolationTests(unittest.TestCase):
    def setUp(self) -> None:
        self.temp = tempfile.TemporaryDirectory()
        self.workspace = Path(self.temp.name)
        self.core = self.workspace / "ldgr-core"
        (self.core / "src/store").mkdir(parents=True)
        (self.core / "src/store/schema.rs").write_text(schema(5, "work"), encoding="utf-8")
        self.adapters = {}
        for adapter in ("code", "research"):
            root = self.workspace / f"ldgr-{adapter}"
            (root / "src").mkdir(parents=True)
            (root / "Cargo.toml").write_text("[package]\nname='fixture'\n", encoding="utf-8")
            (root / "adapter.toml").write_text(
                f'[adapter]\nslug = "{adapter}"\n', encoding="utf-8"
            )
            self.adapters[adapter] = root
        (self.adapters["research"] / "src/migrations.rs").write_text(
            schema(4, "research_program"), encoding="utf-8"
        )
        self.registry = {
            "format": GENERATOR.SOURCE_FORMAT,
            "core": {
                "core_capabilities": ["prompt.v1", "work.v1"],
                "supported_adapter_protocol_epochs": [1],
            },
            "central_components": [],
            "adapters": [
                {
                    "adapter": "code",
                    "adapter_protocol_epoch": 1,
                    "central_components": [],
                    "local_stores": [],
                    "minimum_core_schema": 5,
                    "required_core_capabilities": [],
                },
                {
                    "adapter": "research",
                    "adapter_protocol_epoch": 1,
                    "central_components": [],
                    "local_stores": [
                        {
                            "engine": "sqlite",
                            "migration_sources": ["ldgr-research/src/migrations.rs"],
                            "store_id": "research",
                        }
                    ],
                    "minimum_core_schema": 5,
                    "required_core_capabilities": ["work.v1"],
                },
            ],
        }

    def tearDown(self) -> None:
        self.temp.cleanup()

    def sidecar(self, adapter: str):
        return GENERATOR.build_adapter_sidecar(
            self.workspace,
            self.adapters[adapter],
            next(item for item in self.registry["adapters"] if item["adapter"] == adapter),
            {},
        )

    def test_local_migration_changes_only_owner_metadata_and_release_provenance(self) -> None:
        central_before = GENERATOR.build_central_contract(self.workspace, self.core, self.registry)
        release_before = GENERATOR.build_release_set(self.workspace, self.core)
        code_before = self.sidecar("code")
        research_before = self.sidecar("research")

        migration = self.adapters["research"] / "src/migrations.rs"
        migration.write_text(schema(5, "research_program_v2"), encoding="utf-8")

        central_after = GENERATOR.build_central_contract(self.workspace, self.core, self.registry)
        release_after = GENERATOR.build_release_set(self.workspace, self.core)
        code_after = self.sidecar("code")
        research_after = self.sidecar("research")

        self.assertEqual(central_before, central_after)
        self.assertEqual(code_before, code_after)
        self.assertEqual(research_before["compatibility"], research_after["compatibility"])
        self.assertNotEqual(research_before["local_stores"], research_after["local_stores"])
        self.assertNotEqual(release_before["contract_hash"], release_after["contract_hash"])

    def test_additive_core_schema_does_not_rewrite_adapter_requirements(self) -> None:
        code_before = self.sidecar("code")
        research_before = self.sidecar("research")
        central_before = GENERATOR.build_central_contract(self.workspace, self.core, self.registry)
        profile_before = GENERATOR.build_core_profile(
            self.workspace, self.core, self.registry, central_before
        )

        (self.core / "src/store/schema.rs").write_text(schema(6, "optional_addition"), encoding="utf-8")

        self.assertEqual(code_before, self.sidecar("code"))
        self.assertEqual(research_before, self.sidecar("research"))
        central_after = GENERATOR.build_central_contract(self.workspace, self.core, self.registry)
        profile_after = GENERATOR.build_core_profile(
            self.workspace, self.core, self.registry, central_after
        )
        self.assertNotEqual(central_before["contract_hash"], central_after["contract_hash"])
        self.assertEqual(code_before["compatibility"]["minimum_core_schema"], 5)
        self.assertEqual(profile_after["core_schema_version"], 6)
        self.assertEqual(
            profile_before["core_capabilities"], profile_after["core_capabilities"]
        )

    def test_only_explicit_core_owned_components_enter_central_contract(self) -> None:
        component_source = self.core / "src/store/notes.rs"
        component_source.write_text(schema(1, "adapter_notes_item"), encoding="utf-8")
        self.registry["central_components"] = [
            {
                "migration_sources": ["ldgr-core/src/store/notes.rs"],
                "namespace": "notes",
                "owner_adapter": "code",
                "schema_epoch": 1,
                "schema_version": 1,
            }
        ]
        contract = GENERATOR.build_central_contract(self.workspace, self.core, self.registry)
        self.assertEqual(
            [item["namespace"] for item in contract["components"]], ["core", "notes"]
        )


if __name__ == "__main__":
    unittest.main()
