#![forbid(unsafe_code)]

//! Structural evidence has its own namespace. Rows never invent an atlas node,
//! a frozen project manifest, or compiler-resolved declarations.

use super::*;
use crate::{EXIT_OK, EXIT_PARTIAL};
use crate::output::{Output, MAX_RESPONSE_BYTES};
use fcb::search::DetectedEncoding;
use fcb::source::detect_encoding;

impl CatalogStructureSearchSession {
    pub(super) fn snapshot(&mut self, command: &str, start: usize, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CatalogStructureSearchError> {
        if !(1..=MAX_CATALOG_STRUCTURE_PAGE).contains(&count) || start > self.hits.len() {
            return Err(CatalogStructureSearchError::InvalidLimits);
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
        self.encode_diagnostics(&mut out)?;
        out.literal("}\n")?;
        self.finish(out, &mut canceled)
    }
    pub(super) fn witness(&mut self, position: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, CatalogStructureSearchError> {
        if position >= self.hits.len() { return Err(CatalogStructureSearchError::MissingHit); }
        check(&mut canceled)?; self.validate_active()?;
        let mut out = self.output("hit")?;
        let hit = &self.hits[position];
        out.literal(",\"hit\":{")?; encode_hit(&mut out, hit, position)?;
        out.literal(",\"original_hex\":")?;
        let range = hit.row.selected;
        out.hex(hit.source.capture.bytes().get(range.start().get() as usize..range.end().get() as usize)
            .ok_or(CatalogStructureSearchError::MissingHit)?)?;
        out.literal("},\"additional_source_bytes_read\":\"0\",\"additional_read_calls\":\"0\"}\n")?;
        self.finish(out, &mut canceled)
    }
    /// Copy the exact retained capture to an already admitted, fresh reader
    /// owner. This never resolves or reopens the hit's pathname. Grant and
    /// generation checks precede copying, and again precede publication.
    pub fn open_hit_reader(&mut self, reader_owner: ArenaOwnerId, generation: u64,
        revision: u64, position: usize, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), CatalogStructureSearchError> {
        self.validate_revision(generation, revision)?;
        if reader_owner == self.owner() { return Err(CatalogSessionError::ForeignOwner.into()); }
        let hit = self.hits.get(position).ok_or(CatalogStructureSearchError::MissingHit)?;
        let source = Arc::clone(&hit.source);
        let start = hit.row.selected.start().get().saturating_sub(256);
        check(&mut canceled)?; self.validate_active()?;
        let (reader, info, window) = {
            let mut stop = || canceled() || self.catalog.validate_active().is_err();
            let mut reader = ReaderSession::from_bytes(reader_owner, &source.path.raw().to_path_buf(),
                source.capture.bytes(), &mut stop)?;
            if reader.capture_sha256() != source.digest { return Err(CatalogStructureSearchError::MissingHit); }
            let info = reader.info(&mut stop)?;
            let window = reader.read_window(start, 16 * 1024, &mut stop)?;
            (reader, info, window)
        };
        self.validate_active()?; check(&mut canceled)?;
        let mut out = self.output("open-reader")?;
        out.literal(",\"hit\":{")?; encode_hit(&mut out, &self.hits[position], position)?;
        out.literal("},\"reader_owner\":")?; out.integer(reader_owner.get())?;
        out.literal(",\"activation_source\":\"retained-search-capture\",\"additional_source_bytes_read\":\"0\",\"additional_read_calls\":\"0\",\"reader\":")?;
        out.literal(info.as_str())?;
        out.literal(",\"window\":")?; out.literal(window.as_str())?;
        out.literal("}\n")?;
        let response = self.finish(out, &mut canceled)?;
        Ok((reader, response))
    }
    fn output(&mut self, command: &str) -> Result<Output, CatalogStructureSearchError> {
        let id = self.catalog.next_id()?;
        let mut out = Output::new(self.owner(), MAX_RESPONSE_BYTES, &self.catalog.budget, id)?;
        out.literal("{\"schema\":\"fcb.catalog-structure/1\",\"status\":\"ok\",\"command\":")?;
        out.quoted(command)?;
        self.encode_metadata(&mut out)?;
        Ok(out)
    }
    fn encode_metadata(&self, out: &mut Output) -> Result<(), CatalogStructureSearchError> {
        integer(out, "owner", self.owner().get())?;
        integer(out, "query_generation", self.generation.get())?;
        integer(out, "revision", self.revision)?;
        out.literal(",\"root_path\":")?; out.path(&self.grant().root_path().to_path_buf())?;
        out.literal(",\"scope\":\"progressively-discovered-whole-files\",\"strategy\":\"bounded-complete-capture-extraction\",\"capture_kind\":\"complete-retained-source\",\"source_observation\":\"per-file-captures-not-atomic-workspace\",\"ordering\":\"provisional-discovery-order\",\"membership\":\"progressive-observation\",\"discovery_policy\":")?;
        out.quoted(self.catalog.discovery_policy().name())?;
        self.catalog.encode_rule_policy(out)?;
        self.options.scope.encode(out)?;
        out.literal(",\"requested_mode\":")?; out.quoted(self.options.mode.requested())?;
        out.literal(",\"realized_mode\":")?; out.quoted(self.options.mode.realized())?;
        out.literal(",\"evidence_level\":")?; out.quoted(self.options.mode.realized())?;
        out.literal(",\"semantic_complete\":false,\"compiler_resolved\":false,\"name_case\":\"sensitive\",\"name_mode\":")?;
        out.quoted(match self.options.name_mode { SymbolNameMode::Exact => "exact",
            SymbolNameMode::Prefix => "prefix", SymbolNameMode::Contains => "contains" })?;
        out.literal(",\"needle\":")?; out.quoted(&self.needle)?;
        out.literal(",\"completeness_basis\":\"admitted-extraction-processing\",\"state\":")?;
        out.quoted(if self.canceled { "canceled" } else if self.more() { "discovering" }
            else if self.complete() { "complete" } else { "partial" })?;
        let complete = self.complete();
        let more = self.more();
        for (key, value) in [("more", more), ("terminal", !more), ("selection_available", !self.hits.is_empty()),
            ("discovery_more", self.catalog.discovery.is_some()), ("discovery_complete", self.catalog.complete),
            ("search_complete", complete), ("count_all_matches", true), ("match_count_complete", complete),
            ("truncated", self.matches_seen > self.hits.len() as u64),
            ("page_has_more_matches", self.matches_seen.saturating_sub(self.options.start_after) > self.hits.len() as u64)] {
            boolean(out, key, value)?;
        }
        out.literal(",\"stop_reason\":")?;
        if let Some(stop) = self.stop_reason { out.quoted(stop.code())?; } else { out.literal("null")?; }
        out.literal(",\"discovery_error_code\":")?;
        if let Some(error) = self.discovery_error { out.quoted(&error.to_string())?; } else { out.literal("null")?; }
        out.literal(",\"work_error_code\":")?;
        if let Some(error) = self.work_error { out.quoted(&error.to_string())?; } else { out.literal("null")?; }
        if let Some(files) = self.options.scope.region_files() {
            integer(out, "scope_region_catalogued_files", self.region_catalogued)?;
            integer(out, "scope_region_missing_files", (files as u64).saturating_sub(self.region_catalogued))?;
        }
        for (key, value) in [("catalogued_files", self.catalog.catalogued_files),
            ("eligible_files", self.eligible), ("excluded_files", self.excluded),
            ("pending_catalogued_files", self.catalog.catalogued_files - self.eligible - self.excluded),
            ("files_examined", self.examined), ("files_unexamined", self.eligible - self.examined),
            ("files_scanned", self.scanned), ("incomplete_files", self.incomplete),
            ("unsupported_files", self.unsupported), ("oversized_files", self.oversized),
            ("unavailable_files", self.unavailable), ("analysis_limited_files", self.analysis_limited),
            ("matching_files", self.matching_files), ("matches_seen", self.matches_seen),
            ("retained_hits", self.hits.len() as u64), ("retained_matching_files", self.retained_matching_files),
            ("retained_source_bytes", self.retained_source_bytes as u64),
            ("structure_metadata_bytes", self.metadata_bytes as u64), ("structure_metadata_omitted", self.metadata_omitted),
            ("source_retention_omitted_files", self.source_retention_omitted),
            ("page_start_after", self.options.start_after), ("skipped_matches", self.options.start_after.min(self.matches_seen)),
            ("payload_bytes_read", self.io.bytes), ("read_calls", self.io.calls),
            ("retained_catalog_pages", self.catalog.pages.len() as u64),
            ("retained_catalog_files", self.catalog.pages.iter().map(|page| page.files.len() as u64).sum())] {
            integer(out, key, value)?;
        }
        out.literal(",\"limits\":{\"max_matches\":")?; out.integer(self.options.max_matches as u64)?;
        integer(out, "max_scan_bytes", self.options.max_scan_bytes)?;
        integer(out, "max_read_calls", self.options.max_read_calls)?;
        integer(out, "max_file_bytes", self.options.mode.source_limit() as u64)?;
        integer(out, "max_retained_source_bytes", MAX_CATALOG_STRUCTURE_RETAINED_SOURCE_BYTES as u64)?;
        integer(out, "max_steps_per_advance", MAX_CATALOG_STRUCTURE_STEPS as u64)?;
        out.literal("},\"discovery\":{")?;
        let aggregate = self.catalog.aggregate;
        for (index, (key, value)) in [("files", aggregate.files), ("directories", aggregate.directories),
            ("excluded_entries", aggregate.excluded), ("symlinks_not_followed", aggregate.symlinks),
            ("special_objects", aggregate.special), ("unavailable", aggregate.unavailable), ("cycles", aggregate.cycles),
            ("path_limited", aggregate.path_limited), ("depth_limited", aggregate.depth_limited),
            ("queue_refused", aggregate.queue_refused)].into_iter().enumerate() {
            if index != 0 { out.literal(",")?; }
            out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal("}")?;
        Ok(())
    }
    fn encode_diagnostics(&self, out: &mut Output) -> Result<(), CatalogStructureSearchError> {
        out.literal(",\"diagnostics\":[")?;
        for (index, diagnostic) in self.diagnostics.iter().enumerate() {
            if index != 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(diagnostic.file.get())?;
            out.literal(",\"code\":")?; out.quoted(diagnostic.code)?;
            out.literal("}")?;
        }
        out.literal("]")?;
        boolean(out, "diagnostics_truncated", self.incomplete > self.diagnostics.len() as u64)?;
        Ok(())
    }
    fn finish(&mut self, out: Output, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, CatalogStructureSearchError> {
        self.validate_active()?; check(canceled)?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let id = self.catalog.next_id()?;
        let lease = self.catalog.budget.try_reserve_managed(self.owner(), id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut text = String::new();
        text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
        if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        self.validate_active()?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if self.complete() && self.matches_seen == self.hits.len() as u64 {
            EXIT_OK
        } else { EXIT_PARTIAL }, _lease: lease })
    }
}
fn encode_hit(out: &mut Output, hit: &Hit, position: usize) -> Result<(), OutputError> {
    out.literal("\"index\":")?; out.integer(position as u64)?;
    integer(out, "occurrence_id", hit.row.occurrence)?;
    integer(out, "candidate_id", hit.row.candidate_id)?;
    integer(out, "file_id", hit.source.capture.request().file().get())?;
    integer(out, "source_revision", hit.source.capture.request().revision().get())?;
    integer(out, "source_length", hit.source.capture.bytes().len() as u64)?;
    out.literal(",\"path\":")?; out.path(&hit.source.path.raw().to_path_buf())?;
    out.literal(",\"original_range\":")?; out.range(hit.row.selected)?;
    out.literal(",\"evidence_range\":")?; out.range(hit.row.evidence)?;
    out.literal(",\"name\":")?; out.quoted(&hit.row.name)?;
    out.literal(",\"kind\":")?; out.quoted(hit.row.kind)?;
    out.literal(",\"language\":")?; out.quoted(hit.row.language)?;
    out.literal(",\"heading_slug\":")?;
    if let Some(slug) = &hit.row.heading_slug { out.quoted(slug)?; } else { out.literal("null")?; }
    out.literal(",\"capture_sha256\":")?; out.quoted(&hit.source.digest.to_hex())?;
    out.literal(",\"capture_complete\":true,\"consistency\":\"unchanged-metadata-not-atomic\",\"encoding\":")?;
    out.quoted(match detect_encoding(hit.source.capture.bytes()) {
        DetectedEncoding::Utf8 { .. } => "utf8", DetectedEncoding::Utf16Le => "utf16le",
        DetectedEncoding::Utf16Be => "utf16be", _ => "unsupported",
    })?;
    Ok(())
}
fn integer(out: &mut Output, key: &str, value: u64) -> Result<(), OutputError> {
    out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)
}
fn boolean(out: &mut Output, key: &str, value: bool) -> Result<(), OutputError> {
    out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.boolean(value)
}
