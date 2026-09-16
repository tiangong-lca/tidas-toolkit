//! Atomic, rollback-safe installation of the pinned public specification.
//!
//! An import validates the complete candidate before it touches the repository,
//! then stages every public asset, the pinned manifest, and the provenance
//! record in one staging directory. Publication renames each staged file over
//! its destination; any failure restores the exact bytes that were present
//! before the first rename, including the provenance record, so a failed import
//! cannot leave a partially adopted specification behind.
//!
//! Only the public subset is replaced. The tools-owned methodology documents,
//! the eILCD inputs, and the validation indexes are never copied, removed, or
//! rewritten by this module.
//!
//! # Atomicity and concurrency
//!
//! Publication is *rollback-safe*, not a single indivisible multi-file
//! transaction, and this module does not claim otherwise. Each destination is
//! replaced by renaming a fully written file over it, so no reader ever observes
//! a truncated or partially written file, and a failure restores the prior
//! bytes and mode of every destination that was already replaced. A reader that
//! reads two different files across the window between the first and last
//! rename can still observe one updated file beside one not-yet-updated file;
//! readers are not locked out.
//!
//! Writers *are* serialized. An exclusively created lock file is held across
//! prior-state capture, every replacement, and any rollback, so a failing
//! import can only restore state that no successful import has replaced since.
//! Unique staging directories keep each importer's staging disjoint and only
//! the directory a given call created is ever removed, so another writer's
//! staging state is never inspected, reused, or deleted.
//!
//! Directory ownership is tracked explicitly. Each directory the operation
//! actually makes is recorded as it is created, and a rollback removes only
//! those, deepest first and only while empty. A directory that already existed
//! — including an empty one, and including every ancestor above the first
//! directory this operation created — is never recorded and so never removed,
//! and ownership is never inferred from emptiness or from a shared path prefix.
//!
//! The provenance records are written under `assets/spec`, not under
//! `assets/tidas`, so importing a specification does not by itself change the
//! executable asset set or the runtime fingerprint derived from it. Adopting a
//! candidate whose public bytes genuinely differ *is* an executable-asset
//! change: the full lock and the paired schema lock must then be regenerated
//! and reviewed as a deliberate change, which a green `check` reports.

use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write as _};
use std::path::{Component, Path, PathBuf};

use serde::Serialize;

use crate::AssetError;
use crate::spec_pin::{
    SPEC_ARCHIVE_MAX_BYTES, SPEC_ARCHIVE_MAX_MEMBERS, SPEC_ARCHIVE_PREFIX, SPEC_IMPORT_LOCK,
    SPEC_MANIFEST_PATH, SPEC_PROVENANCE_PATH, SPEC_STAGING_DIR, SpecCheckSummary, SpecPin,
    SpecProvenance, check_repository_public_copy, existing_regular_file, validate_public_path,
};

/// Deterministic import/check report.
#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "snake_case")]
pub struct SpecImportReport {
    pub schema_version: String,
    pub operation: String,
    pub mode: String,
    pub status: String,
    pub package: String,
    pub version: String,
    pub spec_repository: String,
    pub spec_revision: String,
    pub imported_source_repository: String,
    pub imported_source_commit: String,
    pub archive_file: String,
    pub archive_sha256: String,
    pub manifest_path: String,
    pub manifest_sha256: String,
    pub public_asset_count: usize,
    pub public_assets_sha256: String,
    pub archive_verified: bool,
    pub changed_paths: Vec<String>,
    pub unchanged_paths: Vec<String>,
    pub notes: Vec<String>,
}

/// What one import run did, or would have done.
#[derive(Clone, Debug)]
pub struct SpecImportOutcome {
    pub report: SpecImportReport,
    pub provenance: SpecProvenance,
}

/// Extract every tar member, validating the whole archive structure.
///
/// The candidate is a single-member gzip stream containing a single tar
/// archive. Every layer is checked before its contents are trusted:
///
/// * the compressed archive is read under a hard byte cap, so a hostile or
///   truncated file cannot be pulled into memory without bound;
/// * the gzip stream must carry exactly one complete member with a valid
///   trailer, and must be consumed to its last byte — a missing trailer, a
///   truncated deflate stream, concatenated members, or trailing data are all
///   rejected, none of which a plain decode-until-error would notice;
/// * the tar stream must be fully consumed, so a suffix appended after the
///   terminating blocks cannot ride along unnoticed;
/// * the tar header count is bounded independently of the file count, so a
///   flood of directory or extension headers cannot exhaust memory;
/// * every stored header name must equal the canonical path this reader acts
///   on, so a name carrying a normalized `./` or `//` prefix, or a GNU
///   extension header, is rejected rather than silently reinterpreted; and
/// * each member must be a plain file directly under the archive's `package/`
///   root. Symlinks, hard links, device nodes, absolute paths, parent
///   traversal, duplicate members, and non-portable names fail closed.
pub fn extract_archive_members(
    archive_bytes: &[u8],
    _pin: &SpecPin,
) -> Result<BTreeMap<String, Vec<u8>>, AssetError> {
    let decompressed = decompress_single_gzip_member(archive_bytes)?;
    let files = read_tar_members(&decompressed)?;
    if files.is_empty() {
        return Err(AssetError::SpecInvalid(
            "the candidate archive carries no files".to_owned(),
        ));
    }
    Ok(files)
}

/// Read and validate every member of the decompressed tar stream.
fn read_tar_members(decompressed: &[u8]) -> Result<BTreeMap<String, Vec<u8>>, AssetError> {
    let mut archive = tar::Archive::new(decompressed);

    let mut files = BTreeMap::new();
    let mut total = 0_u64;
    let mut header_count = 0_usize;

    {
        let entries = archive.entries().map_err(|error| {
            AssetError::SpecInvalid(format!("the candidate archive is unreadable: {error}"))
        })?;
        for entry in entries {
            let mut entry = entry.map_err(|error| {
                AssetError::SpecInvalid(format!("the candidate archive is malformed: {error}"))
            })?;
            header_count += 1;
            if header_count > SPEC_ARCHIVE_MAX_MEMBERS {
                return Err(AssetError::SpecInvalid(format!(
                    "the candidate archive carries more than {SPEC_ARCHIVE_MAX_MEMBERS} tar headers"
                )));
            }

            // Validate the name exactly as it is stored in the archive. The
            // parsed path is not sufficient evidence: `tar` normalizes `./x`
            // and `//x` into `x`, so a raw header naming something other than
            // the path a reader sees is precisely the case a parsed-only check
            // would miss. `raw_header_position` gives the byte offset of this
            // member's own header block inside the decompressed tar stream.
            let header_offset = usize::try_from(entry.raw_header_position()).map_err(|_| {
                AssetError::SpecInvalid("candidate archive offset overflow".to_owned())
            })?;
            let raw_header = decompressed
                .get(header_offset..header_offset.saturating_add(512))
                .ok_or_else(|| {
                    AssetError::SpecInvalid("the candidate archive header is truncated".to_owned())
                })?;
            let raw_name = raw_member_name(raw_header)?;
            let path = entry.path().map_err(|error| {
                AssetError::SpecInvalid(format!(
                    "the candidate archive has an invalid member path: {error}"
                ))
            })?;
            let name = portable_member_path(&path)?;
            validate_public_path(&name)?;

            // The stored name must be exactly the canonical path this reader
            // acts on. `tar` hands back a path whose components collapse a
            // doubled separator, so comparing the header against the parsed
            // bytes would still accept `package//x` as `package/x`; comparing
            // it against the normalized name closes that gap.
            if raw_name != name {
                return Err(AssetError::SpecInvalid(format!(
                    "the candidate archive member header names {raw_name:?}, which does not survive path normalization to {name:?}"
                )));
            }

            let entry_type = entry.header().entry_type();
            if entry_type.is_dir() {
                // Directories carry no bytes; their contents are bound by file
                // members, so accepting them cannot smuggle unverified data in.
                if name != SPEC_ARCHIVE_PREFIX
                    && !name.starts_with(&format!("{SPEC_ARCHIVE_PREFIX}/"))
                {
                    return Err(AssetError::SpecInvalid(format!(
                        "the candidate archive holds a directory outside its package root: {name}"
                    )));
                }
                continue;
            }
            if !entry_type.is_file() {
                return Err(AssetError::SpecInvalid(format!(
                    "the candidate archive member {name} is not a plain file"
                )));
            }
            if !name.starts_with(&format!("{SPEC_ARCHIVE_PREFIX}/")) {
                return Err(AssetError::SpecInvalid(format!(
                    "the candidate archive member {name} is outside its package root"
                )));
            }
            if files.contains_key(&name) {
                return Err(AssetError::SpecInvalid(format!(
                    "the candidate archive holds {name} more than once"
                )));
            }

            // Bound the prospective total before allocating, so the cap holds
            // even when the declared sizes grew after decompression.
            let declared = entry.size();
            total = total.checked_add(declared).ok_or_else(|| {
                AssetError::SpecInvalid("candidate archive size overflow".to_owned())
            })?;
            if total > SPEC_ARCHIVE_MAX_BYTES {
                return Err(AssetError::SpecInvalid(format!(
                    "the candidate archive expands beyond {SPEC_ARCHIVE_MAX_BYTES} bytes"
                )));
            }
            let mut bytes = Vec::new();
            entry.read_to_end(&mut bytes).map_err(|error| {
                AssetError::SpecInvalid(format!(
                    "the candidate archive member {name} is corrupt: {error}"
                ))
            })?;
            if u64::try_from(bytes.len()).unwrap_or(u64::MAX) != declared {
                return Err(AssetError::SpecInvalid(format!(
                    "the candidate archive member {name} is shorter than its header declares"
                )));
            }
            files.insert(name, bytes);
        }
    }

    // Everything after the terminating blocks must be consumed padding, so a
    // suffix appended to the tar stream cannot be ignored.
    if !archive.into_inner().iter().all(|byte| *byte == 0) {
        return Err(AssetError::SpecInvalid(
            "the candidate archive carries trailing data after its tar stream".to_owned(),
        ));
    }
    Ok(files)
}

