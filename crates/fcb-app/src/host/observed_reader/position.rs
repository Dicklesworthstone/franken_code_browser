#![forbid(unsafe_code)]

//! Exact typed line/column navigation over one bounded forward observation.
//! The shared source scanner owns physical lines and decoder coordinates.
//! This worker retains page context during that SAME pass, never locating a
//! position and then rereading mutable bytes at the resolved offset.

use std::{io::{self, Read}, mem::size_of};
use fcb::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange};
use fcb::search::{CaptureRequest, ExtentReadStats, ExtentText, ExtentWindowRequest,
    ObservedExtent, ResourceAllocationId, ResourceBudget};
use fcb::source::{DetectedEncoding, position::{LineColumnError, LineColumnScanner,
    LineColumnStatus, MAX_POSITION_STEP_BYTES}};
use fcb_core::ResourceLease;
use crate::{AppError, lines::same_metadata, output::Output};
use super::super::reader::ReaderColumnKind;
use super::{HostResponse, Observation, ObservedReaderEncoding, ObservedReaderError,
    ObservedReaderSession, MAX_OBSERVED_READER_LINE_READ_CALLS,
    MAX_OBSERVED_READER_LINE_SCAN_BYTES, MAX_OBSERVED_READER_WINDOW_BYTES,
    check, decode, extent_sha256, range};

/// This immutable receipt belongs to the original observation page, including
/// when echoed by a copy/selection of a smaller original byte range.
#[derive(Clone, Copy)]
pub(super) struct PositionNavigation {
    line: u64,
    column: u64,
    kind: ReaderColumnKind,
    original_offset: u64,
    window_utf8_offset: u64,
    line_start: u64,
    max_scan_bytes: u64,
    max_selected_bytes: usize,
    scanned_input_bytes: u64,
    payload_bytes_read: u64,
    read_calls: u64,
}
impl PositionNavigation {
    pub(super) fn first_physical_line(self, text: &ExtentText) -> Option<u64> {
        // Every byte between this line's start and the resolved point belongs
        // to this line. A preceding line in the page prefix is not guessed.
        (text.range().start().get() >= self.line_start
            && text.range().start().get() <= self.original_offset).then_some(self.line)
    }
}

