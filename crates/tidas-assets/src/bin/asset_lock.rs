//! Repository-internal executable-asset and public-specification tooling.
//!
//! Two concerns live here because they are two halves of the same guarantee.
//! `check`/`write` own the paired schema lock and the complete executable-asset
//! byte lock. `spec-check`/`spec-import` own the pinned public specification the
//! generated public copies derive from: `spec-check` proves the checked-in copy
//! still matches the qualified candidate and writes nothing at all, and
//! `spec-import` atomically replaces exactly that public subset.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use tidas_assets::spec_import::{SpecImportReport, check_public_spec, import_public_spec};
use tidas_assets::spec_pin::{
    SPEC_ARCHIVE_FILE, SPEC_ARCHIVE_SHA256, SPEC_MANIFEST_SHA256, SPEC_REVISION, SPEC_VERSION,
    SpecPin, render_drift,
};
use tidas_assets::{
    AssetError, check_filesystem_lock, check_filesystem_schema_lock, write_lock, write_schema_lock,
};

const REPO_ROOT: &str = env!("CARGO_MANIFEST_DIR");
const USAGE: &str = "\
usage: tidas-asset-lock [check|write|spec-pin-env|spec-check|spec-import|public-rules-check|public-rules-sync]
                        [--archive <PATH>] [--source-root <PATH>]

  check        verify the paired schema lock and the complete executable asset lock
  write        regenerate the paired schema lock, then the complete asset lock
  spec-pin-env print the qualified candidate pin as GitHub environment-file entries
  spec-check   prove the public specification copy matches the pinned candidate (writes nothing)
  spec-import  atomically replace the public subset from the qualified candidate archive
  public-rules-check  verify exact W8 public definitions and toolkit profile composition
  public-rules-sync   import exact W8 public definitions and verify toolkit profile composition

  --archive <PATH>  candidate archive. Required by spec-import and accepted by spec-check,
                    which then re-derives the public subset from it. The archive is never
                    downloaded implicitly from a mutable branch.
  --source-root <PATH> exact tidas-spec checkout. Required by public-rules-sync.";

fn main() {
    let mut action: Option<String> = None;
    let mut archive: Option<PathBuf> = None;
    let mut source_root: Option<PathBuf> = None;
    let mut arguments = std::env::args().skip(1);
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--archive" => {
                let Some(value) = arguments.next() else {
                    eprintln!("--archive requires a path\n{USAGE}");
                    std::process::exit(64);
                };
                archive = Some(PathBuf::from(value));
            }
            "--source-root" => {
                let Some(value) = arguments.next() else {
                    eprintln!("--source-root requires a path\n{USAGE}");
                    std::process::exit(64);
                };
                source_root = Some(PathBuf::from(value));
            }
            "-h" | "--help" => {
                println!("{USAGE}");
                return;
            }
            _ if argument.starts_with('-') => {
                eprintln!("unknown option {argument}\n{USAGE}");
                std::process::exit(64);
            }
            _ => {
                if action.is_some() {
                    eprintln!("exactly one action may be requested\n{USAGE}");
                    std::process::exit(64);
                }
                action = Some(argument);
            }
        }
    }

    let action = action.unwrap_or_else(|| "check".to_owned());
    let root = Path::new(REPO_ROOT);
    if matches!(
        action.as_str(),
        "check" | "write" | "spec-pin-env" | "public-rules-check"
    ) && (archive.is_some() || source_root.is_some())
    {
        eprintln!("--archive applies only to spec-check and spec-import\n{USAGE}");
        std::process::exit(64);
    }
    let result = match action.as_str() {
        "check" => run_lock_check(root),
        "write" => run_lock_write(root),
        "spec-pin-env" => run_spec_pin_env(),
        "spec-check" => run_spec_check(root, archive.as_deref()),
        "spec-import" => run_spec_import(root, archive.as_deref()),
        "public-rules-check" => run_public_rules_check(root),
        "public-rules-sync" => run_public_rules_sync(root, source_root.as_deref()),
        _ => {
            eprintln!("{USAGE}");
            std::process::exit(64);
        }
    };
    if let Err(error) = result {
        eprintln!("{error}");
        if let AssetError::SpecDrift(drift) = &error {
            eprint!("{}", render_drift(drift));
        }
        std::process::exit(1);
    }
}

const PUBLIC_PIN_PATH: &str = "scripts/ci/tidas-public-rules-pin.json";
const PUBLIC_RULES_DIR: &str = "assets/tidas/rules";
const PUBLIC_IDENTITY_PATH: &str = "assets/tidas/rules/public-rules.source.v1.json";
const PROFILE_PATH: &str = "assets/tidas/methodologies/runtime_profiles.v1.json";

