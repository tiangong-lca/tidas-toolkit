use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde_json::Value;
use tempfile::tempdir;
use tidas_conversion::{ConversionDirection, ConversionRequest, convert_directory};
use tidas_import::{ImportRequest, ImportTarget, SourceFormat, run_import};
use tidas_runtime::{CancellationToken, MemoryBudget};

const UNIT_ID: &str = "22222222-2222-4222-8222-222222222222";
const PROPERTY_ID: &str = "33333333-3333-4333-8333-333333333333";
const SOURCE_ID: &str = "77777777-7777-4777-8777-777777777777";
const OTHER_SOURCE_ID: &str = "66666666-6666-4666-8666-666666666666";
const PROCESS_ID: &str = "55555555-5555-4555-8555-555555555555";
const FLOW_IDS: [&str; 3] = [
    "44444444-4444-4444-8444-444444444444",
    "88888888-8888-4888-8888-888888888888",
    "99999999-9999-4999-8999-999999999999",
];

fn write_fixture(root: &Path) {
    for category in [
        "sources",
        "unitgroups",
        "flowproperties",
        "flows",
        "processes",
    ] {
        fs::create_dir_all(root.join(category)).unwrap();
    }
    fs::write(
        root.join(format!("sources/{SOURCE_ID}.xml")),
        format!(
            r#"<sourceDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><sourceInformation><dataSetInformation><common:UUID>{SOURCE_ID}</common:UUID><common:shortName xml:lang="en">Mine-water coefficient archive</common:shortName><common:shortName xml:lang="zh">矿井水系数档案</common:shortName><sourceCitation>Source table, draft edition</sourceCitation><sourceDescriptionOrComment xml:lang="en">The primary archive has not been authenticated.</sourceDescriptionOrComment><sourceDescriptionOrComment xml:lang="zh">原始档案尚未核验。</sourceDescriptionOrComment></dataSetInformation></sourceInformation><administrativeInformation><publicationAndOwnership><common:dataSetVersion>01.00.000</common:dataSetVersion></publicationAndOwnership></administrativeInformation></sourceDataSet>"#
        ),
    )
    .unwrap();
    fs::write(
        root.join(format!("unitgroups/{UNIT_ID}.xml")),
        format!(
            r#"<unitGroupDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><unitGroupInformation><dataSetInformation><common:UUID>{UNIT_ID}</common:UUID><common:name xml:lang="en">Units of mass</common:name></dataSetInformation><quantitativeReference><referenceToReferenceUnit>0</referenceToReferenceUnit></quantitativeReference></unitGroupInformation><units><unit dataSetInternalID="0"><name>kg</name><meanValue>1</meanValue></unit></units></unitGroupDataSet>"#
        ),
    )
    .unwrap();
    fs::write(
        root.join(format!("flowproperties/{PROPERTY_ID}.xml")),
        format!(
            r#"<flowPropertyDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><flowPropertiesInformation><dataSetInformation><common:UUID>{PROPERTY_ID}</common:UUID><common:name xml:lang="en">Mass</common:name></dataSetInformation><quantitativeReference><referenceToReferenceUnitGroup refObjectId="{UNIT_ID}"><common:shortDescription xml:lang="en">Units of mass</common:shortDescription></referenceToReferenceUnitGroup></quantitativeReference></flowPropertiesInformation></flowPropertyDataSet>"#
        ),
    )
    .unwrap();
    for (id, name) in FLOW_IDS.into_iter().zip(["Mercury", "Arsenic", "COD"]) {
        fs::write(
            root.join(format!("flows/{id}.xml")),
            format!(
                r#"<flowDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><flowInformation><dataSetInformation><common:UUID>{id}</common:UUID><name><baseName xml:lang="en">{name}</baseName></name><classificationInformation><common:elementaryFlowCategorization><common:category level="0">Emissions</common:category><common:category level="1">Emissions to water</common:category><common:category level="2">Emissions to water, unspecified</common:category></common:elementaryFlowCategorization></classificationInformation></dataSetInformation><quantitativeReference><referenceToReferenceFlowProperty>0</referenceToReferenceFlowProperty></quantitativeReference></flowInformation><modellingAndValidation><LCIMethod><typeOfDataSet>Elementary flow</typeOfDataSet></LCIMethod></modellingAndValidation><flowProperties><flowProperty dataSetInternalID="0"><referenceToFlowPropertyDataSet refObjectId="{PROPERTY_ID}"><common:shortDescription xml:lang="en">Mass</common:shortDescription></referenceToFlowPropertyDataSet><meanValue>1</meanValue></flowProperty></flowProperties></flowDataSet>"#
            ),
        )
        .unwrap();
    }
    let exchanges = FLOW_IDS.into_iter().zip([("0", "Mercury"), ("4", "Arsenic"), ("5", "COD")])
        .fold(String::new(), |mut output, (id, (internal, name))| {
            write!(
                output,
                r#"<exchange dataSetInternalID="{internal}"><referenceToFlowDataSet refObjectId="{id}" uri="../flows/{id}.xml"><common:shortDescription xml:lang="en">{name}</common:shortDescription></referenceToFlowDataSet><exchangeDirection>Output</exchangeDirection><meanAmount>0.001</meanAmount><dataDerivationTypeStatus>Calculated</dataDerivationTypeStatus><generalComment xml:lang="en">{name} is conditional on k = 1.</generalComment><generalComment xml:lang="zh">{name} 按 k = 1 的条件计算。</generalComment></exchange>"#
            ).unwrap();
            output
        });
    fs::write(
        root.join(format!("processes/{PROCESS_ID}.xml")),
        format!(
            r#"<processDataSet xmlns:common="http://lca.jrc.it/ILCD/Common"><processInformation><dataSetInformation><common:UUID>{PROCESS_ID}</common:UUID><name><baseName xml:lang="en">Mine-water coefficient reference</baseName><baseName xml:lang="zh">矿井水污染物系数参考</baseName></name><common:generalComment xml:lang="en">Conditional coefficients, not a complete treatment inventory.</common:generalComment><common:generalComment xml:lang="zh">条件性系数，不是完整的处理清单。</common:generalComment></dataSetInformation><quantitativeReference type="Other parameter"><functionalUnitOrOther xml:lang="en">1 tonne unwashed raw coal</functionalUnitOrOther><functionalUnitOrOther xml:lang="zh">1 吨未经洗选的原煤</functionalUnitOrOther></quantitativeReference><technology><technologyDescriptionAndIncludedProcesses xml:lang="en">Source-row treatment assumptions only.</technologyDescriptionAndIncludedProcesses><technologicalApplicability xml:lang="en">High-water surface mining only.</technologicalApplicability><technologicalApplicability xml:lang="zh">仅适用于高富水露天煤矿。</technologicalApplicability></technology></processInformation><modellingAndValidation><LCIMethodAndAllocation/><dataSourcesTreatmentAndRepresentativeness><referenceToDataSource type="source data set" refObjectId="{SOURCE_ID}" uri="../sources/{SOURCE_ID}.xml"><common:shortDescription xml:lang="en">Draft row, primary archive unverified</common:shortDescription></referenceToDataSource><useAdviceForDataSet xml:lang="en">Do not use as a measured site inventory.</useAdviceForDataSet><useAdviceForDataSet xml:lang="zh">不可用作现场实测清单。</useAdviceForDataSet></dataSourcesTreatmentAndRepresentativeness></modellingAndValidation><exchanges>{exchanges}</exchanges></processDataSet>"#
        ),
    )
    .unwrap();
}

