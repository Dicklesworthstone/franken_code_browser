#![forbid(unsafe_code)]

//! Safe application services for native shells. These are explicit worker
//! operations, never redraw/input callbacks. No runtime, implicit stdin,
//! source write, network request or native presentation is created here.
//! Structured services use the same dispatcher/JSON as the CLI. Legacy text
//! handoff is an exact bounded UTF-8 observation, not a lossy/truncated preview.

pub mod atlas;
pub mod reader;
pub mod atlas_session;
pub mod atlas_search;
pub mod atlas_paths;

use std::{ffi::OsString, io::{self, Read, Write}, mem::size_of, path::Path};
use fcb::{ByteLength, ByteOffset, ByteRange, SourceRevision};
use fcb::search::{CaptureRequest, ExtentConsistency, ExtentReadState,
    ExtentStepBudget, FileRangeReader, ResourceBudget};
use fcb_core::ResourceLease;
use crate::{AppError, MANAGED_BYTES, allocation, file_id, input, owner};
use crate::output::MAX_RESPONSE_BYTES;

pub const MAX_HOST_TEXT_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_HOST_READ_CALLS: u64 = 4096;
const EXTENT_BYTES: usize = fcb::source::confined::range::MAX_OBSERVED_EXTENT_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HostError { App(AppError), InvalidUtf8, EmbeddedNul, DeliveryFailed }
impl From<AppError> for HostError { fn from(e: AppError) -> Self { Self::App(e) } }
impl std::fmt::Display for HostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"),
            Self::InvalidUtf8 => f.write_str("HOST_TEXT_INVALID_UTF8"),
            Self::EmbeddedNul => f.write_str("HOST_TEXT_EMBEDDED_NUL"),
            Self::DeliveryFailed => f.write_str("HOST_RESPONSE_NOT_DELIVERED"),
        }
    }
}
impl std::error::Error for HostError {}

/// The caller may copy this once to a native string under the retained handoff
/// reservation. After handoff the native host owns/accountably releases it.
/// No durable source identity is assigned to this legacy text-only response.
pub struct HostText {
    text: String,
    read_calls: u64,
    _lease: ResourceLease,
}
impl HostText {
    pub fn as_str(&self) -> &str { &self.text }
    pub fn read_calls(&self) -> u64 { self.read_calls }
    pub fn source_bytes_read(&self) -> usize { self.text.len() }
}

/// Exact whole-file UTF-8, including an initial BOM. The byte reader is shared
/// with retained sessions; only this legacy C-text route refuses embedded NUL.
pub fn read_text(path: &Path, max_bytes: usize, canceled: impl FnMut() -> bool)
    -> Result<HostText, HostError> {
    let raw = read_bytes(path, max_bytes, canceled)?;
    let text = String::from_utf8(raw.bytes).map_err(|_| HostError::InvalidUtf8)?;
    if text.as_bytes().contains(&0) { return Err(HostError::EmbeddedNul); }
    Ok(HostText { text, read_calls: raw.read_calls, _lease: raw._lease })
}

struct HostBytes { bytes: Vec<u8>, read_calls: u64, _lease: ResourceLease }

