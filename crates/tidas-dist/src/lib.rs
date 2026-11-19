//! Deterministic native distribution archives and package-manager metadata.
//!
//! This crate is an internal release tool. It never rebuilds the product
//! binary: every archive, checksum, installer manifest, and package-manager
//! record is derived from the exact `tidas` executable supplied by the caller.

pub mod notice_bundle;
pub mod notices;

use std::collections::BTreeMap;
use std::fmt::Write as _;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use flate2::Compression;
use flate2::GzBuilder;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tempfile::TempDir;
use walkdir::WalkDir;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

const COPY_BUFFER_BYTES: usize = 1024 * 1024;
const WINDOWS_TARGET: &str = "x86_64-pc-windows-msvc";
const REQUIRED_TARGETS: [&str; 4] = [
    "x86_64-unknown-linux-gnu",
    "aarch64-unknown-linux-gnu",
    "aarch64-apple-darwin",
    WINDOWS_TARGET,
];

#[derive(Debug, thiserror::Error)]
pub enum DistError {
    #[error("native notice evidence is invalid: {0}")]
    Notice(String),
    #[error("unsupported release target: {0}")]
    UnsupportedTarget(String),
    #[error("release input is not a regular file: {0}")]
    MissingInput(PathBuf),
    #[error("unsafe archive member path: {0}")]
    UnsafePath(PathBuf),
    #[error("archive does not contain the expected manifest")]
    MissingManifest,
    #[error("archive manifest does not match target/version: expected {expected}, found {found}")]
    ManifestMismatch { expected: String, found: String },
    #[error("archive checksum mismatch: expected {expected}, found {found}")]
    ChecksumMismatch { expected: String, found: String },
    #[error("packaged tidas smoke command failed: {0}")]
    SmokeFailed(String),
    #[error("missing checksum for release artifact: {0}")]
    MissingChecksum(String),
    #[error("invalid checksum line: {0}")]
    InvalidChecksum(String),
    #[error("distribution size overflow")]
    SizeOverflow,
    #[error(transparent)]
    Io(#[from] std::io::Error),
    #[error(transparent)]
    Json(#[from] serde_json::Error),
    #[error(transparent)]
    Zip(#[from] zip::result::ZipError),
    #[error(transparent)]
    Walk(#[from] walkdir::Error),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DistributionManifestV2 {
    pub schema_version: String,
    pub product: String,
    pub version: String,
    pub target: String,
    pub executable: String,
    pub self_contained_native_xml: bool,
    pub third_party_notices: notice_bundle::FileDigest,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct DistributionArtifactV1 {
    pub schema_version: String,
    pub archive: PathBuf,
    pub checksum_file: PathBuf,
    pub sha256: String,
    pub bytes: u64,
    pub target: String,
    pub version: String,
}

#[derive(Clone, Debug)]
pub struct PackageRequest<'a> {
    pub binary: &'a Path,
    pub license: &'a Path,
    pub notices_dir: &'a Path,
    pub target: &'a str,
    pub version: &'a str,
    pub output_dir: &'a Path,
}

pub fn package(request: &PackageRequest<'_>) -> Result<DistributionArtifactV1, DistError> {
    validate_target(request.target)?;
    require_file(request.binary)?;
    require_file(request.license)?;
    let notices = notice_bundle::verify(
        request.notices_dir,
        request.binary,
        request.target,
        request.version,
    )?;
    let license_digest = sha256_file(request.license)?;
    if !notices
        .cargo_packages
        .iter()
        .filter(|package| package.name == "tidas")
        .flat_map(|package| &package.texts)
        .any(|text| text.kind == "project-license" && text.sha256 == license_digest)
    {
        return Err(DistError::Notice(
            "project license differs from the collected source license".to_owned(),
        ));
    }
    let notice_manifest =
        notice_bundle::file_digest(&request.notices_dir.join(notice_bundle::MANIFEST))?;
    fs::create_dir_all(request.output_dir)?;

    let root_name = archive_root(request.version, request.target);
    let staging = tempfile::tempdir_in(request.output_dir)?;
    let root = staging.path().join(&root_name);
    let binary_name = executable_name(request.target);
    let staged_binary = root.join("bin").join(binary_name);
    fs::create_dir_all(staged_binary.parent().expect("binary has parent"))?;
    fs::copy(request.binary, &staged_binary)?;
    #[cfg(unix)]
    set_executable(&staged_binary)?;

    let license_dir = root.join("share").join("licenses").join("tidas");
    fs::create_dir_all(&license_dir)?;
    fs::copy(request.license, license_dir.join("LICENSE"))?;
    let notice_dir = license_dir.join("third-party-notices");
    for (relative, path, _) in collect_files(request.notices_dir)? {
        let destination = notice_dir.join(relative);
        fs::create_dir_all(destination.parent().expect("notice path has parent"))?;
        fs::copy(path, destination)?;
    }
    notice_bundle::verify(&notice_dir, &staged_binary, request.target, request.version)?;
    if notice_bundle::file_digest(&notice_dir.join(notice_bundle::MANIFEST))? != notice_manifest {
        return Err(DistError::Notice(
            "notice bundle changed while packaging".to_owned(),
        ));
    }

    let manifest = DistributionManifestV2 {
        schema_version: "tidas.distribution-manifest.v2".to_owned(),
        product: "tidas".to_owned(),
        version: request.version.to_owned(),
        target: request.target.to_owned(),
        executable: format!("bin/{binary_name}"),
        self_contained_native_xml: true,
        third_party_notices: notice_manifest,
    };
    let mut manifest_bytes = serde_json::to_vec_pretty(&manifest)?;
    manifest_bytes.push(b'\n');
    fs::write(root.join("distribution-manifest.json"), manifest_bytes)?;

    let archive_name = archive_name(request.version, request.target);
    let archive = request.output_dir.join(&archive_name);
    if request.target == WINDOWS_TARGET {
        write_zip(staging.path(), &archive)?;
    } else {
        write_tar_gz(staging.path(), &archive)?;
    }
    let bytes = fs::metadata(&archive)?.len();
    let sha256 = sha256_file(&archive)?;
    let checksum_file = request.output_dir.join(format!("{archive_name}.sha256"));
    fs::write(&checksum_file, format!("{sha256}  {archive_name}\n"))?;

    Ok(DistributionArtifactV1 {
        schema_version: "tidas.distribution-artifact.v1".to_owned(),
        archive,
        checksum_file,
        sha256,
        bytes,
        target: request.target.to_owned(),
        version: request.version.to_owned(),
    })
}

pub fn verify(
    archive: &Path,
    checksum_file: &Path,
    expected_target: &str,
    expected_version: &str,
    smoke: bool,
) -> Result<DistributionManifestV2, DistError> {
    validate_target(expected_target)?;
    require_file(archive)?;
    require_file(checksum_file)?;
    verify_checksum(archive, checksum_file)?;

    let extracted = TempDir::new()?;
    if expected_target == WINDOWS_TARGET {
        extract_zip(archive, extracted.path())?;
    } else {
        extract_tar_gz(archive, extracted.path())?;
    }
    let entries = fs::read_dir(extracted.path())?.collect::<Result<Vec<_>, _>>()?;
    if entries.len() != 1
        || entries[0].file_name()
            != std::ffi::OsStr::new(&archive_root(expected_version, expected_target))
    {
        return Err(DistError::Notice(
            "archive has an unexpected distribution root".to_owned(),
        ));
    }
    let root = extracted
        .path()
        .join(archive_root(expected_version, expected_target));
    let manifest_path = root.join("distribution-manifest.json");
    if !manifest_path.is_file() {
        return Err(DistError::MissingManifest);
    }
    let manifest: DistributionManifestV2 = serde_json::from_slice(&fs::read(&manifest_path)?)?;
    if manifest.target != expected_target
        || manifest.version != expected_version
        || manifest.schema_version != "tidas.distribution-manifest.v2"
        || manifest.product != "tidas"
        || !manifest.self_contained_native_xml
        || manifest.executable != format!("bin/{}", executable_name(expected_target))
    {
        return Err(DistError::ManifestMismatch {
            expected: format!("{expected_version}/{expected_target}"),
            found: format!("{}/{}", manifest.version, manifest.target),
        });
    }
    let executable = root.join(&manifest.executable);
    require_file(&executable)?;
    verify_packaged_notices(&root, &manifest, &executable)?;
    if smoke {
        run_smoke(&executable)?;
    }
    Ok(manifest)
}

fn verify_packaged_notices(
    root: &Path,
    manifest: &DistributionManifestV2,
    executable: &Path,
) -> Result<(), DistError> {
    let prefix = "share/licenses/tidas/third-party-notices";
    let notice_dir = root.join(prefix);
    if notice_bundle::file_digest(&notice_dir.join(notice_bundle::MANIFEST))?
        != manifest.third_party_notices
    {
        return Err(DistError::Notice(
            "packaged notice manifest digest differs".to_owned(),
        ));
    }
    let notices =
        notice_bundle::verify(&notice_dir, executable, &manifest.target, &manifest.version)?;
    let license_digest = sha256_file(&root.join("share/licenses/tidas/LICENSE"))?;
    if !notices
        .cargo_packages
        .iter()
        .filter(|package| package.name == "tidas")
        .flat_map(|package| &package.texts)
        .any(|text| text.kind == "project-license" && text.sha256 == license_digest)
    {
        return Err(DistError::Notice(
            "packaged project license differs from notice evidence".to_owned(),
        ));
    }
    let mut expected = std::collections::BTreeSet::from([
        manifest.executable.clone(),
        "distribution-manifest.json".to_owned(),
        "share/licenses/tidas/LICENSE".to_owned(),
        format!("{prefix}/{}", notice_bundle::MANIFEST),
    ]);
    expected.extend(notices.files.keys().map(|path| format!("{prefix}/{path}")));
    let actual = collect_files(root)?
        .into_iter()
        .map(|(path, _, _)| path)
        .collect();
    if expected != actual {
        return Err(DistError::Notice(
            "native archive contains an unexpected file inventory".to_owned(),
        ));
    }
    Ok(())
}

pub fn render_package_metadata(
    version: &str,
    release_base_url: &str,
    artifacts_dir: &Path,
    output_dir: &Path,
) -> Result<Vec<PathBuf>, DistError> {
    fs::create_dir_all(output_dir)?;
    let checksums = release_checksums(version, artifacts_dir)?;
    let formula_dir = output_dir.join("homebrew");
    fs::create_dir_all(&formula_dir)?;
    let formula_path = formula_dir.join("tidas.rb");
    fs::write(
        &formula_path,
        homebrew_formula(version, release_base_url, &checksums),
    )?;

    let winget_dir = output_dir.join("winget");
    fs::create_dir_all(&winget_dir)?;
    let windows_archive = archive_name(version, WINDOWS_TARGET);
    let windows_sha = checksums
        .get(WINDOWS_TARGET)
        .ok_or_else(|| DistError::MissingChecksum(windows_archive.clone()))?;
    let version_path = winget_dir.join("TianGong.Tidas.yaml");
    let installer_path = winget_dir.join("TianGong.Tidas.installer.yaml");
    let locale_path = winget_dir.join("TianGong.Tidas.locale.en-US.yaml");
    fs::write(&version_path, winget_version_manifest(version))?;
    fs::write(
        &installer_path,
        winget_installer_manifest(
            version,
            &format!("{release_base_url}/{windows_archive}"),
            windows_sha,
        ),
    )?;
    fs::write(&locale_path, winget_locale_manifest(version))?;

    Ok(vec![
        formula_path,
        version_path,
        installer_path,
        locale_path,
    ])
}

#[must_use]
pub fn supported_targets() -> &'static [&'static str] {
    &REQUIRED_TARGETS
}

fn validate_target(target: &str) -> Result<(), DistError> {
    if REQUIRED_TARGETS.contains(&target) {
        Ok(())
    } else {
        Err(DistError::UnsupportedTarget(target.to_owned()))
    }
}

fn require_file(path: &Path) -> Result<(), DistError> {
    if path.is_file() {
        Ok(())
    } else {
        Err(DistError::MissingInput(path.to_path_buf()))
    }
}

fn archive_root(version: &str, target: &str) -> String {
    format!("tidas-v{version}-{target}")
}

fn archive_name(version: &str, target: &str) -> String {
    let extension = if target == WINDOWS_TARGET {
        "zip"
    } else {
        "tar.gz"
    };
    format!("{}.{extension}", archive_root(version, target))
}

fn executable_name(target: &str) -> &'static str {
    if target == WINDOWS_TARGET {
        "tidas.exe"
    } else {
        "tidas"
    }
}

#[cfg(unix)]
fn set_executable(path: &Path) -> Result<(), DistError> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))?;
    Ok(())
}

