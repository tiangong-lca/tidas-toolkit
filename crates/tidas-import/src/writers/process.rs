use serde_json::{Map, Value, json};
use thiserror::Error;

use crate::model::CanonicalEntity;

use super::common::{
    CONTACT_NAME, administrative_for_entity, compliance_declarations, contact_id, dataset_ref,
    format_source_id, import_trace, localized, name_parts,
};
use super::unit_flow::exchange_metadata;

pub fn process_base(
    entity: &CanonicalEntity,
    quantitative_reference: &Value,
) -> Result<Value, ProcessWriteError> {
    let name = entity.name.as_deref().unwrap_or("Process");
    let description = entity
        .raw
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or("Converted from external LCA package");
    let format_id = format_source_id();
    let contact_id = contact_id();
    let location = entity
        .raw
        .get("location")
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .unwrap_or("GLO");
    let reference_year = entity
        .raw
        .get("referenceYear")
        .and_then(Value::as_u64)
        .unwrap_or(9999);
    json_process(
        entity,
        &ProcessParts {
            name,
            description,
            quantitative_reference,
            format_id: &format_id,
            contact_id: &contact_id,
            location,
            reference_year,
        },
    )
}

struct ProcessParts<'a> {
    name: &'a str,
    description: &'a str,
    quantitative_reference: &'a Value,
    format_id: &'a str,
    contact_id: &'a str,
    location: &'a str,
    reference_year: u64,
}

fn json_process(
    entity: &CanonicalEntity,
    parts: &ProcessParts<'_>,
) -> Result<Value, ProcessWriteError> {
    let mut document = json!({
        "processDataSet": {
            "@xmlns": "http://lca.jrc.it/ILCD/Process",
            "@xmlns:common": "http://lca.jrc.it/ILCD/Common",
            "@xmlns:xsi": "http://www.w3.org/2001/XMLSchema-instance",
            "@version": "1.1",
            "@locations": "../ILCDLocations.xml",
            "@xsi:schemaLocation": "http://lca.jrc.it/ILCD/Process ../../schemas/ILCD_ProcessDataSet.xsd",
            "processInformation": {
                "dataSetInformation": {
                    "common:UUID": entity.internal_id,
                    "name": name_parts(parts.name, parts.location),
                    "classificationInformation": {
                        "common:classification": {
                            "@name": "ISIC rev.4",
                            "common:class": [
                                {"@level": "0", "@classId": "T", "#text": "Other service activities"},
                                {"@level": "1", "@classId": "94", "#text": "Activities of membership organizations"},
                                {"@level": "2", "@classId": "949", "#text": "Activities of other membership organizations"},
                                {"@level": "3", "@classId": "9499", "#text": "Activities of other membership organizations n.e.c."}
                            ]
                        }
                    },
                    "common:generalComment": localized(parts.description)
                },
                "quantitativeReference": parts.quantitative_reference,
                "time": {"common:referenceYear": parts.reference_year},
                "geography": {
                    "locationOfOperationSupplyOrProduction": {
                        "@location": parts.location
                    }
                }
            },
            "modellingAndValidation": {
                "LCIMethodAndAllocation": lci_method(entity),
                "dataSourcesTreatmentAndRepresentativeness": data_sources(
                    entity,
                    parts.format_id
                ),
                "validation": {"review": review(entity)},
                "complianceDeclarations": compliance_declarations(true)
            },
            "administrativeInformation": {
                "common:commissionerAndGoal": {
                    "common:referenceToCommissioner": dataset_ref(
                        "contact data set",
                        parts.contact_id,
                        CONTACT_NAME,
                        "contacts"
                    ),
                    "common:intendedApplications": localized(
                        "Converted from external LCA package"
                    )
                },
                "dataEntryBy": administrative_for_entity("processes", entity, true)
                    .get("dataEntryBy")
                    .cloned()
                    .ok_or(ProcessWriteError::AdministrativeShape)?,
                "publicationAndOwnership": administrative_for_entity("processes", entity, true)
                .get("publicationAndOwnership")
                .cloned()
                .ok_or(ProcessWriteError::AdministrativeShape)?
            }
        }
    });
    apply_rich_process_information(entity, &mut document);
    Ok(document)
}

