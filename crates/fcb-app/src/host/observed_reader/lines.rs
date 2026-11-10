#![forbid(unsafe_code)]

//! Exact line navigation is one bounded observation of the already opened
//! file. Reuse the CLI's forward scanner and retain its selected bytes; a
//! second range read could mix line numbers with a different live sequence.

use std::{fs::File, io::{self, Read}};
use fcb::ByteLength;
use fcb::search::{CaptureRequest, ExtentReadStats, ExtentText, ExtentWindowRequest, ObservedExtent};
use fcb::source::{LineNumber, line_index::LineWindowStatus};
use crate::{AppError, args::Encoding, lines::{LineScanRequest, same_metadata, scan_lines}};
use crate::output::Output;
use super::{HostResponse, Observation, ObservedReaderEncoding, ObservedReaderError,
    ObservedReaderSession, MAX_OBSERVED_READER_WINDOW_BYTES, check, decode, extent_sha256, range};

pub const MAX_OBSERVED_READER_LINES: u64 = 4096;
pub const MAX_OBSERVED_READER_LINE_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_OBSERVED_READER_LINE_READ_CALLS: u64 = 1_048_576;

/// These line coordinates belong only to this accepted scan and its retained
/// bytes. Replaying the observation never consults the current file or index.
#[derive(Clone, Copy)]
pub(super) struct LineNavigation {
    first_line: u64,
    last_line: u64,
    requested_first_line: u64,
    requested_lines: u64,
    max_scan_bytes: u64,
    max_selected_bytes: usize,
    scanned_input_bytes: u64,
    payload_bytes_read: u64,
    read_calls: u64,
    lines_seen: u64,
}

impl LineNavigation {
    pub(super) fn first_physical_line(self, page: &Observation, text: &ExtentText) -> Option<u64> {
        if text.range().start() == page.plan.visible.start() { return Some(self.first_line); }
        // The shared decoder removes an actual leading BOM from visible text.
        // It remains in the requested first-line range and exact byte copy.
        if self.first_line != 1 || page.plan.visible.start().get() != 0 { return None; }
        let header = match page.encoding {
            ObservedReaderEncoding::Utf8 if page.extent.bytes().starts_with(&[0xef, 0xbb, 0xbf]) => 3,
            ObservedReaderEncoding::Utf16Le if page.extent.bytes().starts_with(&[0xff, 0xfe]) => 2,
            ObservedReaderEncoding::Utf16Be if page.extent.bytes().starts_with(&[0xfe, 0xff]) => 2,
            _ => 0,
        };
        (header != 0 && text.range().start().get() == header).then_some(1)
    }
}

