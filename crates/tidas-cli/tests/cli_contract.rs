use std::fs;
use std::process::Command;

use tidas_contracts::{CommandNameV1, DiagnosticV1, OperationReportV1};

const TIDAS_ENVIRONMENT: &[&str] = &[
    "TIDAS_CONFIG",
    "TIDAS_LOG",
    "TIDAS_PROGRESS",
    "TIDAS_MEMORY_BUDGET_MIB",
    "TIDAS_QUEUE_CAPACITY",
    "TIDAS_DATABASE_URL",
    "TIDAS_S3_ACCESS_KEY_ID",
    "TIDAS_S3_SECRET_ACCESS_KEY",
    "TIDAS_S3_SESSION_TOKEN",
    "TIDAS_S3_BUCKET",
    "TIDAS_S3_REGION",
    "TIDAS_S3_ENDPOINT",
    "TIDAS_S3_PREFIX",
];

fn tidas() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tidas"));
    for variable in TIDAS_ENVIRONMENT {
        command.env_remove(variable);
    }
    command
}

fn json_output(args: &[&str]) -> (std::process::Output, serde_json::Value) {
    let output = tidas().args(args).output().unwrap();
    let payload = serde_json::from_slice(&output.stdout).unwrap();
    (output, payload)
}

#[test]
fn help_exposes_the_unified_command_and_runtime_contract() {
    let output = tidas().arg("--help").output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for command in [
        "convert", "import", "export", "validate", "release", "ruleset", "version",
    ] {
        assert!(
            stdout.contains(command),
            "{command} missing from:\n{stdout}"
        );
    }
    for option in [
        "--format",
        "--report",
        "--config",
        "--log-level",
        "--progress",
        "--memory-budget-mib",
        "--queue-capacity",
        "--completion",
    ] {
        assert!(stdout.contains(option), "{option} missing from:\n{stdout}");
    }
    assert!(stdout.contains("No configuration file is loaded implicitly"));
    assert!(stdout.contains("Stdout contains only"));
    for legacy in [
        "\n  help ",
        "tidas-convert",
        "tidas-import",
        "tidas-export",
        "tidas-validate",
        "tidas-release-tool",
    ] {
        assert!(!stdout.contains(legacy), "{legacy} leaked into:\n{stdout}");
    }
}

#[test]
fn every_product_command_has_discoverable_help() {
    for command in [
        "convert", "import", "export", "validate", "release", "ruleset", "version",
    ] {
        let output = tidas().args([command, "--help"]).output().unwrap();
        assert!(output.status.success(), "{command}");
        assert!(output.stderr.is_empty(), "{command}");
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(stdout.contains("Usage:"), "{stdout}");
        assert!(stdout.contains(&format!(" {command}")), "{stdout}");
    }
}

#[test]
fn version_json_is_stable_parseable_and_context_complete() {
    let first = tidas()
        .args(["version", "--format", "json"])
        .output()
        .unwrap();
    let second = tidas()
        .args(["version", "--format", "json"])
        .output()
        .unwrap();
    assert!(first.status.success());
    assert!(first.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);
    let typed: OperationReportV1 = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(typed.command, CommandNameV1::Version);
    let payload: serde_json::Value = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(payload["schema_version"], "tidas.operation-report.v1");
    assert_eq!(
        payload["invocation"]["schema_version"],
        "tidas.invocation-context.v1"
    );
    assert_eq!(payload["exit_class"], "success");
    assert_eq!(payload["invocation"]["config_source"], "none");
    assert_eq!(
        payload["invocation"]["memory_budget_bytes"],
        512 * 1024 * 1024
    );
    assert_eq!(payload["invocation"]["queue_capacity"], 256);
    assert_eq!(
        payload["invocation"]["input_policy"],
        "explicit-path-or-dash"
    );
    assert_eq!(payload["invocation"]["report_destination"], "stdout");
    assert_eq!(payload["invocation"]["diagnostic_destination"], "stderr");
    assert!(payload["summary"]["asset_count"].as_u64().unwrap() >= 79);
    assert_eq!(
        payload["summary"]["release_report_schema"],
        "tidas.release-report.v1"
    );
}

