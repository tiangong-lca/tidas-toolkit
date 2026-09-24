//! Bounded, deterministic TIDAS JSON and eILCD XML conversion.

mod format;
mod ordering;
mod projection;
mod transaction;

use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use thiserror::Error;
use tidas_assets::{AssetError, asset_fingerprint, bundled_assets};
use tidas_runtime::{
    BoundedReceiver, CancellationToken, MemoryBudget, RuntimeError, bounded_queue,
};
use walkdir::WalkDir;

use transaction::StagedDirectory;

pub use ordering::{IlcdSchemaOrderer, TidasSchemaOrderer};
pub use projection::{
    EILCD_PROJECTION_RECOVERY_SCHEMA_V1, EilcdProjectionRecoveryV1, EilcdProjectionV1,
    ProjectionRestorationV1, project_tidas_to_eilcd, restore_tidas_projection,
};

pub const CONVERSION_REPORT_SCHEMA_V1: &str = "tidas.conversion-report.v1";
pub const CONVERSION_REPORT_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/conversion-report.v1.schema.json"
));
const FILE_MEMORY_MULTIPLIER: u64 = 8;
const FILE_MEMORY_OVERHEAD: u64 = 4096;
const HASH_BUFFER_BYTES: usize = 1024 * 1024;
const PATH_JOB_OVERHEAD: u64 = 256;
const PROGRESS_FILE_INTERVAL: u64 = 1_000;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ConversionDirection {
    TidasToIlcd,
    IlcdToTidas,
}

impl ConversionDirection {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TidasToIlcd => "tidas-to-ilcd",
            Self::IlcdToTidas => "ilcd-to-tidas",
        }
    }
}

