//! Focused proof for the pinned public-specification import and check.
//!
//! The synthetic candidates here exercise the machinery without the qualified
//! bytes, so they run anywhere. Two things cannot be synthesised and are
//! therefore proved against real, repository-owned inputs instead:
//!
//! * the committed pin, manifest, provenance record, and the 39 checked-in
//!   public assets must agree (`committed_pin_*`), which is the drift proof CI
//!   runs with no network access; and
//! * the qualified candidate archive itself, which is external to this
//!   repository. Those cases are gated on `TIDAS_SPEC_CANDIDATE_ARCHIVE` and
//!   report that they were skipped rather than passing silently.

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::{Path, PathBuf};

use flate2::Compression;
use flate2::write::GzEncoder;
use tidas_assets::spec_import::{check_public_spec, import_public_spec};
use tidas_assets::spec_pin::{
    SPEC_PACKAGE_METADATA_ORIGIN, SPEC_PROVENANCE_PATH, SpecPin, canonical_json,
    hash_canonical_json, read_candidate_archive, subset_digest,
};
use tidas_assets::{AssetError, asset_fingerprint, bundled_assets, verify_embedded_assets};

const ENV_ARCHIVE: &str = "TIDAS_SPEC_CANDIDATE_ARCHIVE";
const SCHEMAS: usize = 18;

/// Environment variable naming the qualified candidate archive.
fn candidate_archive() -> Option<PathBuf> {
    let value = std::env::var_os(ENV_ARCHIVE)?;
    let path = PathBuf::from(value);
    if path.is_file() { Some(path) } else { None }
}

fn sha256(bytes: &[u8]) -> String {
    use sha2::{Digest as _, Sha256};
    let digest = Sha256::digest(bytes);
    digest.iter().fold(String::new(), |mut text, byte| {
        let _ = write!(text, "{byte:02x}");
        text
    })
}

/// A synthetic candidate: bytes plus the pin that binds them.
struct Synthetic {
    archive: Vec<u8>,
    pin: SpecPin,
    /// Repository-relative public asset paths and their bytes.
    assets: BTreeMap<String, Vec<u8>>,
    /// Every archive member, manifest included, for tamper fixtures.
    members: Vec<(String, Vec<u8>)>,
}

/// A well-formed synthetic candidate that mirrors the real layout.
fn synthetic() -> Synthetic {
    synthetic_with(&[])
}

