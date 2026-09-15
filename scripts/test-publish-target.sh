#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/repo/scripts" "$fixture/bin"
cp "$root/scripts/publish-crates.sh" "$fixture/repo/scripts/"
printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/repo/scripts/sync-rust-package-assets.sh"
chmod +x "$fixture/repo/scripts/"*.sh
export TEST_CARGO_LOG="$fixture/cargo.log"
export TEST_REPO="$fixture/repo"
export TEST_TARGET="$fixture/isolated target with spaces"
export PATH="$fixture/bin:$PATH"
cat > "$fixture/bin/cargo" <<'CARGO'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >> "$TEST_CARGO_LOG"
packages=(tidas-contracts tidas-runtime tidas-xml tidas-references tidas-assets tidas-measurement tidas-conversion tidas-rulesets tidas-validation tidas-export tidas-import tidas-release tidas)
case "$1" in
  metadata)
    printf '%s\n' "${packages[@]}" | jq -Rn --arg target "${TEST_TARGET:-}" '
      {target_directory:$target,packages:[inputs|{name:.,version:"0.3.0",publish:["crates-io"],dependencies:
        (if env.TEST_BAD_DOMAIN_EDGE == "measurement" and . == "tidas-measurement" then [{name:"tidas-assets",kind:null}]
         elif env.TEST_BAD_DOMAIN_EDGE == "transitive" and . == "tidas-validation" then [{name:"tidas-rulesets",kind:null}]
         elif env.TEST_BAD_DOMAIN_EDGE == "transitive" and . == "tidas-rulesets" then [{name:"tidas-conversion",kind:null}]
         elif . == "tidas-conversion" then [{name:"tidas-validation",kind:"dev"}]
         else [] end)}]}'
    ;;
  package)
    mkdir -p "$TEST_TARGET/package"
    for package in "${packages[@]}"; do
      [[ "$package" == "${TEST_OMIT:-}" ]] || printf 'fixture crate\n' > "$TEST_TARGET/package/$package-0.3.0.crate"
    done
    ;;
  publish)
    [[ " $* " == *' --dry-run '* ]] || { echo 'Unexpected actual publication' >&2; exit 99; }
    ;;
  *) exit 98 ;;
esac
CARGO
chmod +x "$fixture/bin/cargo"
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
# A configured output location is authoritative even when it differs from env defaults.
bash "$TEST_REPO/scripts/publish-crates.sh" check > "$fixture/custom.log" 2>&1 || { cat "$fixture/custom.log"; fail 'configured Cargo target was ignored'; }
[[ "$(grep -c '^metadata ' "$TEST_CARGO_LOG")" == 1 ]] || fail 'extra metadata query'
[[ "$(grep -c '^qualified ' "$fixture/custom.log")" == 13 ]] || fail 'incomplete package qualification'
TEST_TARGET="$TEST_REPO/target" bash "$TEST_REPO/scripts/publish-crates.sh" check > "$fixture/default.log" 2>&1 || fail 'default target regressed'
# A stale default tree must not satisfy a missing archive in the configured target.
rm "$TEST_TARGET/package/tidas-contracts-0.3.0.crate"
if TEST_OMIT=tidas-contracts bash "$TEST_REPO/scripts/publish-crates.sh" check > "$fixture/missing.log" 2>&1; then fail 'stale default archive accepted'; fi
if TEST_TARGET='' bash "$TEST_REPO/scripts/publish-crates.sh" check > "$fixture/empty.log" 2>&1; then fail 'missing target metadata accepted'; fi
for edge in measurement transitive; do
  if TEST_BAD_DOMAIN_EDGE="$edge" bash "$TEST_REPO/scripts/publish-crates.sh" check > "$fixture/edge-$edge.log" 2>&1; then fail "invalid $edge domain dependency accepted"; fi
  grep -q 'domain dependency violation' "$fixture/edge-$edge.log" || fail "wrong failure for $edge dependency"
done
printf 'Cargo target-directory qualification regressions passed.\n'
