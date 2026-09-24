mod common;
mod contact_source;
mod lifecycle;
mod process;
mod unit_flow;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{BufReader, BufWriter, Read, Write};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tempfile::{Builder, TempDir};
use thiserror::Error;
use tidas_conversion::{
    ConversionDirection, ConversionError, ConversionReportV1, ConversionRequest, convert_directory,
};
use tidas_runtime::{CancellationToken, MemoryBudget, RuntimeError};
use walkdir::WalkDir;

use crate::normalization::{CanonicalFlow, FlowNormalizationError, normalize_flow};
use crate::store::{CanonicalStore, StoreError};

use self::process::ProcessWriteError;

pub(crate) fn contact_id_for_import() -> String {
    common::contact_id()
}

pub(crate) fn format_source_id_for_import() -> String {
    common::format_source_id()
}

pub(crate) fn compliance_source_id_for_import() -> String {
    common::compliance_source_id()
}

pub const IMPORT_PACKAGE_REPORT_SCHEMA_V1: &str = "tidas.import-package-report.v1";
pub const IMPORT_PACKAGE_REPORT_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/import-package-report.v1.schema.json"
));
const CATEGORY_ORDER: [&str; 8] = [
    "contacts",
    "sources",
    "unitgroups",
    "flowproperties",
    "flows",
    "processes",
    "lciamethods",
    "lifecyclemodels",
];
const HASH_BUFFER_BYTES: usize = 1024 * 1024;

pub struct TidasWriteRequest<'a> {
    pub store: &'a CanonicalStore,
    pub output_dir: &'a Path,
    pub cancellation: &'a CancellationToken,
    pub memory_budget: &'a MemoryBudget,
}

pub struct IlcdWriteRequest<'a> {
    pub store: &'a CanonicalStore,
    pub output_dir: &'a Path,
    pub cancellation: &'a CancellationToken,
    pub memory_budget: &'a MemoryBudget,
    pub queue_capacity: usize,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct PackageWriteReportV1 {
    pub schema_version: String,
    pub output_format: String,
    pub object_counts: BTreeMap<String, u64>,
    pub output_bytes: u64,
    pub output_tree_sha256: String,
    pub asset_fingerprint: String,
    pub peak_accounted_memory_bytes: u64,
}

#[derive(Clone, Debug)]
pub struct IlcdWriteReportV1 {
    pub canonical_tidas: PackageWriteReportV1,
    pub conversion: ConversionReportV1,
}

pub fn write_tidas_package(
    request: &TidasWriteRequest<'_>,
) -> Result<PackageWriteReportV1, PackageWriteError> {
    request.cancellation.check()?;
    reject_unsupported_entities(request.store)?;
    let flows = preflight_flows(request.store, request.cancellation)?;
    let staging = StagedDirectory::new(request.output_dir)?;
    for category in CATEGORY_ORDER {
        fs::create_dir_all(staging.path().join(category))?;
    }
    let mut counts = BTreeMap::new();
    let (contact_id, contact) = contact_source::contact();
    write_dataset(staging.path(), "contacts", &contact_id, &contact, request)?;
    counts.insert("contacts".to_owned(), 1);
    for (id, source) in contact_source::sources() {
        write_dataset(staging.path(), "sources", &id, &source, request)?;
        *counts.entry("sources".to_owned()).or_default() += 1;
    }
    write_entities(request, staging.path(), "contacts", &mut counts, |entity| {
        Ok(contact_source::canonical_contact(entity))
    })?;
    write_entities(request, staging.path(), "sources", &mut counts, |entity| {
        Ok(contact_source::canonical_source(entity))
    })?;
    write_entities(
        request,
        staging.path(),
        "unitgroups",
        &mut counts,
        |entity| Ok(unit_flow::unit_group(entity)),
    )?;
    write_entities(
        request,
        staging.path(),
        "flowproperties",
        &mut counts,
        |entity| Ok(unit_flow::flow_property(entity)),
    )?;
    write_flow_entities(request, staging.path(), &flows, &mut counts)?;
    write_process_entities(request, staging.path(), &mut counts)?;
    write_entities(
        request,
        staging.path(),
        "lifecyclemodels",
        &mut counts,
        |entity| Ok(lifecycle::lifecycle_model(entity)),
    )?;
    let (output_bytes, output_tree_sha256) = hash_tree(staging.path(), request)?;
    let report = PackageWriteReportV1 {
        schema_version: IMPORT_PACKAGE_REPORT_SCHEMA_V1.to_owned(),
        output_format: "tidas-json".to_owned(),
        object_counts: counts,
        output_bytes,
        output_tree_sha256,
        asset_fingerprint: tidas_assets::asset_fingerprint()?,
        peak_accounted_memory_bytes: request.memory_budget.peak(),
    };
    request.cancellation.check()?;
    staging.commit()?;
    Ok(report)
}