/// A synthetic candidate with selected public assets replaced.
#[allow(clippy::too_many_lines)] // The fixture is one declarative package layout.
fn synthetic_with(overrides: &[(&str, &[u8])]) -> Synthetic {
    let mut assets = BTreeMap::new();
    for language in ["schemas", "schemas_zh"] {
        for index in 0..SCHEMAS {
            let path = format!("assets/tidas/{language}/tidas_s{index}.json");
            assets.insert(
                path,
                format!("{{\"title\":\"{language}-{index}\"}}\n").into_bytes(),
            );
        }
    }
    for path in [
        "assets/tidas/methodologies/tidas_flows.yaml",
        "assets/tidas/methodologies/tidas_processes.yaml",
    ] {
        assets.insert(path.to_owned(), b"name: synthetic\n".to_vec());
    }
    assets.insert(
        "assets/tidas/schema.lock.json".to_owned(),
        b"{\"version\":1}\n".to_vec(),
    );
    for (path, bytes) in overrides {
        assert!(
            assets.contains_key(*path),
            "override {path} is not a public asset"
        );
        assets.insert((*path).to_owned(), (*bytes).to_vec());
    }
    assert_eq!(assets.len(), 39);

    let mut shipped = BTreeMap::new();
    let mut content = BTreeMap::new();
    let mut source = BTreeMap::new();
    for (path, bytes) in &assets {
        let digest = sha256(bytes);
        shipped.insert(path.clone(), digest.clone());
        content.insert(path.clone(), digest.clone());
        // The candidate records the shipped path as the source path when the
        // import is path-preserving.
        source.insert(path.clone(), digest);
    }

    // The candidate's own metadata. These are bound by the manifest but are not
    // public specification assets, so they are never imported.
    let mut yaml_lines = vec![
        "version: 1".to_owned(),
        "sourceRepoId: tidas-toolkit".to_owned(),
        "sourceRepoCanonicalUrl: https://github.com/tiangong-lca/tidas-toolkit".to_owned(),
        "sourceCommit: 111111111111111111111111111111111111111a".to_owned(),
        "sourceCommitRef: main".to_owned(),
        "sourceLicensePath: LICENSE".to_owned(),
        format!("sourceLicenseSha256: {}", sha256(b"MIT")),
        "sourceLicenseNotice: MIT".to_owned(),
        "excludedSourcePaths:".to_owned(),
        "  - assets/tidas/methodologies/runtime_rulesets.json".to_owned(),
        "  - assets/tidas/methodologies/runtime_rulesets.schema.json".to_owned(),
        "  - assets/tidas/methodologies/elementary_flow_taxonomy_extension.v1.json".to_owned(),
        "files:".to_owned(),
    ];
    for (path, digest) in &source {
        yaml_lines.push(format!("  - sourcePath: {path}"));
        yaml_lines.push(format!("    packagePath: {path}"));
        yaml_lines.push(format!("    sha256: {digest}"));
    }
    let source_import = (yaml_lines.join("\n") + "\n").into_bytes();
    let baseline = format!(
        r#"{{
  "reviewedBaselineVersion": 1,
  "specVersion": "0.1.0",
  "note": "synthetic",
  "source": {{
    "repository": "https://github.com/tiangong-lca/tidas-toolkit",
    "repositoryId": "tidas-toolkit",
    "commit": "111111111111111111111111111111111111111a",
    "commitRef": "main",
    "licensePath": "LICENSE",
    "licenseSha256": "{license}"
  }},
  "fileCount": 39,
  "sourceFilesSha256": "{source_digest}",
  "packageFilesSha256": "{package_digest}"
}}
"#,
        license = sha256(b"MIT"),
        source_digest = subset_digest(&source).unwrap(),
        package_digest = subset_digest(&shipped).unwrap(),
    )
    .into_bytes();

    let mut manifest_text = String::new();
    writeln!(manifest_text, "{{").unwrap();
    let mut rows = Vec::new();
    for (path, digest) in &shipped {
        rows.push(format!(
            "    {{\"path\": {}, \"sha256\": \"{digest}\", \"contentSha256\": \"{digest}\", \
             \"origin\": \"tidas-toolkit\", \"source\": {{\"path\": {}, \"sha256\": \"{digest}\"}}}}",
            serde_json::to_string(path).unwrap(),
            serde_json::to_string(path).unwrap(),
        ));
    }
    // The candidate's own metadata. These are bound by the manifest but are not
    // public specification assets, so they are never imported.
    let metadata: BTreeMap<String, Vec<u8>> = [
        ("LICENSE", b"MIT\n".to_vec()),
        ("README.md", b"# synthetic\n".to_vec()),
        ("package.json", b"{\"name\":\"synthetic\"}\n".to_vec()),
        ("reviewed-baseline.json", baseline.clone()),
        ("source-import.yaml", source_import.clone()),
    ]
    .into_iter()
    .map(|(path, bytes)| (path.to_owned(), bytes))
    .collect();
    for (path, bytes) in &metadata {
        let digest = sha256(bytes);
        rows.push(format!(
            "    {{\"path\": {}, \"sha256\": \"{digest}\", \"contentSha256\": \"{digest}\", \
             \"origin\": \"{SPEC_PACKAGE_METADATA_ORIGIN}\", \"source\": null}}",
            serde_json::to_string(path).unwrap(),
        ));
    }
    rows.sort();
    writeln!(manifest_text, "  \"files\": [").unwrap();
    writeln!(manifest_text, "{}", rows.join(",\n")).unwrap();
    writeln!(manifest_text, "  ],").unwrap();
    // The remaining manifest members are written without the files array.

    let tail = format!(
        r#"  "manifestVersion": 1,
  "package": {{"name": "@tiangong-lca/tidas-spec", "version": "0.1.0"}},
  "specVersion": "0.1.0",
  "source": {{
    "repository": "https://github.com/tiangong-lca/tidas-toolkit",
    "repositoryId": "tidas-toolkit",
    "commit": "111111111111111111111111111111111111111a",
    "commitRef": "main",
    "license": {{"path": "LICENSE", "sha256": "{license}", "notice": "MIT"}},
    "excludedPaths": [
      "assets/tidas/methodologies/runtime_rulesets.json",
      "assets/tidas/methodologies/runtime_rulesets.schema.json",
      "assets/tidas/methodologies/elementary_flow_taxonomy_extension.v1.json"
    ],
    "ownedMetadataOrigin": "{SPEC_PACKAGE_METADATA_ORIGIN}"
  }},
  "counts": {{
    "schemasPerLanguage": {SCHEMAS},
    "languages": ["en", "zh"],
    "methodologies": 2,
    "importedAssets": 39,
    "packageMetadata": 5,
    "files": 44,
    "packagedFiles": 45
  }},
  "assetRoot": "assets/tidas",
  "lock": "assets/tidas/schema.lock.json",
  "aggregates": {{"filesSha256": "@FILES@", "filesContentSha256": "@CONTENT@"}},
  "selfHash": {{"note": "synthetic"}}
}}
"#,
        license = sha256(b"MIT"),
    );
    let mut all_shipped = shipped.clone();
    let mut all_content = content.clone();
    for (path, bytes) in &metadata {
        all_shipped.insert(path.clone(), sha256(bytes));
        all_content.insert(path.clone(), sha256(bytes));
    }
    // The manifest's aggregates cover every file it binds, not just the public
    // subset, so they are computed over the metadata too.
    let files_sha256 = subset_digest(&all_shipped).unwrap();
    let content_sha256 = subset_digest(&all_content).unwrap();
    let tail = tail
        .replace("@FILES@", &files_sha256)
        .replace("@CONTENT@", &content_sha256);
    let manifest_text = manifest_text + &tail;
    let manifest_bytes = manifest_text.into_bytes();

    let mut entries: Vec<(String, Vec<u8>)> = assets
        .iter()
        .map(|(path, bytes)| (path.clone(), bytes.clone()))
        .collect();
    entries.push(("spec-manifest.json".to_owned(), manifest_bytes.clone()));
    for (path, bytes) in &metadata {
        entries.push((path.clone(), bytes.clone()));
    }
    entries.sort_by(|left, right| left.0.cmp(&right.0));

    let archive = tar_gz(&entries);
    let pin = SpecPin {
        package_name: "@tiangong-lca/tidas-spec".to_owned(),
        version: "0.1.0".to_owned(),
        repository: "https://github.com/tiangong-lca/tidas-spec".to_owned(),
        revision: "222222222222222222222222222222222222222b".to_owned(),
        imported_source_repository: "https://github.com/tiangong-lca/tidas-toolkit".to_owned(),
        imported_source_commit: "111111111111111111111111111111111111111a".to_owned(),
        archive_file: "synthetic.tgz".to_owned(),
        archive_sha256: sha256(&archive),
        manifest_sha256: sha256(&manifest_bytes),
        imported_file_count: 39,
        authored_file_count: 0,
        public_file_count: 39,
        package_file_count: 45,
        schemas_per_language: SCHEMAS,
    };
    Synthetic {
        archive,
        pin,
        assets,
        members: entries,
    }
}

/// Write a gzip tar whose final member name is written raw into the header.
///
/// `tar::Builder` refuses to emit a traversal path, so this bypasses its own
/// validation to prove the importer's safety check is what rejects the member
/// rather than the writer never producing it.
fn tar_gz_raw_name(name: &str, bytes: &[u8], entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    builder.mode(tar::HeaderMode::Deterministic);
    for (path, content) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(content.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        builder
            .append_data(&mut header, format!("package/{path}"), content.as_slice())
            .unwrap();
    }
    let mut header = tar::Header::new_gnu();
    header.set_size(bytes.len() as u64);
    header.set_mode(0o644);
    header.set_mtime(0);
    {
        let gnu = header
            .as_gnu_mut()
            .expect("a GNU header was just allocated");
        assert!(name.len() <= gnu.name.len(), "raw member name is too long");
        gnu.name[..name.len()].copy_from_slice(name.as_bytes());
    }
    header.set_cksum();
    builder.append(&header, bytes).unwrap();
    builder.into_inner().unwrap().finish().unwrap()
}

