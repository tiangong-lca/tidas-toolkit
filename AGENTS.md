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
lastReviewedAt: 2026-09-15
lastReviewedCommit: e4afb1628a1112a0866cc01dbaebc429f94228ea
lastReviewedNote: "Reviewed for #195: only complete branch-deletion-only wire input skips source validation. Full code/tag/mixed/unknown/manual gates and exit/argv behavior remain; 35 fixture cases, real TTY and argv mutation probes, actionlint and all seven canonical local gates pass. Four-platform CI and root integration remain pending."
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

The public TIDAS specification belongs in `tiangong-lca/tidas`; generated SDK
surfaces belong in `tiangong-lca/tidas-sdks`; root multi-repo integration belongs
in `lca-workspace`. This repo may dispatch an SDK refresh when owned schema or
methodology assets change, but it does not own generated SDK code.

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
- Issue count must not cause linear operation-report memory growth.
- Successful output publication is atomic and deterministic for identical
  inputs; failures must not expose partial output.
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
