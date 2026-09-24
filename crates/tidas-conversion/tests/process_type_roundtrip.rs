use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use tempfile::tempdir;
use tidas_conversion::{
    ConversionDirection, ConversionRequest, EilcdProjectionRecoveryV1, convert_directory,
};
use tidas_runtime::{CancellationToken, MemoryBudget};
use tidas_validation::{ValidationRequest, validate_ilcd_package, validate_tidas_package};

const PROCESS: &[u8] = include_bytes!("fixtures/process-types-v1/synthetic-process.json");
const PROCESS_PATH: &str = "processes/synthetic.json";
const YEAR_PATH: &str = "/processDataSet/processInformation/time/common:referenceYear";
const METHOD_PATH: &str = "/processDataSet/modellingAndValidation/LCIMethodAndAllocation";

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

fn assert_restored_process(restored: &Path, expected: &Value, issues: &Path) {
    let restored_process: Value =
        serde_json::from_slice(&fs::read(restored.join("data").join(PROCESS_PATH)).unwrap())
            .unwrap();
    assert_eq!(restored_process.pointer(YEAR_PATH), Some(&json!(2019)));
    assert_eq!(restored_process.pointer(METHOD_PATH), Some(&json!({})));
    assert_eq!(
        restored_process.pointer("/processDataSet/processInformation/quantitativeReference/@type"),
        Some(&json!("Other parameter"))
    );
    assert!(
        restored_process
            .pointer(
                "/processDataSet/processInformation/quantitativeReference/referenceToReferenceFlow"
            )
            .is_none()
    );
    assert!(
        restored_process
            .pointer("/processDataSet/modellingAndValidation/LCIMethodAndAllocation/typeOfDataSet")
            .is_none()
    );
    assert_eq!(&restored_process, expected);

    let result = validate_tidas_package(&validation(&restored.join("data"), issues)).unwrap();
    assert!(
        result.summary.ok,
        "reversed TIDAS must pass native schema validation: {}",
        fs::read_to_string(issues).unwrap()
    );
}

fn assert_legacy_reverse(
    ilcd: &Path,
    directory: &Path,
    recovery: &EilcdProjectionRecoveryV1,
    expected: &Value,
) {
    // A previously generated sidecar has no entries for these two JSON shapes.
    // Reverse conversion must still restore the schema-required types from XML.
    let mut legacy_recovery = recovery.clone();
    legacy_recovery
        .restorations
        .retain(|item| item.path != YEAR_PATH && item.path != METHOD_PATH);
    legacy_recovery
        .adaptations
        .remove("preserve-process-reference-year-type");
    legacy_recovery
        .adaptations
        .remove("preserve-empty-process-lci-method");
    let mut legacy_bytes = serde_json::to_vec_pretty(&legacy_recovery).unwrap();
    legacy_bytes.push(b'\n');
    fs::write(
        ilcd.join("data/processes/synthetic.tidas-recovery.json"),
        legacy_bytes,
    )
    .unwrap();
    let restored = directory.join("legacy-restored");
    convert_directory(&conversion(
        &ilcd.join("data"),
        &restored,
        ConversionDirection::IlcdToTidas,
    ))
    .unwrap();
    assert_restored_process(&restored, expected, &directory.join("legacy-issues.jsonl"));
}

#[test]
fn projected_process_recovers_integer_year_and_present_empty_method() {
    let directory = tempdir().unwrap();
    let input = directory.path().join("input");
    let process_dir = input.join("processes");
    fs::create_dir_all(&process_dir).unwrap();
    fs::write(process_dir.join("synthetic.json"), PROCESS).unwrap();
    let expected: Value = serde_json::from_slice(PROCESS).unwrap();
    assert_eq!(expected.pointer(YEAR_PATH), Some(&json!(2019)));
    assert_eq!(expected.pointer(METHOD_PATH), Some(&json!({})));

    let input_issues = directory.path().join("input-issues.jsonl");
    let input_validation = validate_tidas_package(&validation(&input, &input_issues)).unwrap();
    assert!(
        input_validation.summary.ok,
        "synthetic input must be schema-valid: {}",
        fs::read_to_string(&input_issues).unwrap()
    );

    let ilcd = directory.path().join("ilcd");
    let ilcd_repeat = directory.path().join("ilcd-repeat");
    let forward =
        convert_directory(&conversion(&input, &ilcd, ConversionDirection::TidasToIlcd)).unwrap();
    let repeated_forward = convert_directory(&conversion(
        &input,
        &ilcd_repeat,
        ConversionDirection::TidasToIlcd,
    ))
    .unwrap();
    assert_eq!(
        forward.output_tree_sha256,
        repeated_forward.output_tree_sha256
    );
    assert!(forward.peak_accounted_memory_bytes <= 32 * 1024 * 1024);

    let projected_xml = fs::read_to_string(ilcd.join("data/processes/synthetic.xml")).unwrap();
    assert!(projected_xml.contains("<common:referenceYear>2019</common:referenceYear>"));
    assert!(projected_xml.contains("<LCIMethodAndAllocation/>"));
    let recovery: EilcdProjectionRecoveryV1 = serde_json::from_slice(
        &fs::read(ilcd.join("data/processes/synthetic.tidas-recovery.json")).unwrap(),
    )
    .unwrap();
    for path in [YEAR_PATH, METHOD_PATH] {
        assert!(
            recovery.restorations.iter().any(|item| item.path == path),
            "missing exact-type recovery for {path}"
        );
    }
    let ilcd_issues = directory.path().join("ilcd-issues.jsonl");
    let ilcd_validation =
        validate_ilcd_package(&validation(&ilcd.join("data"), &ilcd_issues)).unwrap();
    assert!(
        ilcd_validation.summary.ok,
        "projected eILCD must pass XSD validation: {}",
        fs::read_to_string(&ilcd_issues).unwrap()
    );

    let restored = directory.path().join("restored");
    let restored_repeat = directory.path().join("restored-repeat");
    let reverse = convert_directory(&conversion(
        &ilcd.join("data"),
        &restored,
        ConversionDirection::IlcdToTidas,
    ))
    .unwrap();
    let repeated_reverse = convert_directory(&conversion(
        &ilcd.join("data"),
        &restored_repeat,
        ConversionDirection::IlcdToTidas,
    ))
    .unwrap();
    assert_eq!(
        reverse.output_tree_sha256,
        repeated_reverse.output_tree_sha256
    );
    assert!(reverse.peak_accounted_memory_bytes <= 32 * 1024 * 1024);

    assert_restored_process(
        &restored,
        &expected,
        &directory.path().join("restored-issues.jsonl"),
    );
    assert_legacy_reverse(&ilcd, directory.path(), &recovery, &expected);
}
