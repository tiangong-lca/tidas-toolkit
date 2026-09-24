//! Native, bounded TIDAS and ILCD validation pipelines.

mod batch;
mod contracts;
mod ilcd;
mod pipeline;
mod schema;
mod semantic;
mod spool_destination;

pub use batch::{
    BatchDocumentIdentityV1, BatchValidationIssueEventV1, BatchValidationOutput,
    BatchValidationRequest, DOCUMENT_VALIDATION_BATCH_PROTOCOL,
    DOCUMENT_VALIDATION_MANIFEST_ITEM_JSON_SCHEMA_V1, DOCUMENT_VALIDATION_PROFILE,
    EngineFingerprintV1, PackageFingerprintV1, VALIDATION_DESCRIBE_JSON_SCHEMA_V1,
    VALIDATION_DESCRIBE_SCHEMA_V1, VALIDATION_FINAL_EVENT_JSON_SCHEMA_V1,
    VALIDATION_FINAL_EVENT_SCHEMA_V1, VALIDATION_REPORT_SCHEMA_V1, ValidationDescribeV1,
    ValidationFinalEventV1, ValidationFinalSummaryV1, describe_document_validation,
    run_document_validation_batch,
};
pub use contracts::{
    CategorySummaryV1, SeverityV1, VALIDATION_ISSUE_EVENT_JSON_SCHEMA_V1,
    VALIDATION_ISSUE_EVENT_SCHEMA_V1, VALIDATION_SUMMARY_JSON_SCHEMA_V1,
    VALIDATION_SUMMARY_SCHEMA_V1, ValidationIssueEventV1, ValidationIssueV1, ValidationSummaryV1,
};
pub use ilcd::validate_ilcd_package;
pub use pipeline::{
    ValidationError, ValidationOutput, ValidationProgressReporter, ValidationProgressV1,
    ValidationRequest, validate_tidas_package,
};
pub use schema::{SUPPORTED_TIDAS_CATEGORIES, SchemaError, TidasCategory, is_valid_cas_number};
pub use semantic::SemanticError;
