#!/usr/bin/env bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
cd "$repo_root"

mode="${1:-check}"
if [[ "$mode" != "check" && "$mode" != "publish" ]]; then
  echo "usage: scripts/publish-crates.sh [check|publish]" >&2
  exit 64
fi
if [[ "$mode" == "publish" && -z "${CARGO_REGISTRY_TOKEN:-}" ]]; then
  echo "CARGO_REGISTRY_TOKEN is required for publication" >&2
  exit 1
fi

# This order is part of the release contract. Every registry dependency is
# visible before the dependent package is uploaded.
public_packages=(
  tidas-contracts
  tidas-runtime
  tidas-xml
  tidas-references
  tidas-assets
  tidas-measurement
  tidas-conversion
  tidas-rulesets
  tidas-validation
  tidas-export
  tidas-import
  tidas-release
  tidas
)

run_cargo() {
  if [[ "${TIDAS_ALLOW_DIRTY:-0}" == "1" ]]; then
    cargo "$@" --allow-dirty
  else
    cargo "$@"
  fi
}

metadata="$(cargo metadata --locked --format-version 1 --no-deps)"
target_directory="$(jq -er '.target_directory | strings | select(length > 0)' <<<"$metadata")"
version="$(jq -r '.packages[] | select(.name == "tidas") | .version' <<<"$metadata")"
if [[ -z "$version" || "$version" == "null" ]]; then
  echo "could not resolve the public tidas package version" >&2
  exit 1
fi

expected_packages="$(printf '%s\n' "${public_packages[@]}" | sort)"
actual_packages="$(
  jq -r '.packages[] | select((.publish | length) > 0) | .name' \
    <<<"$metadata" | sort
)"
if [[ "$actual_packages" != "$expected_packages" ]]; then
  echo "public package set does not match the ordered release contract" >&2
  diff -u <(printf '%s\n' "$expected_packages") <(printf '%s\n' "$actual_packages") || true
  exit 1
fi

# Keep shared quantity semantics below both package conversion and validation.
# Check normal/build edges transitively; dev-only package integration tests are allowed.
if ! jq -e '
  .packages as $packages |
  def dependencies($name):
    [$packages[] | select(.name == $name) | .dependencies[]? |
      select(.kind != "dev") | .name];
  def reachable($pending; $seen):
    if ($pending | length) == 0 then $seen
    else $pending[0] as $next |
      if ($seen | index($next)) != null then reachable($pending[1:]; $seen)
      else reachable($pending[1:] + dependencies($next); $seen + [$next]) end
    end;
  ((reachable(["tidas-validation"]; []) | index("tidas-conversion")) == null) and
  ((reachable(["tidas-conversion"]; []) | index("tidas-validation")) == null) and
  (all(dependencies("tidas-measurement")[]; startswith("tidas") | not))
' <<<"$metadata" >/dev/null; then
  echo "domain dependency violation: measurement must be independent and conversion/validation must not depend on one another" >&2
  exit 1
fi

./scripts/sync-rust-package-assets.sh check

crate_file() {
  local package="$1"
  printf '%s/package/%s-%s.crate' "$target_directory" "$package" "$version"
}

sha256_file() {
  shasum -a 256 "$1" | awk '{print $1}'
}

workspace_selection=(--workspace --exclude tidas-dist)
if [[ "$mode" == "publish" ]]; then
  # The tag publish job depends on the separate package qualification job and
  # the four-platform build matrix, so it reuses the exact archives without
  # rebuilding native XML dependencies.
  run_cargo package \
    "${workspace_selection[@]}" \
    --locked \
    --no-verify
else
  run_cargo package \
    "${workspace_selection[@]}" \
    --locked
fi
run_cargo publish \
  "${workspace_selection[@]}" \
  --locked \
  --dry-run \
  --no-verify

for package in "${public_packages[@]}"; do
  echo "checking crates.io package: $package@$version"
  archive="$(crate_file "$package")"
  if [[ ! -f "$archive" ]]; then
    echo "cargo did not produce ${archive#"$repo_root/"}" >&2
    exit 1
  fi
  archive_bytes="$(wc -c <"$archive" | tr -d ' ')"
  if (( archive_bytes > 10000000 )); then
    echo "$package package is $archive_bytes bytes; crates.io limit is 10 MB" >&2
    exit 1
  fi
  echo "qualified $package@$version ($(sha256_file "$archive"), $archive_bytes bytes)"
done

if [[ "$mode" == "check" ]]; then
  echo "all public crates are self-contained and ready for crates.io"
  exit 0
fi

index_url="${TIDAS_CRATES_INDEX_URL:-https://index.crates.io}"

index_path() {
  local package="$1"
  local length="${#package}"
  if (( length == 1 )); then
    printf '1/%s' "$package"
  elif (( length == 2 )); then
    printf '2/%s' "$package"
  elif (( length == 3 )); then
    printf '3/%s/%s' "${package:0:1}" "$package"
  else
    printf '%s/%s/%s' "${package:0:2}" "${package:2:2}" "$package"
  fi
}

registry_checksum() {
  local package="$1"
  local requested_version="$2"
  local response
  local status
  response="$(mktemp)"
  status="$(
    curl \
      --silent \
      --show-error \
      --retry 3 \
      --header 'Cache-Control: no-cache' \
      --output "$response" \
      --write-out '%{http_code}' \
      "$index_url/$(index_path "$package")"
  )" || {
    rm -f "$response"
    echo "failed to query crates.io index for $package" >&2
    return 1
  }

  case "$status" in
    200)
      jq -r --arg version "$requested_version" \
        'select(.vers == $version) | .cksum' "$response" | tail -n 1
      ;;
    404)
      ;;
    *)
      rm -f "$response"
      echo "crates.io index returned HTTP $status for $package" >&2
      return 1
      ;;
  esac
  rm -f "$response"
}

wait_for_checksum() {
  local package="$1"
  local expected="$2"
  local observed
  local attempt
  for attempt in $(seq 1 60); do
    observed="$(registry_checksum "$package" "$version")"
    if [[ "$observed" == "$expected" ]]; then
      return 0
    fi
    if [[ -n "$observed" && "$observed" != "$expected" ]]; then
      echo "published checksum drift for $package@$version: expected $expected, found $observed" >&2
      return 1
    fi
    sleep 10
  done
  echo "timed out waiting for $package@$version to appear in the crates.io index" >&2
  return 1
}

missing_packages=()
missing_checksums=()
publish_selection=()

for package in "${public_packages[@]}"; do
  archive="$(crate_file "$package")"
  local_checksum="$(sha256_file "$archive")"
  remote_checksum="$(registry_checksum "$package" "$version")"

  if [[ -n "$remote_checksum" ]]; then
    if [[ "$remote_checksum" != "$local_checksum" ]]; then
      echo "refusing to skip $package@$version: local checksum $local_checksum differs from crates.io $remote_checksum" >&2
      exit 1
    fi
    echo "already published with matching bytes: $package@$version"
    continue
  fi

  missing_packages+=("$package")
  missing_checksums+=("$local_checksum")
  publish_selection+=(--package "$package")
done

if (( ${#missing_packages[@]} == 0 )); then
  echo "the complete tidas $version crates.io release set is already published"
  exit 0
fi

printf 'publishing %s\n' "${missing_packages[@]}"
run_cargo publish \
  "${publish_selection[@]}" \
  --locked \
  --no-verify

for index in "${!missing_packages[@]}"; do
  wait_for_checksum "${missing_packages[$index]}" "${missing_checksums[$index]}"
done

echo "published the complete tidas $version crates.io release set"
