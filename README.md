---
title: tidas-tools README
docType: guide
scope: repo
status: active
authoritative: false
owner: tidas-tools
language: en
whenToUse:
  - when you need English user-facing CLI examples, native installation, or basic development commands
whenToUpdate:
  - when English CLI examples, installation, development commands, or release notes change
checkPaths:
  - README.md
  - AGENTS.md
  - .docpact/config.yaml
  - docs/agents/**
  - Cargo.toml
  - crates/**
  - packaging/**
  - scripts/install.*
  - scripts/publish-crates.sh
  - scripts/test-release-request.sh
  - scripts/validate-release-request.sh
  - scripts/sync-rust-package-assets.sh
  - .github/workflows/**
lastReviewedAt: 2026-09-20
lastReviewedCommit: 1230d4186a79a24fb17b42995641a13ed5336515
lastReviewedNote: "Toolkit #211 removes the old packaged mixed runtime projection while preserving the catalog report from exact W8 public definitions and toolkit-owned profiles."
related:
  - AGENTS.md
  - .docpact/config.yaml
  - docs/agents/repo-validation.md
  - docs/agents/repo-architecture.md
  - README_CN.md
---

# TianGong TIDAS Tools User Guide

[![crates.io](https://img.shields.io/crates/v/tidas.svg)][crates.io]
[![GitHub Release](https://img.shields.io/github/v/release/tiangong-lca/tidas-toolkit)][releases]

[crates.io]: https://crates.io/crates/tidas
[releases]: https://github.com/tiangong-lca/tidas-toolkit/releases

[English](https://github.com/tiangong-lca/tidas-toolkit/blob/main/README.md) | [中文](https://github.com/tiangong-lca/tidas-toolkit/blob/main/README_CN.md)

This repository provides one cross-platform Rust executable named `tidas` for
TIDAS conversion, import, export, validation, release, and ruleset operations.

## Unified Rust CLI

The Cargo workspace provides stable
machine and invocation contracts, bounded runtime primitives, executable-asset
integrity lock, XML/XSD/XSLT portability boundary, the unified CLI adapter,
and native TIDAS/ILCD validation, reference extraction, batch evidence,
ruleset inspection, bidirectional TIDAS/eILCD conversion, external-format
import, database export, deterministic release control, and reproducible native
distribution:

```bash
cargo build --workspace
cargo build --release -p tidas --bin tidas
cargo run -p tidas --bin tidas -- --help
cargo run -p tidas --bin tidas -- --format json version
cargo run -p tidas --bin tidas -- convert <tidas-package-dir> \
  --output <eilcd-package-dir> --to ilcd --format json
cargo run -p tidas --bin tidas -- convert <eilcd-data-dir> \
  --output <tidas-package-dir> --to tidas --format json
cargo run -p tidas --bin tidas -- import <source-file-or-dir> \
  --output <import-output-dir> --target both --write-mapping --format json
cargo run -p tidas --bin tidas -- export \
  --output <package.zip> --skip-external-docs --format json
cargo run -p tidas --bin tidas -- validate <package-dir> \
  --issues <issues.jsonl> --format json
cargo run -p tidas --bin tidas -- validate <ilcd-dir> \
  --input-format ilcd-xml --issues <issues.jsonl> --format json
cargo run -p tidas --bin tidas -- release build-packages \
  --tidas-dir <canonical-tidas-dir> \
  --dataset-index <canonical-dataset-index.json> \
  --output-dir <release-dir> --format json
cargo run -p tidas --bin tidas -- ruleset --format json
cargo run -p tidas --bin tidas -- --completion bash > tidas.bash
cargo run -p tidas-assets --bin tidas-asset-lock -- check
cargo run -p tidas-dist -- version
```

For a source-built release executable, use the package-qualified command
`cargo build --release -p tidas --bin tidas`; the binary is written to
`target/release/tidas` (`target/release/tidas.exe` on Windows).

The command tree is `convert`, `import`, `export`, `validate`, `release`,
`ruleset`, and `version`. All seven commands are implemented in Rust and none
uses a secondary runtime.

Native import accepts EcoSpold 1/2, SimaPro CSV, openLCA JSON-LD, openLCA
process XLSX, and ILCD files, directories, or ZIP packages. It detects the
source format by default; use `--from-format` to resolve ambiguous inputs.
The command always writes and validates TIDAS internally, optionally publishes
ILCD with `--target ilcd|both`, writes per-process dependency bundles by
default, and enables deterministic `mapping.csv.gz` with `--write-mapping`.
`.zolca` is rejected. Parsing, exchanges, and issue reporting use bounded,
cancel-aware, disk-backed streams, and no partial output is published on
failure.

Native conversion mirrors input under `OUTPUT/data`, preserves package
metadata, materializes the locked target schemas/stylesheets/methodologies,
and publishes the entire output directory atomically. TIDAS documents with
top-level extension metadata use deterministic `.tidas-envelope.json`
sidecars so eILCD remains single-root XML and the reverse conversion restores
the original envelope. When valid TIDAS content is stricter or richer than the
target eILCD XSD surface, conversion also writes deterministic
`.tidas-recovery.json` sidecars: the XML contains the eILCD projection and the
sidecar retains every adapted source fragment for verified reverse recovery.
Traversal rejects symlinks and XML 1.0-invalid
characters; repeated successful runs report the same output-tree SHA-256.

Native export reads active PostgreSQL records from one repeatable-read,
read-only snapshot, streams them through a bounded queue, normalizes TIDAS
package versions, optionally streams S3-compatible external documents, and
publishes one deterministic ZIP atomically. Set `TIDAS_DATABASE_URL`; storage
credentials are accepted only through `TIDAS_S3_ACCESS_KEY_ID`,
`TIDAS_S3_SECRET_ACCESS_KEY`, and optional `TIDAS_S3_SESSION_TOKEN`. Credential
values never appear in reports or diagnostics.

Native validation resolves only embedded integrity-locked schemas. TIDAS
package validation is complete by default: native schema and semantic checks
must pass, the actual TIDAS-to-eILCD adapter is run, the projected XML is
validated against the target eILCD XSDs, and reverse recovery is compared with
the source semantics. `--schema-only` is an explicitly weaker diagnostic mode.
Complete
issues can be written atomically as deterministic JSONL with `--issues`; the
operation report retains bounded counts and the spool hash instead of an
in-memory issue array. ILCD XML uses the same bounded report contract with
offline reusable XSD contexts. `document-validation-batch.v1` adds manifest
preflight, drift-proof issue events, and a deterministic final evidence hash.
Validation progress is bounded and written only to stderr; use
`--progress always` for non-interactive runs.

Global runtime options follow `CLI > TIDAS_* environment > built-in default`
precedence. No configuration file is loaded implicitly. Stdout contains only
the requested human/JSON report or completion script; logs, progress,
diagnostics, and report-file confirmations use stderr. Use `--report <PATH>`
to persist the report without occupying stdout. The default accounted memory
budget is 512 MiB and the default bounded queue capacity is 256. The normative
contract is [docs/agents/cli-contract.md](docs/agents/cli-contract.md).

## Native distribution

Prebuilt GitHub archives are the primary end-user channel. They include the
native XML dependencies and do not require Rust or a development toolchain.
The native release workflow qualifies one exact `tidas` binary for Linux
x86_64/ARM64, macOS Apple Silicon, and Windows x86_64. It builds every
archive twice, compares the bytes, verifies SHA-256, runs packaged `version`,
help, JSON `version`, and `ruleset` probes, generates an SPDX SBOM, and creates
GitHub OIDC provenance/SBOM attestations. Pinned static libxml2/libxslt inputs
keep the archives independent of Homebrew, vcpkg, Java, Node.js, or a
development toolchain at runtime.

The same `v<version>` release publishes the `tidas` source package and all
reusable domain crates to crates.io before the immutable GitHub Release is
created. Developers who already have Rust 1.98.1+ and the platform libxml2 /
libxslt development dependencies can install the unified executable from
source:

```bash
cargo install tidas --version 0.3.0 --locked
```

All public workspace crates use the exact same version so Cargo cannot combine
incompatible domain releases. `tidas-dist` remains repository-internal release
tooling and is never published. Pull requests run a complete multi-package
`cargo package` verification plus crates.io dry-run without credentials; only
the tag-context release workflow receives `CARGO_REGISTRY_TOKEN`.

A native release is authorized by reviewing and merging one append-only
`.github/releases/v<version>.json` request that binds the version to a full
commit SHA. Pull-request validation is read-only and secret-free. The merge
job creates or verifies that exact lightweight tag, then explicitly dispatches
the native release workflow at the tag. This keeps artifact provenance bound
to the released source commit rather than the request merge commit.

After a native version is published, install an explicit immutable version:

```bash
curl --proto '=https' --tlsv1.2 -fsSLO \
  https://raw.githubusercontent.com/tiangong-lca/tidas-toolkit/main/scripts/install.sh
sh install.sh --version 0.2.0 --prefix "$HOME/.local"
```

```powershell
.\scripts\install.ps1 -Version 0.2.0
```

Every GitHub Release also carries generated Homebrew formula and Winget
manifests that reference the same archive hashes. External tap creation or a
Winget community submission is a separate publication approval; those paths
never rebuild the executable. The Homebrew formula selects the Apple Silicon
archive on macOS, and the POSIX installer rejects macOS Intel before any
download. macOS Intel and Windows ARM64 are not supported.

## Development

Rust 1.98.1 or newer is required. Install the platform libxml2/libxslt development packages, then run:

```bash
scripts/audit-rust-only.sh
cargo run --locked -p tidas-assets --bin tidas-asset-lock -- check
cargo run --locked -p tidas-assets --bin tidas-asset-lock -- spec-check \
  --archive <QUALIFIED_CANDIDATE.tgz>
cargo fmt --all --check
cargo clippy --locked --workspace --all-targets -- -D warnings
cargo test --locked --workspace --all-targets
```

Use `cargo run -p tidas-assets --bin tidas-asset-lock -- write` after an intentional schema or executable-asset change, review both lock diffs, and then rerun the checks. See [the validation guide](docs/agents/repo-validation.md) for domain and large-package proof.

The 36 public schemas, two shared methodology documents, and paired `schema.lock.json` are the exact reviewed 0.2.2 public-spec candidate copy: 33 assets retain toolkit import provenance and six public assets are authored or derived by `tidas-spec`. Process `validation.review` accepts either the historical singleton object or a non-empty ordered array, matching repeated ILCD `<review>` elements. W9 additionally pins the reviewed public-rule candidate under `assets/tidas/rules/` by exact commit and hashes. `runtime_profiles.v1.json` owns toolkit severity, phases, blocker defaults, profile membership, and five local rules; the runtime catalog is composed from verified public definitions and toolkit policy without the old mixed-file input. Use `tidas-asset-lock public-rules-sync --source-root <EXACT_TIDAS_SPEC_CHECKOUT>` to refresh the public copy and `public-rules-check` to verify. This candidate binding is not a formal release claim. SDK refresh dispatch remains limited to the tools-owned elementary taxonomy asset.

The implementation that preceded the Rust-only cutover is immutable historical evidence in Git history and the tag declared by `migration/final-python-line.json`. It is not an installation, execution, CI, or release path.

## Contribution

Open a GitHub Issue before implementation. Follow [AGENTS.md](AGENTS.md), the repository Docpact route, and the workspace delivery workflow. Pull requests should include focused validation and any required downstream SDK or root-workspace integration notes.
