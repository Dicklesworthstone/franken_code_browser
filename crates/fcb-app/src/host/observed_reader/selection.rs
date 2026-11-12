#![forbid(unsafe_code)]

//! Exact native selections within one retained observed extent. The shared
//! decoder maps page-local UTF-8 boundaries back to original source bytes.
//! Resolving a selection performs no source I/O and creates no observation.

use fcb_core::{DecodedUtf8Offset, DecodedUtf8Range};
use super::{HostResponse, ObservedReaderError, ObservedReaderSession,
    MAX_OBSERVED_READER_WINDOW_BYTES, check, decode, encode_observation};
use crate::EXIT_OK;

impl ObservedReaderSession {
    /// Resolve a nonempty logical-text selection in an accepted observation.
    /// Offsets count decoded UTF-8 bytes local to that observation's displayed
    /// window, not original bytes, UTF-16 units or visual positions. The host
    /// qualifies native grapheme boundaries and binds the response to the page
    /// the user actually saw.
    ///
    /// Only the observation's immutable bytes and declared decoder are used.
    /// Live edits, path replacements and newer observations cannot change this
    /// selection. An evicted observation is unavailable rather than reread.
    /// BOM exclusion, CRLF and malformed-input replacement mapping belong to
    /// the existing shared decoder. The result never identifies a whole-file
    /// capture, including when the requested window happened to cover the file.
    pub fn resolve_selection(
        &mut self,
        observation: u64,
        utf8_start: u64,
        utf8_end: u64,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<HostResponse, ObservedReaderError> {
        check(&mut canceled)?;
        if utf8_start >= utf8_end { return Err(ObservedReaderError::InvalidRange); }
        let selected_text = DecodedUtf8Range::new(
            DecodedUtf8Offset::new(utf8_start), DecodedUtf8Offset::new(utf8_end),
        ).map_err(|_| ObservedReaderError::InvalidRange)?;
        let page = self.observation(observation)?.clone();
        let ids = self.allocations()?;
        let text = decode(&page, &self.budget, ids, &mut canceled)?;
        let selected = text.text_selection(selected_text)?;
        // The forward and reverse maps must name exactly the selected source
        // units. Neither a decoder-context prefix nor a partial scalar becomes
        // permission to copy an enclosing or unrelated original-byte range.
        if selected.original_range.is_empty()
            || text.source_to_text(selected.original_range)? != selected_text
            || selected.original_bytes != page.extent.range_bytes(selected.original_range)?
        {
            return Err(ObservedReaderError::InvalidRange);
        }
        if selected.original_range.len().get() > MAX_OBSERVED_READER_WINDOW_BYTES as u64 {
            return Err(ObservedReaderError::InvalidLimits);
        }
        check(&mut canceled)?;
        let mut out = self.output("resolve-selection")?;
        encode_observation(&mut out, &page, false)?;
        out.literal(",\"selection_namespace\":\"native-text\",\"window_original_range\":")?;
        out.range(text.range())?;
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
