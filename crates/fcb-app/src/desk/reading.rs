#![forbid(unsafe_code)]

//! Persistent-desk controls for the production retained source reader. Requests
//! carry both reader and seek generations; observed step counts reject retries.
//! Pending progress is not a terminal incomplete answer. Only explicit go moves
//! the pane; closing/retargeting/restoring it retires its old derived work.

use super::{Failure, DeskSession, HostResponse, Output, number, count, pane, ReadingTarget,
    ReadingWindowOptions, LineNumber, ByteOffset, ByteRange, EXIT_OK, EXIT_PARTIAL};
use crate::output::OutputError;
use crate::host::desk::{DeskPaneId, reading::DeskReading};
pub(super) use crate::host::desk::reading::DeskReadingError;
use fcb::{search::ReaderLimits, ui::reading_panes::desk::MAX_DESK_PANES};

pub(super) const READING_HELP: &str = "\nRetained sparse indexes and resumable reading (no background executor):\n\
  reader-prepare PANE READER_GEN [CHECKPOINTS STRIDE_BYTES]\n\
  reader-info PANE READER_GEN\n\
  reader-index PANE READER_GEN EXPECTED_INDEX_STEP BYTES\n\
  reader-line PANE READER_GEN SEEK_GEN LINE\n\
  reader-byte PANE READER_GEN SEEK_GEN OFFSET\n\
  reader-range PANE READER_GEN SEEK_GEN START END\n\
  reader-step PANE READER_GEN SEEK_GEN EXPECTED_SEEK_STEP BYTES\n\
  reader-cancel PANE READER_GEN SEEK_GEN\n\
  reader-go PANE READER_GEN READY_SEEK_GEN\n\
  reader-window PANE READER_GEN READY_SEEK_GEN BYTES LINES\n\
  reader-next PANE READER_GEN READY_SEEK_GEN NEXT_OFFSET BYTES LINES\n\
  reader-clear PANE READER_GEN\n\
Reader generations increase across all panes; seek generations increase within\n\
one reader. Both index/seek step counts begin at zero. A step consumes 4..65536\n\
original bytes at most, not a fixed time. Failed/canceled progress may retain\n\
valid cache work; use returned counters, never replay a stale step blindly.\n\
Prepare does not scan. Optional indexing and seeks share retained checkpoints.\n\
Long jumps report pending without moving the pane or discarding its last ready\n\
window. reader-go commits a resolved line/byte/range into history/bookmarks.\n\
Window continuation uses the last returned NEXT_OFFSET without prefix rescans.\n\
An unfinished seek gives partial on quit/EOF; completed/canceled progress does\n\
not permanently make the session partial. Index completeness is independent.\n\
Line numbers follow editor rows (CRLF/CR/LF, final empty row); columns are not\n\
inferred. UTF-16/BOM and malformed bytes keep the existing decoder semantics.\n\
Sources are still complete admitted desk captures (not arbitrary-size live files).\n\
Legacy view/read/line remain synchronous. No native presentation is implied.\n";

