use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs;
use std::path::Path;

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

fn digest(bytes: &[u8]) -> Value {
    let sha = Sha256::digest(bytes)
        .iter()
        .fold(String::with_capacity(64), |mut out, byte| {
            write!(out, "{byte:02x}").unwrap();
            out
        });
    json!({"sha256":sha,"bytes":bytes.len()})
}

fn text(files: &mut BTreeMap<String, Vec<u8>>, bytes: &[u8], kind: &str, path: &str) -> Value {
    let hash = digest(bytes);
    files.insert(
        format!("texts/{}.txt", hash["sha256"].as_str().unwrap()),
        bytes.to_vec(),
    );
    json!({"sha256":hash["sha256"],"bytes":hash["bytes"],"kind":kind,"source_path":path,"source_url":null})
}

fn reference_texts(files: &mut BTreeMap<String, Vec<u8>>) -> Vec<Value> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packaging/third-party-notices");
    let catalog: Value =
        serde_json::from_slice(&fs::read(root.join("referenced-terms.json")).unwrap()).unwrap();
    catalog["terms"]
        .as_array()
        .unwrap()
        .iter()
        .map(|term| {
            let bytes = fs::read(root.join(term["path"].as_str().unwrap())).unwrap();
            let mut record = text(
                files,
                &bytes,
                "referenced-license-terms",
                term["source_path"].as_str().unwrap(),
            );
            record["source_url"] = term["source_url"].clone();
            record["license_id"] = term["license_id"].clone();
            record
        })
        .collect()
}

pub fn create(output: &Path, binary: &Path, license: &Path, target: &str, version: &str) {
    let triplet = match target {
        "x86_64-pc-windows-msvc" => "x64-windows-static",
        "aarch64-apple-darwin" => "arm64-osx",
        "aarch64-unknown-linux-gnu" => "arm64-linux",
        _ => "x64-linux",
    };
    let mut files = BTreeMap::new();
    let project = text(
        &mut files,
        &fs::read(license).unwrap(),
        "project-license",
        "LICENSE",
    );
    let native = text(
        &mut files,
        b"Native notice fixture\n",
        "native-upstream-notices",
        "share/fixture/copyright",
    );
    let mut rust = vec![
        text(
            &mut files,
            b"Rust project license fixture\n",
            "rust-project-license",
            "LICENSE-MIT",
        ),
        text(
            &mut files,
            b"Rust library report fixture\n",
            "rust-library-notice-report",
            "COPYRIGHT-library.html",
        ),
        text(
            &mut files,
            b"Compiler builtins license fixture\n",
            "rust-library-source-license-or-notice",
            "lib/rustlib/src/rust/library/compiler-builtins/LICENSE.txt",
        ),
        text(
            &mut files,
            b"Libm license fixture\n",
            "rust-library-source-license-or-notice",
            "lib/rustlib/src/rust/library/compiler-builtins/libm/LICENSE.txt",
        ),
    ];
    rust.extend(reference_texts(&mut files));
    let lock = format!("version = 4\n\n[[package]]\nname = \"tidas\"\nversion = \"{version}\"\n");
    let lock_hash = digest(lock.as_bytes())["sha256"].clone();
    files.insert("evidence/Cargo.lock".to_owned(), lock.into_bytes());
    files.insert("evidence/vcpkg-status.txt".to_owned(), format!("Package: libxml2\nVersion: 1\nArchitecture: {triplet}\nStatus: install ok installed\n\nPackage: libxslt\nVersion: 1\nArchitecture: {triplet}\nStatus: install ok installed\n").into_bytes());
    files.insert(
        "evidence/vcpkg-manifest.json".to_owned(),
        serde_json::to_vec(&json!({"builtin-baseline":"2".repeat(40)})).unwrap(),
    );
    files.insert(
        "evidence/rustc-version.txt".to_owned(),
        format!(
            "rustc fixture\ncommit-hash: {}\nrelease: 1.98.0\n",
            "3".repeat(40)
        )
        .into_bytes(),
    );
    files.insert(
        "README.txt".to_owned(),
        b"TEST FIXTURE, not official native publication evidence\n".to_vec(),
    );
    let packages = json!([{"name":"tidas","version":version,"registry_checksum":null,"declared_license":"MIT","scopes":["rust-normal"],"target_kinds":["bin"],"features":[],"texts":[project]}]);
    files.insert(
        "evidence/cargo-packages.json".to_owned(),
        serde_json::to_vec(&packages).unwrap(),
    );
    rust_libraries(&mut files, target);
    let inventory: BTreeMap<_, _> = files
        .iter()
        .map(|(name, bytes)| (name.clone(), digest(bytes)))
        .collect();
    let manifest = json!({
        "schema_version":"tidas.native-notice-bundle.v1","product":"tidas","version":version,"target":target,
        "executable":digest(&fs::read(binary).unwrap()),
        "source":{"repository":"https://github.com/tiangong-lca/tidas-toolkit","commit":"1".repeat(40),"cargo_lock_sha256":lock_hash,"vcpkg_commit":"2".repeat(40),"vcpkg_triplet":triplet,"rustc_commit":"3".repeat(40),"rustc_release":"1.98.0"},
        "cargo_packages":packages,
        "native_packages":[
            {"name":"libxml2","version":"1","scope":"vcpkg-target-build-input","triplet":triplet,"features":[],"texts":[native]},
            {"name":"libxslt","version":"1","scope":"vcpkg-target-build-input","triplet":triplet,"features":[],"texts":[native]}
        ],"rust_library_texts":rust,"files":inventory,
    });
    write_files(output, files);
    fs::write(
        output.join("notice-manifest.json"),
        serde_json::to_vec_pretty(&manifest).unwrap(),
    )
    .unwrap();
}

fn write_files(output: &Path, files: BTreeMap<String, Vec<u8>>) {
    for (path, bytes) in files {
        let path = output.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, bytes).unwrap();
    }
}

fn rust_libraries(files: &mut BTreeMap<String, Vec<u8>>, target: &str) {
    let libraries: BTreeMap<_, _> = [
        "libstd-fixture.rlib",
        "libcore-fixture.rlib",
        "liballoc-fixture.rlib",
        "libcompiler_builtins-fixture.rlib",
    ]
    .iter()
    .map(|name| (name, digest(b"TEST library fixture")))
    .collect();
    files.insert(
        "evidence/rust-target-libraries.json".to_owned(),
        serde_json::to_vec(&json!({"target":target,"libraries":libraries})).unwrap(),
    );
}
