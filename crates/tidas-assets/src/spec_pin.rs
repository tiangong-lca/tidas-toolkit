//! Qualified public-specification pin, manifest parsing, and drift detection.
//!
//! The public TIDAS specification is produced by `tiangong-lca/tidas-spec` and
//! consumed here as one immutable candidate archive. The identity of that
//! archive is recorded as Rust constants so it cannot drift as data: the
//! committed `assets/spec/spec-manifest.json`, the 39 public assets it binds,
//! and the archive itself are all checked against these constants.
//!
//! Nothing in this module mutates the repository. Import lives in
//! [`crate::spec_import`] and consumes the validation results produced here.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Component, Path};

use serde::Deserialize;
use serde_json::Value;

use crate::{AssetError, sha256_hex};

/// npm package name of the public specification.
pub const SPEC_PACKAGE_NAME: &str = "@tiangong-lca/tidas-spec";
/// Candidate package version this repository is pinned to.
pub const SPEC_VERSION: &str = "0.2.0";
/// Canonical repository of the public specification package.
///
/// This is provenance, not an enforceable claim: the specification repository
/// is not read at build time, so the binding identity this repository enforces
/// is [`SPEC_ARCHIVE_SHA256`] together with [`SPEC_MANIFEST_SHA256`]. The
/// revision below is the reviewed specification-repository commit those digests
/// were qualified at, and it is recorded so an upgrade is an explicit event.
pub const SPEC_REPOSITORY: &str = "https://github.com/tiangong-lca/tidas-spec";
/// Reviewed specification-repository revision the qualified archive came from.
pub const SPEC_REVISION: &str = "58dc72f5cb2d203a00388fec71d31091911f7dde";
/// Tools repository the candidate extracted its public assets from.
///
/// Unlike the specification-repository revision, this one travels inside the
/// candidate and is re-derived on every check: the candidate's shipped
/// manifest, its import manifest, and its reviewed baseline must all name it.
pub const SPEC_IMPORTED_SOURCE_REPOSITORY: &str = "https://github.com/tiangong-lca/tidas-toolkit";
/// Tools commit the candidate's public assets were extracted from.
pub const SPEC_IMPORTED_SOURCE_COMMIT: &str = "9c0d8b1c8ceb1841074f5bc6de5fbb7fcc9318f5";
/// Owner of the assets the candidate imports from the tools repository.
///
/// The candidate keeps two origins strictly apart: 34 public assets retain the
/// `tidas-toolkit` origin they were extracted from, while seven authored assets
/// and five package-metadata files carry [`SPEC_PACKAGE_METADATA_ORIGIN`]. Five
/// authored assets belong to the 39-file public runtime subset; the other two
/// and all package metadata are evidence/bookkeeping and are not copied into
/// the runtime asset tree.
pub const SPEC_IMPORTED_ORIGIN: &str = "tidas-toolkit";
/// Owner of the candidate's own package metadata.
pub const SPEC_PACKAGE_METADATA_ORIGIN: &str = "tidas-spec";
/// Canonical archive file name.
pub const SPEC_ARCHIVE_FILE: &str = "tiangong-lca-tidas-spec-0.2.0.tgz";
/// SHA-256 of the qualified candidate archive.
pub const SPEC_ARCHIVE_SHA256: &str =
    "45da9de790ffcdadd1984ffc3544c67a1503c0947470a629e8252aed19100228";
/// SHA-256 of the candidate's own `spec-manifest.json`.
pub const SPEC_MANIFEST_SHA256: &str =
    "4677b9cf864326be9d430bf9760c754c4c0c1905d90e62c161655a159fd758c7";
/// Public specification assets imported into this repository.
pub const SPEC_IMPORTED_FILE_COUNT: usize = 34;
/// Public assets authored or derived in the specification repository.
pub const SPEC_AUTHORED_FILE_COUNT: usize = 7;
/// Complete public runtime subset copied into this repository.
pub const SPEC_PUBLIC_FILE_COUNT: usize = 39;
/// Candidate files that belong to the specification repository itself.
pub const SPEC_PACKAGE_METADATA_FILE_COUNT: usize = 5;
/// Every file carried by the candidate archive, including its manifest.
pub const SPEC_PACKAGE_FILE_COUNT: usize = 47;
/// Schemas per language (English and Chinese are paired).
pub const SPEC_SCHEMAS_PER_LANGUAGE: usize = 18;

/// Pinned candidate manifest, committed verbatim for provenance.
///
/// These two records deliberately sit beside `assets/asset-lock.v1.json` rather
/// than inside `assets/tidas`. The executable asset tree is exactly
/// [`crate::SOURCE_ROOTS`] (`assets/eilcd`, `assets/tidas`,
/// `assets/validation_indexes`) and every file under it is covered by the full
/// executable asset lock. Provenance is not an executable asset: putting it in
/// that tree would change the runtime asset set, the lock, and the derived
/// runtime fingerprint, which is a separately reviewed change this pin does not
/// have authority to make.
pub const SPEC_MANIFEST_PATH: &str = "assets/spec/spec-manifest.json";
/// Provenance record produced by a successful import.
pub const SPEC_PROVENANCE_PATH: &str = "assets/spec/spec-pin.json";
/// Disposable staging root, outside the executable asset tree.
///
/// Each import creates one uniquely named directory under it and removes only
/// that directory, so the root itself and any other writer's directory survive.
pub const SPEC_STAGING_DIR: &str = "assets/spec/.staging";
/// Exclusive import lock, outside the executable asset tree.
///
/// It serializes the whole mutation window of an import — prior-state capture,
/// every replacement, and any rollback — so overlapping imports cannot undo
/// each other. It is a transient lock file, not an asset.
pub const SPEC_IMPORT_LOCK: &str = "assets/spec/.import.lock";
/// Asset root the imported files live under.
pub const SPEC_ASSET_ROOT: &str = "assets/tidas";
/// Directory inside the archive that holds the package payload.
pub const SPEC_ARCHIVE_PREFIX: &str = "package";
/// Schema version of the provenance record.
pub const SPEC_PROVENANCE_SCHEMA_V1: &str = "tidas.spec-pin.v1";

