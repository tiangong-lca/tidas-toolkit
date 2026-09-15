#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
mode="${1:-check}"

if [[ "$mode" != "check" && "$mode" != "update" ]]; then
  echo "usage: scripts/sync-rust-package-assets.sh [check|update]" >&2
  exit 64
fi

contract_copies=(
  "operation-report.v1.schema.json:crates/tidas-contracts/contracts"
  "conversion-report.v1.schema.json:crates/tidas-conversion/contracts"
  "flow-property-conversion-request.v1.schema.json:crates/tidas-measurement/contracts"
  "flow-property-conversion-report.v1.schema.json:crates/tidas-measurement/contracts"
  "export-report.v1.schema.json:crates/tidas-export/contracts"
  "import-execution-report.v1.schema.json:crates/tidas-import/contracts"
  "import-package-report.v1.schema.json:crates/tidas-import/contracts"
  "reference-extraction-result.v1.schema.json:crates/tidas-references/contracts"
  "release-report.v1.schema.json:crates/tidas-release/contracts"
  "methodology-validation-report.v1.schema.json:crates/tidas-rulesets/contracts"
  "ruleset-description.v1.schema.json:crates/tidas-rulesets/contracts"
  "spool-summary.v1.schema.json:crates/tidas-runtime/contracts"
  "document-validation-manifest-item.v1.schema.json:crates/tidas-validation/contracts"
  "validation-describe.v1.schema.json:crates/tidas-validation/contracts"
  "validation-final-event.v1.schema.json:crates/tidas-validation/contracts"
  "validation-issue-event.v1.schema.json:crates/tidas-validation/contracts"
  "validation-summary.v1.schema.json:crates/tidas-validation/contracts"
)

check_file() {
  local source="$1"
  local target="$2"
  if [[ ! -f "$target" ]] || ! cmp -s "$source" "$target"; then
    echo "packaged copy is stale: ${target#"$repo_root/"}" >&2
    return 1
  fi
}

sync_file() {
  local source="$1"
  local target="$2"
  mkdir -p "$(dirname "$target")"
  cp "$source" "$target"
}

for mapping in "${contract_copies[@]}"; do
  source_name="${mapping%%:*}"
  target_dir="${mapping#*:}"
  source="$repo_root/contracts/$source_name"
  target="$repo_root/$target_dir/$source_name"
  if [[ "$mode" == "update" ]]; then
    sync_file "$source" "$target"
  else
    check_file "$source" "$target"
  fi
done

if [[ "$mode" == "update" ]]; then
  echo "updated self-contained Rust package contracts"
else
  echo "self-contained Rust package contracts are current"
fi
