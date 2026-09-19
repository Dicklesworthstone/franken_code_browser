#![forbid(unsafe_code)]

//! Compare two retained desk panes without reopening either source. The existing
//! analysis engine owns bounded Myers refinement and byte correspondence. This
//! adapter retains only its bounded descriptors and the two source-accounting
//! pins. Paging never reruns comparison. Before/after are caller-selected roles,
//! not a claim about file continuity, rename detection or an atomic snapshot.
//! Preparation, response encoding and final destruction belong on a worker.

use std::mem::size_of;
use fcb::BrowserSession;
use fcb::analysis::comparison::{CaptureComparison, ComparisonError, ComparisonQuality,
    ComparisonRelation, ComparisonStats, Correspondence, CorrespondenceKind,
    MAX_DIFF_EDITS, MAX_DIFF_SOURCE_BYTES, MAX_DIFF_WORK};
pub use fcb::analysis::comparison::ComparisonLimits as DeskComparisonLimits;
use super::{DeskSession, DeskSessionError, DeskPaneId, DeskView, DeskError, DeskChange,
    DeskCommand, ByteLength, ByteOffset, ByteRange, QueryGeneration, ResourceLease,
    Output, OutputError, HostResponse, EXIT_OK, EXIT_PARTIAL, check};

pub const MAX_COMPARISON_PAGE: usize = 128;
pub const MAX_COMPARISON_WINDOW_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DeskComparisonError {
    Desk(DeskSessionError), Analysis(ComparisonError), SamePane, MissingSpan,
}
impl std::fmt::Display for DeskComparisonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Desk(e) => write!(f, "{e}"), Self::Analysis(e) => write!(f, "{e}"),
            Self::SamePane => f.write_str("DESK_COMPARISON_SAME_PANE"),
            Self::MissingSpan => f.write_str("DESK_COMPARISON_NO_SPAN"),
        }
    }
}
impl std::error::Error for DeskComparisonError {}
impl From<DeskSessionError> for DeskComparisonError { fn from(e: DeskSessionError) -> Self { Self::Desk(e) } }
impl From<DeskError> for DeskComparisonError { fn from(e: DeskError) -> Self { Self::Desk(e.into()) } }
impl From<ComparisonError> for DeskComparisonError { fn from(e: ComparisonError) -> Self { Self::Analysis(e) } }
impl From<OutputError> for DeskComparisonError { fn from(e: OutputError) -> Self { Self::Desk(e.into()) } }
impl DeskComparisonError {
    pub fn is_canceled(self) -> bool {
        match self { Self::Desk(e) => e.is_canceled(), Self::Analysis(ComparisonError::Canceled) => true, _ => false }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparisonSide { Before, After }

/// One immutable alignment. Hosts supply fresh, non-reused generations and keep
/// the previous object until replacement and its response are accepted. The CLI
/// supplies that publication lifecycle. Comparisons are derived state, not part
/// of a checkpoint; source selections made through them ARE ordinary desk state.
pub struct DeskComparison {
    panes: [DeskPaneId; 2],
    sources: [DeskView; 2],
    generation: u64,
    limits: DeskComparisonLimits,
    spans: Vec<Correspondence>,
    quality: ComparisonQuality,
    relation: ComparisonRelation,
    stats: ComparisonStats,
    _lease: ResourceLease,
}
impl DeskComparison {
    pub fn prepare(desk: &mut DeskSession, expected: u64, before: DeskPaneId, after: DeskPaneId,
        generation: u64, limits: DeskComparisonLimits, mut canceled: impl FnMut() -> bool)
        -> Result<Self, DeskComparisonError> {
        let a = desk.model().source(before, expected)?;
        let b = desk.model().source(after, expected)?;
        if before == after { return Err(DeskComparisonError::SamePane); }
        if generation == 0 { return Err(ComparisonError::Stale.into()); }
        if limits.max_edit_distance > MAX_DIFF_EDITS || limits.max_source_bytes > MAX_DIFF_SOURCE_BYTES
            || limits.max_work > MAX_DIFF_WORK || a.bytes().len() > limits.max_source_bytes
            || b.bytes().len() > limits.max_source_bytes { return Err(ComparisonError::Limits.into()); }
        let labels = a.logical_path().len() + b.logical_path().len();
        check(&mut canceled)?;
        let [before_id, after_id, metadata_id, engine_id] = desk.ids()?;
        let before_view = desk.model().view(before, expected, before_id)?;
        let after_view = desk.model().view(after, expected, after_id)?;
        let capacity = 2 * limits.max_edit_distance + 5;
        let charge = size_of::<Self>() + capacity * size_of::<Correspondence>() + 2 * labels + 1024;
        let lease = desk.budget.try_reserve_managed(desk.model().owner(), metadata_id, ByteLength::new(charge as u64))
            .map_err(|_| ComparisonError::ResourceDenied)?;
        // The existing facade adapter hashes on the worker and shares the exact
        // Arc payload. No source bytes are copied or assigned a different ID.
        let session = BrowserSession::new(desk.model().owner());
        let before_capture = session.prepare_search_capture(before_view.source().clone()).map_err(|_| ComparisonError::Stale)?;
        check(&mut canceled)?;
        let after_capture = session.prepare_search_capture(after_view.source().clone()).map_err(|_| ComparisonError::Stale)?;
        let qgen = QueryGeneration::new(desk.model().owner(), generation).map_err(|_| ComparisonError::Stale)?;
        let comparison = CaptureComparison::build(before_capture.capture(), after_capture.capture(), qgen,
            limits, &desk.budget, engine_id, &mut canceled)?;
        if comparison.spans().len() > capacity { return Err(ComparisonError::ResourceDenied.into()); }
        let mut spans = Vec::new();
        spans.try_reserve_exact(capacity).map_err(|_| ComparisonError::ResourceDenied)?;
        if spans.capacity() > capacity { return Err(ComparisonError::ResourceDenied.into()); }
        spans.extend_from_slice(comparison.spans());
        let quality = comparison.quality(); let relation = comparison.relation(); let stats = comparison.stats();
        check(&mut canceled)?;
        Ok(Self { panes: [before, after], sources: [before_view, after_view], generation,
            limits, spans, quality, relation, stats, _lease: lease })
    }
    pub const fn generation(&self) -> u64 { self.generation }
    pub const fn panes(&self) -> [DeskPaneId; 2] { self.panes }
    pub const fn quality(&self) -> ComparisonQuality { self.quality }
    pub const fn relation(&self) -> ComparisonRelation { self.relation }
    pub const fn stats(&self) -> ComparisonStats { self.stats }
    pub fn is_complete(&self) -> bool { self.quality == ComparisonQuality::Exact }
    pub fn validate_sources(&self, desk: &DeskSession, expected: u64) -> Result<(), DeskComparisonError> {
        for (pane, view) in self.panes.iter().zip(&self.sources) {
            let current = desk.model().source(*pane, expected)?;
            let retained = view.source();
            if current.file() != retained.file() || current.revision() != retained.revision()
                || !std::ptr::eq(current.bytes(), retained.bytes()) { return Err(ComparisonError::Stale.into()); }
        }
        Ok(())
    }
    fn validate(&self, desk: &DeskSession, expected: u64, generation: u64) -> Result<(), DeskComparisonError> {
        self.validate_sources(desk, expected)?;
        if self.generation != generation { return Err(ComparisonError::Stale.into()); }
        Ok(())
    }
    pub fn spans(&self, desk: &DeskSession, expected: u64, generation: u64)
        -> Result<&[Correspondence], DeskComparisonError> {
        self.validate(desk, expected, generation)?; Ok(&self.spans)
    }
    pub fn page(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        first: usize, count: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, DeskComparisonError> {
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        if count == 0 || count > MAX_COMPARISON_PAGE || first > self.spans.len() { return Err(ComparisonError::InvalidRange.into()); }
        let mut out = self.output(desk, "compare-page")?;
        let end = first.saturating_add(count).min(self.spans.len());
        out.literal(",\"spans\":[")?;
        for (i, span) in self.spans[first..end].iter().enumerate() {
            check(&mut canceled)?;
            if i > 0 { out.literal(",")?; }
            out.literal("{\"span\":")?; out.integer((first + i) as u64)?;
            encode_span(&mut out, *span)?; out.literal("}")?;
        }
        out.literal("],\"next_span\":")?;
        if end < self.spans.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
        out.literal("}\n")?;
        Ok(desk.finish(out, self.exit_code(), &mut canceled)?)
    }

    /// Independently page original bytes on EACH side of one span. Offsets are
    /// relative to that span, including unresolved regions. Never expand to a
    /// decoded character boundary or reconstruct malformed source from text.
    /// Empty insertion/deletion sides return real empty windows at their anchor.
    pub fn window(&self, desk: &mut DeskSession, expected: u64, generation: u64,
        ordinal: usize, skips: [u64; 2], max_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, DeskComparisonError> {
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        if max_bytes == 0 || max_bytes > MAX_COMPARISON_WINDOW_BYTES { return Err(ComparisonError::Limits.into()); }
        let span = *self.spans.get(ordinal).ok_or(DeskComparisonError::MissingSpan)?;
        let mut out = self.output(desk, "compare-window")?;
        out.literal(",\"span\":")?; out.integer(ordinal as u64)?; encode_span(&mut out, span)?;
        for (side, region) in [(0, span.before()), (1, span.after())] {
            check(&mut canceled)?;
            if skips[side] > region.len().get() { return Err(ComparisonError::InvalidRange.into()); }
            let start = region.start().get() + skips[side];
            let end = start.saturating_add(max_bytes as u64).min(region.end().get());
            let window = ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).map_err(|_| ComparisonError::InvalidRange)?;
            let (a, b) = window.as_usize_bounds().map_err(|_| ComparisonError::InvalidRange)?;
            let bytes = self.sources[side].source().bytes().get(a..b).ok_or(ComparisonError::InvalidRange)?;
            out.literal(if side == 0 { ",\"before\":{" } else { ",\"after\":{" })?;
            out.literal("\"original_range\":")?; out.range(window)?;
            out.literal(",\"original_hex\":")?; out.hex(bytes)?;
            out.literal(",\"exact_utf8_text\":")?;
            match std::str::from_utf8(bytes) { Ok(text) => out.quoted(text)?, Err(_) => out.literal("null")? }
            out.literal(",\"whole_span_visible\":")?; out.boolean(skips[side] == 0 && end == region.end().get())?;
            out.literal(",\"next_skip\":")?;
            if end < region.end().get() { out.integer(end - region.start().get())?; } else { out.literal("null")?; }
            out.literal("}")?;
        }
        out.literal("}\n")?;
        Ok(desk.finish(out, self.exit_code(), &mut canceled)?)
    }

    /// Navigate ONE explicitly chosen side. Empty sides select a zero-byte caret
    /// at an insertion/deletion boundary. This does not silently reattach notes,
    /// move the other pane, or claim a two-pane transaction. Ordinary desk history,
    /// bookmarks and checkpointing retain the selected exact source region.
    pub fn select(&self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, ordinal: usize, side: ComparisonSide, mut canceled: impl FnMut() -> bool)
        -> Result<DeskChange, DeskComparisonError> {
        desk.validate_mutation(expected, attempt)?;
        self.validate(desk, expected, generation)?; check(&mut canceled)?;
        let span = self.spans.get(ordinal).ok_or(DeskComparisonError::MissingSpan)?;
        let (pane, range) = match side { ComparisonSide::Before => (self.panes[0], span.before()),
            ComparisonSide::After => (self.panes[1], span.after()) };
        Ok(desk.apply(expected, attempt, DeskCommand::Navigate { pane, offset: range.start().get(),
            selection: Some(range) }, &mut canceled)?)
    }
    fn exit_code(&self) -> u8 { if self.is_complete() { EXIT_OK } else { EXIT_PARTIAL } }
    fn output(&self, desk: &mut DeskSession, command: &str) -> Result<Output, DeskComparisonError> {
        let mut out = desk.output(command)?;
        out.literal(",\"comparison_generation\":")?; out.integer(self.generation)?;
        out.literal(",\"comparison_complete\":")?; out.boolean(self.is_complete())?;
        out.literal(",\"comparison_quality\":")?;
        out.quoted(match self.quality { ComparisonQuality::Exact => "exact", ComparisonQuality::WorkLimit => "work-limit", ComparisonQuality::EditLimit => "edit-limit" })?;
        out.literal(",\"relation\":")?;
        out.quoted(match self.relation { ComparisonRelation::Identical => "identical", ComparisonRelation::Different => "different", ComparisonRelation::Undetermined => "undetermined" })?;
        out.literal(",\"coordinate_domain\":\"original-bytes-may-split-text\",\"alignment_semantics\":\"deterministic-not-identity-proof\",\"source_reopened\":false,\"total_spans\":")?;
        out.integer(self.spans.len() as u64)?;
        out.literal(",\"changed_spans\":")?; out.integer(self.spans.iter().filter(|s| s.kind() == CorrespondenceKind::Changed).count() as u64)?;
        out.literal(",\"unresolved_spans\":")?; out.integer(self.spans.iter().filter(|s| s.kind() == CorrespondenceKind::Unresolved).count() as u64)?;
        out.literal(",\"work_units\":")?; out.integer(self.stats.work_units)?;
        out.literal(",\"max_work\":")?; out.integer(self.limits.max_work)?;
        out.literal(",\"max_edit_distance\":")?; out.integer(self.limits.max_edit_distance as u64)?;
        out.literal(",\"edit_distance\":")?;
        match self.stats.edit_distance { Some(n) => out.integer(n as u64)?, None => out.literal("null")? }
        for i in 0..2 {
            out.literal(if i == 0 { ",\"before_source\":{" } else { ",\"after_source\":{" })?;
            let source = self.sources[i].source();
            out.literal("\"pane\":")?; out.integer(self.panes[i].get())?;
            out.literal(",\"file_id\":")?; out.integer(source.file().get())?;
            out.literal(",\"source_revision\":")?; out.integer(source.revision().get())?;
            out.literal(",\"bytes\":")?; out.integer(source.bytes().len() as u64)?;
            out.literal(",\"label\":")?; out.quoted(source.logical_path())?; out.literal("}")?;
        }
        Ok(out)
    }
}
fn encode_span(out: &mut Output, span: Correspondence) -> Result<(), OutputError> {
    out.literal(",\"kind\":")?;
    out.quoted(match span.kind() { CorrespondenceKind::Equal => "equal", CorrespondenceKind::Changed => "changed", CorrespondenceKind::Unresolved => "unresolved" })?;
    out.literal(",\"before_range\":")?; out.range(span.before())?;
    out.literal(",\"after_range\":")?; out.range(span.after())
}
