#!/usr/bin/env bash
set -euo pipefail
root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
helper="$root/scripts/native-cache-contract.sh"
baseline=9e593bb18ea69cc5095e012465dcd675a822ed0d
inputs=aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa
key() { bash "$helper" key "$@"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
original="$(key Linux-X64 ubuntu24-20260914 linux x64-linux "$baseline" "$inputs")"
[[ "$original" == "$(key Linux-X64 ubuntu24-20260914 linux x64-linux "$baseline" "$inputs")" ]] || fail 'unstable identity'
for changed in \
  "$(key Linux-X64 ubuntu24-20260915 linux x64-linux "$baseline" "$inputs")" \
  "$(key Linux-ARM64 ubuntu24-20260914 linux arm64-linux "$baseline" "$inputs")" \
  "$(key Linux-X64 ubuntu24-20260914 linux x64-linux "${baseline%?}f" "$inputs")" \
  "$(key Linux-X64 ubuntu24-20260914 linux x64-linux "$baseline" "${inputs%?}b")"; do
  [[ "$changed" != "$original" ]] || fail 'changed input reused a cache identity'
done
for missing in '' unknown 'line
break'; do
  if key Linux-X64 "$missing" linux x64-linux "$baseline" "$inputs" >/dev/null 2>&1; then fail 'invalid image accepted'; fi
done
if key Linux-X64 ubuntu24-20260914 linux x64-windows-static "$baseline" "$inputs" >/dev/null 2>&1; then fail 'foreign triplet accepted'; fi
if key Windows-X64 windows2025-20260914 linux x64-linux "$baseline" "$inputs" >/dev/null 2>&1; then fail 'foreign runner family accepted'; fi
key macOS-ARM64 macos15-20260914 macos arm64-osx "$baseline" "$inputs" >/dev/null
key Windows-X64 windows2025-20260914 windows x64-windows-static "$baseline" "$inputs" >/dev/null
if key Linux-X64 ubuntu24-20260914 linux x64-linux latest "$inputs" >/dev/null 2>&1; then fail 'floating baseline accepted'; fi
if key Linux-X64 ubuntu24-20260914 linux x64-linux "$baseline" '' >/dev/null 2>&1; then fail 'missing implementation/manifest hash accepted'; fi
if key Linux-X64 ubuntu24-20260914 linux x64-linux "$baseline" "$inputs" extra >/dev/null 2>&1; then fail 'unexpected argument accepted'; fi
bash "$helper" verify linux success skipped
bash "$helper" verify macos success skipped
bash "$helper" verify windows skipped success
for bad in skipped failure cancelled ''; do
  if bash "$helper" verify linux "$bad" skipped >/dev/null 2>&1; then fail 'Unix installation was not required'; fi
  if bash "$helper" verify windows skipped "$bad" >/dev/null 2>&1; then fail 'Windows installation was not required'; fi
done
if bash "$helper" verify windows success success >/dev/null 2>&1; then fail 'wrong platform installation accepted'; fi
if bash "$helper" verify other success skipped >/dev/null 2>&1; then fail 'unsupported family accepted'; fi

# Execute the action's Unix setup, with no network or privileged commands. The
# pinned vcpkg tool tests presence (including empty/0), not the variable's value.
fixture="$(mktemp -d)"
trap 'rm -rf "$fixture"' EXIT
mkdir -p "$fixture/bin"
awk '
  /^      id: unix$/ { unix_step = 1; next }
  unix_step && /^      run: \|$/ { body = 1; next }
  body && /^        / { print substr($0, 9); next }
  body { exit }
' "$root/.github/actions/native-xml/action.yml" > "$fixture/unix-step.sh"
[[ -s "$fixture/unix-step.sh" ]] || fail 'Unix native action was not extracted'
cat > "$fixture/bin/git" <<'GIT'
#!/usr/bin/env bash
set -euo pipefail
case "$1" in
  clone)
    [[ "${@: -1}" == "$RUNNER_TEMP/vcpkg" ]]
    mkdir -p "$RUNNER_TEMP/vcpkg"
    cp "$NATIVE_TEST_FIXTURE/bootstrap" "$RUNNER_TEMP/vcpkg/bootstrap-vcpkg.sh"
    cp "$NATIVE_TEST_FIXTURE/vcpkg" "$RUNNER_TEMP/vcpkg/vcpkg"
    ;;
  -C) printf '%s\n' "$@" > "$RUNNER_TEMP/checkout-args" ;;
  *) exit 99 ;;
esac
GIT
cat > "$fixture/bootstrap" <<'BOOTSTRAP'
#!/usr/bin/env bash
set -euo pipefail
[[ "$#" == 1 && "$1" == -disableMetrics ]]
BOOTSTRAP
cat > "$fixture/vcpkg" <<'VCPKG'
#!/usr/bin/env bash
set -euo pipefail
if [[ "${VCPKG_FORCE_SYSTEM_BINARIES+x}" == x ]]; then
  printf 'Inherited forced-system selection remains present: <%s>\n' "$VCPKG_FORCE_SYSTEM_BINARIES" >&2
  exit 86
fi
printf '%s\n' "$@" > "$RUNNER_TEMP/install-args"
VCPKG
for command in sudo brew; do
  printf '#!/usr/bin/env bash\nexit 0\n' > "$fixture/bin/$command"
