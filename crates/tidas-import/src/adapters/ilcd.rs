use std::collections::BTreeMap;

use serde_json::{Map, Value, json};
use uuid::Uuid;

use super::{AdapterContext, AdapterError, SourceAdapter};
use crate::detect::SourceFormat;
use crate::model::CanonicalEntity;
use crate::report::{ImportIssue, IssueSeverity, IssueSink};
use crate::source::{SourceReadRequest, visit_source_entries};
use crate::store::CanonicalStore;

pub struct IlcdAdapter;

impl SourceAdapter for IlcdAdapter {
    fn format(&self) -> SourceFormat {
        SourceFormat::Ilcd
    }

    fn read(
        &self,
        context: &AdapterContext<'_>,
        store: &mut CanonicalStore,
        issues: &mut dyn IssueSink,
    ) -> Result<(), AdapterError> {
        let request = SourceReadRequest {
            source: context.source,
            allowed_extensions: &["xml"],
            max_entry_bytes: context.max_entry_bytes,
            cancellation: context.cancellation,
            memory_budget: context.memory_budget,
        };
        let mut count = 0_u64;
        visit_source_entries(&request, |entry| {
            let _structured_reservation =
                context.reserve_structured_expansion(entry.bytes.len(), 6)?;
            let json_bytes =
                match tidas_conversion::convert_xml_to_json(entry.bytes, context.cancellation) {
                    Ok(bytes) => bytes,
                    Err(error) => {
                        issues.push(&ImportIssue {
                            severity: IssueSeverity::Error,
                            code: "invalid_ilcd_xml".to_owned(),
                            message: format!("Skipped invalid ILCD XML: {error}"),
                            source_object: Some(entry.label.clone()),
                            context: BTreeMap::new(),
                        })?;
                        return Ok::<(), AdapterError>(());
                    }
                };
            let document: Value = serde_json::from_slice(&json_bytes)?;
            let Some(mut entity) = entity_from_document(&document, &entry.stable_key) else {
                return Ok::<(), AdapterError>(());
            };
            if entity.entity_type == "processes" {
                store.begin_process_exchanges(&entity.internal_id)?;
                if let Some(exchanges) = entity
                    .raw
                    .remove("exchanges")
                    .and_then(|value| value.as_array().cloned())
                {
                    for exchange in exchanges
                        .into_iter()
                        .filter_map(|value| value.as_object().cloned())
                    {
                        store.add_process_exchange(&entity.internal_id, &exchange)?;
                    }
                }
            }
            store.add(&entity)?;
            count = count.saturating_add(1);
            Ok::<(), AdapterError>(())
        })?;
        if count == 0 {
            issues.push(&ImportIssue {
                severity: IssueSeverity::Error,
                code: "no_ilcd_datasets".to_owned(),
                message: "No supported ILCD datasets were found.".to_owned(),
                source_object: None,
                context: BTreeMap::new(),
            })?;
        } else {
            issues.push(&ImportIssue {
                severity: IssueSeverity::Warning,
                code: "ilcd_native_mapping".to_owned(),
                message: "Mapped ILCD datasets with native Rust rules.".to_owned(),
                source_object: None,
                context: BTreeMap::from([("entity_count".to_owned(), json!(count))]),
            })?;
        }
        Ok(())
    }
}

fn entity_from_document(document: &Value, label: &str) -> Option<CanonicalEntity> {
    let (root_name, dataset) = document.as_object()?.iter().next()?;
    let dataset = dataset.as_object()?;
    let (entity_type, information_path): (&str, &[&str]) = match root_name.as_str() {
        "contactDataSet" => ("contacts", &["contactInformation", "dataSetInformation"]),
        "sourceDataSet" => ("sources", &["sourceInformation", "dataSetInformation"]),
        "unitGroupDataSet" => (
            "unitgroups",
            &["unitGroupInformation", "dataSetInformation"],
        ),
        "flowPropertyDataSet" => (
            "flowproperties",
            &["flowPropertiesInformation", "dataSetInformation"],
        ),
        "flowDataSet" => ("flows", &["flowInformation", "dataSetInformation"]),
        "processDataSet" => ("processes", &["processInformation", "dataSetInformation"]),
        _ => return None,
    };
    let information = value_at(dataset, information_path)?;
    let id = field_text_value(information, "common:UUID")
        .filter(|value| Uuid::parse_str(value).is_ok())
        .map_or_else(
            || stable_id(&format!("ilcd/{entity_type}/{label}")),
            ToOwned::to_owned,
        );
    let raw = match entity_type {
        "unitgroups" => unit_group_raw(dataset),
        "flowproperties" => flow_property_raw(dataset),
        "flows" => flow_raw(dataset),
        "processes" => process_raw(dataset),
        "sources" => source_raw(information),
        "contacts" => contact_raw(information),
        _ => Map::new(),
    };
    let mut raw = raw;
    if let Some(version) = value_at(
        dataset,
        &[
            "administrativeInformation",
            "publicationAndOwnership",
            "common:dataSetVersion",
        ],
    )
    .and_then(scalar_text)
    {
        raw.insert("version".to_owned(), Value::String(version.to_owned()));
    }
    raw.insert(
        "sourceTrace".to_owned(),
        json!({"format": "ilcd", "sourceObject": root_name, "file": label}),
    );
    Some(CanonicalEntity {
        entity_type: entity_type.to_owned(),
        internal_id: id.clone(),
        external_id: Some(id),
        name: dataset_name(information, entity_type),
        category_path: classification_path(information),
        raw,
    })
}

