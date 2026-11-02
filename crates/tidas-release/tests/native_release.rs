use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use tidas_import::{ImportRequest, ImportTarget, SourceFormat, run_import};
use tidas_release::{
    RELEASE_REPORT_JSON_SCHEMA_V1, ReleaseDataFormat, ReleaseError, ReleaseProfile, ReleaseRequest,
    ReleaseRuntime, UNIT_PROFILE, run_release,
};
use tidas_runtime::{CancellationToken, MemoryBudget, RuntimeError};
use walkdir::WalkDir;

const UNIT_ID: &str = "11111111-1111-4111-8111-111111111111";
const FLOW_ID: &str = "22222222-2222-4222-8222-222222222222";
const MODEL_ID: &str = "c58f567c-c631-5a3a-90d9-c0cec7290cf8";
const RESULT_ID: &str = "ba3386d3-39c0-5a48-ae4d-e7ad90ec4996";
const VERSION: &str = "01.00.000";

fn runtime() -> ReleaseRuntime {
    ReleaseRuntime {
        cancellation: CancellationToken::default(),
        memory_budget: MemoryBudget::new(64 * 1024 * 1024),
        queue_capacity: 16,
    }
}

fn reference(dataset_type: &str, id: &str, category: &str) -> Value {
    json!({
        "@type": dataset_type,
        "@refObjectId": id,
        "@version": VERSION,
        "@uri": format!("../{category}/{id}_{VERSION}.json")
    })
}

fn write_dataset(
    root: &Path,
    relative: &str,
    document: &Value,
    dataset_type: &str,
    role: &str,
    id: &str,
) -> Value {
    let path = root.join(relative);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    let mut bytes = serde_json::to_vec_pretty(document).unwrap();
    bytes.push(b'\n');
    fs::write(&path, &bytes).unwrap();
    json!({
        "datasetType": dataset_type,
        "role": role,
        "uuid": id,
        "version": VERSION,
        "path": relative,
        "sha256": hex(Sha256::digest(&bytes)),
        "byteSize": bytes.len(),
        "canonicalContentHash": hex(Sha256::digest(serde_json::to_vec(document).unwrap()))
    })
}

#[allow(clippy::too_many_lines)]
fn fixture(root: &Path) -> (PathBuf, PathBuf) {
    let tidas = root.join("tidas");
    let mut entries = vec![
        write_dataset(
            &tidas,
            &format!("flows/{FLOW_ID}_{VERSION}.json"),
            &json!({
                "flowDataSet": {
                    "flowInformation": {
                        "dataSetInformation": {"common:UUID": FLOW_ID}
                    }
                }
            }),
            "flow",
            "support",
            FLOW_ID,
        ),
        write_dataset(
            &tidas,
            &format!("processes/{UNIT_ID}_{VERSION}.json"),
            &json!({
                "processDataSet": {
                    "processInformation": {
                        "dataSetInformation": {"common:UUID": UNIT_ID}
                    },
                    "exchanges": {
                        "exchange": {
                            "referenceToFlowDataSet": reference(
                                "flow data set",
                                FLOW_ID,
                                "flows"
                            )
                        }
                    }
                }
            }),
            "process",
            "unit_process",
            UNIT_ID,
        ),
        write_dataset(
            &tidas,
            &format!("processes/{RESULT_ID}_{VERSION}.json"),
            &json!({
                "processDataSet": {
                    "processInformation": {
                        "dataSetInformation": {"common:UUID": RESULT_ID}
                    },
                    "exchanges": {
                        "exchange": {
                            "referenceToFlowDataSet": reference(
                                "flow data set",
                                FLOW_ID,
                                "flows"
                            )
                        }
                    }
                }
            }),
            "process",
            "result_process",
            RESULT_ID,
        ),
        write_dataset(
            &tidas,
            &format!("lifecyclemodels/{MODEL_ID}_{VERSION}.json"),
            &json!({
                "lifeCycleModelDataSet": {
                    "lifeCycleModelInformation": {
                        "dataSetInformation": {
                            "common:UUID": MODEL_ID,
                            "referenceToResultingProcess": reference(
                                "process data set",
                                RESULT_ID,
                                "processes"
                            )
                        },
                        "technology": {
                            "processes": {
                                "processInstance": {
                                    "referenceToProcess": reference(
                                        "process data set",
                                        UNIT_ID,
                                        "processes"
                                    )
                                }
                            }
                        }
                    }
                }
            }),
            "lifecyclemodel",
            "lifecycle_model",
            MODEL_ID,
        ),
    ];
    entries.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap()
            .cmp(right["path"].as_str().unwrap())
    });
    let index = root.join("canonical-dataset-index.json");
    let mut bytes = serde_json::to_vec_pretty(&json!({
        "schemaVersion": "tiangong.release.canonical-dataset-index.v1",
        "datasetCount": entries.len(),
        "byteSize": entries.iter().map(|entry| entry["byteSize"].as_u64().unwrap()).sum::<u64>(),
        "artifactSetHash": "0".repeat(64),
        "datasets": entries
    }))
    .unwrap();
    bytes.push(b'\n');
    fs::write(&index, bytes).unwrap();
    (tidas, index)
}

