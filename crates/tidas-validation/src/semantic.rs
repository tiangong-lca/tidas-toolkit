use std::collections::{BTreeMap, BTreeSet};

use serde::Deserialize;
use serde_json::Value;
use thiserror::Error;
use tidas_assets::{AssetKind, bundled_asset};
use tidas_rulesets::{RulesetCatalog, RulesetError};

use crate::contracts::ValidationIssueV1;
use crate::schema::TidasCategory;

const DATA_TYPES_PATH: &str = "assets/tidas/schemas/tidas_data_types.json";
const PRODUCT_INDEX_PATH: &str = "assets/validation_indexes/product_flow_category_index.json";

#[derive(Debug)]
pub(crate) struct SemanticCatalog {
    languages: BTreeSet<String>,
    product_categories: BTreeMap<String, ProductCategory>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProductCategory {
    #[serde(rename = "allowedInProductFlowPath")]
    allowed_in_product_flow_path: bool,
    level: String,
    parent: Option<String>,
    text: String,
}

impl SemanticCatalog {
    pub(crate) fn load() -> Result<Self, SemanticError> {
        let data_types = asset_json(DATA_TYPES_PATH, AssetKind::JsonSchema)?;
        let languages = data_types
            .pointer("/$defs/Languages/enum")
            .and_then(Value::as_array)
            .ok_or(SemanticError::MissingLanguageEnum)?
            .iter()
            .map(|value| {
                value
                    .as_str()
                    .map(ToOwned::to_owned)
                    .ok_or(SemanticError::InvalidLanguageEnum)
            })
            .collect::<Result<BTreeSet<_>, _>>()?;
        let product_index = asset_json(PRODUCT_INDEX_PATH, AssetKind::ValidationIndex)?;
        let product_categories = serde_json::from_value(
            product_index
                .get("entries")
                .cloned()
                .ok_or(SemanticError::MissingProductEntries)?,
        )?;
        RulesetCatalog::load()?;
        Ok(Self {
            languages,
            product_categories,
        })
    }

    pub(crate) fn validate(
        &self,
        instance: &Value,
        category: TidasCategory,
        file_path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        self.validate_localized(instance, category, file_path, "", emit)?;
        match category {
            TidasCategory::Flows => self.validate_flows(instance, file_path, emit),
            TidasCategory::Processes => {
                Self::validate_process_like(
                    instance,
                    file_path,
                    &[
                        "processDataSet",
                        "processInformation",
                        "dataSetInformation",
                        "classificationInformation",
                        "common:classification",
                        "common:class",
                    ],
                    category,
                    emit,
                )?;
                Self::validate_process_allocations(instance, file_path, emit)?;
                Self::validate_process_variable_references(instance, file_path, emit)
            }
            TidasCategory::Lifecyclemodels => Self::validate_process_like(
                instance,
                file_path,
                &[
                    "lifecycleModelDataSet",
                    "lifecycleModelInformation",
                    "dataSetInformation",
                    "classificationInformation",
                    "common:classification",
                    "common:class",
                ],
                category,
                emit,
            ),
            TidasCategory::Sources => Self::validate_sources(instance, file_path, emit),
            _ => Ok(()),
        }
    }