fn unit_group_raw(dataset: &Map<String, Value>) -> Map<String, Value> {
    let reference = value_at(
        dataset,
        &[
            "unitGroupInformation",
            "quantitativeReference",
            "referenceToReferenceUnit",
        ],
    )
    .and_then(scalar_text);
    let units = value_at(dataset, &["units", "unit"])
        .map(items)
        .unwrap_or_default()
        .into_iter()
        .filter_map(Value::as_object)
        .map(|unit| {
            let internal_id = field_text(unit, "@dataSetInternalID").unwrap_or("1");
            json!({
                "name": field_text(unit, "name").unwrap_or("unit"),
                "conversionFactor": field_text(unit, "meanValue").unwrap_or("1"),
                "referenceUnit": reference == Some(internal_id),
            })
        })
        .collect::<Vec<_>>();
    Map::from_iter([("units".to_owned(), Value::Array(units))])
}

fn flow_property_raw(dataset: &Map<String, Value>) -> Map<String, Value> {
    let mut raw = Map::new();
    let reference = value_at(
        dataset,
        &[
            "flowPropertiesInformation",
            "quantitativeReference",
            "referenceToReferenceUnitGroup",
        ],
    )
    .and_then(Value::as_object);
    if let Some(reference) = reference {
        if let Some(id) = field_text(reference, "@refObjectId") {
            raw.insert("unitGroupRefId".to_owned(), Value::String(id.to_owned()));
        }
        if let Some(name) = reference
            .get("common:shortDescription")
            .and_then(localized_text)
        {
            raw.insert("unitGroupName".to_owned(), Value::String(name.to_owned()));
        }
    }
    raw
}

fn flow_raw(dataset: &Map<String, Value>) -> Map<String, Value> {
    let mut raw = Map::new();
    let data_set_information = value_at(dataset, &["flowInformation", "dataSetInformation"]);
    if let Some(name) = flow_name_facts(data_set_information) {
        raw.insert("flowName".to_owned(), name);
    }
    let flow_type = value_at(
        dataset,
        &["modellingAndValidation", "LCIMethod", "typeOfDataSet"],
    )
    .and_then(scalar_text)
    .unwrap_or("Product flow");
    raw.insert(
        "flowType".to_owned(),
        Value::String(flow_type.to_ascii_uppercase().replace(' ', "_")),
    );
    for (source, target) in [("CASNumber", "CASNumber"), ("sumFormula", "sumFormula")] {
        if let Some(value) = value_at(dataset, &["flowInformation", "dataSetInformation", source])
            .and_then(scalar_text)
        {
            raw.insert(target.to_owned(), Value::String(value.to_owned()));
        }
    }
    let properties = flow_property_assignments(dataset);
    if !properties.is_empty() {
        if let Some(reference) = properties.iter().find(|property| {
            property
                .get("isRefFlowProperty")
                .and_then(Value::as_bool)
                .unwrap_or(false)
        }) {
            if let Some(id) = reference
                .pointer("/flowProperty/@id")
                .and_then(Value::as_str)
            {
                raw.insert("flowPropertyRefId".to_owned(), Value::String(id.to_owned()));
            }
            if let Some(name) = reference
                .pointer("/flowProperty/name")
                .and_then(Value::as_str)
            {
                raw.insert(
                    "flowPropertyName".to_owned(),
                    Value::String(name.to_owned()),
                );
            }
        }
        raw.insert("flowProperties".to_owned(), Value::Array(properties));
    }
    let categories = value_at(
        dataset,
        &[
            "flowInformation",
            "dataSetInformation",
            "classificationInformation",
            "common:elementaryFlowCategorization",
            "common:category",
        ],
    )
    .map(items)
    .unwrap_or_default()
    .into_iter()
    .filter_map(|category| {
        Some(json!({
            "level": category.get("@level").and_then(scalar_text)?,
            "catId": category.get("@catId").and_then(scalar_text),
            "text": localized_text(category)?
        }))
    })
    .collect::<Vec<_>>();
    if !categories.is_empty() {
        raw.insert(
            "elementaryCategorization".to_owned(),
            Value::Array(categories),
        );
    }
    raw
}

