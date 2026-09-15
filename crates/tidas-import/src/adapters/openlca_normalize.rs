use std::collections::{BTreeMap, BTreeSet};

use bigdecimal::BigDecimal;
use serde_json::{Map, Value, json};
use tidas_measurement::{Quantity, ScaleFactor, decimal_value, scale_quantity};

use super::{AdapterContext, AdapterError};
use crate::report::{ImportIssue, IssueSeverity, IssueSink};
use crate::store::CanonicalStore;

#[derive(Clone)]
struct UnitRecord {
    factor: BigDecimal,
    group_id: String,
    name: Option<String>,
}

#[derive(Clone)]
struct GroupReference {
    unit_id: Option<String>,
    unit_name: String,
}

#[derive(Clone)]
struct FlowRecord {
    factors: BTreeMap<String, BigDecimal>,
    reference_property_id: String,
    reference_property_name: Option<String>,
}

#[derive(Default)]
struct Indexes {
    units: BTreeMap<String, UnitRecord>,
    invalid_units: BTreeSet<String>,
    group_references: BTreeMap<String, GroupReference>,
    property_groups: BTreeMap<String, String>,
    group_properties: BTreeMap<String, Vec<String>>,
    flows: BTreeMap<String, FlowRecord>,
    invalid_flows: BTreeMap<String, &'static str>,
}

#[derive(Default)]
struct Stats {
    scanned: u64,
    normalized_same_property: u64,
    normalized_cross_property: u64,
    already_reference: u64,
    no_unit_info: u64,
    unresolved: BTreeMap<String, u64>,
    unresolved_samples: Vec<Value>,
}

pub(super) fn normalize_exchange_amounts(
    context: &AdapterContext<'_>,
    store: &CanonicalStore,
    issues: &mut dyn IssueSink,
) -> Result<(), AdapterError> {
    let indexes = build_indexes(store)?;
    let processes = store
        .iter_type("processes")?
        .collect::<Result<Vec<_>, _>>()?;
    let mut stats = Stats::default();
    for process in processes {
        let process_id = process.internal_id.clone();
        store.rewrite_process_exchanges::<AdapterError>(&process_id, |exchange| {
            context.cancellation.check()?;
            stats.scanned = stats.scanned.saturating_add(1);
            match normalize_exchange(exchange, &indexes) {
                Outcome::NoUnitInfo => {
                    stats.no_unit_info = stats.no_unit_info.saturating_add(1);
                }
                Outcome::AlreadyReference => {
                    stats.already_reference = stats.already_reference.saturating_add(1);
                }
                Outcome::Normalized {
                    cross_property: true,
                } => {
                    stats.normalized_cross_property =
                        stats.normalized_cross_property.saturating_add(1);
                }
                Outcome::Normalized {
                    cross_property: false,
                } => {
                    stats.normalized_same_property =
                        stats.normalized_same_property.saturating_add(1);
                }
                Outcome::Unresolved(reason) => {
                    *stats.unresolved.entry(reason.to_owned()).or_default() += 1;
                    if stats.unresolved_samples.len() < 20 {
                        stats.unresolved_samples.push(json!({
                            "process_id": process_id,
                            "internalId": exchange.get("internalId").cloned().unwrap_or(Value::Null),
                            "unitId": exchange.get("unitId").cloned().unwrap_or(Value::Null),
                            "unitName": exchange.get("unitName").cloned().unwrap_or(Value::Null),
                            "reason": reason,
                        }));
                    }
                }
            }
            Ok(())
        })?;
    }
    emit_issues(&stats, issues)?;
    Ok(())
}

fn emit_issues(stats: &Stats, issues: &mut dyn IssueSink) -> Result<(), AdapterError> {
    let normalized = stats
        .normalized_same_property
        .saturating_add(stats.normalized_cross_property);
    let unresolved_total = stats.unresolved.values().copied().sum::<u64>();
    if stats.scanned == 0 || (normalized == 0 && unresolved_total == 0) {
        return Ok(());
    }
    let context = BTreeMap::from([
        ("scanned".to_owned(), json!(stats.scanned)),
        (
            "normalized_same_property".to_owned(),
            json!(stats.normalized_same_property),
        ),
        (
            "normalized_cross_property".to_owned(),
            json!(stats.normalized_cross_property),
        ),
        (
            "already_reference".to_owned(),
            json!(stats.already_reference),
        ),
        ("no_unit_info".to_owned(), json!(stats.no_unit_info)),
        ("unresolved".to_owned(), json!(stats.unresolved)),
        (
            "unresolved_samples".to_owned(),
            Value::Array(stats.unresolved_samples.clone()),
        ),
    ]);
    issues.push(&ImportIssue {
        severity: IssueSeverity::Warning,
        code: "exchange_amounts_normalized_to_reference_units".to_owned(),
        message: "Normalized exchange amounts to each flow's reference flow-property unit."
            .to_owned(),
        source_object: None,
        context,
    })?;
    if unresolved_total > 0 {
        issues.push(&ImportIssue {
            severity: IssueSeverity::Error,
            code: "exchange_unit_normalization_unresolved".to_owned(),
            message: "Some exchange amounts could not be normalized; affected imports are blocked before publication."
                .to_owned(),
            source_object: None,
            context: BTreeMap::from([
                ("scanned".to_owned(), json!(stats.scanned)),
                ("unresolved_total".to_owned(), json!(unresolved_total)),
                ("unresolved".to_owned(), json!(stats.unresolved)),
                (
                    "unresolved_samples".to_owned(),
                    Value::Array(stats.unresolved_samples.clone()),
                ),
            ]),
        })?;
    }
    Ok(())
}

