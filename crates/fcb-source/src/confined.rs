#![forbid(unsafe_code)]

//! Explicit native source reads and symlink policy (FCB-009 / FCB-011).
//!
//! [`range`] provides byte-capped, resumable observations of an already OPEN
//! regular file supplied by the host. It never resolves or reopens a pathname.
//!
//! The legacy [`ConfinedSourceReader`] checks paths and their canonical targets,
//! refuses observed special objects, and validates grants before delivery. Its
//! path checks and `File::open` are separate operations: they do NOT establish
//! race-safe confinement under hostile concurrent path replacement, nor prove
//! that a replaced special object cannot be opened. That FCB-009 native gate
//! remains unmet by this path-based route. Use an authorized descriptor supplied
//! by the host's qualified native boundary for the range-reading route.
//!
//! Both routes retain original observed bytes, not an atomic-filesystem claim.
//! Whole reads are now bounded throughout growth and cooperative cancellation;
//! `range` additionally reserves managed capacity before allocating a candidate.

pub mod range;

use std::collections::HashSet;
use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use fcb_core::{ByteLength, FileId, SourceRevision};

use crate::path::NormalizedPath;
use crate::root::RootGrant;
use crate::{CancelFlag, CaptureRequest, CompleteCapture, SourceError};

/// Policy governing symlink resolution during confined traversal.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SymlinkPolicy {
    /// Refuse all symlinks encountered during path resolution.
    DisallowAll,
    /// Allow symlinks only if they point within the root directory.
    AllowWithinRoot,
}

/// A path-checked reader under an explicit [`RootGrant`]. See the module's
/// concurrent-path-replacement limitation before choosing this native route.
#[derive(Clone, Debug)]
pub struct ConfinedSourceReader {
    grant: RootGrant,
    symlink_policy: SymlinkPolicy,
    max_payload: ByteLength,
}

impl ConfinedSourceReader {
    pub fn new(grant: RootGrant, symlink_policy: SymlinkPolicy, max_payload: ByteLength) -> Self {
        Self { grant, symlink_policy, max_payload }
    }

    pub fn grant(&self) -> &RootGrant { &self.grant }
    pub fn symlink_policy(&self) -> SymlinkPolicy { self.symlink_policy }
    pub fn max_payload(&self) -> ByteLength { self.max_payload }

    /// Capture a bounded whole file. This is explicit worker I/O, not a
    /// viewport operation. It has the path-race limitation documented above.
    pub fn read_file(&self, file_id: FileId, revision: SourceRevision,
        rel_path: &NormalizedPath, cancel: &CancelFlag) -> Result<CompleteCapture, SourceError> {
        let request = CaptureRequest::new(file_id, revision)?;
        if file_id.owner() != self.grant.owner() { return Err(SourceError::ForeignOwner); }
        self.grant.validate_active()?;
        if cancel.is_canceled() { return Err(SourceError::Canceled); }
        let root_dir = self.grant.root_path().to_path_buf();
        if !root_dir.is_dir() { return Err(SourceError::RootUnavailable); }
        let canonical_root = fs::canonicalize(&root_dir).map_err(|_| SourceError::RootUnavailable)?;
        let resolved_path = self.resolve_confined_path(&canonical_root, rel_path)?;
        if cancel.is_canceled() { return Err(SourceError::Canceled); }
        let symlink_meta = fs::symlink_metadata(&resolved_path).map_err(map_io_error)?;
        validate_not_special(&symlink_meta)?;
        let meta = fs::metadata(&resolved_path).map_err(map_io_error)?;
        validate_not_special(&meta)?;
        if !meta.is_file() { return Err(SourceError::SpecialObject); }
        let file_len = meta.len();
        if file_len > self.max_payload.get() { return Err(SourceError::PayloadTooLarge); }
        let mut file = safe_open_regular_file(&resolved_path)?;
        let opened = file.metadata().map_err(map_io_error)?;
        if opened.len() != file_len { return Err(SourceError::MetadataMismatch); }
        let buf = read_bounded(&mut file, file_len, self.max_payload, cancel,
            || self.grant.validate_active())?;
        let after = file.metadata().map_err(map_io_error)?;
        if after.len() != file_len || matches!((opened.modified().ok(), after.modified().ok()),
            (Some(before), Some(after)) if before != after) {
            return Err(SourceError::MetadataMismatch);
        }
        if cancel.is_canceled() { return Err(SourceError::Canceled); }
        self.grant.validate_active()?;
        CompleteCapture::new(request, ByteLength::new(file_len), Arc::from(buf.into_boxed_slice()))
    }