/// Read the name field out of a raw 512-byte tar header block.
///
/// This reads the stored bytes rather than the parsed path, so a header whose
/// stored name would be normalized by a reader is detected. Only the GNU/ustar
/// `name` field is considered: a GNU long-name or PAX header keeps its payload
/// in the *data* block while this field holds a marker, so an extension header
/// can never equal the path a reader sees and is always rejected.
fn raw_member_name(header: &[u8]) -> Result<&str, AssetError> {
    let field = header.get(0..100).ok_or_else(|| {
        AssetError::SpecInvalid("the candidate archive header is truncated".to_owned())
    })?;
    let end = field
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(field.len());
    std::str::from_utf8(&field[..end]).map_err(|_| {
        AssetError::SpecInvalid(
            "the candidate archive holds a non-UTF-8 member path in its header".to_owned(),
        )
    })
}

/// Decompress exactly one complete gzip member, bounding and consuming it.
///
/// `GzDecoder` stops at the end of the first member and reports the rest as
/// unread input, and it only detects a missing trailer when the decompressed
/// output has already been read in full. Both properties are checked here so a
/// truncated, concatenated, or trailing-garbage archive fails closed.
fn decompress_single_gzip_member(archive_bytes: &[u8]) -> Result<Vec<u8>, AssetError> {
    if archive_bytes.is_empty() {
        return Err(AssetError::SpecInvalid(
            "the candidate archive is empty".to_owned(),
        ));
    }
    let mut decoder = flate2::bufread::GzDecoder::new(archive_bytes);
    let bound = usize::try_from(SPEC_ARCHIVE_MAX_BYTES).unwrap_or(usize::MAX);
    let mut decompressed = Vec::new();
    // Bounded by construction: read at most one byte beyond the cap, then stop.
    let mut limited = (&mut decoder).take(bound.saturating_add(1) as u64);
    limited.read_to_end(&mut decompressed).map_err(|error| {
        AssetError::SpecInvalid(format!(
            "the candidate archive is not a complete single-member gzip stream: {error}"
        ))
    })?;
    if decompressed.len() > bound {
        return Err(AssetError::SpecInvalid(format!(
            "the candidate archive expands beyond {SPEC_ARCHIVE_MAX_BYTES} bytes"
        )));
    }
    if !decoder.into_inner().is_empty() {
        return Err(AssetError::SpecInvalid(
            "the candidate archive carries more than one gzip member or trailing data".to_owned(),
        ));
    }
    Ok(decompressed)
}

fn portable_member_path(path: &Path) -> Result<String, AssetError> {
    if path.as_os_str().is_empty() {
        return Err(AssetError::SpecInvalid(
            "the candidate archive holds a member with an empty path".to_owned(),
        ));
    }
    let mut parts = Vec::new();
    for component in path.components() {
        match component {
            Component::Normal(part) => {
                let text = part.to_str().ok_or_else(|| {
                    AssetError::SpecInvalid(format!(
                        "the candidate archive holds a non-UTF-8 member path: {}",
                        path.display()
                    ))
                })?;
                parts.push(text);
            }
            _ => {
                return Err(AssetError::SpecInvalid(format!(
                    "unsafe candidate archive member path: {}",
                    path.display()
                )));
            }
        }
    }
    Ok(parts.join("/"))
}

/// One staged file waiting to be published.
struct StagedFile {
    /// Repository-relative destination.
    destination: String,
    /// Repository-relative path inside the staging tree.
    staged: String,
    bytes: Vec<u8>,
    /// Mode to apply on publication (`0o644` for public assets).
    #[cfg_attr(not(unix), allow(dead_code))]
    mode: u32,
}

/// A fault injected at a precise point so tests can prove real rollback.
///
/// This is a private test seam. It is not reachable from any public API,
/// environment variable, or command-line flag, so a caller cannot disable a
/// check or force a failure, and non-test builds cannot express it at all.
#[cfg(test)]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublishFault {
    None,
    /// Fail immediately *after* the `index`-th destination has been replaced.
    AfterRename(usize),
    /// Fail while removing the staging directory, after every rename succeeded.
    OnStagingCleanup,
    /// Proceed without taking the exclusive import lock.
    SkipLock,
    /// Pause while holding the lock, so a concurrent importer can be observed
    /// contending for it rather than merely assumed to.
    PauseWhileLocked,
    /// Pause after the first replacement, while holding the lock.
    PauseAfterFirstRenameThenFail,
}

#[cfg(not(test))]
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum PublishFault {
    None,
}

#[cfg(test)]
use PublishFault::{
    AfterRename, OnStagingCleanup, PauseAfterFirstRenameThenFail, PauseWhileLocked, SkipLock,
};

/// Test-only synchronization so overlap can be *observed* rather than assumed.
///
/// These helpers exist solely under `cfg(test)`: there is no public bypass, no
/// environment variable, and no production code path. A test starts an importer
/// on another thread, waits for the milestone that proves the lock is held, and
/// only then attempts a second importer — so contention is deterministic rather
/// than a matter of timing luck.
#[cfg(test)]
mod hooks {
    use std::sync::{Condvar, Mutex, MutexGuard, mpsc};

    /// Progress an importer reports so a test can synchronize on it.
    #[derive(Clone, Copy, Debug, Eq, PartialEq)]
    pub(super) enum Milestone {
        /// The exclusive import lock is held.
        LockAcquired,
        /// The given destination index has been replaced.
        Renamed(usize),
    }

    static SENDER: Mutex<Option<mpsc::Sender<Milestone>>> = Mutex::new(None);
    static GATE: Mutex<bool> = Mutex::new(false);
    static GATE_CONDVAR: Condvar = Condvar::new();