enum Outcome {
    NoUnitInfo,
    AlreadyReference,
    Normalized { cross_property: bool },
    Unresolved(&'static str),
}

fn normalize_exchange(exchange: &mut Map<String, Value>, indexes: &Indexes) -> Outcome {
    let Some(unit_id) = string(exchange.get("unitId")) else {
        return if has_measurement_selection(exchange) {
            Outcome::Unresolved("missing_exchange_unit_identity")
        } else {
            Outcome::NoUnitInfo
        };
    };
    if indexes.invalid_units.contains(unit_id) {
        return Outcome::Unresolved("duplicate_unit_identity");
    }
    let Some(unit) = indexes.units.get(unit_id) else {
        return Outcome::Unresolved("unknown_unit");
    };
    let Some((property_id, property_group)) = exchange_property(exchange, unit, indexes) else {
        return Outcome::Unresolved("unknown_flow_property");
    };
    if property_group != unit.group_id {
        return Outcome::Unresolved("unit_not_in_property_group");
    }
    let Some(flow_id) = string(exchange.get("flowRefId")) else {
        return Outcome::Unresolved("missing_flow_factors");
    };
    if let Some(reason) = indexes.invalid_flows.get(flow_id) {
        return Outcome::Unresolved(reason);
    }
    let Some(flow) = indexes.flows.get(flow_id) else {
        return Outcome::Unresolved("missing_flow_factors");
    };
    let Some(property_factor) = flow.factors.get(property_id) else {
        return Outcome::Unresolved("missing_flow_factors");
    };
    let Some(reference_factor) = flow.factors.get(&flow.reference_property_id) else {
        return Outcome::Unresolved("missing_flow_factors");
    };
    let cross_property = property_id != flow.reference_property_id;
    let Some(target_group_id) = indexes.property_groups.get(&flow.reference_property_id) else {
        return Outcome::Unresolved("unknown_flow_property");
    };
    let Some(target) = indexes.group_references.get(target_group_id) else {
        return Outcome::Unresolved("missing_reference_unit");
    };
    if target
        .unit_id
        .as_ref()
        .is_some_and(|id| indexes.invalid_units.contains(id))
    {
        return Outcome::Unresolved("duplicate_unit_identity");
    }
    let quantity = Quantity {
        amount: scalar_text(exchange.get("amount")),
        minimum_amount: exchange
            .get("minimumAmount")
            .map(|value| scalar_text(Some(value))),
        maximum_amount: exchange
            .get("maximumAmount")
            .map(|value| scalar_text(Some(value))),
    };
    let (converted, scale) = match scale_quantity(
        &quantity,
        &unit.factor,
        property_factor,
        reference_factor,
        false,
    ) {
        Ok(value) => value,
        Err(error) => return Outcome::Unresolved(error.code),
    };
    let exactly_one = scale.numerator == scale.denominator;
    if !exactly_one && has_absolute_normal_dispersion(exchange) {
        return Outcome::Unresolved("normal_uncertainty_requires_rescaling");
    }
    if exchange
        .get("amountFormula")
        .is_some_and(|value| !value.is_null() && value.as_str() != Some(""))
        && !exactly_one
    {
        return Outcome::Unresolved("formula_requires_rescaling");
    }
    if exactly_one
        && target.unit_id.as_deref() == Some(unit_id)
        && string(exchange.get("unitName")) == Some(target.unit_name.as_str())
        && string(exchange.get("flowPropertyRefId")) == Some(flow.reference_property_id.as_str())
        && !cross_property
    {
        return Outcome::AlreadyReference;
    }

    apply_normalized_exchange(
        exchange,
        target,
        flow,
        converted,
        &scale,
        unit,
        cross_property,
    )
}

fn has_measurement_selection(exchange: &Map<String, Value>) -> bool {
    // A property or unit label does not identify the source unit. Do not guess
    // from the Flow reference unit or silently accept a partial source reference.
    // Preserve the legacy path only when no measurement selection was supplied.
    [
        "unitId",
        "unitName",
        "flowPropertyRefId",
        "flowPropertyName",
    ]
    .iter()
    .any(|key| exchange.get(*key).is_some_and(|value| !value.is_null()))
        || exchange
            .get("sourceTrace")
            .and_then(|trace| trace.get("exchange"))
            .is_some_and(|source| {
                ["unit", "flowProperty"]
                    .iter()
                    .any(|key| source.get(*key).is_some_and(|value| !value.is_null()))
            })
}

fn has_absolute_normal_dispersion(exchange: &Map<String, Value>) -> bool {
    // openLCA normal sd is absolute despite the target field's relative name.
    // Its adapter may already have rounded or omitted sd*2; never silently carry
    // that value across a changed amount scale or infer it was absent at source.
    string(exchange.get("uncertaintyDistributionType")) == Some("normal")
        && (exchange.contains_key("relativeStandardDeviation95In")
            || exchange
                .get("sourceTrace")
                .and_then(|trace| trace.pointer("/exchange/uncertainty/sd"))
                .is_some_and(|value| !value.is_null()))
}

fn apply_normalized_exchange(
    exchange: &mut Map<String, Value>,
    target: &GroupReference,
    flow: &FlowRecord,
    converted: Quantity,
    scale: &ScaleFactor,
    unit: &UnitRecord,
    cross_property: bool,
) -> Outcome {
    preserve(exchange, "amount", "sourceAmount");
    preserve(exchange, "unitId", "sourceUnitId");
    preserve(exchange, "unitName", "sourceUnitName");
    preserve(exchange, "flowPropertyRefId", "sourceFlowPropertyRefId");
    preserve(exchange, "flowPropertyName", "sourceFlowPropertyName");
    exchange.insert("amount".to_owned(), Value::String(converted.amount));
    match &target.unit_id {
        Some(id) => {
            exchange.insert("unitId".to_owned(), Value::String(id.clone()));
        }
        None => {
            exchange.remove("unitId");
        }
    }
    exchange.insert(
        "unitName".to_owned(),
        Value::String(target.unit_name.clone()),
    );
    {
        exchange.insert(
            "flowPropertyRefId".to_owned(),
            Value::String(flow.reference_property_id.clone()),
        );
        match &flow.reference_property_name {
            Some(name) => {
                exchange.insert("flowPropertyName".to_owned(), Value::String(name.clone()));
            }
            None => {
                exchange.remove("flowPropertyName");
            }
        }
    }
    for (key, value) in [
        ("minimumAmount", converted.minimum_amount),
        ("maximumAmount", converted.maximum_amount),
    ] {
        if let Some(value) = value {
            exchange.insert(key.to_owned(), Value::String(value));
        }
    }
    exchange.insert(
        "amountNormalization".to_owned(),
        json!({
            "factor": scale.decimal,
            "numerator": scale.numerator,
            "denominator": scale.denominator,
            "precision": tidas_measurement::PRECISION,
            "rounding": "half-even",
            "sourceUnit": exchange.get("sourceUnitName").cloned().or_else(|| unit.name.clone().map(Value::String)).unwrap_or(Value::Null),
            "targetUnit": target.unit_name,
            "crossProperty": cross_property,
            "amountFormulaNotRescaled": false,
        }),
    );
    Outcome::Normalized { cross_property }
}

fn exchange_property<'a>(
    exchange: &'a Map<String, Value>,
    unit: &'a UnitRecord,
    indexes: &'a Indexes,
) -> Option<(&'a str, &'a str)> {
    if let Some(property_id) = string(exchange.get("flowPropertyRefId")) {
        let group = indexes.property_groups.get(property_id)?;
        return Some((property_id, group));
    }
    let candidates = indexes.group_properties.get(&unit.group_id)?;
    let flow = string(exchange.get("flowRefId")).and_then(|id| indexes.flows.get(id));
    let mut matching = candidates
        .iter()
        .filter(|candidate| flow.is_none_or(|flow| flow.factors.contains_key(*candidate)));
    let property = matching.next()?;
    matching
        .next()
        .is_none()
        .then_some((property.as_str(), unit.group_id.as_str()))
}

