#![forbid(unsafe_code)]

//! Read-only lexical navigation over the reader's retained complete capture.

use fcb::analysis::delimiters::{DelimiterError, matching_delimiter};
use fcb::analysis::symbols::SymbolLanguage;
use super::{HostResponse, ReaderSession, ReaderSessionError, check, range};
use crate::EXIT_OK;

impl ReaderSession {
    /// Match a selected original-source scalar without observing the live path.
    /// Comments/strings and malformed nesting never produce a destination.
    /// This bounded worker operation does not replace search, outline or source
    /// state; the native consumer validates the receipt before navigating.
    pub fn matching_delimiter(&mut self, start: u64, end: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        let language = SymbolLanguage::from_path(self.path.as_os_str().as_encoded_bytes())
            .ok_or(DelimiterError::UnsupportedLanguage)?;
        let selected = range(start, end)?;
        let [id] = self.allocations()?;
        let result = matching_delimiter(self.capture.bytes(), *self.capture.request(), language,
            Some(self.encoding), selected, &self.budget, id, &mut canceled)?;
        check(&mut canceled)?;
        let mut out = self.output("matching-delimiter")?;
        out.literal(",\"delimiter_state\":")?; out.quoted(result.state().name())?;
        out.literal(",\"selected_range\":")?; out.range(result.selected_range())?;
        out.literal(",\"matching_range\":")?;
        if let Some(matching) = result.matching_range() { out.range(matching)?; } else { out.literal("null")?; }
        out.literal(",\"language\":")?; out.quoted(result.language().name())?;
        out.literal(",\"evidence_level\":")?; out.quoted(result.evidence_level())?;
        out.literal(",\"source_context\":\"complete-capture\",\"semantic_resolution\":false}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }
}
