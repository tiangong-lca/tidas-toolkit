//! Offline executable-asset catalog and deterministic integrity lock.

mod schema_lock;
pub mod spec_import;
pub mod spec_pin;

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use include_dir::{Dir, include_dir};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use walkdir::WalkDir;

pub use schema_lock::{
    SCHEMA_LOCK_PATH, check_filesystem_schema_lock, schema_lock_from_filesystem, write_schema_lock,
};

pub const ASSET_LOCK_SCHEMA_V1: &str = "tidas.asset-lock.v1";
pub const ASSET_LOCK_PATH: &str = "assets/asset-lock.v1.json";
pub const ASSET_LOCK_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/asset-lock.v1.schema.json"
));

static TIDAS_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/tidas");
static EILCD_ASSETS: Dir<'_> = include_dir!("$CARGO_MANIFEST_DIR/assets/eilcd");
const PRODUCT_FLOW_CATEGORY_INDEX: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/validation_indexes/product_flow_category_index.json"
));
const EMBEDDED_LOCK: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/assets/asset-lock.v1.json"
));

pub const SOURCE_ROOTS: [&str; 3] = ["assets/eilcd", "assets/tidas", "assets/validation_indexes"];

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum AssetKind {
    JsonSchema,
    ChineseJsonSchema,
    Methodology,
    PublicRule,
    RuntimeRuleset,
    ValidationIndex,
    Xsd,
    Xslt,
    XmlReference,
    LegacySchemaLock,
}

#[derive(Clone, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetEntryV1 {
    pub path: String,
    pub kind: AssetKind,
    pub sha256: String,
    pub bytes: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AssetLockV1 {
    pub schema_version: String,
    pub source_roots: Vec<String>,
    pub entries: Vec<AssetEntryV1>,
}

#[derive(Clone, Debug)]
pub struct BundledAsset {
    pub path: String,
    pub kind: AssetKind,
    pub bytes: &'static [u8],
}

#[must_use]
pub fn bundled_assets() -> Vec<BundledAsset> {
    let mut assets = Vec::new();
    collect_embedded(&TIDAS_ASSETS, "assets/tidas", &mut assets);
    collect_embedded(&EILCD_ASSETS, "assets/eilcd", &mut assets);
    assets.push(BundledAsset {
        path: "assets/validation_indexes/product_flow_category_index.json".to_owned(),
        kind: AssetKind::ValidationIndex,
        bytes: PRODUCT_FLOW_CATEGORY_INDEX,
    });
    assets.sort_by(|left, right| left.path.cmp(&right.path));
    assets
}

#[must_use]
pub fn bundled_asset(path: &str) -> Option<BundledAsset> {
    bundled_assets()
        .into_iter()
        .find(|asset| asset.path == path)
}

fn collect_embedded(dir: &Dir<'static>, prefix: &str, output: &mut Vec<BundledAsset>) {
    for file in dir.files() {
        let relative = file.path().to_string_lossy().replace('\\', "/");
        let path = format!("{prefix}/{relative}");
        output.push(BundledAsset {
            kind: classify(&path).expect("embedded asset paths are controlled by the repository"),
            path,
            bytes: file.contents(),
        });
    }
    for child in dir.dirs() {
        collect_embedded(child, prefix, output);
    }
}

pub fn embedded_lock() -> Result<AssetLockV1, AssetError> {
    Ok(serde_json::from_str(EMBEDDED_LOCK)?)
}

pub fn verify_embedded_assets() -> Result<AssetLockV1, AssetError> {
    let lock = embedded_lock()?;
    validate_lock_shape(&lock)?;
    let expected: BTreeMap<&str, &AssetEntryV1> = lock
        .entries
        .iter()
        .map(|entry| (entry.path.as_str(), entry))
        .collect();
    let actual = bundled_assets();

    let actual_paths: BTreeSet<&str> = actual.iter().map(|asset| asset.path.as_str()).collect();
    let expected_paths: BTreeSet<&str> = expected.keys().copied().collect();
    if actual_paths != expected_paths {
        return Err(AssetError::PathSetMismatch {
            missing: expected_paths
                .difference(&actual_paths)
                .map(ToString::to_string)
                .collect(),
            unexpected: actual_paths
                .difference(&expected_paths)
                .map(ToString::to_string)
                .collect(),
        });
    }

    for asset in actual {
        let entry = expected
            .get(asset.path.as_str())
            .expect("path-set equality was verified");
        verify_entry(entry, asset.kind, asset.bytes)?;
    }
    Ok(lock)
}