    /// Canonicalize the grant root after confirming it is still an accessible directory.
    pub(crate) fn canonical_root_path(&self) -> Result<PathBuf, SourceError> {
        self.grant.validate_active()?;
        let root_dir = self.grant.root_path().to_path_buf();
        if !root_dir.is_dir() { return Err(SourceError::RootUnavailable); }
        fs::canonicalize(&root_dir).map_err(|_| SourceError::RootUnavailable)
    }

    /// Step-by-step path checks. This does not supply descriptor-relative,
    /// race-safe native traversal under concurrent namespace replacement.
    pub(crate) fn resolve_confined_path(&self, canonical_root: &Path,
        rel_path: &NormalizedPath) -> Result<PathBuf, SourceError> {
        let mut current = canonical_root.to_path_buf();
        let mut visited_dirs: HashSet<DirectoryId> = HashSet::new();
        let mut symlink_hops = 0;
        if let Ok(id) = DirectoryId::from_path(canonical_root) { visited_dirs.insert(id); }
        for segment in rel_path.segments() {
            let seg_path = segment.to_path_buf();
            current.push(seg_path);
            let meta = match fs::symlink_metadata(&current) {
                Ok(m) => m,
                Err(e) => return Err(map_io_error(e)),
            };
            if meta.file_type().is_symlink() {
                match self.symlink_policy {
                    SymlinkPolicy::DisallowAll => return Err(SourceError::SymlinkForbidden),
                    SymlinkPolicy::AllowWithinRoot => {
                        symlink_hops += 1;
                        if symlink_hops > 16 { return Err(SourceError::TraversalCycle); }
                        let target = fs::read_link(&current).map_err(map_io_error)?;
                        let target_resolved = if target.is_absolute() { target }
                            else { current.parent().unwrap_or(canonical_root).join(target) };
                        let canonical_target = fs::canonicalize(&target_resolved).map_err(map_io_error)?;
                        if !canonical_target.starts_with(canonical_root) { return Err(SourceError::ForeignSymlink); }
                        if canonical_target.is_dir()
                            && let Ok(dir_id) = DirectoryId::from_path(&canonical_target)
                            && !visited_dirs.insert(dir_id) {
                            return Err(SourceError::TraversalCycle);
                        }
                        current = canonical_target;
                    }
                }
            } else if meta.is_dir() && let Ok(dir_id) = DirectoryId::from_path(&current) {
                visited_dirs.insert(dir_id);
            }
        }
        let canonical_current = fs::canonicalize(&current).map_err(map_io_error)?;
        if !canonical_current.starts_with(canonical_root) { return Err(SourceError::PathEscape); }
        Ok(current)
    }
}

/// Fixed allocation and one-byte growth lookahead, never an unbounded
/// read_to_end. Repeated interruption is bounded even in this synchronous
/// compatibility route. The range reader offers separately resumable I/O.
fn read_bounded(reader: &mut impl Read, length: u64, max_payload: ByteLength,
    cancel: &CancelFlag, mut authorized: impl FnMut() -> Result<(), SourceError>) -> Result<Vec<u8>, SourceError> {
    if length > max_payload.get() { return Err(SourceError::PayloadTooLarge); }
    let length = usize::try_from(length).map_err(|_| SourceError::PayloadTooLarge)?;
    let mut buf = Vec::new();
    buf.try_reserve_exact(length).map_err(|_| SourceError::PayloadTooLarge)?;
    buf.resize(length, 0);
    let mut filled = 0;
    let mut interruptions = 0;
    loop {
        if cancel.is_canceled() { return Err(SourceError::Canceled); }
        authorized()?;
        let mut extra = [0u8; 1];
        let target = if filled == length { &mut extra[..] }
            else { &mut buf[filled..length.min(filled.saturating_add(64 * 1024))] };
        match reader.read(target) {
            Ok(0) if filled == length => return Ok(buf),
            Ok(0) => return Err(SourceError::MetadataMismatch),
            Ok(_) if filled == length => return Err(SourceError::MetadataMismatch),
            Ok(count) => { filled += count; interruptions = 0; }
            Err(error) if error.kind() == std::io::ErrorKind::Interrupted && interruptions < 32 => {
                interruptions += 1;
            }
            Err(error) => return Err(map_io_error(error)),
        }
    }
}

/// Identifies a directory by filesystem device and inode on Unix for cycle detection.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DirectoryId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    hash: u64,
}

impl DirectoryId {
    pub fn from_path(path: &Path) -> Result<Self, SourceError> {
        let meta = fs::metadata(path).map_err(map_io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self { dev: meta.dev(), ino: meta.ino() })
        }
        #[cfg(not(unix))]
        {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            path.hash(&mut hasher);
            Ok(Self { hash: hasher.finish() })
        }
    }
}

