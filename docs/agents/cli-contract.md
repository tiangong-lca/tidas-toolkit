---
title: Unified tidas CLI Contract
docType: contract
scope: repo
status: active
authoritative: true
owner: tidas-tools
language: en
whenToUse:
  - when adding or changing a tidas command, global option, output mode, exit class, or completion script
  - when a downstream system parses tidas JSON or invokes the executable
  - when deciding whether a behavior belongs in the CLI adapter or a reusable domain crate
whenToUpdate:
  - when the public command tree, invocation context, configuration precedence, output channels, or runtime controls change
checkPaths:
  - assets/spec/**
  - docs/agents/cli-contract.md
  - Cargo.toml
  - Cargo.lock
  - crates/tidas-cli/**
  - crates/tidas-conversion/**
  - crates/tidas-import/**
  - crates/tidas-export/**
  - crates/tidas-release/**
  - crates/tidas-contracts/**
  - crates/tidas-runtime/**
  - contracts/**
  - README.md
  - README_CN.md
lastReviewedAt: "2026-09-16"
lastReviewedCommit: 2bdb6b2fdbd2a8862e7cb36d5eb4ff18ed28530b
lastReviewedNote: "Reviewed for W5: the SDK dispatch change is workflow-only and does not alter CLI parsing, output, contracts, or runtime behavior. Public-spec ownership remains separate from tools-owned runtime assets."
related:
  - ../../AGENTS.md
  - ../../.docpact/config.yaml
  - ./repo-architecture.md
  - ./repo-validation.md
  - ../../contracts/operation-report.v1.schema.json
  - ../../contracts/export-report.v1.schema.json
  - ../../contracts/release-report.v1.schema.json
---

# Unified `tidas` CLI Contract

## Product surface

The product ships one executable, `tidas`, with exactly seven top-level
commands:

- `convert`
- `import`
- `export`
- `validate`
- `release`
- `ruleset`
- `version`

Shell completion generation is a global action, not an eighth command:

```bash
tidas --completion bash > tidas.bash
```

The crates.io package name is also `tidas`; `cargo install tidas` installs this
single executable. The source-install channel does not add another command or
legacy alias.

Pre-cutover executable names are not aliases. All seven commands dispatch
directly to Rust domain crates. `unavailable` (69) remains a
reserved stable exit class for a known future Rust capability that is exposed
before implementation.

The repository also builds one internal, unpublished binary,
`tidas-asset-lock`, for executable-asset and public-specification maintenance.
It is not part of the product surface: the published `tidas` executable and its
seven commands are unaffected by it. Its `check`/`write` actions own the paired
schema lock and the complete executable-asset byte lock; `spec-check` proves the
generated public-specification copy still matches its pinned candidate without
writing anything; and `spec-import` replaces exactly that public subset from a
qualified candidate archive supplied explicitly with `--archive`. No action
downloads a candidate from a mutable branch, and none of them is reachable
through `tidas`.

## Adapter boundary

Package `tidas` under `crates/tidas-cli` owns parsing, configuration selection, invocation context,
process cancellation wiring, output routing, help, and completions. It does
not own conversion, import, export, validation, release, or ruleset domain
logic. Functional commands receive a cancellation token, an explicit memory
budget, a bounded queue capacity, and typed inputs before calling reusable
domain crates.

## Configuration precedence

The deterministic precedence order is:

1. explicit command-line option
2. matching `TIDAS_*` environment variable
3. documented built-in default

The runtime never searches the current directory or a home directory for an
implicit configuration file. `--config <PATH>` overrides `TIDAS_CONFIG`.
Configuration selection is recorded in `tidas.invocation-context.v1`.

| Option | Environment | Default |
| --- | --- | --- |
| `--config <PATH>` | `TIDAS_CONFIG` | none |
| `--log-level <LEVEL>` | `TIDAS_LOG` | `warn` |
| `--progress <MODE>` | `TIDAS_PROGRESS` | `auto` |
| `--memory-budget-mib <MIB>` | `TIDAS_MEMORY_BUDGET_MIB` | `512` |
| `--queue-capacity <COUNT>` | `TIDAS_QUEUE_CAPACITY` | `256` |

Export additionally uses:

| Option | Environment | Default |
| --- | --- | --- |
| Database URL | `TIDAS_DATABASE_URL` | required |
| `--external-docs-bucket <BUCKET>` | `TIDAS_S3_BUCKET` | none |
| `--s3-region <REGION>` | `TIDAS_S3_REGION` | `us-east-1` |
| `--s3-endpoint <URL>` | `TIDAS_S3_ENDPOINT` | none |
| `--s3-prefix <PREFIX>` | `TIDAS_S3_PREFIX` | none |

Database and object-storage secrets are environment-only. The S3 access key,
secret key, and optional session token use `TIDAS_S3_ACCESS_KEY_ID`,
`TIDAS_S3_SECRET_ACCESS_KEY`, and `TIDAS_S3_SESSION_TOKEN`. Their values are
never serialized or included in diagnostics.

Zero memory budgets and queue capacities are usage errors.

## Streams and files

- stdin is used only when a future functional command explicitly receives `-`
  in a documented input option; it is never selected implicitly.
- stdout contains exactly one human report, one canonical JSON report, or one
  completion script.
- logs, progress, diagnostics outside a completed report, and file-write
  confirmations use stderr.
- `--report <PATH>` writes the complete report to a temporary sibling and then
  renames it into place; stdout remains empty.
- command-owned large artifacts use command-specific `--output` options,
  so the global report path intentionally uses `--report`.
- JSON mode never mixes logs, banners, or progress with stdout.

`--progress auto` enables progress only for human output attached to a terminal.
Native conversion and package/batch validation emit bounded start, periodic
file/document counts, and completion updates on stderr. `--progress always`
enables the same updates in JSON mode without contaminating stdout;
`--progress never` disables them.

## Machine contracts

`tidas.operation-report.v1` is the F3 envelope for every completed command
dispatch. Its optional `invocation` member is
`tidas.invocation-context.v1` and records:

- configuration source and selected path
- log and progress policy
- resolved progress enablement
- memory budget in bytes
- bounded queue capacity
- explicit-path-or-dash input policy
- report and diagnostic destinations

The report envelope, command names, diagnostics, artifacts, completeness, and
exit classes are F3 versioned contracts. The ordered `summary` object remains
an F2 extension point while individual domain slices are still discovering
their stable projections. Fields may be added compatibly; removal or semantic
change requires a new schema version.

Canonical JSON is UTF-8, LF-terminated, deterministic for identical inputs,
and contains no implicit timestamps, locale values, or unordered collections.

## Native conversion surface

Bidirectional package conversion uses:

```bash
tidas convert <INPUT_DIR> --output <OUTPUT_DIR> --to ilcd --format json
tidas convert <INPUT_DIR> --output <OUTPUT_DIR> --to tidas --format json
```

The command never infers direction. It mirrors input under `OUTPUT_DIR/data`,
copies non-domain package metadata, materializes the locked target assets, and
publishes the whole directory atomically. TIDAS-to-ILCD conversion orders known
dataset members from the locked TIDAS schema catalog before XML serialization;
source JSON object member order therefore cannot change XSD validity. Symlinks,
malformed JSON/XML, multiple unknown roots, XML 1.0-invalid text, and malformed
envelope sidecars are data issues; nested output is a usage error; missing
paths and commit failures use the I/O class; cancellation uses 130.

The operation report summary contains one `conversion` member conforming to
`tidas.conversion-report.v1`. Its artifact is the output directory with total
bytes and a cross-platform tree SHA-256. A deterministic
`.tidas-envelope.json` sidecar preserves top-level TIDAS extension fields that
cannot appear beside the single eILCD XML root; reverse conversion consumes
and merges it. `.tidas-recovery.json` preserves source fragments changed by the
semantic eILCD projection; reverse conversion applies it and verifies the
source semantic hash. The report next action gives the exact `tidas validate
OUTPUT/data --input-format ...` command.

## Native import surface

External-format import uses:

```bash
tidas import <INPUT> --output <OUTPUT_DIR> --format json
tidas import <INPUT> --output <OUTPUT_DIR> \
  --from-format openlca-jsonld --target both \
  --write-mapping --format json
```

Supported `--from-format` values are `ecospold1`, `ecospold2`, `simapro-csv`,
`openlca-jsonld`, `openlca-process-xlsx`, and `ilcd`. Without that option,
bounded signature detection selects the format or returns a data issue for
unsupported/ambiguous input. `.zolca` is explicitly rejected. `--target`
defaults to `tidas`; `ilcd` and `both` request the validated ILCD bridge.
Per-process dependency bundles are written by default and
`--no-process-bundles` disables them. `--write-mapping` enables deterministic
`mapping.csv.gz`; `--max-entry-mib` limits each source entry.

Flow import preflight runs before package publication. Elementary Flow names
may contain only source-backed `baseName`; Product, Waste, and Other Flow names
must also have source-backed `treatmentStandardsRoutes` and
`mixAndLocationTypes`. Missing or placeholder facts return a data issue naming
the source object and canonical field, and no output directory is published.
Unmatched elementary taxonomy paths publish through the documented
air-unspecified fallback and add `elementary_taxonomy_fallback` to
`issues.jsonl`.

The operation report summary contains one `import` member conforming to
`tidas.import-execution-report.v1`. Artifacts carry directory/file hashes and
byte counts, and next actions give exact native `tidas validate` commands for
the published targets. Source data findings, malformed input, and `.zolca`
return `data-issues`; nested output and invalid limits are usage errors;
required path/publication failures use I/O; cancellation uses 130.
`--fail-on-warning` converts an otherwise successful import with warnings to
`data-issues` without discarding the published, validated artifacts.

Adapters write to a disk-backed canonical store; exchanges and issues stream
to bounded spools. Requested TIDAS/ILCD outputs, process bundles, mapping CSV,
and reports are assembled in a sibling staging directory and become visible
only through one atomic commit.

## Native export surface

Database export uses:

```bash
TIDAS_DATABASE_URL='postgresql://…' \
  tidas export --output <PACKAGE.zip> --skip-external-docs --format json

TIDAS_DATABASE_URL='postgresql://…' \
TIDAS_S3_ACCESS_KEY_ID='…' \
TIDAS_S3_SECRET_ACCESS_KEY='…' \
  tidas export --output <PACKAGE.zip> \
  --external-docs-bucket <BUCKET> --s3-endpoint <URL> --format json
```

`--target tidas` is the default; `--target ilcd` serializes database JSON as
eILCD XML. The database is read through one repeatable-read, read-only
snapshot. Only `state_code = 100` category records are exported. TIDAS output
keeps the lexicographically latest fixed-width version per dataset, rewrites
versioned references to that winner, and removes preceding-version references
when multiple exported versions exist.

The database producer and serializer communicate through the global bounded
queue. Object bodies stream chunk by chunk under the shared memory budget.
Object keys and database-derived paths must remain safe relative paths.
Members are sorted and carry fixed ZIP timestamps, compression, and Unix mode
metadata. Publication uses a sibling temporary file and rollback-capable
atomic replacement.

The operation report summary contains one `export` member conforming to
`tidas.export-report.v1`, including record/document counts, normalization
counts, archive bytes/hash, and peak accounted memory. Omitting storage
configuration or using `--skip-external-docs` completes successfully with an
`external_documents_skipped` diagnostic. Missing storage credentials are
usage errors; unsafe source data is `data-issues`; database, storage, ZIP, and
publication failures use I/O; cancellation uses 130.

## Native release surface

Release control uses native subcommands under the single product executable:

```bash
tidas release build-packages \
  --tidas-dir <CANONICAL_DIR> \
  --dataset-index <CANONICAL_INDEX.json> \
  --output-dir <RELEASE_DIR> \
  --format json

tidas release validate-closure \
  --input-dir <CANONICAL_DIR> \
  --dataset-index <CANONICAL_INDEX.json> \
  --profile unit-process-full-closure.v1 \
  --format json

tidas release convert-ilcd --input-dir <CANONICAL_DIR> \
  --output-dir <ILCD_DIR> --format json
tidas release validate-tidas --input-dir <CANONICAL_DIR> --format json
tidas release validate-ilcd --input-dir <ILCD_DIR> --format json
tidas release semantic-roundtrip \
  --tidas-dir <CANONICAL_DIR> --ilcd-dir <ILCD_DIR> --format json
```

The domain consumes a finalized
`tiangong.release.canonical-dataset-index.v1`; it does not assign UUIDs or
versions. Exact references require UUID and version. The two fixed profiles
are `unit-process-full-closure.v1` and
`standalone-lifecyclemodel-result-full-closure.v1`, and the standalone closure
must contain the complete unit closure.

Exact closure follows references required to interpret the packaged datasets.
`referenceToPrecedingDataSetVersion` is retained in the dataset as lineage
metadata, but it does not pull historical dataset versions into package
closure and is not included in the closure report's `reference_count`.
Functional and support references remain fail-closed when their exact target
UUID and version are absent.

`build-packages` is the end-to-end product action. Before any output becomes
visible it runs native TIDAS validation, exact closure, schema-ordered eILCD
derivation, native eILCD validation, normalized semantic round-trip, and the
profile-containment proof. Success atomically replaces `RELEASE_DIR` with
exactly four stored ZIPs: TIDAS and eILCD for each profile. Members are sorted
and use the DOS epoch timestamp and Unix `0644` mode. Failure or cancellation
preserves an existing output directory.

The operation summary contains one `release` member conforming to
`tidas.release-report.v1`. Full result sets are represented by deterministic
counts and hashes; inline closure keys and mismatches are capped at 256 and
carry explicit truncation flags. Package artifacts include final paths,
media type, bytes, member counts, and SHA-256 values. Malformed indexes,
hash drift, missing exact references, validation findings, and round-trip
mismatches use `data-issues`; any output/input path overlap is usage; path,
publication, and ZIP failures use I/O; cancellation uses 130.

## Native validation and ruleset surfaces

The first production domain path is:

```bash
tidas validate <PACKAGE_DIR> \
  --input-format tidas-json \
  --issues <ISSUES.jsonl> \
  --format json
```

`--input-format` is `tidas-json` by default; `ilcd-xml` selects native
namespace-aware XSD validation. Default TIDAS package validation means native
TIDAS schema and semantic validation plus actual eILCD projection, target XSD
validation, and source-semantic recovery. Therefore success guarantees that
the same input is convertible to XSD-valid eILCD without losing TIDAS-only
information. `--schema-only` runs only the native TIDAS checks and must be
reported as diagnostic evidence, not as complete validity. `--issues` is optional. When present, every complete issue is written
in deterministic order to an atomically persisted
`tidas.validation-issue-event.v1` JSONL artifact. Without it, issues are
counted and discarded after validation so report memory remains bounded.

The complete TIDAS operation report contains `validation`,
`eilcd_projection`, `eilcd_projection_validation`, and `semantic_roundtrip`
members. The weaker schema-only and direct ILCD modes contain `validation`.
Data issues produce `completed-with-issues`
and exit 2. Missing input/spool paths use the I/O exit class; cancellation uses
130. The summary records category/document/issue counts, the locked asset
fingerprint, accounted peak memory, and the optional spool count/bytes/hash.

Worker-facing document batches use:

```bash
tidas validate <BATCH_DIR> \
  --protocol document-validation-batch.v1 \
  --input-manifest <MANIFEST.jsonl> \
  --events <EVENTS.jsonl> \
  --format json
```

The manifest is fully preflighted before evidence is emitted. Data findings
are a successful protocol run (exit 0) whose final event proves counts,
logical issue-stream hash, and validation fingerprints. Unsafe paths,
symlinks, missing files, hash drift, or malformed protocol input fail without
a final event. `tidas validate --describe --format json` returns the supported
protocol/profile and engine/asset handshake.

Schema issue diagnostics never serialize the complete rejected JSON instance.
Their human message is capped at 16 KiB and uses an instance placeholder;
structured context carries the schema keyword/path, instance type, bounded
scalar preview or collection size, and SHA-256/original-byte metadata whenever
text is truncated. This keeps each Worker-facing JSONL issue below the 1 MiB
protocol frame ceiling without dropping the issue, changing its ordinal, or
breaking the final logical issue-stream hash.

`tidas ruleset --format json` validates and returns the packaged methodology
catalog. `tidas ruleset --id <RULESET_ID> --format json` returns its ordered
rules; unknown ids use the usage exit class.

## Exit classes

| Class | Code | Meaning |
| --- | ---: | --- |
| `success` | 0 | operation completed successfully |
| `data-issues` | 2 | operation completed and found domain data issues |
| `usage` | 64 | command syntax or option value is invalid |
| `unavailable` | 69 | known Rust function is not yet available |
| `internal` | 70 | invariant, setup, or internal serialization failed |
| `io` | 74 | report or required I/O failed |
| `cancelled` | 130 | shared cancellation token stopped the operation |

Clap usage/help text is written to stderr for malformed invocation. Successful
help, version flags, and completion generation exit zero.

## Validation contract

Public CLI changes must prove:

- exactly seven top-level product commands and no legacy aliases
- help for the root and every product command
- deterministic completions for Bash, Elvish, Fish, PowerShell, and Zsh
- configuration precedence and invocation-context fields
- clean stdout for JSON, report-file, and completion modes
- deterministic repeated JSON
- all affected exit classes
- migration parity fixtures whose semantics are sourced from the frozen Python
  oracle without preserving legacy command names or flag layouts
- Rust 1.98.1 fmt, clippy, tests, and the four-platform CI matrix