pub(super) struct ReadingCommands {
    slots: [Option<DeskReading>; MAX_DESK_PANES], last_generation: u64, progress_reply: bool,
}
impl ReadingCommands {
    pub(super) fn new() -> Self { Self { slots: std::array::from_fn(|_| None), last_generation: 0, progress_reply: false } }
    pub(super) fn start_request(&mut self) { self.progress_reply = false; }
    pub(super) fn is_progress_reply(&self) -> bool { self.progress_reply }
    pub(super) fn session_exit(&self, code: u8) -> u8 {
        if code == EXIT_OK && self.slots.iter().flatten().any(DeskReading::has_pending_work) { EXIT_PARTIAL } else { code }
    }
    pub(super) fn retain_current(&mut self, desk: &DeskSession) {
        for slot in &mut self.slots {
            if slot.as_ref().is_some_and(|r| r.validate_source(desk, desk.model().revision()).is_err()) { *slot = None; }
        }
    }
    pub(super) fn encode_state(&self, out: &mut Output) -> Result<(), OutputError> {
        out.literal(",\"last_reader_generation\":")?; out.integer(self.last_generation)?;
        out.literal(",\"readers\":[")?;
        for (i, r) in self.slots.iter().flatten().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"pane\":")?; out.integer(r.pane().get())?;
            out.literal(",\"generation\":")?; out.integer(r.generation())?;
            out.literal(",\"index_steps\":")?; out.integer(r.index_steps())?;
            out.literal(",\"seek_steps\":")?; out.integer(r.request_steps())?;
            out.literal(",\"pending\":")?; out.boolean(r.has_pending_work())?;
            out.literal(",\"seek_generation\":")?; optional(out, r.request_generation())?;
            out.literal(",\"ready_seek_generation\":")?; optional(out, r.ready_generation())?; out.literal("}")?;
        }
        out.literal("]")
    }
    fn slot(&self, pane: DeskPaneId) -> Result<usize, Failure> {
        self.slots.iter().position(|r| r.as_ref().is_some_and(|r| r.pane() == pane))
            .ok_or(Failure::Protocol("DESK_NO_READER"))
    }
    fn get(&mut self, pane: DeskPaneId) -> Result<&mut DeskReading, Failure> {
        let at = self.slot(pane)?;
        self.slots[at].as_mut().ok_or(Failure::Protocol("DESK_NO_READER"))
    }
    pub(super) fn execute(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        command: &str, args: &[&str], canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        self.retain_current(desk);
        if command == "reader-prepare" {
            let (p, generation, limits) = match args {
                [p, g] => (pane(desk, p)?, number(g)?, ReaderLimits::default()),
                [p, g, n, stride] => (pane(desk, p)?, number(g)?, ReaderLimits { max_checkpoints: count(n)?, min_checkpoint_bytes: count(stride)? }),
                _ => return Err(Failure::Protocol("DESK_READER_COMMAND_SYNTAX")),
            };
            if generation == 0 || generation <= self.last_generation { return Err(DeskReadingError::StaleGeneration.into()); }
            self.last_generation = generation;
            let at = self.slot(p).ok().or_else(|| self.slots.iter().position(Option::is_none))
                .ok_or(Failure::Protocol("DESK_READER_LIMIT"))?;
            let mut candidate = DeskReading::prepare(desk, expected, p, generation, limits, &mut *canceled)?;
            let response = candidate.info(desk, expected, generation, &mut *canceled)?;
            if canceled() { return Err(Failure::Canceled); }
            self.slots[at] = Some(candidate);
            return Ok(response);
        }
        match (command, args) {
            ("reader-info", [p, g]) => {
                self.progress_reply = true;
                Ok(self.get(pane(desk, p)?)?.info(desk, expected, number(g)?, &mut *canceled)?)
            }
            ("reader-index", [p, g, step, bytes]) => {
                self.progress_reply = true;
                Ok(self.get(pane(desk, p)?)?.index_step(desk, expected, number(g)?, number(step)?, count(bytes)?, &mut *canceled)?)
            }
            ("reader-line" | "reader-byte", [p, g, q, value]) => {
                let target = if command == "reader-line" { ReadingTarget::Line(LineNumber::new(number(value)?)
                    .map_err(|_| Failure::Protocol("DESK_INVALID_LINE"))?) } else { ReadingTarget::Byte(ByteOffset::new(number(value)?)) };
                self.progress_reply = true;
                Ok(self.get(pane(desk, p)?)?.begin(desk, expected, number(g)?, number(q)?, target, &mut *canceled)?)
            }
            ("reader-range", [p, g, q, start, end]) => {
                let range = ByteRange::new(ByteOffset::new(number(start)?), ByteOffset::new(number(end)?))
                    .map_err(|_| Failure::Protocol("DESK_INVALID_RANGE"))?;
                self.progress_reply = true;
                Ok(self.get(pane(desk, p)?)?.begin(desk, expected, number(g)?, number(q)?, ReadingTarget::Range(range), &mut *canceled)?)
            }
            ("reader-step", [p, g, q, step, bytes]) => {
                self.progress_reply = true;
                Ok(self.get(pane(desk, p)?)?.step(desk, expected, number(g)?, number(q)?, number(step)?, count(bytes)?, &mut *canceled)?)
            }
            ("reader-cancel", [p, g, q]) => Ok(self.get(pane(desk, p)?)?.cancel(desk, expected, number(g)?, number(q)?, &mut *canceled)?),
            ("reader-go", [p, g, q]) => {
                self.get(pane(desk, p)?)?.go(desk, expected, attempt, number(g)?, number(q)?, &mut *canceled)?;
                Ok(desk.state(|| false)?)
            }
            ("reader-window", [p, g, q, bytes, lines]) => Ok(self.get(pane(desk, p)?)?.window(desk, expected,
                number(g)?, number(q)?, None, ReadingWindowOptions { max_bytes: count(bytes)?, max_lines: count(lines)? }, &mut *canceled)?),
            ("reader-next", [p, g, q, offset, bytes, lines]) => Ok(self.get(pane(desk, p)?)?.window(desk, expected,
                number(g)?, number(q)?, Some(number(offset)?), ReadingWindowOptions { max_bytes: count(bytes)?, max_lines: count(lines)? }, &mut *canceled)?),
            ("reader-clear", [p, g]) => {
                let at = self.slot(pane(desk, p)?)?;
                if self.slots[at].as_ref().map(DeskReading::generation) != Some(number(g)?) { return Err(DeskReadingError::StaleGeneration.into()); }
                let response = desk.state(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.slots[at] = None; Ok(response)
            }
            _ => Err(Failure::Protocol("DESK_READER_COMMAND_SYNTAX")),
        }
    }
}
fn optional(out: &mut Output, value: Option<u64>) -> Result<(), OutputError> {
    match value { Some(n) => out.integer(n), None => out.literal("null") }
}
