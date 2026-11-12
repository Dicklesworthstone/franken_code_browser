#![forbid(unsafe_code)]

//! Application composition for explicit --paged index commands. Shares archive
//! grants/pins, source verification, the exact query engine, bounded JSON, and
//! exclusive publication with the other snapshot commands. No new matcher.

use super::*;
use fcb::search::snapshot_index::paged::{PagedPostings, PagedPostingError, PostingPageIo, POSTING_PAGE_BYTES};

impl From<PagedPostingError> for Failure {
    fn from(error: PagedPostingError) -> Self {
        Self { code: error.to_string(), canceled: error == PagedPostingError::Index(SnapshotIndexError::Canceled) }
    }
}

pub(super) fn execute(options: &Options, out: &mut Output, budget: &ResourceBudget,
    effect: &mut Effect, canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    let source = options.archive.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    let mut archive = catalog::open(source, options.catalog.as_deref(), options.catalog_pin,
        budget, [allocation(151), allocation(161)], &mut *canceled)?;
    if options.action == Action::Build {
        let forward = SnapshotIndex::build(&mut archive, options.build, budget,
            [allocation(152), allocation(153), allocation(154), allocation(155)], &mut *canceled)?;
        let stats = forward.stats();
        let inverse = forward.invert(budget, allocation(162), &mut *canceled)?;
        drop(forward);
        let artifact = inverse.encode_paged(budget, allocation(156), &mut *canceled)?;
        drop(inverse);
        let destination = input::absolute(options.output.as_deref().ok_or_else(|| Failure::new("SNAPSHOT_OUTPUT_REQUIRED"))?)?;
        write_new(&destination, artifact.bytes(), effect, canceled)?;
        if options.json {
            begin(out, "snapshot-index-build")?; summary(out, archive.directory(), stats, artifact.digest())?;
            fields(out)?;
            out.literal(",\"effect\":")?; out.quoted(effect.name())?;
            out.literal(",\"destination\":")?; out.path(&destination)?;
            out.literal(",\"index_bytes\":")?; out.integer(artifact.bytes().len() as u64)?;
            out.literal(",\"index_manifest_bytes\":")?; out.integer(artifact.manifest_bytes() as u64)?;
            out.literal(",\"member_payload_bytes_loaded\":")?; out.integer(archive.load_stats().bytes_read)?;
            out.literal(",\"source_derived_sensitive\":true,\"power_loss_qualified\":false}\n")?;
        } else {
            out.literal("Saved demand-paged index. Retain this manifest pin separately:\n")?;
            out.literal(&artifact.digest().to_hex())?;
            out.literal("\nThe pin covers metadata and every page digest, not a full-file hash. Source-derived data is sensitive.\n")?;
        }
        return Ok(if !archive.directory().discovery_complete() || stats.unavailable_files > 0 || stats.uncovered_files > 0 {
            EXIT_PARTIAL
        } else { EXIT_OK });
    }
    let pin = options.pin.ok_or_else(|| Failure::new("SAVED_INDEX_PIN_REQUIRED"))?;
    let path = input::absolute(options.index.as_deref().ok_or_else(|| Failure::new("SAVED_INDEX_REQUIRED"))?)?;
    let (file, _) = input::open_regular(&path)?;
    let mut index = PagedPostings::open_pinned(file, pin, archive.directory(), 4, budget, allocation(152), &mut *canceled)?;
    if options.action == Action::Inspect {
        if options.json {
            begin(out, "snapshot-index-inspect")?; summary(out, archive.directory(), index.stats(), pin)?; fields(out)?;
            io_fields(out, index.io_stats(), index.file_bytes(), index.cache_capacity_bytes())?;
            out.literal(",\"member_payload_bytes_loaded\":\"0\"}\n")?;
        } else {
            out.literal("Trusted index manifest validated; no posting pages read. Unread page integrity remains unchecked.\n")?;
        }
        return Ok(if !archive.directory().discovery_complete() || index.stats().unavailable_files > 0 || index.stats().uncovered_files > 0 {
            EXIT_PARTIAL
        } else { EXIT_OK });
    }
    let report = if options.ignore_case {
        matching::direct(&mut archive, options.text.as_deref(), None, true, options.count_all_matches,
            options.limit, budget, [allocation(157), allocation(158), allocation(159), allocation(160)], canceled)?
    } else {
        let needle = match (&options.text, &options.raw) {
            (Some(text), None) => IndexedNeedle::text(owner(), text, budget, allocation(157)),
            (None, Some(bytes)) => IndexedNeedle::raw(owner(), bytes, budget, allocation(157)),
            _ => return Err(Failure::new("CLI_INVALID_NEEDLE")),
        }.map_err(|error| Failure::new(&error.to_string()))?;
        let opts = PagedQueryOptions { generation: generation(), first_file: file_id(), first_revision: revision(), max_matches: options.limit };
        let ids = [allocation(158), allocation(159), allocation(160)];
        let query = if options.count_all_matches {
            PagedQuery::new_paged_counting(&mut archive, &needle, &mut index, opts, budget, ids)?
        } else {
            PagedQuery::new_paged(&mut archive, &needle, &mut index, opts, budget, ids)?
        };
        matching::finish(query, budget, canceled)?
    };
    if options.json {
        begin(out, "snapshot-index-search")?; summary(out, archive.directory(), index.stats(), pin)?; search_fields(out, options.ignore_case)?;
        io_fields(out, index.io_stats(), index.file_bytes(), index.cache_capacity_bytes())?;
        out.literal(",\"identity_scope\":\"response-local\"")?;
        matching::fields(out, &report, options.raw.is_some(), options.ignore_case, true)?;
        out.literal(",\"unsupported_text_files_count\":")?; out.integer(report.stats().unsupported_files as u64)?;
        out.literal(",\"index_eliminated_files\":")?; out.integer(report.stats().index_eliminated_files as u64)?;
        out.literal(",\"index_candidates\":")?; out.integer(report.stats().index_candidates as u64)?;
        out.literal(",\"index_fallback_files\":")?; out.integer(report.stats().index_fallback_files as u64)?;
        out.literal(",\"metadata_members_visited\":")?; out.integer(report.stats().members_visited as u64)?;
        out.literal(",\"posting_entries_visited\":")?; out.integer(report.stats().posting_entries_visited as u64)?;
        out.literal(",\"posting_cursor_complete\":")?; out.boolean(report.stats().posting_cursor_complete)?;
        out.literal(",\"member_payload_bytes_loaded\":")?; out.integer(archive.load_stats().bytes_read)?;
        out.literal(",\"loaded_members\":")?; out.integer(archive.load_stats().loaded_members)?;
        out.literal(",\"scanned_bytes\":")?; out.integer(report.stats().scanned_bytes)?;
    } else {
        out.literal(if report.is_complete() { "Complete saved-scope search.\n" } else { "PARTIAL saved-scope search.\n" })?;
        out.literal(if options.ignore_case { "The index manifest was validated; its posting pages were not consulted.\n" }
            else { "Index pages and source members were verified on demand; unread backing integrity remains unchecked.\n" })?;
        matching::human(out, &report, options.ignore_case, true)?;
    }
    matching::hits(&mut archive, &report, options.raw.is_some(), options.json, out, budget, canceled)?;
    if options.json { out.literal("}\n")?; }
    Ok(matching::exit(&report))
}
fn fields(out: &mut Output) -> Result<(), Failure> { search_fields(out, false) }
fn search_fields(out: &mut Output, ignore_case: bool) -> Result<(), Failure> {
    out.literal(",\"index_layout\":\"demand-paged-postings-v1\",\"candidate_strategy\":")?;
    out.quoted(if ignore_case { "direct-verified-member-scan" } else { "rarest-posting-intersection" })?;
    out.literal(",\"index_pin_scope\":\"page-manifest-envelope-sha256\",\"index_body_verified_on_open\":false,\"index_page_verification\":\"sha256-before-use\"")?;
    out.literal(",\"index_page_bytes\":")?; out.integer(POSTING_PAGE_BYTES as u64)?; Ok(())
}
fn io_fields(out: &mut Output, io: PostingPageIo, file_bytes: u64, capacity: usize) -> Result<(), Failure> {
    out.literal(",\"index_bytes\":")?; out.integer(file_bytes)?;
    out.literal(",\"index_open_bytes_read\":")?; out.integer(io.manifest_bytes_read)?;
    out.literal(",\"index_page_bytes_read\":")?; out.integer(io.page_bytes_read)?;
    out.literal(",\"index_read_calls\":")?; out.integer(io.read_calls)?;
    out.literal(",\"index_page_loads\":")?; out.integer(io.page_loads)?;
    out.literal(",\"index_page_cache_hits\":")?; out.integer(io.cache_hits)?;
    out.literal(",\"index_page_evictions\":")?; out.integer(io.evictions)?;
    out.literal(",\"index_cache_capacity_bytes\":")?; out.integer(capacity as u64)?;
    Ok(())
}