done
chmod +x "$fixture/bin/"* "$fixture/bootstrap" "$fixture/vcpkg"
run_unix_fixture() (
  local family="$1" inherited="$2" block="$3" expected="$4"
  export RUNNER_TEMP="$fixture/$family-$inherited-$expected"
  export NATIVE_TEST_FIXTURE="$fixture" GITHUB_WORKSPACE="$fixture/workspace"
  export GITHUB_ENV="$RUNNER_TEMP/github-env" NATIVE_FAMILY="$family"
  export VCPKG_COMMIT="$baseline" VCPKG_DEFAULT_BINARY_CACHE="$RUNNER_TEMP/cache"
  export TRIPLET=arm64-linux PATH="$fixture/bin:$PATH"
  [[ "$family" != macos ]] || export TRIPLET=arm64-osx
  mkdir -p "$RUNNER_TEMP"
  if [[ "$inherited" == absent ]]; then
    unset VCPKG_FORCE_SYSTEM_BINARIES
  else
    export VCPKG_FORCE_SYSTEM_BINARIES="$inherited"
  fi
  local status=0
  bash -e -o pipefail "$block" > "$RUNNER_TEMP/stdout" 2> "$RUNNER_TEMP/stderr" || status="$?"
  if [[ "$inherited" == absent ]]; then
    [[ "${VCPKG_FORCE_SYSTEM_BINARIES+x}" != x ]] || fail 'child changed parent presence'
  else
    [[ "${VCPKG_FORCE_SYSTEM_BINARIES+x}" == x && "$VCPKG_FORCE_SYSTEM_BINARIES" == "$inherited" ]] || fail 'child changed parent value'
  fi
  if [[ "$expected" == rejected ]]; then
    [[ "$status" == 86 ]] || fail 'forced-system control did not reach the presence check'
    printf 'Native Unix control %s inherited <%s>: rejected; parent unchanged.\n' "$family" "$inherited"
    return
  fi
  [[ "$status" == 0 ]] || { cat "$RUNNER_TEMP/stderr" >&2; fail "inherited <$inherited> blocked $family setup"; }
  printf '%s\n' install --triplet "$TRIPLET" \
    "--x-manifest-root=$GITHUB_WORKSPACE/packaging/vcpkg" \
    "--x-install-root=$RUNNER_TEMP/vcpkg/installed" > "$RUNNER_TEMP/expected-install-args"
  cmp "$RUNNER_TEMP/expected-install-args" "$RUNNER_TEMP/install-args" || fail 'native install arguments changed'
  printf '%s\n' -C "$RUNNER_TEMP/vcpkg" checkout "$baseline" > "$RUNNER_TEMP/expected-checkout-args"
  cmp "$RUNNER_TEMP/expected-checkout-args" "$RUNNER_TEMP/checkout-args" || fail 'pinned checkout changed'
  printf '%s\n' "PKG_CONFIG_PATH=$RUNNER_TEMP/vcpkg/installed/$TRIPLET/lib/pkgconfig" \
    LIBXML2_STATIC=1 LIBXSLT_STATIC=1 > "$RUNNER_TEMP/expected-env"
  cmp "$RUNNER_TEMP/expected-env" "$GITHUB_ENV" || fail 'static XML environment changed'
  printf 'Native Unix setup %s inherited <%s>: passed; parent unchanged.\n' "$family" "$inherited"
)
for family in linux macos; do
  for inherited in absent '' 0 1; do
    run_unix_fixture "$family" "$inherited" "$fixture/unix-step.sh" accepted
  done
done
# Removing the correction must reproduce the inherited-presence failure. This
# control runs the same action block and command shims as the passing cases.
sed '/^[[:space:]]*unset VCPKG_FORCE_SYSTEM_BINARIES[[:space:]]*$/d' \
  "$fixture/unix-step.sh" > "$fixture/forced-system-control.sh"
for inherited in '' 0 1; do
  run_unix_fixture linux "$inherited" "$fixture/forced-system-control.sh" rejected
done

rust_ci_matches() {
  local path="$1" pattern
  while IFS= read -r pattern; do
    # The workflow's exact paths and terminal /** patterns are intentional globs.
    # shellcheck disable=SC2254
    case "$path" in $pattern) return 0 ;; esac
  done < <(sed -n '/^  pull_request:/,/^  workflow_dispatch:/s/^      - //p' "$root/.github/workflows/rust-ci.yml")
  return 1
}
for path in Cargo.toml Cargo.lock crates/example/src/lib.rs contracts/example.json \
  assets/example.json migration/example.json .github/workflows/rust-ci.yml \
  scripts/audit-rust-only.sh .githooks/pre-push scripts/pre-push-deletion-only.sh \
  scripts/pre-push-maintenance-only.sh scripts/test-pre-push.sh \
  packaging/vcpkg/vcpkg.json .github/actions/native-xml/action.yml \
  .github/actions/native-xml/create-release-triplet.ps1 \
  .github/actions/native-xml/test-release-triplet.ps1 \
  .github/workflows/rust-release.yml scripts/native-cache-contract.sh scripts/test-native-cache.sh; do
  rust_ci_matches "$path" || fail "Rust CI omits required input: $path"
done
for path in README.md docs/unrelated.md packaging/unrelated.txt .github/workflows/unrelated.yml; do
  if rust_ci_matches "$path"; then fail "Rust CI unexpectedly selects unrelated input: $path"; fi
done
printf 'Native child environment and Rust CI input regressions passed.\n'
printf 'Native cache identity and required-installation regressions passed.\n'