fn collect_files(root: &Path) -> Result<Vec<(String, PathBuf, u32)>, DistError> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).follow_links(false) {
        let entry = entry?;
        if entry.file_type().is_symlink()
            || (!entry.file_type().is_file() && !entry.file_type().is_dir())
        {
            return Err(DistError::UnsafePath(entry.path().to_path_buf()));
        }
        if entry.file_type().is_dir() {
            continue;
        }
        let relative = entry
            .path()
            .strip_prefix(root)
            .map_err(|_| DistError::UnsafePath(entry.path().to_path_buf()))?;
        validate_relative(relative)?;
        let name = portable(relative)?;
        let mode = if name.ends_with("/bin/tidas") || name.ends_with("/bin/tidas.exe") {
            0o755
        } else {
            0o644
        };
        files.push((name, entry.path().to_path_buf(), mode));
    }
    files.sort_by(|left, right| left.0.cmp(&right.0));
    Ok(files)
}

fn write_zip(root: &Path, output: &Path) -> Result<(), DistError> {
    let file = File::create(output)?;
    let mut writer = zip::ZipWriter::new(file);
    let files = collect_files(root)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    for (name, path, mode) in files {
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Deflated)
            .compression_level(Some(9))
            .last_modified_time(zip::DateTime::default())
            .unix_permissions(mode);
        writer.start_file(name, options)?;
        copy_file(&path, &mut writer, &mut buffer)?;
    }
    let finished = writer.finish()?;
    finished.sync_all()?;
    Ok(())
}

