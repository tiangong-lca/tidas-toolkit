//! Native, bounded and deterministic TIDAS release operations.

mod archive;
mod closure;
mod conversion;
mod index;
mod transaction;

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tidas_conversion::ConversionError;
use tidas_runtime::{CancellationToken, MemoryBudget, RuntimeError};
use tidas_validation::{ValidationError, ValidationSummaryV1};

pub use closure::{RESULT_ONLY_PROFILE, RESULT_PROFILE, ReleaseProfile, UNIT_PROFILE};

pub const RELEASE_REPORT_SCHEMA_V2: &str = "tidas.release-report.v2";
pub const RELEASE_REPORT_JSON_SCHEMA_V2: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/release-report.v2.schema.json"
));
pub const RELEASE_REPORT_SCHEMA_V1: &str = "tidas.release-report.v1";
pub const RELEASE_REPORT_JSON_SCHEMA_V1: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/contracts/release-report.v1.schema.json"
));

const INLINE_ITEM_LIMIT: usize = 256;

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseAction {
    BuildPackages,
    ConvertIlcd,
    SemanticRoundtrip,
    ValidateClosure,
    ValidateIlcd,
    ValidateTidas,
}

#[derive(Clone, Debug)]
pub struct ReleaseRuntime {
    pub cancellation: CancellationToken,
    pub memory_budget: MemoryBudget,
    pub queue_capacity: usize,
}

#[derive(Clone, Debug)]
pub enum ReleaseRequest {
    BuildPackages {
        tidas_dir: PathBuf,
        dataset_index: PathBuf,
        output_dir: PathBuf,
        issue_spool: Option<PathBuf>,
    },
    BuildResultPackages {
        tidas_dir: PathBuf,
        dataset_index: PathBuf,
        output_dir: PathBuf,
        issue_spool: Option<PathBuf>,
    },
    ConvertIlcd {
        input_dir: PathBuf,
        output_dir: PathBuf,
    },
    SemanticRoundtrip {
        tidas_dir: PathBuf,
        ilcd_dir: PathBuf,
    },
    ValidateClosure {
        input_dir: PathBuf,
        dataset_index: PathBuf,
        profile: ReleaseProfile,
    },
    ValidateIlcd {
        input_dir: PathBuf,
    },
    ValidateTidas {
        input_dir: PathBuf,
    },
}

