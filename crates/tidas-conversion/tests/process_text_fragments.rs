use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;
use tidas_conversion::{
    ConversionDirection, ConversionError, ConversionRequest, convert_directory,
    project_tidas_to_eilcd,
};
use tidas_runtime::{CancellationToken, MemoryBudget};
use tidas_validation::{ValidationRequest, validate_ilcd_package, validate_tidas_package};

const PROCESS: &[u8] = include_bytes!("fixtures/process-types-v1/synthetic-process.json");
const FIELD: &str =
    "/processDataSet/processInformation/quantitativeReference/functionalUnitOrOther";

fn conversion(input: &Path, output: &Path, direction: ConversionDirection) -> ConversionRequest {
    ConversionRequest {
        input_dir: input.to_path_buf(),
        output_dir: output.to_path_buf(),
        direction,
        cancellation: CancellationToken::default(),
        memory_budget: MemoryBudget::new(32 * 1024 * 1024),
        queue_capacity: 2,
        progress: None,
    }
}

fn validation(input: &Path, issues: &Path) -> ValidationRequest {
    ValidationRequest {
        input_dir: input.to_path_buf(),
        issue_spool: Some(issues.to_path_buf()),
        cancellation: CancellationToken::default(),
        memory_budget: MemoryBudget::new(32 * 1024 * 1024),
        queue_capacity: 2,
        progress: None,
    }
}

fn text(language: &str, value: &str) -> Value {
    json!({"@xml:lang": language, "#text": value})
}

#[test]
fn repeatable_process_text_preserves_fragments_through_xsd_and_roundtrip() {
    let first = text("en", &"a".repeat(495));
    let last = text("en", &"b".repeat(34));
    let chinese = text("zh", "合成说明 & <边界>");
    let cases = [
        json!([first, last]),
        json!([first, chinese, last]),
        json!([text("en", "Short first"), text("en", "Short second")]),
        json!([text("en", &"😀".repeat(500)), text("en", "After boundary")]),
        chinese.clone(),
        json!([chinese]),
        json!([]),
    ];
    for fragments in cases {
        let directory = tempdir().unwrap();
        let input = directory.path().join("input");
        fs::create_dir_all(input.join("processes")).unwrap();
        let mut source: Value = serde_json::from_slice(PROCESS).unwrap();
        *source.pointer_mut(FIELD).unwrap() = fragments.clone();
        let source_bytes = serde_json::to_vec(&source).unwrap();
        fs::write(input.join("processes/synthetic.json"), &source_bytes).unwrap();
        let empty = fragments.as_array().is_some_and(Vec::is_empty);
        // An empty list is representable, but Other parameter still requires a meaningful basis.
        assert_eq!(
            validate_tidas_package(&validation(&input, &directory.path().join("native.jsonl")))
                .unwrap()
                .summary
                .ok,
            !empty
        );
        let projection = project_tidas_to_eilcd(&source, "processes").unwrap();
        assert_eq!(
            projection.document.pointer(FIELD),
            (!empty).then_some(&fragments)
        );
        // An unchanged field must not gain a restoration that can mask an XML edit.
        assert!(
            empty
                || projection
                    .recovery
                    .as_ref()
                    .unwrap()
                    .restorations
                    .iter()
                    .all(|entry| entry.path != FIELD
                        && !FIELD.starts_with(&format!("{}/", entry.path)))
        );
        let ilcd = directory.path().join("ilcd");
        let forward =
            convert_directory(&conversion(&input, &ilcd, ConversionDirection::TidasToIlcd))
                .unwrap();
        let repeated = convert_directory(&conversion(
            &input,
            &directory.path().join("repeat"),
            ConversionDirection::TidasToIlcd,
        ))
        .unwrap();
        assert_eq!(forward.output_tree_sha256, repeated.output_tree_sha256);
        let xml_path = ilcd.join("data/processes/synthetic.xml");
        let xml = fs::read_to_string(&xml_path).unwrap();
        let expected_count = fragments.as_array().map_or(1, Vec::len);
        assert_eq!(
            xml.matches("<functionalUnitOrOther ").count(),
            expected_count
        );
        assert!(
            validate_ilcd_package(&validation(
                &ilcd.join("data"),
                &directory.path().join("xsd.jsonl"),
            ))
            .unwrap()
            .summary
            .ok
        );
        let restored = directory.path().join("restored");
        convert_directory(&conversion(
            &ilcd.join("data"),
            &restored,
            ConversionDirection::IlcdToTidas,
        ))
        .unwrap();
        let result: Value = serde_json::from_slice(
            &fs::read(restored.join("data/processes/synthetic.json")).unwrap(),
        )
        .unwrap();
        // Existing normalization collapses singletons; recovery restores omitted empty arrays.
        let expected = match fragments.as_array() {
            Some(items) if items.len() == 1 => Some(&items[0]),
            _ => Some(&fragments),
        };
        assert_eq!(result.pointer(FIELD), expected);
        assert_eq!(
            fs::read(input.join("processes/synthetic.json")).unwrap(),
            source_bytes
        );
        assert_xml_edit_rejected(&ilcd, directory.path(), &xml);
    }
}

