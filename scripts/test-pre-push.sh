#!/bin/sh
# Focused regression for the pre-push deletion-only classification.
# Uses an isolated temporary Git repository and stub gate commands; it never
# contacts a remote, never builds Rust, and never reads .env or production data.
set -eu

# Isolate the entire test process, including hook invocations, from its caller.
# The source hook still uses Git's real bindings; only this fixture harness clears them.
for git_local_name in $(git rev-parse --local-env-vars); do
  unset "$git_local_name"
done

root="$(CDPATH= cd -- "$(dirname "$0")/.." && pwd)"
hook_src="$root/.githooks/pre-push"
helper_src="$root/scripts/pre-push-deletion-only.sh"

fixture="$(mktemp -d)"
cleanup() { rm -rf "$fixture"; }
trap cleanup EXIT HUP INT TERM

repo="$fixture/repo"
mkdir -p "$repo/.githooks" "$repo/scripts" "$fixture/bin"
cp "$hook_src" "$repo/.githooks/pre-push"
cp "$helper_src" "$repo/scripts/pre-push-deletion-only.sh"
chmod +x "$repo/.githooks/pre-push" "$repo/scripts/pre-push-deletion-only.sh"

trace="$fixture/trace"

# Ordered trace of every stub invocation, including full argv.
cat > "$repo/scripts/docpact-gate.sh" <<'STUB'
#!/bin/sh
{
  printf 'docpact-gate|%s' "$#"
  for argument do printf '|%s' "$argument"; done
  printf '\n'
} >> "$FIXTURE_TRACE"
exit "${FIXTURE_DOCPACT_EXIT:-0}"
STUB
cat > "$repo/scripts/audit-rust-only.sh" <<'STUB'
#!/bin/sh
{
  printf 'audit-rust-only|%s' "$#"
  for argument do printf '|%s' "$argument"; done
  printf '\n'
} >> "$FIXTURE_TRACE"
exit "${FIXTURE_AUDIT_EXIT:-0}"
STUB
cat > "$fixture/bin/cargo" <<'STUB'
#!/bin/sh
case "$1" in
  run) step=asset-lock ;;
  fmt) step=fmt ;;
  clippy) step=clippy ;;
  test) step=test ;;
  *) step="unknown:$1" ;;
esac
{
  printf '%s|%s' "$step" "$#"
  for argument do printf '|%s' "$argument"; done
  printf '\n'
} >> "$FIXTURE_TRACE"
if [ "${FIXTURE_FAIL_STEP:-}" = "$step" ]; then exit "${FIXTURE_FAIL_EXIT:-7}"; fi
exit "${FIXTURE_CARGO_EXIT:-0}"
STUB
chmod +x "$repo/scripts/docpact-gate.sh" "$repo/scripts/audit-rust-only.sh" "$fixture/bin/cargo"

(
  cd "$repo"
  git init -q .
  git config user.email fixture@example.invalid
  git config user.name Fixture
  git config core.hooksPath .githooks
  git config commit.gpgsign false
  printf 'fixture\n' > tracked.txt
  git add tracked.txt
  git commit -qm fixture
) >/dev/null 2>&1

sha1a=1111111111111111111111111111111111111111
sha1b=2222222222222222222222222222222222222222
zero40=0000000000000000000000000000000000000000
sha256a=$(printf 'ab%.0s' $(seq 1 32))
sha256b=$(printf 'cd%.0s' $(seq 1 32))
zero64=$(printf '00%.0s' $(seq 1 32))

failures=0
pass() { printf 'ok   - %s\n' "$1"; }
fail() { printf 'FAIL - %s: %s\n' "$1" "$2"; failures=$((failures+1)); }

# Remote name and a remote path containing a space exercise argument fidelity.
remote_name="origin"
remote_path="$fixture/remote with space.git"

run_hook() { # $1 = stdin file, $2 = extra env assignment (optional)
  : > "$trace"
  set +e
  (
    cd "$repo"
    env FIXTURE_TRACE="$trace" PATH="$fixture/bin:$PATH" ${2:-} \
        sh "$repo/.githooks/pre-push" "$remote_name" "$remote_path" < "$1"
  ) >"$fixture/out" 2>&1
  hook_rc=$?
  set -e
}

