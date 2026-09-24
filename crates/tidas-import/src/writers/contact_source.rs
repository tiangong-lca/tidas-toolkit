use serde_json::{Map, Value, json};

use crate::model::CanonicalEntity;

use super::common::{COMPLIANCE_SOURCE_NAME, compliance_source_id};
use super::common::{
    CONTACT_NAME, FORMAT_SOURCE_NAME, administrative, administrative_for_entity, contact_id,
    format_source_id, import_trace, localized, localized_from_source,
};

pub fn contact() -> (String, Value) {
    let id = contact_id();
    let value = json!({
        "contactDataSet": {
            "@xmlns:common": "http://lca.jrc.it/ILCD/Common",
            "@xmlns": "http://lca.jrc.it/ILCD/Contact",
            "@xmlns:xsi": "http://www.w3.org/2001/XMLSchema-instance",
            "@version": "1.1",
            "@xsi:schemaLocation": "http://lca.jrc.it/ILCD/Contact ../../schemas/ILCD_ContactDataSet.xsd",
            "contactInformation": {
                "dataSetInformation": {
                    "common:UUID": id,
                    "common:shortName": localized(CONTACT_NAME),
                    "common:name": localized(CONTACT_NAME),
                    "classificationInformation": {
                        "common:classification": {
                            "common:class": {
                                "@level": "0",
                                "@classId": "5",
                                "#text": "Other"
                            }
                        }
                    }
                }
            },
            "administrativeInformation": administrative("contacts", &id, false)
        }
    });
    (id, value)
}

pub fn sources() -> [(String, Value); 2] {
    [
        source(
            format_source_id(),
            FORMAT_SOURCE_NAME,
            "1",
            "Data set formats",
        ),
        source(
            compliance_source_id(),
            COMPLIANCE_SOURCE_NAME,
            "3",
            "Compliance systems",
        ),
    ]
}

fn source(id: String, name: &str, class_id: &str, class_name: &str) -> (String, Value) {
    let value = json!({
        "sourceDataSet": {
            "@xmlns:common": "http://lca.jrc.it/ILCD/Common",
            "@xmlns": "http://lca.jrc.it/ILCD/Source",
            "@xmlns:xsi": "http://www.w3.org/2001/XMLSchema-instance",
            "@version": "1.1",
            "@xsi:schemaLocation": "http://lca.jrc.it/ILCD/Source ../../schemas/ILCD_SourceDataSet.xsd",
            "sourceInformation": {
                "dataSetInformation": {
                    "common:UUID": id,
                    "common:shortName": localized(name),
                    "classificationInformation": {
                        "common:classification": {
                            "common:class": {
                                "@level": "0",
                                "@classId": class_id,
                                "#text": class_name
                            }
                        }
                    }
                }
            },
            "administrativeInformation": administrative("sources", &id, false)
        }
    });
    (id, value)
}

pub fn canonical_contact(entity: &CanonicalEntity) -> Value {
    let name = entity.name.as_deref().unwrap_or("Imported contact");
    let mut information = Map::from_iter([
        (
            "common:UUID".to_owned(),
            Value::String(entity.internal_id.clone()),
        ),
        ("common:shortName".to_owned(), localized(name)),
        ("common:name".to_owned(), localized(name)),
        (
            "classificationInformation".to_owned(),
            json!({
                "common:classification": {
                    "common:class": {
                        "@level": "0",
                        "@classId": "5",
                        "#text": "Other"
                    }
                }
            }),
        ),
    ]);
    for (source, target, localized_value) in [
        ("address", "contactAddress", true),
        ("telephone", "telephone", false),
        ("email", "email", false),
        ("website", "WWWAddress", false),
        ("description", "contactDescriptionOrComment", true),
    ] {
        if let Some(value) = entity.raw.get(source).and_then(Value::as_str) {
            information.insert(
                target.to_owned(),
                if localized_value {
                    localized(value)
                } else {
                    Value::String(value.to_owned())
                },
            );
        }
    }
    if let Some(trace) = entity.raw.get("sourceTrace") {
        information.insert("common:other".to_owned(), import_trace(trace));
    }
    json!({
        "contactDataSet": {
            "@xmlns:common": "http://lca.jrc.it/ILCD/Common",
            "@xmlns": "http://lca.jrc.it/ILCD/Contact",
            "@xmlns:xsi": "http://www.w3.org/2001/XMLSchema-instance",
            "@version": "1.1",
            "@xsi:schemaLocation": "http://lca.jrc.it/ILCD/Contact ../../schemas/ILCD_ContactDataSet.xsd",
            "contactInformation": {
                "dataSetInformation": information
            },
            "administrativeInformation": administrative_for_entity("contacts", entity, false)
        }
    })
}

