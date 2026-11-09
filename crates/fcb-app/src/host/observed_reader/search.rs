#![forbid(unsafe_code)]

//! Native-host composition of the public, bounded FileSearch engine. The last
//! accepted query owns a small occurrence table and ONE exact literal witness;
//! no whole-file capture, source spool, new matcher or inferred context exists.

use std::{fs::File, mem::size_of, sync::Arc};
use fcb::{ByteLength, ByteOffset, QueryGeneration, SourceRevision};
use fcb::search::{CaptureRequest, DetectedEncoding, ExtentConsistency, FileSearch,
    FileSearchReport, ResourceAllocationId, ResourceBudget, StreamReadOptions,
    StreamReadState, StreamReadStats, StreamReadStep, StreamingHit, StreamingNeedle};
use fcb::store::Sha256Digest;
use fcb_core::ResourceLease;
use crate::{AppError, EXIT_OK, EXIT_PARTIAL};
use crate::output::Output;
use super::{HostResponse, ObservedReaderEncoding, ObservedReaderError,
    ObservedReaderSession, check, extent_sha256};

pub const MAX_OBSERVED_READER_NEEDLE_BYTES: usize = 4096;
pub const MAX_OBSERVED_READER_MATCHES: usize = 256;
pub const MAX_OBSERVED_READER_SCAN_BYTES: u64 = 4 * 1024 * 1024 * 1024;
pub const MAX_OBSERVED_READER_SEARCH_CALLS: u64 = 1_048_576;

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
    unsupported_at: Option<ByteOffset>,
    max_matches: usize,
    max_scan_bytes: u64,
    literal_text: String,
    literal_bytes: Vec<u8>,
    sha256: Option<Sha256Digest>,
    hits: Vec<StreamingHit>,
    _lease: ResourceLease,
}