#[test]
fn exact_closure_and_schema_ordered_roundtrip_match_the_frozen_python_fixture() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, index) = fixture(temporary.path());
    let closure = run_release(
        &ReleaseRequest::ValidateClosure {
            input_dir: tidas.clone(),
            dataset_index: index,
            profile: ReleaseProfile::UnitProcess,
        },
        &runtime(),
    )
    .unwrap()
    .closure
    .unwrap();
    assert_eq!(closure.profile_id, UNIT_PROFILE);
    assert_eq!(closure.dataset_count, 2);
    assert_eq!(
        closure.dataset_keys,
        [
            format!("flow:{FLOW_ID}:{VERSION}"),
            format!("process:{UNIT_ID}:{VERSION}")
        ]
    );
    assert!(!closure.dataset_keys_truncated);

    let ilcd = temporary.path().join("ilcd");
    let conversion = run_release(
        &ReleaseRequest::ConvertIlcd {
            input_dir: tidas.clone(),
            output_dir: ilcd.clone(),
        },
        &runtime(),
    )
    .unwrap()
    .conversion
    .unwrap();
    assert_eq!(conversion.dataset_count, 4);
    let process_xml =
        fs::read_to_string(ilcd.join(format!("data/processes/{UNIT_ID}_{VERSION}.xml"))).unwrap();
    assert!(
        process_xml.find("processInformation").unwrap() < process_xml.find("exchanges").unwrap()
    );
    assert!(process_xml.contains(&format!("{FLOW_ID}_{VERSION}.xml")));

    let roundtrip = run_release(
        &ReleaseRequest::SemanticRoundtrip {
            tidas_dir: tidas,
            ilcd_dir: ilcd,
        },
        &runtime(),
    )
    .unwrap()
    .roundtrip
    .unwrap();
    assert!(roundtrip.ok, "{:?}", roundtrip.mismatches);
    assert_eq!(roundtrip.dataset_count, 4);
}

#[test]
fn missing_exact_reference_version_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, index) = fixture(temporary.path());
    let unit_path = tidas.join(format!("processes/{UNIT_ID}_{VERSION}.json"));
    let mut document: Value = serde_json::from_slice(&fs::read(&unit_path).unwrap()).unwrap();
    document["processDataSet"]["exchanges"]["exchange"]["referenceToFlowDataSet"]
        .as_object_mut()
        .unwrap()
        .remove("@version");
    let mut body = serde_json::to_vec_pretty(&document).unwrap();
    body.push(b'\n');
    fs::write(&unit_path, &body).unwrap();
    let mut index_value: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    let entry = index_value["datasets"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["uuid"] == UNIT_ID)
        .unwrap();
    entry["sha256"] = Value::String(hex(Sha256::digest(&body)));
    fs::write(&index, serde_json::to_vec_pretty(&index_value).unwrap()).unwrap();

    let error = run_release(
        &ReleaseRequest::ValidateClosure {
            input_dir: tidas,
            dataset_index: index,
            profile: ReleaseProfile::UnitProcess,
        },
        &runtime(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        tidas_release::ReleaseError::ReferenceVersionMissing(_)
    ));
}

#[test]
fn preceding_dataset_version_is_lineage_not_a_closure_dependency() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, index) = fixture(temporary.path());
    let flow_path = tidas.join(format!("flows/{FLOW_ID}_{VERSION}.json"));
    let mut document: Value = serde_json::from_slice(&fs::read(&flow_path).unwrap()).unwrap();
    document["flowDataSet"]["administrativeInformation"]["publicationAndOwnership"]["common:referenceToPrecedingDataSetVersion"] = json!({
        "@type": "flow data set",
        "@refObjectId": FLOW_ID,
        "@version": "00.99.000"
    });
    let mut body = serde_json::to_vec_pretty(&document).unwrap();
    body.push(b'\n');
    fs::write(&flow_path, &body).unwrap();
    update_index_digest(&index, FLOW_ID, &body);

    let closure = run_release(
        &ReleaseRequest::ValidateClosure {
            input_dir: tidas,
            dataset_index: index,
            profile: ReleaseProfile::UnitProcess,
        },
        &runtime(),
    )
    .unwrap()
    .closure
    .unwrap();
    assert_eq!(closure.dataset_count, 2);
    assert_eq!(closure.reference_count, 1);
}