/// Safely open a regular source file without blocking on FIFOs or following forbidden objects.
///
/// On Unix, opens with non-blocking mode to ensure opening a malicious FIFO never indefinitely
/// blocks worker threads (§8.6). After opening, validates the opened file descriptor's
/// metadata: if it is not a regular file (e.g. FIFO, socket, device), the handle is
/// immediately closed and `Err(SourceError::SpecialObject)` is returned.
pub fn safe_open_regular_file(path: &Path) -> Result<File, SourceError> {
    let sym_meta = fs::symlink_metadata(path).map_err(map_io_error)?;
    validate_not_special(&sym_meta)?;

    #[cfg(unix)]
    let file = {
        use std::os::unix::fs::OpenOptionsExt;
        #[cfg(target_os = "linux")]
        const O_NONBLOCK: i32 = 0o4000;
        #[cfg(target_os = "macos")]
        const O_NONBLOCK: i32 = 0x0004;
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        const O_NONBLOCK: i32 = 0;

        fs::OpenOptions::new()
            .read(true)
            .custom_flags(O_NONBLOCK)
            .open(path)
            .map_err(map_io_error)?
    };

    #[cfg(not(unix))]
    let file = fs::File::open(path).map_err(map_io_error)?;

    let opened_meta = file.metadata().map_err(map_io_error)?;
    validate_not_special(&opened_meta)?;
    if !opened_meta.is_file() {
        return Err(SourceError::SpecialObject);
    }

    Ok(file)
}

/// Validates an observed filesystem object is not a FIFO, socket, or device.
pub(crate) fn validate_not_special(meta: &fs::Metadata) -> Result<(), SourceError> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::FileTypeExt;
        let ft = meta.file_type();
        if ft.is_fifo() || ft.is_socket() || ft.is_block_device() || ft.is_char_device() {
            return Err(SourceError::SpecialObject);
        }
    }
    #[cfg(not(unix))]
    {
        let ft = meta.file_type();
        if !ft.is_file() && !ft.is_dir() && !ft.is_symlink() { return Err(SourceError::SpecialObject); }
    }
    Ok(())
}
pub(crate) fn map_io_error(e: std::io::Error) -> SourceError {
    match e.kind() {
        std::io::ErrorKind::NotFound => SourceError::CaptureUnavailable,
        std::io::ErrorKind::PermissionDenied => SourceError::RootUnavailable,
        _ => SourceError::CaptureUnavailable,
    }
}

#[cfg(test)]
mod bounded_compatibility_tests {
    use super::*;
    #[test]
    fn fixed_length_capture_detects_growth_and_truncation() {
        let cancel = CancelFlag::new();
        for bytes in [b"ab".as_slice(), b"abcd"] {
            let mut reader = bytes;
            assert_eq!(read_bounded(&mut reader, 3, ByteLength::new(3), &cancel, || Ok(())), Err(SourceError::MetadataMismatch));
        }
        let mut exact = b"abc".as_slice();
        assert_eq!(read_bounded(&mut exact, 3, ByteLength::new(3), &cancel, || Ok(())).unwrap(), b"abc");
    }
    #[test]
    fn infinite_input_consumes_only_length_plus_one_byte() {
        struct Infinite(usize);
        impl Read for Infinite {
            fn read(&mut self, bytes: &mut [u8]) -> std::io::Result<usize> {
                bytes.fill(b'x'); self.0 += bytes.len(); Ok(bytes.len())
            }
        }
        let mut reader = Infinite(0);
        assert_eq!(read_bounded(&mut reader, 19, ByteLength::new(19), &CancelFlag::new(), || Ok(())), Err(SourceError::MetadataMismatch));
        assert_eq!(reader.0, 20);
    }
    #[test]
    fn revocation_and_repeated_interruptions_terminate() {
        struct Interrupted(usize);
        impl Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                self.0 += 1; Err(std::io::Error::from(std::io::ErrorKind::Interrupted))
            }
        }
        let mut reader = Interrupted(0);
        assert!(read_bounded(&mut reader, 1, ByteLength::new(1), &CancelFlag::new(), || Ok(())).is_err());
        assert_eq!(reader.0, 33);
        let before = reader.0;
        assert_eq!(read_bounded(&mut reader, 1, ByteLength::new(1), &CancelFlag::new(), || Err(SourceError::GrantRevoked)), Err(SourceError::GrantRevoked));
        assert_eq!(reader.0, before);
    }
}