fn write_tar_gz(root: &Path, output: &Path) -> Result<(), DistError> {
    let file = File::create(output)?;
    let gzip = GzBuilder::new().mtime(0).write(file, Compression::best());
    let mut writer = tar::Builder::new(gzip);
    writer.mode(tar::HeaderMode::Deterministic);
    let files = collect_files(root)?;
    for (name, path, mode) in files {
        let metadata = fs::metadata(&path)?;
        let mut header = tar::Header::new_gnu();
        header.set_size(metadata.len());
        header.set_mode(mode);
        header.set_uid(0);
        header.set_gid(0);
        header.set_mtime(0);
        header.set_cksum();
        let mut source = File::open(path)?;
        writer.append_data(&mut header, name, &mut source)?;
    }
    let gzip = writer.into_inner()?;
    let finished = gzip.finish()?;
    finished.sync_all()?;
    Ok(())
}

fn copy_file<W: Write>(path: &Path, writer: &mut W, buffer: &mut [u8]) -> Result<(), DistError> {
    let mut source = File::open(path)?;
    loop {
        let read = source.read(buffer)?;
        if read == 0 {
            break;
        }
        writer.write_all(&buffer[..read])?;
    }
    Ok(())
}

fn validate_relative(path: &Path) -> Result<(), DistError> {
    if path.as_os_str().is_empty()
        || path
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(DistError::UnsafePath(path.to_path_buf()));
    }
    Ok(())
}

