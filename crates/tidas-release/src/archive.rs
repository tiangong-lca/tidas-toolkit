use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};
use walkdir::WalkDir;
use zip::CompressionMethod;
use zip::write::SimpleFileOptions;

use crate::closure::ReleaseProfile;
use crate::index::{DatasetEntry, contained, hex_digest, safe_relative, sha256_file};
use crate::{
    ReferenceClosureReportV1, ReleaseArtifactV1, ReleaseDataFormat, ReleaseError, ReleasePackageV1,
    ReleaseRuntime,
};

const COPY_BUFFER_BYTES: usize = 1024 * 1024;
const MEMBER_MEMORY_BYTES: u64 = 512;

pub(crate) struct PackageRequest<'a> {
    pub profile: ReleaseProfile,
    pub format: ReleaseDataFormat,
    pub entries: &'a [DatasetEntry],
    pub tidas_root: &'a Path,
    pub ilcd_root: &'a Path,
    pub output_dir: &'a Path,
    pub closure: &'a ReferenceClosureReportV1,
    pub runtime: &'a ReleaseRuntime,
}

pub(crate) fn write_package(
    request: &PackageRequest<'_>,
) -> Result<ReleasePackageV1, ReleaseError> {
    request.runtime.cancellation.check()?;
    let members = package_members(
        request.format,
        request.entries,
        request.tidas_root,
        request.ilcd_root,
        request.runtime,
    )?;
    let format_name = match request.format {
        ReleaseDataFormat::Tidas => "tidas",
        ReleaseDataFormat::Ilcd => "ilcd",
    };
    let output = request
        .output_dir
        .join(format!("{}.{}.zip", request.profile.id(), format_name));
    let file = File::create(&output)?;
    let mut writer = zip::ZipWriter::new(file);
    let base_options = SimpleFileOptions::default()
        .compression_method(CompressionMethod::Stored)
        .last_modified_time(zip::DateTime::default())
        .unix_permissions(0o644);
    let buffer_size = u64::try_from(COPY_BUFFER_BYTES).map_err(|_| ReleaseError::SizeOverflow)?;
    let _reservation = request.runtime.memory_budget.reserve(buffer_size)?;
    let mut buffer = vec![0_u8; COPY_BUFFER_BYTES];
    for (name, path) in &members.entries {
        request.runtime.cancellation.check()?;
        let metadata = fs::metadata(path)?;
        let options = base_options.large_file(metadata.len() > u64::from(u32::MAX));
        writer.start_file(name, options)?;
        let mut source = File::open(path)?;
        loop {
            request.runtime.cancellation.check()?;
            let read = source.read(&mut buffer)?;
            if read == 0 {
                break;
            }
            writer.write_all(&buffer[..read])?;
        }
    }
    let finished = writer.finish()?;
    finished.sync_all()?;
    let bytes = fs::metadata(&output)?.len();
    let sha256 = sha256_file(&output, request.runtime)?;
    Ok(ReleasePackageV1 {
        profile_id: request.profile.id().to_owned(),
        format: request.format,
        self_contained: true,
        closure_sha256: request.closure.closure_sha256.clone(),
        dataset_count: request.closure.dataset_count,
        artifact: ReleaseArtifactV1 {
            path: output.to_string_lossy().into_owned(),
            media_type: "application/zip".to_owned(),
            sha256,
            bytes,
            member_count: u64::try_from(members.entries.len())
                .map_err(|_| ReleaseError::SizeOverflow)?,
        },
    })
}

pub(crate) fn artifact_set_hash(packages: &[ReleasePackageV1]) -> String {
    let mut hasher = Sha256::new();
    for package in packages {
        let format = match package.format {
            ReleaseDataFormat::Tidas => "tidas",
            ReleaseDataFormat::Ilcd => "ilcd",
        };
        for value in [
            package.profile_id.as_str(),
            format,
            package.artifact.sha256.as_str(),
        ] {
            hasher.update(value.len().to_le_bytes());
            hasher.update(value.as_bytes());
        }
    }
    hex_digest(hasher.finalize())
}