fn read_json(path: &Path) -> Result<Value, AssetError> {
    Ok(serde_json::from_slice(&fs::read(path)?)?)
}

fn public_error(message: impl Into<String>) -> AssetError {
    AssetError::PublicRules(message.into())
}

fn digest(bytes: &[u8]) -> String {
    let bytes = Sha256::digest(bytes);
    bytes.iter().fold(
        String::with_capacity(bytes.len() * 2),
        |mut output, byte| {
            write!(output, "{byte:02x}").expect("writing to a String cannot fail");
            output
        },
    )
}

fn canonical(value: &Value) -> Result<Vec<u8>, AssetError> {
    let mut bytes = serde_json::to_vec_pretty(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}

fn pin_asset<'a>(pin: &'a Value, name: &str, member: &str) -> Result<&'a str, AssetError> {
    pin.pointer(&format!("/assets/{name}/{member}"))
        .and_then(Value::as_str)
        .ok_or_else(|| public_error(format!("pin is missing assets.{name}.{member}")))
}

fn expected_identity(pin: &Value) -> Result<Value, AssetError> {
    Ok(json!({
        "schema_version": pin.get("schema_version").ok_or_else(|| public_error("pin has no schema_version"))?,
        "repository": pin.get("repository").ok_or_else(|| public_error("pin has no repository"))?,
        "commit": pin.get("commit").ok_or_else(|| public_error("pin has no commit"))?,
        "rules_version": pin.get("rules_version").ok_or_else(|| public_error("pin has no rules_version"))?,
        "status": pin.get("status").ok_or_else(|| public_error("pin has no status"))?,
        "index_sha256": pin_asset(pin, "index", "sha256")?,
        "schema_sha256": pin_asset(pin, "schema", "sha256")?,
    }))
}

fn values_by_id(root: &Value, member: &str) -> Result<BTreeMap<String, Value>, AssetError> {
    let values = root
        .get(member)
        .and_then(Value::as_array)
        .ok_or_else(|| public_error(format!("missing {member} array")))?;
    let mut output = BTreeMap::new();
    for value in values {
        let id = value
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .ok_or_else(|| public_error(format!("{member} contains a missing rule id")))?;
        if output.insert(id.to_owned(), value.clone()).is_some() {
            return Err(public_error(format!(
                "{member} contains duplicate rule {id}"
            )));
        }
    }
    Ok(output)
}

fn required<'a>(value: &'a Value, member: &str, id: &str) -> Result<&'a Value, AssetError> {
    value
        .get(member)
        .ok_or_else(|| public_error(format!("{id} is missing {member}")))
}

fn compose_runtime(public: &Value, profile: &Value) -> Result<Value, AssetError> {
    let public_by_id = values_by_id(public, "rules")?;
    let policy_by_id = values_by_id(profile, "public_rule_policy")?;
    let local_by_id = values_by_id(profile, "local_rules")?;
    if public_by_id.keys().collect::<Vec<_>>() != policy_by_id.keys().collect::<Vec<_>>() {
        return Err(public_error(
            "public definition and toolkit policy IDs differ",
        ));
    }
    if public_by_id.keys().any(|id| local_by_id.contains_key(id)) {
        return Err(public_error(
            "a rule cannot be both public and toolkit-local",
        ));
    }
    let order = profile
        .get("rule_order")
        .and_then(Value::as_array)
        .ok_or_else(|| public_error("profile has no rule_order array"))?;
    let mut seen = BTreeSet::new();
    let mut rules = Vec::with_capacity(order.len());
    for value in order {
        let id = value
            .as_str()
            .ok_or_else(|| public_error("rule_order contains a non-string value"))?;
        if !seen.insert(id.to_owned()) {
            return Err(public_error(format!(
                "rule_order contains duplicate rule {id}"
            )));
        }
        if let Some(definition) = public_by_id.get(id) {
            let policy = &policy_by_id[id];
            rules.push(json!({
                "id": id,
                "dataset_type": required(definition, "dataset_type", id)?,
                "summary": required(definition, "statement", id)?,
                "severity": required(policy, "severity", id)?,
                "phases": required(policy, "phases", id)?,
                "default_blocker": required(policy, "default_blocker", id)?,
                "field_paths": required(definition, "locations", id)?,
                "source_rule_refs": required(definition, "source_refs", id)?,
            }));
        } else if let Some(local) = local_by_id.get(id) {
            rules.push(local.clone());
        } else {
            return Err(public_error(format!(
                "rule_order references unknown rule {id}"
            )));
        }
    }
    if seen.len() != public_by_id.len() + local_by_id.len() {
        return Err(public_error(
            "rule_order does not cover every rule exactly once",
        ));
    }
    let rulesets = profile
        .get("rulesets")
        .and_then(Value::as_array)
        .ok_or_else(|| public_error("profile has no rulesets array"))?;
    for ruleset in rulesets {
        let id = required(ruleset, "id", "ruleset")?
            .as_str()
            .ok_or_else(|| public_error("ruleset id is not a string"))?;
        for rule_id in required(ruleset, "rule_ids", id)?
            .as_array()
            .ok_or_else(|| public_error(format!("ruleset {id} has invalid rule_ids")))?
        {
            let rule_id = rule_id
                .as_str()
                .ok_or_else(|| public_error(format!("ruleset {id} has a non-string rule id")))?;
            if !seen.contains(rule_id) {
                return Err(public_error(format!(
                    "ruleset {id} references unknown rule {rule_id}"
                )));
            }
        }
    }
    Ok(json!({
        "$schema": "runtime_rulesets.schema.json",
        "schema_version": 1,
        "ruleset_version": required(profile, "ruleset_version", "profile")?,
        "purpose": "Compatibility projection composed from exact public definitions and toolkit-owned runtime profile policy.",
        "source_assets": [
            {"asset": "public-rules.v1.json", "kind": "public rule definitions"},
            {"asset": "runtime_profiles.v1.json", "kind": "toolkit runtime profile policy"}
        ],
        "rulesets": rulesets,
        "rules": rules,
    }))
}