    fn registry() -> MutexGuard<'static, Option<mpsc::Sender<Milestone>>> {
        SENDER
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    fn gate() -> MutexGuard<'static, bool> {
        GATE.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Route milestones to `sender`, or stop routing with `None`.
    pub(super) fn observe(sender: Option<mpsc::Sender<Milestone>>) {
        *registry() = sender;
    }

    pub(super) fn emit(milestone: Milestone) {
        if let Some(sender) = registry().as_ref() {
            let _ = sender.send(milestone);
        }
    }

    /// Block until [`release`], bounded so a defect cannot hang the suite.
    pub(super) fn pause() {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
        let mut released = gate();
        while !*released {
            let Some(remaining) = deadline.checked_duration_since(std::time::Instant::now()) else {
                break;
            };
            let (guard, timeout) = GATE_CONDVAR
                .wait_timeout(released, remaining)
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            released = guard;
            if timeout.timed_out() {
                break;
            }
        }
    }

    pub(super) fn release() {
        *gate() = true;
        GATE_CONDVAR.notify_all();
    }

    pub(super) fn reset_gate() {
        *gate() = false;
    }
}

/// Import the pinned public specification into `root`.
///
/// `archive` must be the qualified candidate archive. When `dry_run` is set,
/// the candidate is fully validated and the resulting report is identical to a
/// real run, but no directory, file, or byte in `root` is created or modified.
pub fn import_public_spec(
    root: &Path,
    archive: &Path,
    pin: &SpecPin,
    dry_run: bool,
) -> Result<SpecImportOutcome, AssetError> {
    import_inner(root, archive, pin, dry_run, PublishFault::None)
}

fn import_inner(
    root: &Path,
    archive: &Path,
    pin: &SpecPin,
    dry_run: bool,
    fault: PublishFault,
) -> Result<SpecImportOutcome, AssetError> {
    let candidate = crate::spec_pin::read_candidate_archive(archive, pin)?;

    let mut staged: Vec<StagedFile> = Vec::new();
    for (path, bytes) in &candidate.assets {
        staged.push(StagedFile {
            destination: path.clone(),
            staged: path.clone(),
            bytes: bytes.clone(),
            mode: 0o644,
        });
    }
    let provenance = SpecProvenance::for_candidate(
        pin,
        candidate.assets.len(),
        candidate.public_assets_sha256.clone(),
    );
    staged.push(StagedFile {
        destination: SPEC_MANIFEST_PATH.to_owned(),
        staged: SPEC_MANIFEST_PATH.to_owned(),
        bytes: candidate.manifest_bytes.clone(),
        mode: 0o644,
    });
    staged.push(StagedFile {
        destination: SPEC_PROVENANCE_PATH.to_owned(),
        staged: SPEC_PROVENANCE_PATH.to_owned(),
        bytes: provenance.to_bytes()?,
        mode: 0o644,
    });
    staged.sort_by(|left, right| left.destination.cmp(&right.destination));

    let mut changed = Vec::new();
    let mut unchanged = Vec::new();
    for file in &staged {
        match existing_regular_file(&root.join(&file.destination))? {
            Some(existing) if existing == file.bytes => unchanged.push(file.destination.clone()),
            _ => changed.push(file.destination.clone()),
        }
    }

    let report = SpecImportReport {
        schema_version: "tidas.spec-import.v1".to_owned(),
        operation: "import-public-spec".to_owned(),
        mode: if dry_run { "check" } else { "import" }.to_owned(),
        status: if changed.is_empty() {
            "already-current".to_owned()
        } else if dry_run {
            "would-change".to_owned()
        } else {
            "imported".to_owned()
        },
        package: pin.package_name.clone(),
        version: pin.version.clone(),
        spec_repository: pin.repository.clone(),
        spec_revision: pin.revision.clone(),
        imported_source_repository: pin.imported_source_repository.clone(),
        imported_source_commit: pin.imported_source_commit.clone(),
        archive_file: pin.archive_file.clone(),
        archive_sha256: candidate.archive_sha256.clone().unwrap_or_default(),
        manifest_path: SPEC_MANIFEST_PATH.to_owned(),
        manifest_sha256: pin.manifest_sha256.clone(),
        public_asset_count: candidate.assets.len(),
        public_assets_sha256: candidate.public_assets_sha256.clone(),
        archive_verified: true,
        changed_paths: changed,
        unchanged_paths: unchanged,
        notes: vec![
            "only the pinned 39-file public subset, its manifest, and its provenance record are written"
                .to_owned(),
            "tools-owned runtime rulesets, the taxonomy extension, eILCD inputs, and validation indexes are untouched"
                .to_owned(),
            "provenance is written under assets/spec, outside the executable asset tree and its lock"
                .to_owned(),
        ],
    };

    if dry_run {
        return Ok(SpecImportOutcome { report, provenance });
    }

    // Nothing is staged or published until every destination path is proved to
    // be a plain in-repository location. This runs before the lock is created
    // and before any directory would be made, so a rejected import writes
    // nothing at all.
    for file in &staged {
        let destination = root.join(&file.destination);
        require_safe_path(root, &destination, &file.destination, Leaf::File)?;
    }
    require_owned_directory(root, &root.join("assets/spec"), "assets/spec")?;
    require_safe_path(
        root,
        &root.join(SPEC_IMPORT_LOCK),
        SPEC_IMPORT_LOCK,
        Leaf::File,
    )?;

    // Held across prior-state capture, every replacement, and any rollback, so
    // a concurrent import cannot interleave with this import's mutation window.
    #[cfg(test)]
    let lock_enabled = fault != SkipLock;
    #[cfg(not(test))]
    let lock_enabled = true;
    let _lock = ImportLock::acquire(root, lock_enabled)?;

    // The lock is held from here until this function returns, so announcing it
    // lets a test attempt a truly overlapping import rather than a later one.
    #[cfg(test)]
    if fault == PauseWhileLocked || fault == PauseAfterFirstRenameThenFail {
        hooks::emit(hooks::Milestone::LockAcquired);
    }
    #[cfg(test)]
    if fault == PauseWhileLocked {
        hooks::pause();
    }

    publish(root, &staged, fault)?;
    Ok(SpecImportOutcome { report, provenance })
}

/// What the final component of a guarded path is expected to be.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Leaf {
    /// A regular file, or absent so publication can create it.
    File,
    /// A directory, or absent so publication can create it.
    Directory,
}

/// Reject a path whose ancestry leaves the repository or uses a link.
///
/// `existing_regular_file` only inspects the final component, so a symlinked
/// *parent* directory would let publication write outside the repository. Every
/// ancestor up to `root` — and the leaf itself — must be a real directory or
/// file that either exists as expected or is absent; symlinks, wrong file
/// types, and escapes are refused, including dangling links.
fn require_safe_path(
    root: &Path,
    path: &Path,
    display: &str,
    leaf: Leaf,
) -> Result<(), AssetError> {
    let relative = path.strip_prefix(root).map_err(|_| {
        AssetError::SpecInvalid(format!("{display} is outside the repository root"))
    })?;
    let components: Vec<_> = relative.components().collect();
    if components.is_empty() {
        return Err(AssetError::SpecInvalid(format!(
            "{display} is the repository root"
        )));
    }
    let mut current = root.to_path_buf();
    for (index, component) in components.iter().enumerate() {
        current.push(component);
        let last = index + 1 == components.len();
        match fs::symlink_metadata(&current) {
            // A symlink is refused whether or not its target exists, so a
            // dangling link cannot be used to create state elsewhere.
            Ok(metadata) if metadata.file_type().is_symlink() => {
                return Err(AssetError::SpecInvalid(format!(
                    "{display} resolves through the symbolic link {}; refusing to write outside the repository",
                    current.display()
                )));
            }
            Ok(metadata) if last => match leaf {
                Leaf::File if !metadata.is_file() => {
                    return Err(AssetError::SpecInvalid(format!(
                        "{display} exists and is not a regular file"
                    )));
                }
                Leaf::Directory if !metadata.is_dir() => {
                    return Err(AssetError::SpecInvalid(format!(
                        "{display} exists and is not a directory"
                    )));
                }
                _ => {}
            },
            Ok(metadata) if !metadata.is_dir() => {
                return Err(AssetError::SpecInvalid(format!(
                    "{} is not a directory",
                    current.display()
                )));
            }
            Ok(_) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                // A missing ancestor is fine; publication creates it. A missing
                // leaf is fine too.
            }
            Err(error) => return Err(AssetError::Io(error)),
        }
    }
    Ok(())
}

