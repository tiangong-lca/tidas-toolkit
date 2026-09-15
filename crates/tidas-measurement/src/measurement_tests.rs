use super::*;
use serde_json::{Value, json};

fn request() -> Value {
    serde_json::from_str(include_str!(
        "../tests/fixtures/flow-property-conversion-request.json"
    ))
    .unwrap()
}
fn run(value: &Value) -> MeasurementReport {
    convert_measurement(value).unwrap()
}

#[test]
fn nonzero_reference_id_and_reordering_preserve_conversion_and_input() {
    let mut value = request();
    let original = value.clone();
    let result = run(&value);
    assert_eq!(result.result.amount, "2000");
    assert_eq!(result.result.minimum_amount.as_deref(), Some("1900"));
    assert_eq!(result.result.maximum_amount.as_deref(), Some("2100"));
    assert_eq!(result.reference.flow_property_internal_id, "7");
    assert_eq!(result.reference.unit_internal_id, "9");
    assert_eq!(result.round_trip.amount, "2");
    assert_eq!(result.round_trip.residual, "0");
    assert_eq!(value, original);
    value["flow"]["flowDataSet"]["flowProperties"]["flowProperty"]
        .as_array_mut()
        .unwrap()
        .reverse();
    let reordered = run(&value);
    assert_eq!(reordered.result.amount, "2000");
    assert_ne!(result.request_sha256, reordered.request_sha256);
}

#[test]
fn inverse_and_same_dimension_units_are_consistent() {
    let mut value = request();
    value["direction"] = json!("from-reference");
    value["source"]["amount"] = json!("2000");
    value["source"]["minimum_amount"] = Value::Null;
    value["source"]["maximum_amount"] = Value::Null;
    assert_eq!(run(&value).result.amount, "2");
    value["direction"] = json!("to-reference");
    value["source"]["unit_internal_id"] = json!("3");
    assert_eq!(run(&value).result.amount, "2000");
    assert_eq!(run(&value).factor.decimal, "1");
    assert_eq!(run(&value).reference.unit_name, "kg");
}

#[test]
fn negative_waste_quantities_keep_sign_and_bound_order() {
    let mut value = request();
    value["flow"]["flowDataSet"]["modellingAndValidation"]["LCIMethod"]["typeOfDataSet"] =
        json!("Waste flow");
    value["source"]["amount"] = json!("-2");
    value["source"]["minimum_amount"] = json!("-2.1");
    value["source"]["maximum_amount"] = json!("-1.9");
    let result = run(&value);
    assert_eq!(result.result.amount, "-2000");
    assert_eq!(result.result.minimum_amount.as_deref(), Some("-2100"));
    assert_eq!(result.result.maximum_amount.as_deref(), Some("-1900"));
}

#[test]
fn zero_descriptor_is_valid_but_not_convertible() {
    let mut value = request();
    value["flow"]["flowDataSet"]["flowProperties"]["flowProperty"][0]["meanValue"] = json!("0");
    assert!(inspect_flow_properties(&value["flow"]).is_ok());
    assert_eq!(
        convert_measurement(&value).unwrap_err().code,
        "non_positive_conversion_factor"
    );
    value["source"]["flow_property_internal_id"] = json!("7");
    assert_eq!(run(&value).result.amount, "2");
}

#[test]
fn invalid_conversion_factors_fail_before_arithmetic() {
    for factor in [
        "0",
        "-1",
        "NaN",
        "Infinity",
        "1e999999999999",
        "1e-513",
        "1e309",
        "1e-325",
    ] {
        let mut value = request();
        value["flow"]["flowDataSet"]["flowProperties"]["flowProperty"][0]["meanValue"] =
            json!(factor);
        assert!(convert_measurement(&value).is_err(), "{factor}");
    }
    assert!(parse_decimal(&"1".repeat(257)).is_err());
}

