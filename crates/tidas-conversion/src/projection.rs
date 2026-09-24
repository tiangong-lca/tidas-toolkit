use std::collections::BTreeMap;
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};
use sha2::{Digest, Sha256};

use crate::ConversionError;

pub const EILCD_PROJECTION_RECOVERY_SCHEMA_V1: &str = "tidas.eilcd-projection-recovery.v1";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectionRestorationV1 {
    pub path: String,
    pub original: Value,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct EilcdProjectionRecoveryV1 {
    pub schema_version: String,
    pub source_semantic_sha256: String,
    pub adaptations: BTreeMap<String, u64>,
    pub restorations: Vec<ProjectionRestorationV1>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EilcdProjectionV1 {
    pub document: Value,
    pub recovery: Option<EilcdProjectionRecoveryV1>,
}

/// Projects the TIDAS representation onto the stricter eILCD XML surface.
///
/// TIDAS remains the source representation. Every changed fragment is retained in a
/// deterministic recovery contract instead of being silently discarded.
pub fn project_tidas_to_eilcd(
    document: &Value,
    category: &str,
) -> Result<EilcdProjectionV1, ConversionError> {
    let mut projected = document.clone();
    let source_semantic_sha256 = semantic_sha256(document)?;
    let mut builder = RecoveryBuilder::default();
    adapt_value(&mut projected, "", category, &mut builder);
    let recovery = (!builder.restorations.is_empty()).then_some(EilcdProjectionRecoveryV1 {
        schema_version: EILCD_PROJECTION_RECOVERY_SCHEMA_V1.to_owned(),
        source_semantic_sha256,
        adaptations: builder.adaptations,
        restorations: builder.restorations,
    });
    Ok(EilcdProjectionV1 {
        document: projected,
        recovery,
    })
}

pub fn restore_tidas_projection(
    document: &mut Value,
    recovery: &EilcdProjectionRecoveryV1,
) -> Result<(), ConversionError> {
    if recovery.schema_version != EILCD_PROJECTION_RECOVERY_SCHEMA_V1 {
        return Err(ConversionError::UnsupportedProjectionRecovery(
            recovery.schema_version.clone(),
        ));
    }
    for restoration in &recovery.restorations {
        set_pointer(document, &restoration.path, restoration.original.clone())?;
    }
    let actual = semantic_sha256(document)?;
    if actual != recovery.source_semantic_sha256 {
        return Err(ConversionError::ProjectionRecoveryMismatch {
            expected: recovery.source_semantic_sha256.clone(),
            actual,
        });
    }
    Ok(())
}

pub(crate) fn semantic_sha256(document: &Value) -> Result<String, ConversionError> {
    let normalized = normalize(document.clone(), None);
    let bytes = serde_json::to_vec(&normalized)?;
    Ok(hex_digest(Sha256::digest(bytes)))
}

#[derive(Default)]
struct RecoveryBuilder {
    restorations: Vec<ProjectionRestorationV1>,
    adaptations: BTreeMap<String, u64>,
}

impl RecoveryBuilder {
    fn record(&mut self, path: &str, original: &Value, rule: &str) {
        if self
            .restorations
            .iter()
            .any(|item| is_same_or_ancestor(&item.path, path))
        {
            self.increment(rule);
            return;
        }
        let mut reconstructed = original.clone();
        for descendant in self
            .restorations
            .iter()
            .filter(|item| is_same_or_ancestor(path, &item.path))
        {
            let relative = descendant.path.strip_prefix(path).unwrap_or_default();
            set_pointer(&mut reconstructed, relative, descendant.original.clone())
                .expect("a descendant recovery path must fit its recorded ancestor");
        }
        self.restorations
            .retain(|item| !is_same_or_ancestor(path, &item.path));
        self.restorations.push(ProjectionRestorationV1 {
            path: path.to_owned(),
            original: reconstructed,
        });
        self.restorations
            .sort_by(|left, right| left.path.cmp(&right.path));
        self.increment(rule);
    }

    fn increment(&mut self, rule: &str) {
        *self.adaptations.entry(rule.to_owned()).or_default() += 1;
    }
}

fn adapt_value(value: &mut Value, path: &str, category: &str, recovery: &mut RecoveryBuilder) {
    match value {
        Value::Object(object) => adapt_object(object, path, category, recovery),
        Value::Array(items) => {
            for (index, child) in items.iter_mut().enumerate() {
                adapt_value(
                    child,
                    &join_pointer(path, &index.to_string()),
                    category,
                    recovery,
                );
            }
        }
        _ => {}
    }
}

fn adapt_object(
    object: &mut Map<String, Value>,
    path: &str,
    category: &str,
    recovery: &mut RecoveryBuilder,
) {
    adapt_reference_uri(object, path, recovery);
    merge_duplicate_languages(object, path, recovery);

    match category {
        "processes" => adapt_process_object(object, path, recovery),
        "flows" => adapt_flow_object(object, path, recovery),
        "lciamethods" => adapt_lcia_method_object(object, path, recovery),
        "lifecyclemodels" => adapt_lifecycle_model_object(object, path, recovery),
        "sources" => adapt_source_object(object, path, recovery),
        _ => {}
    }

    let extension_keys: Vec<String> = object
        .keys()
        .filter(|key| key.starts_with("tidasimport:") || key.starts_with("unmatched:"))
        .cloned()
        .collect();
    for key in extension_keys {
        remove_key(
            object,
            path,
            &key,
            "omit-unbound-extension-element",
            recovery,
        );
    }

    let empty_array_keys: Vec<String> = object
        .iter()
        .filter(|(_, value)| value.as_array().is_some_and(Vec::is_empty))
        .map(|(key, _)| key.clone())
        .collect();
    for key in empty_array_keys {
        remove_key(object, path, &key, "omit-empty-array", recovery);
    }
    let normalized_text_keys: Vec<String> = object
        .iter()
        .filter_map(|(key, value)| {
            let text = value.as_str()?;
            (!key.starts_with('@') && text != project_xml_text(text)).then(|| key.clone())
        })
        .collect();
    for key in normalized_text_keys {
        let field_path = join_pointer(path, &escape_pointer(&key));
        let original = object.get(&key).expect("key exists").clone();
        recovery.record(&field_path, &original, "normalize-xml-character-data");
        if let Some(text) = original.as_str() {
            object.insert(key, Value::String(project_xml_text(text)));
        }
    }
    if object.get("#text").is_some_and(is_empty_scalar) {
        remove_key(object, path, "#text", "omit-empty-text", recovery);
    }

    remove_empty_optional_children(object, path, recovery);
    let keys: Vec<String> = object.keys().cloned().collect();
    for key in keys {
        if let Some(child) = object.get_mut(&key) {
            adapt_value(
                child,
                &join_pointer(path, &escape_pointer(&key)),
                category,
                recovery,
            );
        }
    }
    remove_empty_optional_children(object, path, recovery);
}

fn adapt_reference_uri(
    object: &mut Map<String, Value>,
    path: &str,
    recovery: &mut RecoveryBuilder,
) {
    let Some(uri) = object.get("@uri").and_then(Value::as_str) else {
        return;
    };
    let Some(prefix) = uri.strip_suffix(".json") else {
        return;
    };
    let field_path = join_pointer(path, "@uri");
    recovery.record(
        &field_path,
        object.get("@uri").expect("URI exists"),
        "map-tidas-reference-uri",
    );
    object.insert("@uri".to_owned(), Value::String(format!("{prefix}.xml")));
}

fn merge_duplicate_languages(
    object: &mut Map<String, Value>,
    path: &str,
    recovery: &mut RecoveryBuilder,
) {
    let keys: Vec<String> = object.keys().cloned().collect();
    for key in keys {
        let Some(Value::Array(items)) = object.get(&key) else {
            continue;
        };
        let mut counts = BTreeMap::<String, usize>::new();
        for item in items {
            if let Some(language) = item.get("@xml:lang").and_then(Value::as_str) {
                *counts.entry(language.to_owned()).or_default() += 1;
            }
        }
        if !counts.values().any(|count| *count > 1) {
            continue;
        }
        let field_path = join_pointer(path, &escape_pointer(&key));
        recovery.record(
            &field_path,
            object.get(&key).expect("key exists"),
            "merge-localized-language",
        );
        let mut merged = Vec::<Value>::new();
        let mut positions = BTreeMap::<String, usize>::new();
        for item in items {
            let Some(language) = item.get("@xml:lang").and_then(Value::as_str) else {
                merged.push(item.clone());
                continue;
            };
            if let Some(position) = positions.get(language).copied() {
                let existing = &mut merged[position];
                let left = existing
                    .get("#text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                let right = item
                    .get("#text")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_owned();
                if let Some(existing_object) = existing.as_object_mut() {
                    existing_object.insert(
                        "#text".to_owned(),
                        Value::String(match (left.is_empty(), right.is_empty()) {
                            (true, _) => right,
                            (_, true) => left,
                            _ => format!("{left}\n\n{right}"),
                        }),
                    );
                }
            } else {
                positions.insert(language.to_owned(), merged.len());
                merged.push(item.clone());
            }
        }
        object.insert(key, Value::Array(merged));
    }
}

fn adapt_process_object(
    object: &mut Map<String, Value>,
    path: &str,
    recovery: &mut RecoveryBuilder,
) {
    if path.ends_with("/processInformation/time")
        && object.contains_key("timeRepresentativenessDescription")
    {
        let original = Value::Object(object.clone());
        recovery.record(path, &original, "map-process-time-description");
        if let Some(value) = object.remove("timeRepresentativenessDescription") {
            object.insert("common:timeRepresentativenessDescription".to_owned(), value);
        }
    }
    if is_process_exchange_object(path) {
        remove_key(
            object,
            path,
            "quantitativeReference",
            "omit-tidas-process-extension",
            recovery,
        );
    }
    for key in [
        "LCIAResult",
        "generatedFromLifecycleModel",
        "flowProperties",
        "common:reviewDetails",
        "reviewDetails",
    ] {
        remove_key(object, path, key, "omit-tidas-process-extension", recovery);
    }
    if object.contains_key("variableParameter")
        && object
            .get("variableParameter")
            .is_some_and(|value| value.get("@name").is_none() && value.get("name").is_none())
    {
        remove_key(
            object,
            path,
            "variableParameter",
            "omit-incomplete-variable-parameter",
            recovery,
        );
    }
    for key in ["common:dataQualityIndicators", "dataQualityIndicators"] {
        if object.contains_key(key)
            && object.get(key).is_some_and(|value| {
                !has_nonempty_child(value, "common:dataQualityIndicator")
                    && !has_nonempty_child(value, "dataQualityIndicator")
            })
        {
            remove_key(
                object,
                path,
                key,
                "omit-empty-data-quality-indicators",
                recovery,
            );
        }
    }
    truncate_text(object, path, 500, recovery);
    if object.contains_key("unmatched:placeholder") {
        remove_key(
            object,
            path,
            "unmatched:placeholder",
            "omit-import-placeholder",
            recovery,
        );
    }
}

fn is_process_exchange_object(path: &str) -> bool {
    const EXCHANGE_PATH: &str = "/processDataSet/exchanges/exchange";
    path == EXCHANGE_PATH
        || path
            .strip_prefix("/processDataSet/exchanges/exchange/")
            .is_some_and(|index| index.parse::<usize>().is_ok())
}

fn adapt_flow_object(object: &mut Map<String, Value>, path: &str, recovery: &mut RecoveryBuilder) {
    for key in [
        "common:dateOfLastRevision",
        "common:documentationCompliance",
        "common:methodologicalCompliance",
        "common:nomenclatureCompliance",
        "common:qualityCompliance",
        "common:reviewCompliance",
        "functionalUnitFlowProperties",
    ] {
        remove_key(
            object,
            path,
            key,
            "omit-unsupported-flow-extension",
            recovery,
        );
    }
    if object.get("@classId").is_some_and(is_empty_scalar) && object.contains_key("@catId") {
        remove_key(
            object,
            path,
            "@classId",
            "omit-empty-elementary-class-id",
            recovery,
        );
    }
    if path.ends_with("/modellingAndValidation") {
        remove_key(
            object,
            path,
            "validation",
            "omit-unsupported-flow-validation",
            recovery,
        );
    }
    if path.ends_with("/publicationAndOwnership") {
        for key in [
            "common:accessRestrictions",
            "common:copyright",
            "common:licenseType",
        ] {
            remove_key(
                object,
                path,
                key,
                "omit-unsupported-flow-publication-metadata",
                recovery,
            );
        }
    }
    if path.ends_with("/dataSetInformation") {
        remove_key(
            object,
            path,
            "common:shortName",
            "omit-unsupported-flow-short-name",
            recovery,
        );
    }
    truncate_text(object, path, 500, recovery);
}

fn adapt_lifecycle_model_object(
    object: &mut Map<String, Value>,
    path: &str,
    recovery: &mut RecoveryBuilder,
) {
    for key in [
        "common:workflowAndPublicationStatus",
        "common:dateOfLastRevision",
    ] {
        remove_key(
            object,
            path,
            key,
            "omit-unsupported-lifecycle-model-metadata",
            recovery,
        );
    }
    if path.ends_with("/dataEntryBy") {
        for key in [
            "common:referenceToConvertedOriginalDataSetFrom",
            "common:referenceToDataSetUseApproval",
        ] {
            remove_key(
                object,
                path,
                key,
                "omit-unsupported-lifecycle-model-source-reference",
                recovery,
            );
        }
    }
    if path.ends_with("/name") && object.contains_key("flowProperties") {
        let original = Value::Object(object.clone());
        recovery.record(path, &original, "map-lifecycle-model-flow-properties");
        if let Some(value) = object.remove("flowProperties") {
            let mut combined = as_repeated_items(
                object
                    .remove("functionalUnitFlowProperties")
                    .unwrap_or(Value::Array(Vec::new())),
            );
            combined.extend(as_repeated_items(value));
            object.insert(
                "functionalUnitFlowProperties".to_owned(),
                Value::Array(combined),
            );
            merge_duplicate_languages(object, path, recovery);
        }
    }
    if object.contains_key("@type") && is_lifecycle_review_record(path) {
        remove_key(
            object,
            path,
            "@type",
            "omit-unsupported-lifecycle-model-review-type",
            recovery,
        );
    }
    if let Some(Value::Array(items)) = object.get("compliance")
        && items.len() > 1
    {
        let field_path = join_pointer(path, "compliance");
        recovery.record(
            &field_path,
            object.get("compliance").expect("key exists"),
            "select-primary-lifecycle-model-compliance",
        );
        object.insert("compliance".to_owned(), items[0].clone());
    }
}

fn adapt_lcia_method_object(
    object: &mut Map<String, Value>,
    path: &str,
    recovery: &mut RecoveryBuilder,
) {
    for key in ["common:scope", "common:dateOfLastRevision"] {
        remove_key(
            object,
            path,
            key,
            "omit-unsupported-lcia-method-metadata",
            recovery,
        );
    }
}

fn adapt_source_object(
    object: &mut Map<String, Value>,
    path: &str,
    recovery: &mut RecoveryBuilder,
) {
    for key in [
        "common:dateOfLastRevision",
        "common:copyright",
        "common:accessRestrictions",
        "common:licenseType",
        "common:referenceToPersonOrEntityEnteringTheData",
    ] {
        remove_key(
            object,
            path,
            key,
            "omit-unsupported-source-metadata",
            recovery,
        );
    }
    if let Some(Value::String(citation)) = object.get("sourceCitation")
        && citation.chars().count() > 1_000
    {
        let field_path = join_pointer(path, "sourceCitation");
        recovery.record(
            &field_path,
            object.get("sourceCitation").expect("key exists"),
            "truncate-ilcd-source-citation",
        );
        object.insert(
            "sourceCitation".to_owned(),
            Value::String(citation.chars().take(1_000).collect()),
        );
    }
}

fn as_repeated_items(value: Value) -> Vec<Value> {
    match value {
        Value::Array(items) => items,
        Value::Null => Vec::new(),
        item => vec![item],
    }
}

fn has_nonempty_child(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(|child| match child {
        Value::Array(items) => !items.is_empty(),
        Value::Null => false,
        _ => true,
    })
}

fn truncate_text(
    object: &mut Map<String, Value>,
    path: &str,
    maximum: usize,
    recovery: &mut RecoveryBuilder,
) {
    if !(path.contains("/generalComment")
        || path.contains("/comment")
        || path.contains("/baseName"))
    {
        return;
    }
    let Some(Value::String(text)) = object.get("#text") else {
        return;
    };
    if text.chars().count() <= maximum {
        return;
    }
    let text_path = join_pointer(path, "#text");
    recovery.record(
        &text_path,
        object.get("#text").expect("key exists"),
        "truncate-ilcd-multilang-text",
    );
    object.insert(
        "#text".to_owned(),
        Value::String(text.chars().take(maximum).collect()),
    );
}

fn is_lifecycle_review_record(path: &str) -> bool {
    let mut segments = path.rsplit('/');
    let last = segments.next().unwrap_or_default();
    last == "review"
        || (last.parse::<usize>().is_ok() && segments.next().is_some_and(|part| part == "review"))
}

fn remove_empty_optional_children(
    object: &mut Map<String, Value>,
    path: &str,
    recovery: &mut RecoveryBuilder,
) {
    let keys: Vec<String> = object
        .iter()
        .filter_map(|(key, value)| {
            let empty = value.as_object().is_some_and(|child| {
                child.is_empty()
                    || (key == "common:other" && child.keys().all(|name| name.starts_with('@')))
            });
            (empty
                && matches!(
                    key.as_str(),
                    "geography" | "variableParameter" | "common:other"
                ))
            .then(|| key.clone())
        })
        .collect();
    for key in keys {
        remove_key(object, path, &key, "omit-empty-optional-element", recovery);
    }
}

fn remove_key(
    object: &mut Map<String, Value>,
    path: &str,
    key: &str,
    rule: &str,
    recovery: &mut RecoveryBuilder,
) {
    let Some(original) = object.get(key).cloned() else {
        return;
    };
    recovery.record(&join_pointer(path, &escape_pointer(key)), &original, rule);
    object.remove(key);
}

fn set_pointer(root: &mut Value, pointer: &str, value: Value) -> Result<(), ConversionError> {
    if pointer.is_empty() {
        *root = value;
        return Ok(());
    }
    let parts: Vec<String> = pointer
        .split('/')
        .skip(1)
        .map(|part| part.replace("~1", "/").replace("~0", "~"))
        .collect();
    set_pointer_parts(root, &parts, value, pointer)
}

fn set_pointer_parts(
    current: &mut Value,
    parts: &[String],
    value: Value,
    pointer: &str,
) -> Result<(), ConversionError> {
    let Some((part, remaining)) = parts.split_first() else {
        *current = value;
        return Ok(());
    };
    if current.is_null() {
        *current = Value::Object(Map::new());
    }
    if let Value::Object(object) = current {
        if part == "0" && !object.contains_key(part) {
            return set_pointer_parts(current, remaining, value, pointer);
        }
        if remaining.is_empty() {
            object.insert(part.clone(), value);
            return Ok(());
        }
        let child = object
            .entry(part.clone())
            .or_insert_with(|| Value::Object(Map::new()));
        return set_pointer_parts(child, remaining, value, pointer);
    }
    if let Value::Array(items) = current {
        let index = part
            .parse::<usize>()
            .map_err(|_| ConversionError::InvalidProjectionRecoveryPath(pointer.to_owned()))?;
        let child = items
            .get_mut(index)
            .ok_or_else(|| ConversionError::InvalidProjectionRecoveryPath(pointer.to_owned()))?;
        return set_pointer_parts(child, remaining, value, pointer);
    }
    Err(ConversionError::InvalidProjectionRecoveryPath(
        pointer.to_owned(),
    ))
}

fn normalize(value: Value, field_name: Option<&str>) -> Value {
    match value {
        Value::Object(object) => {
            if object.is_empty() {
                return Value::Null;
            }
            let mut entries: Vec<(String, Value)> = object.into_iter().collect();
            entries.sort_by(|left, right| left.0.cmp(&right.0));
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, child)| {
                        let child = normalize(child, Some(&key));
                        (key, child)
                    })
                    .collect(),
            )
        }
        Value::Array(items) => {
            let mut normalized: Vec<Value> = items
                .into_iter()
                .map(|item| normalize(item, field_name))
                .collect();
            if normalized.len() == 1 {
                normalized.remove(0)
            } else {
                Value::Array(normalized)
            }
        }
        Value::Bool(value) => Value::String(if value { "true" } else { "false" }.to_owned()),
        Value::Number(value) => Value::String(value.to_string()),
        Value::String(value) if field_name == Some("@uri") => {
            value.strip_suffix(".xml").map_or_else(
                || Value::String(value.clone()),
                |prefix| Value::String(format!("{prefix}.json")),
            )
        }
        other => other,
    }
}