#[test]
fn missing_functional_reference_target_still_fails_closed() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, index) = fixture(temporary.path());
    let unit_path = tidas.join(format!("processes/{UNIT_ID}_{VERSION}.json"));
    let mut document: Value = serde_json::from_slice(&fs::read(&unit_path).unwrap()).unwrap();
    let missing_id = "33333333-3333-4333-8333-333333333333";
    document["processDataSet"]["exchanges"]["exchange"]["referenceToFlowDataSet"]["@refObjectId"] =
        Value::String(missing_id.to_owned());
    let mut body = serde_json::to_vec_pretty(&document).unwrap();
    body.push(b'\n');
    fs::write(&unit_path, &body).unwrap();
    update_index_digest(&index, UNIT_ID, &body);

    let error = run_release(
        &ReleaseRequest::ValidateClosure {
            input_dir: tidas,
            dataset_index: index,
            profile: ReleaseProfile::UnitProcess,
        },
        &runtime(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        tidas_release::ReleaseError::ReferenceClosureMissing(ref detail)
            if detail.contains(missing_id) && detail.contains("referenceToFlowDataSet")
    ));
}

fn update_index_digest(index: &Path, uuid: &str, body: &[u8]) {
    let mut index_value: Value = serde_json::from_slice(&fs::read(index).unwrap()).unwrap();
    let entry = index_value["datasets"]
        .as_array_mut()
        .unwrap()
        .iter_mut()
        .find(|entry| entry["uuid"] == uuid)
        .unwrap();
    entry["sha256"] = Value::String(hex(Sha256::digest(body)));
    entry["byteSize"] = json!(body.len());
    fs::write(index, serde_json::to_vec_pretty(&index_value).unwrap()).unwrap();
}

#[test]
fn large_closure_report_is_capped_and_explicitly_marked_truncated() {
    let temporary = tempfile::tempdir().unwrap();
    let tidas = temporary.path().join("tidas");
    let mut entries = Vec::new();
    for index in 0..257 {
        let id = format!("00000000-0000-4000-8000-{index:012}");
        entries.push(write_dataset(
            &tidas,
            &format!("flows/{id}_{VERSION}.json"),
            &json!({
                "flowDataSet": {
                    "flowInformation": {
                        "dataSetInformation": {"common:UUID": id.clone()}
                    }
                }
            }),
            "flow",
            "unit_process",
            &id,
        ));
    }
    let index_path = temporary.path().join("large-index.json");
    let mut index_bytes = serde_json::to_vec_pretty(&json!({
        "schemaVersion": "tiangong.release.canonical-dataset-index.v1",
        "datasetCount": entries.len(),
        "byteSize": entries.iter().map(|entry| entry["byteSize"].as_u64().unwrap()).sum::<u64>(),
        "artifactSetHash": "0".repeat(64),
        "datasets": entries
    }))
    .unwrap();
    index_bytes.push(b'\n');
    fs::write(&index_path, index_bytes).unwrap();
    let closure = run_release(
        &ReleaseRequest::ValidateClosure {
            input_dir: tidas,
            dataset_index: index_path,
            profile: ReleaseProfile::UnitProcess,
        },
        &runtime(),
    )
    .unwrap()
    .closure
    .unwrap();
    assert_eq!(closure.dataset_count, 257);
    assert_eq!(closure.dataset_keys.len(), 256);
    assert!(closure.dataset_keys_truncated);
}

