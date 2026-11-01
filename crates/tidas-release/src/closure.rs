use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

use crate::index::{DatasetEntry, DatasetIndex, contained, hex_digest};
use crate::{INLINE_ITEM_LIMIT, ReferenceClosureReportV1, ReleaseError, ReleaseRuntime};

pub const UNIT_PROFILE: &str = "unit-process-full-closure.v1";
pub const RESULT_ONLY_PROFILE: &str = "result-process-full-closure.v1";
pub const RESULT_PROFILE: &str = "standalone-lifecyclemodel-result-full-closure.v1";

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseProfile {
    UnitProcess,
    StandaloneResult,
    ResultProcess,
}

impl ReleaseProfile {
    #[must_use]
    pub const fn id(self) -> &'static str {
        match self {
            Self::UnitProcess => UNIT_PROFILE,
            Self::StandaloneResult => RESULT_PROFILE,
            Self::ResultProcess => RESULT_ONLY_PROFILE,
        }
    }
}

pub(crate) fn resolve(
    input_dir: &Path,
    index: &DatasetIndex,
    profile: ReleaseProfile,
    runtime: &ReleaseRuntime,
) -> Result<(Vec<DatasetEntry>, ReferenceClosureReportV1), ReleaseError> {
    let root_keys: Vec<String> = index
        .entries()
        .iter()
        .filter(|entry| match profile {
            ReleaseProfile::UnitProcess => entry.role == "unit_process",
            ReleaseProfile::ResultProcess => {
                entry.role == "result_process" && entry.dataset_type == "process"
            }
            ReleaseProfile::StandaloneResult => {
                matches!(entry.role.as_str(), "lifecycle_model" | "result_process")
            }
        })
        .map(DatasetEntry::key)
        .collect();
    if root_keys.is_empty() {
        return Err(ReleaseError::ProfileRootsMissing(profile.id().to_owned()));
    }
    let root_count = u64::try_from(root_keys.len()).map_err(|_| ReleaseError::SizeOverflow)?;
    let mut pending = root_keys;
    pending.reverse();
    let mut selected = BTreeMap::<String, DatasetEntry>::new();
    let mut reference_count = 0_u64;
    while let Some(key) = pending.pop() {
        runtime.cancellation.check()?;
        if selected.contains_key(&key) {
            continue;
        }
        let entry = index
            .get(&key)
            .ok_or_else(|| ReleaseError::ReferenceClosureMissing(key.clone()))?
            .clone();
        let path = contained(input_dir, &entry.relative_path)?;
        let metadata = fs::metadata(&path)?;
        let reserve = metadata
            .len()
            .checked_mul(4)
            .ok_or(ReleaseError::SizeOverflow)?;
        let _reservation = runtime.memory_budget.reserve(reserve)?;
        let bytes = fs::read(&path)?;
        let document: Value =
            serde_json::from_slice(&bytes).map_err(|source| ReleaseError::DatasetJson {
                path: path.clone(),
                source,
            })?;
        if profile == ReleaseProfile::ResultProcess
            && entry.role == "result_process"
            && document
                .pointer(
                    "/processDataSet/modellingAndValidation/LCIMethodAndAllocation/typeOfDataSet",
                )
                .and_then(Value::as_str)
                != Some("LCI result")
        {
            return Err(ReleaseError::DatasetIndexInvalid(format!(
                "Result-only root must be an LCI result: {}",
                entry.key()
            )));
        }
        let mut references = BTreeSet::new();
        walk_references(&document, "$", None, &mut references)?;
        reference_count = reference_count
            .checked_add(u64::try_from(references.len()).map_err(|_| ReleaseError::SizeOverflow)?)
            .ok_or(ReleaseError::SizeOverflow)?;
        for (referenced, location) in references.into_iter().rev() {
            if index.get(&referenced).is_none() {
                return Err(ReleaseError::ReferenceClosureMissing(format!(
                    "{} {location} -> {referenced}",
                    entry.key()
                )));
            }
            if !selected.contains_key(&referenced) {
                pending.push(referenced);
            }
        }
        selected.insert(key, entry);
    }

    let entries: Vec<DatasetEntry> = selected.into_values().collect();
    let (semantic_coverage, semantic_diagnostics) =
        semantic_coverage(input_dir, &entries, runtime)?;
    let dataset_count = u64::try_from(entries.len()).map_err(|_| ReleaseError::SizeOverflow)?;
    let all_keys: Vec<String> = entries.iter().map(DatasetEntry::key).collect();
    let closure_sha256 = hash_strings(&all_keys)?;
    let truncated = all_keys.len() > INLINE_ITEM_LIMIT;
    let dataset_keys = all_keys.into_iter().take(INLINE_ITEM_LIMIT).collect();
    Ok((
        entries,
        ReferenceClosureReportV1 {
            profile_id: profile.id().to_owned(),
            root_count,
            dataset_count,
            reference_count,
            closure_sha256,
            dataset_keys,
            dataset_keys_truncated: truncated,
            semantic_coverage,
            semantic_diagnostics,
        },
    ))
}

