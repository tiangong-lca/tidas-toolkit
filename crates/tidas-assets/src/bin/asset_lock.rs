//! Repository-internal executable-asset and public-specification tooling.
//!
//! Two concerns live here because they are two halves of the same guarantee.
//! `check`/`write` own the paired schema lock and the complete executable-asset
//! byte lock. `spec-check`/`spec-import` own the pinned public specification the
//! generated public copies derive from: `spec-check` proves the checked-in copy
//! still matches the qualified candidate and writes nothing at all, and
//! `spec-import` atomically replaces exactly that public subset.

use std::path::{Path, PathBuf};

use tidas_assets::spec_import::{SpecImportReport, check_public_spec, import_public_spec};
use tidas_assets::spec_pin::{SPEC_ARCHIVE_FILE, SpecPin, render_drift};
use tidas_assets::{
    AssetError, check_filesystem_lock, check_filesystem_schema_lock, write_lock, write_schema_lock,
};

const REPO_ROOT: &str = env!("CARGO_MANIFEST_DIR");
const USAGE: &str = "\
usage: tidas-asset-lock [check|write|spec-check|spec-import] [--archive <PATH>]

  check        verify the paired schema lock and the complete executable asset lock
  write        regenerate the paired schema lock, then the complete asset lock
  spec-check   prove the public specification copy matches the pinned candidate (writes nothing)
  spec-import  atomically replace the public subset from the qualified candidate archive

  --archive <PATH>  candidate archive. Required by spec-import and accepted by spec-check,
                    which then re-derives the public subset from it. The archive is never
                    downloaded implicitly from a mutable branch.";

fn main() {
    let mut action: Option<String> = None;
    let mut archive: Option<PathBuf> = None;
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
    if matches!(action.as_str(), "check" | "write") && archive.is_some() {
        eprintln!("--archive applies only to spec-check and spec-import\n{USAGE}");
        std::process::exit(64);
    }
    let result = match action.as_str() {
        "check" => run_lock_check(root),
        "write" => run_lock_write(root),
        "spec-check" => run_spec_check(root, archive.as_deref()),
        "spec-import" => run_spec_import(root, archive.as_deref()),
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