/// Write a gzip tar archive with `package/`-prefixed, deterministic members.
fn tar_gz(entries: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    builder.mode(tar::HeaderMode::Deterministic);
    for (path, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        header.set_cksum();
        let name = format!("package/{path}");
        builder
            .append_data(&mut header, name, bytes.as_slice())
            .unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn write_archive(directory: &Path, bytes: &[u8]) -> PathBuf {
    let path = directory.join("candidate.tgz");
    fs::write(&path, bytes).unwrap();
    path
}

fn write_asset(root: &Path, path: &str, bytes: &[u8]) {
    let absolute = root.join(path);
    fs::create_dir_all(absolute.parent().unwrap()).unwrap();
    fs::write(absolute, bytes).unwrap();
}

/// Every regular file under `root`, repository-relative and sorted.
fn tree_files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut pending = vec![root.to_path_buf()];
    while let Some(directory) = pending.pop() {
        for entry in fs::read_dir(&directory).unwrap() {
            let entry = entry.unwrap();
            let path = entry.path();
            let metadata = fs::symlink_metadata(&path).unwrap();
            if metadata.is_dir() {
                pending.push(path);
            } else {
                found.push(
                    path.strip_prefix(root)
                        .unwrap()
                        .to_string_lossy()
                        .replace('\\', "/"),
                );
            }
        }
    }
    found.sort();
    found
}

// ---------------------------------------------------------------------------
// Canonical encoding
// ---------------------------------------------------------------------------

#[test]
fn canonical_json_matches_the_specification_repository_encoding() {
    // The specification repository's `canonicalJson` sorts object keys, emits no
    // insignificant whitespace, and escapes strings with JSON's own escapes.
    let value = serde_json::json!({"b": 1, "a": [true, null, "x\"y"], "c": {"z": 2, "y": []}});
    let rendered = canonical_json(&value).unwrap();
    assert_eq!(
        rendered,
        r#"{"a":[true,null,"x\"y"],"b":1,"c":{"y":[],"z":2}}"#
    );
    assert_eq!(
        hash_canonical_json(&value).unwrap(),
        sha256(rendered.as_bytes())
    );
    // Insignificant input ordering cannot change the digest.
    let reordered = serde_json::json!({"c": {"y": [], "z": 2}, "a": [true, null, "x\"y"], "b": 1});
    assert_eq!(
        hash_canonical_json(&value).unwrap(),
        hash_canonical_json(&reordered).unwrap()
    );
}

// ---------------------------------------------------------------------------
// Archive and candidate rejection
// ---------------------------------------------------------------------------

#[test]
fn a_malformed_or_truncated_archive_fails_before_any_write() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    let truncated = &candidate.archive[..candidate.archive.len() / 2];
    let path = write_archive(directory.path(), truncated);
    assert!(import_public_spec(&root, &path, &candidate.pin, false).is_err());
    assert_eq!(tree_files(&root), Vec::<String>::new());
}

#[test]
fn a_forged_public_assets_digest_in_the_provenance_record_is_rejected() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);
    import_public_spec(&root, &archive, &candidate.pin, false).unwrap();
    assert!(check_public_spec(&root, Some(&archive), &candidate.pin).is_ok());

    // The record's own claim is not evidence for itself: a malformed value, a
    // well-formed but wrong value, and a value that merely restates the pin
    // must all be rejected in both the offline and archive-backed checks.
    let provenance = root.join(SPEC_PROVENANCE_PATH);
    let original: serde_json::Value =
        serde_json::from_slice(&fs::read(&provenance).unwrap()).unwrap();
    let honest = original["publicAssetsSha256"].as_str().unwrap().to_owned();
    assert!(honest.len() == 64, "fixture layout changed");

    for forged in [
        "not-a-digest".to_owned(),
        String::new(),
        "0".repeat(64),
        // Uppercase hex is not the canonical digest form the pin records.
        honest.to_uppercase(),
    ] {
        let mut value = original.clone();
        value["publicAssetsSha256"] = serde_json::json!(forged);
        fs::write(&provenance, serde_json::to_vec(&value).unwrap()).unwrap();
        assert!(
            check_public_spec(&root, None, &candidate.pin).is_err(),
            "offline check accepted forged publicAssetsSha256 {forged:?}"
        );
        assert!(
            check_public_spec(&root, Some(&archive), &candidate.pin).is_err(),
            "archive-backed check accepted forged publicAssetsSha256 {forged:?}"
        );
        // A rejected check must not have rewritten the forged record.
        let after: serde_json::Value =
            serde_json::from_slice(&fs::read(&provenance).unwrap()).unwrap();
        assert_eq!(after["publicAssetsSha256"], serde_json::json!(forged));
    }

    // Restoring the truthful value makes the check pass again.
    fs::write(&provenance, serde_json::to_vec(&original).unwrap()).unwrap();
    assert!(check_public_spec(&root, None, &candidate.pin).is_ok());
}

#[test]
fn a_record_whose_digest_does_not_match_the_checked_in_bytes_is_rejected() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);
    import_public_spec(&root, &archive, &candidate.pin, false).unwrap();

    // A syntactically valid digest for a *different* subset cannot be smuggled
    // in by editing only the record, because the check recomputes the digest
    // from the bytes it validated.
    let provenance = root.join(SPEC_PROVENANCE_PATH);
    let mut value: serde_json::Value =
        serde_json::from_slice(&fs::read(&provenance).unwrap()).unwrap();
    value["publicAssetsSha256"] = serde_json::json!(sha256(b"some other subset"));
    fs::write(&provenance, serde_json::to_vec(&value).unwrap()).unwrap();

    let error = check_public_spec(&root, None, &candidate.pin).unwrap_err();
    match error {
        AssetError::SpecInvalid(message) => assert!(
            message.contains("checked-in public assets digest"),
            "expected a digest mismatch, got {message}"
        ),
        other => panic!("expected a digest mismatch, got {other}"),
    }
}