#[test]
fn cancellation_never_publishes_conversion_output() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, _) = fixture(temporary.path());
    let output = temporary.path().join("cancelled");
    fs::create_dir(&output).unwrap();
    fs::write(output.join("sentinel"), b"keep").unwrap();
    let cancellation = CancellationToken::default();
    cancellation.cancel();
    let runtime = ReleaseRuntime {
        cancellation,
        memory_budget: MemoryBudget::new(64 * 1024 * 1024),
        queue_capacity: 16,
    };
    let error = run_release(
        &ReleaseRequest::ConvertIlcd {
            input_dir: tidas,
            output_dir: output.clone(),
        },
        &runtime,
    )
    .unwrap_err();
    assert!(matches!(
        error,
        tidas_release::ReleaseError::Runtime(RuntimeError::Cancelled)
    ));
    assert_eq!(fs::read(output.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn output_ancestor_or_input_directory_is_rejected_before_mutation() {
    let temporary = tempfile::tempdir().unwrap();
    let input = temporary.path().join("input");
    fs::create_dir(&input).unwrap();
    fs::write(input.join("sentinel"), b"keep").unwrap();
    let error = run_release(
        &ReleaseRequest::ConvertIlcd {
            input_dir: input.clone(),
            output_dir: temporary.path().to_path_buf(),
        },
        &runtime(),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        tidas_release::ReleaseError::OutputInsideInput(_)
    ));
    assert_eq!(fs::read(input.join("sentinel")).unwrap(), b"keep");

    let same_error = run_release(
        &ReleaseRequest::ConvertIlcd {
            input_dir: input.clone(),
            output_dir: input.clone(),
        },
        &runtime(),
    )
    .unwrap_err();
    assert!(matches!(
        same_error,
        tidas_release::ReleaseError::OutputInsideInput(_)
    ));
    assert_eq!(fs::read(input.join("sentinel")).unwrap(), b"keep");
}

#[test]
fn native_valid_tree_builds_four_repeatable_self_contained_packages() {
    let temporary = tempfile::tempdir().unwrap();
    let source = temporary.path().join("source.csv");
    fs::write(
        &source,
        b"{SimaPro 9.5}\n{CSV separator: semicolon}\n\nProcess\n\nProcess name\nSteel production\n\nComment\nrelease fixture\n\nProducts\nSteel | production route | GLO;kg;1\n\nEmissions to air\nCarbon dioxide;air;kg;2.5\n\nEnd\n",
    )
    .unwrap();
    let imported = temporary.path().join("imported");
    run_import(&ImportRequest {
        source,
        requested_format: Some(SourceFormat::SimaproCsv),
        output_dir: imported.clone(),
        target: ImportTarget::Tidas,
        write_mapping: false,
        write_process_bundles: false,
        cancellation: CancellationToken::default(),
        memory_budget: MemoryBudget::new(128 * 1024 * 1024),
        queue_capacity: 16,
        max_entry_bytes: 8 * 1024 * 1024,
        max_issue_bytes: 64 * 1024,
    })
    .unwrap();
    let tidas = imported.join("tidas");
    let index = write_index_for_valid_package(&tidas, temporary.path());

    let first_output = temporary.path().join("release-a");
    let first_report = run_release(
        &ReleaseRequest::BuildPackages {
            tidas_dir: tidas.clone(),
            dataset_index: index.clone(),
            output_dir: first_output.clone(),
            issue_spool: None,
        },
        &runtime(),
    )
    .unwrap();
    let schema: Value = serde_json::from_str(RELEASE_REPORT_JSON_SCHEMA_V1).unwrap();
    let validator = jsonschema::validator_for(&schema).unwrap();
    assert!(validator.is_valid(&serde_json::to_value(&first_report).unwrap()));
    let first = first_report.build.unwrap();
    assert_eq!(first.packages.len(), 4);
    assert!(first.packages.iter().all(|package| package.self_contained));
    assert!(first.roundtrip.ok);
    assert!(first.tidas_validation.summary.ok);
    assert!(first.ilcd_validation.summary.ok);
    assert_eq!(
        fs::read_dir(&first_output).unwrap().count(),
        4,
        "only the four final archives are published"
    );

    let second = run_release(
        &ReleaseRequest::BuildPackages {
            tidas_dir: tidas,
            dataset_index: index,
            output_dir: temporary.path().join("release-b"),
            issue_spool: None,
        },
        &runtime(),
    )
    .unwrap()
    .build
    .unwrap();
    assert_eq!(first.artifact_set_sha256, second.artifact_set_sha256);
    assert_eq!(
        first
            .packages
            .iter()
            .map(|package| &package.artifact.sha256)
            .collect::<Vec<_>>(),
        second
            .packages
            .iter()
            .map(|package| &package.artifact.sha256)
            .collect::<Vec<_>>()
    );
}

#[test]
fn failed_package_build_writes_the_requested_issue_spool() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, index) = fixture(temporary.path());
    let spool = temporary.path().join("validation-issues.ndjson");
    let error = run_release(
        &ReleaseRequest::BuildPackages {
            tidas_dir: tidas,
            dataset_index: index,
            output_dir: temporary.path().join("release"),
            issue_spool: Some(spool.clone()),
        },
        &runtime(),
    )
    .unwrap_err();
    match error {
        ReleaseError::ValidationIssues {
            format,
            summary,
            issue_spool,
        } => {
            assert_eq!(format, ReleaseDataFormat::Tidas);
            assert_eq!(issue_spool.as_deref(), Some(spool.as_path()));
            assert!(!summary.ok);
            assert!(summary.issue_count > 0);
            assert!(summary.issue_spool.is_some());
        }
        other => panic!("unexpected release error: {other}"),
    }
    let bytes = fs::read(&spool).unwrap();
    assert!(!bytes.is_empty());
    assert!(String::from_utf8_lossy(&bytes).contains("issue_code"));
}