pub fn convert_json_to_xml(
    bytes: &[u8],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ConversionError> {
    format::json_to_xml(bytes, cancellation)
}

pub fn convert_xml_to_json(
    bytes: &[u8],
    cancellation: &CancellationToken,
) -> Result<Vec<u8>, ConversionError> {
    format::xml_to_json(bytes, cancellation)
}

#[derive(Clone, Debug)]
pub struct ConversionRequest {
    pub input_dir: PathBuf,
    pub output_dir: PathBuf,
    pub direction: ConversionDirection,
    pub cancellation: CancellationToken,
    pub memory_budget: MemoryBudget,
    pub queue_capacity: usize,
    pub progress: Option<ConversionProgressReporter>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ConversionProgressV1 {
    pub direction: ConversionDirection,
    pub phase: String,
    pub files_processed: u64,
    pub converted_file_count: u64,
    pub copied_file_count: u64,
    pub asset_file_count: u64,
    pub envelope_sidecar_count: u64,
}

#[derive(Clone)]
pub struct ConversionProgressReporter(Arc<dyn Fn(&ConversionProgressV1) + Send + Sync>);

impl ConversionProgressReporter {
    pub fn new(reporter: impl Fn(&ConversionProgressV1) + Send + Sync + 'static) -> Self {
        Self(Arc::new(reporter))
    }

    fn report(&self, progress: &ConversionProgressV1) {
        (self.0)(progress);
    }
}

impl std::fmt::Debug for ConversionProgressReporter {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ConversionProgressReporter(..)")
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ConversionReportV1 {
    pub schema_version: String,
    pub direction: ConversionDirection,
    pub converted_file_count: u64,
    pub copied_file_count: u64,
    pub asset_file_count: u64,
    pub envelope_sidecar_count: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub output_tree_sha256: String,
    pub asset_fingerprint: String,
    pub peak_accounted_memory_bytes: u64,
}

pub fn convert_directory(
    request: &ConversionRequest,
) -> Result<ConversionReportV1, ConversionError> {
    request.cancellation.check()?;
    if request.queue_capacity == 0 {
        return Err(ConversionError::ZeroQueueCapacity);
    }
    if !request.input_dir.is_dir() {
        return Err(ConversionError::InputNotDirectory(
            request.input_dir.clone(),
        ));
    }
    reject_nested_output(&request.input_dir, &request.output_dir)?;
    let staging = StagedDirectory::new(&request.output_dir)?;
    let orderer = (request.direction == ConversionDirection::TidasToIlcd)
        .then(IlcdSchemaOrderer::from_bundled_assets)
        .transpose()?;
    let mut report = ConversionReportV1 {
        schema_version: CONVERSION_REPORT_SCHEMA_V1.to_owned(),
        direction: request.direction,
        converted_file_count: 0,
        copied_file_count: 0,
        asset_file_count: 0,
        envelope_sidecar_count: 0,
        input_bytes: 0,
        output_bytes: 0,
        output_tree_sha256: String::new(),
        asset_fingerprint: asset_fingerprint()?,
        peak_accounted_memory_bytes: 0,
    };
    report_progress(request, "started", &report, true)?;
    if request.output_dir.exists() {
        if !request.output_dir.is_dir() {
            return Err(ConversionError::OutputNotDirectory(
                request.output_dir.clone(),
            ));
        }
        copy_tree(
            &request.output_dir,
            staging.path(),
            request,
            None,
            None,
            &mut report,
        )?;
    }
    let data_dir = staging.path().join("data");
    fs::create_dir_all(&data_dir)?;
    copy_tree(
        &request.input_dir,
        &data_dir,
        request,
        Some(request.direction),
        orderer.as_ref(),
        &mut report,
    )?;
    write_direction_assets(staging.path(), request, &mut report)?;
    report_progress(request, "hashing", &report, true)?;
    let (output_bytes, output_tree_sha256) = hash_tree(staging.path(), request)?;
    report.output_bytes = output_bytes;
    report.output_tree_sha256 = output_tree_sha256;
    report.peak_accounted_memory_bytes = request.memory_budget.peak();
    request.cancellation.check()?;
    staging.commit()?;
    report_progress(request, "completed", &report, true)?;
    Ok(report)
}

fn copy_tree(
    source_root: &Path,
    target_root: &Path,
    request: &ConversionRequest,
    conversion: Option<ConversionDirection>,
    orderer: Option<&IlcdSchemaOrderer>,
    report: &mut ConversionReportV1,
) -> Result<(), ConversionError> {
    let (sender, receiver) =
        bounded_queue::<FileJob>(request.queue_capacity, request.memory_budget.clone());
    let mut queued = 0_usize;
    let entries = WalkDir::new(source_root)
        .follow_links(false)
        .sort_by(|left, right| {
            left.file_name()
                .to_string_lossy()
                .cmp(&right.file_name().to_string_lossy())
        })
        .into_iter();
    for entry in entries {
        request.cancellation.check()?;
        let entry = entry?;
        if entry.file_type().is_symlink() {
            return Err(ConversionError::Symlink(entry.path().to_path_buf()));
        }
        let relative = safe_relative(source_root, entry.path())?;
        if relative.as_os_str().is_empty() {
            continue;
        }
        let target = target_root.join(relative);
        if entry.file_type().is_dir() {
            fs::create_dir_all(&target)?;
        } else if entry.file_type().is_file() {
            if conversion == Some(ConversionDirection::IlcdToTidas)
                && (is_envelope_sidecar(entry.path())
                    || is_projection_recovery_sidecar(entry.path()))
            {
                let dataset = dataset_path_for_sidecar(entry.path())?;
                if !dataset.is_file() {
                    return Err(ConversionError::OrphanEnvelopeSidecar(entry.into_path()));
                }
                continue;
            }
            let job = FileJob {
                source: entry.into_path(),
                target,
            };
            let estimated = path_job_estimate(&job)?;
            sender.send(job, estimated, &request.cancellation)?;
            queued += 1;
            if queued == request.queue_capacity {
                process_next_file(source_root, &receiver, conversion, orderer, request, report)?;
                queued -= 1;
            }
        }
    }
    while queued > 0 {
        process_next_file(source_root, &receiver, conversion, orderer, request, report)?;
        queued -= 1;
    }
    Ok(())
}

#[derive(Debug)]
struct FileJob {
    source: PathBuf,
    target: PathBuf,
}

fn path_job_estimate(job: &FileJob) -> Result<u64, ConversionError> {
    let source =
        u64::try_from(job.source.as_os_str().len()).map_err(|_| ConversionError::SizeOverflow)?;
    let target =
        u64::try_from(job.target.as_os_str().len()).map_err(|_| ConversionError::SizeOverflow)?;
    source
        .checked_add(target)
        .and_then(|value| value.checked_add(PATH_JOB_OVERHEAD))
        .ok_or(ConversionError::SizeOverflow)
}

fn process_next_file(
    source_root: &Path,
    receiver: &BoundedReceiver<FileJob>,
    conversion: Option<ConversionDirection>,
    orderer: Option<&IlcdSchemaOrderer>,
    request: &ConversionRequest,
    report: &mut ConversionReportV1,
) -> Result<(), ConversionError> {
    let job = receiver.recv(&request.cancellation)?.into_inner();
    let direction = conversion.filter(|direction| is_convertible(&job.source, *direction));
    if let Some(direction) = direction {
        let target = job.target.with_extension(match direction {
            ConversionDirection::TidasToIlcd => "xml",
            ConversionDirection::IlcdToTidas => "json",
        });
        convert_file(
            source_root,
            &job.source,
            &target,
            direction,
            orderer,
            request,
            report,
        )?;
    } else {
        ensure_parent(&job.target)?;
        fs::copy(&job.source, &job.target)?;
        if conversion.is_some() {
            checked_increment(&mut report.copied_file_count)?;
            checked_add(&mut report.input_bytes, fs::metadata(&job.source)?.len())?;
        }
    }
    if conversion.is_some() {
        report_progress(request, "converting", report, false)?;
    }
    Ok(())
}

fn convert_file(
    source_root: &Path,
    source: &Path,
    target: &Path,
    direction: ConversionDirection,
    orderer: Option<&IlcdSchemaOrderer>,
    request: &ConversionRequest,
    report: &mut ConversionReportV1,
) -> Result<(), ConversionError> {
    let byte_count = fs::metadata(source)?.len();
    let estimated = byte_count
        .checked_mul(FILE_MEMORY_MULTIPLIER)
        .and_then(|value| value.checked_add(FILE_MEMORY_OVERHEAD))
        .ok_or(ConversionError::SizeOverflow)?;
    let _reservation = request.memory_budget.reserve(estimated)?;
    let bytes = fs::read(source)?;
    if u64::try_from(bytes.len()).map_err(|_| ConversionError::SizeOverflow)? != byte_count {
        return Err(ConversionError::SourceChanged(source.to_path_buf()));
    }
    let (converted, sidecar_output, projection_sidecar_output) = match direction {
        ConversionDirection::TidasToIlcd => {
            let mut document: serde_json::Value = serde_json::from_slice(&bytes)?;
            let envelope = split_envelope(source_root, source, &mut document)?;
            let projection = dataset_category(source_root, source)
                .map(|category| project_tidas_to_eilcd(&document, category))
                .transpose()?;
            let recovery = projection
                .as_ref()
                .and_then(|value| value.recovery.as_ref())
                .map(serialize_projection_recovery)
                .transpose()?;
            if let Some(projection) = projection {
                document = projection.document;
            }
            if let (Some(orderer), Some(category)) =
                (orderer, dataset_category(source_root, source))
            {
                document = orderer.order_document(&document, category)?;
            }
            (
                format::json_value_to_xml(&document, &request.cancellation)?,
                envelope,
                recovery,
            )
        }
        ConversionDirection::IlcdToTidas => {
            let converted = format::xml_to_json(&bytes, &request.cancellation)?;
            (
                merge_envelope(
                    source,
                    merge_projection_recovery(
                        source,
                        converted,
                        dataset_category(source_root, source),
                        request,
                        report,
                    )?,
                    request,
                    report,
                )?,
                None,
                None,
            )
        }
    };
    ensure_parent(target)?;
    fs::write(target, converted)?;
    if let Some(sidecar) = sidecar_output {
        let path = envelope_sidecar_path(target);
        fs::write(path, sidecar)?;
        checked_increment(&mut report.envelope_sidecar_count)?;
    }
    if let Some(sidecar) = projection_sidecar_output {
        let path = projection_recovery_sidecar_path(target);
        fs::write(path, sidecar)?;
        checked_increment(&mut report.envelope_sidecar_count)?;
    }
    checked_increment(&mut report.converted_file_count)?;
    checked_add(&mut report.input_bytes, byte_count)?;
    Ok(())
}

fn write_direction_assets(
    root: &Path,
    request: &ConversionRequest,
    report: &mut ConversionReportV1,
) -> Result<(), ConversionError> {
    let prefix = match request.direction {
        ConversionDirection::TidasToIlcd => "assets/eilcd/",
        ConversionDirection::IlcdToTidas => "assets/tidas/",
    };
    for asset in bundled_assets() {
        request.cancellation.check()?;
        let Some(relative) = asset.path.strip_prefix(prefix) else {
            continue;
        };
        if request.direction == ConversionDirection::IlcdToTidas
            && !matches!(
                relative.split('/').next(),
                Some("methodologies" | "schemas" | "schemas_zh")
            )
        {
            continue;
        }
        let target = root.join(relative);
        ensure_parent(&target)?;
        fs::write(target, asset.bytes)?;
        checked_increment(&mut report.asset_file_count)?;
    }
    Ok(())
}

fn hash_tree(root: &Path, request: &ConversionRequest) -> Result<(u64, String), ConversionError> {
    let _reservation = request
        .memory_budget
        .reserve(u64::try_from(HASH_BUFFER_BYTES).map_err(|_| ConversionError::SizeOverflow)?)?;
    let mut tree = Sha256::new();
    let mut output_bytes = 0_u64;
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    for entry in WalkDir::new(root)
        .follow_links(false)
        .sort_by(|left, right| {
            left.file_name()
                .to_string_lossy()
                .cmp(&right.file_name().to_string_lossy())
        })
    {
        request.cancellation.check()?;
        let entry = entry?;
        if entry.file_type().is_symlink() {
            return Err(ConversionError::Symlink(entry.into_path()));
        }
        if !entry.file_type().is_file() {
            continue;
        }
        let path = entry.path();
        let relative = normalized_relative(root, path)?;
        let relative_len =
            u64::try_from(relative.len()).map_err(|_| ConversionError::SizeOverflow)?;
        tree.update(relative_len.to_le_bytes());
        tree.update(relative.as_bytes());
        let file_len = entry.metadata()?.len();
        tree.update(file_len.to_le_bytes());
        let mut file = File::open(path)?;
        loop {
            let read = file.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            output_bytes = output_bytes
                .checked_add(u64::try_from(read).map_err(|_| ConversionError::SizeOverflow)?)
                .ok_or(ConversionError::SizeOverflow)?;
            tree.update(&buffer[..read]);
        }
    }
    Ok((output_bytes, digest_hex(&tree.finalize())))
}

fn reject_nested_output(input: &Path, output: &Path) -> Result<(), ConversionError> {
    let input = input.canonicalize()?;
    let output_parent = output.parent().unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(output_parent)?;
    let output = output_parent.canonicalize()?.join(
        output
            .file_name()
            .ok_or_else(|| ConversionError::InvalidOutput(output.to_path_buf()))?,
    );
    if output.starts_with(input) {
        return Err(ConversionError::OutputInsideInput(output));
    }
    Ok(())
}

fn safe_relative<'a>(root: &Path, path: &'a Path) -> Result<&'a Path, ConversionError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| ConversionError::PathOutsideInput(path.to_path_buf()))?;
    if relative
        .components()
        .any(|component| !matches!(component, Component::Normal(_)))
        && !relative.as_os_str().is_empty()
    {
        return Err(ConversionError::PathOutsideInput(path.to_path_buf()));
    }
    for component in relative.components() {
        let Component::Normal(component) = component else {
            continue;
        };
        let name = component
            .to_str()
            .ok_or_else(|| ConversionError::NonPortablePath(path.to_path_buf()))?;
        if !is_portable_component(name) {
            return Err(ConversionError::NonPortablePath(path.to_path_buf()));
        }
    }
    Ok(relative)
}

fn ensure_parent(path: &Path) -> Result<(), ConversionError> {
    fs::create_dir_all(path.parent().unwrap_or_else(|| Path::new(".")))?;
    Ok(())
}

fn checked_increment(value: &mut u64) -> Result<(), ConversionError> {
    *value = value.checked_add(1).ok_or(ConversionError::SizeOverflow)?;
    Ok(())
}

fn checked_add(value: &mut u64, additional: u64) -> Result<(), ConversionError> {
    *value = value
        .checked_add(additional)
        .ok_or(ConversionError::SizeOverflow)?;
    Ok(())
}

fn report_progress(
    request: &ConversionRequest,
    phase: &str,
    report: &ConversionReportV1,
    force: bool,
) -> Result<(), ConversionError> {
    let files_processed = report
        .converted_file_count
        .checked_add(report.copied_file_count)
        .ok_or(ConversionError::SizeOverflow)?;
    if !force && !files_processed.is_multiple_of(PROGRESS_FILE_INTERVAL) {
        return Ok(());
    }
    if let Some(reporter) = &request.progress {
        reporter.report(&ConversionProgressV1 {
            direction: request.direction,
            phase: phase.to_owned(),
            files_processed,
            converted_file_count: report.converted_file_count,
            copied_file_count: report.copied_file_count,
            asset_file_count: report.asset_file_count,
            envelope_sidecar_count: report.envelope_sidecar_count,
        });
    }
    Ok(())
}

fn is_convertible(path: &Path, direction: ConversionDirection) -> bool {
    if path.file_name().is_some_and(|name| name == "manifest.json") {
        return false;
    }
    path.extension().is_some_and(|extension| {
        extension.eq_ignore_ascii_case(match direction {
            ConversionDirection::TidasToIlcd => "json",
            ConversionDirection::IlcdToTidas => "xml",
        })
    })
}

fn split_envelope(
    source_root: &Path,
    source: &Path,
    document: &mut serde_json::Value,
) -> Result<Option<Vec<u8>>, ConversionError> {
    let object = document
        .as_object_mut()
        .ok_or(ConversionError::JsonRootNotObject)?;
    if object.len() == 1 {
        return Ok(None);
    }
    let expected_root = expected_dataset_root(source_root, source)
        .ok_or(ConversionError::JsonRootCount(object.len()))?;
    let dataset =
        object
            .remove(expected_root)
            .ok_or_else(|| ConversionError::MissingDatasetRoot {
                path: source.to_path_buf(),
                expected: expected_root.to_owned(),
            })?;
    let envelope = serde_json::to_vec_pretty(&serde_json::Value::Object(std::mem::take(object)))?;
    let mut sidecar = envelope;
    sidecar.push(b'\n');
    let mut dataset_object = serde_json::Map::new();
    dataset_object.insert(expected_root.to_owned(), dataset);
    *document = serde_json::Value::Object(dataset_object);
    Ok(Some(sidecar))
}

fn merge_envelope(
    source: &Path,
    converted: Vec<u8>,
    request: &ConversionRequest,
    report: &mut ConversionReportV1,
) -> Result<Vec<u8>, ConversionError> {
    let sidecar_path = envelope_sidecar_path(source);
    if !sidecar_path.is_file() {
        return Ok(converted);
    }
    let sidecar_len = fs::metadata(&sidecar_path)?.len();
    let sidecar_estimated = sidecar_len
        .checked_mul(FILE_MEMORY_MULTIPLIER)
        .and_then(|value| value.checked_add(FILE_MEMORY_OVERHEAD))
        .ok_or(ConversionError::SizeOverflow)?;
    let _sidecar_reservation = request.memory_budget.reserve(sidecar_estimated)?;
    let sidecar_bytes = fs::read(&sidecar_path)?;
    if u64::try_from(sidecar_bytes.len()).map_err(|_| ConversionError::SizeOverflow)? != sidecar_len
    {
        return Err(ConversionError::SourceChanged(sidecar_path));
    }
    checked_add(
        &mut report.input_bytes,
        u64::try_from(sidecar_bytes.len()).map_err(|_| ConversionError::SizeOverflow)?,
    )?;
    let mut document: serde_json::Value = serde_json::from_slice(&converted)?;
    let envelope: serde_json::Value = serde_json::from_slice(&sidecar_bytes)?;
    let document = document
        .as_object_mut()
        .ok_or(ConversionError::JsonRootNotObject)?;
    let envelope = envelope
        .as_object()
        .ok_or_else(|| ConversionError::InvalidEnvelope(sidecar_path.clone()))?;
    for (key, value) in envelope {
        if document.insert(key.clone(), value.clone()).is_some() {
            return Err(ConversionError::EnvelopeKeyCollision {
                path: sidecar_path,
                key: key.clone(),
            });
        }
    }
    let mut merged = serde_json::to_vec_pretty(&serde_json::Value::Object(document.clone()))?;
    merged.push(b'\n');
    checked_increment(&mut report.envelope_sidecar_count)?;
    Ok(merged)
}

fn envelope_sidecar_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map_or_else(|| path.as_os_str().to_owned(), std::ffi::OsStr::to_owned);
    let mut name = stem;
    name.push(".tidas-envelope.json");
    path.with_file_name(name)
}

