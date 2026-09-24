use std::path::{Path, PathBuf};

use tempfile::NamedTempFile;

use crate::pipeline::ValidationError;

/// Keep the caller's path for reports while using one filesystem-native parent
/// for both the temporary file and its atomic replacement target.
pub(crate) struct SpoolDestination {
    reported: PathBuf,
    native: PathBuf,
}

impl SpoolDestination {
    pub(crate) fn new(target: &Path) -> Result<Self, ValidationError> {
        let parent = target
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        if !parent.is_dir() {
            return Err(ValidationError::SpoolParentMissing(parent.to_path_buf()));
        }
        #[cfg(windows)]
        let native =
            native_target(parent, target).map_err(|source| ValidationError::PersistSpool {
                path: target.to_path_buf(),
                source,
            })?;
        #[cfg(not(windows))]
        let native = target.to_path_buf();
        Ok(Self {
            reported: target.to_path_buf(),
            native,
        })
    }

    pub(crate) fn temporary(&self) -> Result<NamedTempFile, ValidationError> {
        let parent = self
            .native
            .parent()
            .expect("a spool target always has a parent");
        Ok(NamedTempFile::new_in(parent)?)
    }

    pub(crate) fn persist(&self, temporary: NamedTempFile) -> Result<PathBuf, ValidationError> {
        temporary
            .persist(&self.native)
            .map_err(|error| ValidationError::PersistSpool {
                path: self.reported.clone(),
                source: error.error,
            })?;
        Ok(self.reported.clone())
    }
}

#[cfg(windows)]
fn native_target(parent: &Path, target: &Path) -> std::io::Result<PathBuf> {
    // Rust canonicalize returns Windows extended-length path syntax. tempfile's
    // MoveFileExW persistence needs it on both the temporary and destination
    // paths when a caller's nested task path exceeds MAX_PATH.
    let parent = std::fs::canonicalize(parent)?;
    let name = target.file_name().ok_or_else(|| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidInput,
            "spool target has no file name",
        )
    })?;
    Ok(parent.join(name))
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;

    use super::*;

    #[test]
    fn persistence_replaces_existing_spool_and_reports_the_caller_path() {
        let directory = tempfile::tempdir().unwrap();
        let parent = directory.path().join("review workspace");
        fs::create_dir(&parent).unwrap();
        let target = parent.join("validation-events.jsonl");
        fs::write(&target, b"old\n").unwrap();
        let destination = SpoolDestination::new(&target).unwrap();
        let mut temporary = destination.temporary().unwrap();
        temporary.write_all(b"new\n").unwrap();
        assert_eq!(destination.persist(temporary).unwrap(), target);
        assert_eq!(fs::read(target).unwrap(), b"new\n");
    }
}
