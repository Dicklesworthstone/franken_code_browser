#![forbid(unsafe_code)]

//! Additive retained-file Find controls. The shared decoded scanner owns both
//! matching and original-byte provenance. Only bounded hit ranges survive in
//! the reader; source spellings and wire witnesses are encoded from its capture.

use super::*;
use fcb::search::{DirectSourceScanner, QueryOptions, SearchCoverage, SearchMatch,
    SearchMode, UnicodeNormalization};

/// Same Unicode lowercase comparison as workspace text search, without locale,
/// canonical normalization or full case folding. Counting does not retain more
/// rows and does not turn a byte-limited/unsupported scan into a complete one.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ReaderFindOptions {
    pub case_sensitive: bool,
    pub count_all_matches: bool,
}
impl Default for ReaderFindOptions {
    fn default() -> Self { Self { case_sensitive: true, count_all_matches: false } }
}

const SCAN_SCRATCH_BYTES: usize = 8 * 1024 * 1024;
// Leave ample space for the header/query and native decoding under the existing
// 4 MiB reader response admission. A long needle may retain fewer rows; the
// effective row limit and omissions are explicit in every options response.
const ROW_WIRE_BYTES: usize = 2 * 1024 * 1024;

impl ReaderSession {
    pub fn search_with_options(&mut self, generation: u64, needle: &str,
        max_matches: usize, max_scan_bytes: u64, options: ReaderFindOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if options == ReaderFindOptions::default() {
            return self.search(generation, needle, max_matches, max_scan_bytes, canceled);
        }
        if generation == 0 || generation <= self.last_query_attempt {
            return Err(ReaderSessionError::StaleQuery);
        }
        if needle.is_empty() || needle.len() > MAX_READER_NEEDLE_BYTES
            || max_matches > MAX_READER_MATCHES || (max_matches == 0 && !options.count_all_matches) {
            return Err(ReaderSessionError::InvalidLimits);
        }
        self.last_query_attempt = generation;
        check(&mut canceled)?;

        // One matching unit may cover a complete original scalar. Lowercasing
        // can expand scalars; query UTF-8 byte length is not a source-byte bound.
        let units = needle.chars().try_fold(0usize, |n, ch|
            n.checked_add(if options.case_sensitive { 1 } else { ch.to_lowercase().count() }))
            .ok_or(AppError::Admission)?;
        if units > MAX_READER_NEEDLE_BYTES { return Err(ReaderSessionError::InvalidLimits); }
        let witness_capacity = units.checked_mul(4).ok_or(AppError::Admission)?;
        // JSON text may expand sixfold, original-byte hex twofold. The range,
        // identity and field names have an independent fixed allowance.
        let row_bytes = witness_capacity.checked_mul(8).and_then(|n| n.checked_add(256))
            .ok_or(AppError::Admission)?;
        let retained_limit = max_matches.min(ROW_WIRE_BYTES / row_bytes);
        let [scratch_id, result_id] = self.allocations()?;
        let scratch_bytes = retained_limit.max(4).checked_mul(2)
            .and_then(|n| n.checked_mul(size_of::<SearchMatch>() + witness_capacity))
            .and_then(|n| n.checked_add(SCAN_SCRATCH_BYTES)).ok_or(AppError::Admission)?;
        let _scratch = self.budget.try_reserve_managed(self.owner(), scratch_id,
            ByteLength::new(scratch_bytes as u64)).map_err(|_| AppError::Admission)?;
        let retained_bytes = retained_limit.checked_mul(size_of::<ByteRange>())
            .and_then(|n| n.checked_add(size_of::<AcceptedQuery>())).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.owner(), result_id,
            ByteLength::new(retained_bytes as u64)).map_err(|_| AppError::Admission)?;
        let mut hits = reserve(retained_limit)?;
        let query_generation = QueryGeneration::new(self.owner(), generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let mut query = QueryOptions::new(query_generation);
        query.mode = SearchMode::DecodedText { case_sensitive: options.case_sensitive,
            normalization: UnicodeNormalization::Exact };
        query.max_matches = retained_limit;
        query.max_bytes_scanned = Some(max_scan_bytes);
        query.declared_encoding = Some(self.encoding);
        let report = if options.count_all_matches {
            DirectSourceScanner::scan_complete_capture_counting_with_cancel(
                &self.capture, needle, &query, &mut canceled)?
        } else {
            DirectSourceScanner::scan_complete_capture_with_cancel(
                &self.capture, needle, &query, &mut canceled)?
        };
        check(&mut canceled)?;
        if report.coverage == SearchCoverage::CanceledEarly { return Err(ReaderSessionError::Canceled); }
        let matches_seen = u64::try_from(report.total_matches_counted).map_err(|_| AppError::Admission)?;
        // Bound the receipt independently of retained rows. This is also the
        // native admission contract for the exact/lowercase repertoire here.
        if matches_seen > self.length() || report.matches.len() > retained_limit {
            return Err(AppError::Admission.into());
        }
        let complete = report.is_complete();
        let state = if !report.unsupported_files.is_empty() { "unsupported-text" } else {
            match report.coverage {
                SearchCoverage::Exhaustive => "complete-observed-input",
                SearchCoverage::TruncatedAtLimit { .. } => "match-limit",
                SearchCoverage::BudgetExhausted { .. } => "byte-limit",
                SearchCoverage::CanceledEarly => return Err(ReaderSessionError::Canceled),
            }
        };
        let mut out = self.output("find")?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"mode\":")?;
        out.quoted(if options.case_sensitive { "exact-decoded-literal" } else { "unicode-lowercase-literal" })?;
        out.literal(",\"needle\":")?; out.quoted(needle)?;
        out.literal(",\"case_sensitive\":")?; out.boolean(options.case_sensitive)?;
        out.literal(",\"count_all_matches\":")?; out.boolean(options.count_all_matches)?;
        out.literal(",\"retained_limit\":")?; out.integer(retained_limit as u64)?;
        out.literal(",\"state\":")?; out.quoted(state)?;
        out.literal(",\"search_complete\":")?; out.boolean(complete)?;
        out.literal(",\"match_count_complete\":")?; out.boolean(complete)?;
        out.literal(",\"rows_truncated\":")?; out.boolean(report.total_matches_counted > report.matches.len())?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.scanned_bytes)?;
        out.literal(",\"matches_seen\":")?; out.integer(matches_seen)?;
        out.literal(",\"retained_hits\":")?; out.integer(report.matches.len() as u64)?;
        out.literal(",\"literal_original_hex\":")?;
        if options.case_sensitive {
            if let Some(hit) = report.matches.first() {
                let range = hit.original_byte_range;
                let original = self.capture.bytes().get(range.start().get() as usize..range.end().get() as usize)
                    .ok_or(AppError::Admission)?;
                out.hex(original)?;
            } else { out.literal("null")?; }
        } else { out.literal("null")?; }
        out.literal(",\"unsupported_at\":null,\"hits\":[")?;
        for (index, hit) in report.matches.iter().enumerate() {
            check(&mut canceled)?;
            let range = hit.original_byte_range;
            if range.end().get() > self.length() || range.len().get() > witness_capacity as u64
                || hit.matched_text.capacity() > witness_capacity {
                return Err(AppError::Admission.into());
            }
            hits.push(range);
            if index > 0 { out.literal(",")?; }
            out.literal("{\"hit_index\":")?; out.integer(index as u64)?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id)?;
            out.literal(",\"original_range\":")?; out.range(range)?;
            if !options.case_sensitive {
                out.literal(",\"matched_text\":")?; out.quoted(&hit.matched_text)?;
                out.literal(",\"original_hex\":")?;
                out.hex(&self.capture.bytes()[range.start().get() as usize..range.end().get() as usize])?;
            }
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        let response = self.finish_output(out, if complete { EXIT_OK } else { EXIT_PARTIAL }, &mut canceled)?;
        check(&mut canceled)?;
        self.query = Some(AcceptedQuery { generation, hits, _lease: lease });
        Ok(response)
    }
}