/// Upper bound on bytes accepted from one archive or one public asset.
pub const SPEC_ARCHIVE_MAX_BYTES: u64 = 64 * 1024 * 1024;
/// Upper bound on the number of members accepted from the candidate archive.
pub const SPEC_ARCHIVE_MAX_MEMBERS: usize = 4096;

/// The two shared methodology documents that are part of the public subset.
pub const SPEC_METHODOLOGY_PATHS: [&str; 2] = [
    "assets/tidas/methodologies/tidas_flows.yaml",
    "assets/tidas/methodologies/tidas_processes.yaml",
];

/// Explicit identity of one public-specification candidate.
///
/// Production code always uses [`SpecPin::qualified_candidate`]. Tests build
/// other values to exercise the import/check machinery without the qualified
/// bytes, which is why the identity is a value rather than a bare constant
/// reference.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecPin {
    pub package_name: String,
    pub version: String,
    /// Specification repository, recorded as provenance.
    pub repository: String,
    /// Reviewed specification-repository revision, recorded as provenance.
    pub revision: String,
    /// Tools repository the candidate's assets were extracted from.
    pub imported_source_repository: String,
    /// Tools commit the candidate's assets were extracted from.
    pub imported_source_commit: String,
    pub archive_file: String,
    pub archive_sha256: String,
    pub manifest_sha256: String,
    pub imported_file_count: usize,
    pub authored_file_count: usize,
    pub public_file_count: usize,
    pub package_file_count: usize,
    pub schemas_per_language: usize,
}

impl SpecPin {
    /// The one reviewed and qualified public-specification candidate.
    #[must_use]
    pub fn qualified_candidate() -> Self {
        Self {
            package_name: SPEC_PACKAGE_NAME.to_owned(),
            version: SPEC_VERSION.to_owned(),
            repository: SPEC_REPOSITORY.to_owned(),
            revision: SPEC_REVISION.to_owned(),
            imported_source_repository: SPEC_IMPORTED_SOURCE_REPOSITORY.to_owned(),
            imported_source_commit: SPEC_IMPORTED_SOURCE_COMMIT.to_owned(),
            archive_file: SPEC_ARCHIVE_FILE.to_owned(),
            archive_sha256: SPEC_ARCHIVE_SHA256.to_owned(),
            manifest_sha256: SPEC_MANIFEST_SHA256.to_owned(),
            imported_file_count: SPEC_IMPORTED_FILE_COUNT,
            authored_file_count: SPEC_AUTHORED_FILE_COUNT,
            public_file_count: SPEC_PUBLIC_FILE_COUNT,
            package_file_count: SPEC_PACKAGE_FILE_COUNT,
            schemas_per_language: SPEC_SCHEMAS_PER_LANGUAGE,
        }
    }
}

/// One file bound by the candidate manifest.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecManifestFile {
    pub path: String,
    pub sha256: String,
    pub content_sha256: String,
    pub origin: String,
    #[serde(default)]
    pub source: Option<SpecFileSource>,
}

