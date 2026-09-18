use std::collections::BTreeMap;
use std::fmt::Write as _;

use jsonschema::error::ValidationErrorKind;
use jsonschema::{Resource, Validator};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;
use tidas_assets::{AssetKind, bundled_assets};

use crate::contracts::ValidationIssueV1;

const SCHEMA_ASSET_PREFIX: &str = "assets/tidas/schemas/";
const SCHEMA_BASE_URI: &str = "https://tiangong.earth/assets/tidas/schemas/";
const MAX_SCHEMA_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_INSTANCE_PREVIEW_BYTES: usize = 256;
const MAX_LOCATION_BYTES: usize = 4 * 1024;

#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum TidasCategory {
    Contacts,
    Flowproperties,
    Flows,
    Lciamethods,
    Lifecyclemodels,
    Processes,
    Sources,
    Unitgroups,
}

pub const SUPPORTED_TIDAS_CATEGORIES: [TidasCategory; 8] = [
    TidasCategory::Contacts,
    TidasCategory::Flowproperties,
    TidasCategory::Flows,
    TidasCategory::Lciamethods,
    TidasCategory::Lifecyclemodels,
    TidasCategory::Processes,
    TidasCategory::Sources,
    TidasCategory::Unitgroups,
];

impl TidasCategory {
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Contacts => "contacts",
            Self::Flowproperties => "flowproperties",
            Self::Flows => "flows",
            Self::Lciamethods => "lciamethods",
            Self::Lifecyclemodels => "lifecyclemodels",
            Self::Processes => "processes",
            Self::Sources => "sources",
            Self::Unitgroups => "unitgroups",
        }
    }

    fn schema_filename(self) -> String {
        format!("tidas_{}.json", self.as_str())
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        SUPPORTED_TIDAS_CATEGORIES
            .into_iter()
            .find(|category| category.as_str() == value)
    }
}

pub(crate) struct SchemaCatalog {
    schemas: BTreeMap<String, Value>,
}

impl SchemaCatalog {
    pub(crate) fn load() -> Result<Self, SchemaError> {
        let mut schemas = BTreeMap::new();
        for asset in bundled_assets()
            .into_iter()
            .filter(|asset| asset.kind == AssetKind::JsonSchema)
        {
            let Some(filename) = asset.path.strip_prefix(SCHEMA_ASSET_PREFIX) else {
                continue;
            };
            let asset_path = asset.path.clone();
            let mut schema: Value = serde_json::from_slice(asset.bytes).map_err(|source| {
                SchemaError::InvalidAsset {
                    path: asset_path,
                    source,
                }
            })?;
            let object = schema
                .as_object_mut()
                .ok_or_else(|| SchemaError::AssetNotObject(filename.to_owned()))?;
            object.insert(
                "$id".to_owned(),
                Value::String(format!("{SCHEMA_BASE_URI}{filename}")),
            );
            schemas.insert(filename.to_owned(), schema);
        }
        if schemas.is_empty() {
            return Err(SchemaError::NoSchemas);
        }
        Ok(Self { schemas })
    }

    pub(crate) fn validator(&self, category: TidasCategory) -> Result<TidasValidator, SchemaError> {
        let filename = category.schema_filename();
        let root = self
            .schemas
            .get(&filename)
            .ok_or_else(|| SchemaError::MissingCategory(filename.clone()))?;
        let resources = self.schemas.iter().map(|(resource_name, schema)| {
            (
                format!("{SCHEMA_BASE_URI}{resource_name}"),
                Resource::from_contents(schema.clone()),
            )
        });
        let validator = jsonschema::draft7::options()
            .with_resources(resources)
            .with_format("cas-number", is_valid_cas_number)
            .should_validate_formats(true)
            .build(root)
            .map_err(|source| SchemaError::Compile {
                filename,
                reason: source.to_string(),
            })?;
        Ok(TidasValidator {
            category,
            validator,
        })
    }
}

pub(crate) struct TidasValidator {
    category: TidasCategory,
    validator: Validator,
}