/// Read the WHOLE named regular file, or fail. Admission happens before source
/// allocation/I/O. Independently bounded extents are copied in original order
/// without per-chunk text decoding, so scalar boundaries need not align with
/// chunk boundaries. A final length/mtime comparison covers the whole operation;
/// this identifies an observed sequence, not an atomic filesystem snapshot.
/// Symlink/special-file policy is the same as the ordinary application reader.
fn read_bytes(path: &Path, max_bytes: usize, mut canceled: impl FnMut() -> bool)
    -> Result<HostBytes, HostError> {
    if max_bytes == 0 || max_bytes > MAX_HOST_TEXT_BYTES { return Err(AppError::InputLimit.into()); }
    if canceled() { return Err(AppError::Canceled.into()); }
    let budget = ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
    // Original result + native handoff or retained capture overlap + path scratch.
    let charge = max_bytes.checked_mul(3).and_then(|n| n.checked_add(256 * 1024 + size_of::<HostText>()))
        .ok_or(AppError::Admission)?;
    let lease = budget.try_reserve_managed(owner(), allocation(90), ByteLength::new(charge as u64))
        .map_err(|_| AppError::Admission)?;
    let path = input::absolute(path)?;
    let (file, before) = input::open_regular(&path)?;
    if before.len() > max_bytes as u64 { return Err(AppError::InputLimit.into()); }
    let modified = before.modified().map_err(|_| AppError::SourceChanged)?;
    let observer = file.try_clone().map_err(|_| AppError::Io)?;
    let mut reader = FileRangeReader::new(file_id(), file).map_err(AppError::from)?;
    let length = usize::try_from(before.len()).map_err(|_| AppError::InputLimit)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| AppError::Admission)?;
    if bytes.capacity() > length { return Err(AppError::Admission.into()); }
    let mut calls = 0u64;
    let mut revision = 1u64;
    while bytes.len() < length {
        if canceled() { return Err(AppError::Canceled.into()); }
        let start = bytes.len();
        let end = length.min(start + EXTENT_BYTES);
        let range = ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new(end as u64))
            .map_err(|_| AppError::InvalidRange)?;
        let request = CaptureRequest::new(file_id(), SourceRevision::new(owner(), revision)
            .map_err(|_| AppError::InvalidRange)?).and_then(|r| r.with_range(range))
            .map_err(|_| AppError::InvalidRange)?;
        revision += 1; // At most four admitted extents, never user-sized iteration.
        let mut read = reader.begin(request, &budget, allocation(91)).map_err(AppError::from)?;
        let remaining_calls = MAX_HOST_READ_CALLS - calls;
        while read.state() == ExtentReadState::Pending {
            if read.stats().read_calls >= remaining_calls { return Err(AppError::IoCallLimit.into()); }
            read.step(ExtentStepBudget { max_bytes: 64 * 1024,
                max_calls: (remaining_calls - read.stats().read_calls).min(32) as usize }, &mut canceled)
                .map_err(AppError::from)?;
        }
        calls += read.stats().read_calls;
        let extent = read.finish(&mut canceled).map_err(AppError::from)?;
        if !extent.request_filled() || extent.range() != range
            || extent.observed_length().get() != before.len()
            || extent.final_length().map(|n| n.get()) != Some(before.len())
            || extent.consistency() != ExtentConsistency::UnchangedMetadata {
            return Err(AppError::SourceChanged.into());
        }
        bytes.extend_from_slice(extent.bytes());
    }
    let after = observer.metadata().map_err(|_| AppError::Io)?;
    if after.len() != before.len() || after.modified().ok() != Some(modified) {
        return Err(AppError::SourceChanged.into());
    }
    if canceled() { return Err(AppError::Canceled.into()); }
    Ok(HostBytes { bytes, read_calls: calls, _lease: lease })
}

/// Complete machine response, including partial/error/canceled JSON outcomes.
/// `exit_code` has the existing CLI meaning. Response IDs are invocation-local
/// except when an explicitly retained reader session supplies the identities.
pub struct HostResponse {
    text: String,
    exit_code: u8,
    _lease: ResourceLease,
}
impl HostResponse {
    pub fn as_str(&self) -> &str { &self.text }
    pub fn exit_code(&self) -> u8 { self.exit_code }
}