fn projection_recovery_sidecar_path(path: &Path) -> PathBuf {
    let stem = path
        .file_stem()
        .map_or_else(|| path.as_os_str().to_owned(), std::ffi::OsStr::to_owned);
    let mut name = stem;
    name.push(".tidas-recovery.json");
    path.with_file_name(name)
}

fn serialize_projection_recovery(
    recovery: &EilcdProjectionRecoveryV1,
) -> Result<Vec<u8>, ConversionError> {
    let mut bytes = serde_json::to_vec_pretty(recovery)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn merge_projection_recovery(
    source: &Path,
    converted: Vec<u8>,
    category: Option<&str>,
    request: &ConversionRequest,
    report: &mut ConversionReportV1,
) -> Result<Vec<u8>, ConversionError> {
    let sidecar_path = projection_recovery_sidecar_path(source);
    let has_recovery = sidecar_path.is_file();
    if !has_recovery && category != Some("processes") {
        return Ok(converted);
    }
    let mut document: serde_json::Value = serde_json::from_slice(&converted)?;
    if has_recovery {
        let sidecar_len = fs::metadata(&sidecar_path)?.len();
        let estimated = sidecar_len
            .checked_mul(FILE_MEMORY_MULTIPLIER)
            .and_then(|value| value.checked_add(FILE_MEMORY_OVERHEAD))
            .ok_or(ConversionError::SizeOverflow)?;
        let _reservation = request.memory_budget.reserve(estimated)?;
        let recovery: EilcdProjectionRecoveryV1 =
            serde_json::from_slice(&fs::read(&sidecar_path)?)?;
        restore_tidas_projection(&mut document, &recovery).map_err(|error| {
            ConversionError::ProjectionRecoveryFailed {
                path: sidecar_path.clone(),
                message: error.to_string(),
            }
        })?;
        checked_add(&mut report.input_bytes, sidecar_len)?;
        checked_increment(&mut report.envelope_sidecar_count)?;
    }
    if category == Some("processes") {
        projection::restore_process_schema_types(&mut document);
    }
    let mut restored = serde_json::to_vec_pretty(&document)?;
    restored.push(b'\n');
    Ok(restored)
}

fn is_envelope_sidecar(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".tidas-envelope.json"))
}