fn apply_rich_process_information(entity: &CanonicalEntity, document: &mut Value) {
    if let Some(trace) = entity.raw.get("sourceTrace") {
        insert_at(
            document,
            "/processDataSet/processInformation/dataSetInformation",
            "common:other",
            import_trace(trace),
        );
    }
    if let Some(year) = value_year(entity.raw.get("dataSetValidUntil")) {
        insert_at(
            document,
            "/processDataSet/processInformation/time",
            "common:dataSetValidUntil",
            json!(year),
        );
    }
    if let Some(description) = clean_text(entity.raw.get("timeDescription")) {
        insert_at(
            document,
            "/processDataSet/processInformation/time",
            "common:timeRepresentativenessDescription",
            localized(description),
        );
    }
    if let Some(description) = clean_text(entity.raw.get("locationDescription")) {
        insert_at(
            document,
            "/processDataSet/processInformation/geography/locationOfOperationSupplyOrProduction",
            "descriptionOfRestrictions",
            localized(description),
        );
    }
    if let Some(description) = clean_text(entity.raw.get("technologyDescription")) {
        insert_at(
            document,
            "/processDataSet/processInformation",
            "technology",
            json!({
                "technologyDescriptionAndIncludedProcesses": localized(description)
            }),
        );
    }
}

fn insert_at(document: &mut Value, pointer: &str, field: &str, value: Value) {
    document
        .pointer_mut(pointer)
        .and_then(Value::as_object_mut)
        .expect("native process writer pointer targets an object")
        .insert(field.to_owned(), value);
}

fn lci_method(entity: &CanonicalEntity) -> Value {
    let process_type = if entity.raw.contains_key("ilcdNonFlowReference") {
        entity.raw.get("ilcdProcessType").and_then(Value::as_str)
    } else {
        Some(
            match entity
                .raw
                .get("sourceProcessType")
                .and_then(Value::as_str)
                .unwrap_or_default()
            {
                "LCI_RESULT" => "LCI result",
                "PARTLY_TERMINATED_SYSTEM" => "Partly terminated system",
                "AVOIDED_PRODUCT_SYSTEM" => "Avoided product system",
                _ => "Unit process, single operation",
            },
        )
    };
    let mut section = Map::new();
    if let Some(process_type) = process_type {
        section.insert(
            "typeOfDataSet".to_owned(),
            Value::String(process_type.to_owned()),
        );
    }
    if let Some(value) = clean_text(entity.raw.get("deviationsFromLCIMethodPrinciple")) {
        section.insert(
            "deviationsFromLCIMethodPrinciple".to_owned(),
            localized(value),
        );
    }
    if let Some(method) = entity
        .raw
        .get("sourceDefaultAllocationMethod")
        .and_then(Value::as_str)
        .and_then(allocation_method)
    {
        section.insert(
            "LCIMethodApproaches".to_owned(),
            Value::String(method.to_owned()),
        );
    }
    if let Some(value) = clean_text(entity.raw.get("modellingConstants")) {
        section.insert("modellingConstants".to_owned(), localized(value));
    }
    Value::Object(section)
}

fn allocation_method(value: &str) -> Option<&'static str> {
    match value {
        "ECONOMIC_ALLOCATION" => Some("Allocation - market value"),
        "PHYSICAL_ALLOCATION" | "CAUSAL_ALLOCATION" => Some("Allocation - physical causality"),
        "NO_ALLOCATION" => Some("Not applicable"),
        _ => None,
    }
}

