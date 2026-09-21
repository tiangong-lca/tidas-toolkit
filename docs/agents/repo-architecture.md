---
title: tidas-tools Architecture Notes
docType: guide
scope: repo
status: active
authoritative: false
owner: tidas-tools
language: en
whenToUse:
  - when building a mental model before changing domain logic, assets, validation, distribution, or dispatch automation
  - when deciding which crate or repository owns a behavior
whenToUpdate:
  - when crate ownership, asset locations, release architecture, or downstream dispatch changes
checkPaths:
  - assets/spec/**
  - docs/agents/repo-architecture.md
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
lastReviewedNote: "Reviewed for Toolkit #215: the 0.2.2 public candidate changes Process review cardinality while leaving crate topology and runtime ownership unchanged."
related:
  - ../../AGENTS.md
  - ../../.docpact/config.yaml
  - ./cli-contract.md
  - ./repo-validation.md
  - ../../README.md
---

# Repository architecture

## Product shape

The repository builds one executable, `tidas`, with seven top-level commands:
`convert`, `import`, `export`, `validate`, `release`, `ruleset`, and `version`.
The CLI parses and routes; reusable crates own all domain behavior. No
alternate executable or runtime fallback is part of the product.

| Path | Stable responsibility |
| --- | --- |
| `crates/tidas-cli` | unified executable, invocation context, output routing, completion, cancellation wiring, thin dispatch |
| `crates/tidas-contracts` | stable operation reports, diagnostics, artifacts, completeness, exit classes |
| `crates/tidas-runtime` | bounded queues, memory reservations, cancellation, deterministic spools |
| `crates/tidas-conversion` | bidirectional TIDAS JSON/eILCD XML transformation with schema-ordered ILCD output and atomic publication |
| `crates/tidas-import` | format detection, disk-backed canonicalization, TIDAS/ILCD publication, bundles, mapping |
| `crates/tidas-export` | repeatable-read PostgreSQL extraction, S3-compatible streaming, deterministic ZIP |
| `crates/tidas-validation` | offline TIDAS JSON and ILCD/XSD validation, semantic indexes, batch protocol |
| `crates/tidas-release` | exact closure, schema-ordered ILCD derivation, native gates, deterministic release packages |
| `crates/tidas-rulesets` | public-definition/profile-policy composition, methodology catalog validation, profile selection, fingerprinting |
| `crates/tidas-references` | side-effect-free reference extraction |
| `crates/tidas-xml` | streaming XML inspection and serialized native XSD/XSLT boundary |
| `crates/tidas-dist` | internal deterministic archive/checksum/smoke/SBOM/package-manager tooling |
| root `tidas-assets` package | embedded assets, paired-schema validation, byte lock, fingerprint |
| `contracts/**` | authoritative stable machine schemas |
| `assets/**` | executable schemas, methodologies, validation indexes, XSD, XSLT, XML references |

## Stable contracts and runtime

Machine contracts use explicit `tidas.*.v1` identifiers, deny unknown fields
at stable typed boundaries, and emit deterministic LF-terminated UTF-8 JSON.
Breaking meaning requires a new schema version. Output does not depend on wall
clock, locale, checkout root, or unordered iteration.

Large-data domains receive one cancellation token, explicit memory budget, and
bounded queue capacity. Unbounded details stream to deterministic disk spools;
operation reports retain bounded summaries and hashes. Publication stages in a
sibling temporary path and commits atomically.

## Domain flow

Conversion traverses sorted package trees, rejects symlinks and invalid XML
text, uses deterministic envelope sidecars for top-level TIDAS metadata, locks
target assets, and reports a cross-platform output-tree hash. TIDAS-to-ILCD
conversion orders every known dataset object from the integrity-locked target
eILCD XSD catalog before XML serialization, so source JSON member order cannot
violate an ILCD `xs:sequence`. A semantic projection layer handles
representation differences without changing the TIDAS schemas: target-safe
XML is paired with `.tidas-recovery.json` fragments, and reverse conversion
must reproduce the source semantic hash. Release conversion reuses the same
projection, ordering, XSD-validation, and recovery components.

Import detects EcoSpold 1/2, SimaPro CSV, openLCA JSON-LD, openLCA process
XLSX, and ILCD. Adapters stream into disk-backed canonical entities/exchanges;
typed normalization and preflight run before TIDAS and optional ILCD writers.
Requested outputs are validated before one atomic commit.

Export reads one repeatable-read, read-only PostgreSQL snapshot, streams
records through bounded workers, optionally retrieves S3-compatible object
bodies by chunk, and creates one deterministic archive without serializing
credentials.

Validation resolves only embedded assets. Draft 7 schema resources and ILCD
XSD contexts are compiled offline and reused. Native TIDAS schema/semantic
validation remains independent of conversion; the CLI's default complete
TIDAS validation composes it with actual eILCD projection, XSD validation, and
semantic recovery. This keeps dependency direction acyclic while making a
successful user-facing validation a convertibility guarantee. Issue details stream or are
discarded; batch evidence preflights content hashes and publishes a final
logical stream hash only after drift-free completion. Schema diagnostics
describe rejected instances through bounded summaries and content hashes
rather than embedding arbitrarily large instance values in JSONL events.

Release consumes finalized UUID/version decisions. It resolves exact
standalone/full closure, derives schema-ordered ILCD, runs native validation and
semantic round-trip gates, and publishes two TIDAS plus two ILCD archives.

## Asset ownership

`assets/tidas/schemas/**` and `assets/tidas/schemas_zh/**` must have identical
file sets and identical structure after removing localized `description`
members. All documents must be valid Draft 7 schemas and every local `$ref`
must resolve inside its language catalog.

`assets/tidas/schema.lock.json` records per-language content and contract hashes.
`assets/asset-lock.v1.json` records every executable asset path, kind, length,
and SHA-256. `cargo run -p tidas-assets --bin tidas-asset-lock -- write`
regenerates the paired lock first and the full lock second; `check` validates
both.

The public subset of that tree — 36 schemas, the two shared methodology
documents, and the paired `schema.lock.json` — is a generated copy of the
reviewed 0.2.2 specification candidate published by `tiangong-lca/tidas-spec`. It
contains 33 assets imported from the historical toolkit source and six public
assets authored or derived in the specification repository. Its
identity is Rust source in `crates/tidas-assets/src/spec_pin.rs`: package
version, archive SHA-256, manifest SHA-256, specification revision, and the
tools commit from which the imported subset was extracted. `tidas-asset-lock
spec-import --archive <CANDIDATE>` validates
the archive, every declared file, the candidate's import manifest, and its
reviewed baseline before staging, and publishes with rollback on failure;
`tidas-asset-lock spec-check` proves the checked-in copy still matches and writes
nothing. Publication is rollback-safe rather than one indivisible multi-file
transaction: every destination is replaced by renaming a fully written file over
it, so no reader sees a truncated file, and any failure restores the prior bytes
and mode of everything already replaced. A reader that reads two files across
the rename window can still see one updated and one not-yet-updated file.
Writers are serialized by an exclusively created lock held across prior-state
capture, replacement, and rollback, so a failing import cannot undo a successful
one. Each import stages into its own exclusively created directory and removes
only that directory, so another writer's staging state is never touched.
Directory ownership is tracked explicitly: only directories the operation
actually created are removed, deepest first and only while empty, so a
pre-existing empty directory and every ancestor above the created chain survive
a rollback.
The pin is candidate qualification evidence, not a formal release claim.

Import provenance lives under `assets/spec/` — the candidate manifest verbatim
and a small pin record. That directory is deliberately outside the executable
asset roots (`assets/eilcd`, `assets/tidas`, `assets/validation_indexes`), so
adopting a specification does not by itself alter the asset set, the full lock,
or the runtime fingerprint derived from them. Adopting a candidate whose public
bytes genuinely differ *is* an executable-asset change and must be regenerated
and reviewed as one. The tools-owned runtime rulesets, the elementary taxonomy
extension, eILCD inputs, and validation indexes never come from the public
specification.

Only tools-owned elementary taxonomy changes may dispatch `tidas-sdk`
refresh automation. Generated SDK code remains downstream and never becomes
source of truth here.
The canonical sender is `tiangong-lca/tidas-toolkit` (repository ID
`936459656`, organization ID `327771381`); its dispatch targets
`tiangong-lca/tidas-sdks`. The `tidas_tools_changed` event and exact commit
payload retain their existing contract. The path filter is limited to
`elementary_flow_taxonomy_extension.v1.json`; public schemas and the shared
`tidas_flows.yaml`/`tidas_processes.yaml` methodology files are dispatched by
`tidas-spec` as `tidas_spec_released`. Token authorization must cover the
renamed downstream repository. Source/metadata-only changes do not dispatch
a generated SDK refresh.

W9 splits the catalog into two source layers. The exact candidate-bound
`assets/tidas/rules/public-rules.v1.json` supplies normative public definitions;
`runtime_profiles.v1.json` supplies toolkit-only execution policy and five
local rules. `tidas-rulesets` composes the runtime catalog from both at load
time without an old mixed-file input. `tidas-asset-lock public-rules-check`
independently verifies source identity, byte hashes, schemas, and composition.

## XML/XSD/XSLT portability

- `quick-xml` owns strict streaming inspection.
- `libxml2` through `libxml` owns XSD validation.
- `libxslt` owns XSLT 1.0 compatibility for bundled eILCD stylesheets.
- Native schema/transform calls are serialized behind one process-wide lock.
- Development builds use platform libraries; release builds use pinned static
  inputs and reject build-machine runtime dependency leakage.
- Windows CI and release builds select the `x64-windows-static` XML triplet and
  Rust `crt-static` together. An explicit Cargo target keeps host proc-macro
  builds separate, and native binaries live under the target-specific output
  directory. The release job inspects normal/delayed DLL imports and rejects
  external VC runtime or XML DLL dependencies before packaging.
- Network and arbitrary filesystem resolution fail closed.

The supported product matrix is Linux x86_64/ARM64, macOS Apple Silicon,
and Windows x86_64. macOS Intel and Windows ARM64 are not supported.

## Distribution and release

Pull requests run Rust CI across the four supported targets, verify reproducible
packages, and qualify the complete crates.io set without credentials. Public
crates share one exact version; `tidas-dist` stays internal.

The internal `tidas-dist` notice collectors retain checksum-verified Cargo
normal/build source inputs, installed vcpkg target-port notices and original
Rust library notice material. The complete producer binds those inputs and
referenced terms to the actual executable, source commit, lock and toolchain.
Current notice collectors and package-manager metadata use canonical
`tidas-toolkit` source URLs. The v1 notice reader retains the old `tidas-tools`
metadata namespace only for releases through `0.3.0`; this compatibility does
not establish signature authority or change the executable, lock, notice or
archive integrity checks. Archived manifests and release requests stay intact.

Distribution-manifest v2 includes the digest and length of the complete native
notice manifest; package/verify require matching original material and the exact
archive inventory. Dependency edge scopes, target kinds and Rust source
supersets remain distinct from actual linkage. Original licensing explanations
and canonical reference terms retain separate attribution roles. See
`packaging/README.md` and `packaging/third-party-notices/README.md`.

A reviewed append-only `.github/releases/v<version>.json` binds a native
release to a full source commit. Its merge job creates/verifies the exact tag
and dispatches `rust-release.yml` at that tag. The tag run builds each native
archive twice, compares bytes, verifies checksums, executes packaged probes,
publishes the qualified crates, generates SBOM/attestation evidence, and then
creates the immutable GitHub Release.

Homebrew and Winget metadata derive from the same checksum set. External
package-manager submissions are separate approvals and do not rebuild.

The pre-cutover implementation is retained only in Git history and the tag
declared by `migration/final-python-line.json`; checked-in migration fixtures
remain immutable semantic evidence. They are not an active code, CI,
installation, release, or invocation surface.

## Repository boundaries

- `tiangong-lca/tidas-spec` owns the public specification, its human-facing
  schema source, and its language variants.
- `tiangong-lca/tidas-sdks` owns generated SDK packages.
- `tidas-tools` owns executable behavior, packaged runtime assets, and the
  pinned import of the public specification subset.
- `lca-workspace` owns multi-repo coordination and exact submodule integration.

A merged tidas-tools PR is not workspace integration. The root pointer must be
updated separately when the tracked delivery requires it.

## Local gate

A complete, valid branch-deletion-only input skips source validation because no
source is delivered. All other inputs retain the versioned pre-push hook
with strict Docpact, the Rust-only audit, paired and
full asset locks, formatting, clippy, and workspace tests. See
`docs/agents/repo-validation.md` for focused and scale proof.


Native dependency setup is shared by package qualification and default-branch cache
seeding through `.github/actions/native-xml`. Reuse is limited to vcpkg binary
archives; installed native inputs and final product/notice outputs remain fresh.
Cache seeding owns no release, registry or attestation action. Package archive
location comes from existing Cargo metadata, preserving portable isolated builds.
