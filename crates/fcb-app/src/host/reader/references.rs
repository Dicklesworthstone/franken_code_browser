#![forbid(unsafe_code)]

//! Whole-token occurrence navigation over the reader's retained capture.
//! Token policy, decoding and source evidence come from the public reference
//! engine. This adapter owns only bounded result records and their independent
//! publication generation. It neither reparses source nor resolves bindings.

use std::mem::size_of;
use fcb::search::symbols::{CapturedReferences, ReferenceCandidate, ReferenceError,
    MAX_REFERENCE_ITEMS, MAX_REFERENCE_NAME_BYTES, MAX_REFERENCE_SOURCE_BYTES};
use super::{check, range, reserve, AppError, ByteLength, ByteRange, HostResponse,
    Output, QueryGeneration, ReaderSession, ReaderSessionError, ResourceLease,
    SelectionIdentity, EXIT_OK, EXIT_PARTIAL, MAX_READER_CONTEXT_BYTES,
    MAX_READER_WINDOW_BYTES};

pub const MAX_READER_REFERENCE_PAGE: usize = 128;
pub const MAX_READER_REFERENCE_NAME_BYTES: usize = MAX_REFERENCE_NAME_BYTES;
pub const MAX_READER_REFERENCES: usize = MAX_REFERENCE_ITEMS;
pub const MAX_READER_REFERENCE_SOURCE_BYTES: usize = MAX_REFERENCE_SOURCE_BYTES;

pub(super) struct AcceptedReferences {
    generation: u64,
    name: String,
    rows: Vec<ReferenceCandidate>,
    counted: usize,
    limited: bool,
    max_items: usize,
    _lease: ResourceLease,
}

impl ReaderSession {
    pub fn reference_generation(&self) -> Option<u64> {
        self.references.as_ref().map(|references| references.generation)
    }