#[test]
fn an_archive_missing_its_gzip_trailer_is_rejected() {
    let mut candidate = synthetic();
    // Remove the 8-byte gzip trailer but keep the honest inner manifest and
    // recompute only the outer digest, so nothing but the stream structure can
    // reject it. Reading members to the end would not notice this by itself.
    candidate.archive.truncate(candidate.archive.len() - 8);
    candidate.pin.archive_sha256 = sha256(&candidate.archive);
    let directory = tempfile::tempdir().unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);

    let error = read_candidate_archive(&archive, &candidate.pin).unwrap_err();
    match error {
        AssetError::SpecInvalid(message) => assert!(
            message.contains("single-member gzip stream"),
            "expected a gzip framing rejection, got {message}"
        ),
        other => panic!("expected a gzip framing rejection, got {other}"),
    }
}

#[test]
fn an_archive_with_trailing_data_after_a_valid_member_is_rejected() {
    let second_member = synthetic().archive;
    for suffix in [b"GARBAGE".to_vec(), second_member] {
        let mut candidate = synthetic();
        candidate.archive.extend_from_slice(&suffix);
        candidate.pin.archive_sha256 = sha256(&candidate.archive);
        let directory = tempfile::tempdir().unwrap();
        let archive = write_archive(directory.path(), &candidate.archive);
        let error = read_candidate_archive(&archive, &candidate.pin).unwrap_err();
        assert!(
            matches!(error, AssetError::SpecInvalid(_)),
            "trailing data was accepted: {error}"
        );
    }
}

#[test]
fn a_truncated_archive_is_rejected_without_being_read_without_bound() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();

    // Every strict prefix that still carries the gzip header must fail closed
    // rather than yield a partial member set.
    for cut in [1_usize, 10, 32, candidate.archive.len() / 2] {
        let mut truncated = synthetic();
        truncated.archive.truncate(cut);
        truncated.pin.archive_sha256 = sha256(&truncated.archive);
        let archive = write_archive(directory.path(), &truncated.archive);
        assert!(
            read_candidate_archive(&archive, &truncated.pin).is_err(),
            "an archive truncated to {cut} bytes was accepted"
        );
    }

    // A compressed file beyond the size cap is rejected from its metadata,
    // without being read into memory first.
    let oversized = directory.path().join("oversized.tgz");
    let file = fs::File::create(&oversized).unwrap();
    file.set_len(tidas_assets::spec_pin::SPEC_ARCHIVE_MAX_BYTES + 1)
        .unwrap();
    drop(file);
    let mut pin = candidate.pin.clone();
    pin.archive_sha256 = "0".repeat(64);
    let error = read_candidate_archive(&oversized, &pin).unwrap_err();
    match error {
        AssetError::SpecInvalid(message) => assert!(
            message.contains("beyond the"),
            "expected a size-cap rejection, got {message}"
        ),
        other => panic!("expected a size-cap rejection, got {other}"),
    }
}

#[test]
fn an_archive_whose_header_names_do_not_survive_normalization_is_rejected() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    // A raw header must name exactly the path a reader sees. A `./` prefix is
    // caught by path safety, while a doubled separator is collapsed by `Path`
    // component parsing and would otherwise be accepted as the legitimate
    // member `package/...` — that second case is what the raw-header check adds.
    for (label, transform) in [
        (
            "leading-dot",
            (|name: &str| format!("./{name}")) as fn(&str) -> String,
        ),
        ("doubled-slash", |name: &str| name.replacen('/', "//", 1)),
    ] {
        let rebuilt = tar_gz_with_raw_names(&candidate.members, transform);
        let mut pin = candidate.pin.clone();
        pin.archive_sha256 = sha256(&rebuilt);
        let archive = write_archive(directory.path(), &rebuilt);
        let error = read_candidate_archive(&archive, &pin).unwrap_err();
        assert!(
            matches!(error, AssetError::SpecInvalid(_)),
            "{label}: a non-normalizing header name was accepted: {error}"
        );
        assert_eq!(
            tree_files(&root),
            Vec::<String>::new(),
            "{label}: a rejected archive wrote to the repository"
        );
    }
}

/// Write a tar archive whose stored header names are transformed directly.
fn tar_gz_with_raw_names(entries: &[(String, Vec<u8>)], transform: fn(&str) -> String) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    builder.mode(tar::HeaderMode::Deterministic);
    for (path, bytes) in entries {
        let mut header = tar::Header::new_gnu();
        header.set_size(bytes.len() as u64);
        header.set_mode(0o644);
        header.set_mtime(0);
        {
            let name = transform(&format!("package/{path}"));
            let gnu = header
                .as_gnu_mut()
                .expect("a GNU header was just allocated");
            assert!(name.len() <= gnu.name.len(), "raw name is too long");
            gnu.name[..name.len()].copy_from_slice(name.as_bytes());
        }
        header.set_cksum();
        builder.append(&header, bytes.as_slice()).unwrap();
    }
    builder.into_inner().unwrap().finish().unwrap()
}

#[test]
fn an_archive_whose_digest_does_not_match_the_pin_is_rejected() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    let mut tampered = candidate.archive.clone();
    // Flipping one byte of the compressed stream keeps it readable in the
    // general case, but the digest check must reject it first either way.
    let last = tampered.len() - 1;
    tampered[last] ^= 0xff;
    let path = write_archive(directory.path(), &tampered);
    let error = import_public_spec(&root, &path, &candidate.pin, false).unwrap_err();
    match error {
        AssetError::SpecArchiveDigest { expected, found } => {
            assert_eq!(expected, candidate.pin.archive_sha256);
            assert_ne!(found, expected);
        }
        other => panic!("expected an archive digest failure, got {other}"),
    }
    assert_eq!(tree_files(&root), Vec::<String>::new());
}