pub(crate) fn verify_result_contains_unit(
    unit: &[DatasetEntry],
    result: &[DatasetEntry],
) -> Result<(), ReleaseError> {
    let result_keys: BTreeSet<String> = result.iter().map(DatasetEntry::key).collect();
    for key in unit.iter().map(DatasetEntry::key) {
        if !result_keys.contains(&key) {
            return Err(ReleaseError::StandaloneMissingUnitClosure(key));
        }
    }
    Ok(())
}

fn walk_references(
    value: &Value,
    location: &str,
    parent_key: Option<&str>,
    output: &mut BTreeSet<(String, String)>,
) -> Result<(), ReleaseError> {
    match value {
        Value::Object(object) => {
            if !parent_key.is_some_and(is_preceding_version_reference)
                && let (Some(reference_id), Some(reference_type)) = (
                    object.get("@refObjectId").and_then(Value::as_str),
                    object.get("@type").and_then(Value::as_str),
                )
                && let Some(dataset_type) = reference_dataset_type(reference_type)
            {
                let version = object
                    .get("@version")
                    .and_then(Value::as_str)
                    .filter(|value| !value.is_empty())
                    .ok_or_else(|| {
                        ReleaseError::ReferenceVersionMissing(format!(
                            "{location} -> {reference_type} {reference_id}"
                        ))
                    })?;
                output.insert((
                    format!(
                        "{}:{}:{}",
                        dataset_type,
                        reference_id.to_lowercase(),
                        version
                    ),
                    location.to_owned(),
                ));
            }
            for (key, child) in object {
                walk_references(child, &format!("{location}/{key}"), Some(key), output)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                walk_references(child, &format!("{location}/{index}"), parent_key, output)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn is_preceding_version_reference(key: &str) -> bool {
    key.rsplit(':')
        .next()
        .is_some_and(|name| name.eq_ignore_ascii_case("referenceToPrecedingDataSetVersion"))
}

fn reference_dataset_type(value: &str) -> Option<&'static str> {
    match value.to_ascii_lowercase().as_str() {
        "contact data set" => Some("contact"),
        "flow data set" => Some("flow"),
        "flow property data set" => Some("flowproperty"),
        "unit group data set" => Some("unitgroup"),
        "process data set" => Some("process"),
        "source data set" => Some("source"),
        "lcia method data set" => Some("lciamethod"),
        "life cycle model data set" => Some("lifecyclemodel"),
        _ => None,
    }
}

fn hash_strings(values: &[String]) -> Result<String, ReleaseError> {
    let bytes = serde_json::to_vec(values)?;
    Ok(hex_digest(Sha256::digest(bytes)))
}

// Admission uses only frozen members of the selected, exact reference closure.
fn semantic_coverage(
    input_dir: &Path,
    entries: &[DatasetEntry],
    runtime: &ReleaseRuntime,
) -> Result<
    (
        Option<tidas_validation::SemanticCoverageV1>,
        Option<crate::ClosureSemanticDiagnosticsV1>,
    ),
    ReleaseError,
> {
    let mut flows = Vec::new();
    let mut reservations = Vec::new();
    for entry in entries.iter().filter(|e| e.dataset_type == "flow") {
        runtime.cancellation.check()?;
        let (document, _document_memory) = read_bound_document(input_dir, entry, runtime)?;
        let Some(uuid) = document
            .pointer("/flowDataSet/flowInformation/dataSetInformation/common:UUID")
            .and_then(Value::as_str)
        else {
            continue;
        };
        let Some(version) = document.pointer("/flowDataSet/administrativeInformation/publicationAndOwnership/common:dataSetVersion").and_then(Value::as_str) else { continue; };
        let kind = document
            .pointer("/flowDataSet/modellingAndValidation/LCIMethod/typeOfDataSet")
            .and_then(Value::as_str)
            .unwrap_or("");
        let cost = u64::try_from(256 + uuid.len() + version.len() + kind.len())
            .map_err(|_| ReleaseError::SizeOverflow)?;
        reservations.push(runtime.memory_budget.reserve(cost)?);
        flows.push(tidas_validation::ExactFlowEvidence {
            uuid: uuid.to_owned(),
            version: version.to_owned(),
            r#type: kind.to_owned(),
            content_hash: Some(entry.sha256.clone()),
        });
    }
    let mut coverage = None;
    let mut diagnostics = crate::ClosureSemanticDiagnosticsV1 {
        issue_count: 0,
        issues: vec![],
        truncated: false,
    };
    for entry in entries.iter().filter(|e| e.dataset_type == "process") {
        runtime.cancellation.check()?;
        let path = contained(input_dir, &entry.relative_path)?;
        let estimate = fs::metadata(&path)?
            .len()
            .checked_mul(64)
            .and_then(|v| v.checked_add(4096))
            .ok_or(ReleaseError::SizeOverflow)?;
        let _analysis_memory = runtime.memory_budget.reserve(estimate)?;
        let (document, _document_memory) = read_bound_document(input_dir, entry, runtime)?;
        let projection_bytes =
            tidas_validation::process_semantic_projection_memory_bytes(&document)
                .ok_or(ReleaseError::SizeOverflow)?;
        let _projection_memory = runtime.memory_budget.reserve(projection_bytes)?;
        let analysis = tidas_validation::analyze_process_semantics(&document, &flows);
        runtime.cancellation.check()?;
        append_diagnostics(
            &analysis,
            &entry.relative_path,
            runtime,
            &mut diagnostics,
            &mut reservations,
        )?;
        coverage
            .get_or_insert_with(tidas_validation::SemanticCoverageV1::default)
            .record(&analysis);
    }
    for entry in entries {
        let path = contained(input_dir, &entry.relative_path)?;
        if crate::index::sha256_file(&path, runtime)? != entry.sha256 {
            return Err(ReleaseError::DatasetFileHashMismatch(
                entry.relative_path.clone(),
            ));
        }
    }
    let diagnostics = coverage.as_ref().map(|_| diagnostics);
    Ok((coverage, diagnostics))
}
fn read_bound_document(
    input_dir: &Path,
    entry: &DatasetEntry,
    runtime: &ReleaseRuntime,
) -> Result<(Value, tidas_runtime::MemoryReservation), ReleaseError> {
    let path = contained(input_dir, &entry.relative_path)?;
    let estimate = fs::metadata(&path)?
        .len()
        .checked_mul(8)
        .and_then(|v| v.checked_add(4096))
        .ok_or(ReleaseError::SizeOverflow)?;
    let memory = runtime.memory_budget.reserve(estimate)?;
    let bytes = fs::read(&path)?;
    if hex_digest(Sha256::digest(&bytes)) != entry.sha256 {
        return Err(ReleaseError::DatasetFileHashMismatch(
            entry.relative_path.clone(),
        ));
    }
    let document = serde_json::from_slice(&bytes)
        .map_err(|source| ReleaseError::DatasetJson { path, source })?;
    Ok((document, memory))
}

fn append_diagnostics(
    analysis: &tidas_validation::ProcessSemanticAnalysis,
    relative_path: &str,
    runtime: &ReleaseRuntime,
    diagnostics: &mut crate::ClosureSemanticDiagnosticsV1,
    reservations: &mut Vec<tidas_runtime::MemoryReservation>,
) -> Result<(), ReleaseError> {
    for raw in &analysis.validation_issues {
        runtime.cancellation.check()?;
        diagnostics.issue_count = diagnostics
            .issue_count
            .checked_add(1)
            .ok_or(ReleaseError::SizeOverflow)?;
        if diagnostics.issues.len() == INLINE_ITEM_LIMIT {
            diagnostics.truncated = true;
            continue;
        }
        let location = raw["path"]
            .as_array()
            .expect("native path is an array")
            .iter()
            .map(|part| {
                part.as_str()
                    .map_or_else(|| part.to_string(), str::to_owned)
            })
            .collect::<Vec<_>>()
            .join("/");
        let code = raw["code"].as_str().expect("native finding code");
        let mut issue = tidas_validation::ValidationIssueV1::error(
            code,
            "processes",
            relative_path,
            location,
            code,
        );
        issue
            .context
            .insert("profile".to_owned(), serde_json::json!(analysis.profile));
        issue
            .context
            .insert("params".to_owned(), raw["params"].clone());
        let cost = u64::try_from(serde_json::to_vec(&issue)?.len())
            .map_err(|_| ReleaseError::SizeOverflow)?;
        reservations.push(
            runtime
                .memory_budget
                .reserve(cost.checked_mul(4).ok_or(ReleaseError::SizeOverflow)?)?,
        );
        diagnostics.issues.push(issue);
    }
    Ok(())
}
