#![forbid(unsafe_code)]

//! Native-host composition of the public, bounded FileSearch engine. The last
//! accepted query owns a small occurrence table and exact original witnesses.
//! Exact/whole-token queries share a literal; lowercase queries preserve each spelling.
//! No whole-file capture, source spool, new matcher or inferred context exists.

use std::{fs::File, mem::size_of, sync::Arc};
use fcb::{ByteLength, ByteOffset, ByteRange, QueryGeneration, SourceRevision};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentConsistency, FileSearch,
    FileSearchReport, ResourceAllocationId, ResourceBudget, StreamReadOptions,
    StreamReadState, StreamReadStats, StreamReadStep, StreamingHit, StreamingMode, StreamingNeedle};
use fcb::store::Sha256Digest;
use fcb::search::identifier::{MAX_IDENTIFIER_BYTES, token_scalar};
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL};
use crate::output::Output;
use super::{HostResponse, ObservedReaderEncoding, ObservedReaderError,
    ObservedReaderSession, Observation, MAX_OBSERVED_READER_WINDOW_BYTES, check, decode, extent_sha256};

pub const MAX_OBSERVED_READER_NEEDLE_BYTES: usize = 4096;
pub const MAX_OBSERVED_READER_MATCHES: usize = 256;
pub const MAX_OBSERVED_READER_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_OBSERVED_READER_SEARCH_CALLS: u64 = 1_048_576;
pub const MAX_OBSERVED_READER_OCCURRENCE_BYTES: usize = MAX_IDENTIFIER_BYTES;

/// Whole-token occurrences are exact text candidates, including comments and
/// literals, under the shared conservative token policy. They are not compiler
/// references. Text mode preserves the existing literal/lowercase semantics.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ObservedReaderSearchKind { Text, Occurrences }
impl ObservedReaderSearchKind {
    pub const fn requested(self) -> &'static str {
        match self { Self::Text => "text", Self::Occurrences => "occurrences" }
    }
    pub const fn realized(self, case_sensitive: bool) -> &'static str {
        match self {
            Self::Occurrences => "whole-token-occurrences",
            Self::Text if case_sensitive => "exact-decoded-text",
            Self::Text => "unicode-lowercase-text",
        }
    }
}

pub(super) struct AcceptedSearch {
    request: CaptureRequest,
    generation: QueryGeneration,
    encoding: ObservedReaderEncoding,
    observed_length: ByteLength,
    final_length: Option<ByteLength>,
    consistency: ExtentConsistency,
    state: StreamReadState,
    io: StreamReadStats,
    matches_seen: u64,
    count_all_matches: bool,
    kind: ObservedReaderSearchKind,
    case_sensitive: bool,
    unsupported_at: Option<ByteOffset>,
    max_matches: usize,
    max_scan_bytes: u64,
    literal_text: String,
    literal_bytes: Vec<u8>,
    sha256: Option<Sha256Digest>,
    witnesses: Vec<SearchWitness>,
    hits: Vec<StreamingHit>,
    _lease: ResourceLease,
}

struct SearchWitness { text: String, bytes: Vec<u8>, sha256: Sha256Digest }

/// A verified hit selection belongs to its accepted immutable page. Replacing
/// the current query cannot erase the provenance needed for history replay.
#[derive(Clone, Copy)]
pub(super) struct FindHit {
    kind: ObservedReaderSearchKind,
    case_sensitive: bool,
    generation: u64,
    search_observation: u64,
    hit_index: usize,
    occurrence: u64,
    original_range: ByteRange,
    decoded_start: u64,
    decoded_end: u64,
}

impl ObservedReaderSession {
    /// Native Find naming for the existing search capability. Both entry
    /// points share one accepted query, the same limits and zero-limit probes.
    pub fn find(&mut self, generation: u64, needle: &str, encoding: ObservedReaderEncoding,
        max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        self.search(generation, needle, encoding, max_matches, max_scan_bytes, canceled)
    }

