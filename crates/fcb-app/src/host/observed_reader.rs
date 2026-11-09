#![forbid(unsafe_code)]

//! Bounded reading of ONE explicitly opened regular file, without retaining it
//! in full. This is a separate capability from the immutable `ReaderSession`.
//! Opening observes metadata only. Each window reads a fresh source revision;
//! retained windows and copies use only their original owned extent bytes.
//!
//! The host retains its root grant and runs every call/drop on a worker. Native
//! path composition rejects traversal and checks each selected component; the
//! existing opener protects the leaf object. This is not a claim of race-safe
//! confinement against hostile ancestor replacement. An opened descriptor does
//! not follow later pathname replacements. Reopen explicitly to observe those.
//!
//! `fcb.observed-reader/1` uses decimal-string integers and reversible absolute
//! path bytes. Window receipts distinguish requested/captured/visible ranges,
//! extent-only SHA-256, before/after lengths and metadata consistency. Neither
//! a matching stat nor an extent digest identifies a whole immutable file.
//! Declared encoding belongs to each observation; no earlier BOM is reused.
//! Logical text is not qualified paragraph shaping or a global line index.
//! Explicit bounded searches scan this same open object and retain only exact
//! literal witnesses. Showing surrounding current bytes is a separate window
//! observation; it cannot silently replace the bytes behind an older hit.

mod search;
pub use search::{MAX_OBSERVED_READER_MATCHES, MAX_OBSERVED_READER_NEEDLE_BYTES,
    MAX_OBSERVED_READER_SCAN_BYTES, MAX_OBSERVED_READER_SEARCH_CALLS};

use std::{fs, mem::size_of, path::{Path, PathBuf}, sync::Arc};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentConsistency, ExtentError,
    ExtentReadState, ExtentReadStats, ExtentStepBudget, ExtentText, ExtentViewError,
    ExtentWindowRequest, FileRangeReader, ObservedExtent, QueryGeneration,
    ResourceAllocationId, ResourceBudget};
use fcb::source::{NormalizedPath, SourceError};
use fcb::store::{Sha256, Sha256Digest};
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES, input, workspace};
use crate::output::{MAX_RESPONSE_BYTES, Output, OutputError};
use super::HostResponse;

pub const MAX_OBSERVED_READER_WINDOW_BYTES: usize = 256 * 1024;
pub const MAX_OBSERVED_READER_OBSERVATIONS: usize = 8;
pub const MAX_OBSERVED_READER_READ_CALLS: u64 = 4096;
const MAX_PATH_BYTES: usize = 16_384;
const MAX_PATH_COMPONENTS: usize = 256;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedReaderEncoding { Utf8, Utf16Le, Utf16Be }
impl ObservedReaderEncoding {
    pub const fn code(self) -> &'static str {
        match self { Self::Utf8 => "utf8", Self::Utf16Le => "utf16le", Self::Utf16Be => "utf16be" }
    }
    fn for_extent(self, extent: &ObservedExtent) -> DetectedEncoding {
        match self {
            Self::Utf8 => DetectedEncoding::Utf8 { has_bom: extent.range().start().get() == 0
                && extent.bytes().starts_with(&[0xef, 0xbb, 0xbf]) },
            Self::Utf16Le => DetectedEncoding::Utf16Le,
            Self::Utf16Be => DetectedEncoding::Utf16Be,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedReaderError {
    App(AppError), Source(SourceError), Extent(ExtentError), View(ExtentViewError),
    InvalidPath, InvalidLimits, InvalidRange, MissingObservation, ObservationChanged,
    StaleQuery, MissingHit, Canceled, IdentityExhausted,
}
impl ObservedReaderError {
    pub fn is_canceled(&self) -> bool {
        match self {
            Self::Canceled | Self::Source(SourceError::Canceled) => true,
            Self::Extent(error) | Self::App(AppError::Extent(error)) => extent_canceled(*error),
            Self::View(error) | Self::App(AppError::View(error)) => view_canceled(*error),
            Self::App(error) => error.is_canceled(),
            _ => false,
        }
    }
}
impl std::fmt::Display for ObservedReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(error) => write!(f, "{error}"), Self::Source(error) => write!(f, "{error}"),
            Self::Extent(error) => write!(f, "{error}"), Self::View(error) => write!(f, "{error}"),
            Self::InvalidPath => f.write_str("OBSERVED_READER_INVALID_PATH"),
            Self::InvalidLimits => f.write_str("OBSERVED_READER_INVALID_LIMITS"),
            Self::InvalidRange => f.write_str("OBSERVED_READER_INVALID_RANGE"),
            Self::MissingObservation => f.write_str("OBSERVED_READER_OBSERVATION_UNAVAILABLE"),
            Self::ObservationChanged => f.write_str("OBSERVED_READER_LENGTH_CHANGED_DURING_ADMISSION"),
            Self::StaleQuery => f.write_str("OBSERVED_READER_STALE_QUERY"),
            Self::MissingHit => f.write_str("OBSERVED_READER_MISSING_HIT"),
            Self::Canceled => f.write_str("OBSERVED_READER_CANCELED"),
            Self::IdentityExhausted => f.write_str("OBSERVED_READER_IDENTITY_EXHAUSTED"),
        }
    }
}
impl std::error::Error for ObservedReaderError {}
impl From<AppError> for ObservedReaderError { fn from(error: AppError) -> Self { Self::App(error) } }
impl From<SourceError> for ObservedReaderError { fn from(error: SourceError) -> Self { Self::Source(error) } }
impl From<ExtentError> for ObservedReaderError { fn from(error: ExtentError) -> Self { Self::Extent(error) } }
impl From<ExtentViewError> for ObservedReaderError { fn from(error: ExtentViewError) -> Self { Self::View(error) } }
impl From<OutputError> for ObservedReaderError { fn from(error: OutputError) -> Self { Self::App(error.into()) } }