fn write_index_for_valid_package(tidas: &Path, root: &Path) -> PathBuf {
    let mut entries = Vec::new();
    for item in WalkDir::new(tidas).min_depth(2).max_depth(2) {
        let item = item.unwrap();
        if !item.file_type().is_file()
            || item.path().extension().and_then(|value| value.to_str()) != Some("json")
        {
            continue;
        }
        let relative = item.path().strip_prefix(tidas).unwrap();
        let category = relative
            .parent()
            .and_then(Path::file_name)
            .and_then(|value| value.to_str())
            .unwrap();
        let dataset_type = match category {
            "contacts" => "contact",
            "flows" => "flow",
            "flowproperties" => "flowproperty",
            "unitgroups" => "unitgroup",
            "processes" => "process",
            "sources" => "source",
            "lciamethods" => "lciamethod",
            "lifecyclemodels" => "lifecyclemodel",
            other => panic!("unexpected category {other}"),
        };
        let bytes = fs::read(item.path()).unwrap();
        let document: Value = serde_json::from_slice(&bytes).unwrap();
        let uuid = find_string(&document, "common:UUID").unwrap();
        let version =
            find_string(&document, "common:dataSetVersion").unwrap_or_else(|| VERSION.to_owned());
        let role = match category {
            "flows" => "unit_process",
            "processes" => "result_process",
            _ => "support",
        };
        entries.push(json!({
            "datasetType": dataset_type,
            "role": role,
            "uuid": uuid,
            "version": version,
            "path": relative.to_string_lossy().replace('\\', "/"),
            "sha256": hex(Sha256::digest(&bytes)),
            "byteSize": bytes.len(),
            "canonicalContentHash": hex(Sha256::digest(serde_json::to_vec(&document).unwrap()))
        }));
    }
    entries.sort_by(|left, right| {
        left["path"]
            .as_str()
            .unwrap()
            .cmp(right["path"].as_str().unwrap())
    });
    let path = root.join("valid-canonical-dataset-index.json");
    let mut bytes = serde_json::to_vec_pretty(&json!({
        "schemaVersion": "tiangong.release.canonical-dataset-index.v1",
        "datasetCount": entries.len(),
        "byteSize": entries.iter().map(|entry| entry["byteSize"].as_u64().unwrap()).sum::<u64>(),
        "artifactSetHash": "0".repeat(64),
        "datasets": entries
    }))
    .unwrap();
    bytes.push(b'\n');
    fs::write(&path, bytes).unwrap();
    path
}

fn find_string(value: &Value, key: &str) -> Option<String> {
    match value {
        Value::Object(object) => object
            .get(key)
            .and_then(Value::as_str)
            .map(ToOwned::to_owned)
            .or_else(|| object.values().find_map(|child| find_string(child, key))),
        Value::Array(items) => items.iter().find_map(|child| find_string(child, key)),
        _ => None,
    }
}

fn hex(bytes: impl AsRef<[u8]>) -> String {
    bytes.as_ref().iter().fold(
        String::with_capacity(bytes.as_ref().len() * 2),
        |mut output, byte| {
            write!(&mut output, "{byte:02x}").unwrap();
            output
        },
    )
}

