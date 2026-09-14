#![forbid(unsafe_code)]

//! Confined native filesystem reads, symlink policy, cycle control, and special object rejection (FCB-009.A).
//!
//! # Invariants
//!
//! 1. **Strict Root Confinement**: Reads are confined to the root directory
//!    specified in the [`RootGrant`]. A lexical prefix check alone is not
//!    sufficient; path components are inspected step-by-step using
//!    `symlink_metadata()` to prevent symlink traversal escaping the root.
//! 2. **Symlink Policy & Cycle Control**:
//!    - Under [`SymlinkPolicy::DisallowAll`], any symlink encountered along the
//!      path is rejected with [`SourceError::SymlinkForbidden`].
//!    - Under [`SymlinkPolicy::AllowWithinRoot`], symlinks are followed only if
//!      their targets resolve strictly within the authorized root. Foreign
//!      symlinks are rejected with [`SourceError::ForeignSymlink`].
//!    - Symlink cycles and loops along the traversal ancestry are detected and
//!      rejected with [`SourceError::TraversalCycle`].
//!    - Alias expansion depth is bounded (max 16 hops).
//! 3. **Non-File Object Rejection**: Special filesystem objects (FIFOs, named
//!    pipes, UNIX domain sockets, character/block devices) are strictly refused
//!    with [`SourceError::SpecialObject`]. They are never opened or read, preventing
//!    indefinite blocking on malicious pipes.
//! 4. **Grant Revocation Check**: The root grant is revalidated before reading
//!    and immediately before delivering bytes. A revoked grant returns
//!    [`SourceError::GrantRevoked`].
//! 5. **Cancellation & Size Bounds**: Cooperative cancellation is checked, and
//!    payloads exceeding the configured [`ByteLength`] limit are refused with
//!    [`SourceError::PayloadTooLarge`].

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

/// A confined reader for native source roots under an authorized [`RootGrant`].
#[derive(Clone, Debug)]
pub struct ConfinedSourceReader {
    grant: RootGrant,
    symlink_policy: SymlinkPolicy,
    max_payload: ByteLength,
}

impl ConfinedSourceReader {
    pub fn new(grant: RootGrant, symlink_policy: SymlinkPolicy, max_payload: ByteLength) -> Self {
        Self {
            grant,
            symlink_policy,
            max_payload,
        }
    }

    pub fn grant(&self) -> &RootGrant {
        &self.grant
    }

    pub fn symlink_policy(&self) -> SymlinkPolicy {
        self.symlink_policy
    }

    pub fn max_payload(&self) -> ByteLength {
        self.max_payload
    }

    /// Reads a file confined within the root grant, returning a [`CompleteCapture`].
    pub fn read_file(
        &self,
        file_id: FileId,
        revision: SourceRevision,
        rel_path: &NormalizedPath,
        cancel: &CancelFlag,
    ) -> Result<CompleteCapture, SourceError> {
        // 1. Initial grant and cancellation check
        self.grant.validate_active()?;
        if cancel.is_canceled() {
            return Err(SourceError::Canceled);
        }

        // 2. Validate root directory existence
        let root_dir = self.grant.root_path().to_path_buf();
        if !root_dir.is_dir() {
            return Err(SourceError::RootUnavailable);
        }

        // Canonical root directory for containment verification
        let canonical_root = fs::canonicalize(&root_dir)
            .map_err(|_| SourceError::RootUnavailable)?;

        // 3. Resolve path step-by-step with no-follow checks
        let resolved_path = self.resolve_confined_path(&canonical_root, rel_path)?;

        if cancel.is_canceled() {
            return Err(SourceError::Canceled);
        }

        // 4. Validate opened object (no FIFOs, sockets, block/char devices)
        let symlink_meta = fs::symlink_metadata(&resolved_path)
            .map_err(map_io_error)?;

        validate_not_special(&symlink_meta)?;

        let meta = fs::metadata(&resolved_path).map_err(map_io_error)?;
        validate_not_special(&meta)?;

        if !meta.is_file() {
            return Err(SourceError::SpecialObject);
        }

        // 5. Payload size bound check
        let file_len = meta.len();
        if file_len > self.max_payload.get() {
            return Err(SourceError::PayloadTooLarge);
        }

        // 6. Safe open and read
        let mut file = File::open(&resolved_path).map_err(map_io_error)?;

        let mut buf = Vec::with_capacity(file_len as usize);
        file.read_to_end(&mut buf).map_err(map_io_error)?;

        if (buf.len() as u64) != file_len {
            // Concurrent mutation detected during read
            return Err(SourceError::MetadataMismatch);
        }

        // 7. Check cancellation and grant revocation immediately before delivering bytes
        if cancel.is_canceled() {
            return Err(SourceError::Canceled);
        }
        self.grant.validate_active()?;

        // 8. Construct complete capture
        let request = CaptureRequest::new(file_id, revision)?;
        let byte_length = ByteLength::new(file_len);
        let arc_bytes = Arc::from(buf.into_boxed_slice());

        CompleteCapture::new(request, byte_length, arc_bytes)
    }

