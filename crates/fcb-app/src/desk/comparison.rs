#![forbid(unsafe_code)]

//! One optional comparison for the real stdio desk. Source pins and bounded
//! comparison descriptors use the host desk budget. Publication retains the
//! old result until the replacement AND first response are complete. This
//! module supplies protocol/lifecycle composition, not another diff engine.

use super::{Failure, DeskSession, HostResponse, Output, number, count, pane};
use crate::output::OutputError;
use crate::host::desk::comparison::{DeskComparison, DeskComparisonLimits, ComparisonSide};
pub(super) use crate::host::desk::comparison::DeskComparisonError;

pub(super) const COMPARISON_HELP: &str = "\nCompare two retained desk panes (no live-source reads):\n\
  compare-prepare BEFORE_PANE AFTER_PANE COMPARISON_GEN [EDIT_LIMIT WORK_LIMIT]\n\
  compare-page COMPARISON_GEN FIRST_SPAN COUNT\n\
  compare-window COMPARISON_GEN SPAN BEFORE_SKIP AFTER_SKIP MAX_BYTES\n\
  compare-select COMPARISON_GEN SPAN before|after\n\
  compare-clear COMPARISON_GEN\n\
Pin the old reader before opening the new one. Before/after are explicit roles,\n\
not a claim of file continuity. Sources may also come from repository hits or\n\
restored checkpoints. Comparison never invokes Git or reads the old path again.\n\
Generations increase independently from search/document generations. Failed\n\
replacement preserves the old comparison. Every response reports accepted state.\n\
Defaults: 256 byte edits, 8388608 comparison work units; ceilings 512 and 67108864.\n\
Input adapters also perform bounded digest work, outside the diff work counter.\n\
An exhausted budget leaves UNRESOLVED regions, not a false identical result.\n\
Pages contain 1..128 spans; window limits are 1..65536 bytes PER SIDE. Spans and\n\
skips are zero-based original-byte coordinates, which can split Unicode text.\n\
Each side returns exact hex and optional valid UTF-8, with its own next_skip.\n\
An empty insertion/deletion side is a real zero-byte caret, not missing source.\n\
Select moves only the requested side through ordinary history/bookmark state;\n\
other comparison commands do not change the desk revision. No source writes,\n\
annotation reattachment, line-diff interpretation or native diff UI is implied.\n\
Close/retarget/restore retires stale comparison state. Checkpoints keep sources\n\
and selected ranges, not the derived alignment; prepare it again after restore.\n";

