#![forbid(unsafe_code)]

//! Optional document layouts for the real stdio reading desk. One layout per
//! live pane, independently reflowable, with a session-wide non-reusing attempt
//! counter. Closing/retargeting/restoring a pane retires its derived layout on
//! this worker. Checkpoints retain authoritative sources, not these caches.

use super::{Failure, DeskSession, HostResponse, Output, number, count, pane};
use crate::output::OutputError;
use fcb::ui::reading_panes::desk::{DeskPaneId, MAX_DESK_PANES};
use crate::host::desk::document::{DeskDocument, DeskDocumentOptions, DocumentCopyMode};
pub(super) use crate::host::desk::document::DeskDocumentError;

pub(super) const DOCUMENT_HELP: &str = "\nMarkdown on retained desk panes (no source path reopening):\n\
  doc-prepare PANE DOCUMENT_GEN WIDTH_COLUMNS\n\
  doc-window PANE DOCUMENT_GEN FIRST_FLOW_LINE COUNT\n\
  doc-headings PANE DOCUMENT_GEN FIRST_HEADING COUNT\n\
  doc-heading PANE DOCUMENT_GEN CANONICAL_SLUG\n\
  doc-sync PANE DOCUMENT_GEN COUNT | doc-split PANE DOCUMENT_GEN COUNT\n\
  doc-select PANE DOCUMENT_GEN RENDERED_UTF8_START RENDERED_UTF8_END\n\
  doc-copy PANE DOCUMENT_GEN START END rendered|markdown\n\
  doc-clear PANE DOCUMENT_GEN\n\
Prepare/reflow uses a new increasing document generation, separate from queries.\n\
The source must be complete UTF-8 (BOM supported), at most 64 KiB on this CLI\n\
route. Unsupported bytes remain readable/copyable with ordinary desk commands.\n\
Width is 4..512 logical columns. Pages are zero-based and contain 1..128 rows.\n\
Preview coordinates are rendered UTF-8 bytes, not source bytes or native glyphs.\n\
Heading/select navigate the real source and change the desk revision; other\n\
document commands do not. Sync/split use the current original-byte anchor.\n\
Mapping is to enclosing Markdown regions, not glyph-exact or percentage scroll.\n\
Unmapped syntax/BOM anchors are refused; navigate a heading or mapped source first.\n\
Copy rendered text and enclosing original Markdown are different explicit domains.\n\
No clipboard writes, asset loads, link execution, native shaping or presentation.\n\
Reflow preserves source selection; use doc-sync to restore the preview by span.\n\
Every response lists accepted document generations. Failed reflow keeps the old\n\
layout; stale rendered coordinates are rejected. Close/replace/restore retires\n\
obsolete layouts; doc-prepare explicitly rebuilds from retained source bytes.\n";