    /// Search the same explicitly opened file from byte zero, under independent
    /// byte/call/result limits. Exact UTF-8/UTF-16 semantics and overlapping hits
    /// come from FileSearch. Whole-file completion is an observed sequence with
    /// matching metadata, never an atomic snapshot or rereadable old context.
    ///
    /// A newer attempt consumes its generation and a fresh source observation,
    /// including on cancellation/failure. The previous accepted query remains
    /// usable until a replacement receipt passes its final delivery gate.
    pub fn search(&mut self, generation: u64, needle: &str, encoding: ObservedReaderEncoding,
        max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        self.search_inner(generation, needle, encoding, ObservedReaderSearchKind::Text,
            max_matches, max_scan_bytes, true, false, canceled)
    }

    /// Count every exact occurrence within the same byte/read-call budgets while
    /// retaining at most max_matches rows and one literal witness.
    pub fn search_counted(&mut self, generation: u64, needle: &str, encoding: ObservedReaderEncoding,
        max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        self.search_inner(generation, needle, encoding, ObservedReaderSearchKind::Text,
            max_matches, max_scan_bytes, true, true, canceled)
    }

    /// Search with an explicit frozen matching/count policy. Lowercase matching
    /// uses the shared Unicode scalar engine and preserves each original witness.
    pub fn find_matching(&mut self, generation: u64, needle: &str, encoding: ObservedReaderEncoding,
        case_sensitive: bool, count_all_matches: bool, max_matches: usize, max_scan_bytes: u64,
        canceled: impl FnMut() -> bool) -> Result<HostResponse, ObservedReaderError> {
        self.find_with_kind(generation, needle, encoding, ObservedReaderSearchKind::Text,
            case_sensitive, count_all_matches, max_matches, max_scan_bytes, canceled)
    }

    /// Search literal text or exact whole-token occurrences with one frozen
    /// mode/count policy. Occurrences use the shared streaming token cursor,
    /// including UTF-16 and read-chunk boundaries, without retaining the file.
    /// Only case-sensitive occurrences are supported; incompatible options and
    /// invalid token names fail before source I/O and preserve accepted results.
    pub fn find_with_kind(&mut self, generation: u64, needle: &str, encoding: ObservedReaderEncoding,
        kind: ObservedReaderSearchKind, case_sensitive: bool, count_all_matches: bool,
        max_matches: usize, max_scan_bytes: u64, canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        self.search_inner(generation, needle, encoding, kind, max_matches, max_scan_bytes,
            case_sensitive, count_all_matches, canceled)
    }

