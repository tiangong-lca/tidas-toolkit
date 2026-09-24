use serde_json::{Value, json};
use uuid::Uuid;

use crate::model::CanonicalEntity;

pub const DEFAULT_VERSION: &str = "00.00.001";
pub const PLACEHOLDER_TIMESTAMP: &str = "1900-01-01T00:00:00Z";
pub const CONTACT_NAME: &str = "TianGong LCA import tooling";
pub const FORMAT_SOURCE_NAME: &str = "ILCD format";
pub const COMPLIANCE_SOURCE_NAME: &str = "External LCA source compliance context";

pub fn stable_id(seed: &str) -> String {
    Uuid::new_v5(&Uuid::NAMESPACE_URL, seed.as_bytes()).to_string()
}

pub fn contact_id() -> String {
    stable_id("tidas-tools/import/contact")
}

pub fn format_source_id() -> String {
    stable_id("tidas-tools/import/ilcd-format")
}

pub fn compliance_source_id() -> String {
    stable_id("tidas-tools/import/not-defined-compliance")
}

pub fn localized(text: impl Into<String>) -> Value {
    json!({"@xml:lang": "en", "#text": text.into()})
}

/// Keep the language tags and text of an ILCD field instead of flattening it to
/// the first language. XML scalar text has no language tag, so it receives the
/// same English default used by the other native import writers.
pub fn localized_from_source(value: &Value) -> Value {
    match value {
        Value::String(text) => localized(text),
        Value::Array(items) => Value::Array(items.iter().map(localized_from_source).collect()),
        _ => value.clone(),
    }
}

pub fn dataset_ref(ref_type: &str, id: &str, description: &str, category: &str) -> Value {
    dataset_ref_version(ref_type, id, description, category, None)
}

pub fn dataset_ref_version(
    ref_type: &str,
    id: &str,
    description: &str,
    category: &str,
    version: Option<&str>,
) -> Value {
    json!({
        "@type": ref_type,
        "@refObjectId": id,
        "@version": version.unwrap_or(DEFAULT_VERSION),
        "@uri": format!("../{category}/{id}.json"),
        "common:shortDescription": localized(description),
    })
}

pub fn compliance_declarations(process: bool) -> Value {
    let mut compliance = serde_json::Map::from_iter([
        (
            "common:referenceToComplianceSystem".to_owned(),
            dataset_ref(
                "source data set",
                &compliance_source_id(),
                COMPLIANCE_SOURCE_NAME,
                "sources",
            ),
        ),
        (
            "common:approvalOfOverallCompliance".to_owned(),
            Value::String("Not defined".to_owned()),
        ),
    ]);
    if process {
        for field in [
            "common:nomenclatureCompliance",
            "common:methodologicalCompliance",
            "common:reviewCompliance",
            "common:documentationCompliance",
            "common:qualityCompliance",
        ] {
            compliance.insert(field.to_owned(), Value::String("Not defined".to_owned()));
        }
    }
    json!({"compliance": compliance})
}

pub fn administrative(category: &str, id: &str, process: bool) -> Value {
    administrative_version(category, id, process, DEFAULT_VERSION)
}

pub fn administrative_for_entity(category: &str, entity: &CanonicalEntity, process: bool) -> Value {
    let version = entity
        .raw
        .get("version")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .unwrap_or(DEFAULT_VERSION);
    administrative_version(category, &entity.internal_id, process, version)
}

pub(super) fn administrative_version(
    category: &str,
    id: &str,
    process: bool,
    version: &str,
) -> Value {
    let owner = dataset_ref("contact data set", &contact_id(), CONTACT_NAME, "contacts");
    let mut data_entry = serde_json::Map::from_iter([
        (
            "common:timeStamp".to_owned(),
            Value::String(PLACEHOLDER_TIMESTAMP.to_owned()),
        ),
        (
            "common:referenceToDataSetFormat".to_owned(),
            dataset_ref(
                "source data set",
                &format_source_id(),
                FORMAT_SOURCE_NAME,
                "sources",
            ),
        ),
    ]);
    if process {
        data_entry.insert(
            "common:referenceToPersonOrEntityEnteringTheData".to_owned(),
            owner.clone(),
        );
    }
    let page = match category {
        "unitgroups" => "unitgroups",
        "flowproperties" => "flowproperty",
        "flows" => "productFlow",
        "processes" => "process",
        "lifecyclemodels" => "lifecyclemodel",
        "contacts" => "contact",
        _ => "source",
    };
    let uri = if category == "unitgroups" {
        format!("https://lcdn.tiangong.earth/unitgroups/{id}?version={version}")
    } else {
        format!(
            "https://lcdn.tiangong.earth/datasetdetail/{page}.xhtml?uuid={id}&version={version}"
        )
    };
    let mut publication = serde_json::Map::from_iter([
        (
            "common:dataSetVersion".to_owned(),
            Value::String(version.to_owned()),
        ),
        ("common:permanentDataSetURI".to_owned(), Value::String(uri)),
        ("common:referenceToOwnershipOfDataSet".to_owned(), owner),
    ]);
    if process {
        publication.insert(
            "common:copyright".to_owned(),
            Value::String("false".to_owned()),
        );
        publication.insert(
            "common:licenseType".to_owned(),
            Value::String("Free of charge for all users and uses".to_owned()),
        );
    }
    json!({
        "dataEntryBy": data_entry,
        "publicationAndOwnership": publication,
    })
}

pub fn name_parts(name: &str, location: &str) -> Value {
    json!({
        "baseName": localized(name),
        "treatmentStandardsRoutes": localized("source-described route"),
        "mixAndLocationTypes": localized(location),
    })
}

pub fn import_trace(payload: &Value) -> Value {
    let mut payload = payload.clone();
    prune_empty(&mut payload);
    json!({
        "@xmlns:tidasimport": "https://tiangong.earth/tidas/import-trace/1.0",
        "tidasimport:sourceTrace": {
            "@marker": "TIDAS_IMPORT_TRACE_V1",
            "payload": payload,
        }
    })
}

fn prune_empty(value: &mut Value) -> bool {
    match value {
        Value::Object(object) => {
            object.retain(|_, value| !prune_empty(value));
            object.is_empty()
        }
        Value::Array(items) => {
            items.retain_mut(|value| !prune_empty(value));
            items.is_empty()
        }
        Value::Null => true,
        _ => false,
    }
}