impl ReleaseRequest {
    #[must_use]
    pub const fn action(&self) -> ReleaseAction {
        match self {
            Self::BuildPackages { .. } | Self::BuildResultPackages { .. } => {
                ReleaseAction::BuildPackages
            }
            Self::ConvertIlcd { .. } => ReleaseAction::ConvertIlcd,
            Self::SemanticRoundtrip { .. } => ReleaseAction::SemanticRoundtrip,
            Self::ValidateClosure { .. } => ReleaseAction::ValidateClosure,
            Self::ValidateIlcd { .. } => ReleaseAction::ValidateIlcd,
            Self::ValidateTidas { .. } => ReleaseAction::ValidateTidas,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseReportV1 {
    pub schema_version: String,
    pub action: ReleaseAction,
    pub ok: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub closure: Option<ReferenceClosureReportV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub conversion: Option<IlcdConversionReportV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub validation: Option<ReleaseValidationReportV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub roundtrip: Option<SemanticRoundtripReportV1>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub build: Option<ReleaseBuildReportV1>,
    pub peak_accounted_memory_bytes: u64,
}

impl ReleaseReportV1 {
    fn new(action: ReleaseAction) -> Self {
        Self {
            schema_version: RELEASE_REPORT_SCHEMA_V1.to_owned(),
            action,
            ok: true,
            closure: None,
            conversion: None,
            validation: None,
            roundtrip: None,
            build: None,
            peak_accounted_memory_bytes: 0,
        }
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceClosureReportV1 {
    pub profile_id: String,
    pub root_count: u64,
    pub dataset_count: u64,
    pub reference_count: u64,
    pub closure_sha256: String,
    pub dataset_keys: Vec<String>,
    pub dataset_keys_truncated: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_coverage: Option<tidas_validation::SemanticCoverageV1>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub semantic_diagnostics: Option<ClosureSemanticDiagnosticsV1>,
}

/// Bounded path-specific findings for selected exact Process members.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ClosureSemanticDiagnosticsV1 {
    pub issue_count: u64,
    pub issues: Vec<tidas_validation::ValidationIssueV1>,
    pub truncated: bool,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct IlcdConversionReportV1 {
    pub dataset_count: u64,
    pub input_bytes: u64,
    pub output_bytes: u64,
    pub conversion_set_sha256: String,
    pub output_tree_sha256: String,
    pub asset_fingerprint: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct RoundtripMismatchV1 {
    pub path: String,
    pub location: String,
    pub code: String,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SemanticRoundtripReportV1 {
    pub ok: bool,
    pub dataset_count: u64,
    pub mismatch_count: u64,
    pub semantic_set_sha256: String,
    pub mismatches: Vec<RoundtripMismatchV1>,
    pub mismatches_truncated: bool,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum ReleaseDataFormat {
    Ilcd,
    Tidas,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseValidationReportV1 {
    pub format: ReleaseDataFormat,
    pub summary: ValidationSummaryV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseArtifactV1 {
    pub path: String,
    pub media_type: String,
    pub sha256: String,
    pub bytes: u64,
    pub member_count: u64,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleasePackageV1 {
    pub profile_id: String,
    pub format: ReleaseDataFormat,
    pub self_contained: bool,
    pub closure_sha256: String,
    pub dataset_count: u64,
    pub artifact: ReleaseArtifactV1,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ReleaseBuildReportV1 {
    pub tidas_validation: ReleaseValidationReportV1,
    pub conversion: IlcdConversionReportV1,
    pub ilcd_validation: ReleaseValidationReportV1,
    pub roundtrip: SemanticRoundtripReportV1,
    pub profiles: Vec<ReferenceClosureReportV1>,
    pub packages: Vec<ReleasePackageV1>,
    pub artifact_set_sha256: String,
}

pub fn run_release(
    request: &ReleaseRequest,
    runtime: &ReleaseRuntime,
) -> Result<ReleaseReportV1, ReleaseError> {
    runtime.cancellation.check()?;
    if runtime.queue_capacity == 0 {
        return Err(ReleaseError::ZeroQueueCapacity);
    }
    match request {
        ReleaseRequest::BuildPackages {
            tidas_dir,
            output_dir,
            ..
        }
        | ReleaseRequest::BuildResultPackages {
            tidas_dir,
            output_dir,
            ..
        } => transaction::reject_overlapping_output(tidas_dir, output_dir)?,
        ReleaseRequest::ConvertIlcd {
            input_dir,
            output_dir,
        } => transaction::reject_overlapping_output(input_dir, output_dir)?,
        _ => {}
    }
    let result_only = matches!(
        request,
        ReleaseRequest::BuildResultPackages { .. }
            | ReleaseRequest::ValidateClosure {
                profile: ReleaseProfile::ResultProcess,
                ..
            }
    );
    let mut report = ReleaseReportV1::new(request.action());
    if result_only {
        RELEASE_REPORT_SCHEMA_V2.clone_into(&mut report.schema_version);
    }
    match request {
        ReleaseRequest::ValidateClosure {
            input_dir,
            dataset_index,
            profile,
        } => {
            let index = index::DatasetIndex::load(dataset_index, input_dir, runtime)?;
            let (_, closure) = closure::resolve(input_dir, &index, *profile, runtime)?;
            report.ok = closure.semantic_coverage.as_ref().is_none_or(|c| {
                c.complete
                    && c.checks
                        .values()
                        .all(|v| v.invalid == 0 && v.unresolved == 0)
            });
            report.closure = Some(closure);
        }
        ReleaseRequest::ConvertIlcd {
            input_dir,
            output_dir,
        } => {
            report.conversion = Some(conversion::convert_tidas_to_ilcd(
                input_dir, output_dir, runtime,
            )?);
        }
        ReleaseRequest::SemanticRoundtrip {
            tidas_dir,
            ilcd_dir,
        } => {
            let roundtrip = conversion::semantic_roundtrip(tidas_dir, ilcd_dir, runtime)?;
            report.ok = roundtrip.ok;
            report.roundtrip = Some(roundtrip);
        }
        ReleaseRequest::ValidateTidas { input_dir } => {
            let validation = validate_tree(input_dir, ReleaseDataFormat::Tidas, None, runtime)?;
            report.ok = validation.summary.ok;
            report.validation = Some(validation);
        }
        ReleaseRequest::ValidateIlcd { input_dir } => {
            let validation = validate_tree(input_dir, ReleaseDataFormat::Ilcd, None, runtime)?;
            report.ok = validation.summary.ok;
            report.validation = Some(validation);
        }
        ReleaseRequest::BuildPackages {
            tidas_dir,
            dataset_index,
            output_dir,
            issue_spool,
        }
        | ReleaseRequest::BuildResultPackages {
            tidas_dir,
            dataset_index,
            output_dir,
            issue_spool,
        } => {
            report.build = Some(build_packages(
                tidas_dir,
                dataset_index,
                output_dir,
                issue_spool.as_deref(),
                result_only,
                runtime,
            )?);
        }
    }
    report.peak_accounted_memory_bytes = runtime.memory_budget.peak();
    Ok(report)
}

fn validate_tree(
    input_dir: &std::path::Path,
    format: ReleaseDataFormat,
    issue_spool: Option<&std::path::Path>,
    runtime: &ReleaseRuntime,
) -> Result<ReleaseValidationReportV1, ReleaseError> {
    let request = tidas_validation::ValidationRequest {
        input_dir: input_dir.to_path_buf(),
        issue_spool: issue_spool.map(std::path::Path::to_path_buf),
        cancellation: runtime.cancellation.clone(),
        memory_budget: runtime.memory_budget.clone(),
        queue_capacity: runtime.queue_capacity,
        progress: None,
    };
    let output = match format {
        ReleaseDataFormat::Tidas => tidas_validation::validate_tidas_package(&request)?,
        ReleaseDataFormat::Ilcd => tidas_validation::validate_ilcd_package(&request)?,
    };
    Ok(ReleaseValidationReportV1 {
        format,
        summary: output.summary,
    })
}

fn build_packages(
    tidas_dir: &std::path::Path,
    dataset_index: &std::path::Path,
    output_dir: &std::path::Path,
    issue_spool: Option<&std::path::Path>,
    result_only: bool,
    runtime: &ReleaseRuntime,
) -> Result<ReleaseBuildReportV1, ReleaseError> {
    if let Some(path) = issue_spool
        && path.exists()
    {
        return Err(ReleaseError::IssueSpoolExists(path.to_path_buf()));
    }
    let tidas_validation =
        validate_tree(tidas_dir, ReleaseDataFormat::Tidas, issue_spool, runtime)?;
    if !tidas_validation.summary.ok {
        return Err(ReleaseError::ValidationIssues {
            format: ReleaseDataFormat::Tidas,
            summary: Box::new(tidas_validation.summary),
            issue_spool: issue_spool.map(std::path::Path::to_path_buf),
        });
    }
    remove_successful_spool(issue_spool)?;

    let index = index::DatasetIndex::load(dataset_index, tidas_dir, runtime)?;
    let profiles = resolve_build_profiles(tidas_dir, &index, result_only, runtime)?;

    let staging = transaction::StagedDirectory::new(output_dir)?;
    let ilcd_dir = staging.path().join("ilcd");
    let conversion = conversion::convert_tidas_to_ilcd(tidas_dir, &ilcd_dir, runtime)?;
    let ilcd_validation = validate_tree(&ilcd_dir, ReleaseDataFormat::Ilcd, issue_spool, runtime)?;
    if !ilcd_validation.summary.ok {
        return Err(ReleaseError::ValidationIssues {
            format: ReleaseDataFormat::Ilcd,
            summary: Box::new(ilcd_validation.summary),
            issue_spool: issue_spool.map(std::path::Path::to_path_buf),
        });
    }
    remove_successful_spool(issue_spool)?;
    let roundtrip = conversion::semantic_roundtrip(tidas_dir, &ilcd_dir, runtime)?;
    if !roundtrip.ok {
        return Err(ReleaseError::SemanticRoundtripIssues {
            count: roundtrip.mismatch_count,
        });
    }

    let package_dir = staging.path().join("packages");
    std::fs::create_dir_all(&package_dir)?;
    let mut packages = Vec::with_capacity(profiles.len() * 2);
    for (profile, entries, closure) in &profiles {
        packages.push(archive::write_package(&archive::PackageRequest {
            profile: *profile,
            format: ReleaseDataFormat::Tidas,
            entries,
            tidas_root: tidas_dir,
            ilcd_root: &ilcd_dir,
            output_dir: &package_dir,
            closure,
            runtime,
        })?);
        packages.push(archive::write_package(&archive::PackageRequest {
            profile: *profile,
            format: ReleaseDataFormat::Ilcd,
            entries,
            tidas_root: tidas_dir,
            ilcd_root: &ilcd_dir,
            output_dir: &package_dir,
            closure,
            runtime,
        })?);
    }
    let artifact_set_sha256 = archive::artifact_set_hash(&packages);
    runtime.cancellation.check()?;
    transaction::remove_internal_ilcd(&ilcd_dir)?;
    let final_root = output_dir.to_path_buf();
    for package in &mut packages {
        let file_name = std::path::Path::new(&package.artifact.path)
            .file_name()
            .ok_or(ReleaseError::InvalidGeneratedPath)?
            .to_owned();
        package.artifact.path = final_root.join(file_name).to_string_lossy().into_owned();
    }
    transaction::flatten_packages(staging.path())?;
    staging.commit()?;
    Ok(ReleaseBuildReportV1 {
        tidas_validation,
        conversion,
        ilcd_validation,
        roundtrip,
        profiles: profiles
            .into_iter()
            .map(|(_, _, closure)| closure)
            .collect(),
        packages,
        artifact_set_sha256,
    })
}

type BuildProfile = (
    ReleaseProfile,
    Vec<index::DatasetEntry>,
    ReferenceClosureReportV1,
);

fn resolve_build_profiles(
    tidas_dir: &std::path::Path,
    index: &index::DatasetIndex,
    result_only: bool,
    runtime: &ReleaseRuntime,
) -> Result<Vec<BuildProfile>, ReleaseError> {
    if result_only {
        let (entries, report) =
            closure::resolve(tidas_dir, index, ReleaseProfile::ResultProcess, runtime)?;
        Ok(vec![(ReleaseProfile::ResultProcess, entries, report)])
    } else {
        let (unit_entries, unit_report) =
            closure::resolve(tidas_dir, index, ReleaseProfile::UnitProcess, runtime)?;
        let (result_entries, result_report) =
            closure::resolve(tidas_dir, index, ReleaseProfile::StandaloneResult, runtime)?;
        closure::verify_result_contains_unit(&unit_entries, &result_entries)?;
        Ok(vec![
            (ReleaseProfile::UnitProcess, unit_entries, unit_report),
            (
                ReleaseProfile::StandaloneResult,
                result_entries,
                result_report,
            ),
        ])
    }
}

fn remove_successful_spool(issue_spool: Option<&std::path::Path>) -> Result<(), ReleaseError> {
    let Some(path) = issue_spool else {
        return Ok(());
    };
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(ReleaseError::Io(error)),
    }
}

#[derive(Debug, Error)]
pub enum ReleaseError {
    #[error("release queue capacity must be greater than zero")]
    ZeroQueueCapacity,
    #[error("release input is not a directory: {0}")]
    InputNotDirectory(PathBuf),
    #[error("release output must not equal, contain, or be nested inside an input: {0}")]
    OutputInsideInput(PathBuf),
    #[error("release output is not a replaceable directory: {0}")]
    OutputNotDirectory(PathBuf),
    #[error("release path is unsafe or non-portable: {0}")]
    UnsafePath(PathBuf),
    #[error("release path escapes its declared root: {0}")]
    PathOutsideRoot(PathBuf),
    #[error("release input contains a symbolic link: {0}")]
    Symlink(PathBuf),
    #[error("canonical dataset index is invalid: {0}")]
    DatasetIndexInvalid(String),
    #[error("canonical dataset index schema is unsupported: {0}")]
    DatasetIndexSchemaUnsupported(String),
    #[error("canonical dataset index contains no datasets")]
    DatasetIndexEmpty,
    #[error("duplicate canonical dataset identity: {0}")]
    DuplicateDatasetIdentity(String),
    #[error("duplicate canonical dataset path: {0}")]
    DuplicateDatasetPath(String),
    #[error("indexed dataset file is missing: {0}")]
    DatasetFileMissing(String),
    #[error("indexed dataset hash differs from the canonical index: {0}")]
    DatasetFileHashMismatch(String),
    #[error("release profile has no roots: {0}")]
    ProfileRootsMissing(String),
    #[error("a dataset reference omits its exact @version at {0}")]
    ReferenceVersionMissing(String),
    #[error("exact reference closure is missing {0}")]
    ReferenceClosureMissing(String),
    #[error("the standalone result profile is missing unit-process closure member {0}")]
    StandaloneMissingUnitClosure(String),
    #[error("TIDAS JSON is invalid at {path}: {source}")]
    DatasetJson {
        path: PathBuf,
        #[source]
        source: serde_json::Error,
    },
    #[error("target eILCD ordering schema is missing for {0}")]
    OrderingSchemaMissing(String),
    #[error("target eILCD ordering schema reference is invalid: {0}")]
    OrderingSchemaReference(String),
    #[error("target eILCD ordering schema reference cycle: {0}")]
    OrderingSchemaCycle(String),
    #[error("release validation found issues in the {format:?} tree")]
    ValidationIssues {
        format: ReleaseDataFormat,
        summary: Box<ValidationSummaryV1>,
        issue_spool: Option<PathBuf>,
    },
    #[error("release issue spool already exists: {0}")]
    IssueSpoolExists(PathBuf),
    #[error("semantic round-trip found {count} mismatches")]
    SemanticRoundtripIssues { count: u64 },
    #[error("generated release path is invalid")]
    InvalidGeneratedPath,
    #[error("deterministic archive contains duplicate member {0}")]
    DuplicateArchiveMember(String),
    #[error("release size exceeds the supported integer range")]
    SizeOverflow,
    #[error("failed to atomically publish release output and restore the prior output")]
    CommitRollback {
        source: std::io::Error,
        restore: std::io::Error,
    },
    #[error(transparent)]
    Conversion(#[from] ConversionError),
    #[error(transparent)]
    Validation(#[from] ValidationError),
    #[error(transparent)]
    Runtime(#[from] RuntimeError),
    #[error(transparent)]
    Asset(#[from] tidas_assets::AssetError),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Walk(#[from] walkdir::Error),
    #[error(transparent)]
    Io(#[from] std::io::Error),
}

#[cfg(test)]
mod contract_tests {
    use super::*;

    #[test]
    fn checked_in_schema_matches_the_native_contract() {
        let schema: serde_json::Value =
            serde_json::from_str(RELEASE_REPORT_JSON_SCHEMA_V1).unwrap();
        assert_eq!(
            schema["properties"]["schema_version"]["const"],
            RELEASE_REPORT_SCHEMA_V1
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(
            schema["$defs"]["closure"]["properties"]["dataset_keys"]["maxItems"],
            INLINE_ITEM_LIMIT
        );
        jsonschema::validator_for(&schema).unwrap();
    }

    #[test]
    fn empty_release_report_validates_against_the_checked_in_schema() {
        let schema: serde_json::Value =
            serde_json::from_str(RELEASE_REPORT_JSON_SCHEMA_V1).unwrap();
        let validator = jsonschema::validator_for(&schema).unwrap();
        let value =
            serde_json::to_value(ReleaseReportV1::new(ReleaseAction::ValidateTidas)).unwrap();
        assert!(validator.is_valid(&value));
    }
}