impl ObservedReaderSession {
    /// Scan the opened descriptor from zero to a one-based physical line and
    /// typed one-based logical column. A content-end caret is valid; interior
    /// scalar/surrogate boundaries and missing coordinates are explicit errors.
    /// Tabs count as one unit. This does not promise a shaped visual caret.
    ///
    /// A giant line consumes bounded decoder steps and retained context, not
    /// memory proportional to its length. Scan/call limits are never EOF.
    /// Failed or canceled attempts charge their spent I/O and retain old pages.
    pub fn read_position(&mut self, line: u64, column: u64, kind: ReaderColumnKind,
        max_bytes: usize, max_scan_bytes: u64, encoding: ObservedReaderEncoding,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ObservedReaderError> {
        let revision = self.next_revision()?;
        check(&mut canceled)?;
        if line == 0 || column == 0
            || !(4..=MAX_OBSERVED_READER_WINDOW_BYTES).contains(&max_bytes)
            || !(1..=MAX_OBSERVED_READER_LINE_SCAN_BYTES).contains(&max_scan_bytes) {
            return Err(ObservedReaderError::InvalidLimits);
        }
        self.source_bytes_read.checked_add(max_scan_bytes).ok_or(ObservedReaderError::IdentityExhausted)?;
        self.read_calls.checked_add(MAX_OBSERVED_READER_LINE_READ_CALLS)
            .ok_or(ObservedReaderError::IdentityExhausted)?;
        let [selected_id, workspace_id, extent_id] = self.allocations()?;
        let before = self.search_file.metadata().map_err(|_| AppError::Io)?;
        let request = PositionRequest { line, column, kind, max_bytes, max_scan_bytes,
            max_read_calls: MAX_OBSERVED_READER_LINE_READ_CALLS, encoding };
        let mut source = super::lines::PositionedReader { file: &self.search_file, offset: 0 };
        let mut io = ExtentReadStats::default();
        let scanned = scan_position(&mut source, request, &self.budget, self.owner(),
            [selected_id, workspace_id], &mut io, &mut canceled);
        self.source_bytes_read += io.bytes_read;
        self.read_calls += io.read_calls;
        let scanned = scanned?;
        check(&mut canceled)?;
        let after = self.search_file.metadata().map_err(|_| AppError::Io)?;
        if !same_metadata(&before, &after) { return Err(AppError::SourceChanged.into()); }
        let capture = range(scanned.capture_start,
            scanned.capture_start.checked_add(scanned.selected.len() as u64)
                .ok_or(ObservedReaderError::InvalidRange)?)?;
        let request = CaptureRequest::new(self.reader.file_id(), revision)?.with_range(capture)?;
        let extent = ObservedExtent::from_bytes(request, ByteLength::new(before.len()),
            &scanned.selected, &self.budget, extent_id)?;
        let receipt = PositionNavigation { line, column, kind,
            original_offset: scanned.original_offset, window_utf8_offset: 0,
            line_start: scanned.line_start, max_scan_bytes, max_selected_bytes: max_bytes,
            scanned_input_bytes: scanned.scanned_input_bytes,
            payload_bytes_read: io.bytes_read, read_calls: io.read_calls };
        let visible = scanned.visible;
        drop(scanned);
        let sha256 = extent_sha256(extent.bytes(), &mut canceled)?;
        let mut candidate = Observation { extent, plan: ExtentWindowRequest { visible, capture },
            requested_bytes: max_bytes, encoding, sha256, io, find_hit: None,
            line_navigation: None, position_navigation: Some(receipt) };
        let [decode_id, clipped_decode_id] = self.allocations()?;
        let text = decode(&candidate, &self.budget, [decode_id, clipped_decode_id], &mut canceled)?;
        // Verify through the SAME decoder/map used for the presented page.
        // Floor/clip adjustment can change the page's decoded origin.
        let point = range(receipt.original_offset, receipt.original_offset)?;
        let decoded = text.source_to_text(point)?;
        let verified = text.text_selection(decoded)?;
        if verified.original_range != point || !verified.original_bytes.is_empty() || !decoded.is_empty() {
            return Err(ObservedReaderError::InvalidRange);
        }
        candidate.position_navigation = Some(PositionNavigation {
            window_utf8_offset: decoded.start().get(), ..receipt });
        let response = self.page_response_decoded("position", &candidate, &text, true, &mut canceled)?;
        self.accept_observation(candidate);
        Ok(response)
    }
}

#[derive(Clone, Copy)]
struct PositionRequest {
    line: u64, column: u64, kind: ReaderColumnKind, max_bytes: usize,
    max_scan_bytes: u64, max_read_calls: u64, encoding: ObservedReaderEncoding,
}
struct PositionScan {
    selected: Vec<u8>,
    capture_start: u64,
    visible: ByteRange,
    original_offset: u64,
    line_start: u64,
    scanned_input_bytes: u64,
    _selected: ResourceLease,
}

/// Fixed-capacity rolling raw history. Copy in at most two slices per block;
/// no shifting or per-byte queue operations along a giant line.
struct History {
    bytes: Vec<u8>,
    start: usize,
    len: usize,
    end: u64,
}
impl History {
    fn new(capacity: usize) -> Result<Self, ObservedReaderError> {
        let mut bytes = admitted_vec(capacity)?;
        bytes.resize(capacity, 0);
        Ok(Self { bytes, start: 0, len: 0, end: 0 })
    }
    fn remember(&mut self, base: u64, bytes: &[u8]) -> Result<(), ObservedReaderError> {
        if base != self.end { return Err(ObservedReaderError::InvalidRange); }
        self.end = base.checked_add(bytes.len() as u64).ok_or(ObservedReaderError::InvalidRange)?;
        let capacity = self.bytes.len();
        if bytes.len() >= capacity {
            self.bytes.copy_from_slice(&bytes[bytes.len() - capacity..]);
            self.start = 0; self.len = capacity; return Ok(());
        }
        let at = (self.start + self.len) % capacity;
        let first = bytes.len().min(capacity - at);
        self.bytes[at..at + first].copy_from_slice(&bytes[..first]);
        self.bytes[..bytes.len() - first].copy_from_slice(&bytes[first..]);
        let total = self.len + bytes.len();
        if total > capacity { self.start = (self.start + total - capacity) % capacity; }
        self.len = total.min(capacity);
        Ok(())
    }
    fn retain(&self, selection: &mut Selection) -> Result<(), ObservedReaderError> {
        let base = self.end - self.len as u64;
        let first = self.len.min(self.bytes.len() - self.start);
        selection.retain(base, &self.bytes[self.start..self.start + first])?;
        selection.retain(base + first as u64, &self.bytes[..self.len - first])
    }
}
#[derive(Clone, Copy)]
struct PositionWindow {
    original_offset: u64,
    visible_start: u64,
    visible_end: u64,
    capture_start: u64,
    capture_end: u64,
}
struct Selection {
    bytes: Vec<u8>,
    window: Option<PositionWindow>,
    capacity: usize,
}
impl Selection {
    fn begin(&mut self, original_offset: u64, max_bytes: usize) -> Result<(), ObservedReaderError> {
        if self.window.is_some() { return Ok(()); }
        // Keep four bytes after the target, even with the smallest page.
        let before = (max_bytes as u64 - 4) / 2;
        let visible_start = original_offset.saturating_sub(before);
        let visible_end = visible_start.checked_add(max_bytes as u64)
            .ok_or(ObservedReaderError::InvalidRange)?;
        let capture_start = visible_start.saturating_sub(4);
        let capture_end = visible_end.checked_add(4).ok_or(ObservedReaderError::InvalidRange)?;
        self.window = Some(PositionWindow { original_offset, visible_start, visible_end,
            capture_start, capture_end });
        Ok(())
    }
    fn retain(&mut self, base: u64, bytes: &[u8]) -> Result<(), ObservedReaderError> {
        let Some(window) = self.window else { return Ok(()); };
        let from = window.capture_start.checked_add(self.bytes.len() as u64)
            .ok_or(ObservedReaderError::InvalidRange)?;
        let end = base.checked_add(bytes.len() as u64).ok_or(ObservedReaderError::InvalidRange)?
            .min(window.capture_end);
        if end <= from { return Ok(()); }
        if from < base { return Err(ObservedReaderError::InvalidRange); }
        let selected = &bytes[(from - base) as usize..(end - base) as usize];
        if selected.len() > self.capacity - self.bytes.len() { return Err(ObservedReaderError::InvalidRange); }
        self.bytes.extend_from_slice(selected);
        Ok(())
    }
}

fn admitted_vec(capacity: usize) -> Result<Vec<u8>, ObservedReaderError> {
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if bytes.capacity() > capacity { return Err(AppError::Admission.into()); }
    Ok(bytes)
}
fn scanner_error(_: LineColumnError) -> ObservedReaderError {
    // Includes a typed column inside a Unicode scalar/surrogate. Never snap it.
    ObservedReaderError::InvalidRange
}
fn read(reader: &mut impl Read, buffer: &mut [u8], io: &mut ExtentReadStats,
    canceled: &mut impl FnMut() -> bool) -> Result<Option<usize>, ObservedReaderError> {
    check(canceled)?;
    io.read_calls += 1; io.last_step_calls = 1; io.last_step_bytes = 0;
    match reader.read(buffer) {
        Ok(count) if count <= buffer.len() => {
            io.bytes_read += count as u64; io.last_step_bytes = count; Ok(Some(count))
        }
        Err(error) if error.kind() == io::ErrorKind::Interrupted => {
            io.interrupted_calls += 1; Ok(None)
        }
        _ => Err(AppError::Io.into()),
    }
}

fn scan_position(reader: &mut impl Read, request: PositionRequest, budget: &ResourceBudget,
    owner: ArenaOwnerId, allocations: [ResourceAllocationId; 2], io: &mut ExtentReadStats,
    canceled: &mut impl FnMut() -> bool) -> Result<PositionScan, ObservedReaderError> {
    let capacity = request.max_bytes.checked_add(8).ok_or(AppError::Admission)?;
    let history_capacity = (request.max_bytes - 4) / 2 + 8;
    let selected_lease = budget.try_reserve_managed(owner, allocations[0], ByteLength::new(capacity as u64))
        .map_err(|_| AppError::Admission)?;
    let workspace = LineColumnScanner::workspace_bytes().checked_add(MAX_POSITION_STEP_BYTES)
        .and_then(|n| n.checked_add(history_capacity))
        .and_then(|n| n.checked_add(size_of::<History>() + size_of::<Selection>() + 3))
        .ok_or(AppError::Admission)?;
    let _workspace = budget.try_reserve_managed(owner, allocations[1], ByteLength::new(workspace as u64))
        .map_err(|_| AppError::Admission)?;
    let mut selection = Selection { bytes: admitted_vec(capacity)?, window: None, capacity };
    let mut history = History::new(history_capacity)?;
    let mut prefix = [0u8; 3];
    let mut filled = 0usize;
    let mut eof = false;
    while filled < prefix.len() && io.bytes_read < request.max_scan_bytes && io.read_calls < request.max_read_calls {
        let count = (prefix.len() - filled).min((request.max_scan_bytes - io.bytes_read).min(3) as usize);
        match read(reader, &mut prefix[filled..filled + count], io, canceled)? {
            Some(0) => { eof = true; break; }
            Some(count) => filled += count,
            None => {},
        }
    }
    let encoding = match request.encoding {
        ObservedReaderEncoding::Utf8 => DetectedEncoding::Utf8 {
            has_bom: prefix[..filled].starts_with(&[0xef, 0xbb, 0xbf]) },
        ObservedReaderEncoding::Utf16Le => DetectedEncoding::Utf16Le,
        ObservedReaderEncoding::Utf16Be => DetectedEncoding::Utf16Be,
    };
    let mut cursor = LineColumnScanner::new(request.line, request.column, request.kind, encoding)
        .map_err(scanner_error)?;
    cursor.step(ByteOffset::new(0), &prefix[..filled], filled).map_err(scanner_error)?;
    if let LineColumnStatus::Resolved { original_offset } = cursor.status() {
        selection.begin(original_offset.get(), request.max_bytes)?;
        selection.retain(0, &prefix[..filled])?;
    }
    history.remember(0, &prefix[..filled])?;
    let mut buffer = [0u8; MAX_POSITION_STEP_BYTES];
    loop {
        check(canceled)?;
        if eof && cursor.status() == LineColumnStatus::Pending { cursor.finish().map_err(scanner_error)?; }
        let mut wanted = MAX_POSITION_STEP_BYTES as u64;
        match cursor.status() {
            LineColumnStatus::Resolved { original_offset } => {
                if selection.window.is_none() {
                    selection.begin(original_offset.get(), request.max_bytes)?;
                    history.retain(&mut selection)?;
                }
                let window = selection.window.ok_or(ObservedReaderError::InvalidRange)?;
                if eof || io.bytes_read >= window.capture_end {
                    let end = window.capture_end.min(io.bytes_read);
                    if window.capture_start.checked_add(selection.bytes.len() as u64) != Some(end) {
                        return Err(ObservedReaderError::InvalidRange);
                    }
                    return Ok(PositionScan { selected: selection.bytes, capture_start: window.capture_start,
                        visible: range(window.visible_start, window.visible_end.min(io.bytes_read))?,
                        original_offset: window.original_offset,
                        line_start: cursor.selected_line_start().ok_or(ObservedReaderError::InvalidRange)?.get(),
                        scanned_input_bytes: cursor.bytes_scanned(), _selected: selected_lease });
                }
                wanted = window.capture_end - io.bytes_read;
            }
            LineColumnStatus::MissingLine => return Err(ObservedReaderError::LineMissing),
            LineColumnStatus::MissingColumn => return Err(ObservedReaderError::PositionColumnMissing),
            LineColumnStatus::Pending => {},
        }
        if io.bytes_read == request.max_scan_bytes { return Err(ObservedReaderError::PositionScanByteLimit); }
        if io.read_calls == request.max_read_calls { return Err(ObservedReaderError::PositionReadCallLimit); }
        let count = wanted.min(request.max_scan_bytes - io.bytes_read)
            .min(MAX_POSITION_STEP_BYTES as u64) as usize;
        let base = io.bytes_read;
        let Some(count) = read(reader, &mut buffer[..count], io, canceled)? else { continue; };
        if count == 0 { eof = true; continue; }
        if cursor.status() == LineColumnStatus::Pending {
            cursor.step(ByteOffset::new(base), &buffer[..count], count).map_err(scanner_error)?;
        }
        if let LineColumnStatus::Resolved { original_offset } = cursor.status() {
            if selection.window.is_none() {
                selection.begin(original_offset.get(), request.max_bytes)?;
                history.retain(&mut selection)?;
            }
            selection.retain(base, &buffer[..count])?;
        } else {
            history.remember(base, &buffer[..count])?;
        }
    }
}

pub(super) fn encode_position_navigation(out: &mut Output, receipt: Option<PositionNavigation>)
    -> Result<(), ObservedReaderError> {
    let Some(receipt) = receipt else { return Ok(()); };
    out.literal(",\"position_navigation\":{\"metadata_consistency\":\"unchanged-metadata-not-atomic\"")?;
    for (key, value) in [("line", receipt.line), ("column", receipt.column),
        ("original_offset", receipt.original_offset), ("window_utf8_offset", receipt.window_utf8_offset),
        ("max_scan_bytes", receipt.max_scan_bytes), ("max_selected_bytes", receipt.max_selected_bytes as u64),
        ("scanned_input_bytes", receipt.scanned_input_bytes), ("payload_bytes_read", receipt.payload_bytes_read),
        ("read_calls", receipt.read_calls)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal(",\"column_kind\":")?; out.quoted(receipt.kind.code())?;
    out.literal("}")?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    struct ShortReader {
        bytes: Vec<u8>, at: usize, chunk: usize, interrupted: usize,
        fail_at: Option<usize>, calls: usize, erase_delivered: bool,
    }
    impl Read for ShortReader {
        fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
            self.calls += 1;
            if self.interrupted > 0 {
                self.interrupted -= 1; return Err(io::Error::from(io::ErrorKind::Interrupted));
            }
            if self.fail_at == Some(self.calls) { return Err(io::Error::from(io::ErrorKind::Other)); }
            let count = output.len().min(self.chunk).min(self.bytes.len() - self.at);
            output[..count].copy_from_slice(&self.bytes[self.at..self.at + count]);
            self.at += count;
            // Once delivered, previous source bytes become different. Exact
            // context must remain the originally supplied forward sequence.
            if self.erase_delivered { self.bytes[..self.at].fill(b'!'); }
            Ok(count)
        }
    }
    fn source(bytes: &[u8], chunk: usize) -> ShortReader {
        ShortReader { bytes: bytes.to_vec(), at: 0, chunk, interrupted: 0,
            fail_at: None, calls: 0, erase_delivered: false }
    }
    fn request() -> PositionRequest {
        PositionRequest { line: 1, column: 1, kind: ReaderColumnKind::UnicodeScalar,
            max_bytes: 8, max_scan_bytes: 1024, max_read_calls: 1024,
            encoding: ObservedReaderEncoding::Utf8 }
    }
    fn run(reader: &mut impl Read, request: PositionRequest, canceled: &mut impl FnMut() -> bool)
        -> (Result<PositionScan, ObservedReaderError>, ExtentReadStats) {
        let owner = ArenaOwnerId::new(9081).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
        let mut io = ExtentReadStats::default();
        let result = scan_position(reader, request, &budget, owner,
            [ResourceAllocationId::new(1).unwrap(), ResourceAllocationId::new(2).unwrap()], &mut io, canceled);
        (result, io)
    }
    fn encode(text: &str, encoding: ObservedReaderEncoding) -> Vec<u8> {
        match encoding {
            ObservedReaderEncoding::Utf8 => text.as_bytes().to_vec(),
            ObservedReaderEncoding::Utf16Le => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            ObservedReaderEncoding::Utf16Be => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        }
    }

    #[test]
    fn one_byte_reads_retain_the_original_context_across_bom_crlf_and_surrogate_splits() {
        for encoding in [ObservedReaderEncoding::Utf8, ObservedReaderEncoding::Utf16Le, ObservedReaderEncoding::Utf16Be] {
            let prefix = format!("\u{feff}first\r\n{}", "x".repeat(127));
            let bytes = encode(&format!("{prefix}🦀target\r\nlast"), encoding);
            let expected = encode(&format!("{prefix}🦀"), encoding).len() as u64;
            for chunk in 1..=7 { for max_bytes in [4, 5, 7, 8, 64] {
                let mut reader = source(&bytes, chunk);
                reader.erase_delivered = true;
                let options = PositionRequest { line: 2, column: 129, encoding, max_bytes, ..request() };
                let (result, io) = run(&mut reader, options, &mut || false);
                let scan = result.unwrap();
                assert_eq!(scan.original_offset, expected);
                assert_eq!(scan.line_start, encode("\u{feff}first\r\n", encoding).len() as u64);
                let begin = scan.capture_start as usize;
                assert_eq!(scan.selected, bytes[begin..begin + scan.selected.len()]);
                assert!(scan.selected.len() <= max_bytes + 8);
                assert!(scan.scanned_input_bytes <= io.bytes_read);
                assert!(io.bytes_read <= options.max_scan_bytes);
                assert!(scan.visible.start().get() <= expected && expected <= scan.visible.end().get());
            } }
        }
    }

    #[test]
    fn exact_scan_limit_is_not_real_eof_even_after_resolving_a_content_end_caret() {
        let options = PositionRequest { column: 4, max_bytes: 4, max_scan_bytes: 3, ..request() };
        let (result, spent) = run(&mut source(b"abc", 16), options, &mut || false);
        assert!(matches!(result, Err(ObservedReaderError::PositionScanByteLimit)));
        assert_eq!(spent.bytes_read, 3);
        assert_eq!(spent.read_calls, 1);
        let (result, spent) = run(&mut source(b"abc", 16),
            PositionRequest { max_scan_bytes: 4, ..options }, &mut || false);
        let scan = result.unwrap();
        assert_eq!(scan.original_offset, 3);
        assert_eq!(scan.visible, range(3, 3).unwrap());
        assert_eq!(scan.selected, b"abc");
        assert_eq!(spent.bytes_read, 3);
        assert_eq!(spent.read_calls, 2);
        let (result, _) = run(&mut source(b"abc", 16),
            PositionRequest { column: 5, max_scan_bytes: 4, ..options }, &mut || false);
        assert!(matches!(result, Err(ObservedReaderError::PositionColumnMissing)));
    }

    #[test]
    fn incomplete_bom_is_pending_until_a_real_eof_and_following_context_must_fit() {
        let options = PositionRequest { column: 3, max_bytes: 4, max_scan_bytes: 2, ..request() };
        let (result, spent) = run(&mut source(&[0xef, 0xbb], 1), options, &mut || false);
        assert!(matches!(result, Err(ObservedReaderError::PositionScanByteLimit)));
        assert_eq!(spent.bytes_read, 2);
        let (result, _) = run(&mut source(&[0xef, 0xbb], 1),
            PositionRequest { max_scan_bytes: 4, ..options }, &mut || false);
        assert_eq!(result.unwrap().original_offset, 2);
        let (result, spent) = run(&mut source(b"abcdefghi", 1),
            PositionRequest { max_scan_bytes: 4, ..request() }, &mut || false);
        assert!(matches!(result, Err(ObservedReaderError::PositionScanByteLimit)));
        assert_eq!(spent.bytes_read, 4);
    }

    #[test]
    fn interruptions_io_errors_and_cancellation_keep_all_spent_read_counters() {
        let mut interrupted = source(b"anything", 1);
        interrupted.interrupted = 10;
        let (result, spent) = run(&mut interrupted,
            PositionRequest { max_read_calls: 3, ..request() }, &mut || false);
        assert!(matches!(result, Err(ObservedReaderError::PositionReadCallLimit)));
        assert_eq!(spent.read_calls, 3);
        assert_eq!(spent.interrupted_calls, 3);
        assert_eq!(spent.bytes_read, 0);
        let mut failing = source(b"abcdef", 3);
        failing.fail_at = Some(2);
        let (result, spent) = run(&mut failing, request(), &mut || false);
        assert!(matches!(result, Err(ObservedReaderError::App(AppError::Io))));
        assert_eq!(spent.read_calls, 2);
        assert_eq!(spent.bytes_read, 3);
        let mut polls = 0;
        let (result, spent) = run(&mut source(b"abcdef", 3), request(), &mut || {
            polls += 1; polls == 2
        });
        assert!(result.err().unwrap().is_canceled());
        assert_eq!(spent.read_calls, 1);
        assert_eq!(spent.bytes_read, 3);
    }
}