#[test]
fn cli_options_override_environment_and_environment_overrides_defaults() {
    let output = tidas()
        .env("TIDAS_CONFIG", "environment.toml")
        .env("TIDAS_LOG", "info")
        .env("TIDAS_PROGRESS", "always")
        .env("TIDAS_MEMORY_BUDGET_MIB", "256")
        .env("TIDAS_QUEUE_CAPACITY", "32")
        .args([
            "version",
            "--format",
            "json",
            "--config",
            "command.toml",
            "--log-level",
            "debug",
            "--progress",
            "never",
            "--memory-budget-mib",
            "384",
            "--queue-capacity",
            "48",
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let invocation = &payload["invocation"];
    assert_eq!(invocation["config_source"], "cli");
    assert_eq!(invocation["config_path"], "command.toml");
    assert_eq!(invocation["log_level"], "debug");
    assert_eq!(invocation["progress_mode"], "never");
    assert_eq!(invocation["progress_enabled"], false);
    assert_eq!(invocation["memory_budget_bytes"], 384 * 1024 * 1024);
    assert_eq!(invocation["queue_capacity"], 48);

    let (_, environment_payload) = json_output(&["version", "--format", "json"]);
    assert_eq!(environment_payload["invocation"]["config_source"], "none");
}

#[test]
fn environment_config_is_explicitly_reported() {
    let output = tidas()
        .env("TIDAS_CONFIG", "environment.toml")
        .args(["version", "--format", "json"])
        .output()
        .unwrap();
    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["invocation"]["config_source"], "environment");
    assert_eq!(payload["invocation"]["config_path"], "environment.toml");
}

#[test]
fn report_file_keeps_stdout_clean_and_records_the_destination() {
    let directory = tempfile::tempdir().unwrap();
    let report_path = directory.path().join("version.json");
    let output = tidas()
        .args([
            "version",
            "--format",
            "json",
            "--report",
            report_path.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("wrote version report")
    );
    let payload: serde_json::Value =
        serde_json::from_slice(&fs::read(report_path).unwrap()).unwrap();
    assert_eq!(payload["invocation"]["report_destination"], "file");
}

#[test]
fn completion_scripts_are_deterministic_and_do_not_add_a_product_command() {
    for shell in ["bash", "elvish", "fish", "powershell", "zsh"] {
        let first = tidas().args(["--completion", shell]).output().unwrap();
        let second = tidas().args(["--completion", shell]).output().unwrap();
        assert!(first.status.success(), "{shell}");
        assert!(first.stderr.is_empty(), "{shell}");
        assert_eq!(first.stdout, second.stdout, "{shell}");
        assert!(!first.stdout.is_empty(), "{shell}");
    }
}

#[test]
fn release_is_native_and_requires_a_discoverable_action() {
    let output = tidas()
        .args(["release", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("requires a subcommand"), "{stderr}");
    assert!(!stderr.contains("python"), "{stderr}");

    let help = tidas().args(["release", "--help"]).output().unwrap();
    assert!(help.status.success());
    let stdout = String::from_utf8(help.stdout).unwrap();
    for action in [
        "build-packages",
        "convert-ilcd",
        "semantic-roundtrip",
        "validate-closure",
        "validate-ilcd",
        "validate-tidas",
    ] {
        assert!(stdout.contains(action), "{action} missing from:\n{stdout}");
    }
    assert!(stdout.contains("publish four ZIPs"));
    assert!(!stdout.contains("tidas-release-tool"));
}

#[test]
fn release_action_dispatches_to_the_native_versioned_domain_report() {
    let directory = tempfile::tempdir().unwrap();
    let (output, payload) = json_output(&[
        "release",
        "validate-tidas",
        "--input-dir",
        directory.path().to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    assert_eq!(payload["command"], "release");
    assert_eq!(
        payload["summary"]["release"]["schema_version"],
        "tidas.release-report.v1"
    );
    assert_eq!(payload["summary"]["release"]["action"], "validate-tidas");
    assert_eq!(payload["summary"]["release"]["ok"], true);
}

#[test]
fn release_validation_data_findings_use_the_data_issues_exit_class() {
    let directory = tempfile::tempdir().unwrap();
    fs::create_dir(directory.path().join("sources")).unwrap();
    fs::write(directory.path().join("sources/bad.json"), b"{not-json").unwrap();
    let (output, payload) = json_output(&[
        "release",
        "validate-tidas",
        "--input-dir",
        directory.path().to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(output.stderr.is_empty());
    assert_eq!(payload["exit_class"], "data-issues");
    assert_eq!(payload["summary"]["release"]["ok"], false);
    assert_eq!(
        payload["summary"]["release"]["validation"]["summary"]["issue_count"],
        1
    );
}

#[test]
fn export_help_exposes_native_bounded_and_credential_safe_contract() {
    let output = tidas().args(["export", "--help"]).output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for value in [
        "--output",
        "--target",
        "--external-docs-bucket",
        "--s3-region",
        "--s3-endpoint",
        "--s3-prefix",
        "--skip-external-docs",
        "--network-timeout-seconds",
        "TIDAS_S3_ACCESS_KEY_ID",
        "bounded work queue",
        "atomic",
    ] {
        assert!(stdout.contains(value), "{value} missing from:\n{stdout}");
    }
    assert!(stdout.contains("TIDAS_DATABASE_URL"));
    assert!(!stdout.contains("--database-url"));
    assert!(!stdout.contains("DB_PASSWORD"));
    assert!(!stdout.contains("--aws-secret-access-key"));
}

#[test]
fn export_rejects_missing_storage_credentials_without_connecting_or_leaking_values() {
    let directory = tempfile::tempdir().unwrap();
    let output = tidas()
        .env(
            "TIDAS_DATABASE_URL",
            "postgresql://fixture-user:super-secret@127.0.0.1:1/fixture",
        )
        .args([
            "export",
            "--output",
            directory.path().join("fixture.zip").to_str().unwrap(),
            "--external-docs-bucket",
            "fixture",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(64));
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("missing_export_credentials"));
    assert!(!text.contains("super-secret"));
    assert!(!text.contains("fixture-user"));
}

#[test]
fn export_requires_database_credentials_as_an_environment_only_secret() {
    let directory = tempfile::tempdir().unwrap();
    let (output, payload) = json_output(&[
        "export",
        "--output",
        directory.path().join("fixture.zip").to_str().unwrap(),
        "--skip-external-docs",
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(64));
    assert_eq!(
        payload["diagnostics"][0]["code"],
        "missing_database_credentials"
    );
    assert!(!directory.path().join("fixture.zip").exists());
}

#[test]
fn export_database_failures_use_io_and_never_echo_connection_secrets() {
    let directory = tempfile::tempdir().unwrap();
    let output = tidas()
        .env(
            "TIDAS_DATABASE_URL",
            "postgresql://fixture-user:super-secret@127.0.0.1:1/fixture?sslmode=disable",
        )
        .args([
            "export",
            "--output",
            directory.path().join("fixture.zip").to_str().unwrap(),
            "--skip-external-docs",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(74));
    assert!(output.stderr.is_empty());
    let text = String::from_utf8(output.stdout).unwrap();
    assert!(text.contains("export_io_failed"));
    assert!(!text.contains("super-secret"));
    assert!(!text.contains("fixture-user"));
}

#[test]
fn import_help_exposes_supported_formats_outputs_and_recovery() {
    let output = tidas().args(["import", "--help"]).output().unwrap();
    assert!(output.status.success());
    assert!(output.stderr.is_empty());
    let stdout = String::from_utf8(output.stdout).unwrap();
    for value in [
        "ecospold1",
        "ecospold2",
        "simapro-csv",
        "openlca-jsonld",
        "openlca-process-xlsx",
        "ilcd",
        "--output",
        "--target",
        "--write-mapping",
        "--no-process-bundles",
        "--fail-on-warning",
        "--max-entry-mib",
        ".zolca",
        "tidas validate",
    ] {
        assert!(stdout.contains(value), "{value} missing from:\n{stdout}");
    }
}

#[test]
fn import_is_native_atomic_validated_and_actionable() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input.csv");
    let output = directory.path().join("output");
    fs::write(
        &input,
        b"{SimaPro 9.5}\n\nProcess\n\nProcess name\nSteel\n\nProducts\nSteel | production route | GLO;kg;1\n\nEnd\n",
    )
    .unwrap();
    let run = || {
        tidas()
            .args([
                "import",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--write-mapping",
                "--format",
                "json",
            ])
            .output()
            .unwrap()
    };
    let first = run();
    let second = run();
    assert!(
        first.status.success(),
        "first import failed: status={:?}, stdout={}, stderr={}",
        first.status.code(),
        String::from_utf8_lossy(&first.stdout),
        String::from_utf8_lossy(&first.stderr)
    );
    assert!(
        second.status.success(),
        "second import failed: status={:?}, stdout={}, stderr={}",
        second.status.code(),
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(first.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);
    let report: OperationReportV1 = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report.command, CommandNameV1::Import);
    assert_eq!(report.summary["import"]["detected_format"], "simapro-csv");
    assert_eq!(report.summary["import"]["target"], "tidas");
    assert_eq!(report.summary["import"]["tidas_validation_issue_count"], 0);
    assert_eq!(
        report.artifacts[3].sha256.as_deref(),
        report.summary["import"]["tidas_package"]["output_tree_sha256"].as_str()
    );
    assert!(report.next_actions[0].contains("tidas validate"));
    assert!(output.join("tidas/processes").is_dir());
    assert!(output.join("process-bundles/index.json").is_file());
    assert!(output.join("mapping.csv.gz").is_file());
}

#[test]
fn import_warning_policy_and_invalid_sources_have_stable_exit_classes() {
    let directory = tempfile::tempdir().unwrap();
    let source = directory.path().join("input.csv");
    let output = directory.path().join("output");
    fs::write(
        &source,
        b"{SimaPro 9.5}\n\nProcess\n\nProcess name\nSteel\n\nProducts\nSteel | production route | GLO;kg;1\n\nEnd\n",
    )
    .unwrap();
    let (warning_output, warning_report) = json_output(&[
        "import",
        source.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--fail-on-warning",
        "--format",
        "json",
    ]);
    assert_eq!(
        warning_output.status.code(),
        Some(2),
        "warning import returned: stdout={}, stderr={}",
        String::from_utf8_lossy(&warning_output.stdout),
        String::from_utf8_lossy(&warning_output.stderr)
    );
    assert_eq!(warning_report["status"], "completed-with-issues");
    assert_eq!(warning_report["exit_class"], "data-issues");
    assert!(output.join("tidas").is_dir());

    let missing_facts = directory.path().join("missing-facts.csv");
    fs::write(
        &missing_facts,
        b"{SimaPro 9.5}\n\nProcess\n\nProcess name\nSteel\n\nProducts\nSteel;kg;1\n\nEnd\n",
    )
    .unwrap();
    let missing_output = directory.path().join("missing-output");
    let (preflight_output, preflight_report) = json_output(&[
        "import",
        missing_facts.to_str().unwrap(),
        "--output",
        missing_output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(preflight_output.status.code(), Some(2));
    assert_eq!(preflight_report["exit_class"], "data-issues");
    assert_eq!(
        preflight_report["diagnostics"][0]["code"],
        "import_preflight_failed"
    );
    assert!(
        preflight_report["diagnostics"][0]["message"]
            .as_str()
            .unwrap()
            .contains("CanonicalFlow.name.treatmentStandardsRoutes")
    );
    assert!(!missing_output.exists());

    let zolca = directory.path().join("database.zolca");
    fs::write(&zolca, b"SQLite format 3").unwrap();
    let zolca_output = directory.path().join("zolca-output");
    let (invalid_output, invalid_report) = json_output(&[
        "import",
        zolca.to_str().unwrap(),
        "--output",
        zolca_output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(invalid_output.status.code(), Some(2));
    assert_eq!(invalid_report["exit_class"], "data-issues");
    assert_eq!(
        invalid_report["diagnostics"][0]["code"],
        "import_source_invalid"
    );
    assert!(!zolca_output.exists());

    let missing = directory.path().join("missing.zip");
    let missing_output = directory.path().join("missing-output");
    let (missing_result, missing_report) = json_output(&[
        "import",
        missing.to_str().unwrap(),
        "--output",
        missing_output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(missing_result.status.code(), Some(2));
    assert_eq!(missing_report["exit_class"], "data-issues");
    assert!(!missing_output.exists());

    let malformed = directory.path().join("malformed.csv");
    fs::write(&malformed, b"{SimaPro 9.5}\n").unwrap();
    let malformed_output = directory.path().join("malformed-output");
    let (malformed_result, malformed_report) = json_output(&[
        "import",
        malformed.to_str().unwrap(),
        "--output",
        malformed_output.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(malformed_result.status.code(), Some(2));
    assert_eq!(malformed_report["exit_class"], "data-issues");
    assert!(!malformed_output.exists());
}

#[test]
fn convert_is_native_deterministic_atomic_and_actionable() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    let output = directory.path().join("output");
    fs::create_dir_all(input.join("processes")).unwrap();
    fs::write(
        input.join("processes/process.json"),
        r##"{"processDataSet":{"@version":"1.1","name":{"baseName":[{"@xml:lang":"en","#text":"Steel & circularity"},{"@xml:lang":"zh","#text":"钢"}]},"reference":null}}"##,
    )
    .unwrap();
    fs::write(input.join("README.txt"), b"preserved\n").unwrap();

    let run = || {
        tidas()
            .args([
                "convert",
                input.to_str().unwrap(),
                "--output",
                output.to_str().unwrap(),
                "--to",
                "ilcd",
                "--format",
                "json",
            ])
            .output()
            .unwrap()
    };
    let first = run();
    let second = run();
    assert!(first.status.success());
    assert!(second.status.success());
    assert!(first.stderr.is_empty());
    assert_eq!(first.stdout, second.stdout);

    let report: OperationReportV1 = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report.command, CommandNameV1::Convert);
    assert_eq!(report.summary["conversion"]["direction"], "tidas-to-ilcd");
    assert_eq!(report.summary["conversion"]["converted_file_count"], 1);
    assert_eq!(report.summary["conversion"]["copied_file_count"], 1);
    assert!(
        report.summary["conversion"]["asset_file_count"]
            .as_u64()
            .unwrap()
            > 0
    );
    assert_eq!(
        report.artifacts[0].sha256.as_deref(),
        report.summary["conversion"]["output_tree_sha256"].as_str()
    );
    assert!(report.next_actions[0].contains("tidas validate"));
    assert!(output.join("data/processes/process.xml").is_file());
    assert_eq!(
        fs::read(output.join("data/README.txt")).unwrap(),
        b"preserved\n"
    );
    assert!(output.join("schemas/ILCD_ProcessDataSet.xsd").is_file());
}

#[test]
fn invalid_conversion_data_returns_data_issues_without_publishing_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    let output = directory.path().join("output");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("broken.json"), b"{").unwrap();
    let (result, report) = json_output(&[
        "convert",
        input.to_str().unwrap(),
        "--output",
        output.to_str().unwrap(),
        "--to",
        "ilcd",
        "--format",
        "json",
    ]);
    assert_eq!(result.status.code(), Some(2));
    assert_eq!(report["command"], "convert");
    assert_eq!(report["exit_class"], "data-issues");
    assert_eq!(report["diagnostics"][0]["code"], "conversion_input_invalid");
    assert!(!output.exists());
}

#[test]
fn explicit_conversion_progress_uses_stderr_and_keeps_json_stdout_clean() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    let output = directory.path().join("output");
    fs::create_dir_all(&input).unwrap();
    fs::write(input.join("document.json"), br#"{"root":null}"#).unwrap();
    let result = tidas()
        .args([
            "convert",
            input.to_str().unwrap(),
            "--output",
            output.to_str().unwrap(),
            "--to",
            "ilcd",
            "--progress",
            "always",
            "--format",
            "json",
        ])
        .output()
        .unwrap();
    assert!(result.status.success());
    let report: OperationReportV1 = serde_json::from_slice(&result.stdout).unwrap();
    assert_eq!(report.command, CommandNameV1::Convert);
    let stderr = String::from_utf8(result.stderr).unwrap();
    assert!(stderr.contains("convert phase=started"));
    assert!(stderr.contains("convert phase=hashing"));
    assert!(stderr.contains("convert phase=completed"));
}

#[test]
fn ruleset_catalog_is_native_validated_and_queryable() {
    let (catalog_output, catalog) = json_output(&["ruleset", "--format", "json"]);
    assert!(catalog_output.status.success());
    assert_eq!(catalog["command"], "ruleset");
    assert_eq!(
        catalog["summary"]["ruleset_description"]["ruleset_count"],
        7
    );
    assert_eq!(
        catalog["summary"]["ruleset_description"]["ruleset_version"],
        "2026.05.23"
    );

    let (profile_output, profile) = json_output(&[
        "ruleset",
        "--id",
        "process-authoring/strict",
        "--format",
        "json",
    ]);
    assert!(profile_output.status.success());
    assert_eq!(profile["summary"]["ruleset_id"], "process-authoring/strict");
    assert!(
        profile["summary"]["rules"]
            .as_array()
            .unwrap()
            .iter()
            .any(|rule| rule["severity"] == "warning")
    );

    let (missing_output, missing) =
        json_output(&["ruleset", "--id", "missing/default", "--format", "json"]);
    assert_eq!(missing_output.status.code(), Some(64));
    assert_eq!(missing["diagnostics"][0]["code"], "unknown_ruleset");
}

#[test]
fn native_tidas_validation_streams_deterministic_issues_without_python() {
    let directory = tempfile::tempdir().unwrap();
    let package = directory.path().join("package");
    let sources = package.join("sources");
    fs::create_dir_all(&sources).unwrap();
    fs::write(sources.join("b.json"), "{").unwrap();
    fs::write(sources.join("a.json"), "{}").unwrap();
    let issues = directory.path().join("issues.jsonl");

    let run = || {
        tidas()
            .args([
                "validate",
                package.to_str().unwrap(),
                "--issues",
                issues.to_str().unwrap(),
                "--format",
                "json",
            ])
            .output()
            .unwrap()
    };
    let first = run();
    let first_issues = fs::read(&issues).unwrap();
    let second = run();
    let second_issues = fs::read(&issues).unwrap();

    assert_eq!(first.status.code(), Some(2));
    assert_eq!(second.status.code(), Some(2));
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first_issues, second_issues);
    assert!(first.stderr.is_empty());
    let report: OperationReportV1 = serde_json::from_slice(&first.stdout).unwrap();
    assert_eq!(report.command, CommandNameV1::Validate);
    assert_eq!(report.exit_class.code(), 2);
    assert_eq!(report.summary["validation"]["input_format"], "tidas-json");
    assert_eq!(report.summary["validation"]["document_count"], 2);
    assert!(
        report.summary["validation"]["issue_count"]
            .as_u64()
            .unwrap()
            >= 2
    );
    assert_eq!(report.artifacts.len(), 1);
    assert_eq!(report.artifacts[0].media_type, "application/x-ndjson");
}

#[test]
fn empty_native_tidas_package_is_a_complete_success() {
    let directory = tempfile::tempdir().unwrap();
    let (output, payload) = json_output(&[
        "validate",
        directory.path().to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert!(output.status.success());
    assert_eq!(payload["exit_class"], "success");
    assert_eq!(payload["summary"]["validation"]["document_count"], 0);
    assert_eq!(payload["summary"]["validation"]["issue_count"], 0);
    assert_eq!(
        payload["summary"]["eilcd_projection_validation"]["ok"],
        true
    );
    assert_eq!(payload["summary"]["semantic_roundtrip"]["ok"], true);
}

#[test]
fn schema_only_is_explicitly_weaker_than_complete_tidas_validation() {
    let directory = tempfile::tempdir().unwrap();
    let (output, payload) = json_output(&[
        "validate",
        directory.path().to_str().unwrap(),
        "--schema-only",
        "--format",
        "json",
    ]);
    assert!(output.status.success());
    assert_eq!(payload["summary"]["validation"]["ok"], true);
    assert!(
        payload["summary"]
            .get("eilcd_projection_validation")
            .is_none()
    );
    assert_eq!(payload["diagnostics"][0]["code"], "schema_only_validation");
    assert!(
        payload["next_actions"][0]
            .as_str()
            .unwrap()
            .contains("without --schema-only")
    );
}

#[test]
fn explicit_validation_progress_uses_stderr_and_keeps_json_stdout_clean() {
    let directory = tempfile::tempdir().unwrap();
    let output = tidas()
        .args([
            "validate",
            directory.path().to_str().unwrap(),
            "--progress",
            "always",
            "--format",
            "json",
        ])
        .output()
        .unwrap();

    assert!(output.status.success());
    let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(payload["exit_class"], "success");
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(stderr.contains("phase=started"));
    assert!(stderr.contains("phase=completed"));
    assert!(stderr.contains("documents=0/?"));
}

#[test]
fn validation_describe_and_batch_protocol_are_native_and_deterministic() {
    let directory = tempfile::tempdir().unwrap();
    let (describe_output, describe) = json_output(&["validate", "--describe", "--format", "json"]);
    assert!(describe_output.status.success());
    assert_eq!(
        describe["summary"]["validation_describe"]["protocols"][0],
        "document-validation-batch.v1"
    );
    assert_eq!(
        describe["summary"]["validation_describe"]["package"]["name"],
        "tidas"
    );

    let batch = directory.path().join("batch");
    fs::create_dir_all(batch.join("sources")).unwrap();
    fs::write(batch.join("sources/bad.json"), b"{}").unwrap();
    let manifest = directory.path().join("manifest.jsonl");
    fs::write(
        &manifest,
        "{\"document_key\":\"source:test:01.00.000\",\"category\":\"sources\",\"relative_path\":\"sources/bad.json\",\"content_sha256\":\"44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\"}\n",
    )
    .unwrap();
    let events = directory.path().join("events.jsonl");
    let run = || {
        json_output(&[
            "validate",
            batch.to_str().unwrap(),
            "--protocol",
            "document-validation-batch.v1",
            "--input-manifest",
            manifest.to_str().unwrap(),
            "--events",
            events.to_str().unwrap(),
            "--format",
            "json",
        ])
    };
    let (first_output, first_report) = run();
    let first_events = fs::read(&events).unwrap();
    let (second_output, second_report) = run();
    let second_events = fs::read(&events).unwrap();

    assert!(first_output.status.success());
    assert!(second_output.status.success());
    assert_eq!(first_report, second_report);
    assert_eq!(first_events, second_events);
    assert_eq!(
        first_report["summary"]["validation_batch_final"]["summary"]["error_count"],
        1
    );
    assert_eq!(
        first_report["summary"]["validation_batch_final"]["completed"],
        true
    );
    let event_types: Vec<_> = first_events
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<serde_json::Value>(line).unwrap()["type"].clone())
        .collect();
    assert_eq!(event_types, ["issue", "final"]);
}

#[test]
fn native_ilcd_validation_accepts_valid_assets_and_reports_schema_issues() {
    let directory = tempfile::tempdir().unwrap();
    fs::write(
        directory.path().join("valid.xml"),
        include_bytes!("../../../assets/eilcd/stylesheets/ILCDLocations_Reference.xml"),
    )
    .unwrap();
    let (valid_output, valid_payload) = json_output(&[
        "validate",
        directory.path().to_str().unwrap(),
        "--input-format",
        "ilcd-xml",
        "--format",
        "json",
    ]);
    assert!(valid_output.status.success());
    assert_eq!(
        valid_payload["summary"]["validation"]["input_format"],
        "ilcd-xml"
    );
    assert_eq!(valid_payload["summary"]["validation"]["document_count"], 1);

    fs::write(
        directory.path().join("invalid.xml"),
        br#"<ILCDLocations xmlns="http://lca.jrc.it/ILCD/Locations"/>"#,
    )
    .unwrap();
    let issues = directory.path().join("issues.jsonl");
    let (invalid_output, invalid_payload) = json_output(&[
        "validate",
        directory.path().to_str().unwrap(),
        "--input-format",
        "ilcd-xml",
        "--issues",
        issues.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(invalid_output.status.code(), Some(2));
    assert_eq!(invalid_payload["exit_class"], "data-issues");
    assert_eq!(invalid_payload["summary"]["validation"]["issue_count"], 1);
    let event: serde_json::Value = serde_json::from_slice(
        fs::read(&issues)
            .unwrap()
            .split(|byte| *byte == b'\n')
            .next()
            .unwrap(),
    )
    .unwrap();
    assert_eq!(event["issue"]["issue_code"], "ilcd_schema_error");
}

#[test]
fn malformed_usage_is_actionable_and_uses_the_usage_exit_class() {
    for args in [
        vec![],
        vec!["unknown"],
        vec!["--memory-budget-mib", "0", "version"],
        vec!["--queue-capacity", "0", "version"],
        vec!["--completion", "bash", "version"],
        vec!["--completion", "bash", "--report", "completion.txt"],
    ] {
        let output = tidas().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(64));
        let stderr = String::from_utf8(output.stderr).unwrap();
        assert!(stderr.contains("Usage:") || stderr.contains("try '--help'"));
    }
}

#[test]
fn io_failures_use_the_io_exit_class() {
    let directory = tempfile::tempdir().unwrap();
    let missing_parent = directory.path().join("missing").join("report.json");
    let output = tidas()
        .args([
            "version",
            "--format",
            "json",
            "--report",
            missing_parent.to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(74));
    assert!(output.stdout.is_empty());
    assert!(
        String::from_utf8(output.stderr)
            .unwrap()
            .contains("failed to render its report")
    );
}

#[test]
fn migration_parity_fixture_matches_the_rust_surface() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("fixtures/cli-parity-v1.json")).unwrap();
    assert_eq!(fixture["schema_version"], "tidas.cli-parity.v1");
    for case in fixture["command_cases"].as_array().unwrap() {
        let args: Vec<_> = case["rust_args"]
            .as_array()
            .unwrap()
            .iter()
            .map(|value| value.as_str().unwrap())
            .collect();
        let output = tidas().args(args).output().unwrap();
        let expected_exit_code =
            i32::try_from(case["expected_exit_code"].as_i64().unwrap()).unwrap();
        assert_eq!(
            output.status.code(),
            Some(expected_exit_code),
            "{}",
            case["name"]
        );
        if let Some(exit_class) = case["expected_exit_class"].as_str() {
            let payload: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
            assert_eq!(payload["exit_class"], exit_class, "{}", case["name"]);
            assert_eq!(
                payload["completeness"], case["expected_completeness"],
                "{}",
                case["name"]
            );
        }
    }
    for case in fixture["contract_cases"].as_array().unwrap() {
        let report = OperationReportV1::completed_with_issues(
            CommandNameV1::Validate,
            DiagnosticV1::new(
                case["diagnostic_code"].as_str().unwrap(),
                "The frozen Python oracle reported a data issue.",
            ),
        );
        let payload: serde_json::Value =
            serde_json::from_slice(&report.to_canonical_json_line().unwrap()).unwrap();
        assert_eq!(payload["command"], case["command"], "{}", case["name"]);
        assert_eq!(
            payload["status"], case["expected_status"],
            "{}",
            case["name"]
        );
        assert_eq!(
            payload["exit_class"], case["expected_exit_class"],
            "{}",
            case["name"]
        );
        assert_eq!(
            payload["completeness"], case["expected_completeness"],
            "{}",
            case["name"]
        );
        assert_eq!(
            report.exit_class.code(),
            u8::try_from(case["expected_exit_code"].as_u64().unwrap()).unwrap(),
            "{}",
            case["name"]
        );
    }
}

#[test]
fn reference_unit_conversion_uses_native_stdin_and_deterministic_report() {
    use std::io::Write;
    use std::process::Stdio;
    let request = include_bytes!("fixtures/flow-property-conversion-request.json");
    let mut child = tidas()
        .args(["convert", "-", "--to", "reference-unit", "--format", "json"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(request).unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    let conversion = &report["summary"]["flow_property_conversion"];
    assert_eq!(conversion["result"]["amount"], "2000");
    assert_eq!(conversion["reference"]["flow_property_internal_id"], "7");
    assert_eq!(conversion["reference"]["unit_name"], "kg");
    assert_eq!(conversion["round_trip"]["residual"], "0");
    assert_eq!(conversion["applicability"], "not_assessed");
    assert_eq!(conversion["documents"].as_array().unwrap().len(), 5);
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("request.json");
    fs::write(&input, request).unwrap();
    let (second, other) = json_output(&[
        "convert",
        input.to_str().unwrap(),
        "--to",
        "reference-unit",
        "--format",
        "json",
    ]);
    assert!(second.status.success());
    assert_eq!(other["summary"]["flow_property_conversion"], *conversion);
}

#[test]
fn reference_unit_conversion_rejects_formula_and_artifact_output() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("request.json");
    let mut request: serde_json::Value = serde_json::from_slice(include_bytes!(
        "fixtures/flow-property-conversion-request.json"
    ))
    .unwrap();
    request["source"]["formula"] = serde_json::json!("volume * density");
    fs::write(&input, serde_json::to_vec(&request).unwrap()).unwrap();
    let (output, report) = json_output(&[
        "convert",
        input.to_str().unwrap(),
        "--to",
        "reference-unit",
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(2));
    assert!(report["summary"].get("flow_property_conversion").is_none());
    assert!(
        String::from_utf8(output.stdout)
            .unwrap()
            .contains("formula_requires_evaluation")
    );
    let artifact = directory.path().join("must-not-exist");
    let (output, _) = json_output(&[
        "convert",
        input.to_str().unwrap(),
        "--to",
        "reference-unit",
        "--output",
        artifact.to_str().unwrap(),
        "--format",
        "json",
    ]);
    assert_eq!(output.status.code(), Some(64));
    assert!(!artifact.exists());
}
