use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;
use tidas_conversion::{
    ConversionDirection, ConversionRequest, EilcdProjectionRecoveryV1, convert_directory,
    project_tidas_to_eilcd, restore_tidas_projection,
};
use tidas_runtime::{CancellationToken, MemoryBudget};
use tidas_validation::{ValidationRequest, validate_ilcd_package, validate_tidas_package};

const MODEL: &[u8] = include_bytes!("fixtures/lifecycle-comments-v1/synthetic-model.json");
const COMMENT: &str =
    "/lifeCycleModelDataSet/lifeCycleModelInformation/dataSetInformation/common:generalComment";

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

fn reverse_keys(value: Value) -> Value {
    match value {
        Value::Object(object) => Value::Object(
            object
                .into_iter()
                .rev()
                .map(|(key, child)| (key, reverse_keys(child)))
                .collect(),
        ),
        Value::Array(items) => Value::Array(items.into_iter().map(reverse_keys).collect()),
        other => other,
    }
}

fn assert_projection(source: &Value, comments: &Value, mixed: bool) {
    let projected = project_tidas_to_eilcd(source, "lifecyclemodels").unwrap();
    if mixed {
        let comment = projected.document.pointer(COMMENT).unwrap();
        assert_eq!(comment["@xml:lang"], "mul");
        let text = comment["#text"].as_str().unwrap();
        for fragment in comments.as_array().unwrap() {
            assert!(text.contains(fragment["#text"].as_str().unwrap()));
            assert!(text.contains(&format!("[{}]", fragment["@xml:lang"].as_str().unwrap())));
        }
    }
    let recovery = projected.recovery.as_ref();
    if let Some(recovery) = recovery {
        let mut restored = projected.document.clone();
        restore_tidas_projection(&mut restored, recovery).unwrap();
        assert_eq!(&restored, source);
    }
}

#[test]
fn model_comment_languages_fit_official_xsd_and_recover_without_loss() {
    let en = json!({"@xml:lang": "en", "#text": "Original English note & <scope>."});
    let generated = json!({"@xml:lang": "en", "#text": "Generated English note."});
    let zh = json!({"@xml:lang": "zh", "#text": "原始中文说明。"});
    let pt = json!({"@xml:lang": "pt", "#text": "Descrição original."});
    let cases = [
        (en.clone(), false),
        (json!([en]), false),
        (json!([en, generated]), false),
        (json!([en, zh, generated]), true),
        (json!([en, pt, zh]), true),
    ];
    for (comments, mixed) in cases {
        for reversed in [false, true] {
            let directory = tempdir().unwrap();
            let input = directory.path().join("input");
            fs::create_dir_all(input.join("lifecyclemodels")).unwrap();
            let mut source: Value = serde_json::from_slice(MODEL).unwrap();
            *source.pointer_mut(COMMENT).unwrap() = comments.clone();
            if reversed {
                source = reverse_keys(source);
            }
            let source_bytes = serde_json::to_vec(&source).unwrap();
            let source_path = input.join("lifecyclemodels/synthetic.json");
            fs::write(&source_path, &source_bytes).unwrap();
            let native_issues = directory.path().join("native-issues.jsonl");
            assert!(
                validate_tidas_package(&validation(&input, &native_issues))
                    .unwrap()
                    .summary
                    .ok
            );

            assert_projection(&source, &comments, mixed);

            let ilcd = directory.path().join("ilcd");
            let repeat = directory.path().join("repeat");
            let forward =
                convert_directory(&conversion(&input, &ilcd, ConversionDirection::TidasToIlcd))
                    .unwrap();
            let again = convert_directory(&conversion(
                &input,
                &repeat,
                ConversionDirection::TidasToIlcd,
            ))
            .unwrap();
            assert_eq!(forward.output_tree_sha256, again.output_tree_sha256);
            let xml = fs::read_to_string(ilcd.join("data/lifecyclemodels/synthetic.xml")).unwrap();
            assert_eq!(xml.matches("<common:generalComment ").count(), 1);
            assert_eq!(xml.contains("xml:lang=\"mul\""), mixed);
            let xml_issues = directory.path().join("xml-issues.jsonl");
            assert!(
                validate_ilcd_package(&validation(&ilcd.join("data"), &xml_issues))
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
                &fs::read(restored.join("data/lifecyclemodels/synthetic.json")).unwrap(),
            )
            .unwrap();
            // The recovery contract checks the complete source semantic hash;
            // require exact shape, fragments, order and languages for this field.
            let expected_comment = match &comments {
                Value::Array(items) if items.len() == 1 => &items[0],
                other => other,
            };
            assert_eq!(result.pointer(COMMENT), Some(expected_comment));
            assert_eq!(fs::read(&source_path).unwrap(), source_bytes);
            if mixed {
                let receipt: EilcdProjectionRecoveryV1 = serde_json::from_slice(
                    &fs::read(ilcd.join("data/lifecyclemodels/synthetic.tidas-recovery.json"))
                        .unwrap(),
                )
                .unwrap();
                assert_eq!(
                    receipt.adaptations["project-singleton-lifecycle-model-comment"],
                    1
                );
                assert_eq!(
                    receipt
                        .restorations
                        .iter()
                        .find(|entry| entry.path == COMMENT)
                        .unwrap()
                        .original,
                    comments
                );
            }
        }
    }
}

#[test]
fn singleton_projection_is_scoped_and_does_not_hide_invalid_comment_fragments() {
    let comments = json!([
        {"@xml:lang": "en", "#text": "English"},
        {"@xml:lang": "zh", "#text": "中文"}
    ]);
    let process = json!({"processDataSet": {"processInformation": {"dataSetInformation": {
        "common:generalComment": comments
    }}}});
    let projected = project_tidas_to_eilcd(&process, "processes").unwrap();
    assert_eq!(projected.document, process);
    assert!(projected.recovery.is_none());

    for malformed in [
        json!({"@xml:lang": "zh", "#text": 123}),
        json!({"@xml:lang": "zh", "#text": "中文", "unexpected": "keep"}),
    ] {
        let mut source: Value = serde_json::from_slice(MODEL).unwrap();
        *source.pointer_mut(COMMENT).unwrap() = json!([comments[0], malformed]);
        let projected = project_tidas_to_eilcd(&source, "lifecyclemodels").unwrap();
        assert_eq!(projected.document.pointer(COMMENT), source.pointer(COMMENT));
    }
}