fn portable(path: &Path) -> Result<String, DistError> {
    path.components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| DistError::UnsafePath(path.to_path_buf()))
        })
        .collect::<Result<Vec<_>, _>>()
        .map(|parts| parts.join("/"))
}

fn sha256_file(path: &Path) -> Result<String, DistError> {
    let mut file = File::open(path)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    let mut hasher = Sha256::new();
    loop {
        let read = file.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(hex(hasher.finalize()))
}

fn verify_checksum(archive: &Path, checksum_file: &Path) -> Result<(), DistError> {
    let file_name = archive
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| DistError::UnsafePath(archive.to_path_buf()))?;
    let line = fs::read_to_string(checksum_file)?;
    let (expected, listed_name) = line
        .trim()
        .split_once("  ")
        .ok_or_else(|| DistError::InvalidChecksum(line.clone()))?;
    if listed_name != file_name || expected.len() != 64 {
        return Err(DistError::InvalidChecksum(line));
    }
    let found = sha256_file(archive)?;
    if expected == found {
        Ok(())
    } else {
        Err(DistError::ChecksumMismatch {
            expected: expected.to_owned(),
            found,
        })
    }
}

fn extract_zip(archive: &Path, output: &Path) -> Result<(), DistError> {
    let mut reader = zip::ZipArchive::new(File::open(archive)?)?;
    if reader.len() > 16384 {
        return Err(DistError::Notice(
            "archive member bound exceeded".to_owned(),
        ));
    }
    let mut paths = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for index in 0..reader.len() {
        let mut member = reader.by_index(index)?;
        let relative = member.enclosed_name().ok_or_else(|| match member.name() {
            Ok(name) => DistError::UnsafePath(PathBuf::from(name.into_owned())),
            Err(error) => DistError::from(error),
        })?;
        validate_relative(&relative)?;
        let mode = member.unix_mode().unwrap_or(0) & 0o170_000;
        if !paths.insert(portable(&relative)?) || ![0, 0o100_000, 0o040_000].contains(&mode) {
            return Err(DistError::UnsafePath(relative));
        }
        let destination = output.join(&relative);
        if member.is_dir() {
            fs::create_dir_all(destination)?;
            continue;
        }
        let size = member.size();
        extract_regular(&mut member, &destination, size, &mut total)?;
    }
    Ok(())
}