fn is_projection_recovery_sidecar(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|name| name.to_string_lossy().ends_with(".tidas-recovery.json"))
}

fn dataset_path_for_sidecar(path: &Path) -> Result<PathBuf, ConversionError> {
    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .ok_or_else(|| ConversionError::NonPortablePath(path.to_path_buf()))?;
    let stem = name
        .strip_suffix(".tidas-envelope.json")
        .or_else(|| name.strip_suffix(".tidas-recovery.json"))
        .ok_or_else(|| ConversionError::InvalidEnvelope(path.to_path_buf()))?;
    Ok(path.with_file_name(format!("{stem}.xml")))
}

fn expected_dataset_root(source_root: &Path, source: &Path) -> Option<&'static str> {
    match dataset_category(source_root, source)? {
        "contacts" => Some("contactDataSet"),
        "flowproperties" => Some("flowPropertyDataSet"),
        "flows" => Some("flowDataSet"),
        "lciamethods" => Some("LCIAMethodDataSet"),
        "lifecyclemodels" => Some("lifeCycleModelDataSet"),
        "processes" => Some("processDataSet"),
        "sources" => Some("sourceDataSet"),
        "unitgroups" => Some("unitGroupDataSet"),
        _ => None,
    }
}

fn dataset_category<'a>(source_root: &Path, source: &'a Path) -> Option<&'a str> {
    let category = source
        .strip_prefix(source_root)
        .ok()?
        .components()
        .next()?
        .as_os_str()
        .to_str()?;
    matches!(
        category,
        "contacts"
            | "flowproperties"
            | "flows"
            | "lciamethods"
            | "lifecyclemodels"
            | "processes"
            | "sources"
            | "unitgroups"
    )
    .then_some(category)
}

fn normalized_relative(root: &Path, path: &Path) -> Result<String, ConversionError> {
    let relative = safe_relative(root, path)?;
    let components: Result<Vec<_>, _> = relative
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .ok_or_else(|| ConversionError::NonPortablePath(path.to_path_buf()))
        })
        .collect();
    Ok(components?.join("/"))
}

