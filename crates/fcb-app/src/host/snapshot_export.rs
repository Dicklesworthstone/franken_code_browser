#![forbid(unsafe_code)]

//! Explicit source-containing FCBS export for native and embedded hosts.
//! This is a typed adapter to the existing snapshot writer, not another
//! discovery/capture/encoding/publication implementation. The caller supplies
//! both current read authority and a separately chosen output destination.

use std::{ffi::OsString, path::Path};
use super::{HostError, HostResponse};
use crate::AppError;

pub const MAX_EXPORT_FILES: usize = 65_536;
pub const MAX_EXPORT_FILE_BYTES: usize = 1024 * 1024;
pub const MAX_EXPORT_SOURCE_BYTES: usize = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SnapshotExportOptions {
    pub max_files: usize,
    pub max_file_bytes: usize,
    pub max_source_bytes: usize,
    pub respect_ignores: bool,
}
impl Default for SnapshotExportOptions {
    fn default() -> Self {
        Self { max_files: 20_000, max_file_bytes: MAX_EXPORT_FILE_BYTES,
            max_source_bytes: MAX_EXPORT_SOURCE_BYTES, respect_ignores: true }
    }
}

/// Discover and capture bounded current source, validate an FCBS archive, and
/// create ONE new destination. Existing files are never overwritten or removed.
/// Synchronous host-worker operation; no runtime, thread, or process is started.
///
/// The response is the shared `fcb.cli/1` snapshot-save receipt. A successful
/// archive may still contain unavailable members or incomplete discovery.
/// `effect` is authoritative even for an error/cancellation: no destination,
/// destination created but incomplete, complete bytes with unconfirmed sync,
/// or completed synchronization request. A caller must deliver this response
/// after late cancellation and must not automatically retry a missing receipt.
/// Interrupted destinations are deliberately preserved for explicit inspection.
/// No atomic cross-file observation or power-loss durability is asserted.
pub fn save_repository(root: &Path, destination: &Path, options: SnapshotExportOptions,
    canceled: impl FnMut() -> bool) -> Result<HostResponse, HostError> {
    if destination.as_os_str().is_empty() || destination.as_os_str().len() > 16_384
        || !(1..=MAX_EXPORT_FILES).contains(&options.max_files)
        || options.max_file_bytes > MAX_EXPORT_FILE_BYTES
        || options.max_source_bytes > MAX_EXPORT_SOURCE_BYTES {
        return Err(AppError::InputLimit.into());
    }
    let mut arguments: Vec<OsString> = vec!["snapshot".into(), "save".into(),
        "--output".into(), destination.as_os_str().to_owned(),
        "--max-files".into(), options.max_files.to_string().into(),
        "--max-file-bytes".into(), options.max_file_bytes.to_string().into(),
        "--max-total-bytes".into(), options.max_source_bytes.to_string().into()];
    if options.respect_ignores { arguments.push("--respect-ignores".into()); }
    super::invoke(root, arguments, canceled)
}
