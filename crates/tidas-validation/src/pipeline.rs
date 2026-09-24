use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use tempfile::NamedTempFile;
use thiserror::Error;
use tidas_assets::{AssetError, asset_fingerprint};
use tidas_runtime::{CancellationToken, JsonlSpool, MemoryBudget, RuntimeError, SpoolSummaryV1};

use crate::contracts::{
    CategorySummaryV1, ValidationIssueEventV1, ValidationIssueV1, ValidationSummaryV1,
};
use crate::schema::{SUPPORTED_TIDAS_CATEGORIES, SchemaCatalog, SchemaError, TidasCategory};
use crate::semantic::{SemanticCatalog, SemanticError};
use crate::spool_destination::SpoolDestination;

const PATH_ACCOUNTING_OVERHEAD: u64 = 128;
const JSON_MEMORY_MULTIPLIER: u64 = 8;
const JSON_MEMORY_OVERHEAD: u64 = 4096;
const MAX_ISSUE_EVENT_BYTES: usize = 1024 * 1024;
pub(crate) const PROGRESS_DOCUMENT_INTERVAL: u64 = 1_000;

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ValidationProgressV1 {
    pub input_format: String,
    pub phase: String,
    pub category: Option<String>,
    pub documents_processed: u64,
    pub documents_total: Option<u64>,
    pub issues_found: u64,
}

#[derive(Clone)]
pub struct ValidationProgressReporter(Arc<dyn Fn(&ValidationProgressV1) + Send + Sync>);

impl ValidationProgressReporter {
    pub fn new(reporter: impl Fn(&ValidationProgressV1) + Send + Sync + 'static) -> Self {
        Self(Arc::new(reporter))
    }

    fn report(&self, progress: &ValidationProgressV1) {
        (self.0)(progress);
    }
}

impl std::fmt::Debug for ValidationProgressReporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ValidationProgressReporter(..)")
    }
}

#[derive(Clone, Debug)]
pub struct ValidationRequest {
    pub input_dir: PathBuf,
    pub issue_spool: Option<PathBuf>,
    pub cancellation: CancellationToken,
    pub memory_budget: MemoryBudget,
    pub queue_capacity: usize,
    pub progress: Option<ValidationProgressReporter>,
}

impl ValidationRequest {
    pub(crate) fn report_progress(
        &self,
        input_format: &str,
        phase: &str,
        category: Option<&str>,
        documents_processed: u64,
        documents_total: Option<u64>,
        issues_found: u64,
    ) {
        if let Some(reporter) = &self.progress {
            reporter.report(&ValidationProgressV1 {
                input_format: input_format.to_owned(),
                phase: phase.to_owned(),
                category: category.map(ToOwned::to_owned),
                documents_processed,
                documents_total,
                issues_found,
            });
        }
    }
}

#[derive(Clone, Debug)]
pub struct ValidationOutput {
    pub summary: ValidationSummaryV1,
    pub issue_spool_path: Option<PathBuf>,
}

pub fn validate_tidas_package(
    request: &ValidationRequest,
) -> Result<ValidationOutput, ValidationError> {
    request.cancellation.check()?;
    if request.queue_capacity == 0 {
        return Err(ValidationError::ZeroQueueCapacity);
    }
    if !request.input_dir.is_dir() {
        return Err(ValidationError::InputNotDirectory(
            request.input_dir.clone(),
        ));
    }

    let catalog = SchemaCatalog::load()?;
    let semantic = SemanticCatalog::load()?;
    let mut summary = ValidationSummaryV1::new("tidas-json", asset_fingerprint()?);
    let mut sink = IssueSink::new(request.issue_spool.as_deref())?;
    report_progress(request, &summary, "started", None, None, true);

    for category in SUPPORTED_TIDAS_CATEGORIES {
        request.cancellation.check()?;
        let category_dir = request.input_dir.join(category.as_str());
        if !category_dir.is_dir() {
            continue;
        }
        let validator = catalog.validator(category)?;
        let (files, _path_reservation) = sorted_json_files(&category_dir, &request.memory_budget)?;
        let mut category_summary = CategorySummaryV1 {
            category: category.as_str().to_owned(),
            ..CategorySummaryV1::default()
        };
        for file_path in files {
            request.cancellation.check()?;
            category_summary.document_count += 1;
            summary.document_count += 1;
            validate_file(
                &request.input_dir,
                &file_path,
                category,
                &validator,
                &semantic,
                request,
                &mut summary,
                &mut category_summary,
                &mut sink,
            )?;
            report_progress(
                request,
                &summary,
                "validating",
                Some(category.as_str()),
                None,
                false,
            );
        }
        summary.categories.push(category_summary);
    }

    summary.category_count =
        u64::try_from(summary.categories.len()).map_err(|_| ValidationError::SizeOverflow)?;
    summary.ok = summary.issue_count == 0;
    let (issue_spool_path, spool_summary) = sink.finish()?;
    summary.issue_spool = spool_summary;
    summary.peak_accounted_memory_bytes = request.memory_budget.peak();
    report_progress(request, &summary, "completed", None, None, true);
    Ok(ValidationOutput {
        summary,
        issue_spool_path,
    })
}