pub fn lock_from_filesystem(repo_root: &Path) -> Result<AssetLockV1, AssetError> {
    let mut entries = Vec::new();
    for source_root in SOURCE_ROOTS {
        let absolute_root = repo_root.join(source_root);
        for item in WalkDir::new(&absolute_root).follow_links(false) {
            let item = item?;
            if !item.file_type().is_file() {
                continue;
            }
            let absolute_path = item.path();
            if source_root.ends_with("validation_indexes")
                && absolute_path.extension().and_then(|value| value.to_str()) != Some("json")
            {
                continue;
            }
            let relative_path = absolute_path
                .strip_prefix(repo_root)
                .map_err(|_| AssetError::OutsideRepository(absolute_path.to_path_buf()))?;
            let path = relative_path.to_string_lossy().replace('\\', "/");
            let bytes = fs::read(absolute_path)?;
            entries.push(entry_for_bytes(path, &bytes)?);
        }
    }
    entries.sort();
    let lock = AssetLockV1 {
        schema_version: ASSET_LOCK_SCHEMA_V1.to_owned(),
        source_roots: SOURCE_ROOTS.iter().map(ToString::to_string).collect(),
        entries,
    };
    validate_lock_shape(&lock)?;
    Ok(lock)
}

pub fn write_lock(repo_root: &Path) -> Result<PathBuf, AssetError> {
    let lock = lock_from_filesystem(repo_root)?;
    let output_path = repo_root.join(ASSET_LOCK_PATH);
    let mut bytes = serde_json::to_vec_pretty(&lock)?;
    bytes.push(b'\n');
    fs::write(&output_path, bytes)?;
    Ok(output_path)
}

pub fn check_filesystem_lock(repo_root: &Path) -> Result<(), AssetError> {
    let expected = lock_from_filesystem(repo_root)?;
    let lock_bytes = fs::read(repo_root.join(ASSET_LOCK_PATH))?;
    let actual: AssetLockV1 = serde_json::from_slice(&lock_bytes)?;
    validate_lock_shape(&actual)?;
    if actual == expected {
        Ok(())
    } else {
        Err(AssetError::StaleLock)
    }
}

pub fn asset_fingerprint() -> Result<String, AssetError> {
    let lock = verify_embedded_assets()?;
    let canonical = serde_json::to_vec(&lock)?;
    Ok(sha256_hex(&canonical))
}

fn entry_for_bytes(path: String, bytes: &[u8]) -> Result<AssetEntryV1, AssetError> {
    Ok(AssetEntryV1 {
        kind: classify(&path)?,
        path,
        sha256: sha256_hex(bytes),
        bytes: u64::try_from(bytes.len()).map_err(|_| AssetError::SizeOverflow)?,
    })
}

fn verify_entry(
    entry: &AssetEntryV1,
    actual_kind: AssetKind,
    bytes: &[u8],
) -> Result<(), AssetError> {
    let actual_hash = sha256_hex(bytes);
    let actual_bytes = u64::try_from(bytes.len()).map_err(|_| AssetError::SizeOverflow)?;
    if entry.kind != actual_kind || entry.sha256 != actual_hash || entry.bytes != actual_bytes {
        return Err(AssetError::IntegrityMismatch {
            path: entry.path.clone(),
        });
    }
    Ok(())
}

pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn validate_lock_shape(lock: &AssetLockV1) -> Result<(), AssetError> {
    if lock.schema_version != ASSET_LOCK_SCHEMA_V1 {
        return Err(AssetError::UnsupportedSchema(lock.schema_version.clone()));
    }
    let expected_roots: Vec<String> = SOURCE_ROOTS.iter().map(ToString::to_string).collect();
    if lock.source_roots != expected_roots {
        return Err(AssetError::SourceRootsMismatch);
    }
    if lock
        .entries
        .windows(2)
        .any(|pair| pair[0].path >= pair[1].path)
    {
        return Err(AssetError::EntriesNotStrictlySorted);
    }
    Ok(())
}

fn classify(path: &str) -> Result<AssetKind, AssetError> {
    if path.ends_with("/schema.lock.json") {
        return Ok(AssetKind::LegacySchemaLock);
    }
    if path.contains("/validation_indexes/") && has_extension(path, "json") {
        return Ok(AssetKind::ValidationIndex);
    }
    if path.contains("/schemas_zh/") && has_extension(path, "json") {
        return Ok(AssetKind::ChineseJsonSchema);
    }
    if path.contains("/schemas/") && has_extension(path, "json") {
        return Ok(AssetKind::JsonSchema);
    }
    if path.contains("/methodologies/") {
        return Ok(
            if path.contains("runtime_rulesets") || path.contains("runtime_profiles") {
                AssetKind::RuntimeRuleset
            } else {
                AssetKind::Methodology
            },
        );
    }
    if path.contains("/rules/") && has_extension(path, "json") {
        return Ok(AssetKind::PublicRule);
    }
    if has_extension(path, "xsd") {
        return Ok(AssetKind::Xsd);
    }
    if has_extension(path, "xsl") {
        return Ok(AssetKind::Xslt);
    }
    if has_extension(path, "xml") {
        return Ok(AssetKind::XmlReference);
    }
    Err(AssetError::UnknownKind(path.to_owned()))
}