fn extract_tar_gz(archive: &Path, output: &Path) -> Result<(), DistError> {
    let reader = flate2::read::GzDecoder::new(File::open(archive)?);
    let mut archive = tar::Archive::new(reader);
    let mut paths = std::collections::BTreeSet::new();
    let mut total = 0_u64;
    for entry in archive.entries()? {
        let mut entry = entry?;
        let relative = entry.path()?.into_owned();
        validate_relative(&relative)?;
        if !paths.insert(portable(&relative)?) || paths.len() > 16384 {
            return Err(DistError::UnsafePath(relative));
        }
        let destination = output.join(&relative);
        if entry.header().entry_type().is_dir() {
            fs::create_dir_all(destination)?;
            continue;
        }
        if !entry.header().entry_type().is_file() {
            return Err(DistError::UnsafePath(relative));
        }
        let size = entry.size();
        extract_regular(&mut entry, &destination, size, &mut total)?;
    }
    Ok(())
}

fn extract_regular(
    reader: &mut impl Read,
    destination: &Path,
    size: u64,
    total: &mut u64,
) -> Result<(), DistError> {
    *total = total.checked_add(size).ok_or(DistError::SizeOverflow)?;
    if *total > 512 * 1024 * 1024 {
        return Err(DistError::Notice(
            "native archive size bound exceeded".to_owned(),
        ));
    }
    fs::create_dir_all(
        destination
            .parent()
            .ok_or_else(|| DistError::UnsafePath(destination.to_path_buf()))?,
    )?;
    let mut file = File::options()
        .write(true)
        .create_new(true)
        .open(destination)?;
    let copied = std::io::copy(&mut reader.take(size + 1), &mut file)?;
    if copied != size {
        return Err(DistError::Notice(
            "archive member length differs".to_owned(),
        ));
    }
    #[cfg(unix)]
    if destination
        .file_name()
        .is_some_and(|name| name == "tidas" || name == "tidas.exe")
    {
        set_executable(destination)?;
    }
    Ok(())
}

fn run_smoke(executable: &Path) -> Result<(), DistError> {
    for arguments in [
        vec!["--version"],
        vec!["--help"],
        vec!["--format", "json", "version"],
        vec!["--format", "json", "ruleset"],
    ] {
        let output = Command::new(executable).args(&arguments).output()?;
        if !output.status.success() {
            return Err(DistError::SmokeFailed(format!(
                "{} {} (status {}; stderr: {})",
                executable.display(),
                arguments.join(" "),
                output.status,
                String::from_utf8_lossy(&output.stderr).trim()
            )));
        }
        if output.stdout.is_empty() {
            return Err(DistError::SmokeFailed(format!(
                "{} {} produced empty stdout",
                executable.display(),
                arguments.join(" ")
            )));
        }
    }
    Ok(())
}