fn package_members(
    format: ReleaseDataFormat,
    entries: &[DatasetEntry],
    tidas_root: &Path,
    ilcd_root: &Path,
    runtime: &ReleaseRuntime,
) -> Result<MemberCatalog, ReleaseError> {
    let asset_count = if format == ReleaseDataFormat::Ilcd {
        WalkDir::new(ilcd_root)
            .follow_links(false)
            .into_iter()
            .filter_map(Result::ok)
            .filter(|entry| {
                entry.file_type().is_file()
                    && entry
                        .path()
                        .strip_prefix(ilcd_root)
                        .is_ok_and(|path| !path.starts_with("data"))
            })
            .count()
    } else {
        0
    };
    let dataset_member_capacity = if format == ReleaseDataFormat::Ilcd {
        entries
            .len()
            .checked_mul(3)
            .ok_or(ReleaseError::SizeOverflow)?
    } else {
        entries.len()
    };
    let member_count = dataset_member_capacity
        .checked_add(asset_count)
        .ok_or(ReleaseError::SizeOverflow)?;
    let reserved = u64::try_from(member_count)
        .map_err(|_| ReleaseError::SizeOverflow)?
        .checked_mul(MEMBER_MEMORY_BYTES)
        .ok_or(ReleaseError::SizeOverflow)?;
    let reservation = runtime.memory_budget.reserve(reserved)?;
    let mut members = BTreeMap::new();
    for entry in entries {
        runtime.cancellation.check()?;
        let (name, path) = match format {
            ReleaseDataFormat::Tidas => (
                entry.relative_path.clone(),
                contained(tidas_root, &entry.relative_path)?,
            ),
            ReleaseDataFormat::Ilcd => {
                let xml_relative = Path::new(&entry.relative_path).with_extension("xml");
                let name = format!("data/{}", portable(&xml_relative)?);
                let path = ilcd_root.join(&name);
                (name, path)
            }
        };
        add_member(&mut members, name, path)?;
        if format == ReleaseDataFormat::Ilcd {
            let xml_relative = Path::new(&entry.relative_path).with_extension("xml");
            for suffix in ["tidas-envelope.json", "tidas-recovery.json"] {
                let sidecar_relative = conversion_sidecar_path(&xml_relative, suffix);
                let sidecar_name = format!("data/{}", portable(&sidecar_relative)?);
                let sidecar_path = ilcd_root.join(&sidecar_name);
                if sidecar_path.is_file() {
                    add_member(&mut members, sidecar_name, sidecar_path)?;
                }
            }
        }
    }
    if format == ReleaseDataFormat::Ilcd {
        for item in WalkDir::new(ilcd_root).follow_links(false) {
            runtime.cancellation.check()?;
            let item = item?;
            if item.file_type().is_symlink() {
                return Err(ReleaseError::Symlink(item.into_path()));
            }
            if !item.file_type().is_file() {
                continue;
            }
            let relative = item
                .path()
                .strip_prefix(ilcd_root)
                .map_err(|_| ReleaseError::PathOutsideRoot(item.path().to_path_buf()))?;
            if relative.starts_with("data") {
                continue;
            }
            add_member(&mut members, portable(relative)?, item.into_path())?;
        }
    }
    Ok(MemberCatalog {
        entries: members,
        _reservation: reservation,
    })
}

fn conversion_sidecar_path(path: &Path, suffix: &str) -> PathBuf {
    let stem = path
        .file_stem()
        .map_or_else(|| path.as_os_str().to_owned(), std::ffi::OsStr::to_owned);
    let mut name = stem;
    name.push(".");
    name.push(suffix);
    path.with_file_name(name)
}

struct MemberCatalog {
    entries: BTreeMap<String, PathBuf>,
    _reservation: tidas_runtime::MemoryReservation,
}

fn add_member(
    members: &mut BTreeMap<String, PathBuf>,
    name: String,
    path: PathBuf,
) -> Result<(), ReleaseError> {
    safe_relative(&name)?;
    if !path.is_file() {
        return Err(ReleaseError::DatasetFileMissing(
            path.to_string_lossy().into_owned(),
        ));
    }
    if members.insert(name.clone(), path).is_some() {
        return Err(ReleaseError::DuplicateArchiveMember(name));
    }
    Ok(())
}

