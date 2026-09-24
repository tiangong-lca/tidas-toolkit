---
title: tidas-tools Repo Contract
docType: contract
scope: repo
status: active
authoritative: true
owner: tidas-tools
language: en
whenToUse:
  - when changing the unified tidas CLI, Rust domain crates, executable assets, contracts, validation, or release automation
  - when routing work from lca-workspace into tidas-tools
  - when deciding whether work belongs in tidas-tools, tidas-sdk, tidas, or lca-workspace
whenToUpdate:
  - when product ownership, supported platforms, validation, asset, SDK dispatch, or release automation changes
  - when repo-local documentation governance changes
checkPaths:
  - assets/spec/**
  - AGENTS.md
  - README.md
  - README_CN.md
  - .docpact/config.yaml
  - Cargo.toml
  - Cargo.lock
  - crates/**
  - contracts/**
  - assets/**
  - packaging/**
  - migration/**
  - docs/agents/**
  - scripts/**
  - .github/workflows/**
  - .github/actions/native-xml/**
  - .githooks/pre-push
lastReviewedAt: 2026-09-24
lastReviewedCommit: eb46ce7a6ea681df6ac60f83ef0e323865358912
lastReviewedNote: "Reviewed Toolkit #224 on merged #222/#223: reverse conversion restores both Process year types and the empty LCI method after semantic recovery, without type-only sidecar entries that could hide XML edits. Ownership, Windows spool behavior and release controls remain unchanged."
related:
  - .docpact/config.yaml
  - docs/agents/repo-architecture.md
  - docs/agents/repo-validation.md
  - docs/agents/cli-contract.md
  - migration/python-to-rust-owners.md
---

# tidas-tools repository contract

## Product and ownership

This repository owns one cross-platform executable named `tidas`, with the
top-level commands `convert`, `import`, `export`, `validate`, `release`,
`ruleset`, and `version`. The CLI is a thin adapter over reusable Rust domain
crates. Do not add alternate executables, compatibility aliases, runtime
fallbacks, or language wrappers here.

Repository-owned behavior belongs in the corresponding crate:

| Path | Responsibility |
| --- | --- |
| `crates/tidas-cli` | parsing, configuration precedence, output routing, completion, cancellation wiring, and thin dispatch |
| `crates/tidas-contracts` | stable reports, diagnostics, artifacts, completeness, and exit classes |
| `crates/tidas-runtime` | explicit memory accounting, bounded queues, cancellation, and streaming spools |
| `crates/tidas-conversion` | deterministic TIDAS/eILCD conversion with schema-ordered ILCD output and atomic publication |
| `crates/tidas-import` | bounded external-format import and canonical publication |
| `crates/tidas-export` | repeatable-read database export, S3-compatible streaming, and deterministic ZIP output |
| `crates/tidas-validation` | offline JSON Schema and ILCD/XSD validation |
| `crates/tidas-release` | deterministic closure validation and release-package construction |
| `crates/tidas-rulesets` | methodology/ruleset catalog validation and selection |
| `crates/tidas-references` | pure reference extraction |
| `crates/tidas-xml` | streaming XML inspection and the serialized XSD/XSLT compatibility boundary |
| `crates/tidas-dist` | repository-internal native archive, checksum, smoke, SBOM, Homebrew, and Winget tooling |
| root package `tidas-assets` | embedded assets, paired-schema parity, integrity locks, and fingerprints |

`contracts/**` is authoritative for stable machine schemas. Generated
crate-local contract copies must stay synchronized through
`scripts/sync-rust-package-assets.sh`.

Executable schemas, methodologies, validation indexes, XSD, XSLT, and XML
references live under `assets/**`. They are runtime inputs, not documentation.
`assets/tidas/schema.lock.json` proves the English/Chinese schema file sets,
Draft 7 validity, local-reference closure, localized-description-only
differences, and content/contract hashes. `assets/asset-lock.v1.json` owns the
complete executable-asset byte set. Both locks are generated and checked by
`tidas-asset-lock`.

The 36 public schemas, the two shared methodology documents, and the paired
`schema.lock.json` are a generated copy of the qualified 0.2.3 specification candidate
from `tiangong-lca/tidas-spec`. The pin — package version, archive
SHA-256, manifest SHA-256, specification revision, and the tools commit from
which the imported subset was extracted —
is Rust source in `crates/tidas-assets/src/spec_pin.rs`; the candidate's own
manifest and the import provenance record live under `assets/spec/`, which is
*not* an executable asset root and is not covered by `assets/asset-lock.v1.json`.
Do not hand-edit a public schema or methodology: regenerate it with
`tidas-asset-lock spec-import --archive <QUALIFIED_CANDIDATE>`, and let
`tidas-asset-lock spec-check` prove the copy still matches the candidate.
The candidate contains 33 imported public assets plus six public assets authored
or derived by `tidas-spec`; its remaining two authored evidence files and package metadata
are verified but are not copied into the executable asset tree. This candidate
binding is not a formal release claim.

The W8 public definitions under `assets/tidas/rules/` are an exact
candidate-bound copy owned by `tidas-spec`. Toolkit owns
`runtime_profiles.v1.json`: profile membership, severity, phase, blocker
defaults, local-only rules, authorization, and result disposition.
The runtime catalog is composed directly from those verified inputs; the old
mixed compatibility files are no longer packaged or required. The elementary
taxonomy extension, eILCD inputs, and validation indexes remain tools-owned.

The public TIDAS specification source belongs in `tiangong-lca/tidas-spec`;
generated SDK surfaces belong in `tiangong-lca/tidas-sdks`; root multi-repo
integration belongs in `lca-workspace`. This repo consumes the public
specification as a pinned artifact and may dispatch an SDK refresh when
tools-owned taxonomy assets change, but it does not own the
specification or generated SDK code.

The SDK dispatch workflow is intentionally narrow: public schemas and
`tidas_flows.yaml`/`tidas_processes.yaml` are notified by `tidas-spec`'s
`tidas_spec_released` event. This repository sends `tidas_tools_changed` only
for `elementary_flow_taxonomy_extension.v1.json`, with the exact merged commit
and affected package list.

## Required load order

1. Read this file.
2. Read `.docpact/config.yaml`.
3. From the workspace root, use `scripts/docpact route --root . --intent
   tidas-tools`.
4. Read `docs/agents/repo-architecture.md`.
5. Read `docs/agents/repo-validation.md`.
6. Read `docs/agents/cli-contract.md` for public CLI or downstream invocation
   changes.
7. Read the tracked GitHub Issue and current comments before implementation.

For workspace-tracked delivery, also follow the root workspace
`lca-workspace-delivery-workflow` skill and branch policy.

## Implementation invariants

- Large data paths must stream, use bounded queues, reserve explicit memory,
  check cancellation, and spool unbounded detail to disk.
- Native ILCD import must carry source-backed Process and Source names, caveats,
  data-source references, and exchange derivation context into TIDAS and eILCD.
  A generic import placeholder must not replace a meaningful source field while
  a successful report claims that the process is valid. It must not infer an
  untyped non-flow basis or bind one Source UUID to another local source file.
- Validation issue and batch-event spools must persist atomically under normal
  task-contained Windows paths beyond 260 characters. Reports retain the
  caller's path and distinguish data findings from I/O failure.
- Issue count must not cause linear operation-report memory growth.
- Successful output publication is atomic and deterministic for identical
  inputs; failures must not expose partial output.
- eILCD reverse conversion must restore schema-sensitive Process JSON types,
  including integer reference and optional valid-until years and a present
  empty LCI method object. Recover these types after semantic recovery
  verification; type-only sidecar entries can hide edits to projected XML.
  A normalized semantic recovery hash is not proof that the reversed TIDAS
  package passes native schema validation.
- Traversal is path-sorted, rejects symlinks where required, and rejects unsafe
  archive paths.
- Machine JSON is UTF-8, LF-terminated, versioned, and deterministic.
- Stdout contains only the requested report or completion; diagnostics,
  progress, and file-write confirmations use stderr.
- Configuration precedence is CLI, then `TIDAS_*`, then documented defaults.
- Network or arbitrary filesystem resolution is not part of schema/XSD/XSLT
  validation.
- `quick-xml` owns streaming inspection; libxml2/libxslt calls remain serialized
  behind the compatibility boundary.
- Windows ARM64 and macOS Intel are not supported targets. The required
  matrix is Linux x86_64/ARM64, macOS Apple Silicon, and Windows x86_64.
- Windows release binaries must close over the CRT as well as the XML
  libraries. Match Rust `crt-static` with the static-CRT vcpkg triplet, use an
  explicit Cargo target, and reject external VC runtime/XML imports before
  packaging. A development runner's installed redistributable is not proof of
  a self-contained release.
- Performance and completeness tests run locally before any Worker-host proof.
  The acceptance budgets are native schema validation within 60 seconds,
  complete local package processing within 3 minutes, and peak RSS within
  512 MiB.

## Canonical local validation

Rust 1.98.1 is the required source-build and CI/release toolchain.

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

Use the focused and scale proofs in `docs/agents/repo-validation.md` for the
affected domain. Run Docpact strict validation and lint before handoff.

## Release architecture

- All public crates use one exact workspace version; `tidas-dist` is not
  published.
- Pull requests qualify packages without registry credentials.
- A reviewed, append-only `.github/releases/v<version>.json` request binds a
  native release to an exact commit.
- Merging that request creates/verifies the exact tag and dispatches
  `rust-release.yml` at the tag.
- Only the tag-context release job may read `CARGO_REGISTRY_TOKEN`.
- Release archives are byte-reproducible, checksum-verified, smoke-tested, and
  accompanied by SBOM/provenance evidence.
- External Homebrew tap creation and Winget community submission require
  separate approval and must not rebuild the executable.

CI fetches the qualified candidate from its exact source commit, verifies the
archive digest against the pin, and runs `spec-check`, which fails if a public
copy has been edited by hand. CI obtains the version, revision, archive name,
archive digest, and manifest digest from `tidas-asset-lock spec-pin-env`; the
workflow does not duplicate those values. The download is never resolved
through a branch.

The pre-cutover implementation is immutable historical evidence only. Its
reviewed terminal commit and tag are declared in
`migration/final-python-line.json`; the ownership inventory in
`migration/python-to-rust-owners.md` is historical. Neither is an active
implementation, installation, CI, release, or invocation path.

## Delivery and integration

Use an executable Issue and set its Project item to `In Progress` before
implementation. Branch from canonical `origin/main`, keep changes scoped,
validate locally, then open a PR to canonical `main` with `Closes #<issue>`.
Record material findings and validation in the Issue/PR during the same
session.

A merged repository PR is repo-complete, not necessarily workspace-complete.
If the task requires root integration, update the exact `tidas-tools`
submodule pointer through a separate `lca-workspace` integration PR before the
parent delivery item is Done.

## Local Docpact push gate

Install the versioned hook once per checkout:

```bash
./scripts/install-git-hooks.sh
```

A pre-push input that contains only valid branch deletions delivers no source and
skips validation (`scripts/pre-push-deletion-only.sh`); every other input keeps the
full gate. See `docs/agents/repo-validation.md`.

The pre-push hook runs strict Docpact, the Rust-only repository audit, both
asset locks, formatting, clippy, and the complete workspace test suite. The
Docpact wrapper resolves the CLI without requiring bare `docpact` on `PATH`.


Native CI caching may reuse vcpkg binary archives but must still perform the pinned
checkout/bootstrap/install and every native qualification gate. Default-branch cache
seeding is setup only and cannot publish or attest a product. Follow the cache and
isolated Cargo output regressions in `docs/agents/repo-validation.md` when editing
these harnesses; package locations follow Cargo metadata instead of a fixed directory.
