#!/bin/sh
# Classify one complete git pre-push wire input, read from stdin.
#
# Exit 0 only when every line is a well-formed deletion of an existing branch
# ref. Such a push delivers no source, so the caller may skip source validation.
# Every other input exits 1 so the full gate runs unchanged. The decision is a
# property of the input alone: no environment variable can select the fast path.
#
# Deliberately conservative: non-canonical bytes (CR, NUL), a missing final
# newline, unsupported object-id lengths, tag or non-branch refs, ref
# mismatches, extra fields, and any partially parsed line all fall back to the
# full gate.
set -eu

zero40='0000000000000000000000000000000000000000'
zero64='0000000000000000000000000000000000000000000000000000000000000000'

is_oid() { # $1 = value, $2 = required length
    [ "${#1}" -eq "$2" ] || return 1
    case "$1" in
        *[!0-9a-f]*) return 1 ;;
    esac
    return 0
}

# A terminal is never the pre-push wire format; never block waiting for EOF.
[ -t 0 ] && exit 1

tmp="$(mktemp "${TMPDIR:-/tmp}/pre-push-classify.XXXXXX")" || exit 1
trap 'rm -f "$tmp"' EXIT HUP INT TERM
cat > "$tmp" || exit 1

# Non-empty, canonical, newline-terminated input only.
[ -s "$tmp" ] || exit 1
total="$(wc -c < "$tmp" | tr -d ' ')"
canonical="$(tr -d '\r\000' < "$tmp" | wc -c | tr -d ' ')"
[ "$total" -eq "$canonical" ] || exit 1
[ "$(tail -c 1 "$tmp" | wc -l | tr -d ' ')" -eq 1 ] || exit 1

while IFS=' ' read -r local_ref local_oid remote_ref remote_oid; do
    # Exactly four space-separated fields, none empty.
    case "$local_ref" in '' | *' '*) exit 1 ;; esac
    case "$local_oid" in '' | *' '*) exit 1 ;; esac
    case "$remote_ref" in '' | *' '*) exit 1 ;; esac
    case "$remote_oid" in '' | *' '*) exit 1 ;; esac

    # Deletions only. Git sends "(delete)" as the local ref for a branch
    # deletion; any other first field is a creation or update.
    [ "$local_ref" = '(delete)' ] || exit 1

    # The remote ref must be a real, valid branch ref.
    case "$remote_ref" in
        refs/heads/*) ;;
        *) exit 1 ;;
    esac
    git check-ref-format "$remote_ref" >/dev/null 2>&1 || exit 1

    # The null object id locally; an existing, matching-width object remotely.
    case "$local_oid" in
        "$zero40") oid_length=40 ;;
        "$zero64") oid_length=64 ;;
        *) exit 1 ;;
    esac
    is_oid "$remote_oid" "$oid_length" || exit 1
    [ "$remote_oid" != "$zero40" ] && [ "$remote_oid" != "$zero64" ] || exit 1
done < "$tmp"

exit 0