pub(super) struct ComparisonCommands { accepted: Option<DeskComparison>, last_attempt: u64 }
impl ComparisonCommands {
    pub(super) fn new() -> Self { Self { accepted: None, last_attempt: 0 } }
    pub(super) fn retain_current(&mut self, desk: &DeskSession) {
        if self.accepted.as_ref().is_some_and(|c| c.validate_sources(desk, desk.model().revision()).is_err()) {
            self.accepted = None;
        }
    }
    pub(super) fn encode_state(&self, out: &mut Output) -> Result<(), OutputError> {
        out.literal(",\"last_comparison_attempt\":")?; out.integer(self.last_attempt)?;
        out.literal(",\"comparison\":")?;
        if let Some(c) = &self.accepted {
            out.literal("{\"generation\":")?; out.integer(c.generation())?;
            out.literal(",\"before_pane\":")?; out.integer(c.panes()[0].get())?;
            out.literal(",\"after_pane\":")?; out.integer(c.panes()[1].get())?;
            out.literal(",\"complete\":")?; out.boolean(c.is_complete())?; out.literal("}")?;
        } else { out.literal("null")?; }
        Ok(())
    }
    fn get(&self, generation: u64) -> Result<&DeskComparison, Failure> {
        let c = self.accepted.as_ref().ok_or(Failure::Protocol("DESK_NO_COMPARISON"))?;
        if generation != c.generation() { return Err(Failure::Protocol("COMPARISON_STALE")); }
        Ok(c)
    }
    pub(super) fn execute(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        command: &str, args: &[&str], canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        match (command, args) {
            ("compare-prepare", [before, after, generation]) =>
                self.prepare(desk, expected, before, after, generation, DeskComparisonLimits::default(), canceled),
            ("compare-prepare", [before, after, generation, edits, work]) =>
                self.prepare(desk, expected, before, after, generation, DeskComparisonLimits {
                    max_edit_distance: count(edits)?, max_work: number(work)?, ..Default::default()
                }, canceled),
            ("compare-page", [generation, first, limit]) => {
                let generation = number(generation)?;
                Ok(self.get(generation)?.page(desk, expected, generation, count(first)?, count(limit)?, &mut *canceled)?)
            }
            ("compare-window", [generation, span, before_skip, after_skip, bytes]) => {
                let generation = number(generation)?;
                Ok(self.get(generation)?.window(desk, expected, generation, count(span)?,
                    [number(before_skip)?, number(after_skip)?], count(bytes)?, &mut *canceled)?)
            }
            ("compare-select", [generation, span, side]) => {
                let side = match *side { "before" => ComparisonSide::Before, "after" => ComparisonSide::After,
                    _ => return Err(Failure::Protocol("DESK_COMPARISON_SIDE")) };
                let generation = number(generation)?;
                self.get(generation)?.select(desk, expected, attempt, generation, count(span)?, side, &mut *canceled)?;
                // Source selection is already accepted. Do not report canceled
                // rollback merely because cancellation arrives during encoding.
                Ok(desk.state(|| false)?)
            }
            ("compare-clear", [generation]) => {
                self.get(number(generation)?)?.validate_sources(desk, expected)?;
                let response = desk.state(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                self.accepted = None;
                Ok(response)
            }
            _ => Err(Failure::Protocol("DESK_COMPARISON_COMMAND_SYNTAX")),
        }
    }
    fn prepare(&mut self, desk: &mut DeskSession, expected: u64, before: &str, after: &str,
        generation: &str, mut limits: DeskComparisonLimits, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, Failure> {
        let before = pane(desk, before)?; let after = pane(desk, after)?;
        let generation = number(generation)?;
        if generation == 0 || generation <= self.last_attempt { return Err(Failure::Protocol("COMPARISON_STALE")); }
        self.last_attempt = generation;
        limits.max_source_bytes = usize::try_from(desk.model().limits().source_bytes)
            .map_err(|_| Failure::Protocol("COMPARISON_LIMIT"))?;
        let candidate = DeskComparison::prepare(desk, expected, before, after, generation, limits, &mut *canceled)?;
        let response = candidate.page(desk, expected, generation, 0, 64, &mut *canceled)?;
        if canceled() { return Err(Failure::Canceled); }
        self.accepted = Some(candidate);
        Ok(response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb::{ArenaOwnerId, FileId, SourceRevision, SourceCapture};
    use crate::host::desk::{DeskLimits, DeskCommand};
    fn setup() -> (DeskSession, ComparisonCommands) {
        let owner = ArenaOwnerId::new(5601).unwrap();
        let mut d = DeskSession::new(owner, DeskLimits::default()).unwrap();
        let capture = |id, text: &[u8]| SourceCapture::from_bytes(owner, FileId::new(owner, id).unwrap(),
            SourceRevision::new(owner, id).unwrap(), format!("source-{id}"), text.to_vec()).unwrap();
        let a = d.adopt(0, 1, capture(1, b"old"), 0, None, || false).unwrap().active.unwrap();
        d.apply(1, 2, DeskCommand::Pin { pane: a, pinned: true }, || false).unwrap();
        d.adopt(2, 3, capture(2, b"new"), 0, None, || false).unwrap();
        let mut c = ComparisonCommands::new();
        c.execute(&mut d, 3, 4, "compare-prepare", &["1", "2", "1"], &mut || false).unwrap();
        (d, c)
    }
    #[test]
    fn final_publication_cancellation_preserves_old_comparison_and_consumes_attempt() {
        let (mut probe, mut c) = setup(); let mut calls = 0;
        c.execute(&mut probe, 3, 5, "compare-prepare", &["1", "2", "2"], &mut || { calls += 1; false }).unwrap();
        let (mut d, mut c) = setup(); let mut seen = 0;
        assert!(matches!(c.execute(&mut d, 3, 5, "compare-prepare", &["1", "2", "2"], &mut || { seen += 1; seen == calls }), Err(e) if e.canceled()));
        assert_eq!(c.accepted.as_ref().unwrap().generation(), 1); assert_eq!(c.last_attempt, 2);
        assert_eq!(d.model().revision(), 3);
        assert!(c.execute(&mut d, 3, 6, "compare-page", &["1", "0", "64"], &mut || false).is_ok());
        assert!(c.execute(&mut d, 3, 7, "compare-prepare", &["1", "2", "2"], &mut || false).is_err());
    }
    #[test]
    fn canceled_clear_keeps_accepted_state_and_all_source_pins() {
        let (mut d, mut c) = setup();
        assert!(matches!(c.execute(&mut d, 3, 5, "compare-clear", &["1"], &mut || true), Err(e) if e.canceled()));
        assert_eq!(c.accepted.as_ref().unwrap().generation(), 1);
        assert!(c.execute(&mut d, 3, 6, "compare-page", &["1", "0", "64"], &mut || false).is_ok());
        assert_eq!(d.model().retained_source_count(), 2);
    }
}