fn portable(path: &Path) -> Result<String, ReleaseError> {
    let name = path
        .components()
        .map(|component| {
            component
                .as_os_str()
                .to_str()
                .map(ToOwned::to_owned)
                .ok_or_else(|| ReleaseError::UnsafePath(path.to_path_buf()))
        })
        .collect::<Result<Vec<_>, _>>()?
        .join("/");
    safe_relative(&name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::UNIT_PROFILE;
    use tidas_runtime::{CancellationToken, MemoryBudget};

    #[test]
    fn stored_archive_bytes_names_timestamps_and_permissions_are_repeatable() {
        let temporary = tempfile::tempdir().unwrap();
        let source = temporary.path().join("tidas");
        fs::create_dir(&source).unwrap();
        let dataset = source.join("record.json");
        fs::write(&dataset, b"{\"record\":true}\n").unwrap();
        let entry = DatasetEntry {
            dataset_type: "process".to_owned(),
            role: "unit_process".to_owned(),
            uuid: "11111111-1111-4111-8111-111111111111".to_owned(),
            version: "01.00.000".to_owned(),
            relative_path: "record.json".to_owned(),
            sha256: String::new(),
            canonical_content_hash: String::new(),
        };
        let closure = ReferenceClosureReportV1 {
            profile_id: UNIT_PROFILE.to_owned(),
            root_count: 1,
            dataset_count: 1,
            reference_count: 0,
            closure_sha256: "0".repeat(64),
            dataset_keys: vec![entry.key()],
            dataset_keys_truncated: false,
            semantic_coverage: None,
            semantic_diagnostics: None,
        };
        let runtime = ReleaseRuntime {
            cancellation: CancellationToken::default(),
            memory_budget: MemoryBudget::new(8 * 1024 * 1024),
            queue_capacity: 8,
        };
        let first_dir = temporary.path().join("first");
        let second_dir = temporary.path().join("second");
        fs::create_dir(&first_dir).unwrap();
        fs::create_dir(&second_dir).unwrap();
        let first = write_package(&PackageRequest {
            profile: ReleaseProfile::UnitProcess,
            format: ReleaseDataFormat::Tidas,
            entries: std::slice::from_ref(&entry),
            tidas_root: &source,
            ilcd_root: temporary.path(),
            output_dir: &first_dir,
            closure: &closure,
            runtime: &runtime,
        })
        .unwrap();
        let second = write_package(&PackageRequest {
            profile: ReleaseProfile::UnitProcess,
            format: ReleaseDataFormat::Tidas,
            entries: &[entry],
            tidas_root: &source,
            ilcd_root: temporary.path(),
            output_dir: &second_dir,
            closure: &closure,
            runtime: &runtime,
        })
        .unwrap();
        assert_eq!(first.artifact.sha256, second.artifact.sha256);
        assert_eq!(
            fs::read(&first.artifact.path).unwrap(),
            fs::read(&second.artifact.path).unwrap()
        );

        let file = File::open(&second.artifact.path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        assert_eq!(archive.len(), 1);
        let member = archive.by_index(0).unwrap();
        assert_eq!(member.name().unwrap(), "record.json");
        assert_eq!(member.last_modified(), Some(zip::DateTime::default()));
        assert_eq!(member.unix_mode(), Some(0o100_644));
    }

    #[test]
    fn ilcd_archive_carries_lossless_sidecars_beside_adapted_xml() {
        let temporary = tempfile::tempdir().unwrap();
        let tidas_root = temporary.path().join("tidas");
        let ilcd_root = temporary.path().join("ilcd");
        let output = temporary.path().join("output");
        fs::create_dir_all(tidas_root.join("processes")).unwrap();
        fs::create_dir_all(ilcd_root.join("data/processes")).unwrap();
        fs::create_dir(&output).unwrap();
        fs::write(tidas_root.join("processes/record.json"), b"{}\n").unwrap();
        fs::write(
            ilcd_root.join("data/processes/record.xml"),
            b"<processDataSet/>\n",
        )
        .unwrap();
        fs::write(
            ilcd_root.join("data/processes/record.tidas-envelope.json"),
            b"{\"source\":\"envelope\"}\n",
        )
        .unwrap();
        fs::write(
            ilcd_root.join("data/processes/record.tidas-recovery.json"),
            b"{\"schema_version\":\"tidas.eilcd-projection-recovery.v1\"}\n",
        )
        .unwrap();
        let entry = DatasetEntry {
            dataset_type: "process".to_owned(),
            role: "unit_process".to_owned(),
            uuid: "11111111-1111-4111-8111-111111111111".to_owned(),
            version: "01.00.000".to_owned(),
            relative_path: "processes/record.json".to_owned(),
            sha256: String::new(),
            canonical_content_hash: String::new(),
        };
        let closure = ReferenceClosureReportV1 {
            profile_id: UNIT_PROFILE.to_owned(),
            root_count: 1,
            dataset_count: 1,
            reference_count: 0,
            closure_sha256: "0".repeat(64),
            dataset_keys: vec![entry.key()],
            dataset_keys_truncated: false,
            semantic_coverage: None,
            semantic_diagnostics: None,
        };
        let runtime = ReleaseRuntime {
            cancellation: CancellationToken::default(),
            memory_budget: MemoryBudget::new(8 * 1024 * 1024),
            queue_capacity: 8,
        };
        let package = write_package(&PackageRequest {
            profile: ReleaseProfile::UnitProcess,
            format: ReleaseDataFormat::Ilcd,
            entries: &[entry],
            tidas_root: &tidas_root,
            ilcd_root: &ilcd_root,
            output_dir: &output,
            closure: &closure,
            runtime: &runtime,
        })
        .unwrap();
        let file = File::open(package.artifact.path).unwrap();
        let mut archive = zip::ZipArchive::new(file).unwrap();
        let mut names = (0..archive.len())
            .map(|index| {
                archive
                    .by_index(index)
                    .unwrap()
                    .name()
                    .unwrap()
                    .into_owned()
            })
            .collect::<Vec<_>>();
        names.sort();
        assert_eq!(
            names,
            [
                "data/processes/record.tidas-envelope.json",
                "data/processes/record.tidas-recovery.json",
                "data/processes/record.xml"
            ]
        );
    }
}