/// Refuse to operate on an existing entry that is not a real directory.
fn require_owned_directory(root: &Path, path: &Path, display: &str) -> Result<(), AssetError> {
    require_safe_path(root, path, display, Leaf::Directory)
}

/// Reject a public path reached through a symlinked ancestor.
///
/// The check reads the checked-in copy, so a linked directory could otherwise
/// satisfy the pin with bytes that live outside the repository — a violation of
/// the generated-copy contract rather than a way to meet it.
pub(crate) fn require_plain_repository_path(
    root: &Path,
    path: &Path,
    display: &str,
) -> Result<(), AssetError> {
    require_safe_path(root, path, display, Leaf::File)
}

/// A staging directory this process created and exclusively owns.
///
/// The directory is created at a name generated by the operating system, so it
/// cannot collide with another writer's directory and a pre-existing directory
/// is never reused, emptied, or removed. Only this exact directory is ever
/// cleaned up.
struct OwnedStaging {
    path: PathBuf,
}

impl OwnedStaging {
    fn create(scratch: &Path) -> Result<Self, AssetError> {
        let owned = tempfile::Builder::new()
            .prefix(".import-")
            .tempdir_in(scratch)
            .map_err(|error| {
                AssetError::SpecInvalid(format!(
                    "cannot create an exclusively owned staging directory under {}: {error}",
                    scratch.display()
                ))
            })?;
        // Take ownership of the path so it survives until this value is
        // cleaned up or deliberately left behind.
        Ok(Self { path: owned.keep() })
    }

    /// Remove this staging directory, and nothing else.
    ///
    /// `enabled` is false only under an injected cleanup failure, which stands
    /// in for the directory being left behind.
    fn cleanup(&self, enabled: bool) {
        if enabled {
            let _ = fs::remove_dir_all(&self.path);
        }
    }
}

/// Serializes imports against each other for the whole mutation window.
///
/// Unique staging directories keep writers' *staging* disjoint, but they do not
/// serialize *publication*: without this, one importer's rollback could restore
/// a snapshot captured before another importer committed, silently undoing a
/// successful import. The lock is held across prior-state capture, every
/// replacement, and any rollback, so a failing import can only restore state
/// that no successful import has replaced since.
///
/// The lock is an exclusively created file rather than an advisory lock, so it
/// behaves identically on every supported platform. A lock that already exists
/// means another import is in progress or previously died holding it; this
/// import reports that instead of stealing or deleting another writer's claim.
#[derive(Debug)]
struct ImportLock {
    path: PathBuf,
    held: bool,
    created: CreatedDirectories,
}

impl ImportLock {
    fn acquire(root: &Path, enabled: bool) -> Result<Self, AssetError> {
        let path = root.join(SPEC_IMPORT_LOCK);
        if !enabled {
            return Ok(Self {
                path,
                held: false,
                created: CreatedDirectories::default(),
            });
        }
        // Creating the lock is itself a write, so the path is validated first:
        // a symlinked ancestor must not let the lock file be created outside
        // the repository.
        require_safe_path(root, &path, SPEC_IMPORT_LOCK, Leaf::File)?;
        let parent = path.parent().unwrap_or(root);
        let mut created = CreatedDirectories::default();
        created.create(parent)?;
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
        {
            Ok(mut file) => {
                // Recording the holder makes a stale lock diagnosable.
                let _ = writeln!(file, "pid={}", std::process::id());
                Ok(Self {
                    path,
                    held: true,
                    created,
                })
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                created.remove();
                Err(AssetError::SpecInvalid(format!(
                    "another import holds {SPEC_IMPORT_LOCK}; retry once it completes"
                )))
            }
            Err(error) => {
                created.remove();
                Err(AssetError::Io(error))
            }
        }
    }
}

impl Drop for ImportLock {
    fn drop(&mut self) {
        if self.held {
            let _ = fs::remove_file(&self.path);
        }
        self.created.remove();
    }
}

/// Prior bytes and mode of one destination, captured before any replacement.
struct DestinationSnapshot {
    destination: PathBuf,
    display: String,
    bytes: Option<Vec<u8>>,
    mode: Option<u32>,
}

/// Exactly the directories this operation created, in creation order.
///
/// Ownership is recorded when a directory is actually made, never inferred
/// afterwards from emptiness or a shared prefix. An empty directory that was
/// already present therefore cannot be mistaken for one this operation created,
/// and only recorded directories are ever considered for removal.
#[derive(Debug, Default)]
struct CreatedDirectories {
    created: Vec<PathBuf>,
}

impl CreatedDirectories {
    /// Create every missing ancestor of `directory`, recording each one made.
    ///
    /// `fs::create_dir_all` is deliberately not used: it reports neither which
    /// directories already existed nor which it created, so ownership could not
    /// be established afterwards.
    fn create(&mut self, directory: &Path) -> Result<(), AssetError> {
        let mut missing = Vec::new();
        let mut current = directory;
        loop {
            match fs::symlink_metadata(current) {
                Ok(_) => break,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    missing.push(current.to_path_buf());
                    match current.parent() {
                        Some(parent) => current = parent,
                        None => break,
                    }
                }
                Err(error) => return Err(AssetError::Io(error)),
            }
        }
        for directory in missing.iter().rev() {
            match fs::create_dir(directory) {
                Ok(()) => self.created.push(directory.clone()),
                // Another writer may have created it in the meantime; that
                // directory is not ours to remove.
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
                Err(error) => {
                    self.remove();
                    return Err(AssetError::Io(error));
                }
            }
        }
        Ok(())
    }

    /// Remove the recorded directories, deepest first and only when empty.
    fn remove(&mut self) {
        let mut deepest_first = std::mem::take(&mut self.created);
        deepest_first.sort_by_key(|directory| std::cmp::Reverse(directory.components().count()));
        for directory in deepest_first {
            // A non-empty directory holds something this operation did not
            // create, so it is left exactly as it is.
            let _ = fs::remove_dir(&directory);
        }
    }
}

/// Capture a destination's exact prior state.
fn snapshot(destination: &Path, display: &str) -> Result<DestinationSnapshot, AssetError> {
    let metadata = match fs::symlink_metadata(destination) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(AssetError::Io(error)),
    };
    let (bytes, mode) = match metadata {
        Some(metadata) if metadata.is_file() => {
            #[cfg(unix)]
            let mode = {
                use std::os::unix::fs::PermissionsExt as _;
                Some(metadata.permissions().mode())
            };
            #[cfg(not(unix))]
            let mode = None;
            (Some(fs::read(destination)?), mode)
        }
        Some(_) => {
            return Err(AssetError::SpecInvalid(format!(
                "{display} exists and is not a regular file"
            )));
        }
        None => (None, None),
    };
    Ok(DestinationSnapshot {
        destination: destination.to_path_buf(),
        display: display.to_owned(),
        bytes,
        mode,
    })
}

