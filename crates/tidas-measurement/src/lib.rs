//! Deterministic, evidence-bound product/waste Flow measurement conversion.
//! This module neither resolves providers nor decides physical applicability.

use std::collections::BTreeSet;
use std::fmt::Write as _;
use std::num::NonZeroU64;
use std::str::FromStr;

use bigdecimal::{BigDecimal, Context, RoundingMode};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use thiserror::Error;

pub const REQUEST_SCHEMA: &str = "tidas.flow-property-conversion-request.v1";
pub const REPORT_SCHEMA: &str = "tidas.flow-property-conversion.v1";
pub const PRECISION: u64 = 100;
pub const REQUEST_JSON_SCHEMA: &str =
    include_str!("../contracts/flow-property-conversion-request.v1.schema.json");
pub const REPORT_JSON_SCHEMA: &str =
    include_str!("../contracts/flow-property-conversion-report.v1.schema.json");
const MAX_DECIMAL_BYTES: usize = 256;
const MAX_EXPONENT: i64 = 512;

#[derive(Debug, Error)]
#[error("{code}: {message}")]
pub struct MeasurementError {
    pub code: &'static str,
    pub message: String,
}

fn error(code: &'static str, message: impl Into<String>) -> MeasurementError {
    MeasurementError {
        code,
        message: message.into(),
    }
}

/// Parse bounded decimal text before allocating an arbitrary-precision value.
pub fn parse_decimal(text: &str) -> Result<BigDecimal, MeasurementError> {
    let text = text.trim();
    if text.is_empty() || text.len() > MAX_DECIMAL_BYTES {
        return Err(error(
            "decimal_out_of_bounds",
            "decimal text must contain 1..256 bytes",
        ));
    }
    if let Some((_, exponent)) = text.split_once(['e', 'E']) {
        let exponent = exponent
            .parse::<i64>()
            .map_err(|_| error("invalid_decimal", "invalid decimal exponent"))?;
        if !(-MAX_EXPONENT..=MAX_EXPONENT).contains(&exponent) {
            return Err(error(
                "decimal_out_of_bounds",
                "decimal exponent exceeds +/-512",
            ));
        }
    }
    BigDecimal::from_str(text)
        .map_err(|_| error("invalid_decimal", "expected a finite decimal number"))
}

pub fn decimal_value(value: &Value) -> Result<BigDecimal, MeasurementError> {
    match value {
        Value::String(text) => parse_decimal(text),
        Value::Number(number) => parse_decimal(&number.to_string()),
        _ => Err(error(
            "invalid_decimal",
            "expected decimal string or JSON number",
        )),
    }
}

#[must_use]
pub fn decimal_text(value: &BigDecimal) -> String {
    value.normalized().to_string()
}

fn context() -> Context {
    Context::new(
        NonZeroU64::new(PRECISION).expect("positive precision"),
        RoundingMode::HalfEven,
    )
}