#[test]
fn closure_allocation_admission_uses_selected_exact_flow_content() {
    for (kind, count) in [
        ("Product flow", 1),
        ("Waste flow", 1),
        ("Elementary flow", 1),
        ("Elementary flow", 300),
    ] {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path().join("tidas");
        let flow = json!({"flowDataSet":{"flowInformation":{"dataSetInformation":{"common:UUID":FLOW_ID}},"administrativeInformation":{"publicationAndOwnership":{"common:dataSetVersion":VERSION}},"modellingAndValidation":{"LCIMethod":{"typeOfDataSet":kind}}}});
        let mut process = json!({"processDataSet":{"processInformation":{"quantitativeReference":{"@type":"Reference flow(s)","referenceToReferenceFlow":"0"}},"exchanges":{"exchange":[{"@dataSetInternalID":"0","exchangeDirection":"Input","referenceToFlowDataSet":reference("flow data set",FLOW_ID,"flows")},{"@dataSetInternalID":"1","exchangeDirection":"Output","allocations":{"allocation":{"@internalReferenceToCoProduct":"0","@allocatedFraction":"100"}}}]}}});
        if count > 1 {
            process["processDataSet"]["exchanges"]["exchange"][1]["allocations"]["allocation"] = json!((0..count).map(|i|json!({"@internalReferenceToCoProduct":"0","@allocatedFraction":if i + 1 == count {100} else {0}})).collect::<Vec<_>>());
        }
        let entries = vec![
            write_dataset(&root, "flows/exact.json", &flow, "flow", "support", FLOW_ID),
            write_dataset(
                &root,
                "processes/process.json",
                &process,
                "process",
                "unit_process",
                UNIT_ID,
            ),
        ];
        let index = temporary.path().join("index.json");
        fs::write(&index,serde_json::to_vec(&json!({"schemaVersion":"tiangong.release.canonical-dataset-index.v1","datasetCount":2,"byteSize":0,"artifactSetHash":"0".repeat(64),"datasets":entries})).unwrap()).unwrap();
        let report = run_release(
            &ReleaseRequest::ValidateClosure {
                input_dir: root,
                dataset_index: index,
                profile: ReleaseProfile::UnitProcess,
            },
            &runtime(),
        )
        .unwrap();
        let schema: Value = serde_json::from_str(RELEASE_REPORT_JSON_SCHEMA_V1).unwrap();
        assert!(
            jsonschema::validator_for(&schema)
                .unwrap()
                .is_valid(&serde_json::to_value(&report).unwrap())
        );
        assert_eq!(report.ok, kind != "Elementary flow");
        let closure = report.closure.unwrap();
        let diagnostics = closure.semantic_diagnostics.unwrap();
        assert_eq!(diagnostics.truncated, count > 256);
        assert_eq!(
            diagnostics.issues.len(),
            if kind == "Elementary flow" {
                count.min(256)
            } else {
                0
            }
        );
        if kind == "Elementary flow" {
            assert!(diagnostics.issue_count >= u64::try_from(count).unwrap());
            assert_eq!(diagnostics.issues[0].file_path, "processes/process.json");
            assert!(
                diagnostics.issues[0]
                    .location
                    .ends_with("@internalReferenceToCoProduct")
            );
            assert_eq!(
                diagnostics.issues[0].issue_code,
                "allocation_target_flow_type_invalid"
            );
        }
        let coverage = closure.semantic_coverage.unwrap();
        assert!(coverage.complete);
        assert_eq!(
            coverage.checks["allocation-target-type"].invalid,
            if kind == "Elementary flow" {
                u64::try_from(count).unwrap()
            } else {
                0
            }
        );
    }
}

#[test]
fn closure_dense_projections_reserve_cardinality_before_analysis() {
    for legacy in [false, true] {
        let temp = tempfile::tempdir().unwrap();
        let root = temp.path().join("tidas");
        let flow = json!({"flowDataSet":{"flowInformation":{"dataSetInformation":{"common:UUID":FLOW_ID}},"administrativeInformation":{"publicationAndOwnership":{"common:dataSetVersion":VERSION}},"modellingAndValidation":{"LCIMethod":{"typeOfDataSet":"Waste flow"}}}});
        let rows=(0..250).map(|i| {
            let mut row=json!({"@dataSetInternalID":i.to_string(),"exchangeDirection":"Output","referenceToFlowDataSet":reference("flow data set",FLOW_ID,"flows")});
            if legacy {row["allocations"]=json!({"allocation":{"@allocatedFraction":"0.4"}});}
            row
        }).collect::<Vec<_>>();
        let refs = if legacy {
            json!("0")
        } else {
            json!((0..250).map(|i| i.to_string()).collect::<Vec<_>>())
        };
        let process = json!({"processDataSet":{"processInformation":{"quantitativeReference":{"@type":"Reference flow(s)","referenceToReferenceFlow":refs}},"exchanges":{"exchange":rows}}});
        let entries = vec![
            write_dataset(&root, "flows/exact.json", &flow, "flow", "support", FLOW_ID),
            write_dataset(
                &root,
                "processes/process.json",
                &process,
                "process",
                "unit_process",
                UNIT_ID,
            ),
        ];
        let index = temp.path().join("index.json");
        fs::write(&index,serde_json::to_vec(&json!({"schemaVersion":"tiangong.release.canonical-dataset-index.v1","datasetCount":2,"byteSize":0,"artifactSetHash":"0".repeat(64),"datasets":entries})).unwrap()).unwrap();
        let mut runtime = runtime();
        runtime.memory_budget = MemoryBudget::new(32 * 1024 * 1024);
        assert!(
            fs::metadata(root.join("processes/process.json"))
                .unwrap()
                .len()
                * 72
                + 131_072
                < runtime.memory_budget.limit()
        );
        let result = run_release(
            &ReleaseRequest::ValidateClosure {
                input_dir: root,
                dataset_index: index,
                profile: ReleaseProfile::UnitProcess,
            },
            &runtime,
        );
        assert!(matches!(
            result,
            Err(tidas_release::ReleaseError::Runtime(
                RuntimeError::BudgetExceeded { .. }
            ))
        ));
    }
}