pub(crate) fn report_progress(
    request: &ValidationRequest,
    summary: &ValidationSummaryV1,
    phase: &str,
    category: Option<&str>,
    documents_total: Option<u64>,
    force: bool,
) {
    if !force
        && !summary
            .document_count
            .is_multiple_of(PROGRESS_DOCUMENT_INTERVAL)
    {
        return;
    }
    request.report_progress(
        &summary.input_format,
        phase,
        category,
        summary.document_count,
        documents_total,
        summary.issue_count,
    );
}

#[allow(clippy::too_many_arguments)]
fn validate_file(
    package_root: &Path,
    file_path: &Path,
    category: TidasCategory,
    validator: &crate::schema::TidasValidator,
    semantic: &SemanticCatalog,
    request: &ValidationRequest,
    summary: &mut ValidationSummaryV1,
    category_summary: &mut CategorySummaryV1,
    sink: &mut IssueSink,
) -> Result<(), ValidationError> {
    let relative_path = normalized_relative_path(package_root, file_path)?;
    let file_bytes = fs::metadata(file_path)?.len();
    let estimated_bytes = file_bytes
        .checked_mul(JSON_MEMORY_MULTIPLIER)
        .and_then(|value| value.checked_add(JSON_MEMORY_OVERHEAD))
        .ok_or(ValidationError::SizeOverflow)?;
    let _file_reservation = request.memory_budget.reserve(estimated_bytes)?;
    let bytes = fs::read(file_path)?;
    let instance = match serde_json::from_slice(&bytes) {
        Ok(instance) => instance,
        Err(error) => {
            let issue = ValidationIssueV1::error(
                "invalid_json",
                category.as_str(),
                &relative_path,
                "<root>",
                format!("Invalid JSON: {error}"),
            );
            record_issue(summary, category_summary, sink, issue)?;
            return Ok(());
        }
    };
    for issue in validator.issues(&instance, &relative_path) {
        request.cancellation.check()?;
        record_issue(summary, category_summary, sink, issue)?;
    }
    semantic.validate(&instance, category, &relative_path, &mut |issue| {
        request.cancellation.check()?;
        record_issue(summary, category_summary, sink, issue)
    })?;
    Ok(())
}

pub(crate) fn record_issue(
    summary: &mut ValidationSummaryV1,
    category: &mut CategorySummaryV1,
    sink: &mut IssueSink,
    issue: ValidationIssueV1,
) -> Result<(), ValidationError> {
    summary.record_issue(category, issue.severity);
    sink.push(issue)
}

fn sorted_json_files(
    directory: &Path,
    budget: &MemoryBudget,
) -> Result<(Vec<PathBuf>, tidas_runtime::MemoryReservation), ValidationError> {
    let mut estimated_bytes = 0_u64;
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if is_regular_json_file(&entry)? {
            estimated_bytes = estimated_bytes
                .checked_add(path_estimated_bytes(&entry.path())?)
                .ok_or(ValidationError::SizeOverflow)?;
        }
    }
    let reservation = budget.reserve(estimated_bytes)?;
    let mut files = Vec::new();
    for entry in fs::read_dir(directory)? {
        let entry = entry?;
        if is_regular_json_file(&entry)? {
            files.push(entry.path());
        }
    }
    files.sort_by_key(|path| normalized_sort_key(path));
    Ok((files, reservation))
}