/// Write bytes to a destination without ever exposing a truncated file.
///
/// A plain `fs::write` truncates the destination first, so a concurrent reader
/// could observe an empty or partial file during a rollback. Writing to a
/// sibling temporary file and renaming it keeps the replacement indivisible on
/// Unix and Windows alike.
fn replace_bytes(destination: &Path, bytes: &[u8], mode: Option<u32>) -> Result<(), AssetError> {
    let parent = destination.parent().ok_or_else(|| {
        AssetError::SpecInvalid(format!("{} has no parent directory", destination.display()))
    })?;
    let mut file = tempfile::Builder::new()
        .prefix(".restore-")
        .tempfile_in(parent)?;
    file.write_all(bytes)?;
    file.as_file().sync_all()?;
    #[cfg(unix)]
    if let Some(mode) = mode {
        use std::os::unix::fs::PermissionsExt as _;
        file.as_file()
            .set_permissions(fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = mode;
    file.persist(destination)
        .map_err(|error| AssetError::Io(error.error))?;
    Ok(())
}

/// Restore every destination that was already replaced, then report `error`.
fn rollback(
    staging: &OwnedStaging,
    created: &mut CreatedDirectories,
    snapshots: &[DestinationSnapshot],
    published: usize,
    error: AssetError,
    cleanup_enabled: bool,
) -> AssetError {
    let mut failures = Vec::new();
    for state in snapshots.iter().take(published) {
        let restored = match &state.bytes {
            Some(bytes) => replace_bytes(&state.destination, bytes, state.mode),
            None => fs::remove_file(&state.destination).map_err(AssetError::Io),
        };
        if let Err(restore_error) = restored {
            failures.push(format!("{}: {restore_error}", state.display));
        }
    }
    // Unwind the failure back to the state before this operation ran, by
    // removing only the directories it actually created. A directory that was
    // already there, empty or not, is never recorded and so never removed.
    staging.cleanup(cleanup_enabled);
    created.remove();
    if failures.is_empty() {
        error
    } else {
        AssetError::SpecRollback {
            cause: error.to_string(),
            failures,
        }
    }
}

/// Stage every file and publish it, restoring prior bytes if anything fails.
#[cfg_attr(not(test), allow(unused_variables))]
fn publish(root: &Path, staged: &[StagedFile], fault: PublishFault) -> Result<(), AssetError> {
    // The scratch directory is validated before use so a symlink or regular
    // file planted at that path cannot redirect staging writes elsewhere.
    let scratch = root.join(SPEC_STAGING_DIR);
    require_owned_directory(root, &scratch, SPEC_STAGING_DIR)?;
    let mut created = CreatedDirectories::default();
    created.create(&scratch)?;

    let staging = OwnedStaging::create(&scratch)?;

    let snapshots = match prepare(root, staged, &staging) {
        Ok(snapshots) => snapshots,
        Err(error) => {
            staging.cleanup(true);
            created.remove();
            return Err(error);
        }
    };

    publish_staged(root, staged, fault, &staging, &mut created, &snapshots)
}

/// Write every staged file into the staging tree, then capture prior state.
///
/// Prior state is captured only after the whole staging tree exists, so a
/// preparation failure has written nothing to any destination.
fn prepare(
    root: &Path,
    staged: &[StagedFile],
    staging: &OwnedStaging,
) -> Result<Vec<DestinationSnapshot>, AssetError> {
    for file in staged {
        let target = staging.path.join(&file.staged);
        let parent = target.parent().ok_or_else(|| {
            AssetError::SpecInvalid(format!("staged path has no parent: {}", file.staged))
        })?;
        // Staging lives inside the repository-owned scratch directory, so its
        // directories are cleaned up with the staging tree and are never
        // tracked as publication-created directories.
        fs::create_dir_all(parent)?;
        fs::write(&target, &file.bytes)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&target, fs::Permissions::from_mode(file.mode))?;
        }
    }
    staged
        .iter()
        .map(|file| snapshot(&root.join(&file.destination), &file.destination))
        .collect()
}

/// Rename each staged file over its destination, rolling back on any failure.
fn publish_staged(
    root: &Path,
    staged: &[StagedFile],
    #[cfg_attr(not(test), allow(unused_variables))] fault: PublishFault,
    staging: &OwnedStaging,
    created: &mut CreatedDirectories,
    snapshots: &[DestinationSnapshot],
) -> Result<(), AssetError> {
    let mut published = 0_usize;
    for (index, file) in staged.iter().enumerate() {
        let destination = root.join(&file.destination);
        if let Some(parent) = destination.parent()
            && let Err(error) = created.create(parent)
        {
            return Err(rollback(
                staging, created, snapshots, published, error, true,
            ));
        }
        if let Err(error) = fs::rename(staging.path.join(&file.staged), &destination) {
            return Err(rollback(
                staging,
                created,
                snapshots,
                published,
                error.into(),
                true,
            ));
        }
        published = index + 1;
        #[cfg(test)]
        if fault == PauseAfterFirstRenameThenFail {
            if index == 0 {
                // Prove at least one real replacement happened, then hold the
                // lock long enough for a second importer to be refused.
                hooks::emit(hooks::Milestone::Renamed(index));
                hooks::pause();
            }
            if index + 1 == staged.len() {
                return Err(rollback(
                    staging,
                    created,
                    snapshots,
                    published,
                    AssetError::SpecInvalid(
                        "injected fault after publishing the full subset".to_owned(),
                    ),
                    true,
                ));
            }
        }
        #[cfg(test)]
        if fault == AfterRename(index) {
            if index == 0 {
                hooks::emit(hooks::Milestone::Renamed(index));
            }
            return Err(rollback(
                staging,
                created,
                snapshots,
                published,
                AssetError::SpecInvalid(format!(
                    "injected fault after publishing {}",
                    file.destination
                )),
                true,
            ));
        }
    }

    // Every destination holds its new bytes. Cleanup removes only the staging
    // directory this call exclusively owns, and failing to remove it is not a
    // publication failure: the bytes are correct, so reporting an error would
    // be false and rolling back would discard a successful import.
    #[cfg(test)]
    let cleanup_enabled = fault != OnStagingCleanup;
    #[cfg(not(test))]
    let cleanup_enabled = true;
    staging.cleanup(cleanup_enabled);
    Ok(())
}