fn worker_number(value: &BigDecimal) -> Result<(), MeasurementError> {
    let numeric = decimal_text(value).parse::<f64>().map_err(|_| {
        error(
            "worker_number_range",
            "value cannot be represented by Worker f64",
        )
    })?;
    if !numeric.is_finite() || (numeric == 0.0 && value != &BigDecimal::from(0)) {
        return Err(error(
            "worker_number_range",
            "value overflows or underflows Worker f64",
        ));
    }
    Ok(())
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Quantity {
    pub amount: String,
    #[serde(default)]
    pub minimum_amount: Option<String>,
    #[serde(default)]
    pub maximum_amount: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScaleFactor {
    pub numerator: String,
    pub denominator: String,
    pub decimal: String,
}

/// The one arithmetic implementation shared by canonical conversion and import.
/// Factors are positive. A zero descriptor is valid data but cannot be a divisor.
pub fn scale_quantity(
    quantity: &Quantity,
    unit_factor: &BigDecimal,
    source_property_factor: &BigDecimal,
    reference_property_factor: &BigDecimal,
    reverse: bool,
) -> Result<(Quantity, ScaleFactor), MeasurementError> {
    for value in [
        unit_factor,
        source_property_factor,
        reference_property_factor,
    ] {
        if value <= &BigDecimal::from(0) {
            return Err(error(
                "non_positive_conversion_factor",
                "conversion factors must be strictly positive; zero descriptors cannot be used for conversion",
            ));
        }
        worker_number(value)?;
    }
    let numerator = unit_factor * reference_property_factor;
    let denominator = source_property_factor.clone();
    let (numerator, denominator) = if reverse {
        (denominator, numerator)
    } else {
        (numerator, denominator)
    };
    let inverse = denominator.inverse_with_context(&context());
    let factor = context().multiply(&numerator, &inverse);
    worker_number(&factor)?;
    worker_number(&factor.inverse_with_context(&context()))?;
    let amount = parse_decimal(&quantity.amount)?;
    let minimum = quantity
        .minimum_amount
        .as_deref()
        .map(parse_decimal)
        .transpose()?;
    let maximum = quantity
        .maximum_amount
        .as_deref()
        .map(parse_decimal)
        .transpose()?;
    if minimum.as_ref().is_some_and(|v| v > &amount)
        || maximum.as_ref().is_some_and(|v| v < &amount)
        || minimum
            .as_ref()
            .zip(maximum.as_ref())
            .is_some_and(|(lo, hi)| lo > hi)
    {
        return Err(error(
            "invalid_quantity_interval",
            "minimum <= amount <= maximum must hold before conversion",
        ));
    }
    let convert = |value: &BigDecimal| -> Result<String, MeasurementError> {
        worker_number(value)?;
        let result = context().multiply(&(value * &numerator), &inverse);
        worker_number(&result)?;
        Ok(decimal_text(&result))
    };
    Ok((
        Quantity {
            amount: convert(&amount)?,
            minimum_amount: minimum.as_ref().map(&convert).transpose()?,
            maximum_amount: maximum.as_ref().map(&convert).transpose()?,
        },
        ScaleFactor {
            numerator: decimal_text(&numerator),
            denominator: decimal_text(&denominator),
            decimal: decimal_text(&factor),
        },
    ))
}

pub struct FlowProperties<'a> {
    pub entries: Vec<&'a Value>,
    pub reference: &'a Value,
}

fn text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, MeasurementError> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .ok_or_else(|| error("missing_identity", format!("missing string {pointer}")))
}

fn items(value: &Value) -> Vec<&Value> {
    match value {
        Value::Array(items) => items.iter().collect(),
        Value::Object(_) => vec![value],
        _ => Vec::new(),
    }
}

/// Shared Flow semantics; does not mutate or assign new internal identifiers.
/// Descriptive zero values remain valid, while the selected reference must be 1.
pub fn inspect_flow_properties(document: &Value) -> Result<FlowProperties<'_>, MeasurementError> {
    let reference_id = text(
        document,
        "/flowDataSet/flowInformation/quantitativeReference/referenceToReferenceFlowProperty",
    )?;
    let entries = document
        .pointer("/flowDataSet/flowProperties/flowProperty")
        .map(items)
        .unwrap_or_default();
    if entries.is_empty() {
        return Err(error(
            "missing_flow_properties",
            "Flow needs at least one property",
        ));
    }
    let mut internal_ids = BTreeSet::new();
    let mut property_ids = BTreeSet::new();
    let mut reference = None;
    for entry in &entries {
        let id = text(entry, "/@dataSetInternalID")?;
        if !internal_ids.insert(id) {
            return Err(error(
                "duplicate_flow_property_internal_id",
                format!("duplicate internal ID {id}"),
            ));
        }
        let property_id = text(entry, "/referenceToFlowPropertyDataSet/@refObjectId")?;
        if !property_ids.insert(property_id.to_ascii_lowercase()) {
            return Err(error(
                "duplicate_flow_property",
                format!("duplicate property identity {property_id}"),
            ));
        }
        entry
            .get("meanValue")
            .ok_or_else(|| {
                error(
                    "missing_property_factor",
                    format!("property {id} has no meanValue"),
                )
            })
            .and_then(decimal_value)?;
        if id == reference_id {
            reference = Some(*entry);
        }
    }
    let reference = reference.ok_or_else(|| {
        error(
            "missing_reference_flow_property",
            format!("reference internal ID {reference_id} is absent"),
        )
    })?;
    if decimal_value(&reference["meanValue"])? != 1 {
        return Err(error(
            "reference_property_not_one",
            "reference property meanValue must equal 1; historical content is not silently rewritten",
        ));
    }
    Ok(FlowProperties { entries, reference })
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceQuantity {
    pub flow_property_internal_id: String,
    pub unit_internal_id: String,
    pub amount: String,
    #[serde(default)]
    pub minimum_amount: Option<String>,
    #[serde(default)]
    pub maximum_amount: Option<String>,
    #[serde(default)]
    pub formula: Option<String>,
}