fn result_only_fixture(root: &Path) -> (PathBuf, PathBuf) {
    let source = root.join("result-source.csv");
    fs::write(&source, b"{SimaPro 9.5}\n{CSV separator: semicolon}\n\nProcess\n\nProcess name\nFinal result\n\nComment\nResult-only package fixture\n\nProducts\nSteel | production route | GLO;kg;1\n\nEmissions to air\nCarbon dioxide;air;kg;2.5\n\nEnd\n").unwrap();
    let imported = root.join("result-imported");
    run_import(&ImportRequest {
        source,
        requested_format: Some(SourceFormat::SimaproCsv),
        output_dir: imported.clone(),
        target: ImportTarget::Tidas,
        write_mapping: false,
        write_process_bundles: false,
        cancellation: CancellationToken::default(),
        memory_budget: MemoryBudget::new(128 * 1024 * 1024),
        queue_capacity: 16,
        max_entry_bytes: 8 * 1024 * 1024,
        max_issue_bytes: 64 * 1024,
    })
    .unwrap();
    let tidas = imported.join("tidas");
    for item in fs::read_dir(tidas.join("processes")).unwrap() {
        let path = item.unwrap().path();
        let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        document["processDataSet"]["modellingAndValidation"]["LCIMethodAndAllocation"]["typeOfDataSet"] =
            json!("LCI result");
        fs::write(&path, serde_json::to_vec_pretty(&document).unwrap()).unwrap();
    }
    let index = write_index_for_valid_package(&tidas, root);
    let mut value: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    for entry in value["datasets"].as_array_mut().unwrap() {
        if entry["datasetType"] != "process" {
            entry["role"] = json!("support");
        }
    }
    fs::write(&index, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
    (tidas, index)
}

#[test]
fn result_only_builds_two_repeatable_archives_without_unit_or_model_roots() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, index) = result_only_fixture(temporary.path());
    let build = |name: &str| {
        run_release(
            &ReleaseRequest::BuildResultPackages {
                tidas_dir: tidas.clone(),
                dataset_index: index.clone(),
                output_dir: temporary.path().join(name),
                issue_spool: None,
            },
            &runtime(),
        )
        .unwrap()
    };
    let first = build("result-a");
    assert_eq!(
        first.schema_version,
        tidas_release::RELEASE_REPORT_SCHEMA_V2
    );
    let schema: Value = serde_json::from_str(tidas_release::RELEASE_REPORT_JSON_SCHEMA_V2).unwrap();
    assert!(
        jsonschema::validator_for(&schema)
            .unwrap()
            .is_valid(&serde_json::to_value(&first).unwrap())
    );
    let first = first.build.unwrap();
    let second = build("result-b").build.unwrap();
    assert_eq!(first.packages.len(), 2);
    assert_eq!(first.profiles.len(), 1);
    assert_eq!(
        first.profiles[0].profile_id,
        tidas_release::RESULT_ONLY_PROFILE
    );
    assert_eq!(first.profiles[0].root_count, 1);
    assert_eq!(first.artifact_set_sha256, second.artifact_set_sha256);
    assert!(
        first.roundtrip.ok && first.tidas_validation.summary.ok && first.ilcd_validation.summary.ok
    );
    for package in first.packages {
        assert_eq!(package.profile_id, tidas_release::RESULT_ONLY_PROFILE);
        assert!(package.self_contained);
        let mut zip =
            zip::ZipArchive::new(fs::File::open(&package.artifact.path).unwrap()).unwrap();
        for ordinal in 0..zip.len() {
            assert!(
                !zip.by_index(ordinal)
                    .unwrap()
                    .name()
                    .starts_with("lifecyclemodels/")
            );
        }
    }
    assert_eq!(
        fs::read_dir(temporary.path().join("result-a"))
            .unwrap()
            .count(),
        2
    );
    let legacy = run_release(
        &ReleaseRequest::BuildPackages {
            tidas_dir: tidas,
            dataset_index: index,
            output_dir: temporary.path().join("legacy"),
            issue_spool: None,
        },
        &runtime(),
    )
    .unwrap_err();
    assert!(matches!(legacy, ReleaseError::ProfileRootsMissing(_)));
}

