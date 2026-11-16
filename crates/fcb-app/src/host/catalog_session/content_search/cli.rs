#![forbid(unsafe_code)]

//! The CLI drives the same progressive service one bounded step at a time.
//! Typed counters avoid reparsing native JSON; exact finalized witnesses remain
//! owned here until the CLI's independently bounded output accepts them.

use super::*;
use crate::output::Output;
use fcb::source::discovery::DiscoveryAggregate;
use super::super::{MAX_CATALOG_PAGE_ENTRIES, MAX_CATALOG_PAGE_PATH_BYTES, MAX_CATALOG_PATH_BYTES,
    MAX_CATALOG_RETAINED_PAGES, MAX_CATALOG_RETAINED_FILES, MAX_CATALOG_RETAINED_PATH_BYTES,
    WALK_DEPTH, WALK_QUEUE};

#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CatalogSearchProgress {
    pub revision: u64,
    pub more: bool,
    pub complete: bool,
    pub discovery_complete: bool,
    pub discovery_more: bool,
    pub catalogued_files: u64,
    pub discovery_pages: u64,
    pub discovery: DiscoveryAggregate,
    pub rule_policy_complete: bool,
    pub failed_rule_files: usize,
    pub unresolved_rule_paths: usize,
    pub scope_files: u64,
    pub excluded_files: u64,
    pub pending_catalogued_files: u64,
    pub examined: u64,
    pub incomplete: u64,
    pub unavailable: u64,
    pub unsupported: u64,
    pub failed: u64,
    pub matching_files: u64,
    pub retained_hits: usize,
    pub matches_seen: u64,
    pub skip_matches: u64,
    pub count_all: bool,
    pub bytes_read: u64,
    pub read_calls: u64,
    pub scanned_bytes: u64,
    pub retained_pages: usize,
    pub retained_files: usize,
    pub stop_reason: Option<&'static str>,
}
impl CatalogSearchProgress {
    pub fn truncated(self) -> bool { self.matches_seen > self.retained_hits as u64 }
}

impl CatalogContentSearchSession {
    /// At most one existing walker batch, admission, or FileSearch quantum.
    /// The CLI polls its cancellation flag BETWEEN these calls and then uses
    /// stop_cli so completed files remain publishable. Native hard cancellation
    /// continues to use advance's existing revocation contract.
    pub(crate) fn advance_cli_step(&mut self) -> Result<(), CatalogContentSearchError> {
        self.advance_work(self.generation.get(), self.revision, 1, &mut || false)
    }
    pub(crate) fn stop_cli(&mut self) -> Result<(), CatalogContentSearchError> {
        self.stop_work(self.generation.get(), &mut || false)
    }
    /// Counters remain readable after a source/grant failure. They confer no
    /// authority to reopen sources or publish any unverified active candidate.
    pub(crate) fn cli_progress(&self) -> CatalogSearchProgress {
        let rules = self.catalog.rule_policy().map(|rules| rules.stats());
        CatalogSearchProgress {
            revision: self.revision, more: self.more(), complete: self.complete(),
            discovery_complete: self.catalog.complete, discovery_more: self.catalog.discovery.is_some(),
            catalogued_files: self.catalog.catalogued_files, discovery_pages: self.catalog.latest_page,
            discovery: self.catalog.aggregate,
            rule_policy_complete: self.catalog.rule_policy_complete(),
            failed_rule_files: rules.map_or(0, |stats| stats.failed_files),
            unresolved_rule_paths: rules.map_or(0, |stats| stats.unresolved_paths),
            scope_files: self.search_files(),
            excluded_files: self.excluded + self.eligible - self.search_files(),
            pending_catalogued_files: self.catalog.catalogued_files - self.eligible - self.excluded,
            examined: self.examined, incomplete: self.incomplete, unavailable: self.unavailable,
            unsupported: self.unsupported, failed: self.failed, matching_files: self.matching_files,
            retained_hits: self.hits.len(), matches_seen: self.matches_seen,
            skip_matches: self.options.start_after, count_all: self.options.count_all_matches,
            bytes_read: self.io.bytes_read, read_calls: self.io.read_calls, scanned_bytes: self.io.scanned_bytes,
            retained_pages: self.catalog.pages.len(),
            retained_files: self.catalog.pages.iter().map(|page| page.files.len()).sum(),
            stop_reason: self.stop_reason.map(Stop::code),
        }
    }
    pub(crate) fn encode_cli_metadata(&self, out: &mut Output) -> Result<(), CatalogContentSearchError> {
        self.validate_active()?;
        self.encode_metadata(out)?;
        out.literal(",\"discovery_limits\":{\"max_depth\":")?; out.integer(u64::from(WALK_DEPTH))?;
        for (key, value) in [("max_pending_directories", WALK_QUEUE as usize),
            ("max_path_bytes", MAX_CATALOG_PATH_BYTES), ("page_entries", MAX_CATALOG_PAGE_ENTRIES),
            ("page_path_bytes", MAX_CATALOG_PAGE_PATH_BYTES), ("retained_pages", MAX_CATALOG_RETAINED_PAGES),
            ("retained_files", MAX_CATALOG_RETAINED_FILES), ("retained_path_bytes", MAX_CATALOG_RETAINED_PATH_BYTES)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
        }
        out.literal("}")?;
        self.encode_diagnostics(out, &mut || false)
    }
    /// Every row is from an already finalized file. This never publishes a
    /// tentative expression match or asks the filesystem for witness bytes.
    pub(crate) fn encode_cli_match(&self, position: usize, out: &mut Output)
        -> Result<(), CatalogContentSearchError> {
        self.validate_active()?;
        let hit = self.hits.get(position).ok_or(CatalogContentSearchError::MissingHit)?;
        out.literal("{")?;
        wire::encode_hit(out, hit, position)?;
        out.literal(",\"matched_text\":")?;
        out.quoted(self.matched_text.get(hit.text_start..hit.text_start + hit.text_len)
            .ok_or(CatalogContentSearchError::MissingHit)?)?;
        out.literal(",\"original_hex\":")?;
        out.hex(self.witnesses.get(hit.witness_start..hit.witness_start + hit.witness_len)
            .ok_or(CatalogContentSearchError::MissingHit)?)?;
        self.encode_context(out, hit, &mut || false)?;
        out.literal("}")?;
        Ok(())
    }
    pub(crate) fn encode_cli_human_matches(&self, out: &mut Output)
        -> Result<(), CatalogContentSearchError> {
        self.validate_active()?;
        for hit in &self.hits {
            out.human_text(&hit.path.display_escaped().to_string())?;
            out.literal(": bytes ")?; out.literal(&hit.range.start().get().to_string())?;
            out.literal("..")?; out.literal(&hit.range.end().get().to_string())?;
            out.literal("  ")?;
            out.human_text(self.matched_text.get(hit.text_start..hit.text_start + hit.text_len)
                .ok_or(CatalogContentSearchError::MissingHit)?)?;
            out.literal("\n")?;
            stream_context::human(out, self.retained_context(hit)?, &self.catalog.budget,
                self.context_decode_ids, &mut || false)?;
        }
        Ok(())
    }
}