fn is_regular_json_file(entry: &fs::DirEntry) -> Result<bool, std::io::Error> {
    Ok(entry.file_type()?.is_file()
        && entry
            .path()
            .extension()
            .is_some_and(|extension| extension.eq_ignore_ascii_case("json")))
}

fn path_estimated_bytes(path: &Path) -> Result<u64, ValidationError> {
    let path_bytes =
        u64::try_from(path.as_os_str().len()).map_err(|_| ValidationError::SizeOverflow)?;
    path_bytes
        .checked_add(PATH_ACCOUNTING_OVERHEAD)
        .ok_or(ValidationError::SizeOverflow)
}

fn normalized_sort_key(path: &Path) -> String {
    path.file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .replace('\\', "/")
}

fn normalized_relative_path(root: &Path, path: &Path) -> Result<String, ValidationError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ValidationError::PathOutsideInput(path.to_path_buf()))?;
    Ok(relative.to_string_lossy().replace('\\', "/"))
}

pub(crate) struct IssueSink {
    target: Option<SpoolDestination>,
    spool: Option<JsonlSpool<NamedTempFile>>,
    next_ordinal: u64,
}

impl IssueSink {
    pub(crate) fn new(target: Option<&Path>) -> Result<Self, ValidationError> {
        let (target, spool) = match target {
            Some(target) => {
                let destination = SpoolDestination::new(target)?;
                let temporary = destination.temporary()?;
                (
                    Some(destination),
                    Some(JsonlSpool::new(temporary, MAX_ISSUE_EVENT_BYTES)),
                )
            }
            None => (None, None),
        };
        Ok(Self {
            target,
            spool,
            next_ordinal: 0,
        })
    }

    pub(crate) fn push(&mut self, issue: ValidationIssueV1) -> Result<(), ValidationError> {
        if let Some(spool) = &mut self.spool {
            spool.push(&ValidationIssueEventV1::new(self.next_ordinal, issue))?;
        }
        self.next_ordinal = self
            .next_ordinal
            .checked_add(1)
            .ok_or(ValidationError::SizeOverflow)?;
        Ok(())
    }

    pub(crate) fn finish(
        self,
    ) -> Result<(Option<PathBuf>, Option<SpoolSummaryV1>), ValidationError> {
        let Some(spool) = self.spool else {
            return Ok((None, None));
        };
        let target = self
            .target
            .expect("a configured spool always has a target path");
        let (temporary, summary) = spool.finish()?;
        Ok((Some(target.persist(temporary)?), Some(summary)))
    }
}

