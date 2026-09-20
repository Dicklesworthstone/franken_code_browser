#![forbid(unsafe_code)]

//! Persistent-desk code-navigation protocol. Fixed pane tables retain bounded
//! host objects, not another extractor. Replacement publishes only after its
//! first complete response exists. Global, independent high-water marks prevent
//! a retired pane/source's old ordinal from aliasing a new analysis generation.

use super::{Failure, DeskSession, HostResponse, Output, number, count, hex, pane};
use crate::output::OutputError;
use crate::host::desk::{DeskPaneId, code::{DeskOutline, DeskOutlineOptions, DeskReferences,
    SymbolLanguage, SymbolNameMode, MAX_CODE_NAME_QUERY_BYTES}};
pub(super) use crate::host::desk::code::DeskCodeError;
use fcb::ui::reading_panes::desk::MAX_DESK_PANES;

pub(super) const CODE_HELP: &str = "\nCode navigation on exact retained desk sources:\n\
  code-outline PANE OUTLINE_GEN auto|LANGUAGE [LIMIT]\n\
  code-symbols PANE OUTLINE_GEN FIRST COUNT\n\
  code-find PANE OUTLINE_GEN exact|prefix|contains FIRST COUNT NAME\n\
  code-select PANE OUTLINE_GEN SYMBOL_ID name|evidence\n\
  code-references PANE REFERENCE_GEN LIMIT NAME\n\
  code-references-hex PANE REFERENCE_GEN LIMIT UTF8_NAME_HEX\n\
  code-symbol-references PANE OUTLINE_GEN SYMBOL_ID REFERENCE_GEN LIMIT\n\
  code-ref-page PANE REFERENCE_GEN FIRST COUNT\n\
  code-ref-select PANE REFERENCE_GEN REFERENCE_ID\n\
  code-clear PANE OUTLINE_GEN | code-ref-clear PANE REFERENCE_GEN\n\
Languages: rust, python, javascript, typescript, go, cpp (C/C++). auto uses only\n\
the retained label suffix. No parser/LSP process or live source is invoked.\n\
Outlines are heuristic declaration candidates, NOT exhaustive or compiler-proven.\n\
Names/IDs/parents retain their original inventory identity across filtered pages.\n\
Evidence means the extractor's recorded region, not a semantic function body.\n\
Reference search is whole-token, case-sensitive text in ONE pane; comments and\n\
strings participate. It does not establish language name bindings. Composite\n\
outline names that are not a supported token are refused, never reinterpreted.\n\
Preparation generations increase globally across panes, separately for outlines\n\
and references. Failed/canceled attempts consume their generation but preserve\n\
the previous result. Ordinary search, documents and repository work are separate.\n\
IDs are one-based; page offsets are zero-based and page counts are 1..128.\n\
Outline input: at most 64 KiB with existing complexity guards, 1..4096 items.\n\
Reference input: at most 512 KiB, limit 0..4096; a zero limit probes existence.\n\
Unavailable analysis leaves the exact source readable. Limits/counts/heuristic\n\
evidence remain explicit; a limited inventory is never an exhaustive negative.\n\
Only code-select/code-ref-select change desk revision. Use ordinary copy and\n\
bookmark afterward; selected source bytes survive checkpoints. Derived code\n\
indexes are not saved. Retarget/close/restore retires stale code objects; clearing\n\
an outline does not clear its already accepted independent reference results.\n";