fn data_sources(entity: &CanonicalEntity, format_id: &str) -> Value {
    let raw = &entity.raw;
    let mut section = Map::new();
    section.insert(
        "dataCutOffAndCompletenessPrinciples".to_owned(),
        localized(
            clean_text(raw.get("dataCutOffAndCompletenessPrinciples"))
                .unwrap_or("Converted from external LCA package"),
        ),
    );
    for (field, target) in [
        (
            "dataSelectionAndCombinationPrinciples",
            "dataSelectionAndCombinationPrinciples",
        ),
        (
            "dataTreatmentAndExtrapolationsPrinciples",
            "dataTreatmentAndExtrapolationsPrinciples",
        ),
    ] {
        if let Some(value) = clean_text(raw.get(field)) {
            section.insert(target.to_owned(), localized(value));
        }
    }
    section.insert(
        "referenceToDataSource".to_owned(),
        dataset_ref(
            "source data set",
            format_id,
            "External LCA source metadata",
            "sources",
        ),
    );
    section.insert(
        "annualSupplyOrProductionVolume".to_owned(),
        localized(production_volume(entity)),
    );
    for field in [
        "samplingProcedure",
        "dataCollectionPeriod",
        "uncertaintyAdjustments",
        "useAdviceForDataSet",
    ] {
        if let Some(value) = clean_text(raw.get(field)) {
            section.insert(field.to_owned(), localized(value));
        }
    }
    Value::Object(section)
}

fn production_volume(entity: &CanonicalEntity) -> String {
    if let Some(value) = clean_text(entity.raw.get("productionVolume"))
        && !value.eq_ignore_ascii_case("na")
    {
        return if value.contains("/year") {
            value.to_owned()
        } else {
            format!("{value}/year")
        };
    }
    format!(
        "0 {}/year; source production volume unavailable",
        entity
            .raw
            .get("referenceUnitName")
            .and_then(Value::as_str)
            .unwrap_or("kg")
    )
}

fn clean_text(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| {
            !value.is_empty()
                && !matches!(
                    value.to_ascii_lowercase().as_str(),
                    "<null>" | "null" | "none"
                )
        })
}

fn value_year(value: Option<&Value>) -> Option<u64> {
    match value? {
        Value::Number(value) => value.as_u64(),
        Value::String(value) => value
            .split(|character: char| !character.is_ascii_digit())
            .find(|token| token.len() == 4)
            .and_then(|token| token.parse().ok()),
        _ => None,
    }
}

fn review(entity: &CanonicalEntity) -> Value {
    let Some(indicators) = entity
        .raw
        .get("dataQualityIndicators")
        .and_then(Value::as_array)
        .filter(|indicators| !indicators.is_empty())
    else {
        return json!({"@type": "Not reviewed"});
    };
    let indicator = if indicators.len() == 1 {
        indicators[0].clone()
    } else {
        Value::Array(indicators.clone())
    };
    json!({
        "@type": "Not reviewed",
        "common:dataQualityIndicators": {
            "common:dataQualityIndicator": indicator
        }
    })
}

pub fn exchange_item(
    exchange: &Map<String, Value>,
    internal_id: &str,
) -> Result<Value, ProcessWriteError> {
    let flow_id = exchange
        .get("flowRefId")
        .and_then(Value::as_str)
        .ok_or_else(|| ProcessWriteError::MissingFlowReference(internal_id.to_owned()))?;
    let flow_name = exchange
        .get("flowName")
        .and_then(Value::as_str)
        .unwrap_or("Flow");
    let amount = exchange
        .get("amount")
        .and_then(Value::as_str)
        .unwrap_or("0");
    let direction = if exchange
        .get("isInput")
        .and_then(Value::as_bool)
        .unwrap_or(false)
    {
        "Input"
    } else {
        "Output"
    };
    let mut item = Map::from_iter([
        (
            "@dataSetInternalID".to_owned(),
            Value::String(internal_id.to_owned()),
        ),
        (
            "referenceToFlowDataSet".to_owned(),
            dataset_ref("flow data set", flow_id, flow_name, "flows"),
        ),
        (
            "exchangeDirection".to_owned(),
            Value::String(direction.to_owned()),
        ),
        ("meanAmount".to_owned(), Value::String(amount.to_owned())),
        (
            "resultingAmount".to_owned(),
            Value::String(amount.to_owned()),
        ),
    ]);
    apply_exchange_details(exchange, &mut item);
    item.insert(
        "generalComment".to_owned(),
        localized(exchange_comment(exchange, internal_id)),
    );
    item.insert("common:other".to_owned(), exchange_metadata(exchange));
    Ok(Value::Object(item))
}

