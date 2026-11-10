#![forbid(unsafe_code)]

//! Resolve one native text selection through the same captured-source map as
//! its reader page. The host supplies decoded UTF-8 boundaries, not guessed
//! original-byte positions. No file, query, outline or document state changes.

use fcb_core::{DecodedUtf8Offset, DecodedUtf8Range};
use super::{AppError, BrowserSession, ByteLength, HostResponse, ObservedExtent,
    QueryGeneration, ReaderSession, ReaderSessionError, MAX_READER_WINDOW_BYTES, check, range};
use crate::EXIT_OK;

/// Reader page boundaries can move outward by a few scalar/CRLF context bytes.
/// The actual decoder request remains at most MAX_READER_WINDOW_BYTES, and its
/// exact resulting range must match the caller's accepted visible page.
pub const MAX_READER_SELECTION_WINDOW_BYTES: usize = MAX_READER_WINDOW_BYTES + 16;

impl ReaderSession {
    /// Resolve a nonempty decoded-text range within an accepted original-byte
    /// page. Both ranges are half-open; decoded positions count UTF-8 bytes,
    /// not UTF-16 units, scalars, graphemes or visual glyphs. The native host
    /// remains responsible for native grapheme-boundary hit testing and for
    /// associating the receipt with the source/page the user actually saw.
    ///
    /// The shared decoder owns BOM, malformed-sequence and CRLF semantics.
    /// Original bytes remain lossless even when selected text has replacements.
    /// This materializes at most one page and 256 KiB of selected source; larger
    /// selections fail explicitly instead of silently publishing a prefix.
    pub fn resolve_selection(
        &mut self,
        window_start: u64,
        window_end: u64,
        utf8_start: u64,
        utf8_end: u64,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        let window = range(window_start, window_end)?;
        if window.is_empty() || window_end > self.length() || utf8_start >= utf8_end {
            return Err(ReaderSessionError::InvalidRange);
        }
        if window.len().get() > MAX_READER_SELECTION_WINDOW_BYTES as u64 {
            return Err(ReaderSessionError::InvalidLimits);
        }
        let decoded = DecodedUtf8Range::new(
            DecodedUtf8Offset::new(utf8_start),
            DecodedUtf8Offset::new(utf8_end),
        ).map_err(|_| ReaderSessionError::InvalidRange)?;

        // A maximum-sized page can be slightly wider after its leading scalar
        // boundary moved backwards. Decode a bounded suffix request: only the
        // existing decoder may move that start, and only to EXACTLY window_start.
        let requested_start = window_start.max(window_end.saturating_sub(MAX_READER_WINDOW_BYTES as u64));
        let requested = range(requested_start, window_end)?;
        let captured = range(window_start.saturating_sub(8),
            window_end.saturating_add(8).min(self.length()))?;
        let [extent_id, decode_id] = self.allocations()?;
        let request = self.capture.request().with_range(captured)?;
        let extent = ObservedExtent::from_bytes(
            request, ByteLength::new(self.length()),
            &self.capture.bytes()[captured.start().get() as usize..captured.end().get() as usize],
            &self.budget, extent_id,
        ).map_err(AppError::from)?;
        let view = BrowserSession::new(self.owner()).open_extent(extent)?;
        let text = view.decode(
            requested, self.encoding,
            QueryGeneration::new(self.owner(), 1).map_err(|_| ReaderSessionError::IdentityExhausted)?,
            &self.budget, decode_id, &mut canceled,
        )?;
        if text.range() != window {
            return Err(ReaderSessionError::InvalidRange);
        }
        let selected = text.text_selection(decoded)?;
        if selected.original_range.is_empty()
            || text.source_to_text(selected.original_range)? != decoded
            || selected.original_bytes != view.raw_selection(selected.original_range)?
        {
            return Err(ReaderSessionError::InvalidRange);
        }
        if selected.original_range.len().get() > MAX_READER_WINDOW_BYTES as u64 {
            return Err(ReaderSessionError::InvalidLimits);
        }
        check(&mut canceled)?;
        let mut out = self.output("resolve-selection")?;
        out.literal(",\"selection_namespace\":\"native-text\",\"window_original_range\":")?;
        out.range(window)?;
        out.literal(",\"window_utf8_range\":{\"start\":")?; out.integer(utf8_start)?;
        out.literal(",\"end\":")?; out.integer(utf8_end)?; out.literal("}")?;
        out.literal(",\"original_range\":")?; out.range(selected.original_range)?;
        out.literal(",\"text\":")?; out.quoted(selected.text)?;
        out.literal(",\"original_hex\":")?; out.hex(selected.original_bytes)?;
        out.literal(",\"contains_replacements\":")?; out.boolean(selected.contains_replacements)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }
}