fn verified_public_inputs(root: &Path) -> Result<(Value, Value), AssetError> {
    let pin = read_json(&root.join(PUBLIC_PIN_PATH))?;
    let index_bytes = fs::read(root.join(PUBLIC_RULES_DIR).join("public-rules.v1.json"))?;
    let schema_bytes = fs::read(
        root.join(PUBLIC_RULES_DIR)
            .join("public-rules.v1.schema.json"),
    )?;
    if digest(&index_bytes) != pin_asset(&pin, "index", "sha256")?
        || digest(&schema_bytes) != pin_asset(&pin, "schema", "sha256")?
    {
        return Err(public_error(
            "bundled public-rule bytes differ from the exact pin",
        ));
    }
    if read_json(&root.join(PUBLIC_IDENTITY_PATH))? != expected_identity(&pin)? {
        return Err(public_error(
            "bundled public-rule source identity differs from the exact pin",
        ));
    }
    let public: Value = serde_json::from_slice(&index_bytes)?;
    if public.get("rules_version") != pin.get("rules_version") {
        return Err(public_error(
            "public rules version differs from the exact pin",
        ));
    }
    Ok((pin, public))
}

fn run_public_rules_check(root: &Path) -> Result<(), AssetError> {
    let (pin, public) = verified_public_inputs(root)?;
    let profile = read_json(&root.join(PROFILE_PATH))?;
    let public_schema = read_json(
        &root
            .join(PUBLIC_RULES_DIR)
            .join("public-rules.v1.schema.json"),
    )?;
    let profile_schema =
        read_json(&root.join("assets/tidas/methodologies/runtime_profiles.v1.schema.json"))?;
    for (label, schema, instance, draft) in [
        ("public rules", &public_schema, &public, 7_u8),
        ("runtime profile", &profile_schema, &profile, 20_u8),
    ] {
        let validator = if draft == 7 {
            jsonschema::draft7::new(schema)
        } else {
            jsonschema::draft202012::new(schema)
        }
        .map_err(|error| public_error(format!("{label} schema cannot compile: {error}")))?;
        if let Some(error) = validator.iter_errors(instance).next() {
            return Err(public_error(format!(
                "{label} fails schema validation: {error}"
            )));
        }
    }
    let _ = compose_runtime(&public, &profile)?;
    println!(
        "verified public rules and toolkit profile composition at {}",
        pin["commit"].as_str().unwrap_or("<invalid>")
    );
    Ok(())
}