fn build_indexes(store: &CanonicalStore) -> Result<Indexes, AdapterError> {
    let mut indexes = Indexes::default();
    add_unit_indexes(store, &mut indexes)?;
    add_property_indexes(store, &mut indexes)?;
    add_flow_indexes(store, &mut indexes)?;
    Ok(indexes)
}

fn add_unit_indexes(store: &CanonicalStore, indexes: &mut Indexes) -> Result<(), AdapterError> {
    for group in store.iter_type("unitgroups")? {
        let group = group?;
        let Some(units) = group.raw.get("units").and_then(Value::as_array) else {
            continue;
        };
        let reference = select_reference_unit(units);
        if let Some(reference) = reference
            && let Some(name) = object_name(reference)
        {
            indexes.group_references.insert(
                group.internal_id.clone(),
                GroupReference {
                    unit_id: object_id(reference).map(ToOwned::to_owned),
                    unit_name: name.to_owned(),
                },
            );
        }
        for unit in units.iter().filter_map(Value::as_object) {
            let Some(id) = object_id(unit) else {
                continue;
            };
            let Some(factor) = decimal(unit.get("conversionFactor")) else {
                continue;
            };
            if indexes
                .units
                .insert(
                    id.to_owned(),
                    UnitRecord {
                        factor,
                        group_id: group.internal_id.clone(),
                        name: object_name(unit).map(ToOwned::to_owned),
                    },
                )
                .is_some()
            {
                indexes.invalid_units.insert(id.to_owned());
            }
        }
    }
    Ok(())
}