fn is_empty_scalar(value: &Value) -> bool {
    value.is_null() || value.as_str().is_some_and(|text| text.trim().is_empty())
}

fn project_xml_text(text: &str) -> String {
    text.replace("\r\n", "\n")
        .replace('\r', "\n")
        .trim()
        .to_owned()
}

fn is_same_or_ancestor(ancestor: &str, path: &str) -> bool {
    ancestor == path
        || (path.starts_with(ancestor)
            && path
                .as_bytes()
                .get(ancestor.len())
                .is_some_and(|byte| *byte == b'/'))
}

fn join_pointer(base: &str, segment: &str) -> String {
    format!("{base}/{segment}")
}

fn escape_pointer(value: &str) -> String {
    value.replace('~', "~0").replace('/', "~1")
}

fn hex_digest(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().fold(
        String::with_capacity(bytes.as_ref().len() * 2),
        |mut output, byte| {
            write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        },
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn duplicate_languages_are_projected_and_exact_fragments_are_recoverable() {
        let source = json!({
            "processDataSet": {
                "generalComment": [
                    {"@xml:lang": "en", "#text": "part 1"},
                    {"@xml:lang": "en", "#text": "part 2"},
                    {"@xml:lang": "zh", "#text": "部分"}
                ]
            }
        });
        let projection = project_tidas_to_eilcd(&source, "processes").unwrap();
        assert_eq!(
            projection
                .document
                .pointer("/processDataSet/generalComment/0/#text"),
            Some(&Value::String("part 1\n\npart 2".to_owned()))
        );
        let mut restored = projection.document;
        restore_tidas_projection(&mut restored, projection.recovery.as_ref().unwrap()).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn unsupported_process_fields_are_recovered_after_projection() {
        let source = json!({
            "processDataSet": {
                "exchanges": {"exchange": {"quantitativeReference": true}},
                "LCIAResult": {"key": "method"}
            }
        });
        let projection = project_tidas_to_eilcd(&source, "processes").unwrap();
        assert!(
            projection
                .document
                .pointer("/processDataSet/exchanges/exchange/quantitativeReference")
                .is_none()
        );
        let mut restored = projection.document;
        restore_tidas_projection(&mut restored, projection.recovery.as_ref().unwrap()).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn process_accounting_reference_survives_eilcd_projection() {
        let reference = json!({
            "@type": "Other parameter",
            "functionalUnitOrOther": [
                {"@xml:lang": "en", "#text": "1 tonne unwashed raw coal"},
                {"@xml:lang": "zh", "#text": "1 吨未经洗选的原煤"}
            ]
        });
        let source = json!({
            "processDataSet": {
                "processInformation": {"quantitativeReference": reference},
                "exchanges": {"exchange": {"quantitativeReference": true}}
            }
        });
        let projection = project_tidas_to_eilcd(&source, "processes").unwrap();
        assert_eq!(
            projection
                .document
                .pointer("/processDataSet/processInformation/quantitativeReference"),
            source.pointer("/processDataSet/processInformation/quantitativeReference"),
        );
        assert!(
            projection
                .document
                .pointer("/processDataSet/exchanges/exchange/quantitativeReference")
                .is_none()
        );
        let mut restored = projection.document;
        restore_tidas_projection(&mut restored, projection.recovery.as_ref().unwrap()).unwrap();
        assert_eq!(restored, source);
    }

    #[test]
    fn target_specific_field_adaptations_are_recoverable() {
        let cases = [
            (
                "processes",
                json!({
                    "processDataSet": {
                        "processInformation": {"time": {
                            "timeRepresentativenessDescription": {"#text": "reference year"}
                        }},
                        "reference": {"@uri": "../flows/example.json"}
                    }
                }),
                "/processDataSet/processInformation/time/common:timeRepresentativenessDescription",
            ),
            (
                "flows",
                json!({
                    "flowDataSet": {
                        "flowInformation": {"dataSetInformation": {
                            "common:shortName": {"#text": "extension"}
                        }},
                        "modellingAndValidation": {"validation": {"review": {}}}
                    }
                }),
                "/flowDataSet/modellingAndValidation/validation",
            ),
            (
                "lifecyclemodels",
                json!({
                    "lifeCycleModelDataSet": {"administrativeInformation": {"dataEntryBy": {
                        "common:referenceToConvertedOriginalDataSetFrom": {"@refObjectId": "source"},
                        "common:referenceToDataSetUseApproval": {"@refObjectId": "approval"}
                    }}}
                }),
                "/lifeCycleModelDataSet/administrativeInformation/dataEntryBy/common:referenceToDataSetUseApproval",
            ),
        ];
        for (category, source, adapted_pointer) in cases {
            let projection = project_tidas_to_eilcd(&source, category).unwrap();
            if category == "processes" {
                assert!(projection.document.pointer(adapted_pointer).is_some());
            } else {
                assert!(projection.document.pointer(adapted_pointer).is_none());
            }
            let mut restored = projection.document;
            restore_tidas_projection(&mut restored, projection.recovery.as_ref().unwrap()).unwrap();
            assert_eq!(restored, source);
        }
    }
}