fn apply_exchange_details(exchange: &Map<String, Value>, item: &mut Map<String, Value>) {
    if let Some(location) = exchange.get("location").cloned() {
        item.insert("location".to_owned(), location);
    }
    for field in [
        "minimumAmount",
        "maximumAmount",
        "uncertaintyDistributionType",
    ] {
        if let Some(value) = exchange.get(field).cloned() {
            item.insert(field.to_owned(), value);
        }
    }
    if let Some(value) = exchange
        .get("relativeStandardDeviation95In")
        .and_then(percentage)
    {
        item.insert(
            "relativeStandardDeviation95In".to_owned(),
            Value::String(value),
        );
    }
    if let Some(allocations) = exchange.get("allocations").and_then(Value::as_array) {
        let allocations = allocations
            .iter()
            .filter_map(Value::as_object)
            .filter_map(|allocation| {
                Some(json!({
                    "@internalReferenceToCoProduct": allocation
                        .get("internalReferenceToCoProduct")?
                        .clone(),
                    "@allocatedFraction": allocation.get("allocatedFraction")?.clone(),
                }))
            })
            .collect::<Vec<_>>();
        if !allocations.is_empty() {
            item.insert("allocations".to_owned(), json!({"allocation": allocations}));
        }
    }
    item.insert(
        "dataDerivationTypeStatus".to_owned(),
        Value::String(
            exchange
                .get("dataDerivationTypeStatus")
                .and_then(Value::as_str)
                .filter(|value| matches!(*value, "Measured" | "Calculated" | "Estimated"))
                .unwrap_or("Unknown derivation")
                .to_owned(),
        ),
    );
}

fn exchange_comment(exchange: &Map<String, Value>, internal_id: &str) -> String {
    let mut comments =
        if let Some(value) = exchange.get("sourceExchangeNumber").and_then(Value::as_str) {
            vec![format!("Source EcoSpold1 exchange number: {value}.")]
        } else if let Some(value) = exchange.get("sourceExchangeId").and_then(Value::as_str) {
            vec![format!("Source EcoSpold2 exchange id: {value}.")]
        } else {
            vec![format!("Source exchange internal id: {internal_id}.")]
        };
    if let Some(comment) = exchange
        .get("generalComment")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|comment| !comment.is_empty())
    {
        comments.push(comment.to_owned());
    }
    comments.join(" ")
}

fn percentage(value: &Value) -> Option<String> {
    let mut text = match value {
        Value::String(value) => value.trim().to_owned(),
        Value::Number(value) => value.to_string(),
        _ => return None,
    };
    if text.contains('.') {
        text = text.trim_end_matches('0').trim_end_matches('.').to_owned();
    }
    let unsigned = text
        .strip_prefix('+')
        .or_else(|| text.strip_prefix('-'))
        .unwrap_or(&text);
    let mut parts = unsigned.split('.');
    let integer = parts.next()?;
    let fraction = parts.next().unwrap_or_default();
    if parts.next().is_some()
        || integer.is_empty()
        || !integer.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
        || fraction.len() > 3
        || integer.len().saturating_add(fraction.len()) > 5
    {
        return None;
    }
    Some(text)
}

pub fn functional_unit(exchange: &Map<String, Value>) -> String {
    let amount = exchange
        .get("amount")
        .and_then(Value::as_str)
        .unwrap_or("0");
    let unit = exchange
        .get("unitName")
        .and_then(Value::as_str)
        .unwrap_or("unit");
    let flow = exchange
        .get("flowName")
        .and_then(Value::as_str)
        .unwrap_or("flow");
    format!("{amount} {unit} {flow}")
}

#[derive(Debug, Error)]
pub enum ProcessWriteError {
    #[error("exchange {0} has no flow reference")]
    MissingFlowReference(String),
    #[error("internal administrative dataset shape is invalid")]
    AdministrativeShape,
}