/// Source-of-truth record for one imported asset.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecFileSource {
    pub path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecManifestPackage {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecManifestLicense {
    pub path: String,
    pub sha256: String,
    pub notice: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecManifestSource {
    pub repository: String,
    pub repository_id: String,
    pub commit: String,
    pub commit_ref: String,
    pub license: SpecManifestLicense,
    pub excluded_paths: Vec<String>,
    pub owned_metadata_origin: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecManifestCounts {
    pub schemas_per_language: usize,
    pub languages: Vec<String>,
    pub methodologies: usize,
    pub imported_assets: usize,
    #[serde(default)]
    pub authored_assets: usize,
    pub package_metadata: usize,
    pub files: usize,
    pub packaged_files: usize,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecManifestAggregates {
    pub files_sha256: String,
    pub files_content_sha256: String,
}

/// Parsed `spec-manifest.json`.
///
/// The analysis block (`findings`) is intentionally not modelled: it is
/// descriptive, and the whole document is bound byte-for-byte by
/// [`SpecPin::manifest_sha256`].
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecManifest {
    pub manifest_version: u32,
    pub package: SpecManifestPackage,
    pub spec_version: String,
    pub source: SpecManifestSource,
    pub counts: SpecManifestCounts,
    pub asset_root: String,
    pub lock: String,
    pub files: Vec<SpecManifestFile>,
    pub aggregates: SpecManifestAggregates,
}

/// Parsed `source-import.yaml` from the candidate.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecSourceImport {
    pub version: u32,
    pub source_repo_id: String,
    pub source_repo_canonical_url: String,
    pub source_commit: String,
    pub source_commit_ref: String,
    pub source_license_path: String,
    pub source_license_sha256: String,
    pub source_license_notice: String,
    pub excluded_source_paths: Vec<String>,
    pub files: Vec<SpecSourceImportFile>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecSourceImportFile {
    pub source_path: String,
    pub package_path: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecReviewedBaselineSource {
    pub repository: String,
    pub repository_id: String,
    pub commit: String,
    pub commit_ref: String,
    pub license_path: String,
    pub license_sha256: String,
}

/// Parsed `reviewed-baseline.json` from the candidate.
#[derive(Clone, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SpecReviewedBaseline {
    pub reviewed_baseline_version: u32,
    pub spec_version: String,
    pub source: SpecReviewedBaselineSource,
    pub file_count: usize,
    pub source_files_sha256: String,
    pub package_files_sha256: String,
}

/// Validated view of the candidate, used by both check and import.
#[derive(Debug)]
pub struct SpecCandidate {
    pub manifest: SpecManifest,
    pub manifest_bytes: Vec<u8>,
    /// Public runtime assets in repository-relative path order.
    pub assets: BTreeMap<String, Vec<u8>>,
    /// Digest of the archive the candidate was read from, when one was read.
    pub archive_sha256: Option<String>,
    /// Canonical digest of the complete public runtime subset.
    pub public_assets_sha256: String,
}

impl SpecCandidate {
    /// Public runtime asset paths in deterministic order.
    #[must_use]
    pub fn asset_paths(&self) -> Vec<&str> {
        self.assets.keys().map(String::as_str).collect()
    }
}

/// Outcome of a repository-side drift check.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SpecCheckSummary {
    pub version: String,
    /// Reviewed specification-repository revision recorded by the pin.
    pub spec_revision: String,
    /// Tools commit the imported assets were extracted from.
    pub imported_source_commit: String,
    pub asset_count: usize,
    pub archive_verified: bool,
}

/// Read and digest one file, rejecting anything that is not a regular file.
pub fn read_regular_file(path: &Path) -> Result<Vec<u8>, AssetError> {
    match existing_regular_file(path)? {
        Some(bytes) => Ok(bytes),
        None => Err(AssetError::SpecInvalid(format!(
            "{} does not exist",
            path.display()
        ))),
    }
}

/// Read one file when it exists, rejecting symlinks and non-regular entries.
///
/// Returns `None` only when the path genuinely does not exist, so callers can
/// distinguish "absent" from "present but unusable" without parsing messages.
pub fn existing_regular_file(path: &Path) -> Result<Option<Vec<u8>>, AssetError> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(AssetError::SpecInvalid(format!(
                "cannot read {}: {error}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() {
        return Err(AssetError::SpecInvalid(format!(
            "{} is a symbolic link; the public specification copy must be plain files",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(AssetError::SpecInvalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    Ok(Some(fs::read(path)?))
}

/// Reject unsafe or non-portable repository-relative paths.
///
/// Imported and bound paths are joined onto a repository root, so an absolute
/// path, a parent traversal, a Windows prefix, or a non-ASCII name fails closed
/// rather than resolving outside the intended tree. ASCII is required because
/// the candidate's own aggregates are computed with UTF-16 key ordering.
pub fn validate_public_path(path: &str) -> Result<(), AssetError> {
    if path.is_empty() {
        return Err(AssetError::SpecInvalid("empty public path".to_owned()));
    }
    if !path.is_ascii() {
        return Err(AssetError::SpecInvalid(format!(
            "public path is not ASCII: {path}"
        )));
    }
    if path.contains('\\') {
        return Err(AssetError::SpecInvalid(format!(
            "public path is not portable: {path}"
        )));
    }
    let parsed = Path::new(path);
    if parsed.is_absolute()
        || parsed
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(AssetError::SpecInvalid(format!(
            "unsafe public path: {path}"
        )));
    }
    Ok(())
}

/// Canonical JSON exactly as the specification repository computes it.
///
/// Keys are sorted, separators carry no whitespace, and numbers use their
/// shortest round-trip form — matching `canonicalJson` in the specification
/// repository's `scripts/spec/lib/core.mjs`.
pub fn canonical_json(value: &Value) -> Result<String, AssetError> {
    let mut output = String::new();
    write_canonical_json(value, &mut output)?;
    Ok(output)
}

fn write_canonical_json(value: &Value, output: &mut String) -> Result<(), AssetError> {
    match value {
        Value::Null => output.push_str("null"),
        Value::Bool(true) => output.push_str("true"),
        Value::Bool(false) => output.push_str("false"),
        Value::Number(number) => {
            output.push_str(&number.to_string());
        }
        Value::String(text) => output.push_str(&serde_json::to_string(text)?),
        Value::Array(items) => {
            output.push('[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                write_canonical_json(item, output)?;
            }
            output.push(']');
        }
        Value::Object(object) => {
            let mut keys: Vec<&str> = object.keys().map(String::as_str).collect();
            keys.sort_unstable();
            output.push('{');
            for (index, key) in keys.into_iter().enumerate() {
                if index > 0 {
                    output.push(',');
                }
                output.push_str(&serde_json::to_string(key)?);
                output.push(':');
                let child = object
                    .get(key)
                    .expect("key came from the same object in this scope");
                write_canonical_json(child, output)?;
            }
            output.push('}');
        }
    }
    Ok(())
}

/// SHA-256 of the canonical JSON encoding of a value.
pub fn hash_canonical_json(value: &Value) -> Result<String, AssetError> {
    Ok(sha256_hex(canonical_json(value)?.as_bytes()))
}

/// Parse the candidate manifest and confirm it describes the pinned identity.
pub fn parse_manifest(bytes: &[u8], pin: &SpecPin) -> Result<SpecManifest, AssetError> {
    let manifest: SpecManifest = serde_json::from_slice(bytes).map_err(|error| {
        AssetError::SpecInvalid(format!("spec-manifest.json is malformed: {error}"))
    })?;
    validate_manifest(&manifest, pin)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &SpecManifest, pin: &SpecPin) -> Result<(), AssetError> {
    if manifest.manifest_version != 1 {
        return Err(AssetError::SpecInvalid(format!(
            "unsupported spec manifest version {}",
            manifest.manifest_version
        )));
    }
    if manifest.package.name != pin.package_name {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest package {} is not the pinned {}",
            manifest.package.name, pin.package_name
        )));
    }
    if manifest.package.version != pin.version || manifest.spec_version != pin.version {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest version {} / {} is not the pinned {}",
            manifest.package.version, manifest.spec_version, pin.version
        )));
    }
    if manifest.source.owned_metadata_origin != SPEC_PACKAGE_METADATA_ORIGIN {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest owns its metadata as {}; the pin expects {SPEC_PACKAGE_METADATA_ORIGIN}",
            manifest.source.owned_metadata_origin
        )));
    }
    if manifest.source.repository != pin.imported_source_repository
        || manifest.source.repository_id != SPEC_IMPORTED_ORIGIN
    {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest extracts its assets from {} / {}; the pin expects {} / {SPEC_IMPORTED_ORIGIN}",
            manifest.source.repository,
            manifest.source.repository_id,
            pin.imported_source_repository
        )));
    }
    if manifest.source.commit != pin.imported_source_commit {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest extracts its assets from tools commit {}; the pin expects {}",
            manifest.source.commit, pin.imported_source_commit
        )));
    }
    if manifest.asset_root != SPEC_ASSET_ROOT {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest asset root {} is not {SPEC_ASSET_ROOT}",
            manifest.asset_root
        )));
    }
    validate_manifest_counts(manifest, pin)?;
    validate_manifest_files(&manifest.files)
}