    fn validate_localized(
        &self,
        node: &Value,
        category: TidasCategory,
        file_path: &str,
        path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        match node {
            Value::Object(object) => {
                if let Some(language) = object.get("@xml:lang").and_then(Value::as_str) {
                    let location = if path.is_empty() { "<root>" } else { path };
                    if !self.languages.contains(language) {
                        emit(ValidationIssueV1::error(
                            "localized_text_language_not_in_tidas_enum",
                            category.as_str(),
                            file_path,
                            location,
                            format!(
                                "Localized text error at {location}: @xml:lang '{language}' is not a TIDAS Languages enumeration value"
                            ),
                        ))?;
                    }
                    if let Some(text) = object.get("#text").and_then(Value::as_str) {
                        let has_chinese = text.chars().any(is_chinese_character);
                        if (language == "zh" && !has_chinese) || (language == "en" && has_chinese) {
                            let constraint = if language == "zh" {
                                "must include at least one Chinese character"
                            } else {
                                "must not contain Chinese characters"
                            };
                            emit(ValidationIssueV1::error(
                                "localized_text_language_error",
                                category.as_str(),
                                file_path,
                                location,
                                format!(
                                    "Localized text error at {location}: @xml:lang '{language}' {constraint}"
                                ),
                            ))?;
                        }
                    }
                }
                for (key, child) in object {
                    let child_path = if path.is_empty() {
                        key.clone()
                    } else {
                        format!("{path}/{key}")
                    };
                    self.validate_localized(child, category, file_path, &child_path, emit)?;
                }
            }
            Value::Array(items) => {
                for (index, child) in items.iter().enumerate() {
                    let child_path = if path.is_empty() {
                        index.to_string()
                    } else {
                        format!("{path}/{index}")
                    };
                    self.validate_localized(child, category, file_path, &child_path, emit)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn validate_flows(
        &self,
        instance: &Value,
        file_path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        let Some(dataset_type) = value_at(
            instance,
            &[
                "flowDataSet",
                "modellingAndValidation",
                "LCIMethod",
                "typeOfDataSet",
            ],
        )
        .and_then(Value::as_str) else {
            return Ok(());
        };
        if matches!(dataset_type, "Product flow" | "Waste flow")
            && let Err(problem) = tidas_measurement::inspect_flow_properties(instance)
        {
            emit(ValidationIssueV1::error(
                problem.code,
                TidasCategory::Flows.as_str(),
                file_path,
                "flowDataSet/flowProperties",
                problem.message,
            ))?;
        }
        if dataset_type == "Elementary flow" {
            if let Some(items) = value_at(
                instance,
                &[
                    "flowDataSet",
                    "flowInformation",
                    "dataSetInformation",
                    "classificationInformation",
                    "common:elementaryFlowCategorization",
                    "common:category",
                ],
            )
            .and_then(Value::as_array)
            {
                validate_prefix_hierarchy(
                    items,
                    "@catId",
                    "Elementary flow",
                    TidasCategory::Flows,
                    file_path,
                    emit,
                )?;
            }
        } else if dataset_type == "Product flow" {
            let Some(classifications) = value_at(
                instance,
                &[
                    "flowDataSet",
                    "flowInformation",
                    "dataSetInformation",
                    "classificationInformation",
                    "common:classification",
                ],
            ) else {
                return Ok(());
            };
            let base = "flowDataSet/flowInformation/dataSetInformation/classificationInformation/common:classification";
            match classifications {
                Value::Object(classification) if is_controlled_classification(classification) => {
                    self.validate_product_classes(
                        classification.get("common:class"),
                        &format!("{base}/common:class"),
                        file_path,
                        emit,
                    )?;
                }
                Value::Array(items) => {
                    for (index, item) in items.iter().enumerate() {
                        if let Some(classification) = item.as_object()
                            && is_controlled_classification(classification)
                        {
                            self.validate_product_classes(
                                classification.get("common:class"),
                                &format!("{base}/{index}/common:class"),
                                file_path,
                                emit,
                            )?;
                        }
                    }
                }
                _ => {}
            }
        }
        Ok(())
    }

    fn validate_product_classes(
        &self,
        value: Option<&Value>,
        location: &str,
        file_path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        let values: Vec<&Value> = match value {
            Some(Value::Array(items)) => items.iter().collect(),
            Some(item @ Value::Object(_)) => vec![item],
            _ => Vec::new(),
        };
        for (index, item) in values.iter().enumerate() {
            self.validate_product_item(item, index, location, file_path, emit)?;
        }
        for index in 1..values.len() {
            let Some(parent_id) = values[index - 1].get("@classId").and_then(Value::as_str) else {
                continue;
            };
            let Some(child_id) = values[index].get("@classId").and_then(Value::as_str) else {
                continue;
            };
            let Some(child) = self.product_categories.get(child_id) else {
                continue;
            };
            if child.parent.as_deref() != Some(parent_id) {
                emit(product_issue(
                    "product_category_parent_mismatch",
                    file_path,
                    &format!("{location}/{index}/@classId"),
                    format!(
                        "Product flow category parent-chain error: @classId '{child_id}' expects parent '{}', got '{parent_id}'",
                        child.parent.as_deref().unwrap_or("None")
                    ),
                ))?;
            }
        }
        Ok(())
    }

    fn validate_product_item(
        &self,
        item: &Value,
        index: usize,
        location: &str,
        file_path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        let item_location = format!("{location}/{index}");
        let Some(item) = item.as_object() else {
            return emit(product_issue(
                "product_category_item_shape_error",
                file_path,
                &item_location,
                format!(
                    "Product flow category error: classification item at index {index} must be an object"
                ),
            ));
        };
        let class_id = item.get("@classId").and_then(Value::as_str);
        let raw_level = item.get("@level").and_then(Value::as_str);
        let text = item.get("#text").and_then(Value::as_str);
        let Some(level) = raw_level.and_then(|value| value.parse::<usize>().ok()) else {
            return emit(product_issue(
                "product_category_level_parse_error",
                file_path,
                &format!("{item_location}/@level"),
                format!(
                    "Product flow category error: missing or invalid '@level' at index {index}"
                ),
            ));
        };
        if level != index {
            emit(product_issue(
                "product_category_level_sequence_error",
                file_path,
                &format!("{item_location}/@level"),
                format!(
                    "Product flow category level sorting error: at index {index}, expected level {index}, got {level}"
                ),
            ))?;
        }
        let Some(entry) = class_id.and_then(|id| self.product_categories.get(id)) else {
            return emit(product_issue(
                "product_category_unknown_class_id",
                file_path,
                &format!("{item_location}/@classId"),
                format!(
                    "Product flow category error: unknown @classId '{}' at index {index}",
                    class_id.unwrap_or("None")
                ),
            ));
        };
        let class_id = class_id.expect("an entry was found for this class id");
        if !entry.allowed_in_product_flow_path {
            emit(product_issue(
                "product_category_disallowed_class_id",
                file_path,
                &format!("{item_location}/@classId"),
                format!(
                    "Product flow category error: @classId '{class_id}' is not allowed in flow classification paths"
                ),
            ))?;
        }
        if raw_level != Some(entry.level.as_str()) {
            emit(product_issue(
                "product_category_level_mismatch",
                file_path,
                &format!("{item_location}/@level"),
                format!(
                    "Product flow category error: @classId '{class_id}' expects @level '{}', got '{}'",
                    entry.level,
                    raw_level.unwrap_or("None")
                ),
            ))?;
        }
        if text != Some(entry.text.as_str()) {
            emit(product_issue(
                "product_category_text_mismatch",
                file_path,
                &format!("{item_location}/#text"),
                format!(
                    "Product flow category error: @classId '{class_id}' expects #text '{}', got '{}'",
                    entry.text,
                    text.unwrap_or("None")
                ),
            ))?;
        }
        Ok(())
    }

    fn validate_process_like(
        instance: &Value,
        file_path: &str,
        path: &[&str],
        category: TidasCategory,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        let Some(items) = value_at(instance, path).and_then(Value::as_array) else {
            return Ok(());
        };
        for (index, item) in items.iter().enumerate() {
            let Some(level) = item
                .get("@level")
                .and_then(Value::as_str)
                .and_then(|value| value.parse::<usize>().ok())
            else {
                return Ok(());
            };
            if level != index {
                emit(hierarchy_issue(
                    category,
                    file_path,
                    format!(
                        "Processes classification level sorting error: at index {index}, expected level {index}, got {level}"
                    ),
                ))?;
            }
        }
        for index in 1..items.len() {
            let parent = &items[index - 1];
            let child = &items[index];
            let (Some(parent_id), Some(child_id), Some(parent_level), Some(child_level)) = (
                parent.get("@classId").and_then(Value::as_str),
                child.get("@classId").and_then(Value::as_str),
                parent
                    .get("@level")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<usize>().ok()),
                child
                    .get("@level")
                    .and_then(Value::as_str)
                    .and_then(|value| value.parse::<usize>().ok()),
            ) else {
                return Ok(());
            };
            let valid = if parent_level == 0 && child_level == 1 {
                valid_process_level_one(parent_id, child_id)
            } else {
                child_id.starts_with(parent_id)
            };
            if !valid {
                emit(hierarchy_issue(
                    category,
                    file_path,
                    format!(
                        "Processes classification code error: child code '{child_id}' does not correspond to parent code '{parent_id}'"
                    ),
                ))?;
            }
        }
        Ok(())
    }

    fn validate_sources(
        instance: &Value,
        file_path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        let Some(value) = value_at(
            instance,
            &[
                "sourceDataSet",
                "sourceInformation",
                "dataSetInformation",
                "classificationInformation",
                "common:classification",
                "common:class",
            ],
        ) else {
            return Ok(());
        };
        let values: Vec<&Value> = match value {
            Value::Array(items) => items.iter().collect(),
            Value::Object(_) => vec![value],
            _ => return Ok(()),
        };
        for (index, item) in values.iter().enumerate() {
            let Some(level) = item
                .get("@level")
                .and_then(Value::as_str)
                .and_then(|level| level.parse::<usize>().ok())
            else {
                emit(hierarchy_issue(
                    TidasCategory::Sources,
                    file_path,
                    format!(
                        "Sources classification level parsing error: missing or invalid '@level' at index {index}"
                    ),
                ))?;
                continue;
            };
            if level != index {
                emit(hierarchy_issue(
                    TidasCategory::Sources,
                    file_path,
                    format!(
                        "Sources classification level sorting error: at index {index}, expected level {index}, got {level}"
                    ),
                ))?;
            }
        }
        for index in 1..values.len() {
            let (Some(parent), Some(child)) = (
                values[index - 1].get("@classId").and_then(Value::as_str),
                values[index].get("@classId").and_then(Value::as_str),
            ) else {
                emit(hierarchy_issue(
                    TidasCategory::Sources,
                    file_path,
                    format!(
                        "Sources classification code error: missing '@classId' for parent index {} or child index {index}",
                        index - 1
                    ),
                ))?;
                continue;
            };
            if !child.starts_with(parent) {
                emit(hierarchy_issue(
                    TidasCategory::Sources,
                    file_path,
                    format!(
                        "Sources classification code error: child code '{child}' does not start with parent code '{parent}'"
                    ),
                ))?;
            }
        }
        Ok(())
    }

    fn validate_process_allocations(
        instance: &Value,
        file_path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        let Some(exchanges) = value_at(instance, &["processDataSet", "exchanges", "exchange"])
        else {
            return Ok(());
        };
        let exchanges: Vec<&Value> = match exchanges {
            Value::Array(items) => items.iter().collect(),
            Value::Object(_) => vec![exchanges],
            _ => return Ok(()),
        };
        let ids: BTreeSet<&str> = exchanges
            .iter()
            .filter_map(|exchange| exchange.get("@dataSetInternalID").and_then(Value::as_str))
            .collect();
        for (exchange_index, exchange) in exchanges.iter().enumerate() {
            let Some(allocations) = exchange
                .get("allocations")
                .and_then(|value| value.get("allocation"))
            else {
                continue;
            };
            let allocations: Vec<&Value> = match allocations {
                Value::Array(items) => items.iter().collect(),
                Value::Object(_) => vec![allocations],
                _ => continue,
            };
            for (allocation_index, allocation) in allocations.iter().enumerate() {
                let Some(reference) = allocation
                    .get("@internalReferenceToCoProduct")
                    .and_then(Value::as_str)
                else {
                    continue;
                };
                if !ids.contains(reference) {
                    emit(ValidationIssueV1::error(
                        "allocation_coproduct_reference_missing",
                        TidasCategory::Processes.as_str(),
                        file_path,
                        format!(
                            "processDataSet/exchanges/exchange/{exchange_index}/allocations/allocation/{allocation_index}/@internalReferenceToCoProduct"
                        ),
                        format!(
                            "Allocation references co-product exchange internal id '{reference}', but no exchange with that @dataSetInternalID exists"
                        ),
                    ))?;
                }
            }
        }
        Ok(())
    }

    fn validate_process_variable_references(
        instance: &Value,
        file_path: &str,
        emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
    ) -> Result<(), crate::ValidationError> {
        let declared: BTreeSet<&str> = value_at(
            instance,
            &[
                "processDataSet",
                "mathematicalRelations",
                "variableParameter",
            ],
        )
        .map(|variables| match variables {
            Value::Array(items) => items.iter().collect::<Vec<_>>(),
            Value::Object(_) => vec![variables],
            _ => Vec::new(),
        })
        .unwrap_or_default()
        .into_iter()
        .filter_map(|variable| variable.get("@name").and_then(Value::as_str))
        .collect();
        validate_variable_reference_nodes(instance, "", &declared, file_path, emit)
    }
}

fn validate_variable_reference_nodes(
    node: &Value,
    path: &str,
    declared: &BTreeSet<&str>,
    file_path: &str,
    emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
) -> Result<(), crate::ValidationError> {
    match node {
        Value::Object(object) => {
            for (key, child) in object {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}/{key}")
                };
                if key == "referenceToVariable"
                    && let Some(reference) = child.as_str()
                    && !declared.contains(reference)
                {
                    emit(ValidationIssueV1::error(
                        "variable_parameter_reference_missing",
                        TidasCategory::Processes.as_str(),
                        file_path,
                        &child_path,
                        format!(
                            "referenceToVariable names '{reference}', but no variableParameter with that @name exists"
                        ),
                    ))?;
                }
                validate_variable_reference_nodes(child, &child_path, declared, file_path, emit)?;
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                let child_path = format!("{path}/{index}");
                validate_variable_reference_nodes(child, &child_path, declared, file_path, emit)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn asset_json(path: &str, kind: AssetKind) -> Result<Value, SemanticError> {
    let asset = bundled_asset(path).ok_or_else(|| SemanticError::MissingAsset(path.to_owned()))?;
    if asset.kind != kind {
        return Err(SemanticError::UnexpectedAssetKind(path.to_owned()));
    }
    Ok(serde_json::from_slice(asset.bytes)?)
}

fn validate_prefix_hierarchy(
    items: &[Value],
    id_key: &str,
    label: &str,
    category: TidasCategory,
    file_path: &str,
    emit: &mut impl FnMut(ValidationIssueV1) -> Result<(), crate::ValidationError>,
) -> Result<(), crate::ValidationError> {
    for (index, item) in items.iter().enumerate() {
        let Some(level) = item
            .get("@level")
            .and_then(Value::as_str)
            .and_then(|value| value.parse::<usize>().ok())
        else {
            return Ok(());
        };
        if level != index {
            emit(hierarchy_issue(
                category,
                file_path,
                format!(
                    "{label} classification level sorting error: at index {index}, expected level {index}, got {level}"
                ),
            ))?;
        }
    }
    for index in 1..items.len() {
        let (Some(parent), Some(child)) = (
            items[index - 1].get(id_key).and_then(Value::as_str),
            items[index].get(id_key).and_then(Value::as_str),
        ) else {
            return Ok(());
        };
        if !child.starts_with(parent) {
            emit(hierarchy_issue(
                category,
                file_path,
                format!(
                    "{label} classification code error: child code '{child}' does not start with parent code '{parent}'"
                ),
            ))?;
        }
    }
    Ok(())
}

fn product_issue(
    code: &str,
    file_path: &str,
    location: &str,
    message: String,
) -> ValidationIssueV1 {
    ValidationIssueV1::error(code, "flows", file_path, location, message)
}

fn hierarchy_issue(category: TidasCategory, file_path: &str, message: String) -> ValidationIssueV1 {
    ValidationIssueV1::error(
        "classification_hierarchy_error",
        category.as_str(),
        file_path,
        "<root>",
        message,
    )
}

fn value_at<'a>(mut value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    for segment in path {
        value = value.get(*segment)?;
    }
    Some(value)
}

fn is_controlled_classification(object: &serde_json::Map<String, Value>) -> bool {
    let name = object
        .get("@name")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    let classes = object
        .get("@classes")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .trim()
        .to_ascii_lowercase();
    matches!(
        name.as_str(),
        "" | "cpc" | "ilcd" | "tidas product category" | "tidas product flow category"
    ) || name.starts_with("cpc")
        || name.starts_with("ilcd")
        || classes.contains("product_flow_category_index.json")
        || classes.contains("tidas_flows_product_category.json")
}

fn is_chinese_character(character: char) -> bool {
    matches!(
        character,
        '\u{3400}'..='\u{4DBF}' | '\u{4E00}'..='\u{9FFF}' | '\u{F900}'..='\u{FAFF}'
    )
}

fn valid_process_level_one(parent: &str, child: &str) -> bool {
    let Some(child) = child.parse::<u8>().ok() else {
        return false;
    };
    match parent {
        "A" => (1..=3).contains(&child),
        "B" => (5..=9).contains(&child),
        "C" => (10..=33).contains(&child),
        "D" => child == 35,
        "E" => (36..=39).contains(&child),
        "F" => (41..=43).contains(&child),
        "G" => (46..=47).contains(&child),
        "H" => (49..=53).contains(&child),
        "I" => (55..=56).contains(&child),
        "J" => (58..=60).contains(&child),
        "K" => (61..=63).contains(&child),
        "L" => (64..=66).contains(&child),
        "M" => child == 68,
        "N" => (69..=75).contains(&child),
        "O" => (77..=82).contains(&child),
        "P" => child == 84,
        "Q" => child == 85,
        "R" => (86..=88).contains(&child),
        "S" => (90..=93).contains(&child),
        "T" => (94..=96).contains(&child),
        "U" => (97..=98).contains(&child),
        "V" => child == 99,
        _ => false,
    }
}

#[derive(Debug, Error)]
pub enum SemanticError {
    #[error("required semantic validation asset is missing: {0}")]
    MissingAsset(String),
    #[error("semantic validation asset has an unexpected kind: {0}")]
    UnexpectedAssetKind(String),
    #[error("TIDAS language enumeration is missing")]
    MissingLanguageEnum,
    #[error("TIDAS language enumeration contains a non-string value")]
    InvalidLanguageEnum,
    #[error("product category index does not contain entries")]
    MissingProductEntries,
    #[error(transparent)]
    Ruleset(#[from] RulesetError),
    #[error("semantic validation asset is invalid JSON: {0}")]
    Json(#[from] serde_json::Error),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn locked_indexes_and_runtime_rulesets_load_as_one_catalog() {
        let catalog = SemanticCatalog::load().unwrap();
        assert!(catalog.languages.contains("zh"));
        assert_eq!(
            catalog.product_categories["46121"].text,
            "Electrical transformers"
        );
    }

    #[test]
    fn localized_and_product_classification_issues_match_python_codes() {
        let catalog = SemanticCatalog::load().unwrap();
        let mut instance = serde_json::json!({
            "flowDataSet": {
                "modellingAndValidation": {"LCIMethod": {"typeOfDataSet": "Product flow"}},
                "flowInformation": {"dataSetInformation": {
                    "name": {"baseName": {"@xml:lang": "en", "#text": "中文"}},
                    "classificationInformation": {"common:classification": [
                        {"@name": "HS", "common:class": [{"@level": "0", "@classId": "bad", "#text": "external"}]},
                        {"@name": "CPC", "common:class": [{"@level": "0", "@classId": "bad", "#text": "controlled"}]}
                    ]}
                }}
            }
        });
        instance["flowDataSet"]["flowInformation"]["quantitativeReference"] =
            serde_json::json!({"referenceToReferenceFlowProperty":"7"});
        instance["flowDataSet"]["flowProperties"] = serde_json::json!({"flowProperty":{"@dataSetInternalID":"7","referenceToFlowPropertyDataSet":{"@refObjectId":"20000000-0000-4000-8000-000000000001"},"meanValue":"1"}});
        let mut issues = Vec::new();
        catalog
            .validate(&instance, TidasCategory::Flows, "flow.json", &mut |issue| {
                issues.push(issue);
                Ok(())
            })
            .unwrap();
        assert_eq!(
            issues
                .iter()
                .map(|issue| issue.issue_code.as_str())
                .collect::<Vec<_>>(),
            [
                "localized_text_language_error",
                "product_category_unknown_class_id"
            ]
        );
        assert!(issues[1].location.contains("/1/common:class/0/@classId"));
    }

    #[test]
    fn product_flow_property_pointer_and_zero_descriptor_are_semantic_gates() {
        let catalog = SemanticCatalog::load().unwrap();
        let mut instance = serde_json::json!({"flowDataSet": {
            "modellingAndValidation": {"LCIMethod": {"typeOfDataSet":"Product flow"}},
            "flowInformation": {"quantitativeReference":{"referenceToReferenceFlowProperty":"7"}},
            "flowProperties":{"flowProperty":[
                {"@dataSetInternalID":"2","referenceToFlowPropertyDataSet":{"@refObjectId":"20000000-0000-4000-8000-000000000002"},"meanValue":"0"},
                {"@dataSetInternalID":"7","referenceToFlowPropertyDataSet":{"@refObjectId":"20000000-0000-4000-8000-000000000001"},"meanValue":"1"}
            ]}
        }});
        let mut issues = Vec::new();
        catalog
            .validate(&instance, TidasCategory::Flows, "flow.json", &mut |issue| {
                issues.push(issue);
                Ok(())
            })
            .unwrap();
        assert!(issues.is_empty());
        instance["flowDataSet"]["flowInformation"]["quantitativeReference"]["referenceToReferenceFlowProperty"] =
            serde_json::json!("0");
        catalog
            .validate(&instance, TidasCategory::Flows, "flow.json", &mut |issue| {
                issues.push(issue);
                Ok(())
            })
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].issue_code, "missing_reference_flow_property");
    }

    #[test]
    fn dangling_allocation_coproduct_is_a_tidas_semantic_error() {
        let catalog = SemanticCatalog::load().unwrap();
        let instance = serde_json::json!({
            "processDataSet": {"exchanges": {"exchange": {
                "@dataSetInternalID": "0",
                "allocations": {"allocation": {
                    "@allocatedFraction": "100",
                    "@internalReferenceToCoProduct": "1"
                }}
            }}}
        });
        let mut issues = Vec::new();
        catalog
            .validate(
                &instance,
                TidasCategory::Processes,
                "process.json",
                &mut |issue| {
                    issues.push(issue);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(
            issues[0].issue_code,
            "allocation_coproduct_reference_missing"
        );
        assert!(issues[0].location.contains("@internalReferenceToCoProduct"));
    }

    #[test]
    fn dangling_variable_reference_is_a_tidas_semantic_error() {
        let catalog = SemanticCatalog::load().unwrap();
        let instance = serde_json::json!({
            "processDataSet": {
                "exchanges": {"exchange": {
                    "@dataSetInternalID": "0",
                    "referenceToVariable": "missing-variable"
                }}
            }
        });
        let mut issues = Vec::new();
        catalog
            .validate(
                &instance,
                TidasCategory::Processes,
                "process.json",
                &mut |issue| {
                    issues.push(issue);
                    Ok(())
                },
            )
            .unwrap();
        assert_eq!(issues.len(), 1);
        assert_eq!(issues[0].issue_code, "variable_parameter_reference_missing");
    }
}