#[test]
fn a_manifest_that_does_not_match_the_pinned_digest_is_rejected() {
    let mut candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    // Rebuild the archive with an edited manifest. The archive digest is
    // updated so the *manifest* digest is the binding that must reject it.
    let mut entries = candidate.members.clone();
    let manifest = entries
        .iter_mut()
        .find(|(path, _)| path == "spec-manifest.json")
        .unwrap();
    manifest.1.extend_from_slice(b"\n");
    let rebuilt = tar_gz(&entries);
    let path = write_archive(directory.path(), &rebuilt);
    candidate.pin.archive_sha256 = sha256(&rebuilt);
    let error = import_public_spec(&root, &path, &candidate.pin, false).unwrap_err();
    match error {
        AssetError::SpecManifestDigest { expected, found } => {
            assert_eq!(expected, candidate.pin.manifest_sha256);
            assert_ne!(found, expected);
        }
        other => panic!("expected a manifest digest failure, got {other}"),
    }
    assert_eq!(tree_files(&root), Vec::<String>::new());
}

#[test]
#[cfg(unix)]
fn a_symlinked_public_asset_never_satisfies_the_check() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);
    import_public_spec(&root, &archive, &candidate.pin, false).unwrap();
    assert!(check_public_spec(&root, Some(&archive), &candidate.pin).is_ok());

    // Replace one asset with a symlink pointing at bytes that are themselves
    // correct, so only the symlink rule can reject it.
    let target = directory.path().join("outside.json");
    let drifted = "assets/tidas/schemas/tidas_s0.json";
    fs::write(&target, fs::read(root.join(drifted)).unwrap()).unwrap();
    fs::remove_file(root.join(drifted)).unwrap();
    std::os::unix::fs::symlink(&target, root.join(drifted)).unwrap();

    let error = check_public_spec(&root, Some(&archive), &candidate.pin).unwrap_err();
    match error {
        AssetError::SpecInvalid(message) => assert!(
            message.contains("symbolic link"),
            "expected a symlink rejection, got {message}"
        ),
        other => panic!("expected a symlink rejection, got {other}"),
    }
    // A failed check must not have replaced the symlink with a real file.
    assert!(
        fs::symlink_metadata(root.join(drifted))
            .unwrap()
            .file_type()
            .is_symlink()
    );
}

#[test]
fn an_unexpected_extra_public_asset_outside_the_reviewed_subset_is_rejected() {
    let mut candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    // A 40th imported schema that is not in the reviewed subset, correctly
    // digested everywhere so only the subset rule can reject it.
    let extra_path = "assets/tidas/schemas/tidas_extra.json";
    let extra_bytes = b"{\"title\":\"extra\"}\n".to_vec();
    let extra_digest = sha256(&extra_bytes);

    let mut entries = candidate.members.clone();
    entries.push((extra_path.to_owned(), extra_bytes));
    let manifest = entries
        .iter_mut()
        .find(|(path, _)| path == "spec-manifest.json")
        .unwrap();
    let manifest_text = String::from_utf8(manifest.1.clone()).unwrap();
    let anchor = "    {\"path\": \"assets/tidas/schemas/tidas_s9.json\"".to_owned();
    let injected = format!(
        "    {{\"path\": {path}, \"sha256\": \"{extra_digest}\", \"contentSha256\": \"{extra_digest}\", \"origin\": \"tidas-toolkit\", \"source\": {{\"path\": {path}, \"sha256\": \"{extra_digest}\"}}}},\n",
        path = serde_json::to_string(extra_path).unwrap(),
    );
    assert!(manifest_text.contains(&anchor), "fixture layout changed");
    let manifest_text = manifest_text
        .replace("\"importedAssets\": 39", "\"importedAssets\": 40")
        .replace("\"files\": 44", "\"files\": 45")
        .replace("\"packagedFiles\": 45", "\"packagedFiles\": 46")
        .replace(&anchor, &(injected + &anchor));
    manifest.1 = manifest_text.clone().into_bytes();

    let rebuilt = tar_gz(&entries);
    let archive_path = write_archive(directory.path(), &rebuilt);
    candidate.pin.archive_sha256 = sha256(&rebuilt);
    candidate.pin.manifest_sha256 = sha256(manifest_text.as_bytes());

    let error = import_public_spec(&root, &archive_path, &candidate.pin, false).unwrap_err();
    assert!(
        error.to_string().contains("40 public assets"),
        "expected the subset size rule to reject the extra asset, got {error}"
    );
    assert_eq!(tree_files(&root), Vec::<String>::new());
}

// ---------------------------------------------------------------------------
// Success, idempotency, and no-write checking
// ---------------------------------------------------------------------------

#[test]
fn import_adopts_the_public_subset_and_repeat_import_is_idempotent() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);

    let first = import_public_spec(&root, &archive, &candidate.pin, false).unwrap();
    assert_eq!(first.report.status, "imported");
    assert_eq!(first.report.public_asset_count, 39);
    for (path, bytes) in &candidate.assets {
        assert_eq!(&fs::read(root.join(path)).unwrap(), bytes, "{path}");
    }
    let after_first = tree_files(&root);

    let second = import_public_spec(&root, &archive, &candidate.pin, false).unwrap();
    assert_eq!(second.report.status, "already-current");
    assert!(second.report.changed_paths.is_empty());
    assert_eq!(tree_files(&root), after_first);
    assert_eq!(
        first.report.public_assets_sha256,
        second.report.public_assets_sha256
    );
}

#[test]
fn check_writes_nothing_even_when_the_copy_has_drifted() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);
    import_public_spec(&root, &archive, &candidate.pin, false).unwrap();
    let clean = tree_files(&root);
    assert!(check_public_spec(&root, Some(&archive), &candidate.pin).is_ok());

    // Introduce drift in a generated public copy.
    let drifted = "assets/tidas/schemas/tidas_s0.json";
    write_asset(&root, drifted, b"{\"title\":\"hand-edited\"}\n");
    let error = check_public_spec(&root, Some(&archive), &candidate.pin).unwrap_err();
    match error {
        AssetError::SpecDrift(paths) => assert_eq!(
            paths,
            vec![format!(
                "{drifted}: {}",
                sha256(b"{\"title\":\"hand-edited\"}\n")
            )]
        ),
        other => panic!("expected drift, got {other}"),
    }
    // Check must not have repaired or rewritten anything.
    assert_eq!(tree_files(&root), clean);
    assert_eq!(
        fs::read(root.join(drifted)).unwrap(),
        b"{\"title\":\"hand-edited\"}\n"
    );
}