#[derive(Clone, Copy, Debug, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MeasurementDirection {
    #[default]
    ToReference,
    FromReference,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementRequest {
    pub schema_version: String,
    pub flow: Value,
    pub flow_properties: Vec<Value>,
    pub unit_groups: Vec<Value>,
    pub source: SourceQuantity,
    #[serde(default)]
    pub direction: MeasurementDirection,
    pub conditions: String,
    pub evidence: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DocumentBinding {
    pub category: String,
    pub id: String,
    pub version: String,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReferenceMeasurement {
    pub flow_property_internal_id: String,
    pub flow_property_id: String,
    pub flow_property_version: String,
    pub unit_group_id: String,
    pub unit_group_version: String,
    pub unit_internal_id: String,
    pub unit_name: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundTrip {
    pub amount: String,
    pub residual: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MeasurementReport {
    pub schema_version: String,
    pub request_sha256: String,
    pub documents: Vec<DocumentBinding>,
    pub source: SourceQuantity,
    pub reference: ReferenceMeasurement,
    pub factor: ScaleFactor,
    pub result: Quantity,
    pub round_trip: RoundTrip,
    pub precision: u64,
    pub rounding: String,
    pub direction: MeasurementDirection,
    pub conditions: String,
    pub evidence: Vec<String>,
    pub applicability: String,
}

/// Hash sorted-key JSON with exact normalized decimal-number lexemes (no LF).
/// Numeric metadata shares the bounded parser; strings retain their original bytes.
pub fn canonical_json_sha256(value: &Value) -> Result<String, MeasurementError> {
    fn sorted(value: &Value) -> Result<Value, MeasurementError> {
        match value {
            Value::Object(object) => {
                let mut keys = object.keys().collect::<Vec<_>>();
                keys.sort();
                let members = keys
                    .into_iter()
                    .map(|key| Ok((key.clone(), sorted(&object[key])?)))
                    .collect::<Result<serde_json::Map<_, _>, MeasurementError>>()?;
                Ok(Value::Object(members))
            }
            Value::Array(values) => Ok(Value::Array(
                values.iter().map(sorted).collect::<Result<_, _>>()?,
            )),
            Value::Number(number) => {
                let decimal = parse_decimal(&number.to_string())?;
                serde_json::from_str(&decimal_text(&decimal))
                    .map_err(|_| error("invalid_decimal", "normalized JSON number is invalid"))
            }
            _ => Ok(value.clone()),
        }
    }
    let digest =
        Sha256::digest(serde_json::to_vec(&sorted(value)?).expect("JSON value is serializable"));
    let mut output = String::with_capacity(64);
    for byte in digest {
        write!(output, "{byte:02x}").expect("String formatting succeeds");
    }
    Ok(output)
}

fn binding(
    document: &Value,
    category: &str,
    root: &str,
    info: &str,
) -> Result<DocumentBinding, MeasurementError> {
    let id = text(
        document,
        &format!("/{root}/{info}/dataSetInformation/common:UUID"),
    )?;
    let version = text(
        document,
        &format!("/{root}/administrativeInformation/publicationAndOwnership/common:dataSetVersion"),
    )?;
    if id.len() != 36
        || !id.bytes().enumerate().all(|(i, b)| {
            if [8, 13, 18, 23].contains(&i) {
                b == b'-'
            } else {
                b.is_ascii_hexdigit()
            }
        })
    {
        return Err(error(
            "invalid_dataset_identity",
            "dataset UUID must be an exact UUID string",
        ));
    }
    if version.len() != 9
        || !version.bytes().enumerate().all(|(i, b)| {
            if [2, 5].contains(&i) {
                b == b'.'
            } else {
                b.is_ascii_digit()
            }
        })
    {
        return Err(error(
            "invalid_dataset_identity",
            "dataset version must use NN.NN.NNN",
        ));
    }
    Ok(DocumentBinding {
        category: category.to_owned(),
        id: id.to_owned(),
        version: version.to_owned(),
        sha256: canonical_json_sha256(document)?,
    })
}

fn resolve<'a>(
    documents: &'a [Value],
    reference: &Value,
    category: &str,
    root: &str,
    info: &str,
) -> Result<(&'a Value, DocumentBinding), MeasurementError> {
    let id = text(reference, "/@refObjectId")?;
    let version = text(reference, "/@version")?;
    let mut found = None;
    for document in documents {
        let candidate = binding(document, category, root, info)?;
        if candidate.id == id && candidate.version == version {
            if found.is_some() {
                return Err(error(
                    "duplicate_dependency",
                    format!("duplicate {category} {id}@{version}"),
                ));
            }
            found = Some((document, candidate));
        }
    }
    found.ok_or_else(|| {
        error(
            "missing_exact_dependency",
            format!("missing exact {category} {id}@{version}"),
        )
    })
}

fn property_group<'a>(
    request: &'a MeasurementRequest,
    property: &Value,
) -> Result<(&'a Value, DocumentBinding, DocumentBinding), MeasurementError> {
    let reference = property
        .get("referenceToFlowPropertyDataSet")
        .ok_or_else(|| error("missing_identity", "property reference missing"))?;
    let (property, property_binding) = resolve(
        &request.flow_properties,
        reference,
        "flowproperties",
        "flowPropertyDataSet",
        "flowPropertiesInformation",
    )?;
    let group_ref = property.pointer("/flowPropertyDataSet/flowPropertiesInformation/quantitativeReference/referenceToReferenceUnitGroup").ok_or_else(|| error("missing_identity", "UnitGroup reference missing"))?;
    let (group, group_binding) = resolve(
        &request.unit_groups,
        group_ref,
        "unitgroups",
        "unitGroupDataSet",
        "unitGroupInformation",
    )?;
    Ok((group, property_binding, group_binding))
}

fn group_units(group: &Value) -> Result<(Vec<&Value>, &Value), MeasurementError> {
    let reference_id = text(
        group,
        "/unitGroupDataSet/unitGroupInformation/quantitativeReference/referenceToReferenceUnit",
    )?;
    let units = group
        .pointer("/unitGroupDataSet/units/unit")
        .map(items)
        .unwrap_or_default();
    let mut ids = BTreeSet::new();
    let mut reference = None;
    for unit in &units {
        let id = text(unit, "/@dataSetInternalID")?;
        if !ids.insert(id) {
            return Err(error(
                "duplicate_unit_internal_id",
                format!("duplicate unit ID {id}"),
            ));
        }
        if id == reference_id {
            let factor = unit
                .get("meanValue")
                .ok_or_else(|| error("missing_unit_factor", "reference unit has no meanValue"))
                .and_then(decimal_value)?;
            if factor != 1 {
                return Err(error(
                    "reference_unit_not_one",
                    "UnitGroup reference unit meanValue must equal 1",
                ));
            }
            reference = Some(*unit);
        }
    }
    let reference = reference.ok_or_else(|| {
        error(
            "missing_reference_unit",
            "UnitGroup reference ID does not resolve",
        )
    })?;
    Ok((units, reference))
}

fn parse_request(value: &Value) -> Result<MeasurementRequest, MeasurementError> {
    let request: MeasurementRequest = serde_json::from_value(value.clone())
        .map_err(|e| error("invalid_measurement_request", e.to_string()))?;
    if request.schema_version != REQUEST_SCHEMA {
        return Err(error(
            "unsupported_measurement_schema",
            "unsupported request schema",
        ));
    }
    if request.conditions.trim().is_empty()
        || request.evidence.is_empty()
        || request.evidence.iter().any(|e| e.trim().is_empty())
    {
        return Err(error(
            "missing_conversion_evidence",
            "fixed conditions and evidence identifiers are required; applicability remains an upstream decision",
        ));
    }
    if request
        .source
        .formula
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty())
    {
        return Err(error(
            "formula_requires_evaluation",
            "evaluate and verify formula upstream, retain its provenance, then submit the evaluated amount",
        ));
    }
    let kind = text(
        &request.flow,
        "/flowDataSet/modellingAndValidation/LCIMethod/typeOfDataSet",
    )?;
    if !matches!(kind, "Product flow" | "Waste flow") {
        return Err(error(
            "unsupported_flow_type",
            "conversion is limited to Product and Waste flows",
        ));
    }
    Ok(request)
}

fn support_bindings(
    request: &MeasurementRequest,
    entries: &[&Value],
    flow_binding: DocumentBinding,
) -> Result<Vec<DocumentBinding>, MeasurementError> {
    // Resolve every added property: unused descriptors still add required support.
    let mut documents = vec![flow_binding];
    for property in entries {
        let (group, property_binding, group_binding) = property_group(request, property)?;
        group_units(group)?;
        documents.push(property_binding);
        documents.push(group_binding);
    }
    documents
        .sort_by(|a, b| (&a.category, &a.id, &a.version).cmp(&(&b.category, &b.id, &b.version)));
    documents.dedup_by(|a, b| {
        a.category == b.category && a.id == b.id && a.version == b.version && a.sha256 == b.sha256
    });
    Ok(documents)
}

fn checked_round_trip(original: &str, recovered: &str) -> Result<BigDecimal, MeasurementError> {
    let original = parse_decimal(original)?;
    let residual = parse_decimal(recovered)? - &original;
    let tolerance = original.abs() * parse_decimal("1e-95")?;
    if residual.abs() > tolerance {
        return Err(error(
            "round_trip_mismatch",
            "round trip exceeds the fixed 1e-95 relative decimal tolerance",
        ));
    }
    Ok(residual)
}

pub fn convert_measurement(value: &Value) -> Result<MeasurementReport, MeasurementError> {
    let request = parse_request(value)?;
    let flow_binding = binding(&request.flow, "flows", "flowDataSet", "flowInformation")?;
    let properties = inspect_flow_properties(&request.flow)?;
    let source_property = properties
        .entries
        .iter()
        .copied()
        .find(|p| {
            p.get("@dataSetInternalID").and_then(Value::as_str)
                == Some(request.source.flow_property_internal_id.as_str())
        })
        .ok_or_else(|| {
            error(
                "missing_source_property",
                "source property internal ID is absent",
            )
        })?;
    let (source_group, _, _) = property_group(&request, source_property)?;
    let (target_group, target_property_binding, target_group_binding) =
        property_group(&request, properties.reference)?;
    let (source_units, _) = group_units(source_group)?;
    let (_, target_unit) = group_units(target_group)?;
    let source_unit = source_units
        .into_iter()
        .find(|u| {
            u.get("@dataSetInternalID").and_then(Value::as_str)
                == Some(request.source.unit_internal_id.as_str())
        })
        .ok_or_else(|| {
            error(
                "missing_source_unit",
                "source unit internal ID is absent from its property UnitGroup",
            )
        })?;
    let unit_factor = source_unit
        .get("meanValue")
        .ok_or_else(|| error("missing_unit_factor", "source unit meanValue missing"))
        .and_then(decimal_value)?;
    let property_factor = decimal_value(&source_property["meanValue"])?;
    let reference_factor = decimal_value(&properties.reference["meanValue"])?;
    let quantity = Quantity {
        amount: request.source.amount.clone(),
        minimum_amount: request.source.minimum_amount.clone(),
        maximum_amount: request.source.maximum_amount.clone(),
    };
    let reverse = matches!(request.direction, MeasurementDirection::FromReference);
    let (result, factor) = scale_quantity(
        &quantity,
        &unit_factor,
        &property_factor,
        &reference_factor,
        reverse,
    )?;
    let (roundtrip, _) = scale_quantity(
        &result,
        &unit_factor,
        &property_factor,
        &reference_factor,
        !reverse,
    )?;
    let residual = checked_round_trip(&quantity.amount, &roundtrip.amount)?;
    let documents = support_bindings(&request, &properties.entries, flow_binding)?;
    Ok(MeasurementReport {
        schema_version: REPORT_SCHEMA.to_owned(),
        request_sha256: canonical_json_sha256(value)?,
        documents,
        source: request.source.clone(),
        reference: ReferenceMeasurement {
            flow_property_internal_id: text(properties.reference, "/@dataSetInternalID")?
                .to_owned(),
            flow_property_id: target_property_binding.id,
            flow_property_version: target_property_binding.version,
            unit_group_id: target_group_binding.id,
            unit_group_version: target_group_binding.version,
            unit_internal_id: text(target_unit, "/@dataSetInternalID")?.to_owned(),
            unit_name: text(target_unit, "/name")?.to_owned(),
        },
        factor,
        result,
        round_trip: RoundTrip {
            amount: roundtrip.amount,
            residual: decimal_text(&residual),
        },
        precision: PRECISION,
        rounding: "half-even".to_owned(),
        direction: request.direction,
        conditions: request.conditions,
        evidence: request.evidence,
        applicability: "not_assessed".to_owned(),
    })
}

#[cfg(test)]
#[path = "measurement_tests.rs"]
mod tests;