fn preflight_flows(
    store: &CanonicalStore,
    cancellation: &CancellationToken,
) -> Result<Vec<CanonicalFlow>, PackageWriteError> {
    store
        .iter_type("flows")?
        .map(|entity| {
            cancellation.check()?;
            Ok(normalize_flow(&entity?)?)
        })
        .collect()
}

fn write_flow_entities(
    request: &TidasWriteRequest<'_>,
    root: &Path,
    flows: &[CanonicalFlow],
    counts: &mut BTreeMap<String, u64>,
) -> Result<(), PackageWriteError> {
    for flow in flows {
        request.cancellation.check()?;
        let dataset = unit_flow::flow(flow);
        write_dataset(root, "flows", &flow.id, &dataset, request)?;
        *counts.entry("flows".to_owned()).or_default() += 1;
    }
    Ok(())
}

pub fn write_ilcd_package(
    request: &IlcdWriteRequest<'_>,
) -> Result<IlcdWriteReportV1, PackageWriteError> {
    request.cancellation.check()?;
    let parent = request
        .output_dir
        .parent()
        .unwrap_or_else(|| Path::new("."));
    fs::create_dir_all(parent)?;
    let canonical = Builder::new()
        .prefix(".tidas-import-canonical-")
        .tempdir_in(parent)?;
    let canonical_root = canonical.path().join("tidas");
    let canonical_tidas = write_tidas_package(&TidasWriteRequest {
        store: request.store,
        output_dir: &canonical_root,
        cancellation: request.cancellation,
        memory_budget: request.memory_budget,
    })?;
    let conversion = convert_directory(&ConversionRequest {
        input_dir: canonical_root,
        output_dir: request.output_dir.to_path_buf(),
        direction: ConversionDirection::TidasToIlcd,
        cancellation: request.cancellation.clone(),
        memory_budget: request.memory_budget.clone(),
        queue_capacity: request.queue_capacity,
        progress: None,
    })?;
    Ok(IlcdWriteReportV1 {
        canonical_tidas,
        conversion,
    })
}

fn write_entities(
    request: &TidasWriteRequest<'_>,
    root: &Path,
    category: &str,
    counts: &mut BTreeMap<String, u64>,
    build: impl Fn(&crate::model::CanonicalEntity) -> Result<Value, PackageWriteError>,
) -> Result<(), PackageWriteError> {
    for entity in request.store.iter_type(category)? {
        request.cancellation.check()?;
        let entity = entity?;
        let dataset = build(&entity)?;
        write_dataset(root, category, &entity.internal_id, &dataset, request)?;
        *counts.entry(category.to_owned()).or_default() += 1;
    }
    Ok(())
}

fn write_process_entities(
    request: &TidasWriteRequest<'_>,
    root: &Path,
    counts: &mut BTreeMap<String, u64>,
) -> Result<(), PackageWriteError> {
    for entity in request.store.iter_type("processes")? {
        request.cancellation.check()?;
        let entity = entity?;
        let mut declared_reference = None;
        let mut output_reference = None;
        let mut first_reference = None;
        for (index, exchange) in request
            .store
            .iter_process_exchanges(&entity.internal_id)?
            .enumerate()
        {
            request.cancellation.check()?;
            let exchange = exchange?;
            let reference = (
                index.saturating_add(1).to_string(),
                process::functional_unit(&exchange),
            );
            first_reference.get_or_insert_with(|| reference.clone());
            if declared_reference.is_none()
                && exchange
                    .get("isQuantitativeReference")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                declared_reference = Some(reference.clone());
            }
            if output_reference.is_none()
                && !exchange
                    .get("isInput")
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            {
                output_reference = Some(reference);
            }
        }
        let quantitative_reference = if let Some(reference) = entity.raw.get("ilcdNonFlowReference")
        {
            reference.clone()
        } else {
            let (reference, functional_unit) = declared_reference
                .or(output_reference)
                .or(first_reference)
                .ok_or_else(|| PackageWriteError::ProcessNoExchanges(entity.internal_id.clone()))?;
            json!({
                "@type": "Reference flow(s)",
                "referenceToReferenceFlow": reference,
                "functionalUnitOrOther": common::localized(&functional_unit),
            })
        };
        let base = process::process_base(&entity, &quantitative_reference)?;
        write_process_dataset(root, &entity.internal_id, &base, request)?;
        *counts.entry("processes".to_owned()).or_default() += 1;
    }
    Ok(())
}