/// Every path a public asset or provenance record may be written to.
fn all_destinations(candidate: &Synthetic) -> Vec<String> {
    let mut paths: Vec<String> = candidate.assets.keys().cloned().collect();
    paths.push("assets/spec/spec-manifest.json".to_owned());
    paths.push(SPEC_PROVENANCE_PATH.to_owned());
    paths
}

#[test]
#[cfg(unix)]
fn a_symlinked_destination_parent_is_refused_before_any_external_write() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    let outside = directory.path().join("outside");
    fs::create_dir_all(root.join("assets/tidas")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    // A linked *parent* directory, not a linked leaf: the weakest guard would
    // only ever inspect the final component.
    std::os::unix::fs::symlink(&outside, root.join("assets/tidas/schemas")).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);

    let error = import_public_spec(&root, &archive, &candidate.pin, false).unwrap_err();
    match error {
        AssetError::SpecInvalid(message) => assert!(
            message.contains("symbolic link"),
            "expected a symlink rejection, got {message}"
        ),
        other => panic!("expected a symlink rejection, got {other}"),
    }
    assert_eq!(
        fs::read_dir(&outside).unwrap().count(),
        0,
        "import wrote outside the repository through a linked parent"
    );
    // Nothing at all was created inside the repository either.
    assert!(!root.join("assets/spec").exists());
}

#[test]
#[cfg(unix)]
fn an_intermediate_symlinked_ancestor_and_a_dangling_link_are_both_refused() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();

    // An intermediate linked ancestor, above the asset directories.
    let root = directory.path().join("repo-intermediate");
    let outside = directory.path().join("outside-intermediate");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("assets")).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);
    assert!(import_public_spec(&root, &archive, &candidate.pin, false).is_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    // A dangling link: its target does not exist yet, so following it would
    // create the target outside the repository.
    let root = directory.path().join("repo-dangling");
    let missing = directory.path().join("never-created");
    fs::create_dir_all(root.join("assets/tidas")).unwrap();
    std::os::unix::fs::symlink(&missing, root.join("assets/tidas/schemas_zh")).unwrap();
    assert!(import_public_spec(&root, &archive, &candidate.pin, false).is_err());
    assert!(
        !missing.exists(),
        "a dangling link target was created outside the repository"
    );
}

#[test]
#[cfg(unix)]
fn a_symlinked_provenance_or_staging_path_is_refused() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();

    // `assets/spec` itself a link to an external directory.
    let root = directory.path().join("repo-spec-link");
    let outside = directory.path().join("outside-spec");
    fs::create_dir_all(root.join("assets")).unwrap();
    fs::create_dir_all(&outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("assets/spec")).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);
    assert!(import_public_spec(&root, &archive, &candidate.pin, false).is_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);

    // `assets/spec/.staging` a link to an external directory.
    let root = directory.path().join("repo-staging-link");
    let outside = directory.path().join("outside-staging");
    fs::create_dir_all(&root).unwrap();
    fs::create_dir_all(&outside).unwrap();
    let spec = root.join("assets/spec");
    fs::create_dir_all(&spec).unwrap();
    std::os::unix::fs::symlink(&outside, spec.join(".staging")).unwrap();
    assert!(import_public_spec(&root, &archive, &candidate.pin, false).is_err());
    assert_eq!(fs::read_dir(&outside).unwrap().count(), 0);
}

#[test]
#[cfg(unix)]
fn a_check_cannot_be_satisfied_through_a_linked_ancestor() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);
    import_public_spec(&root, &archive, &candidate.pin, false).unwrap();
    assert!(check_public_spec(&root, Some(&archive), &candidate.pin).is_ok());

    // Move a schema directory outside the repository and link it back: the
    // bytes are still correct, but they are no longer the repository's copy.
    let outside = directory.path().join("outside-schemas");
    fs::rename(root.join("assets/tidas/schemas"), &outside).unwrap();
    std::os::unix::fs::symlink(&outside, root.join("assets/tidas/schemas")).unwrap();

    let error = check_public_spec(&root, Some(&archive), &candidate.pin).unwrap_err();
    match error {
        AssetError::SpecInvalid(message) => assert!(
            message.contains("symbolic link"),
            "expected a symlink rejection, got {message}"
        ),
        other => panic!("expected a symlink rejection, got {other}"),
    }
}

