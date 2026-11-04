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
  - .github/dependabot.yml
  - .github/workflows/**
  - .github/actions/native-xml/**
  - .githooks/pre-push
  - scripts/**
lastReviewedAt: 2026-11-04
lastReviewedCommit: 00120e93ad95d7aade16f64db4bf059213f8f43f
lastReviewedNote: "Toolkit #272: reviewed official singleton Model comment projection, labeled multilingual text and exact recovery; source content, target XSD and delivery boundaries remain unchanged."
related:
  - ../../AGENTS.md
  - ../../.docpact/config.yaml
  - ./repo-architecture.md
  - ./cli-contract.md
  - ../../README.md
---

# Validation guide

## Default baseline

Run this for every product, native, workflow or gate-code change (the narrow
monitoring-only exception is documented below):

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

For LifecycleModel comment projection, run `cargo test --locked -p
tidas-conversion --test lifecycle_comments`. The synthetic fixture must pass
native TIDAS validation and exercise actual conversion, native XSD validation,
reverse conversion and repeated output-tree hashes. Cover single-language,
duplicate-language, bilingual and trilingual comments with both original and
reversed JSON member order. Require visible labeled translations, exact
recovery of adapted comment fragments, unchanged source bytes and unchanged
Process multilingual behavior. The official SDK's singleton cardinality must
remain intact; private production data is supplementary replay evidence, not
a prerequisite for this regression.

The asset command checks both the paired English/Chinese schema contract and
the complete executable-asset byte lock. `spec-check` additionally proves the
generated public copy still matches the qualified `tidas-spec` candidate; CI
first runs `tidas-asset-lock spec-pin-env` and uses its GitHub environment-file
entries to fetch that candidate from its exact source commit and verify the
archive digest before running `spec-check`. The workflow therefore carries no
second version/revision/archive/digest tuple that can become stale, while a
hand-edited generated copy
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

Dependency maintenance is configured in `.github/dependabot.yml` for the root
Cargo workspace, GitHub Actions and `packaging/vcpkg` built-in baseline.
See [dependency maintenance](../dependency-maintenance.md) for supported formats,
raw-pin ownership and exact-range maintenance scope. Updates enter reviewed PRs and keep the
existing product and native qualification gates. Cargo minor/patch updates are
grouped; major updates remain separate review candidates. Configuration changes
need YAML/schema validation and strict Docpact checks. Cargo updates need the
baseline above; Actions updates need the affected workflow's validation and
qualification. Local push hooks do not run for bot-created PRs, so maintainers
must verify the required evidence before merge. A vcpkg proposal retains coupled
manifest/workflow checkout pins and the complete native qualification; the Rust
toolchain and source-bound specification assets retain manual exact qualification.

| Change | Minimum local proof | Higher-risk proof |
| --- | --- | --- |
| CLI, contracts, or shared runtime | baseline; root and affected command help; deterministic JSON/version/completion; report/stdout separation; usage and exit-class tests | configuration precedence, cancellation, bounded queues, memory accounting, spool determinism, and all affected JSON Schema contracts |
| conversion | focused conversion + CLI tests; both directions; representative category round-trips; schema-order/XSD proof with scrambled JSON members; envelope and projection-recovery sidecars; source-semantic hash proof plus native validation of reversed TIDAS; numeric Process reference and optional valid-until years and present empty LCI method recovery, including without a sidecar; reject edits to either projected year or deletion of the empty method before publication; tree hash; symlink, invalid XML, cancellation, budget, rollback | run a reproducible representative package twice, validate every projected XML document and the reversed TIDAS package, recover every adapted TIDAS fragment, compare tree hashes, and record wall time/RSS |
| import | all supported format fixtures; native target validation; ILCD non-flow quantitative-reference regression with an elementary pollutant first and absent process type; bilingual Process/Source provenance, exact source linkage, exchange comments/IDs, and caveat-change sensitivity through TIDAS/eILCD projection plus actual reverse-conversion comparison of review-critical fields; untyped non-flow reference and missing, version-mismatched or URI-mismatched Source fail atomically; deterministic package/mapping/bundle hashes; malformed/unsupported input, cancellation, budget, atomic publication. Toolkit #224 owns complete independent validation of the reversed TIDAS package. | large exchange/issue-spool fixture with wall time/RSS and cross-root determinism |
| export | focused crate/CLI tests; report schema; secret redaction; unsafe paths; cancellation/budget; version suffixes; deterministic ZIP; atomic replacement | disposable local PostgreSQL and S3-compatible fixtures twice, comparing archive bytes and membership |
| release | closure/order/round-trip golden fixtures; missing/inexact reference failure; four deterministic ZIPs; native validation; cancellation/budget; atomic directory publication | run a reproducible representative package twice, compare all four archives, and record wall time/RSS |
| validation/batch/references | compile every bundled schema/XSD root offline; schema and semantic fixtures including internal keyrefs; complete TIDAS projection/XSD/recovery proof; explicit schema-only diagnostic behavior; oversized rejected-instance event below the 1 MiB frame ceiling; bounded issue spool; batch preflight/drift/final-event hash; extraction schema/roles; Windows x64 real-binary short/deep Unicode task-path parity for `--issues` and `--events` under the four-platform Rust CI matrix | reproducible generated large-package validation twice when scale behavior is affected, recording native time, projection/XSD/recovery time, peak RSS, cancellation, and spool hash |
| assets | baseline asset check; representative `git check-attr eol`; schema-local-reference and translation-parity tests | regenerate locks only after reviewing every changed path/hash; compare fingerprints twice |
| public-specification pin | `tidas-asset-lock spec-check`; focused English/Chinese validation proving Process review accepts a singleton or ordered non-empty array, reports an invalid second item at index `1`, and Process/LCIA Method may omit `common:referenceToCompleteReviewReport`; confirm Lifecycle Model behavior and retained tools-owned methodologies, eILCD inputs, and validation indexes are unchanged | `spec-import` against the qualified archive: imported/authored/metadata partition checks, negative, rollback (failure after staging, not only input parsing), no-write check mode, repeated-import idempotency, manual drift detection, XML-to-JSON-to-XML repeated-review preservation, and `.crate` parity of the 39-file public subset |
| W8 public-rule/profile composition | `tidas-asset-lock public-rules-check`; focused `tidas-assets` and `tidas-rulesets` tests; `tidas ruleset` JSON probes | exact-checkout `public-rules-sync`; stale identity, tampered bytes, unknown/duplicate IDs, incomplete ordering, and invalid profile references; compare rule IDs/order and toolkit policy with the pre-W9 baseline |
| SDK dispatch path contract | `bash scripts/ci/test-dispatch-impact.sh` | confirm only the tools-owned elementary taxonomy asset can emit `tidas_tools_changed`; public-rule definitions and toolkit profiles do not independently dispatch the SDK refresh |
| XML/XSD/XSLT | focused `tidas-xml` and validation tests; resolver/security tests; four-platform CI | representative production schemas/stylesheets and static-release dependency inspection |
| native distribution | focused `tidas-dist`; package twice; archive/checksum equality; extract and run version/help/JSON/ruleset; installer syntax and hermetic installer contract tests | four release jobs, clean-machine archive execution, runtime dependency inspection, SBOM and attestation |
| crates.io | sync check; public-set qualification; verify exact version set and `tidas-dist` exclusion; script syntax | inspect each `.crate`; source install; registry absent/existing checksum simulations without a real token |
| release request or final migration marker | shell syntax; tamper/append-only validation; actionlint; strict Docpact | simulate modification/multiple-file/target/tag/ancestry conflicts and confirm exact-tag workflow dispatch |
| governed docs only | strict Docpact config validation and enforced lint | one focused route rendering for the changed intent |

The read-only `published-windows-archive.yml` PR/manual qualification downloads
the immutable `v0.3.3` Windows x64 archive and checksum sidecar, verifies the
exact lightweight tag/source and independently recorded archive digest, then
runs **the archive executable**. Its Toolkit-owned synthetic Process fixture
expands to 400 indexed schema findings; short and task-contained deep Unicode
paths must produce identical issue-spool bytes/hash and the same data-issues
class. A real blocked spool destination must retain native exit 74 and
`validation_io_failed`. The workflow uploads the input, reports and proof but
cannot tag, publish or change the data #33 Process. Source-built Windows CI and
package smoke remain separate evidence, not substitutes for this installed
archive replay.

### Selecting scale proof

Use the baseline and the affected rows above to select validation. Dependency
updates need the baseline plus regressions for the behavior changed by the
updated dependency. For example, a schema-engine update needs representative
valid/invalid schema cases and diagnostics checks. Do not require a private
historical ZIP merely because a dependency changed.

Add local scale probes when throughput, memory, streaming, cancellation or
spooling may be affected, including by a dependency update. Use checked-in
non-sensitive fixtures or deterministic generators that another developer can
run in a fresh checkout. Select document sizes, category/reference shapes and
issue volume that exercise the affected behavior; retain the recipe/seed and
commands in the PR. A small functional fixture alone does not establish scale
behavior. Run repeatability probes twice and record input/output hashes,
counts, wall time, peak RSS, toolchain and machine context. Run these probes
locally before any Worker-host proof.

For a benchmark intended to reproduce the former large-package workload,
record the approximately 122,865-document / 1.07-million-issue shape explicitly.
Its comparison budgets remain native schema validation within 60 seconds,
complete unzip/hash/parse/validate/spool processing within 3 minutes (excluding
remote download), and peak RSS within 512 MiB. Smaller probes must not be
reported as satisfying those workload-specific time budgets. Issue-detail
memory must remain bounded as issue volume increases.

The historical `tidas-package-open_data-1784707539957.zip` is optional diagnostic
material, not a canonical required fixture or a development/merge gate.
Its absence is not a blocker and does not require searching a developer's home
or Downloads directory, requesting a copy, or provisioning a shared download.
Any voluntary replay uses an explicitly supplied path and records its input
identity; it does not replace the reproducible regression evidence above.
Separate consumer release-qualification workflows apply only when explicitly
in scope; do not import them as prerequisites for ordinary Toolkit changes.

The maintained generated issue-volume probe runs the actual package validator
against 64 deterministic Process documents with 256 invalid exchanges each,
checks complete bounded NDJSON output and repeats the run to compare spool
hashes. Run it explicitly when changing schema-engine or issue-stream behavior:

```bash
cargo test --locked --release -p tidas-validation --test generated_schema_scale \
  -- --ignored --nocapture
```

Record wall time and OS peak RSS for the test executable after building it;
`peak_accounted_bytes` is runtime budget accounting, not process RSS. This probe
is not the former 122,865-document workload and does not claim its time budgets.
Schema-engine migrations also need registered relative-reference success,
unregistered HTTP/file-reference rejection, bounded enum/const diagnostics and
regex-evaluation failure coverage alongside the full consumer regression suite.

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
other input is preserved for exact maintenance classification. Only one clean
known existing-branch update whose complete wire and cumulative canonical-Main
ranges modify the
existing regular monitoring config/guide may run Docpact alone on that exact
wire range. Hidden index flags and missing/diverged/ambiguous canonical tracking
context keep the full gate. The helper rechecks candidate state after Docpact. All code/tag/new
branch, mixed, empty/manual, malformed, unknown, dirty and non-allowlisted paths
keep the full gate below unchanged, including all Cargo/native/assets/workflow
and gate-code edits. No environment variable selects this exception. Regression
coverage lives in `scripts/test-pre-push.sh`; supported scope and measured cost
are in `docs/dependency-maintenance.md`.

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

Unix setup removes inherited `VCPKG_FORCE_SYSTEM_BINARIES` in its own process so
vcpkg can select and verify its existing pinned tools. Even an empty value or `0`
forces system selection upstream. `scripts/test-native-cache.sh` executes that
action block with hermetic command shims, checks inherited presence and parent
environment preservation, and removes the correction as a failing control. It also
checks that Rust CI selects native manifest, action, release-workflow and cache
helper changes. Actual selected-tool identity and successful native installation
still require fresh hosted qualification.

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

## Allocation/reference regression proof

Run `cargo test --locked -p tidas-validation --test process_semantics --test
allocation_pipeline` and `cargo test --locked -p tidas-release --test
native_release`. The shared 61-case fixture checks native/SDK validity,
completeness, findings, modes, allocation vectors and coefficients. It includes
legacy processwide Output shares, bounded targetless full-allocation fallback,
scalar-empty compatibility, sparse zero, repeated/Input/non-flow references,
exact metadata failures and decimal preservation. Percentage-point sum tolerance
is `0.0010000001`; the bounded legacy full fallback requires exactly 100.

Full-schema package fixtures cover Input/Output × Product/Waste, exact duplicate,
missing/wrong-version/Elementary evidence, content drift, cancellation and budget
failure. Batch tests prove unlisted neighboring Flow files cannot supply evidence.
Closure tests prove selected exact Product/Waste passes and Elementary rejects.
Keep this source proof distinct from a published native artifact and downstream
version/fingerprint adoption; no production calculation or dataset repair is part
of these checks.

Selected closure reports also expose `semantic_diagnostics` with precise Process
file/field paths and profile context, an exact total issue count, and up to 256
findings with explicit truncation. Direct release validation and every build
validation/profile coverage preserve unresolved partial completeness. Semantic
analysis uses conservative additional memory reservation for its finding and
coverage projections; exhausted budgets fail before successful publication.

`cargo test --locked -p tidas-release --test native_release` proves a failed
package build writes the requested NDJSON issue spool, carries the validation
summary, and retains the spool path. The CLI contract test proves the spool is
reported as an `application/x-ndjson` artifact with hash and byte count.

Additional native wire tests cover float-encoded integer IDs independently in
exchange, qref and allocation-target positions. Dense 250-row reference/share
fixtures prove the previous linear estimate fits the small test budget while
checked projection cardinality rejects before success evidence in package,
batch and selected closure paths. The pure API has no runtime budget; bounded
callers must reserve its dense projections as well as linear input/analysis.

## Result-only release profile

`cargo test --locked -p tidas-release --test native_release` covers two real
Result-only archives, native TIDAS/eILCD validity, semantic roundtrip, exact
closure, repeated archive hashes, Result role/type validation, cancellation and
memory-budget failure. The original four-package fixture still exercises the
default path. `cargo test --locked -p tidas --test cli_contract` checks explicit
profile help, invalid profile rejection and version capability advertisement.
The Result-only report validates against its separate v2 schema; v1 cardinality
is not relaxed. No generated public specification asset changes are needed.

Reproduce the generated 64-Result / 71-dataset closure probe with:

```bash
cargo test --locked -p tidas-release --test native_release \
  generated_result_only_package_scale -- --ignored --nocapture
```

The probe builds both archives twice and compares their artifact-set hash. It
qualifies this fixture size only; record wall time and peak RSS outside Cargo
when measuring runtime resources.