#[derive(Debug, Error)]
pub enum ValidationError {
    #[error("validation input is not a directory: {0}")]
    InputNotDirectory(PathBuf),
    #[error("validation queue capacity must be greater than zero")]
    ZeroQueueCapacity,
    #[error("validation path is outside the input directory: {0}")]
    PathOutsideInput(PathBuf),
    #[error("issue spool parent directory does not exist: {0}")]
    SpoolParentMissing(PathBuf),
    #[error("failed to persist issue spool at {path}: {source}")]
    PersistSpool {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("validation size cannot be represented safely")]
    SizeOverflow,
    #[error("embedded XSD asset has an unexpected path: {0}")]
    UnexpectedXsdAsset(String),
    #[error("recursive validation walk failed: {0}")]
    Walk(String),
    #[error("XML input is invalid: {0}")]
    InvalidXml(String),
    #[error("document validation batch protocol failed: {0}")]
    BatchProtocol(String),
    #[error(transparent)]
    Asset(#[from] AssetError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Schema(#[from] SchemaError),
    #[error(transparent)]
    Semantic(#[from] SemanticError),
    #[error(transparent)]
    Ruleset(#[from] tidas_rulesets::RulesetError),
    #[error("XML validation failed: {0}")]
    Xml(#[from] tidas_xml::XmlError),
    #[error("validation I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("validation JSON processing failed: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::sync::Mutex;

    use super::*;

    fn request(input_dir: &Path, issue_spool: Option<PathBuf>) -> ValidationRequest {
        ValidationRequest {
            input_dir: input_dir.to_path_buf(),
            issue_spool,
            cancellation: CancellationToken::default(),
            memory_budget: MemoryBudget::new(16 * 1024 * 1024),
            queue_capacity: 16,
            progress: None,
        }
    }

    #[test]
    fn empty_package_is_a_complete_success() {
        let directory = tempfile::tempdir().unwrap();
        let output = validate_tidas_package(&request(directory.path(), None)).unwrap();
        assert!(output.summary.ok);
        assert_eq!(output.summary.document_count, 0);
        assert_eq!(output.summary.issue_count, 0);
        assert_eq!(output.summary.categories, []);
    }

    #[test]
    fn progress_reports_start_and_completion_without_affecting_results() {
        let directory = tempfile::tempdir().unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let observed_for_reporter = Arc::clone(&observed);
        let mut request = request(directory.path(), None);
        request.progress = Some(ValidationProgressReporter::new(move |progress| {
            observed_for_reporter.lock().unwrap().push(progress.clone());
        }));

        let output = validate_tidas_package(&request).unwrap();
        let observed = observed.lock().unwrap();
        assert_eq!(observed.len(), 2);
        assert_eq!(observed[0].phase, "started");
        assert_eq!(observed[1].phase, "completed");
        assert_eq!(observed[1].documents_processed, 0);
        assert_eq!(observed[1].issues_found, output.summary.issue_count);
    }

    #[test]
    fn schema_and_parse_issues_stream_in_deterministic_file_order() {
        let directory = tempfile::tempdir().unwrap();
        let sources = directory.path().join("sources");
        fs::create_dir(&sources).unwrap();
        fs::write(sources.join("b.json"), b"{").unwrap();
        fs::write(sources.join("a.json"), b"{}").unwrap();
        let spool_path = directory.path().join("issues.jsonl");

        let first =
            validate_tidas_package(&request(directory.path(), Some(spool_path.clone()))).unwrap();
        let first_bytes = fs::read(&spool_path).unwrap();
        let second =
            validate_tidas_package(&request(directory.path(), Some(spool_path.clone()))).unwrap();
        let second_bytes = fs::read(&spool_path).unwrap();

        assert!(!first.summary.ok);
        assert_eq!(first.summary, second.summary);
        assert_eq!(first_bytes, second_bytes);
        assert_eq!(first.summary.document_count, 2);
        assert!(first.summary.issue_count >= 2);
        let events: Vec<ValidationIssueEventV1> = first_bytes
            .split(|byte| *byte == b'\n')
            .filter(|line| !line.is_empty())
            .map(|line| serde_json::from_slice(line).unwrap())
            .collect();
        assert!(events[0].issue.file_path.ends_with("a.json"));
        assert!(events.last().unwrap().issue.file_path.ends_with("b.json"));
        assert_eq!(
            first.summary.issue_spool.as_ref().unwrap().event_count,
            first.summary.issue_count
        );
    }

    #[test]
    fn cancellation_stops_before_reading_documents() {
        let directory = tempfile::tempdir().unwrap();
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let request = ValidationRequest {
            input_dir: directory.path().to_path_buf(),
            issue_spool: None,
            cancellation,
            memory_budget: MemoryBudget::new(1024),
            queue_capacity: 1,
            progress: None,
        };
        assert!(matches!(
            validate_tidas_package(&request),
            Err(ValidationError::Runtime(RuntimeError::Cancelled))
        ));
    }

    #[test]
    fn oversized_document_is_rejected_by_the_explicit_budget() {
        let directory = tempfile::tempdir().unwrap();
        let sources = directory.path().join("sources");
        fs::create_dir(&sources).unwrap();
        fs::write(sources.join("large.json"), vec![b' '; 1024]).unwrap();
        let request = ValidationRequest {
            input_dir: directory.path().to_path_buf(),
            issue_spool: None,
            cancellation: CancellationToken::default(),
            memory_budget: MemoryBudget::new(1024),
            queue_capacity: 1,
            progress: None,
        };
        assert!(matches!(
            validate_tidas_package(&request),
            Err(ValidationError::Runtime(
                RuntimeError::BudgetExceeded { .. }
            ))
        ));
    }
}