impl ObservedReaderSession {
    /// Search the same explicitly opened file from byte zero, under independent
    /// byte/call/result limits. Exact UTF-8/UTF-16 semantics and overlapping hits
    /// come from FileSearch. Whole-file completion is an observed sequence with
    /// matching metadata, never an atomic snapshot or rereadable old context.
    ///
    /// A newer attempt consumes its generation and a fresh source observation,
    /// including on cancellation/failure. The previous accepted query remains
    /// usable until a replacement receipt passes its final delivery gate.
    pub fn search(&mut self, generation: u64, needle: &str, encoding: ObservedReaderEncoding,
        max_matches: usize, max_scan_bytes: u64, mut canceled: impl FnMut() -> bool)
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
            || max_matches > MAX_OBSERVED_READER_MATCHES || max_scan_bytes > MAX_OBSERVED_READER_SCAN_BYTES {
            return Err(ObservedReaderError::InvalidLimits);
        }
        // Reserve representable cumulative accounting before any payload read.
        // A canceled/failed attempt still spends every actual native read call.
        self.source_bytes_read.checked_add(max_scan_bytes).ok_or(ObservedReaderError::IdentityExhausted)?;
        self.read_calls.checked_add(MAX_OBSERVED_READER_SEARCH_CALLS).ok_or(ObservedReaderError::IdentityExhausted)?;
        let [pattern_id, worker_id, result_id] = self.allocations()?;
        let pattern = StreamingNeedle::text(self.owner(), needle, &self.budget, pattern_id).map_err(AppError::from)?;
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
        let (scanned, io) = scan(file, request, &pattern, options, &self.budget, worker_id, &mut canceled);
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
        let mut out = self.output("search-hit")?;
        encode_query(&mut out, &query)?;
        out.literal(",\"index\":")?; out.integer(position as u64)?;
        out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
        out.literal(",\"original_range\":")?; out.range(hit.original_range())?;
        out.literal(",\"text\":")?; out.quoted(&query.literal_text)?;
        out.literal(",\"original_hex\":")?; out.hex(&query.literal_bytes)?;
        out.literal(",\"witness_sha256\":")?;
        out.quoted(&query.sha256.ok_or(ObservedReaderError::MissingHit)?.to_hex())?;
        out.literal(",\"additional_source_bytes_read\":\"0\",\"additional_read_calls\":\"0\"}\n")?;
        self.finish_output(out, EXIT_OK, &mut canceled)
    }

    fn search_response(&mut self, query: &AcceptedSearch, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, ObservedReaderError> {
        check(canceled)?;
        let mut out = self.output("find")?;
        encode_query(&mut out, query)?;
        out.literal(",\"mode\":\"exact-decoded-text\",\"case_sensitive\":true,\"literal_text\":")?;
        out.quoted(&query.literal_text)?;
        out.literal(",\"literal_original_hex\":")?;
        if query.hits.is_empty() { out.literal("null")?; } else { out.hex(&query.literal_bytes)?; }
        out.literal(",\"literal_sha256\":")?;
        if let Some(digest) = query.sha256 { out.quoted(&digest.to_hex())?; } else { out.literal("null")?; }
        for (key, value) in [("max_matches", query.max_matches as u64), ("max_scan_bytes", query.max_scan_bytes),
            ("max_read_calls", MAX_OBSERVED_READER_SEARCH_CALLS), ("payload_bytes_read", query.io.bytes_read),
            ("scanned_input_bytes", query.io.scanned_bytes), ("read_calls", query.io.read_calls),
            ("peak_input_buffer_bytes", query.io.peak_buffer_bytes as u64), ("matches_seen", query.matches_seen),
            ("retained_hits", query.hits.len() as u64), ("additional_source_bytes_read", query.io.bytes_read),
            ("additional_read_calls", query.io.read_calls)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"truncated\":")?; out.boolean(query.state == StreamReadState::Truncated)?;
        out.literal(",\"unsupported_at\":")?;
        if let Some(offset) = query.unsupported_at { out.integer(offset.get())?; } else { out.literal("null")?; }
        out.literal(",\"hits\":[")?;
        for (index, hit) in query.hits.iter().enumerate() {
            check(canceled)?;
            if index > 0 { out.literal(",")?; }
            out.literal("{\"index\":")?; out.integer(index as u64)?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
            out.literal(",\"original_range\":")?; out.range(hit.original_range())?; out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish_output(out, if query.complete() { EXIT_OK } else { EXIT_PARTIAL }, canceled)
    }
}

impl AcceptedSearch {
    fn retain(report: &FileSearchReport<'_>, encoding: ObservedReaderEncoding,
        max_matches: usize, max_scan_bytes: u64, budget: &ResourceBudget,
        allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, ObservedReaderError> {
        check(canceled)?;
        let search = report.search();
        let text = search.text_literal().ok_or(ObservedReaderError::InvalidRange)?;
        let witness = if search.hits().is_empty() { &[][..] } else { search.witness_bytes(0).map_err(AppError::from)? };
        let charge = search.hits().len().checked_mul(size_of::<StreamingHit>())
            .and_then(|n| n.checked_add(text.len()))
            .and_then(|n| n.checked_add(witness.len()))
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
        let sha256 = if hits.is_empty() { None } else { Some(extent_sha256(witness, canceled)?) };
        check(canceled)?;
        Ok(Self { request: search.request(), generation: search.generation(), encoding,
            observed_length: search.observed_length(), final_length: report.final_length(),
            consistency: report.consistency(), state: search.state(), io: search.stats(),
            matches_seen: search.matches_seen(), unsupported_at: search.unsupported_at(),
            max_matches, max_scan_bytes, literal_text, literal_bytes, sha256, hits, _lease: lease })
    }
    fn complete(&self) -> bool {
        self.state == StreamReadState::Complete && self.consistency == ExtentConsistency::UnchangedMetadata
    }
}

fn scan<'needle>(file: File, request: CaptureRequest, needle: &'needle StreamingNeedle,
    options: StreamReadOptions, budget: &ResourceBudget, allocation: ResourceAllocationId,
    canceled: &mut impl FnMut() -> bool)
    -> (Result<FileSearchReport<'needle>, ObservedReaderError>, StreamReadStats) {
    let mut io = StreamReadStats::default();
    let result = (|| {
        check(canceled)?;
        let mut scan = FileSearch::new(file, request, needle, options, budget, allocation).map_err(AppError::from)?;
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

fn encode_query(out: &mut Output, query: &AcceptedSearch) -> Result<(), ObservedReaderError> {
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
    Ok(())
}

fn reserve<T>(capacity: usize) -> Result<Vec<T>, ObservedReaderError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if values.capacity() > capacity { return Err(AppError::Admission.into()); }
    Ok(values)
}