impl TidasValidator {
    pub(crate) fn issues<'a>(
        &'a self,
        instance: &'a Value,
        file_path: &'a str,
    ) -> impl Iterator<Item = ValidationIssueV1> + 'a {
        self.validator.iter_errors(instance).map(move |error| {
            let raw_location = error.instance_path().to_string();
            let location = raw_location
                .strip_prefix('/')
                .unwrap_or(&raw_location)
                .to_owned();
            let location = if location.is_empty() {
                "<root>".to_owned()
            } else {
                location
            };
            let bounded_location = bounded_text(&location, MAX_LOCATION_BYTES);
            let schema_path = error.schema_path().to_string();
            let detail = error.masked_with("<instance>").to_string();
            let raw_message = format!(
                "Schema error at {} (keyword: {}): {detail}",
                bounded_location.value,
                schema_keyword(error.kind())
            );
            let bounded_message = bounded_text(&raw_message, MAX_SCHEMA_MESSAGE_BYTES);
            let message = bounded_message.value.clone();
            let mut issue = ValidationIssueV1::error(
                "schema_error",
                self.category.as_str(),
                file_path,
                &bounded_location.value,
                message,
            );
            issue.context.insert(
                "schema_keyword".to_owned(),
                Value::String(schema_keyword(error.kind()).to_owned()),
            );
            issue
                .context
                .insert("schema_path".to_owned(), Value::String(schema_path));
            if let ValidationErrorKind::Required { property } = error.kind()
                && let Some(property) = property.as_str()
            {
                let bounded = bounded_text(property, MAX_LOCATION_BYTES);
                issue.context.insert(
                    "required_property".to_owned(),
                    Value::String(bounded.value.clone()),
                );
                add_truncation_context("required_property", &bounded, &mut issue.context);
            }
            add_instance_context(error.instance(), &mut issue.context);
            add_truncation_context("location", &bounded_location, &mut issue.context);
            add_truncation_context("diagnostic", &bounded_message, &mut issue.context);
            issue
        })
    }
}

struct BoundedText {
    value: String,
    original_bytes: Option<usize>,
    sha256: Option<String>,
}

fn bounded_text(value: &str, max_bytes: usize) -> BoundedText {
    if value.len() <= max_bytes {
        return BoundedText {
            value: value.to_owned(),
            original_bytes: None,
            sha256: None,
        };
    }
    let prefix = utf8_prefix(value, max_bytes);
    BoundedText {
        value: prefix.to_owned(),
        original_bytes: Some(value.len()),
        sha256: Some(sha256_hex(value.as_bytes())),
    }
}

fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

fn add_truncation_context(
    prefix: &str,
    bounded: &BoundedText,
    context: &mut BTreeMap<String, Value>,
) {
    let (Some(original_bytes), Some(sha256)) = (bounded.original_bytes, bounded.sha256.as_ref())
    else {
        return;
    };
    context.insert(format!("{prefix}_truncated"), Value::Bool(true));
    context.insert(
        format!("{prefix}_original_bytes"),
        Value::from(original_bytes as u64),
    );
    context.insert(format!("{prefix}_sha256"), Value::String(sha256.clone()));
}

fn add_instance_context(instance: &Value, context: &mut BTreeMap<String, Value>) {
    context.insert(
        "instance_type".to_owned(),
        Value::String(instance_type(instance).to_owned()),
    );
    match instance {
        Value::Null => {}
        Value::Bool(value) => {
            context.insert("instance_preview".to_owned(), Value::Bool(*value));
        }
        Value::Number(value) => {
            context.insert("instance_preview".to_owned(), Value::Number(value.clone()));
        }
        Value::String(value) => {
            context.insert(
                "instance_byte_length".to_owned(),
                Value::from(value.len() as u64),
            );
            let preview = bounded_text(value, MAX_INSTANCE_PREVIEW_BYTES);
            context.insert(
                "instance_preview".to_owned(),
                Value::String(preview.value.clone()),
            );
            add_truncation_context("instance_preview", &preview, context);
        }
        Value::Array(values) => {
            context.insert(
                "instance_item_count".to_owned(),
                Value::from(values.len() as u64),
            );
        }
        Value::Object(values) => {
            context.insert(
                "instance_property_count".to_owned(),
                Value::from(values.len() as u64),
            );
        }
    }
}

const fn instance_type(instance: &Value) -> &'static str {
    match instance {
        Value::Null => "null",
        Value::Bool(_) => "boolean",
        Value::Number(_) => "number",
        Value::String(_) => "string",
        Value::Array(_) => "array",
        Value::Object(_) => "object",
    }
}