#[test]
fn an_unowned_staging_directory_and_its_contents_are_preserved() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(root.join("assets/spec/.staging")).unwrap();
    let sentinel = root.join("assets/spec/.staging/other-writer-state");
    fs::write(&sentinel, b"must survive").unwrap();
    let other_dir = root.join("assets/spec/.staging/.import-other-writer");
    fs::create_dir_all(&other_dir).unwrap();
    let other_file = other_dir.join("partial");
    fs::write(&other_file, b"another writer's staging").unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);

    let outcome = import_public_spec(&root, &archive, &candidate.pin, false).unwrap();
    assert_eq!(outcome.report.status, "imported");
    assert_eq!(
        fs::read(&sentinel).unwrap(),
        b"must survive",
        "another writer's state was destroyed"
    );
    assert_eq!(
        fs::read(&other_file).unwrap(),
        b"another writer's staging",
        "another writer's staging directory was removed"
    );
    // The import's own staging directory is gone, leaving only the other
    // writer's entries.
    let mut left: Vec<String> = fs::read_dir(root.join("assets/spec/.staging"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    left.sort();
    assert_eq!(
        left,
        vec![
            ".import-other-writer".to_owned(),
            "other-writer-state".to_owned()
        ],
        "the import did not clean up exactly its own staging directory"
    );
}

#[test]
fn overlapping_imports_are_serialized_rather_than_interleaved() {
    let candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let archive = write_archive(directory.path(), &candidate.archive);

    // Coincident imports are serialized by an exclusively created lock, so a
    // second importer is refused while the first holds the mutation window
    // rather than staging concurrently and risking an undo of committed bytes.
    let (first, second) = std::thread::scope(|scope| {
        let other = scope.spawn(|| import_public_spec(&root, &archive, &candidate.pin, false));
        let first = import_public_spec(&root, &archive, &candidate.pin, false);
        (first, other.join().unwrap())
    });
    let outcomes = [&first, &second];
    let succeeded = outcomes.iter().filter(|result| result.is_ok()).count();
    let refused = outcomes
        .iter()
        .filter(|result| {
            result
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("another import holds"))
        })
        .count();
    assert!(
        succeeded >= 1,
        "no import completed: {:?}",
        outcomes
            .iter()
            .map(|r| r.as_ref().err().map(ToString::to_string))
            .collect::<Vec<_>>()
    );
    assert_eq!(
        succeeded + refused,
        2,
        "an import neither succeeded nor reported lock contention"
    );

    // Whoever won, the repository holds exactly the candidate's bytes and the
    // lock is released rather than left held.
    for path in all_destinations(&candidate) {
        if let Some(expected) = candidate.assets.get(&path) {
            assert_eq!(&fs::read(root.join(&path)).unwrap(), expected, "{path}");
        }
    }
    assert!(
        !root.join("assets/spec/.import.lock").exists(),
        "the lock leaked"
    );
    if let Ok(entries) = fs::read_dir(root.join("assets/spec/.staging")) {
        let left: Vec<String> = entries
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert!(left.is_empty(), "staging trees were left behind: {left:?}");
    }
}

#[test]
fn a_duplicate_archive_member_is_rejected() {
    let mut candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    // The same member name twice must fail even when both copies are correct,
    // so a later duplicate cannot shadow an earlier verified file.
    let mut entries = candidate.members.clone();
    let duplicate = entries
        .iter()
        .find(|(path, _)| path == "assets/tidas/schemas/tidas_s0.json")
        .expect("fixture layout changed")
        .clone();
    entries.push(duplicate);
    let rebuilt = tar_gz(&entries);
    let path = write_archive(directory.path(), &rebuilt);
    candidate.pin.archive_sha256 = sha256(&rebuilt);

    let error = import_public_spec(&root, &path, &candidate.pin, false).unwrap_err();
    assert!(
        error.to_string().contains("more than once"),
        "expected a duplicate-member rejection, got {error}"
    );
    assert_eq!(tree_files(&root), Vec::<String>::new());
}

#[test]
fn an_unsafe_archive_member_path_is_rejected() {
    let mut candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    // Parent traversal must never be extracted, however well-formed the rest of
    // the archive is.
    let rebuilt = tar_gz_raw_name(
        "../escaped.json",
        b"{\"escaped\":true}\n",
        &candidate.members,
    );
    let path = write_archive(directory.path(), &rebuilt);
    candidate.pin.archive_sha256 = sha256(&rebuilt);

    let error = import_public_spec(&root, &path, &candidate.pin, false).unwrap_err();
    assert!(
        error.to_string().contains("unsafe") || error.to_string().contains("outside"),
        "expected an unsafe-path rejection, got {error}"
    );
    assert_eq!(tree_files(&root), Vec::<String>::new());
    assert!(!directory.path().join("escaped.json").exists());
}

#[test]
fn a_member_whose_bytes_disagree_with_the_manifest_is_rejected_as_corrupt() {
    let mut candidate = synthetic();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();

    // Edited content with an unchanged manifest: the file is corrupt, not
    // merely unknown.
    let mut entries = candidate.members.clone();
    let member = entries
        .iter_mut()
        .find(|(path, _)| path == "assets/tidas/schemas/tidas_s0.json")
        .unwrap();
    member.1 = b"{\"title\":\"corrupted\"}\n".to_vec();
    let rebuilt = tar_gz(&entries);
    let path = write_archive(directory.path(), &rebuilt);
    candidate.pin.archive_sha256 = sha256(&rebuilt);

    let error = import_public_spec(&root, &path, &candidate.pin, false).unwrap_err();
    match error {
        AssetError::SpecInvalid(message) => assert!(
            message.contains("is corrupt"),
            "expected a corruption rejection, got {message}"
        ),
        other => panic!("expected a corruption rejection, got {other}"),
    }
    assert_eq!(tree_files(&root), Vec::<String>::new());
}

#[test]
fn a_repository_copy_bound_to_a_different_candidate_fails_closed() {
    let original = synthetic();
    // A second candidate that differs in exactly one public asset but is
    // otherwise internally consistent and correctly self-bound.
    let other = synthetic_with(&[(
        "assets/tidas/schemas/tidas_s0.json",
        b"{\"title\":\"schemas-different\"}\n",
    )]);
    assert_ne!(original.pin.manifest_sha256, other.pin.manifest_sha256);

    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let original_archive = write_archive(directory.path(), &original.archive);
    import_public_spec(&root, &original_archive, &original.pin, false).unwrap();

    // The repository holds the original bytes. A pin for any other candidate
    // must be rejected against the committed manifest rather than silently
    // reinterpreting the same paths under a new identity.
    let other_archive = directory.path().join("other.tgz");
    fs::write(&other_archive, &other.archive).unwrap();
    let error = check_public_spec(&root, Some(&other_archive), &other.pin).unwrap_err();
    match error {
        AssetError::SpecManifestDigest { expected, found } => {
            assert_eq!(expected, other.pin.manifest_sha256);
            assert_eq!(found, original.pin.manifest_sha256);
        }
        other => panic!("expected a manifest digest rejection, got {other}"),
    }

    // The original pin still checks clean, so the failure did not disturb it.
    assert!(check_public_spec(&root, Some(&original_archive), &original.pin).is_ok());
}