    /// Step-by-step path traversal enforcing root containment, symlink policy,
    /// and cycle detection.
    fn resolve_confined_path(
        &self,
        canonical_root: &Path,
        rel_path: &NormalizedPath,
    ) -> Result<PathBuf, SourceError> {
        let mut current = canonical_root.to_path_buf();
        let mut visited_dirs: HashSet<DirectoryId> = HashSet::new();
        let mut symlink_hops = 0;

        // Record root in visited directory ancestry
        if let Ok(id) = DirectoryId::from_path(canonical_root) {
            visited_dirs.insert(id);
        }

        for segment in rel_path.segments() {
            let seg_path = segment.to_path_buf();
            current.push(seg_path);

            let meta = match fs::symlink_metadata(&current) {
                Ok(m) => m,
                Err(e) => return Err(map_io_error(e)),
            };

            if meta.file_type().is_symlink() {
                match self.symlink_policy {
                    SymlinkPolicy::DisallowAll => {
                        return Err(SourceError::SymlinkForbidden);
                    }
                    SymlinkPolicy::AllowWithinRoot => {
                        symlink_hops += 1;
                        if symlink_hops > 16 {
                            // Bounded alias expansion exceeded -> cycle/loop refusal
                            return Err(SourceError::TraversalCycle);
                        }

                        // Read symlink target
                        let target = fs::read_link(&current).map_err(map_io_error)?;
                        let target_resolved = if target.is_absolute() {
                            target
                        } else {
                            current.parent().unwrap_or(canonical_root).join(target)
                        };

                        // Canonicalize target to verify containment
                        let canonical_target = fs::canonicalize(&target_resolved)
                            .map_err(map_io_error)?;

                        if !canonical_target.starts_with(canonical_root) {
                            return Err(SourceError::ForeignSymlink);
                        }

                        // Cycle detection on directory symlinks
                        if canonical_target.is_dir() {
                            if let Ok(dir_id) = DirectoryId::from_path(&canonical_target) {
                                if !visited_dirs.insert(dir_id) {
                                    return Err(SourceError::TraversalCycle);
                                }
                            }
                        }

                        current = canonical_target;
                    }
                }
            } else if meta.is_dir() {
                if let Ok(dir_id) = DirectoryId::from_path(&current) {
                    visited_dirs.insert(dir_id);
                }
            }
        }

        // Final verification that `current` canonicalizes within `canonical_root`
        let canonical_current = fs::canonicalize(&current).map_err(map_io_error)?;
        if !canonical_current.starts_with(canonical_root) {
            return Err(SourceError::PathEscape);
        }

        Ok(current)
    }
}

/// Identifies a directory by filesystem device and inode on Unix for cycle detection.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct DirectoryId {
    #[cfg(unix)]
    dev: u64,
    #[cfg(unix)]
    ino: u64,
    #[cfg(not(unix))]
    hash: u64,
}

impl DirectoryId {
    fn from_path(path: &Path) -> Result<Self, SourceError> {
        let meta = fs::metadata(path).map_err(map_io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            Ok(Self {
                dev: meta.dev(),
                ino: meta.ino(),
            })
        }
        #[cfg(not(unix))]
        {
            use std::collections::hash_map::DefaultHasher;
            use std::hash::{Hash, Hasher};
            let mut hasher = DefaultHasher::new();
            path.hash(&mut hasher);
            Ok(Self {
                hash: hasher.finish(),
            })
        }
    }
}

/// Validates that a filesystem object is not a FIFO, socket, or device file.
fn validate_not_special(meta: &fs::Metadata) -> Result<(), SourceError> {
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
        if !ft.is_file() && !ft.is_dir() && !ft.is_symlink() {
            return Err(SourceError::SpecialObject);
        }
    }
    Ok(())
}

fn map_io_error(e: std::io::Error) -> SourceError {
    match e.kind() {
        std::io::ErrorKind::NotFound => SourceError::CaptureUnavailable,
        std::io::ErrorKind::PermissionDenied => SourceError::RootUnavailable,
        _ => SourceError::CaptureUnavailable,
    }
}
