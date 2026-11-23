#![forbid(unsafe_code)]

//! CLI composition of the native observed-reader's bounded position worker.
//! The scanner retains context in the same pass that locates the point. The
//! decoder then verifies its exact original-byte to displayed-text mapping.

use super::{Request, MAX_READ_CALLS, same_metadata};
use crate::{AppError, SCHEMA, EXIT_OK, EXIT_NO_MATCH, EXIT_PARTIAL,
    allocation, file_id, generation, owner, revision, input};
use crate::output::Output;
use crate::host::observed_reader::{ObservedReaderError,
    position::{PositionRequest, scan_position}};
use fcb::{BrowserSession, ByteLength, ByteOffset, ByteRange};
use fcb::search::{CaptureRequest, ExtentReadStats, ObservedExtent, ResourceBudget};
use fcb::source::{DetectedEncoding, position::MAX_POSITION_STEP_BYTES};

pub(super) fn execute(request: &Request, out: &mut Output, budget: &ResourceBudget,
    canceled: &mut impl FnMut() -> bool) -> Result<u8, AppError> {
    let column = request.column.ok_or(AppError::InvalidRange)?;
    let _paths = budget.try_reserve_managed(owner(), allocation(60), ByteLength::new(128 * 1024))
        .map_err(|_| AppError::Admission)?;
    let path = input::absolute(request.path.as_deref().ok_or(AppError::InvalidRange)?)?;
    let (mut file, before) = input::open_regular(&path)?;
    let options = PositionRequest { line: request.first.get(), column, kind: request.column_kind,
        max_bytes: request.max_bytes, max_scan_bytes: request.max_scan,
        max_read_calls: MAX_READ_CALLS, encoding: request.encoding };
    let mut io = ExtentReadStats::default();
    let result = scan_position(&mut file, options, budget, owner(),
        [allocation(61), allocation(62)], &mut io, canceled);
    let (scan, state, exit) = match result {
        Ok(scan) => (Some(scan), "resolved", EXIT_OK),
        Err(ObservedReaderError::LineMissing) => (None, "missing-line", EXIT_NO_MATCH),
        Err(ObservedReaderError::PositionColumnMissing) => (None, "missing-column", EXIT_NO_MATCH),
        Err(ObservedReaderError::PositionScanByteLimit) => (None, "scan-byte-limit", EXIT_PARTIAL),
        Err(ObservedReaderError::PositionReadCallLimit) => (None, "read-call-limit", EXIT_PARTIAL),
        Err(error) if error.is_canceled() => return Err(AppError::Canceled),
        Err(ObservedReaderError::App(error)) => return Err(error),
        Err(_) => return Err(AppError::InvalidRange),
    };
    let after = file.metadata().map_err(|_| AppError::Io)?;
    let named = std::fs::symlink_metadata(&path).map_err(|_| AppError::SourceChanged)?;
    if !same_metadata(&before, &after) || !same_metadata(&after, &named) {
        return Err(AppError::SourceChanged);
    }
    if canceled() { return Err(AppError::Canceled); }
    if request.json {
        out.literal("{\"schema\":")?; out.quoted(SCHEMA)?;
        out.literal(",\"status\":\"ok\",\"command\":\"read-position\",\"scope\":\"named-file\",\"identity_scope\":\"response-local\",\"file_id\":\"1\",\"revision\":\"1\",\"path\":")?;
        out.path(&path)?;
        out.literal(",\"consistency\":\"unchanged-metadata-not-atomic\",\"capture_kind\":\"observed-extent\",\"whole_file_immutable\":false,\"state\":")?;
        out.quoted(state)?;
        out.literal(",\"position_complete\":")?; out.boolean(exit != EXIT_PARTIAL)?;
        out.literal(",\"column_kind\":")?; out.quoted(request.column_kind.code())?;
        for (key, value) in [("requested_line", request.first.get()), ("requested_column", column),
            ("observed_length", before.len()), ("payload_bytes_read", io.bytes_read),
            ("read_calls", io.read_calls), ("max_scan_bytes", request.max_scan),
            ("max_selected_bytes", request.max_bytes as u64), ("max_read_calls", MAX_READ_CALLS),
            ("input_buffer_bytes", MAX_POSITION_STEP_BYTES as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
    } else {
        out.literal("Position navigation: ")?; out.literal(state)?; out.literal("\n")?;
    }
    if let Some(scan) = scan {
        let capture_end = scan.capture_start.checked_add(scan.selected.len() as u64)
            .ok_or(AppError::InvalidRange)?;
        let capture_range = range(scan.capture_start, capture_end)?;
        let capture = CaptureRequest::new(file_id(), revision()).map_err(|_| AppError::InvalidRange)?
            .with_range(capture_range).map_err(|_| AppError::InvalidRange)?;
        let extent = ObservedExtent::from_bytes(capture, ByteLength::new(before.len()),
            &scan.selected, budget, allocation(63))?;
        let view = BrowserSession::new(owner()).open_extent(extent)?;
        let encoding = match scan.encoding {
            DetectedEncoding::Utf8 { has_bom } => DetectedEncoding::Utf8 {
                has_bom: has_bom && scan.capture_start == 0 },
            encoding => encoding,
        };
        let mut text = view.decode(scan.visible, encoding, generation(), budget,
            allocation(64), &mut *canceled)?;
        // Decoder boundary adjustment may floor a mid-scalar start. Keep the
        // stated visible-byte cap using the same decoder's end clipping rules.
        if text.range().len().get() > request.max_bytes as u64 {
            let end = text.range().start().get().checked_add(request.max_bytes as u64)
                .ok_or(AppError::InvalidRange)?;
            drop(text);
            text = view.decode(range(scan.visible.start().get(), end)?, encoding,
                generation(), budget, allocation(65), &mut *canceled)?;
        }
        let point = range(scan.original_offset, scan.original_offset)?;
        let decoded = text.source_to_text(point)?;
        let verified = text.text_selection(decoded)?;
        if verified.original_range != point || !verified.original_bytes.is_empty() || !decoded.is_empty() {
            return Err(AppError::InvalidRange);
        }
        if request.json {
            for (key, value) in [("original_offset", scan.original_offset),
                ("window_utf8_offset", decoded.start().get()), ("line_start", scan.line_start),
                ("scanned_input_bytes", scan.scanned_input_bytes)] {
                out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
            }
            out.literal(",\"scan_reached_eof\":")?; out.boolean(scan.reached_eof)?;
            out.literal(",\"captured_range\":")?; out.range(capture_range)?;
            out.literal(",\"requested_visible_range\":")?; out.range(scan.visible)?;
            out.literal(",\"visible_range\":")?; out.range(text.range())?;
            out.literal(",\"boundaries_adjusted\":")?; out.boolean(text.range() != scan.visible)?;
            out.literal(",\"encoding\":")?;
            out.quoted(match encoding { DetectedEncoding::Utf16Le => "utf16le",
                DetectedEncoding::Utf16Be => "utf16be", _ => "utf8" })?;
            out.literal(",\"text_kind\":\"logical-observed-text-not-shaped\",\"text\":")?; out.quoted(text.text())?;
            out.literal(",\"original_hex\":")?; out.hex(view.raw_selection(text.range())?)?;
            out.literal(",\"has_replacements\":")?; out.boolean(text.has_replacements())?;
        } else {
            out.literal("[line ")?; out.literal(&request.first.get().to_string())?;
            out.literal(", ")?; out.literal(request.column_kind.code())?; out.literal(" column ")?;
            out.literal(&column.to_string())?; out.literal("; original byte ")?;
            out.literal(&scan.original_offset.to_string())?; out.literal("; escaped controls]\n")?;
            out.human_text(text.text())?;
            if !text.text().ends_with('\n') { out.literal("\n")?; }
            if text.has_replacements() { out.literal("[malformed text replaced; original bytes retained]\n")?; }
        }
    } else if request.json {
        out.literal(",\"original_offset\":null,\"window_utf8_offset\":null,\"line_start\":null,\"scanned_input_bytes\":null,\"scan_reached_eof\":null,\"captured_range\":null,\"requested_visible_range\":null,\"visible_range\":null,\"text\":null,\"original_hex\":null")?;
    } else if exit == EXIT_PARTIAL {
        out.literal("PARTIAL: the scan or read-call allowance ended before exact context was available. No missing-position claim was made.\n")?;
    } else {
        out.literal("The requested physical line or logical column does not exist in the observed source.\n")?;
    }
    if request.json { out.literal("}\n")?; }
    Ok(exit)
}

fn range(start: u64, end: u64) -> Result<ByteRange, AppError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| AppError::InvalidRange)
}