fn release_checksums(
    version: &str,
    artifacts_dir: &Path,
) -> Result<BTreeMap<String, String>, DistError> {
    let mut checksums = BTreeMap::new();
    for target in REQUIRED_TARGETS {
        let archive = archive_name(version, target);
        let checksum_path = artifacts_dir.join(format!("{archive}.sha256"));
        let line = fs::read_to_string(&checksum_path)
            .map_err(|_| DistError::MissingChecksum(archive.clone()))?;
        let (sha256, listed_name) = line
            .trim()
            .split_once("  ")
            .ok_or_else(|| DistError::InvalidChecksum(line.clone()))?;
        if listed_name != archive || sha256.len() != 64 {
            return Err(DistError::InvalidChecksum(line));
        }
        checksums.insert(target.to_owned(), sha256.to_owned());
    }
    Ok(checksums)
}

fn homebrew_formula(
    version: &str,
    release_base_url: &str,
    checksums: &BTreeMap<String, String>,
) -> String {
    let url = |target: &str| format!("{release_base_url}/{}", archive_name(version, target));
    let sha = |target: &str| checksums.get(target).expect("all targets were validated");
    format!(
        r##"class Tidas < Formula
  desc "Cross-platform TIDAS conversion, import, export, validation, and release CLI"
  homepage "https://github.com/tiangong-lca/tidas-toolkit"
  version "{version}"
  license "MIT"

  on_macos do
    if Hardware::CPU.arm?
      url "{mac_arm_url}"
      sha256 "{mac_arm_sha}"
    end
  end

  on_linux do
    if Hardware::CPU.arm? && Hardware::CPU.is_64_bit?
      url "{linux_arm_url}"
      sha256 "{linux_arm_sha}"
    else
      url "{linux_x64_url}"
      sha256 "{linux_x64_sha}"
    end
  end

  def install
    bin.install "bin/tidas"
  end

  test do
    assert_match version.to_s, shell_output("#{{bin}}/tidas --version")
    assert_match "\"status\":\"succeeded\"", shell_output("#{{bin}}/tidas --format json version")
  end
end
"##,
        mac_arm_url = url("aarch64-apple-darwin"),
        mac_arm_sha = sha("aarch64-apple-darwin"),
        linux_arm_url = url("aarch64-unknown-linux-gnu"),
        linux_arm_sha = sha("aarch64-unknown-linux-gnu"),
        linux_x64_url = url("x86_64-unknown-linux-gnu"),
        linux_x64_sha = sha("x86_64-unknown-linux-gnu"),
    )
}

fn winget_version_manifest(version: &str) -> String {
    format!(
        r"# Generated from the exact tidas release archive; do not hand-edit.
# yaml-language-server: $schema=https://aka.ms/winget-manifest.version.1.10.0.schema.json

PackageIdentifier: TianGong.Tidas
PackageVersion: {version}
DefaultLocale: en-US
ManifestType: version
ManifestVersion: 1.10.0
"
    )
}

fn winget_installer_manifest(version: &str, url: &str, sha256: &str) -> String {
    format!(
        r"# Generated from the exact tidas release archive; do not hand-edit.
# yaml-language-server: $schema=https://aka.ms/winget-manifest.installer.1.10.0.schema.json

PackageIdentifier: TianGong.Tidas
PackageVersion: {version}
InstallerType: zip
NestedInstallerType: portable
NestedInstallerFiles:
  - RelativeFilePath: tidas-v{version}-x86_64-pc-windows-msvc\bin\tidas.exe
    PortableCommandAlias: tidas
Installers:
  - Architecture: x64
    InstallerUrl: {url}
    InstallerSha256: {sha256}
ManifestType: installer
ManifestVersion: 1.10.0
"
    )
}

fn winget_locale_manifest(version: &str) -> String {
    format!(
        r"# Generated from the exact tidas release archive; do not hand-edit.
# yaml-language-server: $schema=https://aka.ms/winget-manifest.defaultLocale.1.10.0.schema.json

PackageIdentifier: TianGong.Tidas
PackageVersion: {version}
PackageLocale: en-US
Publisher: TianGong LCA
PackageName: tidas
License: MIT
ShortDescription: Cross-platform TIDAS conversion, import, export, validation, and release CLI.
PackageUrl: https://github.com/tiangong-lca/tidas-toolkit
ManifestType: defaultLocale
ManifestVersion: 1.10.0
"
    )
}