    /// Find exact same-token text occurrences, including declarations, comments
    /// and literals. This is not compiler reference resolution. The shared
    /// engine admits at most 512 KiB of valid UTF-8/BOM-marked UTF-16 source,
    /// independently of the ordinary reader and declaration-outline limits.
    ///
    /// A zero cap probes existence. The engine counts at most one unstored
    /// occurrence to distinguish an exact cap from a truncated inventory.
    /// Failures/cancellation consume the attempt but preserve accepted results.
    /// The first page is encoded privately before replacing that inventory.
    pub fn prepare_references(&mut self, generation: u64, name: &str, max_items: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        self.reference_attempt(generation)?;
        check(&mut canceled)?;
        let [extract_id, rows_id] = self.allocations()?;
        let query = QueryGeneration::new(self.owner(), generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let extracted = CapturedReferences::build(self.capture.bytes(), *self.capture.request(),
            query, name, Some(self.encoding), max_items, &self.budget, extract_id, &mut canceled)?;
        let count = extracted.candidates().len();
        let charge = count.checked_mul(size_of::<ReferenceCandidate>())
            .and_then(|n| n.checked_add(size_of::<AcceptedReferences>()))
            .and_then(|n| n.checked_add(name.len())).ok_or(AppError::Admission)?;
        // Old accepted rows and extraction scratch stay charged while the new
        // bounded inventory is copied; source backing is never duplicated.
        let lease = self.budget.try_reserve_managed(self.owner(), rows_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut rows = reserve(count)?;
        for row in extracted.candidates() {
            check(&mut canceled)?;
            rows.push(*row);
        }
        let mut owned_name = String::new();
        owned_name.try_reserve_exact(name.len()).map_err(|_| AppError::Admission)?;
        if owned_name.capacity() > name.len() { return Err(AppError::Admission.into()); }
        owned_name.push_str(name);
        let candidate = AcceptedReferences { generation, name: owned_name, rows,
            counted: extracted.total_matches_counted(), limited: extracted.output_limited(),
            max_items, _lease: lease };
        drop(extracted);
        check(&mut canceled)?;
        let mut out = self.output("references")?;
        encode_page(&mut out, &candidate, 0, 64, &mut canceled)?;
        let response = self.finish_output(out, references_exit(&candidate), &mut canceled)?;
        check(&mut canceled)?;
        self.references = Some(candidate);
        Ok(response)
    }

    /// Page retained metadata only. Offsets are not activation IDs; IDs remain
    /// one-based in the accepted inventory regardless of the displayed page.
    pub fn reference_page(&mut self, generation: u64, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if !(1..=MAX_READER_REFERENCE_PAGE).contains(&limit) {
            return Err(ReaderSessionError::InvalidLimits);
        }
        check(&mut canceled)?;
        self.reference_snapshot(generation)?;
        let mut out = self.output("reference-page")?;
        let references = self.reference_snapshot(generation)?;
        encode_page(&mut out, references, start, limit, &mut canceled)?;
        let exit = references_exit(references);
        self.finish_output(out, exit, &mut canceled)
    }

    pub fn reference_range(&self, generation: u64, id: u64) -> Result<ByteRange, ReaderSessionError> {
        Ok(self.reference_row(generation, id)?.original_range())
    }

    /// The normal scalar/CRLF-aware reader verifies the original-byte and
    /// decoded-window selection round trip. No current pathname is opened.
    pub fn reference_window(&mut self, generation: u64, id: u64, context: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if context > MAX_READER_CONTEXT_BYTES { return Err(ReaderSessionError::InvalidLimits); }
        check(&mut canceled)?;
        let row = self.reference_row(generation, id)?;
        let selected = row.original_range();
        let identity = SelectionIdentity::Reference { generation, reference_id: id, line: row.line() };
        let requested = range(selected.start().get().saturating_sub(context as u64 + 4),
            selected.end().get().saturating_add(context as u64 + 4).min(self.length()))?;
        self.window("reference", requested, MAX_READER_WINDOW_BYTES,
            Some((identity, selected)), None, &mut canceled)
    }

    /// Return exact original token bytes for an explicit copy action. Response
    /// ownership is independent of a later clear/close; no clipboard is written.
    pub fn copy_reference(&mut self, generation: u64, id: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        let row = self.reference_row(generation, id)?;
        let identity = SelectionIdentity::Reference { generation, reference_id: id, line: row.line() };
        self.copy("copy-reference", row.original_range(), Some(identity), &mut canceled)
    }

    /// Release only occurrence metadata. Source, find, outline and Markdown
    /// retain separate ownership and selection namespaces, even at equal IDs.
    pub fn clear_references(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        self.reference_attempt(generation)?;
        check(&mut canceled)?;
        let mut out = self.output("references-clear")?;
        out.literal(",\"reference_generation\":")?; out.integer(generation)?;
        out.literal(",\"selection_namespace\":\"reference\",\"accepted_reference_generation\":null,\"retained_references\":\"0\"}\n")?;
        let response = self.finish_output(out, EXIT_OK, &mut canceled)?;
        check(&mut canceled)?;
        self.references = None;
        Ok(response)
    }

    fn reference_attempt(&mut self, generation: u64) -> Result<(), ReaderSessionError> {
        if generation == 0 || generation <= self.last_reference_attempt {
            return Err(ReferenceError::StaleQuery.into());
        }
        self.last_reference_attempt = generation;
        Ok(())
    }

    fn reference_snapshot(&self, generation: u64) -> Result<&AcceptedReferences, ReaderSessionError> {
        let references = self.references.as_ref().ok_or(ReaderSessionError::MissingReferences)?;
        if references.generation != generation { return Err(ReferenceError::StaleQuery.into()); }
        Ok(references)
    }

    fn reference_row(&self, generation: u64, id: u64) -> Result<ReferenceCandidate, ReaderSessionError> {
        let references = self.reference_snapshot(generation)?;
        let index = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(ReferenceError::NotFound)?;
        references.rows.get(index).copied().filter(|row| row.id() == id)
            .ok_or_else(|| ReferenceError::NotFound.into())
    }
}

fn references_exit(references: &AcceptedReferences) -> u8 {
    if references.limited { EXIT_PARTIAL } else { EXIT_OK }
}

fn encode_page(out: &mut Output, references: &AcceptedReferences, start: usize, limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
    if start > references.rows.len() { return Err(ReaderSessionError::InvalidRange); }
    out.literal(",\"reference_generation\":")?; out.integer(references.generation)?;
    out.literal(",\"name\":")?; out.quoted(&references.name)?;
    out.literal(",\"selection_namespace\":\"reference\",\"evidence_level\":\"whole-token-text-candidate\",\"includes_comments_and_literals\":true,\"compiler_resolved\":false,\"semantic_complete\":false,\"name_case\":\"sensitive\",\"line_semantics\":\"LF-counted-source-lines\",\"max_source_bytes\":")?;
    out.integer(MAX_READER_REFERENCE_SOURCE_BYTES as u64)?;
    out.literal(",\"max_items\":")?; out.integer(references.max_items as u64)?;
    out.literal(",\"search_complete\":")?; out.boolean(!references.limited)?;
    out.literal(",\"count_complete\":")?; out.boolean(!references.limited)?;
    out.literal(",\"output_limited\":")?; out.boolean(references.limited)?;
    out.literal(",\"retained_references\":")?; out.integer(references.rows.len() as u64)?;
    out.literal(",\"matches_counted\":")?; out.integer(references.counted as u64)?;
    out.literal(",\"references\":[")?;
    let end = start.saturating_add(limit).min(references.rows.len());
    for (index, row) in references.rows[start..end].iter().enumerate() {
        check(canceled)?;
        if index > 0 { out.literal(",")?; }
        out.literal("{\"reference_id\":")?; out.integer(row.id())?;
        out.literal(",\"original_range\":")?; out.range(row.original_range())?;
        out.literal(",\"line\":")?; out.integer(row.line())?;
        out.literal("}")?;
    }
    out.literal("],\"next_offset\":")?;
    if end < references.rows.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
    out.literal("}\n")?;
    Ok(())
}