#[test]
fn committed_pin_binds_the_qualified_candidate_identity() {
    let pin = SpecPin::qualified_candidate();
    assert_eq!(pin.version, "0.2.0");
    assert_eq!(pin.archive_file, "tiangong-lca-tidas-spec-0.2.0.tgz");
    assert_eq!(
        pin.archive_sha256,
        "45da9de790ffcdadd1984ffc3544c67a1503c0947470a629e8252aed19100228"
    );
    assert_eq!(
        pin.manifest_sha256,
        "4677b9cf864326be9d430bf9760c754c4c0c1905d90e62c161655a159fd758c7"
    );
    assert_eq!(pin.imported_file_count, 34);
    assert_eq!(pin.authored_file_count, 7);
    assert_eq!(pin.public_file_count, 39);
    assert_eq!(pin.package_file_count, 47);
    assert_eq!(pin.schemas_per_language, 18);
}

#[test]
fn provenance_records_the_pin_the_check_enforces() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pin = SpecPin::qualified_candidate();
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("assets/spec/spec-pin.json")).unwrap()).unwrap();
    assert_eq!(provenance["schemaVersion"], "tidas.spec-pin.v1");
    assert_eq!(provenance["package"], pin.package_name);
    assert_eq!(provenance["version"], pin.version);
    assert_eq!(provenance["specRevision"], pin.revision);
    assert_eq!(
        provenance["importedSourceCommit"],
        pin.imported_source_commit
    );
    assert_eq!(provenance["archiveSha256"], pin.archive_sha256);
    assert_eq!(provenance["manifestSha256"], pin.manifest_sha256);
    assert_eq!(provenance["importedFileCount"], 39);
    // A stale provenance record cannot stay checked in beside a moved pin.
    assert!(check_public_spec(root, None, &pin).is_ok());
}

#[test]
fn the_committed_public_copy_matches_the_pinned_manifest() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pin = SpecPin::qualified_candidate();
    let summary = check_public_spec(root, None, &pin).unwrap();
    assert_eq!(summary.asset_count, 39);
    assert_eq!(summary.version, pin.version);
    assert!(!summary.archive_verified);

    // The digest the check derives is the one the specification repository
    // computed for the same 39 files with its own implementation.
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("assets/spec/spec-pin.json")).unwrap()).unwrap();
    assert_eq!(
        provenance["publicAssetsSha256"],
        "b23252b293e2e584fbbd5d8ce72fce4dd575d9facdcb2462bcb15ad3724b2391"
    );
}

#[test]
fn importing_provenance_does_not_disturb_the_executable_asset_identity() {
    // Provenance lives outside the locked executable asset tree, so it cannot
    // shift the asset set, the full lock, or the runtime fingerprint.
    let pin = SpecPin::qualified_candidate();
    assert!(
        !pin.archive_file.is_empty(),
        "the pin must name a concrete archive"
    );
    for source_root in tidas_assets::SOURCE_ROOTS {
        assert!(
            !SPEC_PROVENANCE_PATH.starts_with(source_root),
            "{SPEC_PROVENANCE_PATH} is inside the executable asset root {source_root}"
        );
    }
    let assets = bundled_assets();
    assert!(
        !assets
            .iter()
            .any(|asset| asset.path.starts_with("assets/spec/")),
        "provenance leaked into the embedded executable assets"
    );
    let lock = verify_embedded_assets().unwrap();
    assert_eq!(lock.entries.len(), 83);
    assert_eq!(asset_fingerprint().unwrap(), asset_fingerprint().unwrap());
}

// ---------------------------------------------------------------------------
// Qualified candidate archive (external, explicitly supplied)
// ---------------------------------------------------------------------------

#[test]
fn the_qualified_candidate_archive_reproduces_the_committed_public_copy() {
    let Some(archive) = candidate_archive() else {
        eprintln!("skipped: set {ENV_ARCHIVE} to the qualified candidate archive");
        return;
    };
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let pin = SpecPin::qualified_candidate();
    let candidate = read_candidate_archive(&archive, &pin).unwrap();
    assert_eq!(candidate.assets.len(), 39);
    assert_eq!(
        candidate.archive_sha256.as_deref(),
        Some(pin.archive_sha256.as_str())
    );
    let summary = check_public_spec(root, Some(&archive), &pin).unwrap();
    assert!(summary.archive_verified);
    assert_eq!(summary.asset_count, 39);

    // The candidate's own reviewed baseline and this repository must agree on
    // the public subset digest.
    let provenance: serde_json::Value =
        serde_json::from_slice(&fs::read(root.join("assets/spec/spec-pin.json")).unwrap()).unwrap();
    assert_eq!(
        candidate.public_assets_sha256,
        provenance["publicAssetsSha256"].as_str().unwrap()
    );
}

#[test]
fn a_hand_edited_public_copy_is_detected_against_the_qualified_candidate() {
    let Some(archive) = candidate_archive() else {
        eprintln!("skipped: set {ENV_ARCHIVE} to the qualified candidate archive");
        return;
    };
    let pin = SpecPin::qualified_candidate();
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path().join("repo");
    fs::create_dir_all(&root).unwrap();
    let candidate = read_candidate_archive(&archive, &pin).unwrap();
    for (path, bytes) in &candidate.assets {
        write_asset(&root, path, bytes);
    }
    write_asset(
        &root,
        "assets/spec/spec-manifest.json",
        &candidate.manifest_bytes,
    );
    write_asset(
        &root,
        SPEC_PROVENANCE_PATH,
        &tidas_assets::spec_pin::SpecProvenance::for_candidate(
            &pin,
            candidate.assets.len(),
            candidate.public_assets_sha256.clone(),
        )
        .to_bytes()
        .unwrap(),
    );
    assert!(check_public_spec(&root, Some(&archive), &pin).is_ok());

    // One edited byte in a generated copy must fail the check.
    let drifted = "assets/tidas/schemas/tidas_flows.json";
    let mut edited = fs::read(root.join(drifted)).unwrap();
    edited.push(b' ');
    write_asset(&root, drifted, &edited);
    let error = check_public_spec(&root, Some(&archive), &pin).unwrap_err();
    match error {
        AssetError::SpecDrift(paths) => {
            assert_eq!(paths.len(), 1);
            assert!(paths[0].starts_with(drifted), "{paths:?}");
        }
        other => panic!("expected drift, got {other}"),
    }
}
