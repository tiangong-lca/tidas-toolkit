#!/usr/bin/env python3

"""Classify changed tidas-tools paths that still belong in SDK refreshes."""

from __future__ import annotations

import json
import sys


TOOLS_OWNED_PATHS = frozenset(
    {
        "assets/tidas/methodologies/runtime_rulesets.json",
        "assets/tidas/methodologies/runtime_rulesets.schema.json",
        "assets/tidas/methodologies/elementary_flow_taxonomy_extension.v1.json",
    }
)


def classify(paths: list[str]) -> dict:
    changed = [path.strip() for path in paths if path.strip()]
    tools_paths = [path for path in changed if path in TOOLS_OWNED_PATHS]
    if not tools_paths:
        return {
            "any_changed": False,
            "packages": [],
            "packages_json": "[]",
            "typescript_bump": "patch",
            "python_bump": "patch",
            "reason": "",
            "tools_paths": [],
        }
    return {
        "any_changed": True,
        "packages": ["typescript"],
        "packages_json": '["typescript"]',
        "typescript_bump": "patch",
        "python_bump": "patch",
        "reason": "tools-owned runtime asset update",
        "tools_paths": tools_paths,
    }


def main() -> int:
    result = classify(sys.stdin.read().splitlines())
    json.dump(result, sys.stdout, sort_keys=True)
    sys.stdout.write("\n")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