fn assert_xml_edit_rejected(ilcd: &Path, directory: &Path, xml: &str) {
    if !xml.contains(&"a".repeat(495)) {
        return;
    }
    let xml_path = ilcd.join("data/processes/synthetic.xml");
    fs::write(
        xml_path,
        xml.replace(&"a".repeat(495), "Edited synthetic text"),
    )
    .unwrap();
    let tampered_output = directory.join("tampered");
    let error = convert_directory(&conversion(
        &ilcd.join("data"),
        &tampered_output,
        ConversionDirection::IlcdToTidas,
    ))
    .unwrap_err();
    assert!(matches!(
        error,
        ConversionError::ProjectionRecoveryFailed { .. }
    ));
    assert!(!tampered_output.exists());
}

#[test]
fn oversized_single_fragments_remain_invalid_and_other_fields_still_merge() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("input");
    fs::create_dir_all(input.join("processes")).unwrap();
    let mut source: Value = serde_json::from_slice(PROCESS).unwrap();
    *source.pointer_mut(FIELD).unwrap() =
        json!([text("en", &"界".repeat(501)), text("en", "second")]);
    let comment_path =
        "/processDataSet/processInformation/dataSetInformation/common:generalComment";
    *source.pointer_mut(comment_path).unwrap() = json!([text("en", "first"), text("en", "second")]);
    let projection = project_tidas_to_eilcd(&source, "processes").unwrap();
    assert_eq!(projection.document.pointer(FIELD), source.pointer(FIELD));
    assert_eq!(
        projection.document.pointer(comment_path).unwrap()[0]["#text"],
        "first\n\nsecond"
    );
    fs::write(
        input.join("processes/synthetic.json"),
        serde_json::to_vec(&source).unwrap(),
    )
    .unwrap();
    assert!(
        !validate_tidas_package(&validation(&input, &directory.path().join("native.jsonl")))
            .unwrap()
            .summary
            .ok
    );
    let ilcd = directory.path().join("ilcd");
    convert_directory(&conversion(&input, &ilcd, ConversionDirection::TidasToIlcd)).unwrap();
    let issues = directory.path().join("xsd.jsonl");
    assert!(
        !validate_ilcd_package(&validation(&ilcd.join("data"), &issues))
            .unwrap()
            .summary
            .ok
    );
    assert!(fs::read_to_string(issues).unwrap().contains("maxLength"));

    // Preserve unsupported per-fragment metadata for normal validation, never erase it by merging.
    *source.pointer_mut(FIELD).unwrap() = json!([
        {"@xml:lang":"en", "#text":"first", "@unexpected":"retained"},
        {"@xml:lang":"en", "#text":"second", "@unexpected":"also retained"}
    ]);
    assert_eq!(
        project_tidas_to_eilcd(&source, "processes")
            .unwrap()
            .document
            .pointer(FIELD),
        source.pointer(FIELD)
    );
}