expected_gate_trace() {
  printf 'docpact-gate|2|%s|%s\n' "$remote_name" "$remote_path"
  printf 'audit-rust-only|0\n'
  printf 'asset-lock|8|run|--locked|-p|tidas-assets|--bin|tidas-asset-lock|--|check\n'
  printf 'fmt|3|fmt|--all|--check\n'
  printf 'clippy|7|clippy|--locked|--workspace|--all-targets|--|-D|warnings\n'
  printf 'test|4|test|--locked|--workspace|--all-targets\n'
}

# The full ordered gate trace must match exactly: order, uniqueness and argv.
expect_gate() {
  name="$1"; input="$2"; want_rc="${3:-0}"
  run_hook "$input" "${4:-}"
  if [ "$hook_rc" -ne "$want_rc" ]; then fail "$name" "exit $hook_rc, wanted $want_rc"; return; fi
  expected_gate_trace > "$fixture/expected"
  if ! diff -u "$fixture/expected" "$trace" > "$fixture/trace.diff" 2>&1; then
    fail "$name" "ordered gate trace differs: $(tr '\n' ';' < "$fixture/trace.diff" | cut -c1-200)"; return
  fi
  pass "$name"
}

expect_fast_path() {
  name="$1"; input="$2"
  run_hook "$input" "${3:-}"
  if [ "$hook_rc" -ne 0 ]; then fail "$name" "exit $hook_rc"; return; fi
  if [ -s "$trace" ]; then fail "$name" "gate ran: $(tr '\n' ' ' < "$trace")"; return; fi
  pass "$name"
}

write() { printf '%b' "$2" > "$fixture/$1"; }

# --- canonical wire inputs (classification) --------------------------------
write pure-delete "(delete) $zero40 refs/heads/old-branch $sha1a\n"
write multi-delete "(delete) $zero40 refs/heads/one $sha1a\n(delete) $zero40 refs/heads/two $sha1b\n"
write sha256-delete "(delete) $zero64 refs/heads/old-branch $sha256a\n"
write code-update "refs/heads/main $sha1a refs/heads/main $sha1b\n"
write branch-creation "refs/heads/new $sha1a refs/heads/new $zero40\n"
write tag-push "refs/tags/v1.0.0 $sha1a refs/tags/v1.0.0 $zero40\n"
write tag-delete "(delete) $zero40 refs/tags/v1.0.0 $sha1a\n"
write mixed "(delete) $zero40 refs/heads/gone $sha1a\nrefs/heads/main $sha1a refs/heads/main $sha1b\n"
write delete-last "refs/heads/main $sha1a refs/heads/main $sha1b\n(delete) $zero40 refs/heads/gone $sha1b\n"
write empty ""
write blank-line "\n"
write malformed "(delete) $zero40\n"
write truncated "(delete) $zero40 refs/heads/old-branch\n"
write five-fields "(delete) $zero40 refs/heads/one $sha1a extra\n"
write short-sha "(delete) 0000 refs/heads/one $sha1a\n"
write unknown-sha-length "(delete) $(printf '0%.0s' $(seq 1 50)) refs/heads/one $sha1a\n"
write zero-remote "(delete) $zero40 refs/heads/one $zero40\n"
write invalid-ref "(delete) $zero40 refs/heads/bad name $sha1a\n"
write delete-marker-nonzero-local "(delete) $sha1a refs/heads/one $sha1b\n"
write non-branch-ref "(delete) $zero40 refs/notes/x $sha1a\n"
write no-trailing-newline "(delete) $zero40 refs/heads/old-branch $sha1a"
write crlf "(delete) $zero40 refs/heads/old-branch $sha1a\r\n"
write nul "(delete) $zero40 refs/heads/old-branch $sha1a\000\n"
write ref-dotdot "(delete) $zero40 refs/heads/../evil $sha1a\n"
write width-mismatch "(delete) $zero40 refs/heads/one $sha256a\n"
write non-hex-remote "(delete) $zero40 refs/heads/one zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\n"
write sha256-non-hex-remote "(delete) $zero64 refs/heads/one $(printf 'zz%.0s' $(seq 1 32))\n"

# The polluted invocation covers fixture creation and both hook branches; the
# full wire-format matrix runs once in the parent, not redundantly twice.
if [ "${1:-}" = '--polluted-child' ]; then
  expect_fast_path "polluted caller: deletion" "$fixture/pure-delete"
  expect_gate "polluted caller: code publication" "$fixture/code-update"
  [ "$failures" -eq 0 ]
  exit