fn request(source: &Path, output: &Path) -> ImportRequest {
    ImportRequest {
        source: source.to_path_buf(),
        requested_format: Some(SourceFormat::Ilcd),
        output_dir: output.to_path_buf(),
        target: ImportTarget::Both,
        write_mapping: false,
        write_process_bundles: true,
        cancellation: CancellationToken::default(),
        memory_budget: MemoryBudget::new(32 * 1024 * 1024),
        queue_capacity: 2,
        max_entry_bytes: 1024 * 1024,
        max_issue_bytes: 64 * 1024,
    }
}

fn import_both(source: &Path, output: &Path) -> tidas_import::ImportExecutionReportV1 {
    run_import(&request(source, output)).unwrap()
}

fn localized(value: &Value, language: &str) -> String {
    let items = value
        .as_array()
        .map_or_else(|| vec![value], |items| items.iter().collect());
    items
        .into_iter()
        .find(|item| item["@xml:lang"] == language)
        .and_then(|item| item["#text"].as_str())
        .unwrap()
        .to_owned()
}

fn assert_process_evidence(first: &Path) {
    let process: Value = serde_json::from_slice(
        &fs::read(first.join(format!("tidas/processes/{PROCESS_ID}.json"))).unwrap(),
    )
    .unwrap();
    let process = &process["processDataSet"];
    let information = &process["processInformation"];
    assert_eq!(
        localized(&information["dataSetInformation"]["name"]["baseName"], "zh"),
        "矿井水污染物系数参考"
    );
    assert_eq!(
        localized(
            &information["dataSetInformation"]["common:generalComment"],
            "en"
        ),
        "Conditional coefficients, not a complete treatment inventory."
    );
    assert_eq!(
        localized(
            &information["technology"]["technologicalApplicability"],
            "zh"
        ),
        "仅适用于高富水露天煤矿。"
    );
    assert_eq!(
        information["quantitativeReference"]["@type"],
        "Other parameter"
    );
    assert!(
        information["quantitativeReference"]
            .get("referenceToReferenceFlow")
            .is_none()
    );
    let sources = &process["modellingAndValidation"]["dataSourcesTreatmentAndRepresentativeness"];
    assert_eq!(sources["referenceToDataSource"]["@refObjectId"], SOURCE_ID);
    assert_eq!(sources["referenceToDataSource"]["@version"], "01.00.000");
    assert_eq!(
        sources["referenceToDataSource"]["@uri"],
        format!("../sources/{SOURCE_ID}.json")
    );
    assert_eq!(
        sources["common:other"]["tidasimport:sourceTrace"]["payload"]["originalIlcdReferences"]["@uri"],
        format!("../sources/{SOURCE_ID}.xml")
    );
    assert_eq!(
        localized(&sources["useAdviceForDataSet"], "zh"),
        "不可用作现场实测清单。"
    );
    let exchanges = process["exchanges"]["exchange"].as_array().unwrap();
    assert_eq!(
        exchanges
            .iter()
            .map(|item| item["@dataSetInternalID"].as_str().unwrap())
            .collect::<Vec<_>>(),
        ["0", "4", "5"]
    );
    for (exchange, name) in exchanges.iter().zip(["Mercury", "Arsenic", "COD"]) {
        assert_eq!(exchange["dataDerivationTypeStatus"], "Calculated");
        assert_eq!(
            localized(&exchange["generalComment"], "en"),
            format!("{name} is conditional on k = 1.")
        );
        assert_eq!(
            localized(&exchange["generalComment"], "zh"),
            format!("{name} 按 k = 1 的条件计算。")
        );
    }
}