fn is_portable_component(name: &str) -> bool {
    if name.is_empty()
        || name.ends_with(['.', ' '])
        || name
            .chars()
            .any(|character| character.is_control() || r#"<>:"/\|?*"#.contains(character))
    {
        return false;
    }
    let stem = name
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    !matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        && !(stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && matches!(stem.as_bytes()[3], b'1'..=b'9'))
}

fn digest_hex(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[derive(Debug, Error)]
pub enum ConversionError {
    #[error("input is not a directory: {0}")]
    InputNotDirectory(PathBuf),
    #[error("output exists but is not a directory: {0}")]
    OutputNotDirectory(PathBuf),
    #[error("output directory cannot be nested inside input: {0}")]
    OutputInsideInput(PathBuf),
    #[error("output path has no final component: {0}")]
    InvalidOutput(PathBuf),
    #[error("input traversal encountered a symlink: {0}")]
    Symlink(PathBuf),
    #[error("path is outside the declared input root: {0}")]
    PathOutsideInput(PathBuf),
    #[error("path is not portable across the supported platform matrix: {0}")]
    NonPortablePath(PathBuf),
    #[error("queue capacity must be greater than zero")]
    ZeroQueueCapacity,
    #[error("JSON conversion root must be an object")]
    JsonRootNotObject,
    #[error("JSON conversion root must have exactly one element, got {0}")]
    JsonRootCount(usize),
    #[error("dataset at {path} does not contain expected root {expected}")]
    MissingDatasetRoot { path: PathBuf, expected: String },
    #[error("target eILCD ordering schema is missing for {0}")]
    OrderingSchemaMissing(String),
    #[error("target eILCD ordering schema reference is invalid: {0}")]
    OrderingSchemaReference(String),
    #[error("target eILCD ordering schema reference cycle: {0}")]
    OrderingSchemaCycle(String),
    #[error("conversion envelope sidecar is not a JSON object: {0}")]
    InvalidEnvelope(PathBuf),
    #[error("conversion envelope sidecar has no matching XML dataset: {0}")]
    OrphanEnvelopeSidecar(PathBuf),
    #[error("conversion envelope sidecar {path} collides with dataset key {key}")]
    EnvelopeKeyCollision { path: PathBuf, key: String },
    #[error("unsupported eILCD projection recovery schema: {0}")]
    UnsupportedProjectionRecovery(String),
    #[error("invalid eILCD projection recovery path: {0}")]
    InvalidProjectionRecoveryPath(String),
    #[error(
        "eILCD projection recovery did not reproduce source semantics: expected {expected}, got {actual}"
    )]
    ProjectionRecoveryMismatch { expected: String, actual: String },
    #[error("eILCD projection recovery failed for {path}: {message}")]
    ProjectionRecoveryFailed { path: PathBuf, message: String },
    #[error("XML name is invalid: {0}")]
    InvalidXmlName(String),
    #[error("XML 1.0 text contains forbidden character U+{0:04X}")]
    InvalidXmlCharacter(u32),
    #[error("XML text and attributes must be scalar values")]
    NonScalarText,
    #[error("XML input contains text outside its root")]
    TextOutsideRoot,
    #[error("XML input contains more than one root")]
    MultipleRoots,
    #[error("XML input has no root")]
    MissingRoot,
    #[error("XML input contains an unmatched closing element")]
    UnmatchedEnd,
    #[error("XML input ends with unclosed elements")]
    UnclosedElements,
    #[error("XML document types are forbidden")]
    DoctypeForbidden,
    #[error("conversion size cannot be represented safely")]
    SizeOverflow,
    #[error("input file changed while it was being converted: {0}")]
    SourceChanged(PathBuf),
    #[error("atomic output commit failed and rollback also failed: {source}; {restore}")]
    CommitRollback {
        source: std::io::Error,
        restore: std::io::Error,
    },
    #[error("conversion I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("conversion JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("conversion XML failed: {0}")]
    Xml(#[from] quick_xml::Error),
    #[error("conversion XML attribute failed: {0}")]
    Attribute(#[from] quick_xml::events::attributes::AttrError),
    #[error("conversion XML encoding failed: {0}")]
    Encoding(#[from] quick_xml::encoding::EncodingError),
    #[error("conversion XML escaping failed: {0}")]
    Escape(#[from] quick_xml::escape::EscapeError),
    #[error("conversion runtime failed: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("conversion asset catalog failed: {0}")]
    Asset(#[from] AssetError),
    #[error("conversion traversal failed: {0}")]
    Walk(#[from] walkdir::Error),
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use jsonschema::error::ValidationErrorKind;
    use jsonschema::{Resource, ValidationError};
    use serde_json::{Value, json};
    use tempfile::tempdir;
    use tidas_assets::{AssetKind, bundled_assets};
    use tidas_validation::{ValidationRequest, validate_ilcd_package};

    use super::*;

    const REPRESENTATIVE_DATASETS: &[(&str, &str, &str)] = &[
        ("processes", "process", "processDataSet"),
        ("flows", "flow", "flowDataSet"),
        ("flowproperties", "flow-property", "flowPropertyDataSet"),
        ("unitgroups", "unit-group", "unitGroupDataSet"),
        ("contacts", "contact", "contactDataSet"),
        ("sources", "source", "sourceDataSet"),
        ("lciamethods", "lcia-method", "LCIAMethodDataSet"),
        (
            "lifecyclemodels",
            "lifecycle-model",
            "lifeCycleModelDataSet",
        ),
    ];

    fn request(
        input_dir: &Path,
        output_dir: &Path,
        direction: ConversionDirection,
    ) -> ConversionRequest {
        ConversionRequest {
            input_dir: input_dir.to_path_buf(),
            output_dir: output_dir.to_path_buf(),
            direction,
            cancellation: CancellationToken::default(),
            memory_budget: MemoryBudget::new(32 * 1024 * 1024),
            queue_capacity: 8,
            progress: None,
        }
    }

    fn write_representative_tidas(root: &Path) -> BTreeMap<PathBuf, Value> {
        let mut documents = BTreeMap::new();
        for &(category, file_stem, root_name) in REPRESENTATIVE_DATASETS {
            let relative = PathBuf::from(category).join(format!("{file_stem}.json"));
            let document = json!({
                (root_name): {
                    "@xmlns": "http://lca.jrc.it/ILCD/Process",
                    "@version": "1.1",
                    "name": {
                        "baseName": [
                            {"@xml:lang": "en", "#text": format!("{category} & circularity")},
                            {"@xml:lang": "zh", "#text": "测试"}
                        ]
                    },
                    "reference": null,
                    "value": 1.25,
                    "enabled": true
                }
            });
            let path = root.join(&relative);
            ensure_parent(&path).unwrap();
            fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
            documents.insert(relative, document);
        }
        fs::write(root.join("README.txt"), b"preserved auxiliary file\n").unwrap();
        documents
    }

    fn process_review_validator() -> jsonschema::Validator {
        const SCHEMA_PREFIX: &str = "assets/tidas/schemas/";
        const SCHEMA_BASE_URI: &str = "https://tiangong.earth/assets/tidas/schemas/";

        let schemas = bundled_assets()
            .into_iter()
            .filter(|asset| asset.kind == AssetKind::JsonSchema)
            .map(|asset| {
                let filename = asset.path.strip_prefix(SCHEMA_PREFIX).unwrap().to_owned();
                let schema: Value = serde_json::from_slice(asset.bytes).unwrap();
                (filename, schema)
            })
            .collect::<BTreeMap<_, _>>();
        let process_schema = schemas.get("tidas_processes.json").unwrap();
        let review_schema = process_schema
            .pointer("/properties/processDataSet/properties/modellingAndValidation/properties/validation/properties/review")
            .unwrap()
            .clone();
        let focused_schema = json!({
            "$id": format!("{SCHEMA_BASE_URI}process-review-test.json"),
            "$schema": "http://json-schema.org/draft-07/schema#",
            "type": "object",
            "properties": {"review": review_schema},
            "required": ["review"],
            "$defs": process_schema["$defs"].clone()
        });
        let resources = schemas.iter().map(|(filename, schema)| {
            (
                format!("{SCHEMA_BASE_URI}{filename}"),
                Resource::from_contents(schema.clone()),
            )
        });
        jsonschema::draft7::options()
            .with_resources(resources)
            .build(&focused_schema)
            .unwrap()
    }

    fn collect_validation_locations(error: &ValidationError<'_>, locations: &mut Vec<String>) {
        locations.push(error.instance_path().to_string());
        if let ValidationErrorKind::AnyOf { context } = error.kind() {
            for branch in context {
                for nested in branch {
                    collect_validation_locations(nested, locations);
                }
            }
        }
    }

    #[test]
    fn format_mapping_matches_the_frozen_python_oracle() {
        let cancellation = CancellationToken::default();
        let xml = br#"<root xmlns="urn:test" id="7"> before <item lang="en">A &amp; B</item><item lang="zh"><![CDATA[ C ]]></item> after </root>"#;
        let converted = format::xml_to_json(xml, &cancellation).unwrap();
        let value: Value = serde_json::from_slice(&converted).unwrap();
        assert_eq!(
            value,
            json!({
                "root": {
                    "@id": "7",
                    "@xmlns": "urn:test",
                    "#text": "before  after",
                    "item": [
                        {"@lang": "en", "#text": "A & B"},
                        {"@lang": "zh", "#text": "C"}
                    ]
                }
            })
        );

        let roundtrip = format::json_to_xml(&converted, &cancellation).unwrap();
        let reparsed: Value =
            serde_json::from_slice(&format::xml_to_json(&roundtrip, &cancellation).unwrap())
                .unwrap();
        assert_eq!(reparsed, value);
    }

    #[test]
    fn repeated_process_reviews_preserve_order_roundtrip_and_indexed_validation() {
        let cancellation = CancellationToken::default();
        let source = include_bytes!("../tests/fixtures/repeated-review-process.xml");
        let converted = format::xml_to_json(source, &cancellation).unwrap();
        let document: Value = serde_json::from_slice(&converted).unwrap();
        let review_path = "/processDataSet/modellingAndValidation/validation/review";
        let reviews = document.pointer(review_path).unwrap().as_array().unwrap();

        assert_eq!(reviews.len(), 2);
        assert_eq!(
            reviews[0].pointer("/common:reviewDetails/#text").unwrap(),
            "First review record"
        );
        assert_eq!(
            reviews[1].pointer("/common:reviewDetails/#text").unwrap(),
            "Second review record"
        );

        let validator = process_review_validator();
        validator
            .validate(&json!({"review": reviews.clone()}))
            .unwrap();
        validator
            .validate(&json!({"review": reviews[0].clone()}))
            .unwrap();

        let roundtrip_xml = format::json_to_xml(&converted, &cancellation).unwrap();
        let roundtrip_text = String::from_utf8(roundtrip_xml.clone()).unwrap();
        assert_eq!(roundtrip_text.matches("<review ").count(), 2);
        let reparsed: Value =
            serde_json::from_slice(&format::xml_to_json(&roundtrip_xml, &cancellation).unwrap())
                .unwrap();
        assert_eq!(reparsed.pointer(review_path), document.pointer(review_path));

        let invalid = json!({
            "review": [
                reviews[0].clone(),
                {"@type": "Dependent internal review"}
            ]
        });
        let mut locations = Vec::new();
        for error in validator.iter_errors(&invalid) {
            collect_validation_locations(&error, &mut locations);
        }
        assert!(
            locations.iter().any(|path| path.starts_with("/review/1")),
            "expected an indexed error for the second review, got {locations:?}"
        );
    }

    #[test]
    fn frozen_python_golden_fixture_matches_both_directions() {
        let cancellation = CancellationToken::default();
        let fixture: Value =
            serde_json::from_str(include_str!("../tests/fixtures/conversion-v1/golden.json"))
                .unwrap();
        assert_eq!(fixture["schema_version"], "tidas.conversion-golden.v1");
        assert_eq!(fixture["oracle"]["version"], "1.0.4");

        for case in fixture["xml_to_json"].as_array().unwrap() {
            let actual: Value = serde_json::from_slice(
                &format::xml_to_json(case["input"].as_str().unwrap().as_bytes(), &cancellation)
                    .unwrap(),
            )
            .unwrap();
            assert_eq!(actual, case["expected"], "{}", case["name"]);
        }
        for case in fixture["json_to_xml"].as_array().unwrap() {
            let input = serde_json::to_vec(&case["input"]).unwrap();
            let xml = format::json_to_xml(&input, &cancellation).unwrap();
            let actual: Value =
                serde_json::from_slice(&format::xml_to_json(&xml, &cancellation).unwrap()).unwrap();
            assert_eq!(actual, case["expected_reparsed"], "{}", case["name"]);
        }
    }

    #[test]
    fn illegal_xml_10_characters_are_rejected_in_both_directions() {
        let cancellation = CancellationToken::default();
        assert!(matches!(
            convert_json_to_xml(br#"{"root":"\u0001"}"#, &cancellation),
            Err(ConversionError::InvalidXmlCharacter(1))
        ));
        assert!(matches!(
            convert_xml_to_json(b"<root>\x01</root>", &cancellation),
            Err(ConversionError::InvalidXmlCharacter(1))
        ));
    }

    #[test]
    fn attribute_only_elements_are_serialized_without_character_content() {
        let xml = convert_json_to_xml(
            br#"{"root":{"child":{"@id":"1","@version":"00.00.001"}}}"#,
            &CancellationToken::default(),
        )
        .unwrap();
        let text = String::from_utf8(xml).unwrap();
        assert!(text.contains(r#"<child id="1" version="00.00.001"/>"#));
        assert!(!text.contains("<child id=\"1\" version=\"00.00.001\">\n"));
    }

    #[test]
    fn direct_flow_conversion_applies_schema_order_before_xsd_validation() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("tidas");
        let output = directory.path().join("ilcd");
        let flow_dir = input.join("flows");
        fs::create_dir_all(&flow_dir).unwrap();
        fs::write(
            flow_dir.join("scrambled.json"),
            br##"{
  "flowDataSet": {
    "@xmlns": "http://lca.jrc.it/ILCD/Flow",
    "@xmlns:common": "http://lca.jrc.it/ILCD/Common",
    "@version": "1.1",
    "flowProperties": {
      "flowProperty": {
        "@dataSetInternalID": "0",
        "referenceToFlowPropertyDataSet": {
          "@type": "flow property data set",
          "@refObjectId": "11111111-1111-4111-8111-111111111111",
          "@version": "01.00.000",
          "@uri": "../flowproperties/mass.xml"
        },
        "meanValue": "1"
      }
    },
    "flowInformation": {
      "dataSetInformation": {
        "common:UUID": "00000000-0000-4000-8000-000000000000",
        "name": {
          "baseName": {
            "@xml:lang": "en",
            "#text": "Schema-ordered flow"
          }
        }
      },
      "quantitativeReference": {
        "referenceToReferenceFlowProperty": "0"
      }
    }
  }
}"##,
        )
        .unwrap();

        convert_directory(&request(&input, &output, ConversionDirection::TidasToIlcd)).unwrap();

        let xml = fs::read_to_string(output.join("data/flows/scrambled.xml")).unwrap();
        assert!(xml.find("flowInformation").unwrap() < xml.find("flowProperties").unwrap());

        let issues = output.join("issues.jsonl");
        let validation = validate_ilcd_package(&ValidationRequest {
            input_dir: output.join("data"),
            issue_spool: Some(issues.clone()),
            cancellation: CancellationToken::default(),
            memory_budget: MemoryBudget::new(16 * 1024 * 1024),
            queue_capacity: 4,
            progress: None,
        })
        .unwrap();
        assert!(
            validation.summary.ok,
            "{:?}\n{}",
            validation.summary,
            fs::read_to_string(issues).unwrap()
        );
    }

    #[test]
    fn contact_conversion_uses_ilcd_sequence_instead_of_tidas_property_order() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("tidas");
        let output = directory.path().join("ilcd");
        let contact_dir = input.join("contacts");
        fs::create_dir_all(&contact_dir).unwrap();
        fs::write(
            contact_dir.join("scrambled.json"),
            br##"{
  "contactDataSet": {
    "@xmlns": "http://lca.jrc.it/ILCD/Contact",
    "@xmlns:common": "http://lca.jrc.it/ILCD/Common",
    "@version": "1.1",
    "contactInformation": {
      "dataSetInformation": {
        "common:UUID": "66666666-6666-4666-8666-666666666666",
        "common:shortName": {"@xml:lang": "en", "#text": "Steel team"},
        "common:name": {"@xml:lang": "en", "#text": "Steel team"},
        "contactAddress": {"@xml:lang": "en", "#text": "Zurich"},
        "email": "team@example.org",
        "telephone": "+41",
        "WWWAddress": "https://example.org"
      }
    },
    "administrativeInformation": {
      "publicationAndOwnership": {
        "common:dataSetVersion": "01.00.000"
      }
    }
  }
}"##,
        )
        .unwrap();

        convert_directory(&request(&input, &output, ConversionDirection::TidasToIlcd)).unwrap();
        let xml = fs::read_to_string(output.join("data/contacts/scrambled.xml")).unwrap();
        assert!(xml.find("telephone").unwrap() < xml.find("email").unwrap());
        assert!(xml.find("email").unwrap() < xml.find("WWWAddress").unwrap());
        let issues = output.join("issues.jsonl");
        let validation = validate_ilcd_package(&ValidationRequest {
            input_dir: output.join("data"),
            issue_spool: Some(issues.clone()),
            cancellation: CancellationToken::default(),
            memory_budget: MemoryBudget::new(16 * 1024 * 1024),
            queue_capacity: 4,
            progress: None,
        })
        .unwrap();
        assert!(
            validation.summary.ok,
            "{:?}\n{}",
            validation.summary,
            fs::read_to_string(issues).unwrap()
        );
    }

    #[test]
    fn representative_datasets_roundtrip_with_assets_and_stable_hashes() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("tidas");
        let ilcd_a = directory.path().join("ilcd-a");
        let ilcd_b = directory.path().join("ilcd-b");
        let restored = directory.path().join("restored");
        fs::create_dir_all(&input).unwrap();
        let expected = write_representative_tidas(&input);

        fs::create_dir_all(&ilcd_a).unwrap();
        fs::write(ilcd_a.join("existing.txt"), b"preserved output\n").unwrap();
        let first =
            convert_directory(&request(&input, &ilcd_a, ConversionDirection::TidasToIlcd)).unwrap();
        let mut single_slot = request(&input, &ilcd_b, ConversionDirection::TidasToIlcd);
        single_slot.queue_capacity = 1;
        let second = convert_directory(&single_slot).unwrap();

        assert_eq!(first.converted_file_count, 8);
        assert_eq!(first.copied_file_count, 1);
        assert!(first.asset_file_count > 0);
        assert!(first.peak_accounted_memory_bytes <= 32 * 1024 * 1024);
        assert!(ilcd_a.join("schemas/ILCD_ProcessDataSet.xsd").is_file());
        assert!(ilcd_a.join("stylesheets").is_dir());
        assert_eq!(
            fs::read(ilcd_a.join("existing.txt")).unwrap(),
            b"preserved output\n"
        );
        assert_ne!(first.output_tree_sha256, second.output_tree_sha256);

        fs::remove_file(ilcd_a.join("existing.txt")).unwrap();
        let clean_first =
            convert_directory(&request(&input, &ilcd_a, ConversionDirection::TidasToIlcd)).unwrap();
        assert_eq!(clean_first.output_tree_sha256, second.output_tree_sha256);

        let reverse = convert_directory(&request(
            &ilcd_a.join("data"),
            &restored,
            ConversionDirection::IlcdToTidas,
        ))
        .unwrap();
        assert_eq!(reverse.converted_file_count, 8);
        assert!(restored.join("schemas/tidas_processes.json").is_file());
        assert!(restored.join("schemas_zh/tidas_processes.json").is_file());
        assert!(restored.join("methodologies").is_dir());
        for (relative, expected_document) in expected {
            let actual: Value =
                serde_json::from_slice(&fs::read(restored.join("data").join(relative)).unwrap())
                    .unwrap();
            let mut expected_roundtrip = expected_document;
            let root = expected_roundtrip
                .as_object_mut()
                .unwrap()
                .values_mut()
                .next()
                .unwrap()
                .as_object_mut()
                .unwrap();
            root.insert("enabled".to_owned(), Value::String("true".to_owned()));
            root.insert("value".to_owned(), Value::String("1.25".to_owned()));
            assert_eq!(actual, expected_roundtrip);
        }

        let schema: Value = serde_json::from_str(CONVERSION_REPORT_JSON_SCHEMA_V1).unwrap();
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        for report in [clean_first, second, reverse] {
            let instance = serde_json::to_value(report).unwrap();
            let errors: Vec<_> = validator
                .iter_errors(&instance)
                .map(|error| error.to_string())
                .collect();
            assert!(errors.is_empty(), "{errors:?}");
        }
    }

    #[test]
    fn failed_conversion_preserves_existing_output_and_cleans_staging() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        fs::create_dir_all(&input).unwrap();
        fs::create_dir_all(&output).unwrap();
        fs::write(input.join("broken.json"), b"{").unwrap();
        fs::write(output.join("sentinel.txt"), b"original").unwrap();

        let error = convert_directory(&request(&input, &output, ConversionDirection::TidasToIlcd))
            .unwrap_err();
        assert!(matches!(error, ConversionError::Json(_)));
        assert_eq!(fs::read(output.join("sentinel.txt")).unwrap(), b"original");
        let leftovers: Vec<_> = fs::read_dir(directory.path())
            .unwrap()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry
                    .file_name()
                    .to_string_lossy()
                    .starts_with(".tidas-conversion-")
            })
            .collect();
        assert!(leftovers.is_empty(), "{leftovers:?}");
    }

    #[test]
    fn package_manifest_is_copied_and_dataset_envelopes_roundtrip_losslessly() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        let ilcd = directory.path().join("ilcd");
        let restored = directory.path().join("restored");
        fs::create_dir_all(input.join("flows")).unwrap();
        let manifest = json!({
            "format": "tiangong-tidas-package",
            "version": 2,
            "total_count": 1
        });
        let enveloped = json!({
            "flowDataSet": {
                "@version": "1.1",
                "name": {"baseName": "electricity"}
            },
            "version": "01.01.000",
            "json_tg": {
                "source": "data-foundry",
                "coordinates": [1, 2]
            }
        });
        fs::write(
            input.join("manifest.json"),
            serde_json::to_vec_pretty(&manifest).unwrap(),
        )
        .unwrap();
        fs::write(
            input.join("flows/enveloped.json"),
            serde_json::to_vec_pretty(&enveloped).unwrap(),
        )
        .unwrap();

        let forward =
            convert_directory(&request(&input, &ilcd, ConversionDirection::TidasToIlcd)).unwrap();
        assert_eq!(forward.converted_file_count, 1);
        assert_eq!(forward.copied_file_count, 1);
        assert_eq!(forward.envelope_sidecar_count, 1);
        assert!(ilcd.join("data/flows/enveloped.xml").is_file());
        assert!(
            ilcd.join("data/flows/enveloped.tidas-envelope.json")
                .is_file()
        );
        let copied_manifest: Value =
            serde_json::from_slice(&fs::read(ilcd.join("data/manifest.json")).unwrap()).unwrap();
        assert_eq!(copied_manifest, manifest);

        let reverse = convert_directory(&request(
            &ilcd.join("data"),
            &restored,
            ConversionDirection::IlcdToTidas,
        ))
        .unwrap();
        assert_eq!(reverse.converted_file_count, 1);
        assert_eq!(reverse.copied_file_count, 1);
        assert_eq!(reverse.envelope_sidecar_count, 1);
        assert!(
            !restored
                .join("data/flows/enveloped.tidas-envelope.json")
                .exists()
        );
        let actual: Value =
            serde_json::from_slice(&fs::read(restored.join("data/flows/enveloped.json")).unwrap())
                .unwrap();
        assert_eq!(actual, enveloped);
    }

    #[test]
    fn cancellation_budget_and_nested_output_fail_before_commit() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        fs::create_dir_all(&input).unwrap();
        fs::write(input.join("document.json"), br#"{"root":"value"}"#).unwrap();

        let cancelled_output = directory.path().join("cancelled");
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let mut cancelled = request(&input, &cancelled_output, ConversionDirection::TidasToIlcd);
        cancelled.cancellation = cancellation;
        assert!(matches!(
            convert_directory(&cancelled),
            Err(ConversionError::Runtime(RuntimeError::Cancelled))
        ));
        assert!(!cancelled_output.exists());

        let budget_output = directory.path().join("budget");
        let mut constrained = request(&input, &budget_output, ConversionDirection::TidasToIlcd);
        constrained.memory_budget = MemoryBudget::new(1);
        assert!(matches!(
            convert_directory(&constrained),
            Err(ConversionError::Runtime(
                RuntimeError::BudgetExceeded { .. }
            ))
        ));
        assert!(!budget_output.exists());

        let nested = input.join("generated");
        assert!(matches!(
            convert_directory(&request(&input, &nested, ConversionDirection::TidasToIlcd)),
            Err(ConversionError::OutputInsideInput(_))
        ));
        assert!(!nested.exists());
    }

    #[cfg(unix)]
    #[test]
    fn traversal_rejects_symlinks_without_publishing_output() {
        use std::os::unix::fs::symlink;

        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        fs::create_dir_all(&input).unwrap();
        fs::write(directory.path().join("external.json"), br#"{"root":null}"#).unwrap();
        symlink(
            directory.path().join("external.json"),
            input.join("linked.json"),
        )
        .unwrap();
        assert!(matches!(
            convert_directory(&request(&input, &output, ConversionDirection::TidasToIlcd)),
            Err(ConversionError::Symlink(_))
        ));
        assert!(!output.exists());
    }

    #[cfg(unix)]
    #[test]
    fn traversal_rejects_windows_incompatible_names() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        fs::create_dir_all(&input).unwrap();
        fs::write(input.join("bad:name.json"), br#"{"root":null}"#).unwrap();
        assert!(matches!(
            convert_directory(&request(&input, &output, ConversionDirection::TidasToIlcd)),
            Err(ConversionError::NonPortablePath(_))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn reverse_conversion_rejects_orphan_envelope_sidecars() {
        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        fs::create_dir_all(input.join("flows")).unwrap();
        fs::write(
            input.join("flows/orphan.tidas-envelope.json"),
            br#"{"version":"1"}"#,
        )
        .unwrap();
        assert!(matches!(
            convert_directory(&request(&input, &output, ConversionDirection::IlcdToTidas)),
            Err(ConversionError::OrphanEnvelopeSidecar(_))
        ));
        assert!(!output.exists());
    }

    #[test]
    fn checked_in_schema_matches_the_report_version() {
        let schema: Value = serde_json::from_str(CONVERSION_REPORT_JSON_SCHEMA_V1).unwrap();
        assert_eq!(
            schema["properties"]["schema_version"]["const"],
            CONVERSION_REPORT_SCHEMA_V1
        );
        assert_eq!(schema["additionalProperties"], false);
    }

    #[test]
    fn progress_reports_stable_phases_without_changing_results() {
        use std::sync::Mutex;

        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        let output = directory.path().join("output");
        fs::create_dir_all(&input).unwrap();
        fs::write(input.join("document.json"), br#"{"root":null}"#).unwrap();
        let observed = Arc::new(Mutex::new(Vec::new()));
        let reporter_observed = Arc::clone(&observed);
        let mut conversion = request(&input, &output, ConversionDirection::TidasToIlcd);
        conversion.progress = Some(ConversionProgressReporter::new(move |progress| {
            reporter_observed.lock().unwrap().push(progress.clone());
        }));

        let report = convert_directory(&conversion).unwrap();
        assert_eq!(report.converted_file_count, 1);
        let progress = observed.lock().unwrap();
        assert_eq!(
            progress
                .iter()
                .map(|event| event.phase.as_str())
                .collect::<Vec<_>>(),
            ["started", "hashing", "completed"]
        );
        assert_eq!(progress.last().unwrap().files_processed, 1);
        assert_eq!(progress.last().unwrap().converted_file_count, 1);
    }
}