fn write_process_dataset(
    root: &Path,
    process_id: &str,
    base: &Value,
    request: &TidasWriteRequest<'_>,
) -> Result<(), PackageWriteError> {
    let dataset = base
        .get("processDataSet")
        .ok_or(PackageWriteError::ProcessBaseShape)?;
    let mut dataset_bytes = serde_json::to_vec(dataset)?;
    if dataset_bytes.pop() != Some(b'}') {
        return Err(PackageWriteError::ProcessBaseShape);
    }
    let accounted =
        u64::try_from(dataset_bytes.len()).map_err(|_| PackageWriteError::SizeOverflow)?;
    let _base_reservation = request.memory_budget.reserve(accounted)?;
    let path = root.join("processes").join(format!("{process_id}.json"));
    let mut writer = BufWriter::new(File::create(path)?);
    writer.write_all(br#"{"processDataSet":"#)?;
    writer.write_all(&dataset_bytes)?;
    writer.write_all(br#","exchanges":{"exchange":["#)?;
    let mut first = true;
    for (index, exchange) in request
        .store
        .iter_process_exchanges(process_id)?
        .enumerate()
    {
        request.cancellation.check()?;
        let exchange = exchange?;
        let item = process::exchange_item(&exchange, &index.saturating_add(1).to_string())?;
        let bytes = serde_json::to_vec(&item)?;
        let accounted = u64::try_from(bytes.len()).map_err(|_| PackageWriteError::SizeOverflow)?;
        let _reservation = request.memory_budget.reserve(accounted)?;
        if !first {
            writer.write_all(b",")?;
        }
        writer.write_all(&bytes)?;
        first = false;
    }
    writer.write_all(b"]}}}\n")?;
    writer.flush()?;
    Ok(())
}

fn write_dataset(
    root: &Path,
    category: &str,
    id: &str,
    dataset: &Value,
    request: &TidasWriteRequest<'_>,
) -> Result<(), PackageWriteError> {
    let mut bytes = serde_json::to_vec_pretty(dataset)?;
    bytes.push(b'\n');
    let accounted = u64::try_from(bytes.len()).map_err(|_| PackageWriteError::SizeOverflow)?;
    let _reservation = request.memory_budget.reserve(accounted)?;
    let path = root.join(category).join(format!("{id}.json"));
    let mut writer = BufWriter::new(File::create(path)?);
    writer.write_all(&bytes)?;
    writer.flush()?;
    Ok(())
}

fn reject_unsupported_entities(store: &CanonicalStore) -> Result<(), PackageWriteError> {
    for category in ["lciamethods"] {
        if store.counts().get(category).copied().unwrap_or(0) > 0 {
            return Err(PackageWriteError::UnsupportedCanonicalType(
                category.to_owned(),
            ));
        }
    }
    for (category, reserved) in [
        ("contacts", vec![common::contact_id()]),
        (
            "sources",
            vec![common::format_source_id(), common::compliance_source_id()],
        ),
    ] {
        for id in reserved {
            if store.get(category, &id)?.is_some() {
                return Err(PackageWriteError::ReservedIdentifier { category, id });
            }
        }
    }
    Ok(())
}

fn hash_tree(
    root: &Path,
    request: &TidasWriteRequest<'_>,
) -> Result<(u64, String), PackageWriteError> {
    let buffer_bytes =
        u64::try_from(HASH_BUFFER_BYTES).map_err(|_| PackageWriteError::SizeOverflow)?;
    let _reservation = request.memory_budget.reserve(buffer_bytes)?;
    let mut buffer = vec![0_u8; HASH_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    let mut total = 0_u64;
    let entries = WalkDir::new(root)
        .follow_links(false)
        .sort_by_file_name()
        .into_iter();
    for entry in entries {
        request.cancellation.check()?;
        let entry = entry?;
        if !entry.file_type().is_file() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| PackageWriteError::OutsideOutput(entry.path().to_path_buf()))?;
        let normalized = relative.to_string_lossy().replace('\\', "/");
        hasher.update(normalized.as_bytes());
        hasher.update([0]);
        let mut reader = BufReader::new(File::open(entry.path())?);
        loop {
            request.cancellation.check()?;
            let read = reader.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            total = total
                .checked_add(u64::try_from(read).map_err(|_| PackageWriteError::SizeOverflow)?)
                .ok_or(PackageWriteError::SizeOverflow)?;
            hasher.update(&buffer[..read]);
        }
        hasher.update([0xff]);
    }
    let digest = hasher.finalize();
    let mut hash = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut hash, "{byte:02x}").expect("writing to a String cannot fail");
    }
    Ok((total, hash))
}

