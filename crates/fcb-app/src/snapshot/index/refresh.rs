#![forbid(unsafe_code)]

//! Composition only: pinned prior segments + target snapshot -> new artifact.
//! The existing exact search engine, catalog opener and no-overwrite writer own
//! their respective contracts. No live roots, config discovery, or implicit pins.

use super::*;

pub(super) fn execute(options: &Options, out: &mut Output, budget: &ResourceBudget,
    effect: &mut Effect, canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if canceled() { return Err(Failure::canceled()); }
    let base_path = options.base.as_deref().ok_or_else(|| Failure::new("SAVED_INDEX_BASE_REQUIRED"))?;
    let base = catalog::open(base_path, options.base_catalog.as_deref(), options.base_catalog_pin,
        budget, [allocation(151), allocation(161)], &mut *canceled)?;
    let base_digest = base.directory().digest();
    let base_validation = base.directory().validation_stats();
    let base_fully_verified = base.directory().fully_verified_on_open();
    let old_pin = options.pin.ok_or_else(|| Failure::new("SAVED_INDEX_PIN_REQUIRED"))?;
    let loaded = load_index(options.index.as_deref().ok_or_else(|| Failure::new("SAVED_INDEX_REQUIRED"))?, budget, &mut *canceled)?;
    let old_index_bytes = loaded.bytes.len();
    let old = SnapshotIndex::decode_pinned(&loaded.bytes, old_pin, base.directory(), budget, allocation(152), &mut *canceled)?;
    drop(loaded);
    drop(base); // All reusable source keys and semantics are owned by `old`.
    let target_path = options.archive.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?;
    let mut target = catalog::open(target_path, options.catalog.as_deref(), options.catalog_pin,
        budget, [allocation(151), allocation(161)], &mut *canceled)?;
    let refreshed = old.refresh(&mut target, options.build, budget,
        [allocation(170), allocation(171), allocation(172), allocation(173), allocation(174)], &mut *canceled)?;
    let counters = refreshed.stats();
    drop(old); // New segments are independent; this never changes old disk files.
    let next = refreshed.into_index();
    let artifact = next.encode(budget, allocation(156), &mut *canceled)?;
    let destination = input::absolute(options.output.as_deref().ok_or_else(|| Failure::new("SNAPSHOT_OUTPUT_REQUIRED"))?)?;
    write_new(&destination, artifact.bytes(), effect, canceled)?;
    // Once written, use the parent command's effect-aware receipt delivery.
    if options.json {
        begin(out, "snapshot-index-refresh")?;
        summary(out, target.directory(), next.stats(), artifact.digest())?;
        out.literal(",\"effect\":")?; out.quoted(effect.name())?;
        out.literal(",\"destination\":")?; out.path(&destination)?;
        out.literal(",\"index_bytes\":")?; out.integer(artifact.bytes().len() as u64)?;
        out.literal(",\"base_snapshot_digest\":")?; out.quoted(&base_digest.to_hex())?;
        out.literal(",\"base_index_digest\":")?; out.quoted(&old_pin.to_hex())?;
        out.literal(",\"base_index_bytes\":")?; out.integer(old_index_bytes as u64)?;
        out.literal(",\"base_archive_validation_bytes\":")?; out.integer(base_validation.bytes_read)?;
        out.literal(",\"base_archive_body_verified_on_open\":")?; out.boolean(base_fully_verified)?;
        out.literal(",\"reused_files\":")?; out.integer(counters.reused_files as u64)?;
        out.literal(",\"reused_source_bytes\":")?; out.integer(counters.reused_source_bytes)?;
        out.literal(",\"reused_grams\":")?; out.integer(counters.reused_grams as u64)?;
        out.literal(",\"rebuilt_files\":")?; out.integer(counters.rebuilt_files as u64)?;
        out.literal(",\"attempted_files\":")?; out.integer(counters.attempted_files as u64)?;
        out.literal(",\"reuse_quota_refusals\":")?; out.integer(counters.reuse_quota_refusals as u64)?;
        out.literal(",\"member_payload_bytes_loaded\":")?; out.integer(target.load_stats().bytes_read)?;
        out.literal(",\"loaded_members\":")?; out.integer(target.load_stats().loaded_members)?;
        out.literal(",\"new_segment_source_bytes_loaded\":")?; out.integer(counters.loaded_source_bytes)?;
        out.literal(",\"reuse_identity\":\"source-sha256-length-and-semantics\",\"rename_inferred\":false") ;
        out.literal(",\"source_derived_sensitive\":true,\"power_loss_qualified\":false}\n")?;
    } else {
        out.literal("Saved refreshed substring index. Retain its new trusted digest separately:\n")?;
        out.literal(&artifact.digest().to_hex())?; out.literal("\nReused files: ")?;
        out.literal(&counters.reused_files.to_string())?; out.literal("; newly indexed files: ")?;
        out.literal(&counters.rebuilt_files.to_string())?; out.literal("; uncovered files: ")?;
        out.literal(&next.stats().uncovered_files.to_string())?; out.literal("\n")?;
        if !base_fully_verified || !target.directory().fully_verified_on_open() {
            out.literal("Pinned catalogs skipped body validation. Unread backing integrity remains unchecked; every newly loaded member was digest-verified.\n")?;
        }
        out.literal("Reuse concerns exact saved content, not rename identity or live freshness. Prior artifacts are unchanged.\n")?;
    }
    Ok(if !target.directory().discovery_complete() || next.stats().unavailable_files > 0 || next.stats().uncovered_files > 0 {
        EXIT_PARTIAL
    } else { EXIT_OK })
}
