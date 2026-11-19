use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use thiserror::Error;
use tidas_runtime::{CancellationToken, MemoryBudget, RuntimeError};
use walkdir::WalkDir;
use zip::ZipArchive;

#[derive(Clone)]
pub struct SourceReadRequest<'a> {
    pub source: &'a Path,
    pub allowed_extensions: &'a [&'a str],
    pub max_entry_bytes: u64,
    pub cancellation: &'a CancellationToken,
    pub memory_budget: &'a MemoryBudget,
}

pub struct SourceEntry<'a> {
    pub label: String,
    pub stable_key: String,
    pub relative_path: PathBuf,
    pub bytes: &'a [u8],
}

#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct SourceReadSummary {
    pub entries_visited: u64,
    pub bytes_read: u64,
}

pub fn visit_source_entries<E>(
    request: &SourceReadRequest<'_>,
    mut visitor: impl FnMut(SourceEntry<'_>) -> Result<(), E>,
) -> Result<SourceReadSummary, E>
where
    E: From<SourceReadError>,
{
    if request.max_entry_bytes == 0 {
        return Err(SourceReadError::ZeroEntryLimit.into());
    }
    if !request.source.exists() {
        return Err(SourceReadError::MissingSource(request.source.to_path_buf()).into());
    }
    request
        .cancellation
        .check()
        .map_err(SourceReadError::from)?;
    let mut summary = SourceReadSummary::default();
    if request.source.is_dir() {
        let walker = WalkDir::new(request.source)
            .follow_links(false)
            .sort_by_file_name();
        for entry in walker {
            request
                .cancellation
                .check()
                .map_err(SourceReadError::from)?;
            let entry = entry.map_err(SourceReadError::from)?;
            if entry.file_type().is_symlink() {
                return Err(SourceReadError::Symlink(entry.into_path()).into());
            }
            if !entry.file_type().is_file()
                || !extension_allowed(entry.path(), request.allowed_extensions)
            {
                continue;
            }
            let relative_path = entry
                .path()
                .strip_prefix(request.source)
                .map_err(|_| SourceReadError::PathOutsideSource(entry.path().to_path_buf()))
                .map_err(E::from)?
                .to_path_buf();
            visit_file(
                entry.path(),
                relative_path,
                request,
                &mut summary,
                &mut visitor,
            )?;
        }
    } else if extension(request.source).as_deref() == Some("zip") {
        visit_zip(request, &mut summary, &mut visitor)?;
    } else if request.source.is_file()
        && extension_allowed(request.source, request.allowed_extensions)
    {
        let relative_path = request
            .source
            .file_name()
            .map(PathBuf::from)
            .ok_or_else(|| SourceReadError::InvalidSourceName(request.source.to_path_buf()))
            .map_err(E::from)?;
        visit_file(
            request.source,
            relative_path,
            request,
            &mut summary,
            &mut visitor,
        )?;
    } else {
        return Err(SourceReadError::UnsupportedSource(request.source.to_path_buf()).into());
    }
    Ok(summary)
}

fn visit_file<E>(
    path: &Path,
    relative_path: PathBuf,
    request: &SourceReadRequest<'_>,
    summary: &mut SourceReadSummary,
    visitor: &mut impl FnMut(SourceEntry<'_>) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<SourceReadError>,
{
    let size = path.metadata().map_err(SourceReadError::from)?.len();
    ensure_size(&relative_path, size, request.max_entry_bytes).map_err(E::from)?;
    let reservation = request
        .memory_budget
        .reserve(size)
        .map_err(SourceReadError::from)?;
    let file = File::open(path).map_err(SourceReadError::from)?;
    let bytes = read_bounded(file, size, request.max_entry_bytes).map_err(E::from)?;
    visitor(SourceEntry {
        label: path.to_string_lossy().into_owned(),
        stable_key: stable_path_key(&relative_path),
        relative_path,
        bytes: &bytes,
    })?;
    drop(reservation);
    summary.entries_visited = summary.entries_visited.saturating_add(1);
    summary.bytes_read = summary.bytes_read.saturating_add(
        u64::try_from(bytes.len())
            .map_err(|_| SourceReadError::SizeOverflow)
            .map_err(E::from)?,
    );
    Ok(())
}

fn visit_zip<E>(
    request: &SourceReadRequest<'_>,
    summary: &mut SourceReadSummary,
    visitor: &mut impl FnMut(SourceEntry<'_>) -> Result<(), E>,
) -> Result<(), E>
where
    E: From<SourceReadError>,
{
    let file = File::open(request.source).map_err(SourceReadError::from)?;
    let mut archive = ZipArchive::new(file).map_err(SourceReadError::from)?;
    for index in 0..archive.len() {
        request
            .cancellation
            .check()
            .map_err(SourceReadError::from)?;
        let mut entry = archive.by_index(index).map_err(SourceReadError::from)?;
        if entry.is_dir() {
            continue;
        }
        let relative_path = entry
            .enclosed_name()
            .ok_or_else(|| match entry.name() {
                Ok(name) => SourceReadError::UnsafeArchivePath(name.into_owned()),
                Err(error) => SourceReadError::from(error),
            })
            .map_err(E::from)?
            .clone();
        if zip_entry_is_symlink(entry.unix_mode()) {
            return Err(SourceReadError::ArchiveSymlink(relative_path).into());
        }
        if !extension_allowed(&relative_path, request.allowed_extensions) {
            continue;
        }
        let size = entry.size();
        ensure_size(&relative_path, size, request.max_entry_bytes).map_err(E::from)?;
        let reservation = request
            .memory_budget
            .reserve(size)
            .map_err(SourceReadError::from)?;
        let bytes = read_bounded(&mut entry, size, request.max_entry_bytes).map_err(E::from)?;
        visitor(SourceEntry {
            label: format!("{}:{}", request.source.display(), relative_path.display()),
            stable_key: stable_path_key(&relative_path),
            relative_path,
            bytes: &bytes,
        })?;
        drop(reservation);
        summary.entries_visited = summary.entries_visited.saturating_add(1);
        summary.bytes_read = summary.bytes_read.saturating_add(
            u64::try_from(bytes.len())
                .map_err(|_| SourceReadError::SizeOverflow)
                .map_err(E::from)?,
        );
    }
    Ok(())
}

fn read_bounded(
    mut reader: impl Read,
    declared_size: u64,
    limit: u64,
) -> Result<Vec<u8>, SourceReadError> {
    let capacity = usize::try_from(declared_size).map_err(|_| SourceReadError::SizeOverflow)?;
    let mut bytes = Vec::with_capacity(capacity);
    reader
        .by_ref()
        .take(limit.saturating_add(1))
        .read_to_end(&mut bytes)?;
    let actual = u64::try_from(bytes.len()).map_err(|_| SourceReadError::SizeOverflow)?;
    if actual > limit {
        return Err(SourceReadError::EntryTooLarge {
            path: PathBuf::from("<stream>"),
            actual,
            limit,
        });
    }
    Ok(bytes)
}

fn ensure_size(path: &Path, actual: u64, limit: u64) -> Result<(), SourceReadError> {
    if actual > limit {
        Err(SourceReadError::EntryTooLarge {
            path: path.to_path_buf(),
            actual,
            limit,
        })
    } else {
        Ok(())
    }
}

fn extension_allowed(path: &Path, allowed: &[&str]) -> bool {
    extension(path).is_some_and(|extension| {
        allowed
            .iter()
            .any(|allowed| extension.eq_ignore_ascii_case(allowed))
    })
}

fn extension(path: &Path) -> Option<String> {
    path.extension()
        .and_then(|value| value.to_str())
        .map(str::to_ascii_lowercase)
}

fn stable_path_key(path: &Path) -> String {
    path.components()
        .filter_map(|component| match component {
            std::path::Component::Normal(value) => Some(value.to_string_lossy()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("/")
}

fn zip_entry_is_symlink(mode: Option<u32>) -> bool {
    mode.is_some_and(|mode| mode & 0o170_000 == 0o120_000)
}

#[derive(Debug, Error)]
pub enum SourceReadError {
    #[error("source does not exist: {0}")]
    MissingSource(PathBuf),
    #[error("source name is invalid: {0}")]
    InvalidSourceName(PathBuf),
    #[error("source type or extension is unsupported: {0}")]
    UnsupportedSource(PathBuf),
    #[error("source traversal found a symlink: {0}")]
    Symlink(PathBuf),
    #[error("archive traversal found a symlink: {0}")]
    ArchiveSymlink(PathBuf),
    #[error("archive entry path is unsafe: {0}")]
    UnsafeArchivePath(String),
    #[error("source path escaped its root: {0}")]
    PathOutsideSource(PathBuf),
    #[error("source entry {path} is {actual} bytes, above the {limit}-byte limit")]
    EntryTooLarge {
        path: PathBuf,
        actual: u64,
        limit: u64,
    },
    #[error("source entry limit must be greater than zero")]
    ZeroEntryLimit,
    #[error("source size cannot be represented on this platform")]
    SizeOverflow,
    #[error("source I/O failed: {0}")]
    Io(#[from] std::io::Error),
    #[error("source traversal failed: {0}")]
    Walk(#[from] walkdir::Error),
    #[error("source ZIP inspection failed: {0}")]
    Zip(#[from] zip::result::ZipError),
    #[error("source runtime control failed: {0}")]
    Runtime(#[from] RuntimeError),
}

#[cfg(test)]
pub(crate) fn zip_with_raw_name(name: &[u8], content: &[u8]) -> Vec<u8> {
    use std::io::{Cursor, Write as _};

    let placeholder = "x".repeat(name.len());
    let decoded = std::str::from_utf8(name).ok();
    let mut writer = zip::ZipWriter::new(Cursor::new(Vec::new()));
    writer
        .start_file(
            decoded.unwrap_or(placeholder.as_str()),
            zip::write::SimpleFileOptions::default(),
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

#[cfg(test)]
mod tests {
    use std::io::Write as _;

    use tempfile::tempdir;
    use zip::write::SimpleFileOptions;

    use super::*;

    fn controls(limit: u64) -> (CancellationToken, MemoryBudget) {
        (CancellationToken::default(), MemoryBudget::new(limit))
    }

    #[test]
    fn directory_entries_are_visited_in_portable_name_order() {
        let directory = tempdir().unwrap();
        std::fs::write(directory.path().join("b.json"), b"b").unwrap();
        std::fs::write(directory.path().join("a.json"), b"a").unwrap();
        std::fs::write(directory.path().join("skip.xml"), b"x").unwrap();
        let (cancellation, memory_budget) = controls(1024);
        let request = SourceReadRequest {
            source: directory.path(),
            allowed_extensions: &["json"],
            max_entry_bytes: 1024,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        };
        let mut paths = Vec::new();
        let summary = visit_source_entries(&request, |entry| {
            paths.push(entry.relative_path);
            Ok::<(), SourceReadError>(())
        })
        .unwrap();
        assert_eq!(paths, [PathBuf::from("a.json"), PathBuf::from("b.json")]);
        assert_eq!(summary.entries_visited, 2);
        assert_eq!(summary.bytes_read, 2);
        assert_eq!(memory_budget.used(), 0);
    }

    #[test]
    fn zip_entries_are_bounded_and_unsafe_names_are_rejected() {
        let directory = tempdir().unwrap();
        let archive_path = directory.path().join("source.zip");
        let mut archive = zip::ZipWriter::new(File::create(&archive_path).unwrap());
        let options = SimpleFileOptions::default();
        archive.start_file("../escape.json", options).unwrap();
        archive.write_all(b"{}").unwrap();
        archive.finish().unwrap();
        let (cancellation, memory_budget) = controls(1024);
        let request = SourceReadRequest {
            source: &archive_path,
            allowed_extensions: &["json"],
            max_entry_bytes: 1024,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        };
        assert!(matches!(
            visit_source_entries(&request, |_| Ok(())),
            Err(SourceReadError::UnsafeArchivePath(_))
        ));
    }

    #[test]
    fn zip_entry_names_preserve_utf8_and_legacy_cp437() {
        let directory = tempdir().unwrap();
        let archive_path = directory.path().join("source.zip");
        for (raw_name, expected) in [
            ("目录/记录.json".as_bytes(), "目录/记录.json"),
            (b"\x82.json".as_slice(), "é.json"),
        ] {
            std::fs::write(&archive_path, zip_with_raw_name(raw_name, b"{}\n")).unwrap();
            let (cancellation, memory_budget) = controls(1024);
            let request = SourceReadRequest {
                source: &archive_path,
                allowed_extensions: &["json"],
                max_entry_bytes: 1024,
                cancellation: &cancellation,
                memory_budget: &memory_budget,
            };
            let mut visited = Vec::new();
            let summary = visit_source_entries(&request, |entry| {
                visited.push((entry.relative_path, entry.bytes.to_vec()));
                Ok::<(), SourceReadError>(())
            })
            .unwrap();
            assert_eq!(visited, [(PathBuf::from(expected), b"{}\n".to_vec())]);
            assert_eq!(summary.entries_visited, 1);
            assert_eq!(summary.bytes_read, 3);
            assert_eq!(memory_budget.used(), 0);
        }
    }

    #[test]
    fn unsafe_legacy_zip_name_is_reported_before_visiting() {
        let directory = tempdir().unwrap();
        let archive_path = directory.path().join("source.zip");
        std::fs::write(&archive_path, zip_with_raw_name(b"../\x82.json", b"{}")).unwrap();
        let (cancellation, memory_budget) = controls(1024);
        let request = SourceReadRequest {
            source: &archive_path,
            allowed_extensions: &["json"],
            max_entry_bytes: 1024,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        };
        let mut visited = false;
        assert!(matches!(
            visit_source_entries(&request, |_| {
                visited = true;
                Ok(())
            }),
            Err(SourceReadError::UnsafeArchivePath(name)) if name == "../é.json"
        ));
        assert!(!visited);
        assert_eq!(memory_budget.used(), 0);
    }

    #[test]
    fn corrupt_zip_entry_returns_zip_error_before_visiting() {
        let directory = tempdir().unwrap();
        let archive_path = directory.path().join("source.zip");
        let mut bytes = zip_with_raw_name(b"record.json", b"{}");
        bytes[0] ^= 1;
        std::fs::write(&archive_path, bytes).unwrap();
        let (cancellation, memory_budget) = controls(1024);
        let request = SourceReadRequest {
            source: &archive_path,
            allowed_extensions: &["json"],
            max_entry_bytes: 1024,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        };
        let mut visited = false;
        assert!(matches!(
            visit_source_entries(&request, |_| {
                visited = true;
                Ok(())
            }),
            Err(SourceReadError::Zip(_))
        ));
        assert!(!visited);
        assert_eq!(memory_budget.used(), 0);
    }

    #[test]
    fn declared_size_and_memory_budget_fail_before_visiting() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("large.json");
        std::fs::write(&source, vec![0_u8; 128]).unwrap();
        let (cancellation, memory_budget) = controls(64);
        let request = SourceReadRequest {
            source: &source,
            allowed_extensions: &["json"],
            max_entry_bytes: 256,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        };
        assert!(matches!(
            visit_source_entries(&request, |_| Ok(())),
            Err(SourceReadError::Runtime(
                RuntimeError::BudgetExceeded { .. }
            ))
        ));

        let larger_budget = MemoryBudget::new(256);
        let request = SourceReadRequest {
            max_entry_bytes: 64,
            memory_budget: &larger_budget,
            ..request
        };
        assert!(matches!(
            visit_source_entries(&request, |_| Ok(())),
            Err(SourceReadError::EntryTooLarge { .. })
        ));
    }

    #[test]
    fn cancellation_stops_before_reading() {
        let directory = tempdir().unwrap();
        let source = directory.path().join("source.json");
        std::fs::write(&source, b"{}").unwrap();
        let (cancellation, memory_budget) = controls(1024);
        cancellation.cancel();
        let request = SourceReadRequest {
            source: &source,
            allowed_extensions: &["json"],
            max_entry_bytes: 1024,
            cancellation: &cancellation,
            memory_budget: &memory_budget,
        };
        assert!(matches!(
            visit_source_entries(&request, |_| Ok(())),
            Err(SourceReadError::Runtime(RuntimeError::Cancelled))
        ));
    }
}