fn flow_name_facts(data_set_information: Option<&Value>) -> Option<Value> {
    let mut name_parts = Map::new();
    for field in [
        "treatmentStandardsRoutes",
        "mixAndLocationTypes",
        "flowProperties",
    ] {
        if let Some(value) = data_set_information
            .and_then(|information| information.get("name"))
            .and_then(|name| name.get(field))
            .and_then(localized_text)
        {
            name_parts.insert(field.to_owned(), Value::String(value.to_owned()));
        }
    }
    (!name_parts.is_empty()).then_some(Value::Object(name_parts))
}

fn flow_property_assignments(dataset: &Map<String, Value>) -> Vec<Value> {
    let reference_internal = value_at(
        dataset,
        &[
            "flowInformation",
            "quantitativeReference",
            "referenceToReferenceFlowProperty",
        ],
    )
    .and_then(scalar_text);
    value_at(dataset, &["flowProperties", "flowProperty"])
        .map(items)
        .unwrap_or_default()
        .into_iter()
        .filter_map(Value::as_object)
        .enumerate()
        .filter_map(|(source_order, item)| {
            let reference = item.get("referenceToFlowPropertyDataSet")?.as_object()?;
            let id = field_text(reference, "@refObjectId")?;
            let name = reference
                .get("common:shortDescription")
                .and_then(localized_text)
                .unwrap_or("Flow property");
            let internal_id = field_text(item, "@dataSetInternalID");
            let mut property = Map::from_iter([
                ("@id".to_owned(), Value::String(id.to_owned())),
                ("name".to_owned(), Value::String(name.to_owned())),
            ]);
            if let Some(version) = field_text(reference, "@version") {
                property.insert("@version".to_owned(), Value::String(version.to_owned()));
            }
            Some(json!({
                "flowProperty": property,
                "conversionFactor": field_text(item, "meanValue").unwrap_or("1"),
                "isRefFlowProperty": reference_internal.is_none_or(|reference| {
                    internal_id == Some(reference)
                }),
                "sourceEvidence": {
                    "format": "ilcd",
                    "sourcePath": format!("flowProperties.flowProperty[{source_order}]"),
                    "sourceInternalId": internal_id,
                }
            }))
        })
        .collect()
}

fn process_raw(dataset: &Map<String, Value>) -> Map<String, Value> {
    let description = value_at(
        dataset,
        &[
            "processInformation",
            "dataSetInformation",
            "common:generalComment",
        ],
    )
    .and_then(localized_text)
    .unwrap_or("Imported from ILCD.");
    let reference_exchange = value_at(
        dataset,
        &[
            "processInformation",
            "quantitativeReference",
            "referenceToReferenceFlow",
        ],
    )
    .and_then(scalar_text);
    let exchanges = value_at(dataset, &["exchanges", "exchange"])
        .map(items)
        .unwrap_or_default()
        .into_iter()
        .filter_map(Value::as_object)
        .enumerate()
        .filter_map(|(index, exchange)| process_exchange(exchange, index, reference_exchange))
        .collect();
    let mut raw = Map::from_iter([
        (
            "description".to_owned(),
            Value::String(description.to_owned()),
        ),
        ("exchanges".to_owned(), Value::Array(exchanges)),
    ]);
    preserve_process_evidence(dataset, &mut raw);
    if let Some(reference) = value_at(dataset, &["processInformation", "quantitativeReference"])
        .and_then(Value::as_object)
        && let Some(reference_type) = reference.get("@type").and_then(scalar_text)
        && reference_type != "Reference flow(s)"
    {
        let mut preserved =
            Map::from_iter([("@type".to_owned(), Value::String(reference_type.to_owned()))]);
        if let Some(value) = reference.get("functionalUnitOrOther") {
            preserved.insert("functionalUnitOrOther".to_owned(), value.clone());
        }
        if let Some(value) = reference.get("common:other") {
            preserved.insert("common:other".to_owned(), value.clone());
        }
        raw.insert("ilcdNonFlowReference".to_owned(), Value::Object(preserved));
        raw.insert(
            "ilcdProcessType".to_owned(),
            value_at(
                dataset,
                &[
                    "modellingAndValidation",
                    "LCIMethodAndAllocation",
                    "typeOfDataSet",
                ],
            )
            .and_then(scalar_text)
            .map_or(Value::Null, |value| Value::String(value.to_owned())),
        );
    }
    if let Some(location) = value_at(
        dataset,
        &[
            "processInformation",
            "geography",
            "locationOfOperationSupplyOrProduction",
            "@location",
        ],
    )
    .and_then(scalar_text)
    {
        raw.insert("location".to_owned(), Value::String(location.to_owned()));
    }
    if let Some(year) = value_at(
        dataset,
        &["processInformation", "time", "common:referenceYear"],
    )
    .and_then(scalar_text)
    .and_then(|value| value.parse::<u32>().ok())
    {
        raw.insert("referenceYear".to_owned(), json!(year));
    }
    raw
}