fn hex(digest: impl AsRef<[u8]>) -> String {
    let bytes = digest.as_ref();
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

#[cfg(test)]
#[path = "../tests/support/notice_fixture.rs"]
mod notice_fixture;

#[cfg(test)]
mod tests {
    use super::*;

    fn fake_binary(path: &Path) {
        fs::write(path, b"native tidas fixture\n").unwrap();
    }

    fn zip_with_raw_name(name: &[u8], content: &[u8]) -> Vec<u8> {
        let placeholder = "x".repeat(name.len());
        let decoded = std::str::from_utf8(name).ok();
        let mut writer = zip::ZipWriter::new(std::io::Cursor::new(Vec::new()));
        writer
            .start_file(
                decoded.unwrap_or(placeholder.as_str()),
                SimpleFileOptions::default(),
            )
            .unwrap();
        writer.write_all(content).unwrap();
        let mut bytes = writer.finish().unwrap().into_inner();
        if decoded.is_some() {
            return bytes;
        }
        let offsets = bytes
            .windows(placeholder.len())
            .enumerate()
            .filter_map(|(offset, value)| (value == placeholder.as_bytes()).then_some(offset))
            .collect::<Vec<_>>();
        assert_eq!(offsets.len(), 2, "local and central entry names");
        for offset in offsets {
            bytes[offset..offset + name.len()].copy_from_slice(name);
        }
        bytes
    }

    #[test]
    fn zip_extraction_preserves_utf8_and_legacy_cp437_names() {
        let temporary = tempfile::tempdir().unwrap();
        let archive = temporary.path().join("source.zip");
        for (index, (raw_name, expected)) in [
            ("目录/记录.json".as_bytes(), "目录/记录.json"),
            (b"\x82.json".as_slice(), "é.json"),
        ]
        .into_iter()
        .enumerate()
        {
            fs::write(&archive, zip_with_raw_name(raw_name, b"{}\n")).unwrap();
            let output = temporary.path().join(index.to_string());
            fs::create_dir(&output).unwrap();
            extract_zip(&archive, &output).unwrap();
            assert_eq!(fs::read(output.join(expected)).unwrap(), b"{}\n");
        }
    }

    #[test]
    fn zip_extraction_rejects_unsafe_legacy_names_without_writing() {
        let temporary = tempfile::tempdir().unwrap();
        let archive = temporary.path().join("source.zip");
        let output = temporary.path().join("extracted");
        fs::create_dir(&output).unwrap();
        fs::write(&archive, zip_with_raw_name(b"../\x82.json", b"{}")).unwrap();
        assert!(matches!(
            extract_zip(&archive, &output),
            Err(DistError::UnsafePath(path)) if path == Path::new("../é.json")
        ));
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
        assert!(!temporary.path().join("é.json").exists());
    }

    #[test]
    fn zip_extraction_reports_corrupt_entries_without_writing() {
        let temporary = tempfile::tempdir().unwrap();
        let archive = temporary.path().join("source.zip");
        let output = temporary.path().join("extracted");
        fs::create_dir(&output).unwrap();
        let mut bytes = zip_with_raw_name(b"record.json", b"{}");
        bytes[0] ^= 1;
        fs::write(&archive, bytes).unwrap();
        assert!(matches!(
            extract_zip(&archive, &output),
            Err(DistError::Zip(_))
        ));
        assert_eq!(fs::read_dir(&output).unwrap().count(), 0);
    }

    #[test]
    fn archives_and_checksums_are_byte_repeatable() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("tidas");
        let license = temporary.path().join("LICENSE");
        fake_binary(&binary);
        fs::write(&license, b"MIT\n").unwrap();
        let first_dir = temporary.path().join("first");
        let second_dir = temporary.path().join("second");
        let notices_dir = temporary.path().join("notices");
        notice_fixture::create(
            &notices_dir,
            &binary,
            &license,
            "x86_64-unknown-linux-gnu",
            "0.1.0",
        );
        let first = package(&PackageRequest {
            binary: &binary,
            license: &license,
            notices_dir: &notices_dir,
            target: "x86_64-unknown-linux-gnu",
            version: "0.1.0",
            output_dir: &first_dir,
        })
        .unwrap();
        let second = package(&PackageRequest {
            binary: &binary,
            license: &license,
            notices_dir: &notices_dir,
            target: "x86_64-unknown-linux-gnu",
            version: "0.1.0",
            output_dir: &second_dir,
        })
        .unwrap();
        assert_eq!(first.sha256, second.sha256);
        assert_eq!(
            fs::read(first.archive).unwrap(),
            fs::read(second.archive).unwrap()
        );
        assert_eq!(
            fs::read(first.checksum_file).unwrap(),
            fs::read(second.checksum_file).unwrap()
        );
    }

    #[test]
    fn verifier_rejects_checksum_drift_before_extraction() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("tidas.exe");
        let license = temporary.path().join("LICENSE");
        fake_binary(&binary);
        fs::write(&license, b"MIT\n").unwrap();
        let notices_dir = temporary.path().join("notices");
        notice_fixture::create(&notices_dir, &binary, &license, WINDOWS_TARGET, "0.1.0");
        let artifact = package(&PackageRequest {
            binary: &binary,
            license: &license,
            notices_dir: &notices_dir,
            target: WINDOWS_TARGET,
            version: "0.1.0",
            output_dir: temporary.path(),
        })
        .unwrap();
        let mut bytes = fs::read(&artifact.archive).unwrap();
        bytes[0] ^= 1;
        fs::write(&artifact.archive, bytes).unwrap();
        assert!(matches!(
            verify(
                &artifact.archive,
                &artifact.checksum_file,
                WINDOWS_TARGET,
                "0.1.0",
                false
            ),
            Err(DistError::ChecksumMismatch { .. })
        ));
    }

    #[test]
    fn supported_targets_are_exactly_the_four_approved_tuples() {
        assert_eq!(
            supported_targets(),
            &[
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
                "aarch64-apple-darwin",
                "x86_64-pc-windows-msvc",
            ]
        );
    }

    #[test]
    fn packaging_rejects_the_retired_macos_intel_target() {
        let temporary = tempfile::tempdir().unwrap();
        let binary = temporary.path().join("tidas");
        let license = temporary.path().join("LICENSE");
        fake_binary(&binary);
        fs::write(&license, b"MIT\n").unwrap();
        let result = package(&PackageRequest {
            binary: &binary,
            license: &license,
            notices_dir: temporary.path(),
            target: "x86_64-apple-darwin",
            version: "0.1.0",
            output_dir: temporary.path(),
        });
        assert!(
            matches!(result, Err(DistError::UnsupportedTarget(target)) if target == "x86_64-apple-darwin")
        );
    }

    #[test]
    fn metadata_uses_the_four_exact_archive_checksums() {
        let temporary = tempfile::tempdir().unwrap();
        for (index, target) in REQUIRED_TARGETS.iter().enumerate() {
            let archive = archive_name("0.1.0", target);
            fs::write(
                temporary.path().join(format!("{archive}.sha256")),
                format!("{:064x}  {archive}\n", index + 1),
            )
            .unwrap();
        }
        let output = temporary.path().join("metadata");
        let paths = render_package_metadata(
            "0.1.0",
            "https://example.invalid/v0.1.0",
            temporary.path(),
            &output,
        )
        .unwrap();
        assert_eq!(paths.len(), 4);
        let formula = fs::read_to_string(output.join("homebrew/tidas.rb")).unwrap();
        assert!(formula.contains("aarch64-apple-darwin"));
        assert!(!formula.contains("x86_64-apple-darwin"));
        assert!(formula.contains(&format!("{:064x}", 1)));
        let winget_version = fs::read_to_string(output.join("winget/TianGong.Tidas.yaml")).unwrap();
        assert!(
            winget_version
                .contains("$schema=https://aka.ms/winget-manifest.version.1.10.0.schema.json")
        );
        let winget =
            fs::read_to_string(output.join("winget/TianGong.Tidas.installer.yaml")).unwrap();
        assert!(
            winget.contains("$schema=https://aka.ms/winget-manifest.installer.1.10.0.schema.json")
        );
        assert!(winget.contains("Architecture: x64"));
        assert!(
            winget
                .contains("RelativeFilePath: tidas-v0.1.0-x86_64-pc-windows-msvc\\bin\\tidas.exe")
        );
        assert!(winget.contains(&format!("{:064x}", 4)));
        assert!(!winget.contains(&format!("{:064x}", 5)));
        let winget_locale =
            fs::read_to_string(output.join("winget/TianGong.Tidas.locale.en-US.yaml")).unwrap();
        assert!(
            winget_locale.contains(
                "$schema=https://aka.ms/winget-manifest.defaultLocale.1.10.0.schema.json"
            )
        );
    }
}