fn validate_manifest_counts(manifest: &SpecManifest, pin: &SpecPin) -> Result<(), AssetError> {
    if manifest.counts.files != manifest.files.len() {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest counts.files {} disagrees with {} bound files",
            manifest.counts.files,
            manifest.files.len()
        )));
    }
    if manifest.counts.imported_assets != pin.imported_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest imports {} public assets; the pin declares {}",
            manifest.counts.imported_assets, pin.imported_file_count
        )));
    }
    if manifest.counts.schemas_per_language != pin.schemas_per_language {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest declares {} schemas per language; the pin declares {}",
            manifest.counts.schemas_per_language, pin.schemas_per_language
        )));
    }
    if manifest.counts.package_metadata != SPEC_PACKAGE_METADATA_FILE_COUNT {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest declares {} metadata files; the candidate carries {SPEC_PACKAGE_METADATA_FILE_COUNT}",
            manifest.counts.package_metadata
        )));
    }
    if manifest.counts.authored_assets != pin.authored_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest declares {} authored assets; the pin declares {}",
            manifest.counts.authored_assets, pin.authored_file_count
        )));
    }
    if manifest.counts.imported_assets
        + manifest.counts.authored_assets
        + manifest.counts.package_metadata
        != manifest.counts.files
    {
        return Err(AssetError::SpecInvalid(
            "spec manifest counts do not partition its bound files".to_owned(),
        ));
    }
    if manifest.counts.packaged_files != pin.package_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest declares {} packaged files; the pin declares {}",
            manifest.counts.packaged_files, pin.package_file_count
        )));
    }
    let mut languages = manifest.counts.languages.clone();
    languages.sort();
    if languages != ["en", "zh"] {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest languages {languages:?} are not the paired en/zh sets"
        )));
    }
    if manifest.counts.methodologies != SPEC_METHODOLOGY_PATHS.len() {
        return Err(AssetError::SpecInvalid(format!(
            "spec manifest declares {} methodology documents; the public subset has {}",
            manifest.counts.methodologies,
            SPEC_METHODOLOGY_PATHS.len()
        )));
    }
    Ok(())
}

/// Every bound file must have a safe path, a known origin, and valid digests.
fn validate_manifest_files(files: &[SpecManifestFile]) -> Result<(), AssetError> {
    let mut seen = BTreeSet::new();
    for file in files {
        validate_public_path(&file.path)?;
        if file.origin != SPEC_IMPORTED_ORIGIN && file.origin != SPEC_PACKAGE_METADATA_ORIGIN {
            return Err(AssetError::SpecInvalid(format!(
                "{} declares an unrecognised origin {}",
                file.path, file.origin
            )));
        }
        if file.origin == SPEC_PACKAGE_METADATA_ORIGIN && file.source.is_some() {
            return Err(AssetError::SpecInvalid(format!(
                "{} is candidate metadata but declares an imported-asset source record",
                file.path
            )));
        }
        if !seen.insert(file.path.as_str()) {
            return Err(AssetError::SpecInvalid(format!(
                "spec manifest binds {} more than once",
                file.path
            )));
        }
        for digest in [&file.sha256, &file.content_sha256] {
            if digest.len() != 64 || !digest.bytes().all(|byte| byte.is_ascii_hexdigit()) {
                return Err(AssetError::SpecInvalid(format!(
                    "spec manifest digest for {} is not a SHA-256 value",
                    file.path
                )));
            }
        }
    }
    Ok(())
}

/// The public subset the pin expects, derived from the manifest's own origins.
pub fn imported_paths(manifest: &SpecManifest) -> Result<BTreeSet<String>, AssetError> {
    let mut imported = BTreeSet::new();
    for file in &manifest.files {
        if file.origin != SPEC_IMPORTED_ORIGIN {
            continue;
        }
        let source = file.source.as_ref().ok_or_else(|| {
            AssetError::SpecInvalid(format!(
                "{} is declared as an imported asset without a source record",
                file.path
            ))
        })?;
        if source.path != file.path {
            return Err(AssetError::SpecInvalid(format!(
                "{} records source path {}",
                file.path, source.path
            )));
        }
        if source.sha256 != file.sha256 {
            return Err(AssetError::SpecInvalid(format!(
                "{} records source digest that differs from its shipped digest",
                file.path
            )));
        }
        imported.insert(file.path.clone());
    }
    Ok(imported)
}

/// Confirm the imported and authored runtime subset is exactly the reviewed public specification.
///
/// The public subset is 36 schemas (18 English plus 18 Chinese), 2 shared
/// methodology documents, and the paired `schema.lock.json`. Anything else in
/// either direction fails closed.
pub fn validate_public_subset(
    manifest: &SpecManifest,
    pin: &SpecPin,
) -> Result<BTreeSet<String>, AssetError> {
    let imported = imported_paths(manifest)?;
    if imported.len() != pin.imported_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate imports {} public assets; the pin declares {}",
            imported.len(),
            pin.imported_file_count
        )));
    }

    let public: BTreeSet<String> = manifest
        .files
        .iter()
        .map(|file| file.path.clone())
        .filter(|path| {
            path.starts_with("assets/tidas/schemas/")
                || path.starts_with("assets/tidas/schemas_zh/")
                || SPEC_METHODOLOGY_PATHS.contains(&path.as_str())
                || path == "assets/tidas/schema.lock.json"
        })
        .collect();
    if public.len() != pin.public_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate carries {} public assets; the pin declares {}",
            public.len(),
            pin.public_file_count
        )));
    }
    let mut en = 0_usize;
    let mut zh = 0_usize;
    for path in &public {
        if path.starts_with("assets/tidas/schemas/") {
            en += 1;
        } else if path.starts_with("assets/tidas/schemas_zh/") {
            zh += 1;
        } else if !SPEC_METHODOLOGY_PATHS.contains(&path.as_str())
            && path != "assets/tidas/schema.lock.json"
        {
            return Err(AssetError::SpecInvalid(format!(
                "unexpected public asset outside the reviewed subset: {path}"
            )));
        }
    }
    if en != pin.schemas_per_language || zh != pin.schemas_per_language {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate imports {en} English and {zh} Chinese schemas; the pin declares {} each",
            pin.schemas_per_language
        )));
    }
    if public.len() != 2 * pin.schemas_per_language + SPEC_METHODOLOGY_PATHS.len() + 1 {
        return Err(AssetError::SpecInvalid(
            "the candidate's public subset is not 2 schema sets, the shared methodologies, and the paired lock"
                .to_owned(),
        ));
    }

    // The retained tools-owned methodologies must never appear in the subset.
    for excluded in [
        "assets/tidas/methodologies/runtime_rulesets.json",
        "assets/tidas/methodologies/runtime_rulesets.schema.json",
        "assets/tidas/methodologies/elementary_flow_taxonomy_extension.v1.json",
    ] {
        if public.contains(excluded) {
            return Err(AssetError::SpecInvalid(format!(
                "{excluded} is tools-owned and must not be imported from the public specification"
            )));
        }
    }
    Ok(public)
}