fi

expect_fast_path "pure branch deletion skips the gate"        "$fixture/pure-delete"
expect_fast_path "multiple branch deletions skip the gate"    "$fixture/multi-delete"
expect_fast_path "SHA256 wire deletion skips the gate"        "$fixture/sha256-delete"

expect_gate "code update runs the full gate"                  "$fixture/code-update"
expect_gate "branch creation runs the full gate"              "$fixture/branch-creation"
expect_gate "tag push runs the full gate"                     "$fixture/tag-push"
expect_gate "tag deletion runs the full gate"                 "$fixture/tag-delete"
expect_gate "mixed delete+update runs the full gate"          "$fixture/mixed"
expect_gate "code then delete still runs the full gate"       "$fixture/delete-last"
expect_gate "empty input runs the full gate"                  "$fixture/empty"
expect_gate "blank line runs the full gate"                   "$fixture/blank-line"
expect_gate "malformed line runs the full gate"               "$fixture/malformed"
expect_gate "truncated line runs the full gate"               "$fixture/truncated"
expect_gate "five fields run the full gate"                   "$fixture/five-fields"
expect_gate "short SHA runs the full gate"                    "$fixture/short-sha"
expect_gate "unknown SHA length runs the full gate"           "$fixture/unknown-sha-length"
expect_gate "zero remote oid runs the full gate"              "$fixture/zero-remote"
expect_gate "invalid ref runs the full gate"                  "$fixture/invalid-ref"
expect_gate "delete marker with non-zero local oid runs the full gate" "$fixture/delete-marker-nonzero-local"
expect_gate "non-branch ref runs the full gate"               "$fixture/non-branch-ref"
expect_gate "missing trailing newline runs the full gate"     "$fixture/no-trailing-newline"
expect_gate "CRLF input runs the full gate"                   "$fixture/crlf"
expect_gate "NUL byte runs the full gate"                     "$fixture/nul"
expect_gate "dot-dot ref runs the full gate"                  "$fixture/ref-dotdot"
expect_gate "40/64 width mismatch runs the full gate"         "$fixture/width-mismatch"
expect_gate "non-hex remote SHA runs the full gate"           "$fixture/non-hex-remote"
expect_gate "non-hex SHA256 remote runs the full gate"        "$fixture/sha256-non-hex-remote"

# Locale-sensitive shell ranges must not admit uppercase object IDs.
write locale-lower-sha1 "(delete) $zero40 refs/heads/hex 0123456789abcdef0123456789abcdef01234567\n"
write locale-upper-sha1 "(delete) $zero40 refs/heads/hex $(printf 'A%.0s' $(seq 1 40))\n"
write locale-upper-sha256 "(delete) $zero64 refs/heads/hex $(printf 'A%.0s' $(seq 1 64))\n"
for oid_test_locale in C en_US.UTF-8; do
  # C is always exercised. A host without this UTF-8 locale must report that
  # limitation instead of claiming the affected collation was tested.
  if [ "$oid_test_locale" != C ]; then
    if ! env LC_ALL="$oid_test_locale" locale charmap > "$fixture/locale-charmap" 2> "$fixture/locale-warning" ||
       [ -s "$fixture/locale-warning" ] ||
       ! grep -Eq '^UTF-?8$' "$fixture/locale-charmap"; then
      printf 'skip - unavailable test locale: %s\n' "$oid_test_locale"
      continue
    fi
  fi
  expect_fast_path "lowercase SHA1 deletion ($oid_test_locale)" "$fixture/locale-lower-sha1" "LC_ALL=$oid_test_locale"
  expect_fast_path "lowercase SHA256 deletion ($oid_test_locale)" "$fixture/sha256-delete" "LC_ALL=$oid_test_locale"
  expect_gate "uppercase SHA1 keeps full gate ($oid_test_locale)" "$fixture/locale-upper-sha1" 0 "LC_ALL=$oid_test_locale"
  expect_gate "uppercase SHA256 keeps full gate ($oid_test_locale)" "$fixture/locale-upper-sha256" 0 "LC_ALL=$oid_test_locale"
done

