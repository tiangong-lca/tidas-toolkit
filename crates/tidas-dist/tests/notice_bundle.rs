use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use flate2::{Compression, GzBuilder};
use sha2::{Digest, Sha256};
use tidas_dist::notice_bundle::{MANIFEST, NoticeBundleManifestV1};
use tidas_dist::{DistError, PackageRequest, notice_bundle, package, supported_targets, verify};

#[path = "support/notice_fixture.rs"]
mod notice_fixture;

fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            write!(out, "{byte:02x}").unwrap();
            out
        })
}

fn fixture(root: &Path, target: &str) {
    fs::write(root.join("binary"), b"native binary TEST fixture\n").unwrap();
    fs::write(root.join("LICENSE"), b"owner MIT TEST fixture\n").unwrap();
    notice_fixture::create(
        &root.join("notices"),
        &root.join("binary"),
        &root.join("LICENSE"),
        target,
        "0.2.2",
    );
}

fn read_manifest(root: &Path) -> NoticeBundleManifestV1 {
    serde_json::from_slice(&fs::read(root.join("notices").join(MANIFEST)).unwrap()).unwrap()
}

fn write_manifest(root: &Path, manifest: &NoticeBundleManifestV1) {
    fs::write(
        root.join("notices").join(MANIFEST),
        serde_json::to_vec_pretty(manifest).unwrap(),
    )
    .unwrap();
}

#[test]
fn notice_bundles_are_required_and_bound_on_every_supported_target() {
    for target in supported_targets() {
        let temporary = tempfile::tempdir().unwrap();
        let root = temporary.path();
        fixture(root, target);
        let artifact = package(&PackageRequest {
            binary: &root.join("binary"),
            license: &root.join("LICENSE"),
            notices_dir: &root.join("notices"),
            target,
            version: "0.2.2",
            output_dir: &root.join("dist"),
        })
        .unwrap();
        let verified = verify(
            &artifact.archive,
            &artifact.checksum_file,
            target,
            "0.2.2",
            false,
        )
        .unwrap();
        assert_eq!(verified.schema_version, "tidas.distribution-manifest.v2");
        fs::write(root.join("binary"), b"different binary\n").unwrap();
        assert!(
            notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2")
                .is_err()
        );
        assert!(
            package(&PackageRequest {
                binary: &root.join("binary"),
                license: &root.join("LICENSE"),
                notices_dir: &root.join("missing"),
                target,
                version: "0.2.2",
                output_dir: &root.join("failed")
            })
            .is_err()
        );
        assert!(!root.join("failed").exists());
    }
}

#[test]
fn omitted_packages_and_referenced_terms_are_rejected_against_retained_evidence() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let target = "aarch64-apple-darwin";
    fixture(root, target);
    let original = read_manifest(root);
    let mut missing_native = original.clone();
    missing_native.native_packages.pop();
    write_manifest(root, &missing_native);
    assert!(
        notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2")
            .is_err()
    );
    let mut missing_cargo = original.clone();
    missing_cargo.cargo_packages.clear();
    write_manifest(root, &missing_cargo);
    assert!(
        notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2")
            .is_err()
    );
    let mut missing_terms = original;
    missing_terms
        .rust_library_texts
        .retain(|text| text.license_id.as_deref() != Some("Unicode-3.0"));
    write_manifest(root, &missing_terms);
    assert!(
        notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2")
            .is_err()
    );
}

#[test]
fn changed_canonical_terms_fail_even_when_local_inventory_hashes_are_recomputed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let target = "aarch64-apple-darwin";
    fixture(root, target);
    let mut manifest = read_manifest(root);
    let text = manifest
        .rust_library_texts
        .iter_mut()
        .find(|text| text.license_id.as_deref() == Some("MIT"))
        .unwrap();
    let old_path = format!("texts/{}.txt", text.sha256);
    let changed = b"Not the reviewed original license terms\n";
    text.sha256 = sha256(changed);
    text.bytes = changed.len() as u64;
    let new_path = format!("texts/{}.txt", text.sha256);
    manifest.files.remove(&old_path);
    manifest.files.insert(
        new_path.clone(),
        notice_bundle::FileDigest {
            sha256: text.sha256.clone(),
            bytes: text.bytes,
        },
    );
    fs::remove_file(root.join("notices").join(old_path)).unwrap();
    fs::write(root.join("notices").join(new_path), changed).unwrap();
    write_manifest(root, &manifest);
    assert!(
        notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2")
            .is_err()
    );
}

#[test]
fn missing_archive_notice_fails_after_outer_checksum_is_recomputed() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let target = "x86_64-unknown-linux-gnu";
    fixture(root, target);
    let manifest = read_manifest(root);
    let artifact = package(&PackageRequest {
        binary: &root.join("binary"),
        license: &root.join("LICENSE"),
        notices_dir: &root.join("notices"),
        target,
        version: "0.2.2",
        output_dir: &root.join("dist"),
    })
    .unwrap();
    let extracted = root.join("extracted");
    let input = flate2::read::GzDecoder::new(fs::File::open(&artifact.archive).unwrap());
    tar::Archive::new(input).unpack(&extracted).unwrap();
    let archive_root = format!("tidas-v0.2.2-{target}");
    let omitted = manifest
        .files
        .keys()
        .find(|path| path.starts_with("texts/"))
        .unwrap();
    fs::remove_file(
        extracted
            .join(&archive_root)
            .join("share/licenses/tidas/third-party-notices")
            .join(omitted),
    )
    .unwrap();
    let gzip = GzBuilder::new().mtime(0).write(
        fs::File::create(&artifact.archive).unwrap(),
        Compression::default(),
    );
    let mut tar = tar::Builder::new(gzip);
    tar.append_dir_all(&archive_root, extracted.join(&archive_root))
        .unwrap();
    tar.into_inner().unwrap().finish().unwrap();
    let bytes = fs::read(&artifact.archive).unwrap();
    fs::write(
        &artifact.checksum_file,
        format!(
            "{}  {}\n",
            sha256(&bytes),
            artifact.archive.file_name().unwrap().to_str().unwrap()
        ),
    )
    .unwrap();
    let error = verify(
        &artifact.archive,
        &artifact.checksum_file,
        target,
        "0.2.2",
        false,
    )
    .unwrap_err();
    assert!(!matches!(error, DistError::ChecksumMismatch { .. }));
}

#[test]
fn canonical_notice_source_keeps_historical_bytes_readable_without_accepting_other_sources() {
    let temporary = tempfile::tempdir().unwrap();
    let root = temporary.path();
    let target = "aarch64-apple-darwin";
    fixture(root, target);
    let mut original = read_manifest(root);
    assert_eq!(
        original.source.repository,
        "https://github.com/tiangong-lca/tidas-toolkit"
    );
    original.source.repository = "https://github.com/tiangong-lca/tidas-tools".to_owned();
    write_manifest(root, &original);
    notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2").unwrap();
    let mut canonical = original.clone();
    canonical.source.repository = "https://github.com/tiangong-lca/tidas-toolkit".to_owned();
    write_manifest(root, &canonical);
    notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2").unwrap();
    canonical.source.repository = "https://github.com/unrelated/tidas-toolkit".to_owned();
    write_manifest(root, &canonical);
    assert!(
        notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.2.2")
            .is_err()
    );
    let mut future = original;
    future.version = "0.3.1".to_owned();
    write_manifest(root, &future);
    let error = notice_bundle::verify(&root.join("notices"), &root.join("binary"), target, "0.3.1")
        .unwrap_err();
    assert!(error.to_string().contains("native notice source identity"));
}