pub fn canonical_source(entity: &CanonicalEntity) -> Value {
    let name = entity.name.as_deref().unwrap_or("Imported source");
    let short_name = entity
        .raw
        .get("ilcdShortName")
        .map_or_else(|| localized(name), localized_from_source);
    let mut information = serde_json::Map::from_iter([
        (
            "common:UUID".to_owned(),
            Value::String(entity.internal_id.clone()),
        ),
        ("common:shortName".to_owned(), short_name),
        (
            "classificationInformation".to_owned(),
            json!({
                "common:classification": {
                    "common:class": {
                        "@level": "0",
                        "@classId": "6",
                        "#text": "Other source types"
                    }
                }
            }),
        ),
    ]);
    if let Some(citation) = entity.raw.get("textReference").and_then(Value::as_str) {
        information.insert(
            "sourceCitation".to_owned(),
            Value::String(citation.to_owned()),
        );
    }
    if let Some(citation) = entity.raw.get("sourceCitation").and_then(Value::as_str) {
        information.insert(
            "sourceCitation".to_owned(),
            Value::String(citation.to_owned()),
        );
    }
    if let Some(publication_type) = entity.raw.get("publicationType").and_then(Value::as_str) {
        information.insert(
            "publicationType".to_owned(),
            Value::String(publication_type.to_owned()),
        );
    }
    insert_source_description(entity, &mut information);
    if let Some(files) = entity.raw.get("referenceToDigitalFile") {
        let references = match files {
            Value::Array(files) => files
                .iter()
                .filter_map(Value::as_str)
                .map(|uri| json!({"@uri": digital_file_uri(uri)}))
                .collect::<Vec<_>>(),
            Value::String(uri) => vec![json!({"@uri": digital_file_uri(uri)})],
            _ => Vec::new(),
        };
        if !references.is_empty() {
            information.insert(
                "referenceToDigitalFile".to_owned(),
                if references.len() == 1 {
                    references.into_iter().next().unwrap()
                } else {
                    Value::Array(references)
                },
            );
        }
    }
    if let Some(trace) = entity.raw.get("sourceTrace") {
        information.insert("common:other".to_owned(), import_trace(trace));
    }
    json!({
        "sourceDataSet": {
            "@xmlns:common": "http://lca.jrc.it/ILCD/Common",
            "@xmlns": "http://lca.jrc.it/ILCD/Source",
            "@xmlns:xsi": "http://www.w3.org/2001/XMLSchema-instance",
            "@version": "1.1",
            "@xsi:schemaLocation": "http://lca.jrc.it/ILCD/Source ../../schemas/ILCD_SourceDataSet.xsd",
            "sourceInformation": {"dataSetInformation": information},
            "administrativeInformation": administrative_for_entity("sources", entity, false)
        }
    })
}

fn insert_source_description(entity: &CanonicalEntity, information: &mut Map<String, Value>) {
    if let Some(description) = entity.raw.get("ilcdDescription") {
        information.insert(
            "sourceDescriptionOrComment".to_owned(),
            localized_from_source(description),
        );
        return;
    }
    let mut description = entity
        .raw
        .get("description")
        .and_then(Value::as_str)
        .map(ToOwned::to_owned)
        .unwrap_or_default();
    if let Some(url) = entity.raw.get("url").and_then(Value::as_str) {
        if !description.is_empty() {
            description.push(' ');
        }
        description.push_str("External source URL: ");
        description.push_str(url);
    }
    if !description.is_empty() {
        information.insert(
            "sourceDescriptionOrComment".to_owned(),
            localized(description),
        );
    }
}

fn digital_file_uri(uri: &str) -> String {
    if uri.contains(':') {
        uri.to_owned()
    } else {
        format!("file:{uri}")
    }
}
