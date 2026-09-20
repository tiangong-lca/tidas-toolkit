#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
workflow="$root/.github/workflows/dispatch-tidas-sdk-sync.yml"

for path in \
  assets/tidas/methodologies/elementary_flow_taxonomy_extension.v1.json
do
  count="$(grep -Fxc "      - \"$path\"" "$workflow")"
  if [[ "$count" != 1 ]]; then
    echo "error: expected one exact SDK dispatch path for $path, found $count" >&2
    exit 1
  fi
done

if grep -Fq 'runtime_rulesets' "$workflow"; then
  echo "error: retired mixed runtime projection must not trigger SDK refresh" >&2
  exit 1
fi

if grep -Eq 'assets/tidas/(schemas|schemas_zh|methodologies)/\*\*' "$workflow"; then
  echo "error: broad public-spec or methodology dispatch path is forbidden" >&2
  exit 1
fi
if ! grep -Fq 'packages_json=["typescript"]' "$workflow"; then
  echo "error: SDK dispatch workflow must select only the TypeScript package" >&2
  exit 1
fi

echo "SDK dispatch path contract passed"