struct StagedDirectory {
    target: PathBuf,
    staging: TempDir,
}

impl StagedDirectory {
    fn new(target: &Path) -> Result<Self, PackageWriteError> {
        let parent = target.parent().unwrap_or_else(|| Path::new("."));
        fs::create_dir_all(parent)?;
        let staging = Builder::new()
            .prefix(".tidas-import-output-")
            .tempdir_in(parent)?;
        Ok(Self {
            target: target.to_path_buf(),
            staging,
        })
    }

    fn path(&self) -> &Path {
        self.staging.path()
    }

    fn commit(self) -> Result<(), PackageWriteError> {
        let parent = self.target.parent().unwrap_or_else(|| Path::new("."));
        if !self.target.exists() {
            fs::rename(self.staging.path(), &self.target)?;
            return Ok(());
        }
        if !self.target.is_dir() {
            return Err(PackageWriteError::OutputNotDirectory(self.target));
        }
        let backup = Builder::new()
            .prefix(".tidas-import-backup-")
            .tempdir_in(parent)?;
        let previous = backup.path().join("previous");
        fs::rename(&self.target, &previous)?;
        if let Err(source) = fs::rename(self.staging.path(), &self.target) {
            let restore = fs::rename(&previous, &self.target);
            return match restore {
                Ok(()) => Err(PackageWriteError::Io(source)),
                Err(restore) => Err(PackageWriteError::CommitRollback { source, restore }),
            };
        }
        Ok(())
    }
}