fn has_extension(path: &str, expected: &str) -> bool {
    Path::new(path)
        .extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case(expected))
}

#[derive(Debug, Error)]
pub enum AssetError {
    #[error("asset lock uses unsupported schema version {0}")]
    UnsupportedSchema(String),
    #[error("asset lock source roots do not match the owned executable roots")]
    SourceRootsMismatch,
    #[error("asset lock entries must be strictly path-sorted and unique")]
    EntriesNotStrictlySorted,
    #[error("asset path set mismatch; missing={missing:?}, unexpected={unexpected:?}")]
    PathSetMismatch {
        missing: Vec<String>,
        unexpected: Vec<String>,
    },
    #[error("asset integrity mismatch for {path}")]
    IntegrityMismatch { path: String },
    #[error("asset lock is stale; run `cargo run -p tidas-assets --bin tidas-asset-lock -- write`")]
    StaleLock,
    #[error("cannot classify executable asset {0}")]
    UnknownKind(String),
    #[error("asset path is outside the repository: {0}")]
    OutsideRepository(PathBuf),
    #[error("asset size does not fit the lock contract")]
    SizeOverflow,
    #[error("failed to read or write an asset: {0}")]
    Io(#[from] std::io::Error),
    #[error("failed to encode or decode the asset lock: {0}")]
    Json(#[from] serde_json::Error),
    #[error("failed to walk an asset tree: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("TIDAS paired-schema lock violation: {0}")]
    SchemaLock(String),
    #[error("public specification input is invalid: {0}")]
    SpecInvalid(String),
    #[error("public-rule profile composition is invalid: {0}")]
    PublicRules(String),
    #[error("public specification archive digest mismatch: expected {expected}, found {found}")]
    SpecArchiveDigest { expected: String, found: String },
    #[error("public specification manifest digest mismatch: expected {expected}, found {found}")]
    SpecManifestDigest { expected: String, found: String },
    #[error("the public specification copy has drifted from its pin: {} paths differ", .0.len())]
    SpecDrift(Vec<String>),
    #[error("public specification rollback failed after: {cause}; unrestored: {failures:?}")]
    SpecRollback {
        cause: String,
        failures: Vec<String>,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn embedded_assets_match_the_full_lock() {
        let lock = verify_embedded_assets().unwrap();
        assert_eq!(lock.entries.len(), bundled_assets().len());
        assert!(lock.entries.len() >= 79);
    }

    #[test]
    fn fingerprint_is_repeatable() {
        assert_eq!(asset_fingerprint().unwrap(), asset_fingerprint().unwrap());
    }

    #[test]
    fn bundled_asset_lookup_returns_exact_offline_bytes() {
        let path = "assets/tidas/schemas/tidas_sources.json";
        let asset = bundled_asset(path).unwrap();
        assert_eq!(asset.path, path);
        assert_eq!(asset.kind, AssetKind::JsonSchema);
        assert!(asset.bytes.starts_with(b"{"));
        assert!(bundled_asset("assets/tidas/schemas/missing.json").is_none());
    }

    #[test]
    fn every_asset_kind_is_represented() {
        let kinds: BTreeSet<_> = bundled_assets()
            .into_iter()
            .map(|asset| asset.kind)
            .collect();
        assert_eq!(
            kinds,
            BTreeSet::from([
                AssetKind::JsonSchema,
                AssetKind::ChineseJsonSchema,
                AssetKind::Methodology,
                AssetKind::PublicRule,
                AssetKind::RuntimeRuleset,
                AssetKind::ValidationIndex,
                AssetKind::Xsd,
                AssetKind::Xslt,
                AssetKind::XmlReference,
                AssetKind::LegacySchemaLock,
            ])
        );
    }

    #[test]
    fn checked_in_json_schema_matches_the_lock_version() {
        let schema: serde_json::Value = serde_json::from_str(ASSET_LOCK_JSON_SCHEMA_V1).unwrap();
        assert_eq!(
            schema["properties"]["schema_version"]["const"],
            ASSET_LOCK_SCHEMA_V1
        );
        assert_eq!(schema["additionalProperties"], false);
    }
}