#[test]
fn result_only_rejects_false_roles_budget_and_cancellation_without_publishing() {
    let temporary = tempfile::tempdir().unwrap();
    let (tidas, index) = result_only_fixture(temporary.path());
    let request = ReleaseRequest::BuildResultPackages {
        tidas_dir: tidas.clone(),
        dataset_index: index.clone(),
        output_dir: temporary.path().join("failed"),
        issue_spool: None,
    };
    let small = ReleaseRuntime {
        memory_budget: MemoryBudget::new(1),
        ..runtime()
    };
    assert!(run_release(&request, &small).is_err());
    assert!(!temporary.path().join("failed").exists());
    let cancelled = runtime();
    cancelled.cancellation.cancel();
    assert!(run_release(&request, &cancelled).is_err());
    assert!(!temporary.path().join("failed").exists());
    let process = fs::read_dir(tidas.join("processes"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let mut document: Value = serde_json::from_slice(&fs::read(&process).unwrap()).unwrap();
    document["processDataSet"]["modellingAndValidation"]["LCIMethodAndAllocation"]["typeOfDataSet"] =
        json!("Unit process, single operation");
    let bytes = serde_json::to_vec_pretty(&document).unwrap();
    fs::write(&process, &bytes).unwrap();
    let uuid = find_string(&document, "common:UUID").unwrap();
    update_index_digest(&index, &uuid, &bytes);
    assert!(matches!(
        run_release(
            &ReleaseRequest::ValidateClosure {
                input_dir: tidas,
                dataset_index: index,
                profile: ReleaseProfile::ResultProcess
            },
            &runtime()
        ),
        Err(ReleaseError::DatasetIndexInvalid(_))
    ));
}

#[test]
#[ignore = "generated 64-Result package repeatability and resource probe"]
fn generated_result_only_package_scale() {
    // Golden digests describe only this checked-in deterministic generator.
    // Runtime logs carry fixed fixture identifiers and resource measurements.
    const EXPECTED_INPUT: &str = "a9ae0e37d6e78afb740b7f8775d0bea4a687cf94edafe2b01708593d0086603f";
    const EXPECTED_ARTIFACTS: &str =
        "8d15e3968631d578567bd19229abfc0a332d96bee6b6695b025aca725bd1e372";

    let temporary = tempfile::tempdir().unwrap();
    let (tidas, _) = result_only_fixture(temporary.path());
    let source = fs::read_dir(tidas.join("processes"))
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let original: Value = serde_json::from_slice(&fs::read(&source).unwrap()).unwrap();
    let version = find_string(&original, "common:dataSetVersion").unwrap();
    for ordinal in 1..64 {
        let uuid = format!("00000000-0000-4000-8000-{ordinal:012}");
        let mut document = original.clone();
        document["processDataSet"]["processInformation"]["dataSetInformation"]["common:UUID"] =
            json!(uuid);
        fs::write(
            tidas.join(format!("processes/{uuid}_{version}.json")),
            serde_json::to_vec(&document).unwrap(),
        )
        .unwrap();
    }
    let index = write_index_for_valid_package(&tidas, temporary.path());
    let mut value: Value = serde_json::from_slice(&fs::read(&index).unwrap()).unwrap();
    for entry in value["datasets"].as_array_mut().unwrap() {
        if entry["datasetType"] != "process" {
            entry["role"] = json!("support");
        }
    }
    fs::write(&index, serde_json::to_vec(&value).unwrap()).unwrap();
    assert_eq!(
        hex(Sha256::digest(fs::read(&index).unwrap())),
        EXPECTED_INPUT
    );
    println!("fixture_input_sha256={EXPECTED_INPUT}");
    let mut previous = None;
    for attempt in 0..2 {
        let started = std::time::Instant::now();
        let report = run_release(
            &ReleaseRequest::BuildResultPackages {
                tidas_dir: tidas.clone(),
                dataset_index: index.clone(),
                output_dir: temporary.path().join(format!("scale-{attempt}")),
                issue_spool: None,
            },
            &runtime(),
        )
        .unwrap()
        .build
        .unwrap();
        assert_eq!(report.profiles[0].root_count, 64);
        assert_eq!(report.packages.len(), 2);
        assert!(
            report.tidas_validation.summary.ok
                && report.ilcd_validation.summary.ok
                && report.roundtrip.ok
        );
        assert_eq!(report.profiles[0].dataset_count, 71);
        assert_eq!(report.artifact_set_sha256, EXPECTED_ARTIFACTS);
        println!(
            "attempt={attempt} roots=64 datasets=71 elapsed_ms={} expected_artifact_set={EXPECTED_ARTIFACTS}",
            started.elapsed().as_millis()
        );
        if let Some(hash) = previous {
            assert_eq!(report.artifact_set_sha256, hash);
        }
        previous = Some(report.artifact_set_sha256);
    }
}