pub(super) struct DocumentCommands {
    documents: [Option<DeskDocument>; MAX_DESK_PANES],
    last_attempt: u64,
}
impl DocumentCommands {
    pub(super) fn new() -> Self {
        Self { documents: std::array::from_fn(|_| None), last_attempt: 0 }
    }
    pub(super) fn retain_current(&mut self, desk: &DeskSession) {
        for slot in &mut self.documents {
            if slot.as_ref().is_some_and(|document| document.validate_source(desk, desk.model().revision()).is_err()) {
                *slot = None;
            }
        }
    }
    pub(super) fn encode_state(&self, out: &mut Output) -> Result<(), OutputError> {
        out.literal(",\"last_document_attempt\":")?; out.integer(self.last_attempt)?;
        out.literal(",\"documents\":[")?;
        for (i, document) in self.documents.iter().flatten().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"pane\":")?; out.integer(document.pane().get())?;
            out.literal(",\"document_generation\":")?; out.integer(document.generation())?;
            out.literal(",\"file_id\":")?; out.integer(document.source_file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(document.source_revision().get())?;
            out.literal("}")?;
        }
        out.literal("]")
    }
    fn slot(&self, pane: DeskPaneId) -> Result<usize, Failure> {
        self.documents.iter().position(|d| d.as_ref().is_some_and(|d| d.pane() == pane))
            .ok_or(Failure::Protocol("DESK_NO_DOCUMENT"))
    }
    fn get(&self, pane: DeskPaneId) -> Result<&DeskDocument, Failure> {
        self.documents[self.slot(pane)?].as_ref().ok_or(Failure::Protocol("DESK_NO_DOCUMENT"))
    }
    pub(super) fn execute(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        command: &str, args: &[&str], canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        match (command, args) {
            ("doc-prepare", [p, generation, width]) => {
                let pane = pane(desk, p)?; let generation = number(generation)?;
                let width_columns = u32::try_from(number(width)?).map_err(|_| Failure::Protocol("DESK_DOCUMENT_WIDTH"))?;
                if generation == 0 || generation <= self.last_attempt { return Err(Failure::Protocol("DOCUMENT_READ_STALE_GENERATION")); }
                self.last_attempt = generation;
                let options = DeskDocumentOptions { width_columns, ..Default::default() };
                if let Ok(slot) = self.slot(pane) {
                    let document = self.documents[slot].as_mut().ok_or(Failure::Protocol("DESK_NO_DOCUMENT"))?;
                    return Ok(document.reflow(desk, expected, generation, options, &mut *canceled)?);
                }
                let slot = self.documents.iter().position(Option::is_none).ok_or(Failure::Protocol("DESK_DOCUMENT_LIMIT"))?;
                let candidate = DeskDocument::prepare(desk, expected, pane, generation, options, &mut *canceled)?;
                let response = candidate.overview(desk, expected, generation, &mut *canceled)?;
                // Private layout AND complete response precede publication.
                if canceled() { return Err(Failure::Canceled); }
                self.documents[slot] = Some(candidate);
                Ok(response)
            }
            ("doc-window" | "doc-headings", [p, generation, first, count_text]) => {
                let document = self.get(pane(desk, p)?)?;
                if command == "doc-window" {
                    Ok(document.window(desk, expected, number(generation)?, count(first)?, count(count_text)?, &mut *canceled)?)
                } else {
                    Ok(document.headings(desk, expected, number(generation)?, count(first)?, count(count_text)?, &mut *canceled)?)
                }
            }
            ("doc-sync" | "doc-split", [p, generation, count_text]) => {
                let pane = pane(desk, p)?;
                let offset = desk.model().location(pane, expected)?.offset;
                let document = self.get(pane)?;
                if command == "doc-sync" {
                    Ok(document.at_source(desk, expected, number(generation)?, offset, count(count_text)?, &mut *canceled)?)
                } else {
                    Ok(document.split(desk, expected, number(generation)?, offset, count(count_text)?, &mut *canceled)?)
                }
            }
            ("doc-heading", [p, generation, slug]) => {
                self.get(pane(desk, p)?)?.seek_heading(desk, expected, attempt, number(generation)?, slug, &mut *canceled)?;
                // Navigation already committed. Later delivery is not rollback.
                Ok(desk.state(|| false)?)
            }
            ("doc-select", [p, generation, start, end]) => {
                self.get(pane(desk, p)?)?.select_source(desk, expected, attempt,
                    number(generation)?, count(start)?, count(end)?, &mut *canceled)?;
                Ok(desk.state(|| false)?)
            }
            ("doc-copy", [p, generation, start, end, mode]) => {
                let mode = match *mode { "rendered" => DocumentCopyMode::RenderedText,
                    "markdown" => DocumentCopyMode::EnclosingMarkdown,
                    _ => return Err(Failure::Protocol("DESK_DOCUMENT_COPY_MODE")) };
                Ok(self.get(pane(desk, p)?)?.copy(desk, expected, number(generation)?, count(start)?, count(end)?, mode, &mut *canceled)?)
            }
            ("doc-clear", [p, generation]) => {
                let slot = self.slot(pane(desk, p)?)?;
                self.documents[slot].as_ref().ok_or(Failure::Protocol("DESK_NO_DOCUMENT"))?
                    .layout(desk, expected, number(generation)?)?;
                let response = desk.state(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.documents[slot] = None;
                Ok(response)
            }
            _ => Err(Failure::Protocol("DESK_DOCUMENT_COMMAND_SYNTAX")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb::{ArenaOwnerId, FileId, SourceRevision, SourceCapture};
    use crate::host::desk::DeskLimits;
    fn setup() -> (DeskSession, DocumentCommands) {
        let owner = ArenaOwnerId::new(90001).unwrap();
        let mut desk = DeskSession::new(owner, DeskLimits::default()).unwrap();
        let source = SourceCapture::from_bytes(owner, FileId::new(owner, 1).unwrap(),
            SourceRevision::new(owner, 1).unwrap(), "README.md", b"# Title\n\nBody text.\n".to_vec()).unwrap();
        desk.adopt(0, 1, source, 0, None, || false).unwrap();
        (desk, DocumentCommands::new())
    }
    #[test]
    fn final_prepare_cancellation_never_installs_an_unpublished_layout() {
        let (mut probe, mut commands) = setup(); let mut calls = 0;
        commands.execute(&mut probe, 1, 2, "doc-prepare", &["1", "1", "80"], &mut || { calls += 1; false }).unwrap();
        let (mut desk, mut commands) = setup(); let mut seen = 0;
        let result = commands.execute(&mut desk, 1, 2, "doc-prepare", &["1", "1", "80"],
            &mut || { seen += 1; seen == calls });
        assert!(matches!(result, Err(e) if e.canceled()));
        assert!(commands.documents.iter().all(Option::is_none));
        assert_eq!(commands.last_attempt, 1); assert_eq!(desk.model().revision(), 1);
        assert!(commands.execute(&mut desk, 1, 3, "doc-prepare", &["1", "1", "80"], &mut || false).is_err());
        commands.execute(&mut desk, 1, 4, "doc-prepare", &["1", "2", "80"], &mut || false).unwrap();
    }
    #[test]
    fn canceled_reflow_and_clear_preserve_the_previous_layout() {
        let (mut desk, mut commands) = setup();
        commands.execute(&mut desk, 1, 2, "doc-prepare", &["1", "1", "80"], &mut || false).unwrap();
        assert!(matches!(commands.execute(&mut desk, 1, 3, "doc-prepare", &["1", "2", "8"], &mut || true), Err(e) if e.canceled()));
        assert_eq!(commands.get(desk.model().active().unwrap()).unwrap().generation(), 1);
        assert_eq!(commands.last_attempt, 2);
        assert!(commands.execute(&mut desk, 1, 4, "doc-clear", &["1", "1"], &mut || true).is_err());
        commands.execute(&mut desk, 1, 5, "doc-window", &["1", "1", "0", "8"], &mut || false).unwrap();
        assert_eq!(desk.model().revision(), 1);
    }
}
