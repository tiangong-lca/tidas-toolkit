use std::fs;
use std::process::Command;

use serde_json::Value;

#[path = "support/notice_fixture.rs"]
mod notice_fixture;

fn command() -> Command {
    Command::new(env!("CARGO_BIN_EXE_tidas-dist"))
}

#[test]
fn migration_fixture_keeps_python_as_oracle_but_only_tidas_as_product() {
    let fixture: Value =
        serde_json::from_str(include_str!("fixtures/distribution-migration-v1.json")).unwrap();
    assert_eq!(fixture["schema_version"], "tidas.distribution-migration.v1");
    assert_eq!(
        fixture["rust_product"]["executables"],
        serde_json::json!(["tidas"])
    );
    assert_eq!(
        fixture["rust_product"]["runtime_languages"],
        serde_json::json!([])
    );
    assert_eq!(
        fixture["decisions"]["legacy_entrypoint_compatibility"],
        false
    );
    assert_eq!(fixture["decisions"]["python_wrapper"], false);
    assert!(fixture["rust_product"]["warning_exit_code"].is_null());
}

#[test]
fn cli_success_malformed_input_determinism_and_exit_codes_are_stable() {
    let temporary = tempfile::tempdir().unwrap();
    let binary = temporary.path().join("tidas");
    let license = temporary.path().join("LICENSE");
    fs::write(&binary, b"native fixture\n").unwrap();
    fs::write(&license, b"MIT\n").unwrap();
    let first = temporary.path().join("first");
    let second = temporary.path().join("second");
    let notices = temporary.path().join("notices");
    notice_fixture::create(
        &notices,
        &binary,
        &license,
        "x86_64-unknown-linux-gnu",
        env!("CARGO_PKG_VERSION"),
    );

    for output_dir in [&first, &second] {
        let output = command()
            .args([
                "package",
                "--binary",
                binary.to_str().unwrap(),
                "--license",
                license.to_str().unwrap(),
                "--notices-dir",
                notices.to_str().unwrap(),
                "--target",
                "x86_64-unknown-linux-gnu",
                "--output-dir",
                output_dir.to_str().unwrap(),
            ])
            .output()
            .unwrap();
        assert_eq!(
            output.status.code(),
            Some(0),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let report: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(report["schema_version"], "tidas.distribution-artifact.v1");
    }

    let archive = format!(
        "tidas-v{}-x86_64-unknown-linux-gnu.tar.gz",
        env!("CARGO_PKG_VERSION")
    );
    assert_eq!(
        fs::read(first.join(&archive)).unwrap(),
        fs::read(second.join(&archive)).unwrap()
    );

    let verified = command()
        .args([
            "verify",
            "--archive",
            first.join(&archive).to_str().unwrap(),
            "--checksum",
            first.join(format!("{archive}.sha256")).to_str().unwrap(),
            "--target",
            "x86_64-unknown-linux-gnu",
        ])
        .output()
        .unwrap();
    assert_eq!(verified.status.code(), Some(0));

    let malformed = command()
        .args([
            "package",
            "--binary",
            binary.to_str().unwrap(),
            "--license",
            license.to_str().unwrap(),
            "--notices-dir",
            notices.to_str().unwrap(),
            "--target",
            "windows-arm64-unsupported",
            "--output-dir",
            temporary.path().to_str().unwrap(),
        ])
        .output()
        .unwrap();
    assert_eq!(malformed.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&malformed.stderr).contains("unsupported release target"));
}