/// Check the repository's public copy without writing anything.
///
/// When `archive` is supplied the check also re-derives the public subset from
/// the qualified candidate and compares it byte-for-byte with the repository,
/// which is what makes a manual edit to a generated copy detectable in CI.
pub fn check_public_spec(
    root: &Path,
    archive: Option<&Path>,
    pin: &SpecPin,
) -> Result<SpecCheckSummary, AssetError> {
    check_repository_public_copy(root, pin, archive)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec_pin::{SPEC_METHODOLOGY_PATHS, SPEC_SCHEMAS_PER_LANGUAGE, subset_digest};
    use flate2::Compression;
    use flate2::write::GzEncoder;
    use std::fmt::Write as _;
    use std::sync::mpsc;

    fn hex(bytes: &[u8]) -> String {
        use sha2::{Digest as _, Sha256};
        Sha256::digest(bytes)
            .iter()
            .fold(String::new(), |mut text, byte| {
                let _ = write!(text, "{byte:02x}");
                text
            })
    }

    /// A minimal but fully valid candidate: 39 assets plus candidate metadata.
    #[allow(clippy::too_many_lines)] // One declarative package layout.
    ///
    /// These tests exercise publication, so the fixture only has to satisfy the
    /// pin checks; the richer tamper fixtures live in the integration tests.
    fn fixture() -> (Vec<u8>, SpecPin) {
        let mut assets = BTreeMap::new();
        for index in 0..SPEC_SCHEMAS_PER_LANGUAGE {
            for language in ["schemas", "schemas_zh"] {
                assets.insert(
                    format!("assets/tidas/{language}/tidas_s{index}.json"),
                    format!("{{\"title\":\"{language}-{index}\"}}\n").into_bytes(),
                );
            }
        }
        for path in SPEC_METHODOLOGY_PATHS {
            assets.insert(path.to_owned(), b"name: fixture\n".to_vec());
        }
        assets.insert(
            "assets/tidas/schema.lock.json".to_owned(),
            b"{\"version\":1}\n".to_vec(),
        );
        assert_eq!(assets.len(), 39);

        let mut shipped = BTreeMap::new();
        for (path, bytes) in &assets {
            shipped.insert(path.clone(), hex(bytes));
        }
        let source: BTreeMap<String, String> = shipped.clone();
        let license = hex(b"MIT");

        let mut yaml: Vec<String> = vec![
            "version: 1".to_owned(),
            "sourceRepoId: tidas-toolkit".to_owned(),
            "sourceRepoCanonicalUrl: https://github.com/tiangong-lca/tidas-toolkit".to_owned(),
            "sourceCommit: 111111111111111111111111111111111111111a".to_owned(),
            "sourceCommitRef: main".to_owned(),
            "sourceLicensePath: LICENSE".to_owned(),
            format!("sourceLicenseSha256: {license}"),
            "sourceLicenseNotice: MIT".to_owned(),
            "excludedSourcePaths: []".to_owned(),
            "files:".to_owned(),
        ];
        for (path, digest) in &source {
            yaml.push(format!("  - sourcePath: {path}"));
            yaml.push(format!("    packagePath: {path}"));
            yaml.push(format!("    sha256: {digest}"));
        }
        let source_import = (yaml.join("\n") + "\n").into_bytes();
        let baseline = format!(
            "{{\"reviewedBaselineVersion\":1,\"specVersion\":\"0.1.0\",\
             \"source\":{{\"repository\":\"https://github.com/tiangong-lca/tidas-toolkit\",\
             \"repositoryId\":\"tidas-toolkit\",\
             \"commit\":\"111111111111111111111111111111111111111a\",\"commitRef\":\"main\",\
             \"licensePath\":\"LICENSE\",\"licenseSha256\":\"{license}\"}},\
             \"fileCount\":39,\"sourceFilesSha256\":\"{}\",\"packageFilesSha256\":\"{}\"}}\n",
            subset_digest(&source).unwrap(),
            subset_digest(&shipped).unwrap(),
        )
        .into_bytes();

        let mut metadata: BTreeMap<String, Vec<u8>> = BTreeMap::new();
        metadata.insert("LICENSE".to_owned(), b"MIT\n".to_vec());
        metadata.insert("README.md".to_owned(), b"# fixture\n".to_vec());
        metadata.insert(
            "package.json".to_owned(),
            b"{\"name\":\"fixture\"}\n".to_vec(),
        );
        metadata.insert("reviewed-baseline.json".to_owned(), baseline.clone());
        metadata.insert("source-import.yaml".to_owned(), source_import.clone());

        let mut shipped_all = shipped.clone();
        for (path, bytes) in &metadata {
            shipped_all.insert(path.clone(), hex(bytes));
        }
        let files_sha256 = subset_digest(&shipped_all).unwrap();

        let mut rows: Vec<String> = Vec::new();
        for (path, digest) in &shipped {
            rows.push(format!(
                "{{\"path\":{},\"sha256\":\"{digest}\",\"contentSha256\":\"{digest}\",\
                 \"origin\":\"tidas-toolkit\",\
                 \"source\":{{\"path\":{},\"sha256\":\"{digest}\"}}}}",
                serde_json::to_string(path).unwrap(),
                serde_json::to_string(path).unwrap(),
            ));
        }
        for (path, bytes) in &metadata {
            let digest = hex(bytes);
            rows.push(format!(
                "{{\"path\":{},\"sha256\":\"{digest}\",\"contentSha256\":\"{digest}\",\
                 \"origin\":\"tidas-spec\",\"source\":null}}",
                serde_json::to_string(path).unwrap(),
            ));
        }
        let manifest = format!(
            "{{\"manifestVersion\":1,\
             \"package\":{{\"name\":\"@tiangong-lca/tidas-spec\",\"version\":\"0.1.0\"}},\
             \"specVersion\":\"0.1.0\",\
             \"source\":{{\"repository\":\"https://github.com/tiangong-lca/tidas-toolkit\",\
             \"repositoryId\":\"tidas-toolkit\",\
             \"commit\":\"111111111111111111111111111111111111111a\",\"commitRef\":\"main\",\
             \"license\":{{\"path\":\"LICENSE\",\"sha256\":\"{license}\",\"notice\":\"MIT\"}},\
             \"excludedPaths\":[],\"ownedMetadataOrigin\":\"tidas-spec\"}},\
             \"counts\":{{\"schemasPerLanguage\":{SPEC_SCHEMAS_PER_LANGUAGE},\
             \"languages\":[\"en\",\"zh\"],\"methodologies\":2,\"importedAssets\":39,\
             \"packageMetadata\":5,\"files\":44,\"packagedFiles\":45}},\
             \"assetRoot\":\"assets/tidas\",\"lock\":\"assets/tidas/schema.lock.json\",\
             \"files\":[{}],\
             \"aggregates\":{{\"filesSha256\":\"{files_sha256}\",\
             \"filesContentSha256\":\"{files_sha256}\"}},\"selfHash\":{{\"note\":\"fixture\"}}}}\n",
            rows.join(","),
        )
        .into_bytes();

        let mut members: Vec<(String, Vec<u8>)> = assets
            .iter()
            .map(|(path, bytes)| (path.clone(), bytes.clone()))
            .collect();
        members.push(("spec-manifest.json".to_owned(), manifest.clone()));
        members.extend(metadata);
        members.sort_by(|left, right| left.0.cmp(&right.0));

        let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
        builder.mode(tar::HeaderMode::Deterministic);
        for (path, bytes) in &members {
            let mut header = tar::Header::new_gnu();
            header.set_size(bytes.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_cksum();
            builder
                .append_data(&mut header, format!("package/{path}"), bytes.as_slice())
                .unwrap();
        }
        let archive = builder.into_inner().unwrap().finish().unwrap();

        let pin = SpecPin {
            package_name: "@tiangong-lca/tidas-spec".to_owned(),
            version: "0.1.0".to_owned(),
            repository: "https://github.com/tiangong-lca/tidas-spec".to_owned(),
            revision: "222222222222222222222222222222222222222b".to_owned(),
            imported_source_repository: "https://github.com/tiangong-lca/tidas-toolkit".to_owned(),
            imported_source_commit: "111111111111111111111111111111111111111a".to_owned(),
            archive_file: "fixture.tgz".to_owned(),
            archive_sha256: hex(&archive),
            manifest_sha256: hex(&manifest),
            imported_file_count: 39,
            package_file_count: 45,
            schemas_per_language: SPEC_SCHEMAS_PER_LANGUAGE,
        };
        (archive, pin)
    }

    fn write_archive(directory: &Path, bytes: &[u8]) -> PathBuf {
        let path = directory.join("fixture.tgz");
        fs::write(&path, bytes).unwrap();
        path
    }

    fn write_file(root: &Path, relative: &str, bytes: &[u8]) {
        let absolute = root.join(relative);
        fs::create_dir_all(absolute.parent().unwrap()).unwrap();
        fs::write(absolute, bytes).unwrap();
    }

    #[cfg(unix)]
    fn mode_of(path: &Path) -> u32 {
        use std::os::unix::fs::PermissionsExt as _;
        fs::metadata(path).unwrap().permissions().mode() & 0o7777
    }

    /// Destinations chosen so a fault lands only after real replacements.
    const PRIOR: [(&str, &[u8]); 3] = [
        (
            "assets/tidas/schemas/tidas_s0.json",
            b"{\"title\":\"prior-en\"}\n",
        ),
        (
            "assets/tidas/schemas_zh/tidas_s4.json",
            b"{\"title\":\"prior-zh\"}\n",
        ),
        (SPEC_PROVENANCE_PATH, b"{\"schemaVersion\":\"prior\"}\n"),
    ];

    /// Sorted publication order, which is what the fault index refers to.
    fn published_order(pin: &SpecPin) -> Vec<String> {
        let mut paths: Vec<String> = Vec::new();
        for index in 0..SPEC_SCHEMAS_PER_LANGUAGE {
            for language in ["schemas", "schemas_zh"] {
                paths.push(format!("assets/tidas/{language}/tidas_s{index}.json"));
            }
        }
        for path in SPEC_METHODOLOGY_PATHS {
            paths.push(path.to_owned());
        }
        paths.push("assets/tidas/schema.lock.json".to_owned());
        paths.push(SPEC_MANIFEST_PATH.to_owned());
        paths.push(SPEC_PROVENANCE_PATH.to_owned());
        paths.sort();
        let _ = pin;
        paths
    }

    #[test]
    fn a_fault_after_a_real_replacement_restores_bytes_modes_and_absence() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);

        // Two pre-existing files and, deliberately, a destination that does not
        // exist at all, so both restoration and removal are exercised.
        let mut prior_bytes = Vec::new();
        for (path, bytes) in PRIOR {
            write_file(&root, path, bytes);
            prior_bytes.push((path, bytes));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(root.join(PRIOR[0].0), fs::Permissions::from_mode(0o640)).unwrap();
        }
        let absent = "assets/tidas/schemas_zh/tidas_s7.json";
        assert!(!root.join(absent).exists());

        // Fail after enough replacements to have covered every prior file and
        // the absent destination too.
        let order = published_order(&pin);
        let last_prior = order.iter().position(|path| path == PRIOR[1].0).unwrap();
        let fault =
            import_inner(&root, &archive, &pin, false, AfterRename(last_prior + 1)).unwrap_err();

        assert!(
            fault.to_string().contains("injected fault"),
            "expected the injected fault, got {fault}"
        );
        // Every replaced destination holds exactly its prior bytes again.
        for (path, bytes) in &prior_bytes {
            assert_eq!(
                &fs::read(root.join(path)).unwrap(),
                bytes,
                "{path} was not restored"
            );
        }
        // Prior modes are restored too, not just bytes.
        #[cfg(unix)]
        assert_eq!(mode_of(&root.join(PRIOR[0].0)), 0o640, "prior mode lost");
        // The destination that did not exist is absent again.
        assert!(
            !root.join(absent).exists(),
            "a destination that was absent before the failed import still exists"
        );
        // No published schema beyond the pre-existing ones remains.
        let schema_dir = root.join("assets/tidas/schemas");
        if schema_dir.exists() {
            let mut left: Vec<String> = fs::read_dir(&schema_dir)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            left.sort();
            assert_eq!(left, vec!["tidas_s0.json".to_owned()], "leftover schemas");
        }
        // No staging directory survives a rolled-back import.
        let staging = root.join(SPEC_STAGING_DIR);
        if let Ok(entries) = fs::read_dir(&staging) {
            let left: Vec<String> = entries
                .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
                .collect();
            assert!(left.is_empty(), "staging not cleaned: {left:?}");
        }
    }

    #[test]
    fn a_fault_immediately_after_the_first_replacement_still_restores_it() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);

        let order = published_order(&pin);
        let first = &order[0];
        let prior = b"{\"title\":\"prior-first\"}\n";
        write_file(&root, first, prior);

        let fault = import_inner(&root, &archive, &pin, false, AfterRename(0)).unwrap_err();
        assert!(fault.to_string().contains("injected fault"), "got {fault}");
        assert_eq!(
            &fs::read(root.join(first)).unwrap(),
            prior,
            "the very first replacement was not restored"
        );
    }

    #[test]
    fn a_fixture_import_without_a_fault_publishes_every_file() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);

        let outcome = import_inner(&root, &archive, &pin, false, PublishFault::None).unwrap();
        assert_eq!(outcome.report.status, "imported");
        assert_eq!(outcome.report.public_asset_count, 39);
        for path in published_order(&pin) {
            assert!(root.join(&path).is_file(), "{path} was not published");
        }
        // The lock is released, not left held, after a successful import.
        assert!(
            !root.join(SPEC_IMPORT_LOCK).exists(),
            "the lock was not released"
        );
    }

    #[test]
    fn an_injected_cleanup_failure_does_not_report_a_publication_failure() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);

        let outcome = import_inner(&root, &archive, &pin, false, OnStagingCleanup).unwrap();
        assert_eq!(outcome.report.status, "imported");
        // The published bytes are complete despite the staging directory
        // remaining behind.
        assert!(root.join(SPEC_PROVENANCE_PATH).is_file());
        let staging = root.join(SPEC_STAGING_DIR);
        assert_eq!(
            fs::read_dir(&staging).unwrap().count(),
            1,
            "the injected cleanup failure did not leave exactly its own staging directory"
        );
    }

    /// Every published path with its bytes, for whole-tree comparison.
    fn published_state(root: &Path, paths: &[String]) -> Vec<(String, Vec<u8>)> {
        paths
            .iter()
            .map(|path| (path.clone(), fs::read(root.join(path)).unwrap()))
            .collect()
    }

    #[test]
    fn a_failing_import_restores_the_state_it_captured_under_the_lock() {
        // A successful import commits the candidate's bytes.
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);
        import_inner(&root, &archive, &pin, false, PublishFault::None).unwrap();

        let order = published_order(&pin);
        let committed = published_state(&root, &order);

        // A later import fails after genuinely replacing several destinations.
        // Because the lock covers prior-state capture as well as publication,
        // the state it restores is the state the successful import committed —
        // never a snapshot taken before that import ran.
        let error =
            import_inner(&root, &archive, &pin, false, AfterRename(order.len() / 2)).unwrap_err();
        assert!(error.to_string().contains("injected fault"), "got {error}");
        assert_eq!(
            published_state(&root, &order),
            committed,
            "a failing import did not restore the committed state"
        );
    }

    #[test]
    fn the_import_lock_excludes_a_second_importer_and_is_released() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();

        let held = ImportLock::acquire(&root, true).unwrap();
        let blocked = ImportLock::acquire(&root, true).unwrap_err();
        assert!(
            blocked.to_string().contains("another import holds"),
            "expected lock contention, got {blocked}"
        );
        // A contended import must not have consumed the holder's claim.
        assert!(root.join(SPEC_IMPORT_LOCK).exists());

        drop(held);
        assert!(
            !root.join(SPEC_IMPORT_LOCK).exists(),
            "the lock was not released on drop"
        );
        // Released, so the next import proceeds.
        let next = ImportLock::acquire(&root, true).unwrap();
        drop(next);
    }

    /// One importer holds the lock while a second genuinely attempts it.
    ///
    /// Overlap is *observed*, not assumed: the first importer reports that it
    /// holds the lock, and the second is attempted only after that milestone.
    /// It stages, then pauses after its first real replacement, still holding
    /// the lock. Only then does the test attempt the second importer, prove it
    /// is refused, release the first so it fails and rolls back, and finally
    /// retry the second to prove it succeeds against the restored state.
    #[test]
    fn overlapping_importers_observe_real_lock_contention_and_leave_exact_state() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);
        let order = published_order(&pin);

        let (sender, receiver) = mpsc::channel();
        hooks::reset_gate();
        hooks::observe(Some(sender));
        let first_root = root.clone();
        let first_archive = archive.clone();
        let first_pin = pin.clone();
        let first = std::thread::spawn(move || {
            import_inner(
                &first_root,
                &first_archive,
                &first_pin,
                false,
                PauseAfterFirstRenameThenFail,
            )
        });

        // Wait for the milestone that proves the lock is held and at least one
        // replacement has happened, rather than sleeping and hoping.
        let reached = |receiver: &mpsc::Receiver<_>, wanted| {
            for _ in 0..64 {
                match receiver.recv_timeout(std::time::Duration::from_secs(30)) {
                    Ok(milestone) => {
                        if milestone == wanted {
                            return true;
                        }
                    }
                    Err(_) => return false,
                }
            }
            false
        };
        assert!(
            reached(&receiver, hooks::Milestone::LockAcquired),
            "the first importer never reported holding the lock"
        );
        assert!(
            reached(&receiver, hooks::Milestone::Renamed(0)),
            "the first importer never reported a real replacement"
        );

        // The second importer must be refused while the first holds the lock.
        let contended = import_inner(&root, &archive, &pin, false, PublishFault::None);
        assert!(
            contended
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("another import holds")),
            "expected real lock contention, got {contended:?}"
        );
        // The refused attempt must not have disturbed the holder's claim.
        assert!(root.join(SPEC_IMPORT_LOCK).exists());

        // With the lock disabled the same overlap is not refused, which is what
        // proves the contention above came from the lock and not from timing.
        let unlocked = import_inner(&root, &archive, &pin, false, SkipLock);
        assert!(
            unlocked.is_ok(),
            "a lock-free importer should not be refused: {:?}",
            unlocked.err()
        );

        hooks::release();
        let failed = first.join().unwrap();
        assert!(
            failed
                .as_ref()
                .err()
                .is_some_and(|error| error.to_string().contains("full subset")),
            "the first importer did not fail as injected: {failed:?}"
        );
        hooks::observe(None);
        assert!(!root.join(SPEC_IMPORT_LOCK).exists(), "the lock leaked");

        // Retry after the failing importer rolled back: it must succeed and the
        // repository must hold exactly the candidate's bytes.
        let retry = import_inner(&root, &archive, &pin, false, PublishFault::None);
        assert!(retry.is_ok(), "{:?}", retry.err());
        let expected = {
            let reference = tempfile::tempdir().unwrap();
            let reference_root = reference.path().join("repo");
            fs::create_dir_all(&reference_root).unwrap();
            import_inner(&reference_root, &archive, &pin, false, PublishFault::None).unwrap();
            published_state(&reference_root, &order)
        };
        assert_eq!(
            published_state(&root, &order),
            expected,
            "the retry did not leave exactly the committed state"
        );
    }

    /// A failing import followed by a successful one, in that order.
    ///
    /// This is a sequential ordering test, not an overlap proof; real contention
    /// is covered by `overlapping_importers_observe_real_lock_contention_and_leave_exact_state`.
    #[test]
    fn a_sequential_failure_then_success_leaves_the_committed_state() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);
        let order = published_order(&pin);

        let failing = import_inner(&root, &archive, &pin, false, AfterRename(5));
        assert!(failing.is_err(), "the injected fault did not fire");
        let succeeding = import_inner(&root, &archive, &pin, false, PublishFault::None);
        assert!(succeeding.is_ok(), "{:?}", succeeding.err());

        let expected = {
            let reference = tempfile::tempdir().unwrap();
            let reference_root = reference.path().join("repo");
            fs::create_dir_all(&reference_root).unwrap();
            import_inner(&reference_root, &archive, &pin, false, PublishFault::None).unwrap();
            published_state(&reference_root, &order)
        };
        assert_eq!(published_state(&root, &order), expected);
        assert!(!root.join(SPEC_IMPORT_LOCK).exists(), "the lock leaked");
    }

    /// A pre-existing empty parent directory must survive a rolled-back import.
    #[test]
    fn rollback_preserves_a_preexisting_empty_parent_directory() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let existing = root.join("assets/tidas");
        fs::create_dir_all(&existing).unwrap();
        let archive = write_archive(directory.path(), &archive);
        let order = published_order(&pin);

        let error =
            import_inner(&root, &archive, &pin, false, AfterRename(order.len() - 1)).unwrap_err();
        assert!(error.to_string().contains("injected fault"), "{error}");
        assert!(
            existing.is_dir(),
            "rollback deleted a preexisting empty directory"
        );
    }

    #[test]
    fn a_failed_import_removes_parent_directories_created_only_for_the_lock() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);

        let error = import_inner(&root, &archive, &pin, false, AfterRename(0)).unwrap_err();
        assert!(error.to_string().contains("injected fault"), "{error}");
        assert!(
            !root.join(SPEC_IMPORT_LOCK).exists(),
            "the import lock leaked"
        );
        assert!(
            !root.join("assets").exists(),
            "a failed import left an assets parent created only for the lock"
        );
    }

    /// A pre-existing empty ancestor above the created chain is also preserved.
    #[test]
    fn rollback_preserves_preexisting_empty_ancestors_and_neighbours() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(root.join("assets/spec")).unwrap();
        fs::create_dir_all(root.join("assets/tidas/methodologies")).unwrap();
        // Unrelated neighbouring state that must be untouched.
        fs::write(
            root.join("assets/tidas/methodologies/runtime_rulesets.json"),
            b"{\"tools\":\"owned\"}\n",
        )
        .unwrap();

        let archive = write_archive(directory.path(), &archive);
        let order = published_order(&pin);
        let error =
            import_inner(&root, &archive, &pin, false, AfterRename(order.len() - 1)).unwrap_err();
        assert!(error.to_string().contains("injected fault"), "{error}");

        assert!(
            root.join("assets/spec").is_dir(),
            "rollback removed the preexisting assets/spec"
        );
        assert!(
            root.join("assets/tidas/methodologies").is_dir(),
            "rollback removed a preexisting methodologies directory"
        );
        assert_eq!(
            fs::read(root.join("assets/tidas/methodologies/runtime_rulesets.json")).unwrap(),
            b"{\"tools\":\"owned\"}\n",
            "rollback disturbed a tools-owned neighbouring file"
        );
        // Directories this import really did create are gone again.
        assert!(
            !root.join("assets/tidas/schemas").exists(),
            "a directory created by the import was not removed"
        );
    }

    /// Directories are only recorded when this operation actually creates them.
    #[test]
    fn created_directories_records_only_what_it_creates() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let preexisting = root.join("assets/tidas");
        fs::create_dir_all(&preexisting).unwrap();

        let mut created = CreatedDirectories::default();
        created
            .create(&root.join("assets/tidas/schemas/deep"))
            .unwrap();
        assert!(
            root.join("assets/tidas/schemas/deep").is_dir(),
            "the chain was not created"
        );
        assert_eq!(
            created.created,
            vec![
                root.join("assets/tidas/schemas"),
                root.join("assets/tidas/schemas/deep"),
            ],
            "only genuinely created directories may be recorded"
        );

        created.remove();
        assert!(
            preexisting.is_dir(),
            "the pre-existing ancestor was removed"
        );
        assert!(
            !root.join("assets/tidas/schemas").exists(),
            "a created directory was left behind"
        );
    }

    /// A created directory that later gains foreign content is never removed.
    #[test]
    fn a_created_directory_holding_foreign_content_is_preserved() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        let mut created = CreatedDirectories::default();
        created.create(&root.join("assets/tidas/schemas")).unwrap();
        fs::write(root.join("assets/tidas/schemas/other-writer.json"), b"x").unwrap();

        created.remove();
        assert!(
            root.join("assets/tidas/schemas/other-writer.json")
                .is_file(),
            "rollback removed a directory holding another writer's file"
        );
    }

    #[test]
    fn a_fault_while_taking_the_lock_refuses_rather_than_stealing_it() {
        let (archive, pin) = fixture();
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path().join("repo");
        fs::create_dir_all(&root).unwrap();
        let archive = write_archive(directory.path(), &archive);

        // A lock held by someone else must not be deleted or bypassed.
        let lock_path = root.join(SPEC_IMPORT_LOCK);
        fs::create_dir_all(lock_path.parent().unwrap()).unwrap();
        fs::write(&lock_path, b"pid=other\n").unwrap();
        let error = import_inner(&root, &archive, &pin, false, PublishFault::None).unwrap_err();
        assert!(
            error.to_string().contains("another import holds"),
            "expected a lock contention error, got {error}"
        );
        assert_eq!(
            fs::read(&lock_path).unwrap(),
            b"pid=other\n",
            "another writer's lock was modified"
        );
        assert!(!root.join(SPEC_PROVENANCE_PATH).exists());
    }
}