fn run_public_rules_sync(root: &Path, source_root: Option<&Path>) -> Result<(), AssetError> {
    let Some(source_root) = source_root else {
        eprintln!("public-rules-sync requires --source-root <PATH>\n{USAGE}");
        std::process::exit(64);
    };
    let pin = read_json(&root.join(PUBLIC_PIN_PATH))?;
    let output = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .current_dir(source_root)
        .output()?;
    if !output.status.success()
        || String::from_utf8_lossy(&output.stdout).trim() != pin["commit"].as_str().unwrap_or("")
    {
        return Err(public_error(
            "public-rule source checkout differs from the exact pin",
        ));
    }
    for (name, destination) in [
        ("index", "public-rules.v1.json"),
        ("schema", "public-rules.v1.schema.json"),
    ] {
        let bytes = fs::read(source_root.join(pin_asset(&pin, name, "path")?))?;
        if digest(&bytes) != pin_asset(&pin, name, "sha256")? {
            return Err(public_error(format!(
                "public-rule {name} differs from the exact pin"
            )));
        }
        fs::write(root.join(PUBLIC_RULES_DIR).join(destination), bytes)?;
    }
    fs::write(
        root.join(PUBLIC_IDENTITY_PATH),
        canonical(&expected_identity(&pin)?)?,
    )?;
    run_public_rules_check(root)
}

fn run_lock_check(root: &Path) -> Result<(), AssetError> {
    check_filesystem_schema_lock(root)?;
    check_filesystem_lock(root)?;
    println!("paired schema lock and executable asset lock are current");
    Ok(())
}

fn run_lock_write(root: &Path) -> Result<(), AssetError> {
    let schema_path = write_schema_lock(root)?;
    println!("wrote {}", schema_path.display());
    let asset_path = write_lock(root)?;
    println!("wrote {}", asset_path.display());
    Ok(())
}

fn render_spec_pin_env() -> Result<String, AssetError> {
    let entries = [
        ("TIDAS_SPEC_CANDIDATE_VERSION", SPEC_VERSION),
        ("TIDAS_SPEC_CANDIDATE_COMMIT", SPEC_REVISION),
        ("TIDAS_SPEC_CANDIDATE_ARCHIVE", SPEC_ARCHIVE_FILE),
        ("TIDAS_SPEC_CANDIDATE_SHA256", SPEC_ARCHIVE_SHA256),
        ("TIDAS_SPEC_CANDIDATE_MANIFEST_SHA256", SPEC_MANIFEST_SHA256),
    ];
    let mut output = String::new();
    for (name, value) in entries {
        if value.is_empty() || value.contains(['\n', '\r']) {
            return Err(AssetError::SpecInvalid(format!(
                "{name} cannot be written safely to a GitHub environment file"
            )));
        }
        writeln!(output, "{name}={value}").expect("writing to a String cannot fail");
    }
    Ok(output)
}

fn run_spec_pin_env() -> Result<(), AssetError> {
    print!("{}", render_spec_pin_env()?);
    Ok(())
}

fn run_spec_check(root: &Path, archive: Option<&Path>) -> Result<(), AssetError> {
    let pin = SpecPin::qualified_candidate();
    let summary = check_public_spec(root, archive, &pin)?;
    println!("pinned public specification {}", summary.version);
    println!("reviewed specification revision {}", summary.spec_revision);
    println!(
        "imported assets from tools commit {}",
        summary.imported_source_commit
    );
    println!("{} public assets match the candidate", summary.asset_count);
    if summary.archive_verified {
        println!("the qualified candidate archive was re-derived and matched");
    } else {
        println!(
            "archive not supplied ({SPEC_ARCHIVE_FILE}); pass --archive to re-derive the public subset"
        );
    }
    Ok(())
}

fn run_spec_import(root: &Path, archive: Option<&Path>) -> Result<(), AssetError> {
    let Some(archive) = archive else {
        eprintln!("spec-import requires --archive <PATH>\n{USAGE}");
        std::process::exit(64);
    };
    let pin = SpecPin::qualified_candidate();
    let outcome = import_public_spec(root, archive, &pin, false)?;
    print_report(&outcome.report);
    Ok(())
}

fn print_report(report: &SpecImportReport) {
    println!(
        "{}",
        serde_json::to_string_pretty(report).expect("the import report is serializable")
    );
}

#[cfg(test)]
mod public_rule_tests {
    use super::*;

    fn inputs() -> (Value, Value) {
        let root = Path::new(REPO_ROOT);
        (
            read_json(&root.join(PUBLIC_RULES_DIR).join("public-rules.v1.json")).unwrap(),
            read_json(&root.join(PROFILE_PATH)).unwrap(),
        )
    }

    #[test]
    fn exact_public_and_profile_inputs_compose() {
        let (public, profile) = inputs();
        let composed = compose_runtime(&public, &profile).unwrap();
        assert_eq!(composed["rules"].as_array().unwrap().len(), 14);
        assert_eq!(profile["public_rule_policy"].as_array().unwrap().len(), 9);
        assert_eq!(profile["local_rules"].as_array().unwrap().len(), 5);
    }