/// Canonical digest of a set of `path -> sha256` pairs.
pub fn subset_digest(subset: &BTreeMap<String, String>) -> Result<String, AssetError> {
    hash_canonical_json(&Value::Object(
        subset
            .iter()
            .map(|(path, digest)| (path.clone(), Value::String(digest.clone())))
            .collect(),
    ))
}

/// Recompute the candidate's shipped and content aggregates from actual bytes.
fn recompute_two_aggregates(
    shipped: &BTreeMap<String, String>,
    content: &BTreeMap<String, String>,
) -> Result<(String, String), AssetError> {
    let shipped_json = Value::Object(
        shipped
            .iter()
            .map(|(path, digest)| (path.clone(), Value::String(digest.clone())))
            .collect(),
    );
    let content_json = Value::Object(
        content
            .iter()
            .map(|(path, digest)| (path.clone(), Value::String(digest.clone())))
            .collect(),
    );
    Ok((
        hash_canonical_json(&shipped_json)?,
        hash_canonical_json(&content_json)?,
    ))
}

/// Cross-check the manifest, the import manifest, and the reviewed baseline.
///
/// Each record is an independent reviewed input, so a fabricated source
/// identity or a drifted asset inventory cannot be made to pass by editing one
/// of them.
pub fn validate_source_evidence(
    manifest: &SpecManifest,
    source_import: &SpecSourceImport,
    baseline: &SpecReviewedBaseline,
    pin: &SpecPin,
    shipped: &BTreeMap<String, String>,
    content: &BTreeMap<String, String>,
) -> Result<(), AssetError> {
    let imported = check_source_records(manifest, source_import, baseline, pin, shipped)?;
    check_reviewed_digests(
        manifest,
        source_import,
        baseline,
        shipped,
        content,
        &imported,
    )
}

/// Confirm the three source records agree on one reviewed source and subset.
fn check_source_records(
    manifest: &SpecManifest,
    source_import: &SpecSourceImport,
    baseline: &SpecReviewedBaseline,
    pin: &SpecPin,
    shipped: &BTreeMap<String, String>,
) -> Result<BTreeSet<String>, AssetError> {
    if source_import.version != 1 || baseline.reviewed_baseline_version != 1 {
        return Err(AssetError::SpecInvalid(
            "candidate source evidence uses an unsupported record version".to_owned(),
        ));
    }
    if baseline.spec_version != pin.version
        || source_import.source_commit != pin.imported_source_commit
    {
        return Err(AssetError::SpecInvalid(
            "candidate source evidence does not describe the pinned version and extraction commit"
                .to_owned(),
        ));
    }
    if source_import.source_repo_canonical_url != manifest.source.repository
        || source_import.source_repo_id != manifest.source.repository_id
        || source_import.source_commit != manifest.source.commit
        || source_import.source_commit_ref != manifest.source.commit_ref
        || source_import.source_license_path != manifest.source.license.path
        || source_import.source_license_sha256 != manifest.source.license.sha256
        || source_import.source_license_notice != manifest.source.license.notice
    {
        return Err(AssetError::SpecInvalid(
            "the candidate's import manifest and shipped manifest disagree about the reviewed source"
                .to_owned(),
        ));
    }
    if source_import.excluded_source_paths != manifest.source.excluded_paths {
        return Err(AssetError::SpecInvalid(
            "the candidate's import manifest and shipped manifest disagree about excluded paths"
                .to_owned(),
        ));
    }
    if source_import.files.len() != pin.imported_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate's import manifest lists {} files; the pin declares {}",
            source_import.files.len(),
            pin.imported_file_count
        )));
    }

    let mut imported = BTreeSet::new();
    for file in &source_import.files {
        validate_public_path(&file.package_path)?;
        if !imported.insert(file.package_path.clone()) {
            return Err(AssetError::SpecInvalid(format!(
                "the candidate's import manifest lists {} more than once",
                file.package_path
            )));
        }
        let shipped_digest = shipped.get(&file.package_path).ok_or_else(|| {
            AssetError::SpecInvalid(format!(
                "the candidate's import manifest lists {} but ships different bytes",
                file.package_path
            ))
        })?;
        if shipped_digest != &file.sha256 {
            return Err(AssetError::SpecInvalid(format!(
                "{} is shipped as {shipped_digest} but the import manifest declares {}",
                file.package_path, file.sha256
            )));
        }
    }
    if imported != imported_paths(manifest)? {
        return Err(AssetError::SpecInvalid(
            "the candidate's import manifest and shipped manifest bind different public subsets"
                .to_owned(),
        ));
    }

    if baseline.source.repository != manifest.source.repository
        || baseline.source.repository_id != manifest.source.repository_id
        || baseline.source.commit != manifest.source.commit
        || baseline.source.commit_ref != manifest.source.commit_ref
        || baseline.source.license_path != manifest.source.license.path
        || baseline.source.license_sha256 != manifest.source.license.sha256
    {
        return Err(AssetError::SpecInvalid(
            "the candidate's reviewed baseline and shipped manifest disagree about the reviewed source"
                .to_owned(),
        ));
    }
    if baseline.file_count != pin.imported_file_count
        || baseline.file_count != source_import.files.len()
        || baseline.file_count != imported.len()
    {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate's reviewed baseline counts {} files, which disagrees with the reviewed subset",
            baseline.file_count
        )));
    }
    Ok(imported)
}

