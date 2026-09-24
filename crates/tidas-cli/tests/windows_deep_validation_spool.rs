#![cfg(windows)]

use std::ffi::OsStr;
use std::fs;
use std::os::windows::ffi::OsStrExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;

fn tidas() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tidas"))
}

fn run_schema(input: &Path, issues: &Path) -> (Output, Value) {
    let output = tidas()
        .arg("validate")
        .arg(input)
        .args(["--input-format", "tidas-json", "--schema-only", "--issues"])
        .arg(issues)
        .args(["--format", "json", "--progress", "never"])
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap();
    (output, report)
}

fn run_batch(input: &Path, manifest: &Path, events: &Path) -> (Output, Value) {
    let output = tidas()
        .arg("validate")
        .arg(input)
        .args([
            "--protocol",
            "document-validation-batch.v1",
            "--input-manifest",
        ])
        .arg(manifest)
        .arg("--events")
        .arg(events)
        .args(["--format", "json", "--progress", "never"])
        .output()
        .unwrap();
    let report = serde_json::from_slice(&output.stdout).unwrap();
    (output, report)
}

fn long_task_relative_path() -> PathBuf {
    PathBuf::from("项目 workspace")
        .join(".foundry")
        .join("workspaces")
        .join(format!("task-{}", "a".repeat(64)))
        .join("outputs")
        .join("assessment")
        .join("b".repeat(64))
        .join(format!("run-{}", "c".repeat(40)))
        .join("process")
        .join(format!(".tidas-validate-stage-{}", "d".repeat(36)))
}

fn utf16_length(value: &OsStr) -> usize {
    value.encode_wide().count()
}

#[test]
fn native_validation_keeps_issue_and_batch_spools_under_deep_unicode_task_paths() {
    let directory = tempfile::tempdir().unwrap();
    let input = directory.path().join("input");
    fs::create_dir_all(input.join("sources")).unwrap();
    fs::write(input.join("sources/bad.json"), b"{}").unwrap();
    let shallow = directory.path().join("short");
    fs::create_dir(&shallow).unwrap();

    let relative = long_task_relative_path();
    let deep = directory.path().join(&relative);
    assert!(utf16_length(deep.as_os_str()) > 324);
    // Setup uses the already-existing root's verbatim path. The executable
    // receives the ordinary caller path, as Foundry passes it on Windows.
    let native_deep = fs::canonicalize(directory.path()).unwrap().join(relative);
    fs::create_dir_all(&native_deep).unwrap();
    assert!(deep.is_dir());

    let short_issues = shallow.join("validation-events.jsonl");
    let deep_issues = deep.join("validation-events.jsonl");
    let (short_exit, short_report) = run_schema(&input, &short_issues);
    let (deep_exit, deep_report) = run_schema(&input, &deep_issues);
    assert_eq!(short_exit.status.code(), Some(2), "{short_report}");
    assert_eq!(deep_exit.status.code(), Some(2), "{deep_report}");
    for report in [&short_report, &deep_report] {
        assert_eq!(report["status"], "completed-with-issues");
        assert_eq!(report["exit_class"], "data-issues");
        assert_eq!(report["summary"]["validation"]["error_count"], 1);
        assert_eq!(
            report["summary"]["validation"]["issue_spool"]["event_count"],
            1
        );
    }
    assert_eq!(
        fs::read(&short_issues).unwrap(),
        fs::read(native_deep.join("validation-events.jsonl")).unwrap()
    );
    assert_eq!(
        short_report["summary"]["validation"]["issue_spool"]["sha256"],
        deep_report["summary"]["validation"]["issue_spool"]["sha256"]
    );
    assert_eq!(
        deep_report["artifacts"][0]["path"].as_str(),
        deep_issues.to_str()
    );

    let manifest = directory.path().join("manifest.jsonl");
    fs::write(&manifest, "{\"document_key\":\"source:test:01.00.000\",\"category\":\"sources\",\"relative_path\":\"sources/bad.json\",\"content_sha256\":\"44136fa355b3678a1146ad16f7e8649e94fb4fc21fe77e8310c060f61caaff8a\"}\n").unwrap();
    let short_events = shallow.join("batch-events.jsonl");
    let deep_events = deep.join("batch-events.jsonl");
    let (short_exit, short_report) = run_batch(&input, &manifest, &short_events);
    let (deep_exit, deep_report) = run_batch(&input, &manifest, &deep_events);
    assert_eq!(short_exit.status.code(), Some(0), "{short_report}");
    assert_eq!(deep_exit.status.code(), Some(0), "{deep_report}");
    assert_eq!(
        short_report["summary"]["validation_batch_final"],
        deep_report["summary"]["validation_batch_final"]
    );
    let bytes = fs::read(&short_events).unwrap();
    assert_eq!(
        bytes,
        fs::read(native_deep.join("batch-events.jsonl")).unwrap()
    );
    let events = bytes
        .split(|byte| *byte == b'\n')
        .filter(|line| !line.is_empty())
        .map(|line| serde_json::from_slice::<Value>(line).unwrap())
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);
    assert_eq!(events[0]["type"], "issue");
    assert_eq!(events[1]["type"], "final");
    assert_eq!(events[1]["completed"], true);
    assert_eq!(
        deep_report["artifacts"][0]["path"].as_str(),
        deep_events.to_str()
    );
}