#[test]
fn missing_duplicates_and_nonunit_reference_are_explicit_errors() {
    for (pointer, new_value, code) in [
        (
            "/flow/flowDataSet/flowInformation/quantitativeReference/referenceToReferenceFlowProperty",
            json!("0"),
            "missing_reference_flow_property",
        ),
        (
            "/flow/flowDataSet/flowProperties/flowProperty/1/meanValue",
            json!("1.00000000000000000000000000000000001"),
            "reference_property_not_one",
        ),
        (
            "/flow/flowDataSet/flowProperties/flowProperty/0/@dataSetInternalID",
            json!("7"),
            "duplicate_flow_property_internal_id",
        ),
        (
            "/flow/flowDataSet/flowProperties/flowProperty/0/referenceToFlowPropertyDataSet/@version",
            json!("01.00.001"),
            "missing_exact_dependency",
        ),
    ] {
        let mut value = request();
        *value.pointer_mut(pointer).unwrap() = new_value;
        assert_eq!(convert_measurement(&value).unwrap_err().code, code);
    }
    let mut value = request();
    let duplicate = value["flow"]["flowDataSet"]["flowProperties"]["flowProperty"][0].clone();
    let entries = value["flow"]["flowDataSet"]["flowProperties"]["flowProperty"]
        .as_array_mut()
        .unwrap();
    entries.push(duplicate);
    entries[2]["@dataSetInternalID"] = json!("12");
    assert_eq!(
        convert_measurement(&value).unwrap_err().code,
        "duplicate_flow_property"
    );
}

#[test]
fn formulas_bad_bounds_missing_evidence_and_elementary_are_blocked() {
    for (key, field, expected) in [
        ("formula", json!("a * b"), "formula_requires_evaluation"),
        ("minimum_amount", json!("3"), "invalid_quantity_interval"),
        ("maximum_amount", json!("NaN"), "invalid_decimal"),
        ("amount", json!("1e-400"), "invalid_quantity_interval"),
    ] {
        let mut value = request();
        value["source"][key] = field;
        assert_eq!(convert_measurement(&value).unwrap_err().code, expected);
    }
    let mut value = request();
    value["evidence"] = json!([]);
    assert_eq!(
        convert_measurement(&value).unwrap_err().code,
        "missing_conversion_evidence"
    );
    let mut value = request();
    value["flow"]["flowDataSet"]["modellingAndValidation"]["LCIMethod"]["typeOfDataSet"] =
        json!("Elementary flow");
    assert_eq!(
        convert_measurement(&value).unwrap_err().code,
        "unsupported_flow_type"
    );
}

#[test]
fn decimal_core_is_deterministic_and_reports_rational_factor() {
    let q = Quantity {
        amount: "1".to_owned(),
        minimum_amount: None,
        maximum_amount: None,
    };
    let (result, factor) = scale_quantity(
        &q,
        &parse_decimal("1").unwrap(),
        &parse_decimal("3").unwrap(),
        &parse_decimal("1").unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(factor.numerator, "1");
    assert_eq!(factor.denominator, "3");
    assert_eq!(result.amount, format!("0.{}", "3".repeat(100)));
    let (again, _) = scale_quantity(
        &q,
        &parse_decimal("1").unwrap(),
        &parse_decimal("3").unwrap(),
        &parse_decimal("1").unwrap(),
        false,
    )
    .unwrap();
    assert_eq!(result.amount, again.amount);
}

#[test]
fn request_and_report_match_machine_contracts() {
    for (schema, value) in [
        (REQUEST_JSON_SCHEMA, request()),
        (
            REPORT_JSON_SCHEMA,
            serde_json::to_value(run(&request())).unwrap(),
        ),
    ] {
        let schema: Value = serde_json::from_str(schema).unwrap();
        let validator = jsonschema::draft202012::new(&schema).unwrap();
        let errors = validator
            .iter_errors(&value)
            .map(|e| e.to_string())
            .collect::<Vec<_>>();
        assert!(errors.is_empty(), "{errors:?}");
    }
}

#[test]
fn metadata_number_lexemes_have_one_exact_canonical_hash() {
    for (left, right) in [
        (r#"{"b":1.0,"a":1e-6}"#, r#"{"a":0.000001,"b":1}"#),
        (r#"{"x":1e20}"#, r#"{"x":100000000000000000000}"#),
        (r#"{"x":-0.0}"#, r#"{"x":0}"#),
    ] {
        let a: Value = serde_json::from_str(left).unwrap();
        let b: Value = serde_json::from_str(right).unwrap();
        assert_eq!(
            canonical_json_sha256(&a).unwrap(),
            canonical_json_sha256(&b).unwrap()
        );
        let mut ra = request();
        ra["metadata"] = a; // request unknown fields are rejected, so metadata belongs inside the Flow document.
        let a = ra.as_object_mut().unwrap().remove("metadata").unwrap();
        ra["flow"]["metadata"] = a;
        let mut rb = request();
        rb["flow"]["metadata"] = b;
        assert_eq!(run(&ra).request_sha256, run(&rb).request_sha256);
    }
    let huge: Value = serde_json::from_str(r#"{"x":1e9999999999}"#).unwrap();
    assert!(canonical_json_sha256(&huge).is_err());
}