fn assert_source_projection(first: &Path) {
    let source_json: Value = serde_json::from_slice(
        &fs::read(first.join(format!("tidas/sources/{SOURCE_ID}.json"))).unwrap(),
    )
    .unwrap();
    let source_information =
        &source_json["sourceDataSet"]["sourceInformation"]["dataSetInformation"];
    assert_eq!(
        localized(&source_information["common:shortName"], "zh"),
        "矿井水系数档案"
    );
    assert_eq!(
        source_information["sourceCitation"],
        "Source table, draft edition"
    );
    assert_eq!(
        localized(&source_information["sourceDescriptionOrComment"], "en"),
        "The primary archive has not been authenticated."
    );

    let projected_process =
        fs::read_to_string(first.join(format!("ilcd/data/processes/{PROCESS_ID}.xml"))).unwrap();
    assert!(projected_process.contains("矿井水污染物系数参考"));
    assert!(projected_process.contains("不可用作现场实测清单。"));
    assert!(projected_process.contains(&format!("refObjectId=\"{SOURCE_ID}\"")));
    for internal_id in ["0", "4", "5"] {
        assert!(projected_process.contains(&format!("dataSetInternalID=\"{internal_id}\"")));
    }
    let projected_source =
        fs::read_to_string(first.join(format!("ilcd/data/sources/{SOURCE_ID}.xml"))).unwrap();
    assert!(projected_source.contains("原始档案尚未核验。"));
    assert!(projected_source.contains("矿井水系数档案"));
}