fn select_reference_unit(units: &[Value]) -> Option<&Map<String, Value>> {
    let mut declared = units.iter().filter_map(Value::as_object).filter(|unit| {
        unit.get("referenceUnit")
            .and_then(Value::as_bool)
            .unwrap_or(false)
    });
    if let Some(first) = declared.next() {
        return (declared.next().is_none()
            && decimal(first.get("conversionFactor")) == Some(BigDecimal::from(1)))
        .then_some(first);
    }
    let mut candidates = units
        .iter()
        .filter_map(Value::as_object)
        .filter(|unit| decimal(unit.get("conversionFactor")) == Some(BigDecimal::from(1)));
    let first = candidates.next()?;
    candidates.next().is_none().then_some(first)
}

fn add_property_indexes(store: &CanonicalStore, indexes: &mut Indexes) -> Result<(), AdapterError> {
    for property in store.iter_type("flowproperties")? {
        let property = property?;
        let Some(group_id) = string(property.raw.get("unitGroupRefId")) else {
            continue;
        };
        indexes
            .property_groups
            .insert(property.internal_id.clone(), group_id.to_owned());
        indexes
            .group_properties
            .entry(group_id.to_owned())
            .or_default()
            .push(property.internal_id);
    }
    Ok(())
}

fn add_flow_indexes(store: &CanonicalStore, indexes: &mut Indexes) -> Result<(), AdapterError> {
    for flow in store.iter_type("flows")? {
        let flow = flow?;
        let Some(entries) = flow.raw.get("flowProperties").and_then(Value::as_array) else {
            indexes
                .invalid_flows
                .insert(flow.internal_id, "missing_flow_properties");
            continue;
        };
        let mut factors = BTreeMap::new();
        let mut reference = None;
        let mut invalid = None;
        for entry in entries {
            let Some(property) = entry.get("flowProperty").and_then(Value::as_object) else {
                invalid = Some("missing_flow_property_identity");
                continue;
            };
            let Some(id) = object_id(property) else {
                invalid = Some("missing_flow_property_identity");
                continue;
            };
            let Some(factor) = decimal(entry.get("conversionFactor")) else {
                invalid = Some("missing_or_invalid_flow_factor");
                continue;
            };
            if factors.insert(id.to_owned(), factor.clone()).is_some() {
                invalid = Some("duplicate_flow_property");
            }
            if entry
                .get("isRefFlowProperty")
                .and_then(Value::as_bool)
                .unwrap_or(false)
            {
                if reference.is_some() {
                    invalid = Some("duplicate_reference_flow_property");
                }
                if factor != 1 {
                    invalid = Some("reference_property_not_one");
                }
                reference = Some((id.to_owned(), object_name(property).map(ToOwned::to_owned)));
            }
        }
        if let Some(reason) = invalid {
            indexes.invalid_flows.insert(flow.internal_id, reason);
            continue;
        }
        let Some((reference_property_id, reference_property_name)) = reference else {
            indexes
                .invalid_flows
                .insert(flow.internal_id, "missing_reference_flow_property");
            continue;
        };
        indexes.flows.insert(
            flow.internal_id,
            FlowRecord {
                factors,
                reference_property_id,
                reference_property_name,
            },
        );
    }
    Ok(())
}

fn preserve(exchange: &mut Map<String, Value>, source: &str, target: &str) {
    if let Some(value) = exchange.get(source).cloned() {
        exchange.insert(target.to_owned(), value);
    }
}

fn decimal(value: Option<&Value>) -> Option<BigDecimal> {
    value.and_then(|value| decimal_value(value).ok())
}

