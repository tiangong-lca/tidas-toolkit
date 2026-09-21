---
title: tidas-tools Validation Guide
docType: guide
scope: repo
status: active
authoritative: false
owner: tidas-tools
language: en
whenToUse:
  - when a tidas-tools change is ready for local validation
  - when selecting proof for a Rust domain, executable asset, release, or automation change
  - when writing PR validation notes
whenToUpdate:
  - when canonical checks, supported platforms, scale budgets, or release proof changes
checkPaths:
  - assets/spec/**
  - docs/agents/repo-validation.md
  - AGENTS.md
  - .docpact/config.yaml
  - Cargo.toml
  - Cargo.lock
  - crates/**
  - contracts/**
  - assets/**
  - packaging/**
  - migration/**
  - .github/workflows/**
  - .github/actions/native-xml/**
  - .githooks/pre-push
  - scripts/**
lastReviewedAt: "2026-09-21"
lastReviewedCommit: "acdbf09ecd5e6baad6bae89054c57186cfbb7e69"
lastReviewedNote: "Reviewed for Toolkit #215: public-spec qualification now downloads the exact 0.2.2 archive and includes singleton/repeated Process reviews, ordered XML round-trip, and an indexed second-item failure."
related:
  - ../../AGENTS.md
  - ../../.docpact/config.yaml
  - ./repo-architecture.md
  - ./cli-contract.md
  - ../../README.md
---

# Validation guide

## Default baseline

Run this for every non-documentation change:

```bash
scripts/audit-rust-only.sh
cargo run --locked -p tidas-assets --bin tidas-asset-lock -- check
cargo run --locked -p tidas-assets --bin tidas-asset-lock -- spec-check \
  --archive <QUALIFIED_CANDIDATE.tgz>
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
scripts/sync-rust-package-assets.sh check
scripts/publish-crates.sh check
```

Rust 1.98.1 is the required compiler for local, CI and release validation;
there is no separate Rust 1.88 compatibility matrix.

The asset command checks both the paired English/Chinese schema contract and
the complete executable-asset byte lock. `spec-check` additionally proves the
generated public copy still matches the qualified `tidas-spec` candidate; CI
fetches that candidate from its exact source commit and verifies the archive
digest against the Rust pin before running it, so a hand-edited generated copy
fails the pull request. Import provenance lives under `assets/spec/`, outside
the executable asset roots, so pinning a specification cannot silently move the
runtime fingerprint; adopting genuinely different public bytes regenerates the
locks as a deliberate, reviewed change. The pre-push hook adds strict
Docpact. Pull requests run the product matrix on Linux x86_64/ARM64, macOS
Apple Silicon, and Windows x86_64. macOS Intel and Windows ARM64 are
intentionally absent; the POSIX installer rejects macOS Intel before any
download.

Windows qualification uses `CARGO_BUILD_TARGET=x86_64-pc-windows-msvc`,
target-specific `-C target-feature=+crt-static` and vcpkg's
`x64-windows-static` triplet. The release job reads the built PE's dependency
and import tables with the selected Visual Studio DUMPBIN, rejects unbundled
VC runtime/XML DLLs, then performs repeated package/checksum verification and
the extracted archive smoke. A smoke on a developer runner alone cannot prove
closure: v0.2.1's `VCRUNTIME140.dll` import was masked by installed VC tools.
The corrected artifact must remove that dependency rather than require a user
to install a redistributable or copy a development-machine DLL.

## Validation matrix

| Change | Minimum local proof | Higher-risk proof |
| --- | --- | --- |
| CLI, contracts, or shared runtime | baseline; root and affected command help; deterministic JSON/version/completion; report/stdout separation; usage and exit-class tests | configuration precedence, cancellation, bounded queues, memory accounting, spool determinism, and all affected JSON Schema contracts |
| conversion | focused conversion + CLI tests; both directions; representative category round-trips; schema-order/XSD proof with scrambled JSON members; envelope and projection-recovery sidecars; source-semantic hash proof; tree hash; symlink, invalid XML, cancellation, budget, rollback | run the local package twice, validate every projected XML document, recover every adapted TIDAS fragment, compare tree hashes, and record wall time/RSS |
| import | all supported format fixtures; native target validation; deterministic package/mapping/bundle hashes; malformed/unsupported input, cancellation, budget, atomic publication | large exchange/issue-spool fixture with wall time/RSS and cross-root determinism |
| export | focused crate/CLI tests; report schema; secret redaction; unsafe paths; cancellation/budget; version suffixes; deterministic ZIP; atomic replacement | disposable local PostgreSQL and S3-compatible fixtures twice, comparing archive bytes and membership |
| release | closure/order/round-trip golden fixtures; missing/inexact reference failure; four deterministic ZIPs; native validation; cancellation/budget; atomic directory publication | run the local 237 MiB package twice, compare all four archives, and record wall time/RSS |
| validation/batch/references | compile every bundled schema/XSD root offline; schema and semantic fixtures including internal keyrefs; complete TIDAS projection/XSD/recovery proof; explicit schema-only diagnostic behavior; oversized rejected-instance event below the 1 MiB frame ceiling; bounded issue spool; batch preflight/drift/final-event hash; extraction schema/roles | local large-package validation twice, recording native time, projection/XSD/recovery time, peak RSS, cancellation, and spool hash |
| assets | baseline asset check; representative `git check-attr eol`; schema-local-reference and translation-parity tests | regenerate locks only after reviewing every changed path/hash; compare fingerprints twice |
| public-specification pin | `tidas-asset-lock spec-check`; focused English/Chinese validation proving Process review accepts a singleton or ordered non-empty array, reports an invalid second item at index `1`, and Process/LCIA Method may omit `common:referenceToCompleteReviewReport`; confirm Lifecycle Model behavior and retained tools-owned methodologies, eILCD inputs, and validation indexes are unchanged | `spec-import` against the qualified archive: imported/authored/metadata partition checks, negative, rollback (failure after staging, not only input parsing), no-write check mode, repeated-import idempotency, manual drift detection, XML-to-JSON-to-XML repeated-review preservation, and `.crate` parity of the 39-file public subset |
| W8 public-rule/profile composition | `tidas-asset-lock public-rules-check`; focused `tidas-assets` and `tidas-rulesets` tests; `tidas ruleset` JSON probes | exact-checkout `public-rules-sync`; stale identity, tampered bytes, unknown/duplicate IDs, incomplete ordering, and invalid profile references; compare rule IDs/order and toolkit policy with the pre-W9 baseline |
| SDK dispatch path contract | `bash scripts/ci/test-dispatch-impact.sh` | confirm only the tools-owned elementary taxonomy asset can emit `tidas_tools_changed`; public-rule definitions and toolkit profiles do not independently dispatch the SDK refresh |
| XML/XSD/XSLT | focused `tidas-xml` and validation tests; resolver/security tests; four-platform CI | representative production schemas/stylesheets and static-release dependency inspection |
| native distribution | focused `tidas-dist`; package twice; archive/checksum equality; extract and run version/help/JSON/ruleset; installer syntax and hermetic installer contract tests | four release jobs, clean-machine archive execution, runtime dependency inspection, SBOM and attestation |
| crates.io | sync check; public-set qualification; verify exact version set and `tidas-dist` exclusion; script syntax | inspect each `.crate`; source install; registry absent/existing checksum simulations without a real token |
| release request or final migration marker | shell syntax; tamper/append-only validation; actionlint; strict Docpact | simulate modification/multiple-file/target/tag/ancestry conflicts and confirm exact-tag workflow dispatch |
| governed docs only | strict Docpact config validation and enforced lint | one focused route rendering for the changed intent |

Scale proofs must run locally first. The canonical large package is outside
Git:

```text
/Users/biao/Code/lca-workspace/lca-workspace/_test_data/tidas-package-open_data-1784707539957.zip
```

Acceptance limits:

- native schema validation: at most 60 seconds
- unzip/hash/parse/validate/spool, excluding remote download: at most 3 minutes
- peak RSS: at most 512 MiB
- issue-detail memory must remain bounded even near 1.07 million issues

## PR validation note

Record:

1. exact baseline commands and results;
2. focused CLI/domain probes and input formats;
3. scale wall time, peak RSS, output/spool hashes, and repeat count when run;
4. package/archive/clean-machine proof when distribution changes;
5. whether the owned asset paths require a downstream `tidas-sdk` refresh;
6. any cross-platform proof left to CI.

Do not claim a deferred cross-platform job or external connector test as a
local pass.

For notice input collection, run `cargo test --locked -p tidas-dist` and the
internal exporters against the actual locked Cargo registry archives, native
installation and Rust sysroot. Regressions must cover tampered crate and
supplement bytes, missing native/toolchain material, symlink traversal,
repeatable output and preservation of an existing output on failure. Record
the selected target and package counts as source-input evidence. Then generate
the executable-bound bundle from a clean committed source tree with the actual
static-build environment and Rust source/documentation components. Package
twice and run archive verification and smoke. Tests must reject a foreign
binary, omitted dependency records, altered canonical terms even with updated
local hashes, and deleted archive notices even with an updated outer checksum.
Distribution-manifest v2 and the complete notice tree must qualify on all four
native jobs before a versioned release. Source-only exports do not prove that
native packaging or publication has completed.

## Local Docpact push gate

```bash
./scripts/install-git-hooks.sh
```

The hook first classifies the complete pre-push input with
`scripts/pre-push-deletion-only.sh`. A push that only deletes existing branch
refs delivers no source, so it exits without running source validation. Every
other input — code or tag publication, branch creation, mixed, empty, manual,
malformed, truncated, unknown object-id length or invalid ref input — keeps the
full gate below unchanged. The classifier is a property of the input alone; no
environment variable selects the fast path. Regression coverage lives in
`scripts/test-pre-push.sh`.

The hook runs strict Docpact, the Rust-only audit, both asset locks, format,
clippy, and all workspace tests. Override an unusual comparison only with an
explicit `DOCPACT_BASE_REF`.

For repository migration, exercise the actual SDK-dispatch shell block with a
local `gh` capture shim and verify the canonical endpoint plus unchanged exact
SHA/event/package payload without sending a real refresh. Native notice tests
must preserve historical input readability and reject unrelated namespaces and
future versions claiming the historical namespace. Installer contracts and the
full native package matrix retain their usual integrity requirements.


## Native CI archive reuse and isolated Cargo outputs

For native-cache or package-gate changes, also run `bash scripts/test-native-cache.sh`,
`bash scripts/test-publish-target.sh`, shell syntax checks and actionlint on the
changed workflows before pushing. Cache identity regressions cover missing inputs,
image/platform/triplet/baseline/input-hash changes and required installation outcomes.
Package regressions cover default and configured output paths (including spaces),
stale default archives and one metadata query. The packaging gate uses Cargo's
reported `target_directory`, so isolated `CARGO_TARGET_DIR` or Cargo configuration
does not cause a false missing-archive failure after successful packaging.

The native XML action restores only vcpkg compressed binary archives. It creates a
fresh vcpkg checkout and installed tree and runs bootstrap/install every time. Only
cache transport is optional; skipped or failed native installation fails the final
action verifier. Windows checks each native command's exit code. Existing static
imports, source-bound notices, repeatable archive bytes, checksums, smoke and the
complete four-platform qualification remain mandatory. A cache hit is not a
qualification receipt.

Outer keys bind the hosted image/platform, triplet, pinned vcpkg baseline and the
manifest/action/key implementation bytes. vcpkg retains its own ABI selection.
Main pushes affecting native-cache inputs seed default-branch archives through the
same action, without running product release or attestation jobs. PRs, tags and
manual dispatch retain their existing full native qualification behavior; publication
remains tag-only. GitHub does not evaluate path filters for tag pushes, so the
main seed path filter does not narrow version-tag qualification. RustCI dependency
selection is unchanged.

Compare actual cold/warm runs at matching keys/images: installation time, cache
restore/save, whole jobs and all gate outcomes. Count the default-branch seed and
cache storage as costs. Historical Windows steps (442s native release, 405s RustCI)
are context, not measured savings. See the hosted evidence below for current qualification status.

References: [Cargo metadata output](https://doc.rust-lang.org/cargo/commands/cargo-metadata.html),
[vcpkg archive cache](https://learn.microsoft.com/en-us/vcpkg/consume/binary-caching-default)
and [binary cache providers](https://learn.microsoft.com/en-us/vcpkg/reference/binarycaching).
The added cache action uses verified stable v6.1.0 pinned to its full executable SHA.

Local #193 evidence: the unchanged source baseline passed audit, assets, formatting,
clippy, workspace tests and sync; its package gate exposed the configured-target bug.
After the fix, all seven canonical gates passed in an isolated target directory,
including actual package/dry-run/checksum qualification (58.694s). These local
results are not a native CI speedup claim. Workflow actionlint also
passes; the separate hosted evidence below owns native runtime and cache results.


### Native cache cold baseline, 2026-09-15

[Run 34935796586, attempt 1](https://github.com/tiangong-lca/tidas-toolkit/actions/runs/34935796586)
at `37ce8602fb8aec00fd182f8e2976f7911ff783c4` passed all four native
jobs, package dry-run, complete-set aggregation and Winget validation. Main-only
seeding and tag-only publication were skipped. Each native archive lookup missed
and each completed job saved its key. Composite contexts, image identities,
installation and mandatory-outcome checks executed successfully on all four hosts.

| Platform | Native action (seconds) | Job (seconds) | Native cache save (seconds) |
| --- | ---: | ---: | ---: |
| Linux x64 | 50 | 271 | 1 |
| Linux ARM64 | 50 | 230 | 2 |
| macOS ARM64 | 62 | 407 | 1 |
| Windows x64 | 411 | 902 | 2 |

The native-action interval includes key tests, cache lookup, checkout/bootstrap,
installation and outcome verification. A documentation-only follow-up retains the
same native inputs for warm comparison. Existing Rust target caching can also
warm independently; do not attribute the entire job delta solely to this new
archive cache. Compare actual keys/images, restored-package logs, archive size,
transfer overhead and all qualification results. Synthetic skipped jobs are not
timing samples. Default-branch seeding is still pending delivery.

Required-field diagnostic changes must prove that the exact missing property is machine-readable while missing review/compliance fields remain validator errors. Keep schemas, exit class and raw issue counts unchanged; verify bounded context and ordinary non-required issues too.