fn assert_review_evidence_survives_reverse_conversion(first: &Path, recovered: &Path) {
    let original_process: Value = serde_json::from_slice(
        &fs::read(first.join(format!("tidas/processes/{PROCESS_ID}.json"))).unwrap(),
    )
    .unwrap();
    let reversed_process: Value = serde_json::from_slice(
        &fs::read(recovered.join(format!("data/data/processes/{PROCESS_ID}.json"))).unwrap(),
    )
    .unwrap();
    for pointer in [
        "/processDataSet/processInformation/dataSetInformation/name/baseName",
        "/processDataSet/processInformation/dataSetInformation/common:generalComment",
        "/processDataSet/processInformation/quantitativeReference",
        "/processDataSet/processInformation/technology",
        "/processDataSet/modellingAndValidation/dataSourcesTreatmentAndRepresentativeness/referenceToDataSource",
        "/processDataSet/modellingAndValidation/dataSourcesTreatmentAndRepresentativeness/useAdviceForDataSet",
        "/processDataSet/exchanges/exchange",
    ] {
        assert_eq!(
            original_process.pointer(pointer),
            reversed_process.pointer(pointer),
            "{pointer}"
        );
    }
    let original_source: Value = serde_json::from_slice(
        &fs::read(first.join(format!("tidas/sources/{SOURCE_ID}.json"))).unwrap(),
    )
    .unwrap();
    let reversed_source: Value = serde_json::from_slice(
        &fs::read(recovered.join(format!("data/data/sources/{SOURCE_ID}.json"))).unwrap(),
    )
    .unwrap();
    assert_eq!(original_source, reversed_source);
}

#[test]
fn bilingual_ilcd_evidence_survives_import_projection_and_round_trip() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source");
    write_fixture(&source);
    let first = directory.path().join("first");
    let report = import_both(&source, &first);
    assert_eq!(report.tidas_validation_issue_count, 0);
    assert_eq!(report.ilcd_validation_issue_count, Some(0));
    assert_eq!(
        report.process_bundles.unwrap().unresolved_reference_count,
        0
    );

    assert_process_evidence(&first);
    assert_source_projection(&first);

    // This is the actual eILCD -> TIDAS conversion path. The separately
    // tracked conversion defect (#224) still prevents claiming that every
    // reversed Process field passes independent TIDAS schema validation.
    let recovered = directory.path().join("recovered");
    convert_directory(&ConversionRequest {
        input_dir: first.join("ilcd"),
        output_dir: recovered.clone(),
        direction: ConversionDirection::IlcdToTidas,
        cancellation: CancellationToken::default(),
        memory_budget: MemoryBudget::new(32 * 1024 * 1024),
        queue_capacity: 2,
        progress: None,
    })
    .unwrap();
    assert_review_evidence_survives_reverse_conversion(&first, &recovered);

    let second = directory.path().join("second");
    let repeated = import_both(&source, &second);
    assert_eq!(
        report.tidas_package.output_tree_sha256,
        repeated.tidas_package.output_tree_sha256
    );
    assert_eq!(
        report.ilcd_conversion.unwrap().output_tree_sha256,
        repeated.ilcd_conversion.unwrap().output_tree_sha256
    );

    let source_path = source.join(format!("sources/{SOURCE_ID}.xml"));
    let changed = fs::read_to_string(&source_path).unwrap().replace(
        "The primary archive has not been authenticated.",
        "The primary archive has not been authenticated; this row is restricted.",
    );
    fs::write(source_path, changed).unwrap();
    let revised = import_both(&source, &directory.path().join("revised"));
    assert_ne!(
        report.tidas_package.output_tree_sha256,
        revised.tidas_package.output_tree_sha256
    );
}