/// Re-derive every digest the candidate's reviewed records claim to bind.
fn check_reviewed_digests(
    manifest: &SpecManifest,
    source_import: &SpecSourceImport,
    baseline: &SpecReviewedBaseline,
    shipped: &BTreeMap<String, String>,
    content: &BTreeMap<String, String>,
    imported: &BTreeSet<String>,
) -> Result<(), AssetError> {
    let source_hashes: BTreeMap<&str, &str> = source_import
        .files
        .iter()
        .map(|file| (file.source_path.as_str(), file.sha256.as_str()))
        .collect();
    let source_json = Value::Object(
        source_hashes
            .iter()
            .map(|(path, digest)| ((*path).to_owned(), Value::String((*digest).to_owned())))
            .collect(),
    );
    if hash_canonical_json(&source_json)? != baseline.source_files_sha256 {
        return Err(AssetError::SpecInvalid(
            "the candidate's reviewed baseline does not bind the source digests its import manifest declares"
                .to_owned(),
        ));
    }

    let shipped_subset: BTreeMap<&str, &str> = imported
        .iter()
        .map(|path| {
            (
                path.as_str(),
                shipped
                    .get(path)
                    .expect("subset membership was checked against the shipped map")
                    .as_str(),
            )
        })
        .collect();
    let shipped_json = Value::Object(
        shipped_subset
            .iter()
            .map(|(path, digest)| ((*path).to_owned(), Value::String((*digest).to_owned())))
            .collect(),
    );
    if hash_canonical_json(&shipped_json)? != baseline.package_files_sha256 {
        return Err(AssetError::SpecInvalid(
            "the candidate's shipped public subset is not the set its reviewed baseline approved"
                .to_owned(),
        ));
    }

    let (files_sha256, files_content_sha256) = recompute_two_aggregates(shipped, content)?;
    if files_sha256 != manifest.aggregates.files_sha256
        || files_content_sha256 != manifest.aggregates.files_content_sha256
    {
        return Err(AssetError::SpecInvalid(
            "the candidate's manifest aggregates do not match the bytes it ships".to_owned(),
        ));
    }
    Ok(())
}

/// Validate a candidate whose file bytes are already materialized in memory.
///
/// This is the single validation path shared by the repository check and the
/// archive import, so both bind the same identity.
pub fn validate_candidate(
    pin: &SpecPin,
    manifest_bytes: &[u8],
    source_import_bytes: &[u8],
    baseline_bytes: &[u8],
    all_files: &BTreeMap<String, Vec<u8>>,
) -> Result<SpecCandidate, AssetError> {
    let manifest_digest = sha256_hex(manifest_bytes);
    if manifest_digest != pin.manifest_sha256 {
        return Err(AssetError::SpecManifestDigest {
            expected: pin.manifest_sha256.clone(),
            found: manifest_digest,
        });
    }
    let manifest = parse_manifest(manifest_bytes, pin)?;
    let public = validate_public_subset(&manifest, pin)?;

    if all_files.len() != manifest.files.len()
        || all_files.len() + 1 != manifest.counts.packaged_files
    {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate carries {} files but its manifest binds {} of {} packaged files",
            all_files.len(),
            manifest.files.len(),
            manifest.counts.packaged_files
        )));
    }

    let mut shipped = BTreeMap::new();
    let mut content = BTreeMap::new();
    let mut assets = BTreeMap::new();
    for file in &manifest.files {
        let bytes = all_files.get(&file.path).ok_or_else(|| {
            AssetError::SpecInvalid(format!(
                "the candidate manifest binds {} but the candidate does not carry it",
                file.path
            ))
        })?;
        let digest = sha256_hex(bytes);
        if digest != file.sha256 {
            return Err(AssetError::SpecInvalid(format!(
                "{} is corrupt: the manifest binds {} but the candidate carries {digest}",
                file.path, file.sha256
            )));
        }
        let normalized = normalize_line_endings(bytes);
        let content_digest = sha256_hex(&normalized);
        if content_digest != file.content_sha256 {
            return Err(AssetError::SpecInvalid(format!(
                "{} has content digest {content_digest}; the manifest binds {}",
                file.path, file.content_sha256
            )));
        }
        if public.contains(&file.path) {
            assets.insert(file.path.clone(), bytes.clone());
        }
        shipped.insert(file.path.clone(), digest);
        content.insert(file.path.clone(), content_digest);
    }
    for path in all_files.keys() {
        if !shipped.contains_key(path) {
            return Err(AssetError::SpecInvalid(format!(
                "the candidate carries {path}, which its manifest does not bind"
            )));
        }
    }
    if assets.len() != pin.public_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate yielded {} public assets; the pin declares {}",
            assets.len(),
            pin.public_file_count
        )));
    }

    let source_import: SpecSourceImport =
        noyalib::compat::serde_yaml::from_slice(source_import_bytes).map_err(|error| {
            AssetError::SpecInvalid(format!("source-import.yaml is malformed: {error}"))
        })?;
    let baseline: SpecReviewedBaseline =
        serde_json::from_slice(baseline_bytes).map_err(|error| {
            AssetError::SpecInvalid(format!("reviewed-baseline.json is malformed: {error}"))
        })?;
    validate_source_evidence(
        &manifest,
        &source_import,
        &baseline,
        pin,
        &shipped,
        &content,
    )?;

    Ok(SpecCandidate {
        manifest,
        manifest_bytes: manifest_bytes.to_vec(),
        assets,
        archive_sha256: None,
        public_assets_sha256: String::new(),
    })
}

/// CRLF to LF normalization used by the candidate's content digests.
fn normalize_line_endings(bytes: &[u8]) -> Vec<u8> {
    let mut normalized = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'\r' && bytes.get(index + 1) == Some(&b'\n') {
            normalized.push(b'\n');
            index += 2;
        } else {
            normalized.push(bytes[index]);
            index += 1;
        }
    }
    normalized
}

/// Describe the drift between the pinned manifest and the repository copy.
pub fn detect_drift(
    root: &Path,
    manifest: &SpecManifest,
    imported: &BTreeSet<String>,
) -> Result<Vec<String>, AssetError> {
    let mut drift = Vec::new();
    for file in &manifest.files {
        if !imported.contains(&file.path) {
            continue;
        }
        let absolute = root.join(&file.path);
        let bytes = read_regular_file(&absolute)?;
        let digest = sha256_hex(&bytes);
        if digest != file.sha256 {
            drift.push(format!("{}: {digest}", file.path));
        }
    }
    drift.sort();
    Ok(drift)
}