// Keep raw payloads, not decoder maps: eight worst-case non-ASCII maps would
// multiply memory unnecessarily. Replay decodes this same bounded owned data.
#[derive(Clone)]
struct Observation {
    extent: ObservedExtent,
    plan: ExtentWindowRequest,
    requested_bytes: usize,
    encoding: ObservedReaderEncoding,
    sha256: Sha256Digest,
    io: ExtentReadStats,
}

/// Each session requires a fresh non-reused owner. All accepted observations
/// remain immutable, including after live writes. At most eight raw extents
/// survive; an evicted ID fails rather than rereading its old coordinates.
pub struct ObservedReaderSession {
    reader: FileRangeReader,
    // Duplicated from the admitted descriptor, never reopened by pathname.
    // Positioned scan reads do not move the range reader's shared file cursor.
    search_file: fs::File,
    path: PathBuf,
    initial_length: ByteLength,
    observations: [Option<Observation>; MAX_OBSERVED_READER_OBSERVATIONS],
    next_slot: usize,
    last_observation: u64,
    last_query_attempt: u64,
    query: Option<Arc<search::AcceptedSearch>>,
    next_allocation: u64,
    source_bytes_read: u64,
    read_calls: u64,
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl ObservedReaderSession {
    /// Open the CURRENT explicitly selected root-relative native path. No
    /// catalog/search capture is accepted or silently substituted by this API.
    /// Only metadata is read. The caller retains current access until close.
    pub fn open(owner: ArenaOwnerId, root: &Path, relative: &[u8],
        mut canceled: impl FnMut() -> bool) -> Result<Self, ObservedReaderError> {
        check(&mut canceled)?;
        validate_paths(root, relative)?;
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
        // Admission includes temporary path/segment overlap, the retained path
        // and all fixed observation slots, before path allocation or native I/O.
        let lease = budget.try_reserve_managed(owner, allocation(1)?,
            ByteLength::new((256 * 1024 + size_of::<Self>()) as u64)).map_err(|_| AppError::Admission)?;
        let root = input::absolute(root)?;
        let mut parts = relative.split(|byte| *byte == b'/');
        let mut selected = NormalizedPath::from_dirent_name(parts.next().ok_or(ObservedReaderError::InvalidPath)?)?;
        for part in parts { check(&mut canceled)?; selected = selected.join_segment(part)?; }
        check(&mut canceled)?;
        let metadata = fs::symlink_metadata(&root).map_err(|_| AppError::Io)?;
        if metadata.file_type().is_symlink() { return Err(AppError::Symlink.into()); }
        if !metadata.is_dir() { return Err(ObservedReaderError::InvalidPath); }
        let path = workspace::checked_source_path(&root, &selected)?;
        check(&mut canceled)?;
        let (file, metadata) = input::open_regular(&path)?;
        let search_file = file.try_clone().map_err(|_| AppError::Io)?;
        let file_id = FileId::new(owner, 1).map_err(|_| ObservedReaderError::IdentityExhausted)?;
        let reader = FileRangeReader::new(file_id, file)?;
        check(&mut canceled)?;
        Ok(Self { reader, search_file, path, initial_length: ByteLength::new(metadata.len()),
            observations: std::array::from_fn(|_| None), next_slot: 0, last_observation: 0,
            last_query_attempt: 0, query: None,
            next_allocation: 2, source_bytes_read: 0, read_calls: 0, budget, _lease: lease })
    }

    /// Stored metadata and actual cumulative I/O counters; performs no stat or
    /// payload read. Initial length is explicitly the opening observation.
    pub fn info(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ObservedReaderError> {
        check(&mut canceled)?;
        let mut out = self.output("info")?;
        out.literal(",\"initial_observed_length\":")?; out.integer(self.initial_length.get())?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(self.source_bytes_read)?;
        out.literal(",\"read_calls\":")?; out.integer(self.read_calls)?;
        out.literal(",\"cache_capacity\":")?; out.integer(MAX_OBSERVED_READER_OBSERVATIONS as u64)?;
        out.literal(",\"retained_payload_bytes\":")?;
        out.integer(self.observations.iter().flatten().map(|page| page.extent.bytes().len() as u64).sum())?;
        let mut ids: [u64; MAX_OBSERVED_READER_OBSERVATIONS] = std::array::from_fn(|i|
            self.observations[i].as_ref().map_or(0, |page| page.extent.request().revision().get()));
        ids.sort_unstable();
        out.literal(",\"retained_observations\":[")?;
        for (position, id) in ids.into_iter().filter(|id| *id != 0).enumerate() {
            if position > 0 { out.literal(",")?; }
            out.integer(id)?;
        }
        out.literal("]}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    /// One bounded current-file observation. Failed/canceled attempts consume
    /// their revision and actual I/O, but cannot evict any accepted observation.
    /// Scalars/CRLF may adjust the visible bounds; no prefix scan is performed.
    pub fn read_window(&mut self, offset: u64, bytes: usize, encoding: ObservedReaderEncoding,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ObservedReaderError> {
        self.last_observation = self.last_observation.checked_add(1).ok_or(ObservedReaderError::IdentityExhausted)?;
        let revision = SourceRevision::new(self.owner(), self.last_observation).map_err(|_| ObservedReaderError::IdentityExhausted)?;
        check(&mut canceled)?;
        if !(4..=MAX_OBSERVED_READER_WINDOW_BYTES).contains(&bytes) { return Err(ObservedReaderError::InvalidLimits); }
        // Refuse before reading if cumulative accounting could no longer be
        // represented. Failed native reads spend these allowances too.
        self.source_bytes_read.checked_add((MAX_OBSERVED_READER_WINDOW_BYTES + 16) as u64)
            .ok_or(ObservedReaderError::IdentityExhausted)?;
        self.read_calls.checked_add(MAX_OBSERVED_READER_READ_CALLS).ok_or(ObservedReaderError::IdentityExhausted)?;
        let length = self.reader.observed_length()?;
        let plan = ExtentWindowRequest::new(ByteOffset::new(offset), bytes, length)?;
        let mut request = CaptureRequest::new(self.reader.file_id(), revision)?;
        if !plan.capture.is_empty() { request = request.with_range(plan.capture)?; }
        let [read_id] = self.allocations()?;
        let (captured, io) = capture(&mut self.reader, request, &self.budget, read_id, &mut canceled);
        self.source_bytes_read += io.bytes_read; // Headroom was admitted above.
        self.read_calls += io.read_calls;
        let extent = captured?;
        // Planning and the read's initial stat are distinct observations. Do
        // not encode bounds planned from a different length as though identical.
        if extent.observed_length() != length { return Err(ObservedReaderError::ObservationChanged); }
        let sha256 = extent_sha256(extent.bytes(), &mut canceled)?;
        let candidate = Observation { extent, plan, requested_bytes: bytes, encoding, sha256, io };
        let response = self.page_response("window", &candidate, true, &mut canceled)?;
        // Publication is a single infallible move after the final delivery
        // cancellation gate. A missing foreign reply can recover by ID/info.
        self.observations[self.next_slot] = Some(candidate);
        self.next_slot = (self.next_slot + 1) % MAX_OBSERVED_READER_OBSERVATIONS;
        Ok(response)
    }

    pub fn retained_window(&mut self, observation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        check(&mut canceled)?;
        let page = self.observation(observation)?.clone(); // Shares payload AND its lease.
        self.page_response("retained-window", &page, false, &mut canceled)
    }

    /// Exact retained bytes, never a live read or a clipboard side effect.
    /// A stale ID, unknown hole, reversed range or oversized copy is refused.
    pub fn copy_range(&mut self, observation: u64, start: u64, end: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ObservedReaderError> {
        check(&mut canceled)?;
        let selected = range(start, end)?;
        if selected.len().get() > MAX_OBSERVED_READER_WINDOW_BYTES as u64 { return Err(ObservedReaderError::InvalidLimits); }
        let page = self.observation(observation)?.clone();
        let bytes = page.extent.range_bytes(selected)?;
        let mut out = self.output("copy-range")?;
        encode_observation(&mut out, &page, false)?;
        out.literal(",\"copy_domain\":\"original-bytes\",\"original_range\":")?; out.range(selected)?;
        out.literal(",\"original_hex\":")?; out.hex(bytes)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    fn owner(&self) -> ArenaOwnerId { self.reader.file_id().owner() }
    fn observation(&self, id: u64) -> Result<&Observation, ObservedReaderError> {
        self.observations.iter().flatten().find(|page| page.extent.request().revision().get() == id)
            .ok_or(ObservedReaderError::MissingObservation)
    }
    fn allocations<const N: usize>(&mut self) -> Result<[ResourceAllocationId; N], ObservedReaderError> {
        let first = self.next_allocation;
        self.next_allocation = first.checked_add(N as u64).ok_or(ObservedReaderError::IdentityExhausted)?;
        Ok(std::array::from_fn(|i| ResourceAllocationId::new(first + i as u64).expect("checked nonzero allocation")))
    }
    fn page_response(&mut self, command: &str, page: &Observation, new_read: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, ObservedReaderError> {
        check(canceled)?;
        let [decode_id, clipped_decode_id] = self.allocations()?;
        let text = decode(page, &self.budget, [decode_id, clipped_decode_id], canceled)?;
        let mut out = self.output(command)?;
        encode_observation(&mut out, page, new_read)?;
        out.literal(",\"text_kind\":\"logical-observed-text-not-shaped\",\"visible_range\":")?; out.range(text.range())?;
        out.literal(",\"boundaries_adjusted\":")?; out.boolean(text.range() != page.plan.visible)?;
        out.literal(",\"first_physical_line\":")?;
        if let Some(line) = text.first_line_number().filter(|_| !text.range().is_empty()) {
            out.integer(line)?;
        } else { out.literal("null")?; }
        out.literal(",\"text\":")?; out.quoted(text.text())?;
        out.literal(",\"original_hex\":")?; out.hex(page.extent.range_bytes(text.range())?)?;
        out.literal(",\"has_replacements\":")?; out.boolean(text.has_replacements())?;
        out.literal(",\"next_offset\":")?;
        if let Some(next) = text.next_offset() { out.integer(next.get())?; } else { out.literal("null")?; }
        out.literal("}\n")?;
        let exit = if whole_observation(&page.extent) { EXIT_OK } else { EXIT_PARTIAL };
        self.finish_output(out, exit, canceled)
    }
    fn output(&mut self, command: &str) -> Result<Output, ObservedReaderError> {
        let [id] = self.allocations()?;
        let mut out = Output::new(self.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.observed-reader/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.owner().get())?;
        out.literal(",\"file_id\":")?; out.integer(self.reader.file_id().get())?;
        out.literal(",\"path\":")?; out.path(&self.path)?;
        out.literal(",\"capture_kind\":\"observed-extent\",\"source_observation\":\"open-file-ranges-not-atomic\",\"whole_file_immutable\":false,\"native_presented\":false")?;
        Ok(out)
    }
    fn finish_output(&mut self, out: Output, exit_code: u8, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        check(canceled)?;
        let [id] = self.allocations()?;
        let length = out.as_bytes().len();
        let charge = length.checked_mul(2).and_then(|n| n.checked_add(size_of::<HostResponse>() + 16))
            .ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.owner(), id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut text = String::new(); text.try_reserve_exact(length).map_err(|_| AppError::Admission)?;
        if text.capacity() > length { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        check(canceled)?;
        Ok(HostResponse { text, exit_code, _lease: lease })
    }
}

fn capture(reader: &mut FileRangeReader, request: CaptureRequest, budget: &ResourceBudget,
    allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
    -> (Result<ObservedExtent, ObservedReaderError>, ExtentReadStats) {
    let mut io = ExtentReadStats::default();
    let result = (|| {
        check(canceled)?;
        let mut read = reader.begin(request, budget, allocation)?;
        while read.state() == ExtentReadState::Pending {
            if io.read_calls >= MAX_OBSERVED_READER_READ_CALLS { return Err(AppError::IoCallLimit.into()); }
            let calls = (MAX_OBSERVED_READER_READ_CALLS - io.read_calls).min(32) as usize;
            let result = read.step(ExtentStepBudget { max_bytes: 64 * 1024, max_calls: calls }, &mut *canceled);
            io = read.stats(); // Preserve spent work even when this step fails.
            result?;
        }
        Ok(read.finish(&mut *canceled)?)
    })();
    (result, io)
}

fn decode(page: &Observation, budget: &ResourceBudget, ids: [ResourceAllocationId; 2],
    canceled: &mut impl FnMut() -> bool) -> Result<ExtentText, ObservedReaderError> {
    let owner = page.extent.request().file().owner();
    let generation = QueryGeneration::new(owner, page.extent.request().revision().get())
        .map_err(|_| ObservedReaderError::IdentityExhausted)?;
    let view = BrowserSession::new(owner).open_extent(page.extent.clone())?;
    let encoding = page.encoding.for_extent(&page.extent);
    let text = view.decode(page.plan.visible, encoding, generation, budget, ids[0], &mut *canceled)?;
    if text.range().len().get() <= page.requested_bytes as u64 { return Ok(text); }
    // Flooring the start can expand a mid-scalar request by up to three bytes.
    // Ask the SAME decoder to clip the end, retaining its scalar/CRLF rules.
    // Drop the first map before allocating the replacement; no overlap spike.
    let end = text.range().start().get().checked_add(page.requested_bytes as u64)
        .ok_or(ObservedReaderError::InvalidRange)?;
    drop(text);
    Ok(view.decode(range(page.plan.visible.start().get(), end)?, encoding,
        generation, budget, ids[1], canceled)?)
}

fn encode_observation(out: &mut Output, page: &Observation, new_read: bool) -> Result<(), ObservedReaderError> {
    let extent = &page.extent;
    out.literal(",\"observation\":")?; out.integer(extent.request().revision().get())?;
    out.literal(",\"encoding\":")?; out.quoted(page.encoding.code())?;
    out.literal(",\"encoding_basis\":\"explicit-for-this-observation\",\"requested_offset\":")?;
    out.integer(page.plan.visible.start().get())?;
    out.literal(",\"requested_bytes\":")?; out.integer(page.requested_bytes as u64)?;
    out.literal(",\"requested_visible_range\":")?; out.range(page.plan.visible)?;
    out.literal(",\"requested_capture_range\":")?; out.range(page.plan.capture)?;
    out.literal(",\"observed_length\":")?; out.integer(extent.observed_length().get())?;
    out.literal(",\"final_observed_length\":")?;
    if let Some(length) = extent.final_length() { out.integer(length.get())?; } else { out.literal("null")?; }
    out.literal(",\"captured_range\":")?; out.range(extent.range())?;
    out.literal(",\"captured_bytes\":")?; out.integer(extent.bytes().len() as u64)?;
    out.literal(",\"extent_sha256\":")?; out.quoted(&page.sha256.to_hex())?;
    out.literal(",\"consistency\":")?;
    out.quoted(match extent.consistency() {
        ExtentConsistency::HostSupplied => "host-supplied-sequence",
        ExtentConsistency::UnchangedMetadata => "unchanged-metadata-not-atomic",
        ExtentConsistency::ChangedDuringRead => "changed-during-read",
        ExtentConsistency::ShortRead => "short-read",
        ExtentConsistency::MetadataUnavailable => "metadata-unavailable",
    })?;
    out.literal(",\"requested_capture_filled\":")?; out.boolean(extent.request_filled())?;
    out.literal(",\"whole_file_observed\":")?; out.boolean(whole_observation(extent))?;
    for (key, value) in [("observation_source_bytes_read", page.io.bytes_read), ("observation_read_calls", page.io.read_calls),
        ("additional_source_bytes_read", if new_read { page.io.bytes_read } else { 0 }),
        ("additional_read_calls", if new_read { page.io.read_calls } else { 0 })] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    Ok(())
}
fn whole_observation(extent: &ObservedExtent) -> bool {
    extent.request_filled() && extent.covers_whole_observation()
        && extent.consistency() == ExtentConsistency::UnchangedMetadata
}
fn extent_sha256(bytes: &[u8], canceled: &mut impl FnMut() -> bool) -> Result<Sha256Digest, ObservedReaderError> {
    let mut hash = Sha256::new();
    for chunk in bytes.chunks(64 * 1024) { check(canceled)?; hash.update(chunk); }
    check(canceled)?;
    Ok(hash.finalize())
}
fn range(start: u64, end: u64) -> Result<ByteRange, ObservedReaderError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| ObservedReaderError::InvalidRange)
}
fn allocation(id: u64) -> Result<ResourceAllocationId, ObservedReaderError> {
    ResourceAllocationId::new(id).map_err(|_| ObservedReaderError::IdentityExhausted)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), ObservedReaderError> {
    if canceled() { Err(ObservedReaderError::Canceled) } else { Ok(()) }
}
fn extent_canceled(error: ExtentError) -> bool {
    matches!(error, ExtentError::Canceled | ExtentError::Source(SourceError::Canceled))
}
fn view_canceled(error: ExtentViewError) -> bool {
    match error { ExtentViewError::Canceled => true, ExtentViewError::Extent(error) => extent_canceled(error), _ => false }
}

fn validate_paths(root: &Path, relative: &[u8]) -> Result<(), ObservedReaderError> {
    if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
    #[cfg(unix)] {
        use std::os::unix::ffi::OsStrExt;
        let root = root.as_os_str().as_bytes();
        if root.first() != Some(&b'/') || root.len() > MAX_PATH_BYTES || root.contains(&0)
            || (root != b"/" && root[1..].split(|byte| *byte == b'/').any(invalid_component))
            || relative.is_empty() || relative.contains(&0) || relative.len() > MAX_PATH_BYTES
            || relative.split(|byte| *byte == b'/').any(invalid_component)
            || relative.split(|byte| *byte == b'/').count() > MAX_PATH_COMPONENTS {
            return Err(ObservedReaderError::InvalidPath);
        }
        let separator = usize::from(root != b"/");
        if root.len().checked_add(separator).and_then(|n| n.checked_add(relative.len()))
            .is_none_or(|length| length > MAX_PATH_BYTES) { return Err(ObservedReaderError::InvalidPath); }
        Ok(())
    }
    #[cfg(not(unix))] { let _ = (root, relative); Err(AppError::UnsupportedPlatform.into()) }
}
fn invalid_component(component: &[u8]) -> bool {
    component.is_empty() || component == b"." || component == b".."
}