pub(super) struct CodeCommands {
    outlines: [Option<DeskOutline>; MAX_DESK_PANES],
    references: [Option<DeskReferences>; MAX_DESK_PANES],
    last_outline_attempt: u64,
    last_reference_attempt: u64,
}
impl CodeCommands {
    pub(super) fn new() -> Self {
        Self { outlines: std::array::from_fn(|_| None), references: std::array::from_fn(|_| None),
            last_outline_attempt: 0, last_reference_attempt: 0 }
    }
    pub(super) fn retain_current(&mut self, desk: &DeskSession) {
        for slot in &mut self.outlines {
            if slot.as_ref().is_some_and(|o| o.validate_source(desk, desk.model().revision()).is_err()) { *slot = None; }
        }
        for slot in &mut self.references {
            if slot.as_ref().is_some_and(|r| r.validate_source(desk, desk.model().revision()).is_err()) { *slot = None; }
        }
    }
    pub(super) fn encode_state(&self, out: &mut Output) -> Result<(), OutputError> {
        out.literal(",\"last_code_outline_attempt\":")?; out.integer(self.last_outline_attempt)?;
        out.literal(",\"last_code_reference_attempt\":")?; out.integer(self.last_reference_attempt)?;
        out.literal(",\"code_outlines\":[")?;
        for (i, o) in self.outlines.iter().flatten().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"pane\":")?; out.integer(o.pane().get())?;
            out.literal(",\"generation\":")?; out.integer(o.generation())?;
            out.literal(",\"output_limited\":")?; out.boolean(o.output_limited())?; out.literal("}")?;
        }
        out.literal("],\"code_references\":[")?;
        for (i, r) in self.references.iter().flatten().enumerate() {
            if i > 0 { out.literal(",")?; }
            out.literal("{\"pane\":")?; out.integer(r.pane().get())?;
            out.literal(",\"generation\":")?; out.integer(r.generation())?;
            out.literal(",\"complete\":")?; out.boolean(r.is_complete())?; out.literal("}")?;
        }
        out.literal("]")
    }
    fn outline_slot(&self, pane: DeskPaneId) -> Result<usize, Failure> {
        self.outlines.iter().position(|o| o.as_ref().is_some_and(|o| o.pane() == pane))
            .or_else(|| self.outlines.iter().position(Option::is_none)).ok_or(Failure::Protocol("DESK_CODE_PANE_LIMIT"))
    }
    fn get_outline(&self, pane: DeskPaneId, generation: u64) -> Result<&DeskOutline, Failure> {
        let o = self.outlines.iter().flatten().find(|o| o.pane() == pane).ok_or(Failure::Protocol("DESK_NO_OUTLINE"))?;
        if o.generation() != generation { return Err(DeskCodeError::StaleGeneration.into()); }
        Ok(o)
    }
    fn get_references(&self, pane: DeskPaneId, generation: u64) -> Result<&DeskReferences, Failure> {
        let r = self.references.iter().flatten().find(|r| r.pane() == pane).ok_or(Failure::Protocol("DESK_NO_REFERENCES"))?;
        if r.generation() != generation { return Err(DeskCodeError::StaleGeneration.into()); }
        Ok(r)
    }
    fn publish_references(&mut self, desk: &mut DeskSession, expected: u64, candidate: DeskReferences,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        let slot = self.references.iter().position(|r| r.as_ref().is_some_and(|r| r.pane() == candidate.pane()))
            .or_else(|| self.references.iter().position(Option::is_none)).ok_or(Failure::Protocol("DESK_CODE_PANE_LIMIT"))?;
        let response = candidate.page(desk, expected, candidate.generation(), 0, 64, &mut *canceled)?;
        if canceled() { return Err(Failure::Canceled); }
        self.references[slot] = Some(candidate);
        Ok(response)
    }
    pub(super) fn execute(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        command: &str, args: &[&str], canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        self.retain_current(desk);
        match (command, args) {
            ("code-outline", [p, generation, language]) =>
                self.prepare_outline(desk, expected, p, generation, language, 4096, canceled),
            ("code-outline", [p, generation, language, limit]) =>
                self.prepare_outline(desk, expected, p, generation, language, count(limit)?, canceled),
            ("code-symbols", [p, generation, first, limit]) =>
                Ok(self.get_outline(pane(desk, p)?, number(generation)?)?.page(desk, expected,
                    number(generation)?, "", SymbolNameMode::Exact, count(first)?, count(limit)?, &mut *canceled)?),
            ("code-find", [p, generation, mode, first, limit, name]) => {
                let mode = match *mode { "exact" => SymbolNameMode::Exact, "prefix" => SymbolNameMode::Prefix,
                    "contains" => SymbolNameMode::Contains, _ => return Err(Failure::Protocol("DESK_CODE_NAME_MODE")) };
                Ok(self.get_outline(pane(desk, p)?, number(generation)?)?.page(desk, expected,
                    number(generation)?, name, mode, count(first)?, count(limit)?, &mut *canceled)?)
            }
            ("code-select", [p, generation, symbol, extent]) => {
                let whole = match *extent { "name" => false, "evidence" => true,
                    _ => return Err(Failure::Protocol("DESK_CODE_SELECTION_KIND")) };
                self.get_outline(pane(desk, p)?, number(generation)?)?.select(desk, expected, attempt,
                    number(generation)?, number(symbol)?, whole, &mut *canceled)?;
                Ok(desk.state(|| false)?)
            }
            ("code-references" | "code-references-hex", [p, generation, limit, name]) => {
                let p = pane(desk, p)?; let generation = number(generation)?; let limit = count(limit)?;
                let decoded;
                let name = if command.ends_with("-hex") {
                    decoded = String::from_utf8(hex(name, MAX_CODE_NAME_QUERY_BYTES)?).map_err(|_| Failure::Protocol("DESK_INVALID_TEXT"))?;
                    decoded.as_str()
                } else { name };
                admit(&mut self.last_reference_attempt, generation)?;
                let candidate = DeskReferences::prepare(desk, expected, p, generation, name, limit, &mut *canceled)?;
                self.publish_references(desk, expected, candidate, canceled)
            }
            ("code-symbol-references", [p, outline, symbol, reference, limit]) => {
                let p = pane(desk, p)?; let outline = number(outline)?; let symbol = number(symbol)?;
                let reference = number(reference)?; let limit = count(limit)?;
                self.get_outline(p, outline)?;
                admit(&mut self.last_reference_attempt, reference)?;
                let candidate = self.get_outline(p, outline)?.references(desk, expected, outline, symbol, reference, limit, &mut *canceled)?;
                self.publish_references(desk, expected, candidate, canceled)
            }
            ("code-ref-page", [p, generation, first, limit]) =>
                Ok(self.get_references(pane(desk, p)?, number(generation)?)?.page(desk, expected,
                    number(generation)?, count(first)?, count(limit)?, &mut *canceled)?),
            ("code-ref-select", [p, generation, id]) => {
                self.get_references(pane(desk, p)?, number(generation)?)?.select(desk, expected, attempt,
                    number(generation)?, number(id)?, &mut *canceled)?;
                Ok(desk.state(|| false)?)
            }
            ("code-clear" | "code-ref-clear", [p, generation]) => {
                let p = pane(desk, p)?; let generation = number(generation)?;
                if command == "code-clear" { self.get_outline(p, generation)?.validate_source(desk, expected)?; }
                else { self.get_references(p, generation)?.validate_source(desk, expected)?; }
                let response = desk.state(&mut *canceled)?;
                if canceled() { return Err(Failure::Canceled); }
                if command == "code-clear" {
                    for slot in &mut self.outlines { if slot.as_ref().is_some_and(|o| o.pane() == p) { *slot = None; } }
                } else {
                    for slot in &mut self.references { if slot.as_ref().is_some_and(|r| r.pane() == p) { *slot = None; } }
                }
                Ok(response)
            }
            _ => Err(Failure::Protocol("DESK_CODE_COMMAND_SYNTAX")),
        }
    }
    fn prepare_outline(&mut self, desk: &mut DeskSession, expected: u64, p: &str, generation: &str,
        language: &str, max_items: usize, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, Failure> {
        let p = pane(desk, p)?; let generation = number(generation)?;
        let language = if language == "auto" { None } else {
            Some(SymbolLanguage::from_name(language).ok_or(Failure::Protocol("DESK_CODE_UNSUPPORTED_LANGUAGE"))?)
        };
        admit(&mut self.last_outline_attempt, generation)?;
        let slot = self.outline_slot(p)?;
        let candidate = DeskOutline::prepare(desk, expected, p, generation, DeskOutlineOptions { language, max_items }, &mut *canceled)?;
        let response = candidate.page(desk, expected, generation, "", SymbolNameMode::Exact, 0, 64, &mut *canceled)?;
        if canceled() { return Err(Failure::Canceled); }
        self.outlines[slot] = Some(candidate);
        Ok(response)
    }
}
fn admit(last: &mut u64, generation: u64) -> Result<(), Failure> {
    if generation == 0 || generation <= *last { return Err(DeskCodeError::StaleGeneration.into()); }
    *last = generation; Ok(())
}

#[cfg(test)]
mod tests;