/// Confirm the repository holds the pinned public copy and nothing else moved.
///
/// This is the check that runs in CI: it binds the committed candidate manifest
/// byte-for-byte, re-derives the public subset, and compares every public asset
/// against the pinned digest. It never writes.
pub fn check_repository_public_copy(
    root: &Path,
    pin: &SpecPin,
    archive: Option<&Path>,
) -> Result<SpecCheckSummary, AssetError> {
    let manifest_path = root.join(SPEC_MANIFEST_PATH);
    crate::spec_import::require_plain_repository_path(root, &manifest_path, SPEC_MANIFEST_PATH)?;
    let manifest_bytes = read_regular_file(&manifest_path)?;
    let manifest_digest = sha256_hex(&manifest_bytes);
    if manifest_digest != pin.manifest_sha256 {
        return Err(AssetError::SpecManifestDigest {
            expected: pin.manifest_sha256.clone(),
            found: manifest_digest,
        });
    }
    let manifest = parse_manifest(&manifest_bytes, pin)?;
    let public = validate_public_subset(&manifest, pin)?;

    // Only the public subset lives in this repository. The candidate's own
    // metadata (its README, licence, package.json, import manifest, and
    // reviewed baseline) is bound by the pinned manifest digest, not copied
    // here, so it is never expected on disk.
    let mut repository_files = BTreeMap::new();
    for file in &manifest.files {
        if !public.contains(&file.path) {
            continue;
        }
        let absolute = root.join(&file.path);
        crate::spec_import::require_plain_repository_path(root, &absolute, &file.path)?;
        let bytes = read_regular_file(&absolute)?;
        let digest = sha256_hex(&bytes);
        if digest != file.sha256 {
            return Err(AssetError::SpecDrift(vec![format!(
                "{}: {digest}",
                file.path
            )]));
        }
        repository_files.insert(file.path.clone(), bytes);
    }
    if repository_files.len() != pin.public_file_count {
        return Err(AssetError::SpecInvalid(format!(
            "the repository holds {} public assets; the pin declares {}",
            repository_files.len(),
            pin.public_file_count
        )));
    }

    // The provenance record must agree with the pin that is actually enforced,
    // so an import that recorded a different candidate cannot be checked in.
    // The provenance record is checked against the digest this check derives
    // from the public bytes it just validated against the pinned manifest, so a
    // forged record — malformed or merely different — is rejected rather than
    // compared with itself.
    let observed_public_assets_sha256 = subset_digest(
        &repository_files
            .iter()
            .map(|(path, bytes)| (path.clone(), sha256_hex(bytes)))
            .collect(),
    )?;
    let provenance_path = root.join(SPEC_PROVENANCE_PATH);
    crate::spec_import::require_plain_repository_path(
        root,
        &provenance_path,
        SPEC_PROVENANCE_PATH,
    )?;
    let provenance_bytes = read_regular_file(&provenance_path)?;
    let provenance: SpecProvenance =
        serde_json::from_slice(&provenance_bytes).map_err(|error| {
            AssetError::SpecInvalid(format!("{SPEC_PROVENANCE_PATH} is malformed: {error}"))
        })?;
    provenance.validate(pin, &observed_public_assets_sha256)?;

    let archive_verified = match archive {
        Some(path) => {
            // Re-derive the whole candidate and compare it with the repository
            // byte-for-byte. This is what makes a manual edit to a generated
            // public copy detectable rather than merely inconsistent.
            let candidate = read_candidate_archive(path, pin)?;
            if candidate.assets.len() != repository_files.len() {
                return Err(AssetError::SpecInvalid(
                    "the candidate archive and the repository disagree about the public subset size"
                        .to_owned(),
                ));
            }
            let mut drift = Vec::new();
            for (asset_path, bytes) in &candidate.assets {
                match repository_files.get(asset_path) {
                    Some(repository) if repository == bytes => {}
                    Some(_) => drift.push(format!(
                        "{asset_path}: the repository copy differs from the candidate archive"
                    )),
                    None => drift.push(format!(
                        "{asset_path}: the archive ships it but the repository does not hold it"
                    )),
                }
            }
            for asset_path in repository_files.keys() {
                if !candidate.assets.contains_key(asset_path) {
                    drift.push(format!(
                        "{asset_path}: the repository holds it but the candidate archive does not"
                    ));
                }
            }
            drift.sort();
            if !drift.is_empty() {
                return Err(AssetError::SpecDrift(drift));
            }
            true
        }
        None => false,
    };

    Ok(SpecCheckSummary {
        version: manifest.spec_version.clone(),
        spec_revision: pin.revision.clone(),
        imported_source_commit: manifest.source.commit.clone(),
        asset_count: public.len(),
        archive_verified,
    })
}

/// Provenance record written next to the imported public assets.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct SpecProvenance {
    pub schema_version: String,
    pub package: String,
    pub version: String,
    /// Specification repository this repository adopted the public copy from.
    pub spec_repository: String,
    /// Reviewed specification-repository revision the qualified archive came from.
    pub spec_revision: String,
    /// Tools repository the imported subset came from.
    pub imported_source_repository: String,
    /// Tools commit the imported subset came from.
    pub imported_source_commit: String,
    pub archive_file: String,
    pub archive_sha256: String,
    pub manifest_path: String,
    pub manifest_sha256: String,
    pub imported_file_count: usize,
    /// Canonical digest of the complete 39-file public runtime subset.
    ///
    /// This includes the five public assets authored or derived by the
    /// specification repository, so it intentionally differs from the
    /// reviewed baseline digest over the 34 historical toolkit imports.
    pub public_assets_sha256: String,
}

