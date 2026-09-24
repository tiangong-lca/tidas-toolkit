use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufRead, BufReader, Read, Seek};
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::NamedTempFile;
use tidas_assets::asset_fingerprint;
use tidas_rulesets::{RulesetCatalog, RulesetDescriptionV1, RulesetError};
use tidas_runtime::{JsonlSpool, SpoolSummaryV1};

use crate::contracts::{SeverityV1, ValidationIssueV1};
use crate::pipeline::{PROGRESS_DOCUMENT_INTERVAL, ValidationError, ValidationRequest};
use crate::schema::{SchemaCatalog, TidasCategory, TidasValidator};
use crate::semantic::SemanticCatalog;
use crate::spool_destination::SpoolDestination;

pub const DOCUMENT_VALIDATION_BATCH_PROTOCOL: &str = "document-validation-batch.v1";
pub const DOCUMENT_VALIDATION_PROFILE: &str = "tidas-document-conformance.v1";
pub const VALIDATION_DESCRIBE_SCHEMA_V1: &str = "tidas.validation-describe.v1";
pub const VALIDATION_FINAL_EVENT_SCHEMA_V1: &str = "tidas.validation-final-event.v1";
pub const VALIDATION_REPORT_SCHEMA_V1: &str = "tidas.validation-report.v1";
pub const DOCUMENT_VALIDATION_MANIFEST_ITEM_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/document-validation-manifest-item.v1.schema.json"
));
pub const VALIDATION_DESCRIBE_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/validation-describe.v1.schema.json"
));
pub const VALIDATION_FINAL_EVENT_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/validation-final-event.v1.schema.json"
));
const VALIDATION_ISSUE_EVENT_SCHEMA_V1: &str = "tidas.validation-issue-event.v1";
const MAX_EVENT_BYTES: usize = 1024 * 1024;
const JSON_MEMORY_MULTIPLIER: u64 = 8;
const JSON_MEMORY_OVERHEAD: u64 = 4096;
const HASH_BUFFER_LEN: usize = 1024 * 1024;

#[derive(Clone, Debug)]
pub struct BatchValidationRequest {
    pub validation: ValidationRequest,
    pub input_manifest: PathBuf,
    pub event_spool: Option<PathBuf>,
    pub profile: String,
}