    #[test]
    fn spec_pin_env_is_deterministic_and_workflow_has_no_duplicate_pin() {
        let rendered = render_spec_pin_env().unwrap();
        assert_eq!(
            rendered,
            format!(
                "TIDAS_SPEC_CANDIDATE_VERSION={SPEC_VERSION}\n\
                 TIDAS_SPEC_CANDIDATE_COMMIT={SPEC_REVISION}\n\
                 TIDAS_SPEC_CANDIDATE_ARCHIVE={SPEC_ARCHIVE_FILE}\n\
                 TIDAS_SPEC_CANDIDATE_SHA256={SPEC_ARCHIVE_SHA256}\n\
                 TIDAS_SPEC_CANDIDATE_MANIFEST_SHA256={SPEC_MANIFEST_SHA256}\n"
            )
        );

        let workflow =
            fs::read_to_string(Path::new(REPO_ROOT).join(".github/workflows/rust-ci.yml")).unwrap();
        assert!(workflow.contains("-- spec-pin-env >> \"$GITHUB_ENV\""));
        for line in workflow.lines() {
            let line = line.trim_start();
            assert!(
                !line.starts_with("TIDAS_SPEC_CANDIDATE_COMMIT:")
                    && !line.starts_with("TIDAS_SPEC_CANDIDATE_ARCHIVE:")
                    && !line.starts_with("TIDAS_SPEC_CANDIDATE_SHA256:")
                    && !line.starts_with("TIDAS_SPEC_CANDIDATE_MANIFEST_SHA256:")
                    && !line.starts_with("TIDAS_SPEC_CANDIDATE_VERSION:"),
                "rust-ci.yml must load the candidate pin from spec-pin-env, not define {line}"
            );
        }
    }

    #[test]
    fn missing_public_policy_is_rejected() {
        let (public, mut profile) = inputs();
        profile["public_rule_policy"].as_array_mut().unwrap().pop();
        let error = compose_runtime(&public, &profile).unwrap_err().to_string();
        assert!(error.contains("definition and toolkit policy IDs differ"));
    }

    #[test]
    fn unknown_profile_reference_is_rejected() {
        let (public, mut profile) = inputs();
        profile["rulesets"][0]["rule_ids"]
            .as_array_mut()
            .unwrap()
            .push(json!("tidas.process.unknown"));
        let error = compose_runtime(&public, &profile).unwrap_err().to_string();
        assert!(error.contains("references unknown rule"));
    }

    #[test]
    fn tampered_public_bytes_and_stale_identity_are_rejected() {
        let source = Path::new(REPO_ROOT);
        let temporary = tempfile::tempdir().unwrap();
        for relative in [
            PUBLIC_PIN_PATH,
            PUBLIC_IDENTITY_PATH,
            PROFILE_PATH,
            "assets/tidas/methodologies/runtime_profiles.v1.schema.json",
        ] {
            let destination = temporary.path().join(relative);
            fs::create_dir_all(destination.parent().unwrap()).unwrap();
            fs::copy(source.join(relative), destination).unwrap();
        }
        let destination_rules = temporary.path().join(PUBLIC_RULES_DIR);
        fs::create_dir_all(&destination_rules).unwrap();
        for name in ["public-rules.v1.json", "public-rules.v1.schema.json"] {
            fs::copy(
                source.join(PUBLIC_RULES_DIR).join(name),
                destination_rules.join(name),
            )
            .unwrap();
        }
        let index = destination_rules.join("public-rules.v1.json");
        let mut bytes = fs::read(&index).unwrap();
        bytes.push(b' ');
        fs::write(&index, bytes).unwrap();
        assert!(
            verified_public_inputs(temporary.path())
                .unwrap_err()
                .to_string()
                .contains("bytes differ")
        );

        fs::copy(
            source.join(PUBLIC_RULES_DIR).join("public-rules.v1.json"),
            &index,
        )
        .unwrap();
        let identity_path = temporary.path().join(PUBLIC_IDENTITY_PATH);
        let mut identity = read_json(&identity_path).unwrap();
        identity["commit"] = json!("0000000000000000000000000000000000000000");
        fs::write(identity_path, canonical(&identity).unwrap()).unwrap();
        assert!(
            verified_public_inputs(temporary.path())
                .unwrap_err()
                .to_string()
                .contains("source identity differs")
        );
    }
}