#[derive(Debug, Error)]
pub enum PackageWriteError {
    #[error("output path exists and is not a directory: {0}")]
    OutputNotDirectory(PathBuf),
    #[error("canonical entity type has no native TIDAS writer yet: {0}")]
    UnsupportedCanonicalType(String),
    #[error("canonical {category} entity uses reserved import dependency id {id}")]
    ReservedIdentifier { category: &'static str, id: String },
    #[error("process {0} has no canonical exchanges")]
    ProcessNoExchanges(String),
    #[error("native process base dataset has an invalid shape")]
    ProcessBaseShape,
    #[error("output path escaped staging root: {0}")]
    OutsideOutput(PathBuf),
    #[error("package size overflow")]
    SizeOverflow,
    #[error(
        "failed to commit output and restore previous output: commit={source}; restore={restore}"
    )]
    CommitRollback {
        source: std::io::Error,
        restore: std::io::Error,
    },
    #[error("package writer I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("package writer JSON failed: {0}")]
    Json(#[from] serde_json::Error),
    #[error("package writer traversal failed: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("package writer runtime failed: {0}")]
    Runtime(#[from] RuntimeError),
    #[error("canonical store failed: {0}")]
    Store(#[from] StoreError),
    #[error("flow normalization/preflight failed: {0}")]
    FlowPreflight(#[from] FlowNormalizationError),
    #[error("process writer failed: {0}")]
    Process(#[from] ProcessWriteError),
    #[error("asset verification failed: {0}")]
    Asset(#[from] tidas_assets::AssetError),
    #[error("ILCD conversion failed: {0}")]
    Conversion(#[from] ConversionError),
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;
    use tidas_validation::{ValidationRequest, validate_tidas_package};

    use super::*;
    use crate::adapters::{AdapterContext, SourceAdapter};
    use crate::report::IssueSpool;

    #[test]
    fn simapro_store_writes_schema_valid_deterministic_tidas_and_ilcd() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.csv");
        fs::write(
            &source,
            b"{SimaPro 9.5}\n{CSV separator: semicolon}\n\nProcess\n\nProcess name\nSteel production\n\nComment\nfixture\n\nProducts\nSteel | production route | GLO;kg;1\n\nEmissions to air\nCarbon dioxide;air;kg;2.5\n\nEnd\n",
        )
        .unwrap();
        let cancellation = CancellationToken::default();
        let memory_budget = MemoryBudget::new(32 * 1024 * 1024);
        let mut store = CanonicalStore::create(Some(directory.path())).unwrap();
        let mut issues = IssueSpool::new(Vec::new(), 64 * 1024);
        crate::adapters::SimaProCsvAdapter
            .read(
                &AdapterContext {
                    source: &source,
                    cancellation: &cancellation,
                    memory_budget: &memory_budget,
                    max_entry_bytes: 1024 * 1024,
                },
                &mut store,
                &mut issues,
            )
            .unwrap();
        issues.finish().unwrap();

        let first = directory.path().join("first");
        let second = directory.path().join("second");
        let first_report = write_tidas_package(&TidasWriteRequest {
            store: &store,
            output_dir: &first,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        })
        .unwrap();
        let second_report = write_tidas_package(&TidasWriteRequest {
            store: &store,
            output_dir: &second,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        })
        .unwrap();
        assert_eq!(
            first_report.output_tree_sha256,
            second_report.output_tree_sha256
        );
        assert_eq!(first_report.object_counts["processes"], 1);
        assert_eq!(first_report.object_counts["flows"], 2);
        assert_eq!(first_report.object_counts["unitgroups"], 1);
        assert_eq!(first_report.object_counts["flowproperties"], 1);
        let tidas_issues = directory.path().join("tidas-issues.jsonl");
        let validation = validate_tidas_package(&ValidationRequest {
            input_dir: first.clone(),
            issue_spool: Some(tidas_issues.clone()),
            cancellation: cancellation.clone(),
            memory_budget: memory_budget.clone(),
            queue_capacity: 2,
            progress: None,
        })
        .unwrap();
        assert!(
            validation.summary.ok,
            "validation summary: {:?}; issues: {}",
            validation.summary,
            fs::read_to_string(tidas_issues).unwrap_or_default(),
        );
        let flow_property = fs::read_to_string(first.join("flowproperties").join(format!(
            "{}.json",
            common::stable_id("tidas-tools/import/flowproperty/kg")
        )))
        .unwrap();
        assert!(flow_property.contains("Amount in kg"));

        let ilcd = directory.path().join("ilcd");
        let ilcd_report = write_ilcd_package(&IlcdWriteRequest {
            store: &store,
            output_dir: &ilcd,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
            queue_capacity: 2,
        })
        .unwrap();
        assert!(ilcd_report.conversion.converted_file_count > 0);
        assert!(ilcd.join("data/processes").is_dir());
        let ilcd_issues = directory.path().join("ilcd-issues.jsonl");
        let ilcd_validation = tidas_validation::validate_ilcd_package(&ValidationRequest {
            input_dir: ilcd.clone(),
            issue_spool: Some(ilcd_issues.clone()),
            cancellation: cancellation.clone(),
            memory_budget: memory_budget.clone(),
            queue_capacity: 2,
            progress: None,
        })
        .unwrap();
        assert!(
            ilcd_validation.summary.ok,
            "{}",
            fs::read_to_string(ilcd_issues).unwrap_or_default()
        );
        assert_eq!(memory_budget.used(), 0);
    }

    #[test]
    fn cancellation_preserves_existing_output() {
        let directory = tempdir().unwrap();
        let output = directory.path().join("output");
        fs::create_dir(&output).unwrap();
        fs::write(output.join("sentinel"), b"keep").unwrap();
        let store = CanonicalStore::create(Some(directory.path())).unwrap();
        let cancellation = CancellationToken::default();
        cancellation.cancel();
        let error = write_tidas_package(&TidasWriteRequest {
            store: &store,
            output_dir: &output,
            cancellation: &cancellation,
            memory_budget: &MemoryBudget::new(1024 * 1024),
        })
        .unwrap_err();
        assert!(matches!(
            error,
            PackageWriteError::Runtime(RuntimeError::Cancelled)
        ));
        assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
    }

    #[test]
    fn flow_preflight_rejects_missing_source_name_facts_before_publication() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.csv");
        fs::write(
            &source,
            b"{SimaPro 9.5}\n{CSV separator: semicolon}\n\nProcess\n\nProcess name\nSteel production\n\nProducts\nSteel;kg;1\n\nEnd\n",
        )
        .unwrap();
        let cancellation = CancellationToken::default();
        let memory_budget = MemoryBudget::new(8 * 1024 * 1024);
        let mut store = CanonicalStore::create(Some(directory.path())).unwrap();
        let mut issues = IssueSpool::new(Vec::new(), 64 * 1024);
        crate::adapters::SimaProCsvAdapter
            .read(
                &AdapterContext {
                    source: &source,
                    cancellation: &cancellation,
                    memory_budget: &memory_budget,
                    max_entry_bytes: 1024 * 1024,
                },
                &mut store,
                &mut issues,
            )
            .unwrap();
        issues.finish().unwrap();
        let output = directory.path().join("output");
        let error = write_tidas_package(&TidasWriteRequest {
            store: &store,
            output_dir: &output,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        })
        .unwrap_err();
        let message = error.to_string();
        assert!(matches!(error, PackageWriteError::FlowPreflight(_)));
        assert!(message.contains("simapro-csv:Products"));
        assert!(message.contains("CanonicalFlow.name.treatmentStandardsRoutes"));
        assert!(!output.exists());
    }

    #[test]
    fn extension_identity_survives_tidas_ilcd_tidas_round_trip() {
        let directory = tempdir().unwrap();
        let flow_id = "11111111-1111-4111-8111-111111111111";
        let mut store = CanonicalStore::create(Some(directory.path())).unwrap();
        store
            .add(&crate::model::CanonicalEntity {
                entity_type: "flows".to_owned(),
                internal_id: flow_id.to_owned(),
                external_id: None,
                name: Some("Carbon dioxide".to_owned()),
                category_path: vec![
                    "Emissions".to_owned(),
                    "Emissions to air".to_owned(),
                    "Emissions to non-urban air high stack".to_owned(),
                ],
                raw: serde_json::Map::from_iter([
                    (
                        "flowType".to_owned(),
                        Value::String("ELEMENTARY_FLOW".to_owned()),
                    ),
                    ("unitName".to_owned(), Value::String("kg".to_owned())),
                    (
                        "sourceTrace".to_owned(),
                        serde_json::json!({
                            "format": "fixture",
                            "sourceObject": "extension-round-trip"
                        }),
                    ),
                ]),
            })
            .unwrap();
        let cancellation = CancellationToken::default();
        let memory_budget = MemoryBudget::new(16 * 1024 * 1024);
        let ilcd = directory.path().join("ilcd");
        write_ilcd_package(&IlcdWriteRequest {
            store: &store,
            output_dir: &ilcd,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
            queue_capacity: 2,
        })
        .unwrap();
        let mut round_trip_store = CanonicalStore::create(Some(directory.path())).unwrap();
        let mut issues = IssueSpool::new(Vec::new(), 64 * 1024);
        crate::adapters::IlcdAdapter
            .read(
                &AdapterContext {
                    source: &ilcd,
                    cancellation: &cancellation,
                    memory_budget: &memory_budget,
                    max_entry_bytes: 1024 * 1024,
                },
                &mut round_trip_store,
                &mut issues,
            )
            .unwrap();
        issues.finish().unwrap();
        let round_trip = round_trip_store.get("flows", flow_id).unwrap().unwrap();
        let normalized = normalize_flow(&round_trip).unwrap();
        assert_eq!(normalized.classification.taxonomy_id, "tidas-ef-extension");
        assert_eq!(normalized.classification.taxonomy_version, "1");
        assert_eq!(
            normalized.classification.extension_node_id.as_deref(),
            Some("1.3.12")
        );
        assert_eq!(
            normalized.classification.categories.last().unwrap().label,
            "Emissions to non-urban air high stack"
        );
    }
}