impl SpecProvenance {
    /// Provenance for one validated candidate.
    #[must_use]
    pub fn for_candidate(pin: &SpecPin, asset_count: usize, public_assets_sha256: String) -> Self {
        Self {
            schema_version: SPEC_PROVENANCE_SCHEMA_V1.to_owned(),
            package: pin.package_name.clone(),
            version: pin.version.clone(),
            spec_repository: pin.repository.clone(),
            spec_revision: pin.revision.clone(),
            imported_source_repository: pin.imported_source_repository.clone(),
            imported_source_commit: pin.imported_source_commit.clone(),
            archive_file: pin.archive_file.clone(),
            archive_sha256: pin.archive_sha256.clone(),
            manifest_path: SPEC_MANIFEST_PATH.to_owned(),
            manifest_sha256: pin.manifest_sha256.clone(),
            imported_file_count: asset_count,
            public_assets_sha256,
        }
    }

    /// Render the record as deterministic LF-terminated JSON.
    pub fn to_bytes(&self) -> Result<Vec<u8>, AssetError> {
        let mut bytes = serde_json::to_vec_pretty(self)?;
        bytes.push(b'\n');
        Ok(bytes)
    }

    /// Confirm this record describes `pin` and the public bytes it binds.
    ///
    /// `public_assets_sha256` is the digest **recomputed from the validated
    /// public bytes**, never the value read back out of this record. Comparing
    /// the stored field with itself would accept any value at all, including a
    /// malformed one, so the expected subset digest is supplied by the caller
    /// after it has verified every public asset against the pinned manifest.
    fn validate(&self, pin: &SpecPin, public_assets_sha256: &str) -> Result<(), AssetError> {
        if !is_sha256_hex(&self.public_assets_sha256) {
            return Err(AssetError::SpecInvalid(format!(
                "{SPEC_PROVENANCE_PATH} records `publicAssetsSha256` as {:?}, which is not a SHA-256 digest",
                self.public_assets_sha256
            )));
        }
        if self.public_assets_sha256 != public_assets_sha256 {
            return Err(AssetError::SpecInvalid(format!(
                "{SPEC_PROVENANCE_PATH} records public subset digest {}, but the checked-in public assets digest to {public_assets_sha256}",
                self.public_assets_sha256
            )));
        }
        let expected =
            Self::for_candidate(pin, pin.public_file_count, public_assets_sha256.to_owned());
        if self != &expected {
            return Err(AssetError::SpecInvalid(format!(
                "{SPEC_PROVENANCE_PATH} does not record the pinned qualified candidate"
            )));
        }
        Ok(())
    }
}

/// Whether `value` is exactly 64 hexadecimal digits.
#[must_use]
pub fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

/// Read and fully validate a candidate archive.
///
/// The archive digest, every member's path and type, the shipped manifest, the
/// import manifest, and the reviewed baseline are all checked before any byte
/// of this function's result is used.
pub fn read_candidate_archive(path: &Path, pin: &SpecPin) -> Result<SpecCandidate, AssetError> {
    // The compressed size is bounded before the file is read, so an oversized
    // input is rejected without ever being held in memory.
    let metadata = fs::symlink_metadata(path)?;
    if metadata.file_type().is_symlink() {
        return Err(AssetError::SpecInvalid(format!(
            "{} is a symbolic link; the candidate archive must be a plain file",
            path.display()
        )));
    }
    if !metadata.is_file() {
        return Err(AssetError::SpecInvalid(format!(
            "{} is not a regular file",
            path.display()
        )));
    }
    if metadata.len() > SPEC_ARCHIVE_MAX_BYTES {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate archive is {} bytes, beyond the {SPEC_ARCHIVE_MAX_BYTES} byte limit",
            metadata.len()
        )));
    }
    let archive_bytes = fs::read(path)?;
    let archive_digest = sha256_hex(&archive_bytes);
    if archive_digest != pin.archive_sha256 {
        return Err(AssetError::SpecArchiveDigest {
            expected: pin.archive_sha256.clone(),
            found: archive_digest,
        });
    }
    let files = crate::spec_import::extract_archive_members(&archive_bytes, pin)?;

    let manifest_bytes = files
        .get(&format!("{SPEC_ARCHIVE_PREFIX}/spec-manifest.json"))
        .ok_or_else(|| {
            AssetError::SpecInvalid(
                "the candidate archive carries no spec-manifest.json".to_owned(),
            )
        })?
        .clone();
    let source_import_bytes = files
        .get(&format!("{SPEC_ARCHIVE_PREFIX}/source-import.yaml"))
        .ok_or_else(|| {
            AssetError::SpecInvalid(
                "the candidate archive carries no source-import.yaml".to_owned(),
            )
        })?
        .clone();
    let baseline_bytes = files
        .get(&format!("{SPEC_ARCHIVE_PREFIX}/reviewed-baseline.json"))
        .ok_or_else(|| {
            AssetError::SpecInvalid(
                "the candidate archive carries no reviewed-baseline.json".to_owned(),
            )
        })?
        .clone();

    // The manifest binds every shipped file except itself; the candidate's own
    // count records that, so a manifest that under-binds its payload fails here.
    let mut bound: BTreeMap<String, Vec<u8>> = files
        .into_iter()
        .filter_map(|(path, bytes)| {
            path.strip_prefix(&format!("{SPEC_ARCHIVE_PREFIX}/"))
                .map(|relative| (relative.to_owned(), bytes))
        })
        .collect();
    for metadata in [
        "spec-manifest.json",
        "source-import.yaml",
        "reviewed-baseline.json",
    ] {
        if !bound.contains_key(metadata) {
            return Err(AssetError::SpecInvalid(format!(
                "the candidate archive carries no {metadata}"
            )));
        }
    }
    if bound.remove("spec-manifest.json").is_none() {
        return Err(AssetError::SpecInvalid(
            "the candidate archive carries no spec-manifest.json".to_owned(),
        ));
    }

    let mut candidate = validate_candidate(
        pin,
        &manifest_bytes,
        &source_import_bytes,
        &baseline_bytes,
        &bound,
    )?;
    candidate.public_assets_sha256 = subset_digest(
        &candidate
            .assets
            .iter()
            .map(|(path, bytes)| (path.clone(), sha256_hex(bytes)))
            .collect(),
    )?;
    candidate.archive_sha256 = Some(archive_digest);
    Ok(candidate)
}

/// Format a deterministic `path: digest` drift list.
#[must_use]
pub fn render_drift(drift: &[String]) -> String {
    let mut rendered = String::new();
    for entry in drift {
        let _ = writeln!(rendered, "  {entry}");
    }
    rendered
}
