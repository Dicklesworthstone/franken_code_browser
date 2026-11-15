#![forbid(unsafe_code)]

//! Progressive membership and exact retained witnesses have their own wire
//! namespace. No atlas node, layout revision or closed manifest is invented.

use super::*;
use crate::{EXIT_OK, EXIT_PARTIAL};
use crate::output::Output;
use super::super::MAX_CATALOG_RESPONSE_BYTES;

impl CatalogContentSearchSession {
    pub(super) fn snapshot(&mut self, command: &str, start: usize, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogContentSearchError> {
        if !(1..=MAX_ATLAS_STREAM_SEARCH_PAGE).contains(&count) || start > self.hits.len() {
            return Err(CatalogContentSearchError::InvalidLimits);
        }
        check(&mut canceled)?; self.validate_active()?;
        let mut out = self.output(command)?;
        let end = self.hits.len().min(start + count);
        integer(&mut out, "page_offset", start as u64)?;
        integer(&mut out, "page_count", (end - start) as u64)?;
        out.literal(",\"hits\":[")?;
        for (position, hit) in self.hits.iter().enumerate().take(end).skip(start) {
            check(&mut canceled)?;
            if position != start { out.literal(",")?; }
            out.literal("{")?; encode_hit(&mut out, hit, position)?; out.literal("}")?;
        }
        out.literal("],\"next_offset\":")?;
        if end < self.hits.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
        self.encode_diagnostics(&mut out, &mut canceled)?;
        out.literal("}\n")?;
        self.finish(out, &mut canceled)
    }

    pub(super) fn encode_diagnostics(&self, out: &mut Output, canceled: &mut impl FnMut() -> bool)
        -> Result<(), CatalogContentSearchError> {
        out.literal(",\"diagnostics\":[")?;
        for (position, receipt) in self.diagnostics.iter().enumerate() {
            check(canceled)?;
            if position != 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(receipt.request.file().get())?;
            integer(out, "source_revision", receipt.request.revision().get())?;
            encode_receipt(out, *receipt)?;
            out.literal(",\"unsupported_at\":")?;
            if let Some(offset) = receipt.unsupported_at { out.integer(offset.get())?; } else { out.literal("null")?; }
            out.literal("}")?;
        }
        out.literal("]")?;
        boolean(out, "diagnostics_truncated", self.incomplete > self.diagnostics.len() as u64)?;
        Ok(())
    }

    pub(super) fn witness(&mut self, position: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, CatalogContentSearchError> {
        if position >= self.hits.len() { return Err(CatalogContentSearchError::MissingHit); }
        check(&mut canceled)?; self.validate_active()?;
        let mut out = self.output("hit")?;
        let hit = &self.hits[position];
        out.literal(",\"hit\":{")?; encode_hit(&mut out, hit, position)?;
        out.literal(",\"text\":")?;
        out.quoted(self.matched_text.get(hit.text_start..hit.text_start + hit.text_len)
            .ok_or(CatalogContentSearchError::MissingHit)?)?;
        out.literal(",\"original_hex\":")?;
        out.hex(self.witnesses.get(hit.witness_start..hit.witness_start + hit.witness_len)
            .ok_or(CatalogContentSearchError::MissingHit)?)?;
        out.literal("},\"additional_source_bytes_read\":\"0\",\"additional_read_calls\":\"0\"}\n")?;
        self.finish(out, &mut canceled)
    }

    fn output(&mut self, command: &str) -> Result<Output, CatalogContentSearchError> {
        let id = self.catalog.next_id()?;
        let mut out = Output::new(self.owner(), MAX_CATALOG_RESPONSE_BYTES, &self.catalog.budget, id)?;
        out.literal("{\"schema\":\"fcb.catalog-stream/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        self.encode_metadata(&mut out)?;
        Ok(out)
    }

    pub(super) fn encode_metadata(&self, out: &mut Output) -> Result<(), CatalogContentSearchError> {
        integer(out, "owner", self.owner().get())?;
        integer(out, "query_generation", self.generation.get())?;
        integer(out, "revision", self.revision)?;
        integer(out, "progress_sequence", self.revision)?;
        out.literal(",\"root_path\":")?; out.path(&self.grant().root_path().to_path_buf())?;
        out.literal(",\"scope\":\"progressively-discovered-whole-files\",\"strategy\":\"streaming-whole-file\",
            \"capture_kind\":\"literal-witnesses-only\",\"source_observation\":\"per-file-streams-not-atomic-workspace\",
            \"ordering\":\"provisional-discovery-order\",\"membership\":\"progressive-observation\",\"discovery_policy\":")?;
        out.quoted(self.catalog.discovery_policy().name())?;
        self.catalog.encode_rule_policy(out)?;
        self.options.scope.encode(out)?;
        out.literal(",\"mode\":")?;
        out.quoted(match self.options.mode {
            CatalogContentSearchMode::Expression => "decoded-text-expression",
            CatalogContentSearchMode::Occurrences => "whole-token-occurrences",
            CatalogContentSearchMode::Text if self.options.scope.case_sensitive() => "exact-decoded-literal",
            CatalogContentSearchMode::Text => "unicode-lowercase-literal",
        })?;
        out.literal(",\"needle\":")?;
        out.quoted(self.needle.text_value().ok_or(CatalogContentSearchError::InvalidLimits)?)?;
        if let Some(expression) = &self.expression {
            out.literal(",\"expression\":")?; out.quoted(&expression.raw_query)?;
            out.literal(",\"primary_needle\":")?; out.quoted(&expression.primary_needle)?;
            out.literal(",\"query_syntax\":\"fcb-bounded-expression/1\",\"predicate_scope\":\"same-observed-stream\",
                \"path_filter_domain\":\"root-relative-utf8-or-escaped-search-key\"")?;
            integer(out, "expression_scope_files", self.expression_scope_files)?;
            integer(out, "expression_excluded_files", self.eligible - self.expression_scope_files)?;
        }
        if self.options.mode == CatalogContentSearchMode::Occurrences {
            out.literal(",\"evidence_level\":\"whole-token-text-candidate\",\"includes_comments_and_literals\":true,
                \"compiler_resolved\":false,\"semantic_complete\":false")?;
        }
        if let Some(files) = self.options.scope.region_files() {
            integer(out, "scope_region_catalogued_files", self.region_catalogued)?;
            integer(out, "scope_region_missing_files", (files as u64).saturating_sub(self.region_catalogued))?;
        }
        let complete = self.complete();
        let more = self.more();
        for (key, value) in [("more", more), ("terminal", !more), ("selection_available", !more),
            ("discovery_more", self.catalog.discovery.is_some()), ("discovery_complete", self.catalog.complete),
            ("search_complete", complete), ("count_all_matches", self.options.count_all_matches),
            ("match_count_complete", complete),
            ("page_has_more_matches", self.matches_seen.saturating_sub(self.options.start_after) > self.hits.len() as u64),
            ("truncated", self.matches_seen > self.hits.len() as u64)] {
            boolean(out, key, value)?;
        }
        out.literal(",\"stop_reason\":")?;
        if let Some(stop) = self.stop_reason { out.quoted(stop.code())?; } else { out.literal("null")?; }
        out.literal(",\"discovery_error_code\":")?;
        if let Some(error) = self.discovery_error { out.quoted(&error.to_string())?; } else { out.literal("null")?; }
        for (key, value) in [("catalogued_files", self.catalog.catalogued_files),
            ("eligible_files", self.eligible), ("excluded_files", self.excluded),
            ("pending_catalogued_files", self.catalog.catalogued_files - self.eligible - self.excluded),
            ("excluded_entries", self.catalog.aggregate.excluded), ("files_examined", self.examined),
            ("files_unexamined", self.search_files() - self.examined), ("incomplete_files", self.incomplete),
            ("unavailable_files", self.unavailable), ("unsupported_files", self.unsupported), ("failed_files", self.failed),
            ("matching_files", self.matching_files), ("retained_hits", self.hits.len() as u64),
            ("retained_matching_files", self.retained_matching_files), ("matches_seen", self.matches_seen),
            ("page_start_after", self.options.start_after), ("skipped_matches", self.options.start_after.min(self.matches_seen)),
            ("payload_bytes_read", self.io.bytes_read), ("scanned_input_bytes", self.io.scanned_bytes),
            ("read_calls", self.io.read_calls), ("interrupted_read_calls", self.io.interrupted_calls),
            ("peak_input_buffer_bytes", self.io.peak_buffer_bytes as u64),
            ("retained_witness_bytes", self.witnesses.len() as u64),
            ("retained_catalog_pages", self.catalog.pages.len() as u64),
            ("retained_catalog_files", self.catalog.pages.iter().map(|page| page.files.len() as u64).sum())] {
            integer(out, key, value)?;
        }
        out.literal(",\"limits\":{\"max_matches\":")?; out.integer(self.options.limits.max_matches as u64)?;
        integer(out, "max_scan_bytes", self.options.limits.max_scan_bytes)?;
        integer(out, "max_read_calls", self.options.limits.max_read_calls)?;
        integer(out, "max_steps_per_advance", MAX_CATALOG_STREAM_STEPS as u64)?;
        out.literal("},\"discovery\":{")?;
        let aggregate = self.catalog.aggregate;
        for (position, (key, value)) in [("files", aggregate.files), ("directories", aggregate.directories),
            ("excluded_entries", aggregate.excluded), ("symlinks_not_followed", aggregate.symlinks),
            ("special_objects", aggregate.special), ("unavailable", aggregate.unavailable), ("cycles", aggregate.cycles),
            ("path_limited", aggregate.path_limited), ("depth_limited", aggregate.depth_limited),
            ("queue_refused", aggregate.queue_refused)].into_iter().enumerate() {
            if position != 0 { out.literal(",")?; }
            out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal("}")?;
        Ok(())
    }
    fn finish(&mut self, out: Output, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, CatalogContentSearchError> {
        self.validate_active()?; check(canceled)?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let id = self.catalog.next_id()?;
        let lease = self.catalog.budget.try_reserve_managed(self.owner(), id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut text = String::new(); text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
        if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        self.validate_active()?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if self.complete() && self.matches_seen == self.hits.len() as u64 {
            EXIT_OK
        } else { EXIT_PARTIAL }, _lease: lease })
    }
}

fn integer(out: &mut Output, key: &str, value: u64) -> Result<(), OutputError> {
    out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)
}
fn boolean(out: &mut Output, key: &str, value: bool) -> Result<(), OutputError> {
    out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.boolean(value)
}
pub(super) fn encode_hit(out: &mut Output, hit: &Hit, position: usize) -> Result<(), CatalogContentSearchError> {
    if hit.range.len().get() != hit.witness_len as u64 { return Err(CatalogContentSearchError::MissingHit); }
    out.literal("\"index\":")?; out.integer(position as u64)?;
    integer(out, "occurrence_id", hit.occurrence)?;
    integer(out, "file_id", hit.file.request.file().get())?;
    integer(out, "source_revision", hit.file.request.revision().get())?;
    out.literal(",\"path\":")?; out.path(&hit.path.raw().to_path_buf())?;
    out.literal(",\"original_range\":")?; out.range(hit.range)?;
    encode_receipt(out, hit.file)?;
    out.literal(",\"witness_sha256\":")?; out.quoted(&hit.digest.to_hex())?;
    Ok(())
}
fn encode_receipt(out: &mut Output, receipt: Receipt) -> Result<(), CatalogContentSearchError> {
    out.literal(",\"file_state\":")?;
    out.quoted(if receipt.unavailable.is_some() { "unavailable" } else { receipt.state.code() })?;
    boolean(out, "file_complete", receipt.complete)?;
    out.literal(",\"file_error_code\":")?;
    if let Some(error) = receipt.unavailable { out.quoted(&error.to_string())?; }
    else if let StreamReadState::Failed(error) = receipt.state { out.quoted(&error.to_string())?; }
    else { out.literal("null")?; }
    out.literal(",\"encoding\":")?;
    match receipt.encoding {
        Some(DetectedEncoding::Utf8 { .. }) => out.quoted("utf8")?,
        Some(DetectedEncoding::Utf16Le) => out.quoted("utf16le")?,
        Some(DetectedEncoding::Utf16Be) => out.quoted("utf16be")?,
        _ => out.literal("null")?,
    }
    out.literal(",\"observed_length\":")?;
    if let Some(length) = receipt.length { out.integer(length.get())?; } else { out.literal("null")?; }
    out.literal(",\"final_observed_length\":")?;
    if let Some(length) = receipt.final_length { out.integer(length.get())?; } else { out.literal("null")?; }
    out.literal(",\"consistency\":")?;
    match receipt.consistency {
        Some(ExtentConsistency::UnchangedMetadata) => out.quoted("unchanged-metadata-not-atomic")?,
        Some(ExtentConsistency::ChangedDuringRead) => out.quoted("changed-during-read")?,
        Some(ExtentConsistency::ShortRead) => out.quoted("short-read")?,
        Some(_) => out.quoted("metadata-unavailable")?,
        None => out.literal("null")?,
    }
    Ok(())
}