fn preserve_process_evidence(dataset: &Map<String, Value>, raw: &mut Map<String, Value>) {
    let information = value_at(dataset, &["processInformation", "dataSetInformation"]);
    if let Some(base_name) =
        information.and_then(|value| value_at_value(value, &["name", "baseName"]))
    {
        raw.insert("ilcdBaseName".to_owned(), base_name.clone());
    }
    if let Some(comment) = information.and_then(|value| value.get("common:generalComment")) {
        raw.insert("ilcdGeneralComment".to_owned(), comment.clone());
    }
    if let Some(technology) = value_at(dataset, &["processInformation", "technology"]) {
        raw.insert("ilcdTechnology".to_owned(), technology.clone());
    }
    if let Some(sources) = value_at(
        dataset,
        &[
            "modellingAndValidation",
            "dataSourcesTreatmentAndRepresentativeness",
        ],
    ) {
        if let Some(reference) = sources.get("referenceToDataSource") {
            raw.insert("ilcdReferenceToDataSource".to_owned(), reference.clone());
        }
        if let Some(advice) = sources.get("useAdviceForDataSet") {
            raw.insert("ilcdUseAdviceForDataSet".to_owned(), advice.clone());
        }
    }
}

fn process_exchange(
    exchange: &Map<String, Value>,
    index: usize,
    reference_exchange: Option<&str>,
) -> Option<Value> {
    let reference = exchange.get("referenceToFlowDataSet")?.as_object()?;
    let flow_id = field_text(reference, "@refObjectId")?;
    let flow_name = reference
        .get("common:shortDescription")
        .and_then(localized_text)
        .unwrap_or("Flow");
    let amount = field_text(exchange, "meanAmount")
        .or_else(|| field_text(exchange, "resultingAmount"))
        .unwrap_or("0");
    let direction = field_text(exchange, "exchangeDirection").unwrap_or("Output");
    let internal_id = field_text(exchange, "@dataSetInternalID")
        .map_or_else(|| index.saturating_add(1).to_string(), ToOwned::to_owned);
    let mut value = Map::from_iter([
        ("internalId".to_owned(), Value::String(internal_id.clone())),
        (
            "flow".to_owned(),
            json!({"@id": flow_id, "name": flow_name}),
        ),
        ("flowRefId".to_owned(), Value::String(flow_id.to_owned())),
        ("flowName".to_owned(), Value::String(flow_name.to_owned())),
        (
            "isInput".to_owned(),
            Value::Bool(direction.eq_ignore_ascii_case("input")),
        ),
        ("amount".to_owned(), Value::String(amount.to_owned())),
        (
            "isQuantitativeReference".to_owned(),
            Value::Bool(reference_exchange == Some(internal_id.as_str())),
        ),
    ]);
    if let Some(description) = reference.get("common:shortDescription") {
        value.insert("ilcdFlowShortDescription".to_owned(), description.clone());
    }
    if let Some(comment) = exchange.get("generalComment") {
        value.insert("ilcdGeneralComment".to_owned(), comment.clone());
    }
    value.insert(
        "sourceTrace".to_owned(),
        json!({
            "format": "ilcd",
            "sourceInternalId": internal_id,
            "sourceFlowUri": reference.get("@uri"),
        }),
    );
    for field in [
        "location",
        "minimumAmount",
        "maximumAmount",
        "uncertaintyDistributionType",
        "relativeStandardDeviation95In",
        "dataDerivationTypeStatus",
        "generalComment",
    ] {
        if let Some(field_value) = exchange.get(field).and_then(text_value) {
            value.insert(field.to_owned(), Value::String(field_value.to_owned()));
        }
    }
    Some(Value::Object(value))
}

