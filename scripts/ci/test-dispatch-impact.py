#!/usr/bin/env python3

from __future__ import annotations

import importlib.util
import json
import unittest
from pathlib import Path


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location("detect_sdk_impact", ROOT / "scripts/ci/detect-sdk-impact.py")
assert SPEC and SPEC.loader
MODULE = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(MODULE)


class DispatchImpactTests(unittest.TestCase):
    def test_tools_only_dispatches_typescript(self) -> None:
        result = MODULE.classify(["assets/tidas/methodologies/runtime_rulesets.json"])
        self.assertTrue(result["any_changed"])
        self.assertEqual(result["packages"], ["typescript"])
        self.assertEqual(json.loads(result["packages_json"]), ["typescript"])

    def test_spec_owned_only_does_not_dispatch(self) -> None:
        result = MODULE.classify([
            "assets/tidas/schemas/tidas_processes.json",
            "assets/tidas/schemas_zh/tidas_processes.json",
            "assets/tidas/methodologies/tidas_flows.yaml",
            "assets/tidas/methodologies/tidas_processes.yaml",
        ])
        self.assertFalse(result["any_changed"])
        self.assertEqual(result["packages"], [])

    def test_mixed_change_dispatches_only_tools_paths(self) -> None:
        result = MODULE.classify([
            "assets/tidas/schemas/tidas_processes.json",
            "assets/tidas/methodologies/runtime_rulesets.schema.json",
        ])
        self.assertTrue(result["any_changed"])
        self.assertEqual(result["tools_paths"], ["assets/tidas/methodologies/runtime_rulesets.schema.json"])
        self.assertEqual(result["packages"], ["typescript"])

    def test_unrelated_change_does_not_dispatch(self) -> None:
        self.assertFalse(MODULE.classify(["README.md", "crates/tidas-runtime/src/lib.rs"])["any_changed"])


class WorkflowContractTests(unittest.TestCase):
    def test_workflow_filters_only_tools_owned_paths(self) -> None:
        raw = (ROOT / ".github/workflows/dispatch-tidas-sdk-sync.yml").read_text(encoding="utf-8")
        for path in MODULE.TOOLS_OWNED_PATHS:
            self.assertIn(f'"{path}"', raw)
        self.assertNotIn('"assets/tidas/schemas/**"', raw)
        self.assertNotIn('"assets/tidas/schemas_zh/**"', raw)
        self.assertNotIn('"assets/tidas/methodologies/**"', raw)
        self.assertIn("detect-sdk-impact.py", raw)


if __name__ == "__main__":
    unittest.main()