#[derive(Clone, Debug)]
pub struct BatchValidationOutput {
    pub final_event: ValidationFinalEventV1,
    pub event_spool_path: Option<PathBuf>,
    pub event_spool: Option<SpoolSummaryV1>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationDescribeV1 {
    pub schema_version: String,
    pub package: PackageFingerprintV1,
    pub protocols: Vec<String>,
    pub profiles: Vec<String>,
    pub report_schema_versions: Vec<String>,
    pub event_schema_versions: Vec<String>,
    pub engines: EngineFingerprintV1,
    pub asset_fingerprint: String,
    pub ruleset_catalog: RulesetDescriptionV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageFingerprintV1 {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EngineFingerprintV1 {
    pub rust_minimum: String,
    pub jsonschema: String,
    pub xml: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchDocumentIdentityV1 {
    pub dataset_type: Option<String>,
    pub dataset_id: Option<String>,
    pub dataset_version: Option<String>,
    pub content_sha256: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct BatchValidationIssueEventV1 {
    #[serde(rename = "type")]
    pub event_type: String,
    pub schema_version: String,
    pub protocol: String,
    pub profile: String,
    pub document_key: String,
    pub document_ordinal: u64,
    pub issue_ordinal: u64,
    pub identity: BatchDocumentIdentityV1,
    pub issue: ValidationIssueV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationFinalSummaryV1 {
    pub document_count: u64,
    pub issue_count: u64,
    pub error_count: u64,
    pub warning_count: u64,
    pub info_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ValidationFinalEventV1 {
    #[serde(rename = "type")]
    pub event_type: String,
    pub schema_version: String,
    pub protocol: String,
    pub profile: String,
    pub completed: bool,
    pub summary: ValidationFinalSummaryV1,
    pub logical_issue_stream_sha256: String,
    pub fingerprints: ValidationDescribeV1,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestItem {
    document_key: String,
    category: String,
    relative_path: String,
    content_sha256: String,
    #[serde(default)]
    identity: ManifestIdentity,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ManifestIdentity {
    #[serde(rename = "dataset_type")]
    kind: Option<String>,
    #[serde(rename = "dataset_id")]
    id: Option<String>,
    #[serde(rename = "dataset_version")]
    version: Option<String>,
}

struct BatchDocument {
    document_key: String,
    category: TidasCategory,
    relative_path: String,
    content_sha256: String,
    identity: ManifestIdentity,
    path: PathBuf,
}

pub fn describe_document_validation(
    asset_fingerprint: String,
) -> Result<ValidationDescribeV1, RulesetError> {
    let rulesets = RulesetCatalog::load()?;
    Ok(ValidationDescribeV1 {
        schema_version: VALIDATION_DESCRIBE_SCHEMA_V1.to_owned(),
        package: PackageFingerprintV1 {
            name: "tidas".to_owned(),
            version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        protocols: vec![DOCUMENT_VALIDATION_BATCH_PROTOCOL.to_owned()],
        profiles: vec![DOCUMENT_VALIDATION_PROFILE.to_owned()],
        report_schema_versions: vec![VALIDATION_REPORT_SCHEMA_V1.to_owned()],
        event_schema_versions: vec![
            VALIDATION_ISSUE_EVENT_SCHEMA_V1.to_owned(),
            VALIDATION_FINAL_EVENT_SCHEMA_V1.to_owned(),
        ],
        engines: EngineFingerprintV1 {
            rust_minimum: "1.88".to_owned(),
            jsonschema: "0.40".to_owned(),
            xml: "libxml2/libxslt".to_owned(),
        },
        asset_fingerprint,
        ruleset_catalog: rulesets.description().clone(),
    })
}

pub fn run_document_validation_batch(
    request: &BatchValidationRequest,
) -> Result<BatchValidationOutput, ValidationError> {
    request.validation.cancellation.check()?;
    if request.validation.queue_capacity == 0 {
        return Err(ValidationError::ZeroQueueCapacity);
    }
    if request.profile != DOCUMENT_VALIDATION_PROFILE {
        return Err(ValidationError::BatchProtocol(format!(
            "unsupported document validation profile: {}",
            request.profile
        )));
    }
    if !request.validation.input_dir.is_dir() {
        return Err(ValidationError::InputNotDirectory(
            request.validation.input_dir.clone(),
        ));
    }

    let canonical_root = request.validation.input_dir.canonicalize()?;
    let manifest_bytes = fs::metadata(&request.input_manifest)?.len();
    let manifest_estimate = manifest_bytes
        .checked_mul(8)
        .and_then(|value| value.checked_add(JSON_MEMORY_OVERHEAD))
        .ok_or(ValidationError::SizeOverflow)?;
    let _manifest_reservation = request
        .validation
        .memory_budget
        .reserve(manifest_estimate)?;
    let documents = load_batch_manifest(&canonical_root, &request.input_manifest, request)?;
    let catalog = SchemaCatalog::load()?;
    let semantic = SemanticCatalog::load()?;
    let mut global_spool = EventSpool::new(request.event_spool.as_deref())?;
    let document_count =
        u64::try_from(documents.len()).map_err(|_| ValidationError::SizeOverflow)?;
    report_batch_progress(request, "started", 0, document_count, 0, true);
    let (final_summary, logical_issue_stream_sha256) =
        validate_documents(&documents, &catalog, &semantic, request, &mut global_spool)?;
    let fingerprints = describe_document_validation(asset_fingerprint()?)?;
    let final_event = ValidationFinalEventV1 {
        event_type: "final".to_owned(),
        schema_version: VALIDATION_FINAL_EVENT_SCHEMA_V1.to_owned(),
        protocol: DOCUMENT_VALIDATION_BATCH_PROTOCOL.to_owned(),
        profile: request.profile.clone(),
        completed: true,
        summary: final_summary,
        logical_issue_stream_sha256,
        fingerprints,
    };
    global_spool.push(&serde_json::to_value(&final_event)?)?;
    let (event_spool_path, event_spool) = global_spool.finish()?;
    report_batch_progress(
        request,
        "completed",
        final_event.summary.document_count,
        final_event.summary.document_count,
        final_event.summary.issue_count,
        true,
    );
    Ok(BatchValidationOutput {
        final_event,
        event_spool_path,
        event_spool,
    })
}

fn validate_documents(
    documents: &[BatchDocument],
    catalog: &SchemaCatalog,
    semantic: &SemanticCatalog,
    request: &BatchValidationRequest,
    global_spool: &mut EventSpool,
) -> Result<(ValidationFinalSummaryV1, String), ValidationError> {
    let validators = compile_batch_validators(documents, catalog)?;
    let mut logical_hasher = Sha256::new();
    let mut summary = ValidationFinalSummaryV1 {
        document_count: u64::try_from(documents.len())
            .map_err(|_| ValidationError::SizeOverflow)?,
        issue_count: 0,
        error_count: 0,
        warning_count: 0,
        info_count: 0,
    };
    for (document_ordinal, document) in documents.iter().enumerate() {
        request.validation.cancellation.check()?;
        assert_document_hash(document, request)?;
        let validator = validators
            .get(&document.category)
            .expect("every manifest category is compiled before validation");
        let file_bytes = fs::metadata(&document.path)?.len();
        let estimated_bytes = file_bytes
            .checked_mul(JSON_MEMORY_MULTIPLIER)
            .and_then(|value| value.checked_add(JSON_MEMORY_OVERHEAD))
            .ok_or(ValidationError::SizeOverflow)?;
        let _file_reservation = request.validation.memory_budget.reserve(estimated_bytes)?;
        let bytes = fs::read(&document.path)?;
        let instance: serde_json::Value = serde_json::from_slice(&bytes).map_err(|error| {
            ValidationError::BatchProtocol(format!(
                "{} is not valid JSON: {error}",
                document.relative_path
            ))
        })?;
        let mut document_spool = JsonlSpool::new(NamedTempFile::new()?, MAX_EVENT_BYTES);
        let document_ordinal =
            u64::try_from(document_ordinal).map_err(|_| ValidationError::SizeOverflow)?;
        let mut issue_ordinal = 0_u64;
        for issue in validator.issues(&instance, &document.relative_path) {
            request.validation.cancellation.check()?;
            spool_batch_issue(
                &mut document_spool,
                request,
                document,
                document_ordinal,
                issue_ordinal,
                issue,
                &mut summary,
            )?;
            issue_ordinal = issue_ordinal
                .checked_add(1)
                .ok_or(ValidationError::SizeOverflow)?;
        }
        semantic.validate(
            &instance,
            document.category,
            &document.relative_path,
            &mut |issue| {
                request.validation.cancellation.check()?;
                spool_batch_issue(
                    &mut document_spool,
                    request,
                    document,
                    document_ordinal,
                    issue_ordinal,
                    issue,
                    &mut summary,
                )?;
                issue_ordinal = issue_ordinal
                    .checked_add(1)
                    .ok_or(ValidationError::SizeOverflow)?;
                Ok(())
            },
        )?;
        let (mut document_events, _) = document_spool.finish()?;
        assert_document_hash(document, request)?;
        document_events.rewind()?;
        let reader = BufReader::new(document_events);
        for line in reader.lines() {
            request.validation.cancellation.check()?;
            let mut line = line?;
            line.push('\n');
            logical_hasher.update(line.as_bytes());
            let event: serde_json::Value = serde_json::from_str(&line)?;
            global_spool.push(&event)?;
        }
        let documents_processed = document_ordinal
            .checked_add(1)
            .ok_or(ValidationError::SizeOverflow)?;
        report_batch_progress(
            request,
            "validating",
            documents_processed,
            summary.document_count,
            summary.issue_count,
            false,
        );
    }
    Ok((summary, digest_hex(&logical_hasher.finalize())))
}

fn compile_batch_validators(
    documents: &[BatchDocument],
    catalog: &SchemaCatalog,
) -> Result<BTreeMap<TidasCategory, TidasValidator>, ValidationError> {
    let mut validators = BTreeMap::new();
    for document in documents {
        if let std::collections::btree_map::Entry::Vacant(entry) =
            validators.entry(document.category)
        {
            entry.insert(catalog.validator(document.category)?);
        }
    }
    Ok(validators)
}

fn report_batch_progress(
    request: &BatchValidationRequest,
    phase: &str,
    processed: u64,
    total: u64,
    issues: u64,
    force: bool,
) {
    if force || processed.is_multiple_of(PROGRESS_DOCUMENT_INTERVAL) {
        request.validation.report_progress(
            "tidas-json",
            phase,
            None,
            processed,
            Some(total),
            issues,
        );
    }
}

#[allow(clippy::too_many_arguments)]
fn spool_batch_issue(
    spool: &mut JsonlSpool<NamedTempFile>,
    request: &BatchValidationRequest,
    document: &BatchDocument,
    document_ordinal: u64,
    issue_ordinal: u64,
    issue: ValidationIssueV1,
    summary: &mut ValidationFinalSummaryV1,
) -> Result<(), ValidationError> {
    let severity = issue.severity;
    spool.push(&serde_json::to_value(BatchValidationIssueEventV1 {
        event_type: "issue".to_owned(),
        schema_version: VALIDATION_ISSUE_EVENT_SCHEMA_V1.to_owned(),
        protocol: DOCUMENT_VALIDATION_BATCH_PROTOCOL.to_owned(),
        profile: request.profile.clone(),
        document_key: document.document_key.clone(),
        document_ordinal,
        issue_ordinal,
        identity: BatchDocumentIdentityV1 {
            dataset_type: document.identity.kind.clone(),
            dataset_id: document.identity.id.clone(),
            dataset_version: document.identity.version.clone(),
            content_sha256: document.content_sha256.clone(),
        },
        issue,
    })?)?;
    summary.issue_count += 1;
    match severity {
        SeverityV1::Error => summary.error_count += 1,
        SeverityV1::Warning => summary.warning_count += 1,
        SeverityV1::Info => summary.info_count += 1,
    }
    Ok(())
}

fn load_batch_manifest(
    root: &Path,
    manifest_path: &Path,
    request: &BatchValidationRequest,
) -> Result<Vec<BatchDocument>, ValidationError> {
    let manifest = BufReader::new(File::open(manifest_path)?);
    let mut documents = Vec::new();
    let mut seen_keys = BTreeSet::new();
    let mut seen_paths = BTreeSet::new();
    for (line_index, raw_line) in manifest.lines().enumerate() {
        request.validation.cancellation.check()?;
        let line_number = line_index + 1;
        let raw_line = raw_line?;
        if raw_line.trim().is_empty() {
            continue;
        }
        let item: ManifestItem = serde_json::from_str(&raw_line).map_err(|error| {
            ValidationError::BatchProtocol(format!(
                "manifest line {line_number} is invalid: {error}"
            ))
        })?;
        require_nonempty(&item.document_key, "document_key", line_number)?;
        require_nonempty(&item.category, "category", line_number)?;
        require_nonempty(&item.relative_path, "relative_path", line_number)?;
        require_nonempty(&item.content_sha256, "content_sha256", line_number)?;
        validate_optional_identity(&item.identity, line_number)?;
        let category =
            TidasCategory::parse(&item.category.to_ascii_lowercase()).ok_or_else(|| {
                ValidationError::BatchProtocol(format!(
                    "manifest line {line_number} has unsupported category: {}",
                    item.category
                ))
            })?;
        let content_sha256 = item.content_sha256.to_ascii_lowercase();
        if !is_sha256(&content_sha256) {
            return Err(ValidationError::BatchProtocol(format!(
                "manifest line {line_number} content_sha256 is invalid"
            )));
        }
        if !seen_keys.insert(item.document_key.clone()) {
            return Err(ValidationError::BatchProtocol(format!(
                "duplicate document_key: {}",
                item.document_key
            )));
        }
        let relative_path = normalize_relative_path(&item.relative_path, line_number)?;
        if !seen_paths.insert(relative_path.clone()) {
            return Err(ValidationError::BatchProtocol(format!(
                "duplicate relative_path: {relative_path}"
            )));
        }
        let path = safe_regular_file(root, &relative_path, line_number)?;
        let document = BatchDocument {
            document_key: item.document_key,
            category,
            relative_path,
            content_sha256,
            identity: item.identity,
            path,
        };
        assert_document_hash(&document, request)?;
        documents.push(document);
    }
    Ok(documents)
}

fn require_nonempty(value: &str, name: &str, line: usize) -> Result<(), ValidationError> {
    if value.is_empty() {
        Err(ValidationError::BatchProtocol(format!(
            "manifest line {line} requires non-empty string {name}"
        )))
    } else {
        Ok(())
    }
}

fn validate_optional_identity(
    identity: &ManifestIdentity,
    line: usize,
) -> Result<(), ValidationError> {
    for (name, value) in [
        ("dataset_type", identity.kind.as_deref()),
        ("dataset_id", identity.id.as_deref()),
        ("dataset_version", identity.version.as_deref()),
    ] {
        if value.is_some_and(str::is_empty) {
            return Err(ValidationError::BatchProtocol(format!(
                "manifest line {line} identity.{name} must be a non-empty string or null"
            )));
        }
    }
    Ok(())
}

fn normalize_relative_path(value: &str, line: usize) -> Result<String, ValidationError> {
    if value.is_empty()
        || value.starts_with('/')
        || value.contains('\\')
        || value.contains('\0')
        || value
            .split('/')
            .any(|part| part.is_empty() || part == "." || part == ".." || part.contains(':'))
    {
        return Err(ValidationError::BatchProtocol(format!(
            "manifest line {line} relative_path escapes the batch root"
        )));
    }
    Ok(value.to_owned())
}

fn safe_regular_file(
    root: &Path,
    relative_path: &str,
    line: usize,
) -> Result<PathBuf, ValidationError> {
    let mut current = root.to_path_buf();
    for component in Path::new(relative_path).components() {
        let Component::Normal(part) = component else {
            return Err(ValidationError::BatchProtocol(format!(
                "manifest line {line} relative_path escapes the batch root"
            )));
        };
        current.push(part);
        let metadata = fs::symlink_metadata(&current).map_err(|error| {
            ValidationError::BatchProtocol(format!(
                "manifest line {line} cannot access {relative_path}: {error}"
            ))
        })?;
        if metadata.file_type().is_symlink() {
            return Err(ValidationError::BatchProtocol(format!(
                "manifest line {line} references a symlink: {relative_path}"
            )));
        }
    }
    if !current.is_file() {
        return Err(ValidationError::BatchProtocol(format!(
            "manifest line {line} path is not a regular file: {relative_path}"
        )));
    }
    let resolved = current.canonicalize()?;
    if !resolved.starts_with(root) {
        return Err(ValidationError::BatchProtocol(format!(
            "manifest line {line} path escapes the batch root: {relative_path}"
        )));
    }
    Ok(resolved)
}

fn assert_document_hash(
    document: &BatchDocument,
    request: &BatchValidationRequest,
) -> Result<(), ValidationError> {
    if fs::symlink_metadata(&document.path)?
        .file_type()
        .is_symlink()
        || !document.path.is_file()
    {
        return Err(ValidationError::BatchProtocol(format!(
            "manifest document is not a regular file: {}",
            document.relative_path
        )));
    }
    let _hash_reservation = request
        .validation
        .memory_budget
        .reserve(u64::try_from(HASH_BUFFER_LEN).map_err(|_| ValidationError::SizeOverflow)?)?;
    let mut file = File::open(&document.path)?;
    let mut buffer = vec![0_u8; HASH_BUFFER_LEN];
    let mut hasher = Sha256::new();
    loop {
        request.validation.cancellation.check()?;
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    let actual = digest_hex(&hasher.finalize());
    if actual != document.content_sha256 {
        return Err(ValidationError::BatchProtocol(format!(
            "content hash mismatch for {}: expected {}, got {actual}",
            document.relative_path, document.content_sha256
        )));
    }
    Ok(())
}

fn is_sha256(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}

fn digest_hex(digest: &[u8]) -> String {
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

struct EventSpool {
    target: Option<SpoolDestination>,
    spool: Option<JsonlSpool<NamedTempFile>>,
}

impl EventSpool {
    fn new(target: Option<&Path>) -> Result<Self, ValidationError> {
        let (target, spool) = match target {
            Some(target) => {
                let destination = SpoolDestination::new(target)?;
                let temporary = destination.temporary()?;
                (
                    Some(destination),
                    Some(JsonlSpool::new(temporary, MAX_EVENT_BYTES)),
                )
            }
            None => (None, None),
        };
        Ok(Self { target, spool })
    }

    fn push(&mut self, event: &serde_json::Value) -> Result<(), ValidationError> {
        if let Some(spool) = &mut self.spool {
            spool.push(event)?;
        }
        Ok(())
    }

    fn finish(self) -> Result<(Option<PathBuf>, Option<SpoolSummaryV1>), ValidationError> {
        let Some(spool) = self.spool else {
            return Ok((None, None));
        };
        let target = self
            .target
            .expect("a configured event spool always has a target");
        let (temporary, summary) = spool.finish()?;
        Ok((Some(target.persist(temporary)?), Some(summary)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use tidas_runtime::{CancellationToken, MemoryBudget};

    fn request(root: &Path, manifest: &Path, events: &Path) -> BatchValidationRequest {
        BatchValidationRequest {
            validation: ValidationRequest {
                input_dir: root.to_path_buf(),
                issue_spool: None,
                cancellation: CancellationToken::default(),
                memory_budget: MemoryBudget::new(32 * 1024 * 1024),
                queue_capacity: 8,
                progress: None,
            },
            input_manifest: manifest.to_path_buf(),
            event_spool: Some(events.to_path_buf()),
            profile: DOCUMENT_VALIDATION_PROFILE.to_owned(),
        }
    }

    fn hex_sha256(bytes: &[u8]) -> String {
        digest_hex(&Sha256::digest(bytes))
    }

    #[test]
    fn issues_then_final_are_deterministic_and_logically_hashed() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("batch");
        fs::create_dir_all(root.join("sources")).unwrap();
        fs::write(root.join("sources/bad.json"), b"{}").unwrap();
        let manifest = directory.path().join("manifest.jsonl");
        fs::write(
            &manifest,
            format!(
                "{{\"document_key\":\"source:test:01.00.000\",\"category\":\"sources\",\"relative_path\":\"sources/bad.json\",\"content_sha256\":\"{}\",\"identity\":{{\"dataset_type\":\"source\",\"dataset_id\":\"11111111-1111-1111-1111-111111111111\",\"dataset_version\":\"01.00.000\"}}}}\n",
                hex_sha256(b"{}")
            ),
        )
        .unwrap();
        let events = directory.path().join("events.jsonl");

        let first = run_document_validation_batch(&request(&root, &manifest, &events)).unwrap();
        let first_bytes = fs::read(&events).unwrap();
        let second = run_document_validation_batch(&request(&root, &manifest, &events)).unwrap();
        let second_bytes = fs::read(&events).unwrap();

        assert_eq!(first.final_event, second.final_event);
        assert_eq!(first_bytes, second_bytes);
        let lines: Vec<&[u8]> = first_bytes.split_inclusive(|byte| *byte == b'\n').collect();
        assert_eq!(lines.len(), 2);
        let issue_schema: Value =
            serde_json::from_str(crate::contracts::VALIDATION_ISSUE_EVENT_JSON_SCHEMA_V1).unwrap();
        let issue_validator = jsonschema::draft202012::new(&issue_schema).unwrap();
        let issue_event: Value = serde_json::from_slice(lines[0]).unwrap();
        assert!(issue_validator.is_valid(&issue_event));
        let mut final_schema: Value =
            serde_json::from_str(VALIDATION_FINAL_EVENT_JSON_SCHEMA_V1).unwrap();
        final_schema["properties"]["fingerprints"] =
            serde_json::from_str(VALIDATION_DESCRIBE_JSON_SCHEMA_V1).unwrap();
        let final_validator = jsonschema::draft202012::new(&final_schema).unwrap();
        let final_event: Value = serde_json::from_slice(lines[1]).unwrap();
        assert!(final_validator.is_valid(&final_event));
        assert_eq!(
            first.final_event.logical_issue_stream_sha256,
            hex_sha256(lines[0])
        );
        assert_eq!(first.final_event.summary.document_count, 1);
        assert_eq!(first.final_event.summary.error_count, 1);
    }

    #[test]
    fn oversized_schema_issue_stays_within_the_batch_frame_limit() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("batch");
        fs::create_dir_all(root.join("sources")).unwrap();
        let oversized = "x".repeat(2 * 1024 * 1024);
        let document = serde_json::to_vec(&oversized).unwrap();
        fs::write(root.join("sources/bad.json"), &document).unwrap();
        let manifest = directory.path().join("manifest.jsonl");
        fs::write(
            &manifest,
            format!(
                "{{\"document_key\":\"source:oversized:01.00.000\",\"category\":\"sources\",\"relative_path\":\"sources/bad.json\",\"content_sha256\":\"{}\",\"identity\":{{\"dataset_type\":\"source\",\"dataset_id\":\"22222222-2222-2222-2222-222222222222\",\"dataset_version\":\"01.00.000\"}}}}\n",
                hex_sha256(&document)
            ),
        )
        .unwrap();
        let events = directory.path().join("events.jsonl");

        let first = run_document_validation_batch(&request(&root, &manifest, &events)).unwrap();
        let first_bytes = fs::read(&events).unwrap();
        let second = run_document_validation_batch(&request(&root, &manifest, &events)).unwrap();
        let second_bytes = fs::read(&events).unwrap();

        assert_eq!(first.final_event, second.final_event);
        assert_eq!(first_bytes, second_bytes);
        let lines: Vec<&[u8]> = first_bytes.split_inclusive(|byte| *byte == b'\n').collect();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].len() <= MAX_EVENT_BYTES);
        let issue_event: Value = serde_json::from_slice(lines[0]).unwrap();
        assert_eq!(issue_event["type"], "issue");
        assert_eq!(issue_event["issue"]["context"]["schema_keyword"], "type");
        assert_eq!(
            issue_event["issue"]["context"]["instance_byte_length"],
            Value::from(oversized.len() as u64)
        );
        assert_eq!(
            first.final_event.logical_issue_stream_sha256,
            hex_sha256(lines[0])
        );
        let final_event: Value = serde_json::from_slice(lines[1]).unwrap();
        assert_eq!(final_event["type"], "final");
        assert_eq!(first.final_event.summary.error_count, 1);
    }

    #[test]
    fn unsafe_paths_duplicates_and_hash_drift_fail_before_completion() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("batch");
        fs::create_dir(&root).unwrap();
        let manifest = directory.path().join("manifest.jsonl");
        fs::write(
            &manifest,
            "{\"document_key\":\"bad\",\"category\":\"sources\",\"relative_path\":\"../bad.json\",\"content_sha256\":\"0000000000000000000000000000000000000000000000000000000000000000\"}\n",
        )
        .unwrap();
        let events = directory.path().join("events.jsonl");
        let error = run_document_validation_batch(&request(&root, &manifest, &events)).unwrap_err();
        assert!(error.to_string().contains("escapes"));
        assert!(!events.exists());
    }

    #[test]
    fn checked_in_batch_contracts_match_the_rust_versions() {
        let manifest: serde_json::Value =
            serde_json::from_str(DOCUMENT_VALIDATION_MANIFEST_ITEM_JSON_SCHEMA_V1).unwrap();
        let describe: serde_json::Value =
            serde_json::from_str(VALIDATION_DESCRIBE_JSON_SCHEMA_V1).unwrap();
        let final_event: serde_json::Value =
            serde_json::from_str(VALIDATION_FINAL_EVENT_JSON_SCHEMA_V1).unwrap();
        assert_eq!(manifest["additionalProperties"], false);
        assert_eq!(
            describe["properties"]["schema_version"]["const"],
            VALIDATION_DESCRIBE_SCHEMA_V1
        );
        assert_eq!(
            final_event["properties"]["schema_version"]["const"],
            VALIDATION_FINAL_EVENT_SCHEMA_V1
        );
    }
}
