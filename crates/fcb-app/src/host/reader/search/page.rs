#![forbid(unsafe_code)]

//! Bounded occurrence pages over one retained immutable capture. Every replay
//! starts at byte zero so decoding, overlapping matches and scalar expansions
//! keep their original occurrence identities. Skipped rows are never retained.

use super::*;

impl ReaderSession {
    /// Return a bounded page after the first start_after occurrences. This is
    /// an ordinal, not a byte offset; two matches can name the same source unit.
    /// Every attempt needs a fresh generation. Only a fully encoded successful
    /// response replaces the accepted hit table, preserving old hits on failure.
    ///
    /// Replaying the immutable capture performs no source I/O. Its byte budget
    /// includes the skipped prefix; this is bounded replay, not an index seek.
    /// matches_seen is cumulative from source start. skipped_matches and
    /// has_more_matches distinguish omitted earlier rows from later matches.
    pub fn search_page(&mut self, generation: u64, needle: &str, start_after: u64,
        max_matches: usize, max_scan_bytes: u64, options: ReaderFindOptions,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        if generation == 0 || generation <= self.last_query_attempt {
            return Err(ReaderSessionError::StaleQuery);
        }
        if needle.is_empty() || needle.len() > MAX_READER_NEEDLE_BYTES
            || max_matches > MAX_READER_MATCHES || start_after > self.length()
            || (max_matches == 0 && !options.count_all_matches) {
            return Err(ReaderSessionError::InvalidLimits);
        }
        self.last_query_attempt = generation;
        check(&mut canceled)?;

        let [pattern_id, worker_id, result_id] = self.allocations()?;
        let pattern = StreamingNeedle::text_matching(self.owner(), needle,
            options.case_sensitive, &self.budget, pattern_id)?;
        // Bound wire escaping and hex independently of the scanner's retained
        // storage. A long query can reduce page size without losing occurrences.
        let witness_capacity = pattern.max_witness_bytes();
        let row_bytes = witness_capacity.checked_mul(8).and_then(|n| n.checked_add(256))
            .ok_or(AppError::Admission)?;
        let retained_limit = max_matches.min(ROW_WIRE_BYTES / row_bytes);
        if max_matches > 0 && retained_limit == 0 { return Err(AppError::Admission.into()); }
        let charge = retained_limit.checked_mul(size_of::<ByteRange>())
            .and_then(|n| n.checked_add(size_of::<AcceptedQuery>())).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(self.owner(), result_id,
            ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let mut hits = reserve(retained_limit)?;
        let query_generation = QueryGeneration::new(self.owner(), generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let mut scan_options = StreamReadOptions::new(query_generation);
        scan_options.max_matches = retained_limit;
        scan_options.max_bytes = max_scan_bytes;
        scan_options.encoding = Some(self.encoding);
        let mut scan = ReaderSearch::new_page(self.capture.bytes(), *self.capture.request(),
            ByteLength::new(self.length()), &pattern, scan_options, start_after,
            options.count_all_matches, &self.budget, worker_id)?;
        while scan.state() == StreamReadState::Pending {
            scan.step(StreamReadStep::default(), query_generation, &mut canceled)?;
        }
        let (_, report) = scan.finish()?;
        check(&mut canceled)?;
        if report.state() == StreamReadState::Canceled { return Err(ReaderSessionError::Canceled); }
        if let StreamReadState::Failed(error) = report.state() { return Err(error.into()); }
        if report.matches_seen() > self.length() || report.hits().len() > retained_limit {
            return Err(AppError::Admission.into());
        }

        let mut out = self.output("find")?;
        out.literal(",\"query_generation\":")?; out.integer(generation)?;
        out.literal(",\"mode\":")?;
        out.quoted(if options.case_sensitive { "exact-decoded-literal" } else { "unicode-lowercase-literal" })?;
        out.literal(",\"needle\":")?; out.quoted(needle)?;
        out.literal(",\"case_sensitive\":")?; out.boolean(options.case_sensitive)?;
        out.literal(",\"count_all_matches\":")?; out.boolean(options.count_all_matches)?;
        out.literal(",\"retained_limit\":")?; out.integer(retained_limit as u64)?;
        out.literal(",\"page_start_after\":")?; out.integer(report.page_start_after())?;
        out.literal(",\"skipped_matches\":")?; out.integer(report.skipped_matches())?;
        out.literal(",\"has_more_matches\":")?; out.boolean(report.has_more_matches())?;
        out.literal(",\"state\":")?; out.quoted(report.state().code())?;
        out.literal(",\"search_complete\":")?; out.boolean(report.input_complete())?;
        out.literal(",\"match_count_complete\":")?; out.boolean(report.input_complete())?;
        out.literal(",\"rows_truncated\":")?;
        out.boolean(report.matches_seen() > report.hits().len() as u64)?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.stats().scanned_bytes)?;
        out.literal(",\"matches_seen\":")?; out.integer(report.matches_seen())?;
        out.literal(",\"retained_hits\":")?; out.integer(report.hits().len() as u64)?;
        out.literal(",\"literal_original_hex\":")?;
        if options.case_sensitive && !report.hits().is_empty() {
            out.hex(report.witness_bytes(0)?)?;
        } else { out.literal("null")?; }
        out.literal(",\"unsupported_at\":")?;
        if let Some(offset) = report.unsupported_at() { out.integer(offset.get())?; }
        else { out.literal("null")?; }
        out.literal(",\"hits\":[")?;
        for (index, hit) in report.hits().iter().enumerate() {
            check(&mut canceled)?;
            let range = hit.original_range();
            let witness = report.witness_bytes(index)?;
            if range.end().get() > self.length() || witness.len() > witness_capacity {
                return Err(AppError::Admission.into());
            }
            hits.push(range);
            if index > 0 { out.literal(",")?; }
            out.literal("{\"hit_index\":")?; out.integer(index as u64)?;
            out.literal(",\"occurrence_id\":")?; out.integer(hit.occurrence_id())?;
            out.literal(",\"original_range\":")?; out.range(range)?;
            if !options.case_sensitive {
                let text = report.matched_text(index)?;
                if text.len() > witness_capacity { return Err(AppError::Admission.into()); }
                out.literal(",\"matched_text\":")?; out.quoted(text)?;
                out.literal(",\"original_hex\":")?; out.hex(witness)?;
            }
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        let exit = if report.input_complete() { EXIT_OK } else { EXIT_PARTIAL };
        let response = self.finish_output(out, exit, &mut canceled)?;
        check(&mut canceled)?;
        self.query = Some(AcceptedQuery { generation, hits, _lease: lease });
        Ok(response)
    }
}