const fn schema_keyword(kind: &ValidationErrorKind) -> &'static str {
    match kind {
        ValidationErrorKind::AdditionalItems { .. } => "additionalItems",
        ValidationErrorKind::AdditionalProperties { .. } => "additionalProperties",
        ValidationErrorKind::AnyOf { .. } => "anyOf",
        ValidationErrorKind::BacktrackLimitExceeded { .. }
        | ValidationErrorKind::Pattern { .. } => "pattern",
        ValidationErrorKind::Constant { .. } => "const",
        ValidationErrorKind::Contains => "contains",
        ValidationErrorKind::ContentEncoding { .. } | ValidationErrorKind::FromUtf8 { .. } => {
            "contentEncoding"
        }
        ValidationErrorKind::ContentMediaType { .. } => "contentMediaType",
        ValidationErrorKind::Custom { .. } => "custom",
        ValidationErrorKind::Enum { .. } => "enum",
        ValidationErrorKind::ExclusiveMaximum { .. } => "exclusiveMaximum",
        ValidationErrorKind::ExclusiveMinimum { .. } => "exclusiveMinimum",
        ValidationErrorKind::FalseSchema => "falseSchema",
        ValidationErrorKind::Format { .. } => "format",
        ValidationErrorKind::MaxItems { .. } => "maxItems",
        ValidationErrorKind::Maximum { .. } => "maximum",
        ValidationErrorKind::MaxLength { .. } => "maxLength",
        ValidationErrorKind::MaxProperties { .. } => "maxProperties",
        ValidationErrorKind::MinItems { .. } => "minItems",
        ValidationErrorKind::Minimum { .. } => "minimum",
        ValidationErrorKind::MinLength { .. } => "minLength",
        ValidationErrorKind::MinProperties { .. } => "minProperties",
        ValidationErrorKind::MultipleOf { .. } => "multipleOf",
        ValidationErrorKind::Not { .. } => "not",
        ValidationErrorKind::OneOfMultipleValid { .. }
        | ValidationErrorKind::OneOfNotValid { .. } => "oneOf",
        ValidationErrorKind::PropertyNames { .. } => "propertyNames",
        ValidationErrorKind::Required { .. } => "required",
        ValidationErrorKind::Type { .. } => "type",
        ValidationErrorKind::UnevaluatedItems { .. } => "unevaluatedItems",
        ValidationErrorKind::UnevaluatedProperties { .. } => "unevaluatedProperties",
        ValidationErrorKind::UniqueItems => "uniqueItems",
        ValidationErrorKind::Referencing(_) => "$ref",
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[must_use]
pub fn is_valid_cas_number(value: &str) -> bool {
    let mut parts = value.rsplitn(2, '-');
    let Some(check_digit) = parts.next().and_then(|part| part.parse::<u8>().ok()) else {
        return false;
    };
    let Some(body) = parts.next() else {
        return false;
    };
    let Some((first, second)) = body.split_once('-') else {
        return false;
    };
    if !(2..=7).contains(&first.len())
        || second.len() != 2
        || !first.bytes().all(|byte| byte.is_ascii_digit())
        || !second.bytes().all(|byte| byte.is_ascii_digit())
    {
        return false;
    }
    let checksum = first
        .bytes()
        .chain(second.bytes())
        .rev()
        .enumerate()
        .map(|(index, byte)| u64::from(byte - b'0') * (index as u64 + 1))
        .sum::<u64>();
    checksum % 10 == u64::from(check_digit)
}

#[derive(Debug, Error)]
pub enum SchemaError {
    #[error("no bundled TIDAS JSON schemas were found")]
    NoSchemas,
    #[error("bundled schema {0} is not a JSON object")]
    AssetNotObject(String),
    #[error("missing bundled category schema {0}")]
    MissingCategory(String),
    #[error("bundled schema {path} is invalid JSON: {source}")]
    InvalidAsset {
        path: String,
        source: serde_json::Error,
    },
    #[error("failed to compile bundled schema {filename}: {reason}")]
    Compile { filename: String, reason: String },
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn cas_number_contract_matches_the_python_oracle() {
        assert!(is_valid_cas_number("64-17-5"));
        assert!(is_valid_cas_number("7732-18-5"));
        assert!(!is_valid_cas_number("64-17-6"));
        assert!(!is_valid_cas_number("2023600"));
        assert!(!is_valid_cas_number(" 64-17-5 "));
    }

    #[test]
    fn every_supported_category_compiles_offline() {
        let catalog = SchemaCatalog::load().unwrap();
        for category in SUPPORTED_TIDAS_CATEGORIES {
            catalog.validator(category).unwrap();
        }
    }

    #[test]
    fn missing_review_fields_are_structured_errors_without_relaxing_validation() {
        for (category, root) in [
            (TidasCategory::Processes, "processDataSet"),
            (TidasCategory::Lifecyclemodels, "lifeCycleModelDataSet"),
        ] {
            let validator = SchemaCatalog::load().unwrap().validator(category).unwrap();
            let document = json!({root: {"modellingAndValidation": {}}});
            let issues = validator
                .issues(&document, "fixture.json")
                .collect::<Vec<_>>();
            for field in ["validation", "complianceDeclarations"] {
                let issue = issues
                    .iter()
                    .find(|issue| issue.context.get("required_property") == Some(&json!(field)))
                    .unwrap();
                assert_eq!(issue.location, format!("{root}/modellingAndValidation"));
                assert_eq!(issue.context["schema_keyword"], "required");
                assert_eq!(serde_json::to_value(issue).unwrap()["severity"], "error");
            }
        }
    }

    #[test]
    fn oversized_schema_instances_emit_bounded_diagnostics() {
        let validator = SchemaCatalog::load()
            .unwrap()
            .validator(TidasCategory::Sources)
            .unwrap();
        let oversized = "数".repeat(1024 * 1024);
        let document = Value::String(oversized.clone());
        let issue = validator
            .issues(&document, "sources/oversized.json")
            .next()
            .unwrap();

        assert!(issue.message.len() <= MAX_SCHEMA_MESSAGE_BYTES);
        assert!(!issue.message.contains(&oversized));
        assert_eq!(issue.context["schema_keyword"], "type");
        assert_eq!(issue.context["instance_type"], "string");
        assert_eq!(
            issue.context["instance_byte_length"],
            Value::from(oversized.len() as u64)
        );
        assert_eq!(issue.context["instance_preview_truncated"], true);
        assert_eq!(
            issue.context["instance_preview_sha256"],
            sha256_hex(oversized.as_bytes())
        );
    }

    #[test]
    fn english_and_chinese_flow_schemas_share_the_type_aware_name_contract() {
        let fixtures: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/flow-name-schema-v1/cases.json"
        ))
        .unwrap();
        let template: Value = serde_json::from_str(include_str!(
            "../tests/fixtures/flow-name-schema-v1/template.json"
        ))
        .unwrap();
        assert_eq!(
            fixtures["schema_version"],
            "tidas.flow-name-schema-fixtures.v1"
        );
        for (language, catalog) in [
            ("en", SchemaCatalog::load().unwrap()),
            ("zh", schema_catalog("assets/tidas/schemas_zh/")),
        ] {
            let validator = catalog.validator(TidasCategory::Flows).unwrap();
            for case in fixtures["cases"].as_array().unwrap() {
                let mut document = template.clone();
                document["flowDataSet"]["flowInformation"]["dataSetInformation"]["name"] =
                    case["flow_name"].clone();
                document["flowDataSet"]["modellingAndValidation"]["LCIMethod"]["typeOfDataSet"] =
                    case["flow_type"].clone();
                let issues = validator
                    .issues(&document, "flows/fixture.json")
                    .collect::<Vec<_>>();
                assert_eq!(
                    issues.is_empty(),
                    case["valid"].as_bool().unwrap(),
                    "{language} case {}: {}",
                    case["name"].as_str().unwrap(),
                    json!(issues),
                );
            }
        }
    }

    #[test]
    fn source_digital_file_accepts_opaque_relative_locator_in_both_languages() {
        for catalog in [
            SchemaCatalog::load().unwrap(),
            schema_catalog("assets/tidas/schemas_zh/"),
        ] {
            let field_schema = catalog.schemas["tidas_sources.json"]
                .pointer("/properties/sourceDataSet/properties/sourceInformation/properties/dataSetInformation/properties/referenceToDigitalFile")
                .unwrap()
                .clone();
            let validator = jsonschema::draft7::new(&field_schema).unwrap();
            assert!(validator.is_valid(&serde_json::json!({
                "@uri": "../external_docs/report.jpg"
            })));
            assert!(validator.is_valid(&serde_json::json!([
                {"@uri": "../external_docs/report.jpg"},
                {"@uri": "https://example.test/report.pdf"}
            ])));
        }
    }

    fn schema_catalog(prefix: &str) -> SchemaCatalog {
        let schemas = bundled_assets()
            .into_iter()
            .filter_map(|asset| {
                let filename = asset.path.strip_prefix(prefix)?;
                let mut schema: Value = serde_json::from_slice(asset.bytes).unwrap();
                schema.as_object_mut().unwrap().insert(
                    "$id".to_owned(),
                    Value::String(format!("{SCHEMA_BASE_URI}{filename}")),
                );
                Some((filename.to_owned(), schema))
            })
            .collect::<BTreeMap<_, _>>();
        assert!(!schemas.is_empty());
        SchemaCatalog { schemas }
    }
}