fn source_raw(information: &Value) -> Map<String, Value> {
    let mut raw = Map::new();
    if let Some(short_name) = information.get("common:shortName") {
        raw.insert("ilcdShortName".to_owned(), short_name.clone());
    }
    if let Some(description) = information.get("sourceDescriptionOrComment") {
        raw.insert("ilcdDescription".to_owned(), description.clone());
    }
    if let Some(short_name) = information.get("common:shortName").and_then(localized_text) {
        raw.insert("shortName".to_owned(), Value::String(short_name.to_owned()));
    }
    if let Some(citation) = information.get("sourceCitation").and_then(scalar_text) {
        raw.insert(
            "sourceCitation".to_owned(),
            Value::String(citation.to_owned()),
        );
    }
    for (source, target) in [
        ("publicationType", "publicationType"),
        ("sourceDescriptionOrComment", "description"),
    ] {
        if let Some(value) = information.get(source).and_then(text_value) {
            raw.insert(target.to_owned(), Value::String(value.to_owned()));
        }
    }
    let mut digital_files = Vec::new();
    for reference in information
        .get("referenceToDigitalFile")
        .map(items)
        .unwrap_or_default()
    {
        let Some(uri) = reference.get("@uri").and_then(scalar_text) else {
            continue;
        };
        if uri.to_ascii_lowercase().starts_with("http://")
            || uri.to_ascii_lowercase().starts_with("https://")
        {
            raw.entry("url".to_owned())
                .or_insert_with(|| Value::String(uri.to_owned()));
        } else {
            digital_files.push(Value::String(uri.to_owned()));
        }
    }
    if !digital_files.is_empty() {
        raw.insert(
            "referenceToDigitalFile".to_owned(),
            Value::Array(digital_files),
        );
    }
    raw
}

fn contact_raw(information: &Value) -> Map<String, Value> {
    let mut raw = Map::new();
    for (source, target) in [
        ("common:shortName", "shortName"),
        ("email", "email"),
        ("telephone", "telephone"),
        ("WWWAddress", "website"),
        ("contactAddress", "address"),
        ("contactDescriptionOrComment", "description"),
    ] {
        if let Some(value) = information.get(source).and_then(text_value) {
            raw.insert(target.to_owned(), Value::String(value.to_owned()));
        }
    }
    raw
}

fn dataset_name(information: &Value, entity_type: &str) -> Option<String> {
    let name = if matches!(entity_type, "flows" | "processes") {
        value_at_value(information, &["name", "baseName"])
    } else {
        information.get("common:name")
    };
    name.and_then(localized_text)
        .or_else(|| information.get("common:shortName").and_then(localized_text))
        .map(ToOwned::to_owned)
}

fn classification_path(information: &Value) -> Vec<String> {
    value_at_value(
        information,
        &[
            "classificationInformation",
            "common:classification",
            "common:class",
        ],
    )
    .map(items)
    .unwrap_or_default()
    .into_iter()
    .filter_map(localized_text)
    .map(ToOwned::to_owned)
    .collect()
}

fn value_at<'a>(object: &'a Map<String, Value>, path: &[&str]) -> Option<&'a Value> {
    let (first, rest) = path.split_first()?;
    let mut current = object.get(*first)?;
    for field in rest {
        current = current.get(*field)?;
    }
    Some(current)
}

fn value_at_value<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let mut current = value;
    for field in path {
        current = current.get(*field)?;
    }
    Some(current)
}

fn field_text<'a>(object: &'a Map<String, Value>, field: &str) -> Option<&'a str> {
    object.get(field).and_then(scalar_text)
}

fn field_text_value<'a>(value: &'a Value, field: &str) -> Option<&'a str> {
    value.get(field).and_then(scalar_text)
}

fn scalar_text(value: &Value) -> Option<&str> {
    value.as_str()
}

fn localized_text(value: &Value) -> Option<&str> {
    value
        .as_str()
        .or_else(|| value.get("#text").and_then(Value::as_str))
}

fn text_value(value: &Value) -> Option<&str> {
    scalar_text(value).or_else(|| localized_text(value))
}

fn items(value: &Value) -> Vec<&Value> {
    value
        .as_array()
        .map_or_else(|| vec![value], |items| items.iter().collect())
}