#[test]
fn missing_ilcd_source_ref_fails_without_publishing_a_generic_substitute() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source");
    write_fixture(&source);
    fs::remove_file(source.join(format!("sources/{SOURCE_ID}.xml"))).unwrap();
    let output = directory.path().join("output");
    let error = run_import(&request(&source, &output)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("absent from the imported package")
    );
    assert!(!output.exists());
}

#[test]
fn mismatched_ilcd_source_version_fails_before_publication() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source");
    write_fixture(&source);
    let process = source.join(format!("processes/{PROCESS_ID}.xml"));
    let original = fs::read_to_string(&process).unwrap();
    let reference = format!("refObjectId=\"{SOURCE_ID}\" uri=");
    let revised = original.replace(
        &reference,
        &format!("refObjectId=\"{SOURCE_ID}\" version=\"02.00.000\" uri="),
    );
    assert_ne!(original, revised);
    fs::write(process, revised).unwrap();
    let output = directory.path().join("output");
    let error = run_import(&request(&source, &output)).unwrap_err();
    assert!(error.to_string().contains("imported version is 01.00.000"));
    assert!(!output.exists());
}

#[test]
fn untyped_textual_ilcd_reference_never_promotes_first_pollutant() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source");
    write_fixture(&source);
    let process = source.join(format!("processes/{PROCESS_ID}.xml"));
    let original = fs::read_to_string(&process).unwrap();
    let revised = original.replace(
        "quantitativeReference type=\"Other parameter\"",
        "quantitativeReference",
    );
    assert_ne!(original, revised);
    fs::write(process, revised).unwrap();
    let output = directory.path().join("output");
    let error = run_import(&request(&source, &output)).unwrap_err();
    assert!(error.to_string().contains("error issue"));
    assert!(!output.exists());
}

#[test]
fn local_source_uri_must_point_to_the_referenced_source_uuid() {
    let directory = tempdir().unwrap();
    let source = directory.path().join("source");
    write_fixture(&source);
    let first_source = source.join(format!("sources/{SOURCE_ID}.xml"));
    fs::write(
        source.join(format!("sources/{OTHER_SOURCE_ID}.xml")),
        fs::read_to_string(first_source)
            .unwrap()
            .replace(SOURCE_ID, OTHER_SOURCE_ID),
    )
    .unwrap();
    let process = source.join(format!("processes/{PROCESS_ID}.xml"));
    let original = fs::read_to_string(&process).unwrap();
    let local_uri = format!("uri=\"../sources/{SOURCE_ID}.xml\"");
    let wrong_uri = format!("uri=\"../sources/{OTHER_SOURCE_ID}.xml\"");
    let mismatched = original.replace(&local_uri, &wrong_uri);
    assert_ne!(original, mismatched);
    fs::write(&process, mismatched).unwrap();
    let output = directory.path().join("rejected");
    let error = run_import(&request(&source, &output)).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("local URI pointing to a different file")
    );
    assert!(!output.exists());

    fs::write(
        process,
        original.replace(&local_uri, "uri=\"https://example.org/archive\""),
    )
    .unwrap();
    let external = directory.path().join("external-uri");
    let report = import_both(&source, &external);
    assert_eq!(report.tidas_validation_issue_count, 0);
    assert_eq!(report.ilcd_validation_issue_count, Some(0));
}