# --- failure propagation: exact code and no later gate step ----------------
expect_failure() { # name, input, failing step, expected exit code, env assignment
  name="$1"; input="$2"; step="$3"; want_rc="$4"; env_assign="$5"
  run_hook "$input" "$env_assign"
  if [ "$hook_rc" -ne "$want_rc" ]; then fail "$name" "exit $hook_rc, wanted $want_rc"; return; fi
  seen="$(cut -d'|' -f1 "$trace" | tr '\n' ' ')"
  case "$seen" in
    *"$step"*) ;;
    *) fail "$name" "failing step '$step' never ran"; return ;;
  esac
  after="$(printf '%s\n' "$seen" | sed 's/.*'"$step"'//')"
  if [ -n "$(printf '%s' "$after" | tr -d ' ')" ]; then
    fail "$name" "gate continued after $step: $after"; return
  fi
  pass "$name"
}

expect_failure "Docpact failure exits 64 and stops the gate" \
  "$fixture/code-update" docpact-gate 64 "FIXTURE_DOCPACT_EXIT=64"
expect_failure "audit failure exits 9 and stops the gate" \
  "$fixture/code-update" audit-rust-only 9 "FIXTURE_AUDIT_EXIT=9"
expect_failure "intermediate Cargo failure exits 7 and stops the gate" \
  "$fixture/code-update" fmt 7 "FIXTURE_FAIL_STEP=fmt FIXTURE_FAIL_EXIT=7"
expect_failure "final Cargo failure exits 7" \
  "$fixture/code-update" test 7 "FIXTURE_FAIL_STEP=test FIXTURE_FAIL_EXIT=7"

# --- helper availability and classifier failure fall back to the gate ------
mv "$repo/scripts/pre-push-deletion-only.sh" "$fixture/classifier-original"
expect_gate "missing classifier falls back to the complete ordered gate" "$fixture/pure-delete"
printf '#!/bin/sh\nexit 42\n' > "$repo/scripts/pre-push-deletion-only.sh"
chmod +x "$repo/scripts/pre-push-deletion-only.sh"
expect_gate "failing classifier falls back to the complete ordered gate" "$fixture/pure-delete"
mv "$fixture/classifier-original" "$repo/scripts/pre-push-deletion-only.sh"

# --- manual TTY input must not block --------------------------------------
# The guard must run before any stdin read; a terminal is never wire format.
guard_line="$(grep -n '\[ -t 0 \] && exit 1' "$helper_src" | cut -d: -f1)"
read_line="$(grep -n 'cat > "\$tmp"' "$helper_src" | head -1 | cut -d: -f1)"
if [ -n "$guard_line" ] && [ -n "$read_line" ] && [ "$guard_line" -lt "$read_line" ]; then
  pass "TTY guard precedes the first stdin read (line $guard_line < $read_line)"
else
  fail "TTY guard precedes the first stdin read" "guard=$guard_line read=$read_line"
fi

# Run this same harness under foreign bindings; do not test a copied cleanup snippet.
if [ "${1:-}" != '--polluted-child' ]; then
  foreign="$fixture/foreign-repo"
  git init -q "$foreign"
  git -C "$foreign" config core.hooksPath "$fixture/no-hooks"
  git -C "$foreign" config commit.gpgsign false
  git -C "$foreign" -c user.name=Fixture -c user.email=fixture@example.invalid commit -qm initial --allow-empty
  git -C "$foreign" show-ref > "$fixture/foreign-before"
  cp "$foreign/.git/config" "$fixture/foreign-config"
  set +e
  env GIT_DIR="$foreign/.git" GIT_WORK_TREE="$foreign" GIT_INDEX_FILE="$foreign/.git/index" \
    sh "$root/scripts/test-pre-push.sh" --polluted-child > "$fixture/polluted.log" 2>&1
  polluted_rc=$?
  set -e
  git -C "$foreign" show-ref > "$fixture/foreign-after"
  if [ "$polluted_rc" -eq 0 ] && cmp -s "$fixture/foreign-before" "$fixture/foreign-after" \
    && cmp -s "$fixture/foreign-config" "$foreign/.git/config" \
    && [ -z "$(git -C "$foreign" status --porcelain)" ]; then
    pass "fixture creation and both hook paths isolate foreign Git bindings and preserve foreign repo"
  else
    fail "fixture creation and both hook paths isolate foreign Git bindings" "child exit=$polluted_rc"
    cat "$fixture/polluted.log" >&2
  fi
fi

if [ "$failures" -ne 0 ]; then
  printf '\n%d case(s) failed\n' "$failures" >&2
  exit 1
fi
printf '\nall pre-push deletion fast-path cases passed\n'
