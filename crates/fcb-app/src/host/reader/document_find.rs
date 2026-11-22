#![forbid(unsafe_code)]

//! Literal navigation in the retained upstream logical Markdown text. This is
//! a host adapter over the shared incremental matcher, not another Markdown
//! parser, layout engine, or original-source capture.

use fcb::search::stream::{ByteSearchCursor, StreamError};
use fcb::document::reader::DocumentReadError;
use super::{check, range, reserve, ByteLength, HostResponse, ReaderSession,
    ReaderSessionError, EXIT_OK};
use crate::AppError;

pub const MAX_READER_DOCUMENT_FIND_NEEDLE_BYTES: usize = 1024;
pub const MAX_READER_DOCUMENT_FIND_PAGE: usize = 128;
pub const MAX_READER_DOCUMENT_FIND_BYTES: usize = 16 * 1024 * 1024;
const SCAN_STEP_BYTES: usize = 64 * 1024;
const SCAN_BATCH_HITS: usize = 256;
// Covers the 1024-entry KMP table, one 256-hit batch, at most 128 retained
// result records, vector/cursor headers and allocation rounding. The immutable
// rendered text remains owned and charged by the accepted document layout.
const FIND_WORKSPACE_BYTES: u64 = 64 * 1024;

struct FindHit { id: u64, start: u64, end: u64, first_line: usize }

impl ReaderSession {
    /// A stateless, complete scan of the accepted logical rendered text. Pages
    /// use zero-based occurrence ordinals, not source bytes or flow-row numbers.
    /// Replaying a page reads no source and retains no document-sized hit table.
    ///
    /// Every reply echoes the exact needle and document generation. A native
    /// caller must additionally fence its current query/job before publishing
    /// the page. Reflow rejects old document generations; failed/canceled Find
    /// changes neither the layout nor any source-search/outline state.
    ///
    /// Returned UTF-8 ranges belong ONLY to rendered text. Opening original
    /// Markdown still requires document_selection_source's provenance audit.
    /// The row is containing/next context (last for trailing separators, or
    /// empty window zero when no rows exist), never glyph/source coordinates.
    pub fn find_document(&mut self, generation: u64, needle: &str, first: usize,
        count: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        if needle.is_empty() || needle.len() > MAX_READER_DOCUMENT_FIND_NEEDLE_BYTES
            || !(1..=MAX_READER_DOCUMENT_FIND_PAGE).contains(&count) {
            return Err(ReaderSessionError::InvalidLimits);
        }
        check(&mut canceled)?;
        let length = self.document_snapshot(generation)?.rendered_text().len();
        if length > MAX_READER_DOCUMENT_FIND_BYTES { return Err(ReaderSessionError::InvalidLimits); }
        // At most one overlapping occurrence can start at each rendered byte.
        if first > length { return Err(ReaderSessionError::InvalidRange); }
        let [allocation] = self.allocations()?;
        let _workspace = self.budget.try_reserve_managed(self.owner(), allocation,
            ByteLength::new(FIND_WORKSPACE_BYTES)).map_err(|_| AppError::Admission)?;
        let mut hits = reserve::<FindHit>(count)?;
        let document = self.document_snapshot(generation)?;
        let text = document.rendered_text();
        let mut cursor = ByteSearchCursor::new(needle.as_bytes()).map_err(scan_error)?;
        let mut scanned = 0;
        while scanned < text.len() {
            check(&mut canceled)?;
            let batch = cursor.step(&text.as_bytes()[scanned..], SCAN_STEP_BYTES,
                SCAN_BATCH_HITS).map_err(scan_error)?;
            if batch.consumed == 0 || batch.consumed > text.len() - scanned {
                return Err(DocumentReadError::InvalidOutput.into());
            }
            scanned += batch.consumed;
            for hit in batch.hits {
                if hit.occurrence_id <= first as u64 || hits.len() == count { continue; }
                let start = usize::try_from(hit.start).map_err(|_| ReaderSessionError::InvalidRange)?;
                let end = usize::try_from(hit.end).map_err(|_| ReaderSessionError::InvalidRange)?;
                // Both inputs are UTF-8. Keep the range/domain check explicit
                // before publishing a shared byte match as logical text.
                if text.get(start..end) != Some(needle) { return Err(DocumentReadError::InvalidOutput.into()); }
                let first_line = document.window_at_rendered(start, 1)?.first_index();
                hits.push(FindHit { id: hit.occurrence_id, start: hit.start, end: hit.end, first_line });
            }
        }
        check(&mut canceled)?;
        let total = cursor.matches_seen();
        if cursor.scanned_bytes() != length as u64 || total > length as u64 {
            return Err(DocumentReadError::InvalidOutput.into());
        }
        if first as u64 > total { return Err(ReaderSessionError::InvalidRange); }
        drop(cursor);
        let next = first as u64 + hits.len() as u64;
        let mut out = self.output("document-find")?;
        super::document::summary(&mut out, self.document_snapshot(generation)?)?;
        out.literal(",\"needle\":")?; out.quoted(needle)?;
        out.literal(",\"search_domain\":\"rendered-text-utf8\",\"case_sensitive\":true,\"normalization\":\"exact\",\"overlapping\":true,\"max_scan_bytes\":")?;
        out.integer(MAX_READER_DOCUMENT_FIND_BYTES as u64)?;
        out.literal(",\"scanned_rendered_bytes\":")?; out.integer(length as u64)?;
        out.literal(",\"first_match\":")?; out.integer(first as u64)?;
        out.literal(",\"matches_seen\":")?; out.integer(total)?;
        out.literal(",\"match_count_complete\":true,\"next_match\":")?;
        if next < total { out.integer(next)?; } else { out.literal("null")?; }
        out.literal(",\"matches\":[")?;
        for (index, hit) in hits.iter().enumerate() {
            check(&mut canceled)?;
            if index != 0 { out.literal(",")?; }
            out.literal("{\"match_id\":")?; out.integer(hit.id)?;
            out.literal(",\"rendered_utf8_range\":")?; out.range(range(hit.start, hit.end)?)?;
            out.literal(",\"first_flow_line\":")?; out.integer(hit.first_line as u64)?;
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }
}

fn scan_error(error: StreamError) -> ReaderSessionError {
    match error {
        StreamError::AllocationFailed => AppError::Admission.into(),
        StreamError::OffsetOverflow => ReaderSessionError::InvalidRange,
        StreamError::EmptyNeedle | StreamError::NeedleTooLong => ReaderSessionError::InvalidLimits,
    }
}