    fn search_inner(&mut self, generation: u64, needle: &str, encoding: ObservedReaderEncoding,
        kind: ObservedReaderSearchKind, max_matches: usize, max_scan_bytes: u64,
        case_sensitive: bool, count_all_matches: bool,
        mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        if generation == 0 || generation <= self.last_query_attempt { return Err(ObservedReaderError::StaleQuery); }
        self.last_query_attempt = generation;
        self.last_observation = self.last_observation.checked_add(1).ok_or(ObservedReaderError::IdentityExhausted)?;
        let revision = SourceRevision::new(self.owner(), self.last_observation)
            .map_err(|_| ObservedReaderError::IdentityExhausted)?;
        let generation = QueryGeneration::new(self.owner(), generation)
            .map_err(|_| ObservedReaderError::IdentityExhausted)?;
        check(&mut canceled)?;
        if needle.is_empty() || needle.len() > MAX_OBSERVED_READER_NEEDLE_BYTES
            || max_matches > MAX_OBSERVED_READER_MATCHES || max_scan_bytes > MAX_OBSERVED_READER_SCAN_BYTES
            || (kind == ObservedReaderSearchKind::Occurrences && !case_sensitive) {
            return Err(ObservedReaderError::InvalidLimits);
        }
        // Reserve representable cumulative accounting before any payload read.
        // A canceled/failed attempt still spends every actual native read call.
        self.source_bytes_read.checked_add(max_scan_bytes).ok_or(ObservedReaderError::IdentityExhausted)?;
        self.read_calls.checked_add(MAX_OBSERVED_READER_SEARCH_CALLS).ok_or(ObservedReaderError::IdentityExhausted)?;
        let [pattern_id, worker_id, result_id] = self.allocations()?;
        let pattern = match kind {
            ObservedReaderSearchKind::Text =>
                StreamingNeedle::text_matching(self.owner(), needle, case_sensitive, &self.budget, pattern_id),
            ObservedReaderSearchKind::Occurrences =>
                StreamingNeedle::identifier(self.owner(), needle, &self.budget, pattern_id),
        }.map_err(AppError::from)?;
        let request = CaptureRequest::new(self.reader.file_id(), revision)?;
        let options = StreamReadOptions { generation, max_matches, max_bytes: max_scan_bytes,
            max_read_calls: MAX_OBSERVED_READER_SEARCH_CALLS,
            encoding: Some(match encoding {
                ObservedReaderEncoding::Utf8 => DetectedEncoding::Utf8 { has_bom: true },
                ObservedReaderEncoding::Utf16Le => DetectedEncoding::Utf16Le,
                ObservedReaderEncoding::Utf16Be => DetectedEncoding::Utf16Be,
            }) };
        check(&mut canceled)?;
        let file = self.search_file.try_clone().map_err(|_| AppError::Io)?;
        let (scanned, io) = scan(file, request, &pattern, options, &self.budget, worker_id,
            count_all_matches, &mut canceled);
        self.source_bytes_read += io.bytes_read; // Headroom was admitted above.
        self.read_calls += io.read_calls;
        let report = scanned?;
        let candidate = Arc::new(AcceptedSearch::retain(&report, encoding, max_matches, max_scan_bytes,
            &self.budget, result_id, &mut canceled)?);
        // The temporary report/compiled query need not overlap output encoding.
        // The retained occurrence table and witness own their independent lease.
        drop(report);
        drop(pattern);
        let response = self.search_response(&candidate, &mut canceled)?;
        // No fallible operation/cancellation poll can publish a partial query.
        self.query = Some(candidate);
        Ok(response)
    }