fn scalar_text(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(text)) => text.clone(),
        Some(Value::Number(number)) => number.to_string(),
        _ => String::new(),
    }
}

fn string(value: Option<&Value>) -> Option<&str> {
    value
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
}

fn object_id(object: &Map<String, Value>) -> Option<&str> {
    string(object.get("@id"))
}

fn object_name(object: &Map<String, Value>) -> Option<&str> {
    string(object.get("name"))
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use tidas_runtime::{CancellationToken, MemoryBudget};
    use tidas_validation::{ValidationRequest, validate_tidas_package};

    use super::*;
    use crate::model::CanonicalEntity;
    use crate::report::IssueSpool;
    use crate::writers::{TidasWriteRequest, write_tidas_package};

    const MASS_GROUP: &str = "11111111-1111-4111-8111-111111111111";
    const ENERGY_GROUP: &str = "22222222-2222-4222-8222-222222222222";
    const KG: &str = "33333333-3333-4333-8333-333333333333";
    const GRAM: &str = "44444444-4444-4444-8444-444444444444";
    const MJ: &str = "55555555-5555-4555-8555-555555555555";
    const UNKNOWN: &str = "66666666-6666-4666-8666-666666666666";
    const MASS_PROPERTY: &str = "77777777-7777-4777-8777-777777777777";
    const ENERGY_PROPERTY: &str = "88888888-8888-4888-8888-888888888888";
    const STEEL: &str = "99999999-9999-4999-8999-999999999999";
    const FUEL: &str = "aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaa";
    const PROCESS: &str = "bbbbbbbb-bbbb-4bbb-8bbb-bbbbbbbbbbbb";

    #[test]
    fn frozen_python_unit_normalization_semantics_are_streamed() {
        let store = normalization_fixture();
        let cancellation = CancellationToken::default();
        let memory_budget = MemoryBudget::new(4 * 1024 * 1024);
        let context = AdapterContext {
            source: Path::new("fixture"),
            cancellation: &cancellation,
            memory_budget: &memory_budget,
            max_entry_bytes: 1024,
        };
        let mut issues = IssueSpool::new(Vec::new(), 64 * 1024);
        normalize_exchange_amounts(&context, &store, &mut issues).unwrap();
        let (issue_bytes, summary) = issues.finish().unwrap();
        let values = store
            .iter_process_exchanges(PROCESS)
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_normalized_values(&values);
        assert_eq!(summary.warning_count, 1);
        assert_eq!(summary.error_count, 1);
        let issue_text = String::from_utf8(issue_bytes).unwrap();
        assert!(issue_text.contains("exchange_amounts_normalized_to_reference_units"));
        assert!(issue_text.contains("exchange_unit_normalization_unresolved"));

        let output = tempfile::tempdir().unwrap();
        write_tidas_package(&TidasWriteRequest {
            store: &store,
            output_dir: output.path(),
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        })
        .unwrap();
        let validation = validate_tidas_package(&ValidationRequest {
            input_dir: output.path().to_path_buf(),
            issue_spool: None,
            cancellation,
            memory_budget,
            queue_capacity: 2,
            progress: None,
        })
        .unwrap();
        assert!(validation.summary.ok);
    }

    #[test]
    fn selected_measurement_without_unit_identity_blocks_without_mutation() {
        let indexes = build_indexes(&normalization_fixture()).unwrap();
        for evidence in [
            json!({"flowPropertyRefId": ENERGY_PROPERTY}),
            json!({"flowPropertyName": "Energy"}),
            json!({"unitName": "MJ"}),
            json!({"unitId": ""}),
            json!({"sourceTrace": {"exchange": {"unit": {"name": "MJ"}}}}),
            json!({"sourceTrace": {"exchange": {"unit": {"@id": MJ}}}}),
            json!({"sourceTrace": {"exchange": {"unit": {}}}}),
            json!({"sourceTrace": {"exchange": {"flowProperty": {"@id": ENERGY_PROPERTY}}}}),
        ] {
            let mut value = Map::from_iter([
                ("internalId".to_owned(), json!(1)),
                ("flowRefId".to_owned(), json!(FUEL)),
                ("amount".to_owned(), json!("100")),
            ]);
            value.extend(evidence.as_object().unwrap().clone());
            let original = value.clone();
            assert!(
                matches!(
                    normalize_exchange(&mut value, &indexes),
                    Outcome::Unresolved("missing_exchange_unit_identity")
                ),
                "{evidence}"
            );
            assert_eq!(value, original);
        }
    }

    #[test]
    fn absence_of_measurement_metadata_keeps_existing_no_unit_behavior() {
        let indexes = build_indexes(&normalization_fixture()).unwrap();
        let mut value = serde_json::from_value::<Map<String, Value>>(json!({
            "flowRefId": FUEL,
            "amount": "100",
            "unitId": null,
            "unitName": null,
            "flowPropertyRefId": null,
            "flowPropertyName": null,
            "sourceTrace": {"format": "openlca-jsonld", "exchange": {
                "amount": "100", "unit": null, "flowProperty": null
            }}
        }))
        .unwrap();
        let original = value.clone();
        assert!(matches!(
            normalize_exchange(&mut value, &indexes),
            Outcome::NoUnitInfo
        ));
        assert_eq!(value, original);
    }

    #[test]
    fn factor_one_still_normalizes_property_and_unit_identity() {
        let store = normalization_fixture();
        let mut indexes = build_indexes(&store).unwrap();
        indexes
            .flows
            .get_mut(FUEL)
            .unwrap()
            .factors
            .insert(ENERGY_PROPERTY.to_owned(), BigDecimal::from(1));
        let mut value = exchange(1, FUEL, "2", MJ, "MJ", ENERGY_PROPERTY);
        assert!(matches!(
            normalize_exchange(&mut value, &indexes),
            Outcome::Normalized {
                cross_property: true
            }
        ));
        assert_eq!(value["amount"], "2");
        assert_eq!(value["unitId"], KG);
        assert_eq!(value["flowPropertyRefId"], MASS_PROPERTY);
        assert_eq!(value["amountNormalization"]["factor"], "1");
    }

    #[test]
    fn formulas_and_invalid_bounds_block_without_partial_mutation() {
        let indexes = build_indexes(&normalization_fixture()).unwrap();
        for (key, value, reason) in [
            ("amountFormula", json!("a*b"), "formula_requires_rescaling"),
            ("minimumAmount", json!("oops"), "invalid_decimal"),
        ] {
            let mut exchange = exchange(1, FUEL, "100", MJ, "MJ", ENERGY_PROPERTY);
            exchange.insert(key.to_owned(), value);
            let original = exchange.clone();
            assert!(
                matches!(normalize_exchange(&mut exchange, &indexes), Outcome::Unresolved(actual) if actual == reason)
            );
            assert_eq!(exchange, original);
        }
    }

    #[test]
    fn rounded_factor_one_does_not_hide_formula_rescaling() {
        let mut indexes = build_indexes(&normalization_fixture()).unwrap();
        indexes.flows.get_mut(FUEL).unwrap().factors.insert(
            ENERGY_PROPERTY.to_owned(),
            BigDecimal::from(1) + decimal(Some(&json!("1e-150"))).unwrap(),
        );
        let mut value = exchange(1, FUEL, "2", MJ, "MJ", ENERGY_PROPERTY);
        value.insert("amountFormula".to_owned(), json!("a*b"));
        let original = value.clone();
        assert!(matches!(
            normalize_exchange(&mut value, &indexes),
            Outcome::Unresolved("formula_requires_rescaling")
        ));
        assert_eq!(value, original);
    }

    #[test]
    fn absolute_normal_dispersion_blocks_rescaling_without_partial_mutation() {
        let indexes = build_indexes(&normalization_fixture()).unwrap();
        for dispersion in [
            json!({"relativeStandardDeviation95In":"6.000"}),
            json!({"sourceTrace":{"exchange":{"uncertainty":{"sd":"100"}}}}),
        ] {
            let mut value = exchange(1, STEEL, "500", GRAM, "g", MASS_PROPERTY);
            value.insert("uncertaintyDistributionType".to_owned(), json!("normal"));
            value.insert("minimumAmount".to_owned(), json!("400"));
            value.extend(dispersion.as_object().unwrap().clone());
            let original = value.clone();
            assert!(matches!(
                normalize_exchange(&mut value, &indexes),
                Outcome::Unresolved("normal_uncertainty_requires_rescaling")
            ));
            assert_eq!(value, original);
        }
    }

    #[test]
    fn normal_dispersion_survives_factor_one_identity_normalization() {
        let mut indexes = build_indexes(&normalization_fixture()).unwrap();
        indexes
            .flows
            .get_mut(FUEL)
            .unwrap()
            .factors
            .insert(ENERGY_PROPERTY.to_owned(), BigDecimal::from(1));
        let mut value = exchange(1, FUEL, "2", MJ, "MJ", ENERGY_PROPERTY);
        value.insert("uncertaintyDistributionType".to_owned(), json!("normal"));
        value.insert("relativeStandardDeviation95In".to_owned(), json!("0.600"));
        assert!(matches!(
            normalize_exchange(&mut value, &indexes),
            Outcome::Normalized {
                cross_property: true
            }
        ));
        assert_eq!(value["amount"], "2");
        assert_eq!(value["unitId"], KG);
        assert_eq!(value["flowPropertyRefId"], MASS_PROPERTY);
        assert_eq!(value["relativeStandardDeviation95In"], "0.600");
    }

    #[test]
    fn lognormal_dispersion_is_dimensionless_and_normal_bounds_remain_convertible() {
        let indexes = build_indexes(&normalization_fixture()).unwrap();
        for kind in ["log-normal", "normal"] {
            let mut value = exchange(1, STEEL, "500", GRAM, "g", MASS_PROPERTY);
            value.insert("uncertaintyDistributionType".to_owned(), json!(kind));
            value.insert("minimumAmount".to_owned(), json!("400"));
            value.insert("maximumAmount".to_owned(), json!("600"));
            if kind == "log-normal" {
                value.insert("relativeStandardDeviation95In".to_owned(), json!("1.440"));
            }
            assert!(matches!(
                normalize_exchange(&mut value, &indexes),
                Outcome::Normalized {
                    cross_property: false
                }
            ));
            assert_eq!(value["amount"], "0.5");
            assert_eq!(value["minimumAmount"], "0.4");
            assert_eq!(value["maximumAmount"], "0.6");
            if kind == "log-normal" {
                assert_eq!(value["relativeStandardDeviation95In"], "1.440");
            }
        }
    }

    #[test]
    fn duplicate_or_nonunit_reference_properties_do_not_get_silently_selected() {
        for (entries, expected) in [
            (
                vec![
                    (MASS_PROPERTY, "Mass", "1", true),
                    (MASS_PROPERTY, "Mass", "2", false),
                ],
                "duplicate_flow_property",
            ),
            (
                vec![(MASS_PROPERTY, "Mass", "2", true)],
                "reference_property_not_one",
            ),
            (
                vec![(MASS_PROPERTY, "Mass", "1", false)],
                "missing_reference_flow_property",
            ),
        ] {
            let mut store = CanonicalStore::create(None).unwrap();
            add_flow(&mut store, FUEL, entries);
            let indexes = build_indexes(&store).unwrap();
            assert_eq!(indexes.invalid_flows.get(FUEL), Some(&expected));
            assert!(!indexes.flows.contains_key(FUEL));
        }
    }

    #[test]
    fn ambiguous_unit_reference_and_duplicate_unit_id_are_blocked() {
        let duplicate_reference = json!([
            {"@id":KG,"name":"kg","conversionFactor":"1","referenceUnit":true},
            {"@id":GRAM,"name":"g","conversionFactor":"1","referenceUnit":true}
        ]);
        assert!(select_reference_unit(duplicate_reference.as_array().unwrap()).is_none());
        let mut store = CanonicalStore::create(None).unwrap();
        add_entity(
            &mut store,
            "unitgroups",
            MASS_GROUP,
            json!([
                {"@id":KG,"name":"kg","conversionFactor":"1","referenceUnit":true},
                {"@id":KG,"name":"kg-copy","conversionFactor":"1"}
            ]),
        );
        let indexes = build_indexes(&store).unwrap();
        assert!(indexes.invalid_units.contains(KG));
    }

    fn normalization_fixture() -> CanonicalStore {
        let mut store = CanonicalStore::create(None).unwrap();
        add_entity(
            &mut store,
            "unitgroups",
            MASS_GROUP,
            json!([
                {"@id": KG, "name": "kg", "conversionFactor": 1, "referenceUnit": true},
                {"@id": GRAM, "name": "g", "conversionFactor": 0.001}
            ]),
        );
        add_entity(
            &mut store,
            "unitgroups",
            ENERGY_GROUP,
            json!([
                {"@id": MJ, "name": "MJ", "conversionFactor": 1, "referenceUnit": true}
            ]),
        );
        add_property(&mut store, MASS_PROPERTY, MASS_GROUP, "Mass");
        add_property(&mut store, ENERGY_PROPERTY, ENERGY_GROUP, "Energy");
        add_flow(&mut store, STEEL, vec![(MASS_PROPERTY, "Mass", "1", true)]);
        add_flow(
            &mut store,
            FUEL,
            vec![
                (MASS_PROPERTY, "Mass", "1", true),
                (ENERGY_PROPERTY, "Energy", "50", false),
            ],
        );
        store
            .add(&CanonicalEntity {
                entity_type: "processes".to_owned(),
                internal_id: PROCESS.to_owned(),
                external_id: Some(PROCESS.to_owned()),
                name: Some("Steel production".to_owned()),
                category_path: Vec::new(),
                raw: Map::from_iter([(
                    "dataQualityIndicators".to_owned(),
                    json!([
                        {"@name": "Methodological appropriateness and consistency", "@value": "Good"},
                        {"@name": "Completeness", "@value": "Very good"}
                    ]),
                )]),
            })
            .unwrap();
        store.begin_process_exchanges(PROCESS).unwrap();
        for exchange in [
            exchange(1, STEEL, "1", KG, "kg", MASS_PROPERTY),
            exchange(2, STEEL, "500", GRAM, "g", MASS_PROPERTY)
                .into_iter()
                .chain([
                    ("minimumAmount".to_owned(), json!(400)),
                    ("maximumAmount".to_owned(), json!(600)),
                ])
                .collect(),
            exchange(3, FUEL, "100", MJ, "MJ", ENERGY_PROPERTY),
            exchange(4, STEEL, "7", UNKNOWN, "bogus", MASS_PROPERTY),
        ] {
            store.add_process_exchange(PROCESS, &exchange).unwrap();
        }
        store
    }

    fn assert_normalized_values(values: &[Map<String, Value>]) {
        assert_eq!(values[0]["amount"], "1");
        assert_eq!(values[1]["amount"], "0.5");
        assert_eq!(values[1]["minimumAmount"], "0.4");
        assert_eq!(values[1]["maximumAmount"], "0.6");
        assert_eq!(values[1]["sourceAmount"], "500");
        assert_eq!(values[1]["sourceUnitId"], GRAM);
        assert_eq!(values[1]["unitId"], KG);
        assert_eq!(values[1]["amountNormalization"]["factor"], "0.001");
        assert_eq!(values[1]["amountNormalization"]["crossProperty"], false);
        assert_eq!(values[2]["amount"], "2");
        assert_eq!(values[2]["sourceUnitId"], MJ);
        assert_eq!(values[2]["unitId"], KG);
        assert_eq!(values[2]["flowPropertyRefId"], MASS_PROPERTY);
        assert_eq!(values[2]["sourceFlowPropertyRefId"], ENERGY_PROPERTY);
        assert_eq!(values[2]["amountNormalization"]["factor"], "0.02");
        assert_eq!(values[2]["amountNormalization"]["crossProperty"], true);
        assert_eq!(values[3]["amount"], "7");
        assert!(values[3].get("amountNormalization").is_none());
    }

    fn add_entity(store: &mut CanonicalStore, kind: &str, id: &str, units: Value) {
        store
            .add(&CanonicalEntity {
                entity_type: kind.to_owned(),
                internal_id: id.to_owned(),
                external_id: Some(id.to_owned()),
                name: Some(kind.to_owned()),
                category_path: Vec::new(),
                raw: Map::from_iter([("units".to_owned(), units)]),
            })
            .unwrap();
    }

    fn add_property(store: &mut CanonicalStore, id: &str, group_id: &str, name: &str) {
        store
            .add(&CanonicalEntity {
                entity_type: "flowproperties".to_owned(),
                internal_id: id.to_owned(),
                external_id: Some(id.to_owned()),
                name: Some(name.to_owned()),
                category_path: Vec::new(),
                raw: Map::from_iter([(
                    "unitGroupRefId".to_owned(),
                    Value::String(group_id.to_owned()),
                )]),
            })
            .unwrap();
    }

    fn add_flow(store: &mut CanonicalStore, id: &str, properties: Vec<(&str, &str, &str, bool)>) {
        let properties = properties
            .into_iter()
            .map(|(id, name, factor, reference)| {
                json!({
                    "flowProperty": {"@id": id, "name": name},
                    "conversionFactor": factor,
                    "isRefFlowProperty": reference,
                })
            })
            .collect();
        store
            .add(&CanonicalEntity {
                entity_type: "flows".to_owned(),
                internal_id: id.to_owned(),
                external_id: Some(id.to_owned()),
                name: Some(id.to_owned()),
                category_path: Vec::new(),
                raw: Map::from_iter([
                    ("flowProperties".to_owned(), Value::Array(properties)),
                    (
                        "flowName".to_owned(),
                        json!({
                            "treatmentStandardsRoutes": "fixture route",
                            "mixAndLocationTypes": "GLO"
                        }),
                    ),
                ]),
            })
            .unwrap();
    }

    fn exchange(
        internal_id: u64,
        flow_id: &str,
        amount: &str,
        unit_id: &str,
        unit_name: &str,
        property_id: &str,
    ) -> Map<String, Value> {
        Map::from_iter([
            ("internalId".to_owned(), json!(internal_id)),
            ("flowRefId".to_owned(), Value::String(flow_id.to_owned())),
            ("amount".to_owned(), Value::String(amount.to_owned())),
            ("unitId".to_owned(), Value::String(unit_id.to_owned())),
            ("unitName".to_owned(), Value::String(unit_name.to_owned())),
            (
                "flowPropertyRefId".to_owned(),
                Value::String(property_id.to_owned()),
            ),
        ])
    }
}