/// Bounded viewport into one named file, with the shared exact byte/decoder map.
pub fn read_window(path: &Path, offset: u64, bytes: u64, canceled: impl FnMut() -> bool)
    -> Result<HostResponse, HostError> {
    invoke(path, vec!["read".into(), "--offset".into(), offset.to_string().into(),
        "--bytes".into(), bytes.to_string().into()], canceled)
}
pub fn read_lines(path: &Path, line: u64, lines: u64, canceled: impl FnMut() -> bool)
    -> Result<HostResponse, HostError> {
    invoke(path, vec!["read-lines".into(), "--line".into(), line.to_string().into(),
        "--lines".into(), lines.to_string().into()], canceled)
}
/// Metadata-only discovery/viewport. Use retained Rust map objects for gestures,
/// rather than calling this one-shot preparation service on every camera update.
pub fn atlas_plan(root: &Path, canceled: impl FnMut() -> bool) -> Result<HostResponse, HostError> {
    invoke(root, vec!["atlas".into()], canceled)
}
pub fn search_workspace(root: &Path, needle: &str, canceled: impl FnMut() -> bool)
    -> Result<HostResponse, HostError> {
    if needle.len() > crate::args::MAX_SINGLE_ARGUMENT { return Err(AppError::InputLimit.into()); }
    invoke(root, vec!["search".into(), "--workspace".into(), "--text".into(), needle.into()], canceled)
}
/// Logical FrankenMarkdown flow, not native shaping or a reinterpreted source
/// line number. Each invocation captures the explicitly named current file.
pub fn markdown_window(path: &Path, first_line: u64, lines: u64, width: u64,
    canceled: impl FnMut() -> bool) -> Result<HostResponse, HostError> {
    invoke(path, vec!["markdown".into(), "--line".into(), first_line.to_string().into(),
        "--lines".into(), lines.to_string().into(), "--width".into(), width.to_string().into()], canceled)
}
pub fn markdown_heading(path: &Path, heading: &str, lines: u64, width: u64,
    canceled: impl FnMut() -> bool) -> Result<HostResponse, HostError> {
    if heading.len() > 4096 { return Err(AppError::InputLimit.into()); }
    invoke(path, vec!["markdown".into(), "--heading".into(), heading.into(),
        "--lines".into(), lines.to_string().into(), "--width".into(), width.to_string().into()], canceled)
}

fn invoke(path: &Path, mut arguments: Vec<OsString>, canceled: impl FnMut() -> bool)
    -> Result<HostResponse, HostError> {
    if path.as_os_str().is_empty() || path.as_os_str().len() > 16_384 { return Err(AppError::InputLimit.into()); }
    arguments.extend(["--json".into(), "--".into(), path.as_os_str().to_owned()]);
    let budget = ResourceBudget::new(owner(), ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
    let lease = budget.try_reserve_managed(owner(), allocation(92),
        ByteLength::new((3 * MAX_RESPONSE_BYTES + 256 * 1024) as u64)).map_err(|_| AppError::Admission)?;
    let mut stdout = Sink::new(MAX_RESPONSE_BYTES)?;
    let mut stderr = Sink::new(8192)?;
    let exit_code = crate::run(&arguments, &mut NoInput, &mut stdout, &mut stderr, canceled);
    if stdout.failed || stderr.failed || !stderr.bytes.is_empty() || stdout.bytes.is_empty() {
        return Err(HostError::DeliveryFailed);
    }
    let text = String::from_utf8(stdout.bytes).map_err(|_| HostError::InvalidUtf8)?;
    if text.as_bytes().contains(&0) { return Err(HostError::EmbeddedNul); }
    Ok(HostResponse { text, exit_code, _lease: lease })
}
struct NoInput;
impl Read for NoInput {
    fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
        Err(io::Error::new(io::ErrorKind::PermissionDenied, "HOST_STDIN_NOT_GRANTED"))
    }
}
struct Sink { bytes: Vec<u8>, limit: usize, failed: bool }
impl Sink {
    fn new(limit: usize) -> Result<Self, HostError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(limit).map_err(|_| AppError::Admission)?;
        if bytes.capacity() > limit { return Err(AppError::Admission.into()); }
        Ok(Self { bytes, limit, failed: false })
    }
}
impl Write for Sink {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        if bytes.len() > self.limit - self.bytes.len() {
            self.failed = true;
            return Err(io::Error::new(io::ErrorKind::WriteZero, "HOST_OUTPUT_LIMIT"));
        }
        self.bytes.extend_from_slice(bytes); Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> { Ok(()) }
}