    /// Resolve a retained hit without stat, path resolution or source I/O. Only
    /// the exact literal bytes are available; current surrounding source must
    /// be requested separately through read_window and receives a NEW revision.
    pub fn search_hit(&mut self, generation: u64, position: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ObservedReaderError> {
        check(&mut canceled)?;
        let query = self.query.as_ref().filter(|query| query.generation.get() == generation)
            .ok_or(ObservedReaderError::StaleQuery)?.clone();
        let hit = query.hits.get(position).ok_or(ObservedReaderError::MissingHit)?;
        let (text, bytes, digest) = query.witness(position)?;
        let mut out = self.output("search-hit")?;
        encode_query(&mut out, &query)?;
        out.literal(",\"index\":")?; out.integer(position as u64)?;
        out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
        out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"text\":")?; out.quoted(text)?;
        out.literal(",\"original_hex\":")?; out.hex(bytes)?;
        out.literal(",\"witness_sha256\":")?;
        out.quoted(&digest.to_hex())?;
        out.literal(",\"additional_source_bytes_read\":\"0\",\"additional_read_calls\":\"0\"}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    /// Read surrounding current bytes as a NEW observation and verify the
    /// retained literal against both original bytes and the shared decoder.
    /// Unlike search_hit, this operation performs bounded source I/O. A changed
    /// witness, invalid decoded selection or cancellation cannot evict history.
    pub fn observe_hit(&mut self, generation: u64, hit_index: usize, context_bytes: usize,
        window_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        let revision = self.next_revision()?;
        check(&mut canceled)?;
        let query = self.query.as_ref().filter(|query| query.generation.get() == generation)
            .ok_or(ObservedReaderError::StaleQuery)?.clone();
        let hit = *query.hits.get(hit_index).ok_or(ObservedReaderError::MissingHit)?;
        let (witness_text, witness, _) = query.witness(hit_index)?;
        let witness_bytes = witness.len();
        // Scalar/CRLF floors can move both visible edges. Occurrences also
        // require both adjacent scalars (or a qualified input boundary); zero
        // requested context must not skip whole-token revalidation.
        let occurrences = query.kind == ObservedReaderSearchKind::Occurrences;
        let slack = if occurrences { 24 } else { 8 };
        if !(4..=MAX_OBSERVED_READER_WINDOW_BYTES).contains(&window_bytes)
            || context_bytes > MAX_OBSERVED_READER_WINDOW_BYTES
            || witness_bytes.checked_add(slack).is_none_or(|need| need > window_bytes) {
            return Err(ObservedReaderError::InvalidLimits);
        }
        let prefix = if occurrences {
            context_bytes.max(8).min(window_bytes - witness_bytes - 16)
        } else {
            context_bytes.min(window_bytes - witness_bytes - 8)
        } as u64;
        let offset = hit.original_range().start().get().saturating_sub(prefix);
        let mut candidate = self.capture_window(revision, offset, window_bytes, query.encoding, &mut canceled)?;
        let [decode_id, clipped_decode_id] = self.allocations()?;
        let text = decode(&candidate, &self.budget, [decode_id, clipped_decode_id], &mut canceled)?;
        let original = hit.original_range();
        if candidate.extent.range_bytes(original).ok() != Some(witness) {
            return Err(ObservedReaderError::SearchHitChanged);
        }
        let decoded = text.source_to_text(original).map_err(|_| ObservedReaderError::SearchHitChanged)?;
        let selection = text.text_selection(decoded).map_err(|_| ObservedReaderError::SearchHitChanged)?;
        if selection.original_range != original || selection.original_bytes != witness
            || selection.text != witness_text || selection.contains_replacements {
            return Err(ObservedReaderError::SearchHitChanged);
        }
        if occurrences {
            let start = usize::try_from(decoded.start().get()).map_err(|_| ObservedReaderError::InvalidRange)?;
            let end = usize::try_from(decoded.end().get()).map_err(|_| ObservedReaderError::InvalidRange)?;
            verify_token_context(&candidate, &text, start, end)?;
        }
        candidate.find_hit = Some(FindHit { kind: query.kind, case_sensitive: query.case_sensitive,
            generation, search_observation: query.request.revision().get(),
            hit_index, occurrence: hit.occurrence_id(), original_range: original,
            decoded_start: decoded.start().get(), decoded_end: decoded.end().get() });
        let response = self.page_response_decoded("observe-hit", &candidate, &text, true, &mut canceled)?;
        self.accept_observation(candidate);
        Ok(response)
    }

    fn search_response(&mut self, query: &AcceptedSearch, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        check(canceled)?;
        let mut out = self.output("find")?;
        encode_query(&mut out, query)?;
        out.literal(",\"literal_text\":")?;
        out.quoted(&query.literal_text)?;
        // Additive native aliases preserve every existing search receipt field.
        out.literal(",\"search_kind\":\"continuous-open-file-observation\",\"needle\":")?;
        out.quoted(&query.literal_text)?;
        out.literal(",\"literal_original_hex\":")?;
        if query.hits.is_empty() || !query.case_sensitive { out.literal("null")?; } else { out.hex(&query.literal_bytes)?; }
        out.literal(",\"literal_sha256\":")?;
        if let Some(digest) = query.sha256 { out.quoted(&digest.to_hex())?; } else { out.literal("null")?; }
        for (key, value) in [("max_matches", query.max_matches as u64), ("max_scan_bytes", query.max_scan_bytes),
            ("max_read_calls", MAX_OBSERVED_READER_SEARCH_CALLS), ("payload_bytes_read", query.io.bytes_read),
            ("scanned_input_bytes", query.io.scanned_bytes), ("read_calls", query.io.read_calls),
            ("source_bytes_read", query.io.bytes_read), ("scanned_bytes", query.io.scanned_bytes),
            ("peak_input_buffer_bytes", query.io.peak_buffer_bytes as u64), ("matches_seen", query.matches_seen),
            ("retained_hits", query.hits.len() as u64), ("retained_witness_bytes", query.witness_bytes() as u64),
            ("additional_source_bytes_read", query.io.bytes_read),
            ("additional_read_calls", query.io.read_calls)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"truncated\":")?; out.boolean(query.matches_seen > query.hits.len() as u64)?;
        out.literal(",\"unsupported_at\":")?;
        if let Some(offset) = query.unsupported_at { out.integer(offset.get())?; } else { out.literal("null")?; }
        out.literal(",\"hits\":[")?;
        for (index, hit) in query.hits.iter().enumerate() {
            check(canceled)?;
            if index > 0 { out.literal(",")?; }
            out.literal("{\"index\":")?; out.integer(index as u64)?;
            out.literal(",\"hit_index\":")?; out.integer(index as u64)?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
            if !query.case_sensitive {
                out.literal(",\"witness_sha256\":")?; out.quoted(&query.witness(index)?.2.to_hex())?;
            }
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish_output(out, if query.complete() && query.matches_seen == query.hits.len() as u64 {
            EXIT_OK
        } else { EXIT_PARTIAL }, canceled)
    }
}

/// Compare adjacent decoded scalars from this same new observation. A matching
/// witness can cease to be a whole token after a neighboring edit; a clipped
/// window or a byte limit cannot stand in for a delimiter or observed EOF.
fn verify_token_context(page: &Observation, text: &fcb::search::ExtentText, start: usize, end: usize)
    -> Result<(), ObservedReaderError> {
    let before = text.text().get(..start).ok_or(ObservedReaderError::SearchHitChanged)?;
    let after = text.text().get(end..).ok_or(ObservedReaderError::SearchHitChanged)?;
    match before.chars().next_back() {
        Some(scalar) if !token_scalar(scalar) => {}
        None if page.plan.visible.start().get() == 0 => {}
        _ => return Err(ObservedReaderError::SearchHitChanged),
    }
    match after.chars().next() {
        Some(scalar) if !token_scalar(scalar) => {}
        None if text.range().end().get() == page.extent.observed_length().get()
            && page.extent.request_filled()
            && page.extent.consistency() == ExtentConsistency::UnchangedMetadata => {}
        _ => return Err(ObservedReaderError::SearchHitChanged),
    }
    Ok(())
}

pub(super) fn encode_find_hit(out: &mut Output, hit: Option<FindHit>) -> Result<(), ObservedReaderError> {
    out.literal(",\"find_hit\":")?;
    let Some(hit) = hit else { out.literal("null")?; return Ok(()); };
    out.literal("{\"query_generation\":")?; out.integer(hit.generation)?;
    out.literal(",\"search_observation\":")?; out.integer(hit.search_observation)?;
    out.literal(",\"hit_index\":")?; out.integer(hit.hit_index as u64)?;
    out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence)?;
    out.literal(",\"original_range\":")?; out.range(hit.original_range)?;
    out.literal(",\"decoded_utf8_range\":{\"start\":")?; out.integer(hit.decoded_start)?;
    out.literal(",\"end\":")?; out.integer(hit.decoded_end)?;
    out.literal("},\"witness_verified\":true,\"verification\":\"equal-original-bytes-in-new-observation\"")?;
    encode_mode(out, hit.kind, hit.case_sensitive)?;
    out.literal(",\"token_boundaries_verified\":")?;
    out.boolean(hit.kind == ObservedReaderSearchKind::Occurrences)?;
    out.literal("}")?;
    Ok(())
}

pub(super) fn encode_search_info(out: &mut Output, query: Option<&AcceptedSearch>)
    -> Result<(), ObservedReaderError> {
    // Accepted metadata stays unchanged across failed attempts and read-only
    // witness activation, preserving the existing info/no-I/O contract.
    out.literal(",\"accepted_search_generation\":")?;
    if let Some(query) = query { out.integer(query.generation.get())?; } else { out.literal("null")?; }
    out.literal(",\"accepted_search_observation\":")?;
    if let Some(query) = query { out.integer(query.request.revision().get())?; } else { out.literal("null")?; }
    out.literal(",\"retained_search_hits\":")?; out.integer(query.map_or(0, |query| query.hits.len() as u64))?;
    out.literal(",\"retained_search_witness_bytes\":")?; out.integer(query.map_or(0, |query| query.witness_bytes() as u64))?;
    Ok(())
}

impl AcceptedSearch {
    fn retain(report: &FileSearchReport<'_>, encoding: ObservedReaderEncoding,
        max_matches: usize, max_scan_bytes: u64, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, ObservedReaderError> {
        check(canceled)?;
        let search = report.search();
        let text = search.text_literal().ok_or(ObservedReaderError::InvalidRange)?;
        let (kind, case_sensitive) = match search.mode() {
            StreamingMode::ExactText => (ObservedReaderSearchKind::Text, true),
            StreamingMode::LowercaseText => (ObservedReaderSearchKind::Text, false),
            StreamingMode::Identifier => (ObservedReaderSearchKind::Occurrences, true),
            StreamingMode::OriginalBytes => return Err(ObservedReaderError::InvalidRange),
        };
        let witness = if search.hits().is_empty() || !case_sensitive { &[][..] }
            else { search.witness_bytes(0).map_err(AppError::from)? };
        let mut per_hit_charge = 0usize;
        if !case_sensitive {
            for (ordinal, _) in search.hits().iter().enumerate() {
                per_hit_charge = per_hit_charge.checked_add(size_of::<SearchWitness>())
                    .and_then(|n| n.checked_add(search.witness_bytes(ordinal).ok()?.len()))
                    .and_then(|n| n.checked_add(search.matched_text(ordinal).ok()?.len())).ok_or(AppError::Admission)?;
            }
        }
        let charge = search.hits().len().checked_mul(size_of::<StreamingHit>())
            .and_then(|n| n.checked_add(text.len()))
            .and_then(|n| n.checked_add(witness.len()))
            .and_then(|n| n.checked_add(per_hit_charge))
            .and_then(|n| n.checked_add(size_of::<Self>() + 2 * size_of::<usize>())).ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(search.request().file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut literal_text = String::new();
        literal_text.try_reserve_exact(text.len()).map_err(|_| AppError::Admission)?;
        if literal_text.capacity() > text.len() { return Err(AppError::Admission.into()); }
        literal_text.push_str(text);
        let mut literal_bytes = reserve(witness.len())?;
        literal_bytes.extend_from_slice(witness);
        let mut hits = reserve(search.hits().len())?;
        hits.extend_from_slice(search.hits());
        let sha256 = if hits.is_empty() || !case_sensitive { None } else { Some(extent_sha256(witness, canceled)?) };
        let mut witnesses = reserve(if case_sensitive { 0 } else { hits.len() })?;
        if !case_sensitive {
            for (ordinal, _) in hits.iter().enumerate() {
                check(canceled)?;
                let bytes = search.witness_bytes(ordinal).map_err(AppError::from)?;
                let spelling = search.matched_text(ordinal).map_err(AppError::from)?;
                let mut text = String::new();
                text.try_reserve_exact(spelling.len()).map_err(|_| AppError::Admission)?;
                if text.capacity() > spelling.len() { return Err(AppError::Admission.into()); }
                text.push_str(spelling);
                let mut retained = reserve(bytes.len())?;
                retained.extend_from_slice(bytes);
                witnesses.push(SearchWitness { text, bytes: retained, sha256: extent_sha256(bytes, canceled)? });
            }
        }
        check(canceled)?;
        Ok(Self { request: search.request(), generation: search.generation(), encoding,
            observed_length: search.observed_length(), final_length: report.final_length(),
            consistency: report.consistency(), state: search.state(), io: search.stats(),
            matches_seen: search.matches_seen(), count_all_matches: search.count_all_matches(), kind, case_sensitive,
            unsupported_at: search.unsupported_at(), max_matches, max_scan_bytes,
            literal_text, literal_bytes, sha256, witnesses, hits, _lease: lease })
    }
    fn witness(&self, ordinal: usize) -> Result<(&str, &[u8], Sha256Digest), ObservedReaderError> {
        self.hits.get(ordinal).ok_or(ObservedReaderError::MissingHit)?;
        if self.case_sensitive {
            Ok((&self.literal_text, &self.literal_bytes, self.sha256.ok_or(ObservedReaderError::MissingHit)?))
        } else {
            let witness = self.witnesses.get(ordinal).ok_or(ObservedReaderError::MissingHit)?;
            Ok((&witness.text, &witness.bytes, witness.sha256))
        }
    }
    fn witness_bytes(&self) -> usize {
        if self.case_sensitive { self.literal_bytes.len() } else { self.witnesses.iter().map(|witness| witness.bytes.len()).sum() }
    }
    fn complete(&self) -> bool {
        self.state == StreamReadState::Complete && self.consistency == ExtentConsistency::UnchangedMetadata
    }
}

fn scan<'needle>(file: File, request: CaptureRequest, needle: &'needle StreamingNeedle,
    options: StreamReadOptions, budget: &ResourceBudget, allocation: ResourceAllocationId,
    count_all_matches: bool, canceled: &mut impl FnMut() -> bool)
    -> (Result<FileSearchReport<'needle>, ObservedReaderError>, StreamReadStats) {
    let mut io = StreamReadStats::default();
    let result = (|| {
        check(canceled)?;
        let mut scan = if count_all_matches {
            FileSearch::new_counting(file, request, needle, options, budget, allocation)
        } else {
            FileSearch::new(file, request, needle, options, budget, allocation)
        }.map_err(AppError::from)?;
        while scan.state() == StreamReadState::Pending {
            let step = scan.step(StreamReadStep::default(), options.generation, &mut *canceled);
            io = scan.stats(); // Include attempted I/O even if this step fails.
            step.map_err(AppError::from)?;
        }
        if scan.state() == StreamReadState::Canceled { return Err(ObservedReaderError::Canceled); }
        check(canceled)?;
        if let StreamReadState::Failed(error) = scan.state() { return Err(AppError::from(error).into()); }
        Ok(scan.finish().map_err(AppError::from)?)
    })();
    (result, io)
}

fn encode_mode(out: &mut Output, kind: ObservedReaderSearchKind, case_sensitive: bool)
    -> Result<(), ObservedReaderError> {
    out.literal(",\"mode\":")?; out.quoted(kind.realized(case_sensitive))?;
    out.literal(",\"requested_mode\":")?; out.quoted(kind.requested())?;
    out.literal(",\"realized_mode\":")?; out.quoted(kind.realized(case_sensitive))?;
    out.literal(",\"case_sensitive\":")?; out.boolean(case_sensitive)?;
    if kind == ObservedReaderSearchKind::Occurrences {
        out.literal(",\"evidence_level\":\"whole-token-text-candidate\",\"includes_comments_and_literals\":true,\"compiler_resolved\":false,\"semantic_complete\":false")?;
    }
    Ok(())
}

fn encode_query(out: &mut Output, query: &AcceptedSearch) -> Result<(), ObservedReaderError> {
    encode_mode(out, query.kind, query.case_sensitive)?;
    out.literal(",\"query_generation\":")?; out.integer(query.generation.get())?;
    out.literal(",\"search_observation\":")?; out.integer(query.request.revision().get())?;
    out.literal(",\"encoding\":")?; out.quoted(query.encoding.code())?;
    out.literal(",\"encoding_basis\":\"explicit-for-this-search\",\"retention\":\"literal-witnesses-only\",\"observed_length\":")?;
    out.integer(query.observed_length.get())?;
    out.literal(",\"final_observed_length\":")?;
    if let Some(length) = query.final_length { out.integer(length.get())?; } else { out.literal("null")?; }
    out.literal(",\"consistency\":")?;
    out.quoted(match query.consistency {
        ExtentConsistency::UnchangedMetadata => "unchanged-metadata-not-atomic",
        ExtentConsistency::ChangedDuringRead => "changed-during-read",
        ExtentConsistency::ShortRead => "short-read",
        _ => "metadata-unavailable",
    })?;
    out.literal(",\"state\":")?; out.quoted(query.state.code())?;
    out.literal(",\"input_complete\":")?; out.boolean(query.state == StreamReadState::Complete)?;
    out.literal(",\"search_complete\":")?; out.boolean(query.complete())?;
    out.literal(",\"count_all_matches\":")?; out.boolean(query.count_all_matches)?;
    out.literal(",\"match_count_complete\":")?; out.boolean(query.complete())?;
    Ok(())
}

fn reserve<T>(capacity: usize) -> Result<Vec<T>, ObservedReaderError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if values.capacity() > capacity { return Err(AppError::Admission.into()); }
    Ok(values)
}