impl ObservedReaderSession {
    /// Scan from byte zero to an exact one-based line window, within explicit
    /// prefix I/O and retained-selection limits. Newlines use the shared
    /// CR/LF/CRLF and UTF-16 rules. A real EOF may resolve fewer requested lines;
    /// a byte/call limit never becomes a missing-line claim or truncated page.
    ///
    /// The selected bytes come from that same scan. Metadata changes, limits,
    /// I/O errors and cancellation preserve every previously accepted page.
    /// Spent native reads remain charged even when this attempt is not accepted.
    pub fn read_lines(&mut self, first_line: u64, line_count: u64, max_bytes: usize,
        max_scan_bytes: u64, encoding: ObservedReaderEncoding, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        let revision = self.next_revision()?;
        check(&mut canceled)?;
        if first_line == 0 || !(1..=MAX_OBSERVED_READER_LINES).contains(&line_count)
            || first_line.checked_add(line_count - 1).is_none()
            || !(4..=MAX_OBSERVED_READER_WINDOW_BYTES).contains(&max_bytes)
            || !(1..=MAX_OBSERVED_READER_LINE_SCAN_BYTES).contains(&max_scan_bytes) {
            return Err(ObservedReaderError::InvalidLimits);
        }
        // Refuse before source I/O if cumulative accounting would overflow.
        self.source_bytes_read.checked_add(max_scan_bytes).ok_or(ObservedReaderError::IdentityExhausted)?;
        self.read_calls.checked_add(MAX_OBSERVED_READER_LINE_READ_CALLS)
            .ok_or(ObservedReaderError::IdentityExhausted)?;
        let [selected_id, input_id, extent_id] = self.allocations()?;
        let before = self.search_file.metadata().map_err(|_| AppError::Io)?;
        let request = LineScanRequest {
            first: LineNumber::new(first_line).map_err(|_| ObservedReaderError::InvalidLimits)?,
            count: line_count, max_bytes, max_scan: max_scan_bytes,
            max_read_calls: MAX_OBSERVED_READER_LINE_READ_CALLS,
            encoding: match encoding {
                ObservedReaderEncoding::Utf8 => Encoding::Utf8,
                ObservedReaderEncoding::Utf16Le => Encoding::Utf16Le,
                ObservedReaderEncoding::Utf16Be => Encoding::Utf16Be,
            },
        };
        let mut source = PositionedReader { file: &self.search_file, offset: 0 };
        let mut io = ExtentReadStats::default();
        let scanned = scan_lines(&mut source, request, &self.budget, self.owner(),
            [selected_id, input_id], &mut io, &mut canceled);
        self.source_bytes_read += io.bytes_read;
        self.read_calls += io.read_calls;
        let scanned = scanned?;
        check(&mut canceled)?;
        let after = self.search_file.metadata().map_err(|_| AppError::Io)?;
        // Check the same opened object, never the pathname: a later rename or
        // replacement cannot make this session silently switch source files.
        if !same_metadata(&before, &after) { return Err(AppError::SourceChanged.into()); }
        match scanned.stop {
            "resolved" => {},
            "missing" => return Err(ObservedReaderError::LineMissing),
            "scan-byte-limit" => return Err(ObservedReaderError::LineScanByteLimit),
            "output-byte-limit" => return Err(ObservedReaderError::LineOutputByteLimit),
            "read-call-limit" => return Err(ObservedReaderError::LineReadCallLimit),
            _ => return Err(ObservedReaderError::InvalidRange),
        }
        let LineWindowStatus::Resolved { first, last, range: visible } = scanned.cursor.status()
            else { return Err(ObservedReaderError::InvalidRange); };
        let start = scanned.backing_start.ok_or(ObservedReaderError::InvalidRange)?;
        let end = start.checked_add(scanned.selected.len() as u64).ok_or(ObservedReaderError::InvalidRange)?;
        let capture = range(start, end)?;
        let request = CaptureRequest::new(self.reader.file_id(), revision)?.with_range(capture)?;
        // from_bytes truthfully labels the source-owned extent HostSupplied.
        // The separate line receipt records the host's same-descriptor metadata
        // evidence, without upgrading that evidence to an atomic snapshot.
        let extent = ObservedExtent::from_bytes(request, ByteLength::new(before.len()),
            &scanned.selected, &self.budget, extent_id)?;
        let receipt = LineNavigation { first_line: first.get(), last_line: last.get(),
            requested_first_line: first_line, requested_lines: line_count,
            max_scan_bytes, max_selected_bytes: max_bytes,
            scanned_input_bytes: scanned.cursor.bytes_scanned(), payload_bytes_read: io.bytes_read,
            read_calls: io.read_calls, lines_seen: scanned.cursor.lines_seen() };
        // Drop the temporary retained scan buffer before allocating decode maps.
        drop(scanned);
        let sha256 = extent_sha256(extent.bytes(), &mut canceled)?;
        let candidate = Observation { extent, plan: ExtentWindowRequest { visible, capture },
            requested_bytes: max_bytes, encoding, sha256, io, find_hit: None, line_navigation: Some(receipt) };
        let [decode_id, clipped_decode_id] = self.allocations()?;
        let text = decode(&candidate, &self.budget, [decode_id, clipped_decode_id], &mut canceled)?;
        // Line windows are already scalar/CRLF aligned. Only the shared
        // decoder's first-line BOM stripping may move their visible start.
        if text.range().end() != visible.end() || receipt.first_physical_line(&candidate, &text).is_none() {
            return Err(ObservedReaderError::InvalidRange);
        }
        let response = self.page_response_decoded("lines", &candidate, &text, true, &mut canceled)?;
        self.accept_observation(candidate);
        Ok(response)
    }
}

/// File::try_clone shares a seek cursor on Unix. Positioned reads let every
/// line request start at zero without disturbing window/search operations.
struct PositionedReader<'a> { file: &'a File, offset: u64 }
impl Read for PositionedReader<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        #[cfg(unix)] {
            use std::os::unix::fs::FileExt;
            let count = self.file.read_at(output, self.offset)?;
            self.offset = self.offset.checked_add(count as u64)
                .ok_or_else(|| io::Error::from(io::ErrorKind::InvalidData))?;
            Ok(count)
        }
        #[cfg(not(unix))] { let _ = (self.file, self.offset, output); Err(io::Error::from(io::ErrorKind::Unsupported)) }
    }
}

pub(super) fn encode_line_navigation(out: &mut Output, receipt: Option<LineNavigation>)
    -> Result<(), ObservedReaderError> {
    let Some(receipt) = receipt else { return Ok(()); };
    out.literal(",\"line_navigation\":{\"metadata_consistency\":\"unchanged-metadata-not-atomic\"")?;
    for (key, value) in [("first_line", receipt.first_line), ("last_line", receipt.last_line),
        ("requested_first_line", receipt.requested_first_line), ("requested_lines", receipt.requested_lines),
        ("max_scan_bytes", receipt.max_scan_bytes), ("max_selected_bytes", receipt.max_selected_bytes as u64),
        ("scanned_input_bytes", receipt.scanned_input_bytes), ("payload_bytes_read", receipt.payload_bytes_read),
        ("read_calls", receipt.read_calls), ("lines_seen", receipt.lines_seen)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal("}")?;
    Ok(())
}