fn stable_id(seed: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, seed.as_bytes()).to_string()
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tempfile::tempdir;
    use tidas_runtime::{CancellationToken, MemoryBudget};
    use tidas_validation::{ValidationRequest, validate_tidas_package};

    use super::*;
    use crate::report::IssueSpool;
    use crate::writers::{TidasWriteRequest, write_tidas_package};

    fn write_support_entities(root: &Path) {
        std::fs::write(
            root.join("contacts/contact.xml"),
            r#"<contactDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><contactInformation><dataSetInformation><common:UUID>66666666-6666-4666-8666-666666666666</common:UUID><common:shortName xml:lang="en">Steel team</common:shortName><common:name xml:lang="en">Steel team</common:name><contactAddress xml:lang="en">Zurich</contactAddress><telephone>+41</telephone><email>team@example.org</email><WWWAddress>https://example.org</WWWAddress><contactDescriptionOrComment xml:lang="en">Owner</contactDescriptionOrComment></dataSetInformation></contactInformation><administrativeInformation><publicationAndOwnership><common:dataSetVersion>20.20.002</common:dataSetVersion></publicationAndOwnership></administrativeInformation></contactDataSet>"#,
        )
        .unwrap();
        std::fs::write(
            root.join("sources/source.xml"),
            r#"<sourceDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><sourceInformation><dataSetInformation><common:UUID>77777777-7777-4777-8777-777777777777</common:UUID><common:shortName xml:lang="en">Steel source</common:shortName><sourceCitation>Citation</sourceCitation><sourceDescriptionOrComment xml:lang="en">Description</sourceDescriptionOrComment><referenceToDigitalFile uri="../external_docs/steel.png"/><referenceToDigitalFile uri="https://example.org/source"/></dataSetInformation></sourceInformation><administrativeInformation><publicationAndOwnership><common:dataSetVersion>20.20.002</common:dataSetVersion></publicationAndOwnership></administrativeInformation></sourceDataSet>"#,
        )
        .unwrap();
    }

    fn assert_valid_package(
        store: &CanonicalStore,
        output: &Path,
        cancellation: CancellationToken,
        memory_budget: MemoryBudget,
    ) {
        write_tidas_package(&TidasWriteRequest {
            store,
            output_dir: output,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        })
        .unwrap();
        let issues = output.with_extension("validation-issues.jsonl");
        let validation = validate_tidas_package(&ValidationRequest {
            input_dir: output.to_path_buf(),
            issue_spool: Some(issues.clone()),
            cancellation,
            memory_budget,
            queue_capacity: 2,
            progress: None,
        })
        .unwrap();
        assert!(
            validation.summary.ok,
            "{:?}\n{}",
            validation.summary,
            std::fs::read_to_string(issues).unwrap()
        );
    }

    #[test]
    fn non_flow_ilcd_reference_does_not_become_first_pollutant_output() {
        for reference_type in ["Other parameter", "Functional unit", "Production period"] {
            let directory = tempdir().unwrap();
            let xml = format!(
                r#"<processDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><processInformation><dataSetInformation><common:UUID>55555555-5555-4555-8555-555555555555</common:UUID><name><baseName xml:lang="en">Synthetic water coefficient</baseName></name></dataSetInformation><quantitativeReference type="{reference_type}"><functionalUnitOrOther xml:lang="en">1 tonne unwashed raw coal</functionalUnitOrOther><functionalUnitOrOther xml:lang="zh">1 吨未经洗选的原煤</functionalUnitOrOther></quantitativeReference></processInformation><modellingAndValidation><LCIMethodAndAllocation/></modellingAndValidation><exchanges><exchange dataSetInternalID="4"><referenceToFlowDataSet refObjectId="fe0acd60-3ddc-11dd-a8ca-0050c2490048"><common:shortDescription xml:lang="en">Mercury</common:shortDescription></referenceToFlowDataSet><exchangeDirection>Output</exchangeDirection><meanAmount>0.000001</meanAmount></exchange></exchanges></processDataSet>"#
            );
            let cancellation = CancellationToken::default();
            let converted =
                tidas_conversion::convert_xml_to_json(xml.as_bytes(), &cancellation).unwrap();
            let document: Value = serde_json::from_slice(&converted).unwrap();
            let mut entity = entity_from_document(&document, "synthetic.xml").unwrap();
            let process_id = entity.internal_id.clone();
            let exchanges = entity.raw.remove("exchanges").unwrap();
            let mut store = CanonicalStore::create(Some(directory.path())).unwrap();
            store.begin_process_exchanges(&process_id).unwrap();
            for exchange in exchanges.as_array().unwrap() {
                store
                    .add_process_exchange(&process_id, exchange.as_object().unwrap())
                    .unwrap();
            }
            store.add(&entity).unwrap();
            let output = directory.path().join("tidas");
            let memory_budget = MemoryBudget::new(16 * 1024 * 1024);
            write_tidas_package(&TidasWriteRequest {
                store: &store,
                output_dir: &output,
                cancellation: &cancellation,
                memory_budget: &memory_budget,
            })
            .unwrap();
            let written: Value = serde_json::from_slice(
                &std::fs::read(output.join("processes").join(format!("{process_id}.json")))
                    .unwrap(),
            )
            .unwrap();
            let process = &written["processDataSet"];
            let reference = &process["processInformation"]["quantitativeReference"];
            assert_eq!(reference["@type"], reference_type);
            assert!(reference.get("referenceToReferenceFlow").is_none());
            assert_eq!(
                reference["functionalUnitOrOther"].as_array().unwrap().len(),
                2
            );
            assert!(
                process["modellingAndValidation"]["LCIMethodAndAllocation"]
                    .get("typeOfDataSet")
                    .is_none()
            );
            assert_eq!(
                process["exchanges"]["exchange"][0]["referenceToFlowDataSet"]["@refObjectId"],
                "fe0acd60-3ddc-11dd-a8ca-0050c2490048"
            );
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn ilcd_identifiers_relations_and_values_survive_native_import() {
        let directory = tempdir().unwrap();
        let root = directory.path().join("ilcd");
        for category in [
            "contacts",
            "sources",
            "unitgroups",
            "flowproperties",
            "flows",
            "processes",
        ] {
            std::fs::create_dir_all(root.join(category)).unwrap();
        }
        write_support_entities(&root);
        let unit_id = "22222222-2222-4222-8222-222222222222";
        let property_id = "33333333-3333-4333-8333-333333333333";
        let ncv_property_id = "88888888-8888-4888-8888-888888888888";
        let flow_id = "44444444-4444-4444-8444-444444444444";
        let process_id = "55555555-5555-4555-8555-555555555555";
        std::fs::write(
            root.join("unitgroups/u.xml"),
            format!(
                r#"<unitGroupDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><unitGroupInformation><dataSetInformation><common:UUID>{unit_id}</common:UUID><common:name xml:lang="en">Units of mass</common:name></dataSetInformation><quantitativeReference><referenceToReferenceUnit>1</referenceToReferenceUnit></quantitativeReference></unitGroupInformation><units><unit dataSetInternalID="1"><name>kg</name><meanValue>1</meanValue></unit></units></unitGroupDataSet>"#
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("flowproperties/p.xml"),
            format!(
                r#"<flowPropertyDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><flowPropertiesInformation><dataSetInformation><common:UUID>{property_id}</common:UUID><common:name xml:lang="en">Mass</common:name></dataSetInformation><quantitativeReference><referenceToReferenceUnitGroup refObjectId="{unit_id}"><common:shortDescription xml:lang="en">Units of mass</common:shortDescription></referenceToReferenceUnitGroup></quantitativeReference></flowPropertiesInformation></flowPropertyDataSet>"#
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("flowproperties/ncv.xml"),
            format!(
                r#"<flowPropertyDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><flowPropertiesInformation><dataSetInformation><common:UUID>{ncv_property_id}</common:UUID><common:name xml:lang="en">Net calorific value</common:name></dataSetInformation><quantitativeReference><referenceToReferenceUnitGroup refObjectId="{unit_id}"><common:shortDescription xml:lang="en">Energy per mass</common:shortDescription></referenceToReferenceUnitGroup></quantitativeReference></flowPropertiesInformation></flowPropertyDataSet>"#
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("flows/f.xml"),
            format!(
                r#"<flowDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><flowInformation><dataSetInformation><common:UUID>{flow_id}</common:UUID><name><baseName xml:lang="en">Steel</baseName><treatmentStandardsRoutes xml:lang="en">production route</treatmentStandardsRoutes><mixAndLocationTypes xml:lang="en">GLO</mixAndLocationTypes></name><CASNumber>7439-89-6</CASNumber><sumFormula>Fe</sumFormula></dataSetInformation><quantitativeReference><referenceToReferenceFlowProperty>0</referenceToReferenceFlowProperty></quantitativeReference></flowInformation><modellingAndValidation><LCIMethod><typeOfDataSet>Product flow</typeOfDataSet></LCIMethod></modellingAndValidation><administrativeInformation><publicationAndOwnership><common:dataSetVersion>20.25.001</common:dataSetVersion></publicationAndOwnership></administrativeInformation><flowProperties><flowProperty dataSetInternalID="0"><referenceToFlowPropertyDataSet refObjectId="{property_id}"><common:shortDescription xml:lang="en">Mass</common:shortDescription></referenceToFlowPropertyDataSet><meanValue>1</meanValue></flowProperty><flowProperty dataSetInternalID="1"><referenceToFlowPropertyDataSet refObjectId="{ncv_property_id}"><common:shortDescription xml:lang="en">Net calorific value</common:shortDescription></referenceToFlowPropertyDataSet><meanValue>42.500</meanValue></flowProperty></flowProperties></flowDataSet>"#
            ),
        )
        .unwrap();
        std::fs::write(
            root.join("processes/x.xml"),
            format!(
                r#"<processDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><processInformation><dataSetInformation><common:UUID>{process_id}</common:UUID><name><baseName xml:lang="en">Steel process</baseName></name></dataSetInformation><quantitativeReference><referenceToReferenceFlow>0</referenceToReferenceFlow></quantitativeReference><time><common:referenceYear>2022</common:referenceYear></time><geography><locationOfOperationSupplyOrProduction location="GLO"/></geography></processInformation><administrativeInformation><publicationAndOwnership><common:dataSetVersion>20.25.001</common:dataSetVersion></publicationAndOwnership></administrativeInformation><exchanges><exchange dataSetInternalID="0"><referenceToFlowDataSet refObjectId="{flow_id}"><common:shortDescription xml:lang="en">Steel</common:shortDescription></referenceToFlowDataSet><exchangeDirection>Output</exchangeDirection><meanAmount>1</meanAmount><relativeStandardDeviation95In>12.3</relativeStandardDeviation95In><dataDerivationTypeStatus>Calculated</dataDerivationTypeStatus><generalComment xml:lang="en">measured batch</generalComment></exchange></exchanges></processDataSet>"#
            ),
        )
        .unwrap();
        let cancellation = CancellationToken::default();
        let memory_budget = MemoryBudget::new(16 * 1024 * 1024);
        let mut store = CanonicalStore::create(Some(directory.path())).unwrap();
        let mut issues = IssueSpool::new(Vec::new(), 64 * 1024);
        IlcdAdapter
            .read(
                &AdapterContext {
                    source: &root,
                    cancellation: &cancellation,
                    memory_budget: &memory_budget,
                    max_entry_bytes: 1024 * 1024,
                },
                &mut store,
                &mut issues,
            )
            .unwrap();
        issues.finish().unwrap();
        assert_eq!(
            store.get("flows", flow_id).unwrap().unwrap().raw["flowPropertyRefId"],
            property_id
        );
        let source_properties = store.get("flows", flow_id).unwrap().unwrap().raw["flowProperties"]
            .as_array()
            .unwrap()
            .clone();
        assert_eq!(source_properties.len(), 2);
        assert_eq!(source_properties[0]["flowProperty"]["@id"], property_id);
        assert_eq!(source_properties[1]["flowProperty"]["@id"], ncv_property_id);
        assert_eq!(source_properties[1]["conversionFactor"], "42.500");
        assert_eq!(
            store.get("flows", flow_id).unwrap().unwrap().raw["version"],
            "20.25.001"
        );
        assert_eq!(
            store.get("flows", flow_id).unwrap().unwrap().raw["CASNumber"],
            "7439-89-6"
        );
        assert_eq!(
            store.get("processes", process_id).unwrap().unwrap().raw["referenceYear"],
            2022
        );
        let process = store.get("processes", process_id).unwrap().unwrap();
        let exchange = store
            .iter_process_exchanges(&process.internal_id)
            .unwrap()
            .next()
            .unwrap()
            .unwrap();
        assert_eq!(exchange["isQuantitativeReference"], true);
        assert_eq!(exchange["dataDerivationTypeStatus"], "Calculated");
        assert_eq!(store.counts()["contacts"], 1);
        assert_eq!(store.counts()["sources"], 1);
        let output = directory.path().join("tidas");
        assert_valid_package(&store, &output, cancellation, memory_budget);
        let written_process: Value = serde_json::from_slice(
            &std::fs::read(output.join("processes").join(format!("{process_id}.json"))).unwrap(),
        )
        .unwrap();
        assert_eq!(
            written_process["processDataSet"]["processInformation"]["quantitativeReference"]["referenceToReferenceFlow"],
            "0"
        );
        assert_eq!(
            written_process["processDataSet"]["exchanges"]["exchange"][0]["@dataSetInternalID"],
            "0"
        );
        let written_flow: Value = serde_json::from_slice(
            &std::fs::read(output.join("flows").join(format!("{flow_id}.json"))).unwrap(),
        )
        .unwrap();
        let written_properties = written_flow["flowDataSet"]["flowProperties"]["flowProperty"]
            .as_array()
            .unwrap();
        assert_eq!(written_properties.len(), 2);
        assert_eq!(written_properties[0]["@dataSetInternalID"], "1");
        assert_eq!(
            written_properties[0]["referenceToFlowPropertyDataSet"]["@refObjectId"],
            property_id
        );
        assert_eq!(
            written_properties[1]["referenceToFlowPropertyDataSet"]["@refObjectId"],
            ncv_property_id
        );
        assert_eq!(written_properties[1]["meanValue"], "42.500");
    }
}
