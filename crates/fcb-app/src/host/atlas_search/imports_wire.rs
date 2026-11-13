#![forbid(unsafe_code)]

//! Bounded receipts for the lexical import projection. Every page preserves
//! occurrence IDs and exact captured evidence; filters do not renumber rows.
use super::*;

fn identity(out: &mut Output, snapshot: &ImportsSnapshot) -> Result<(), AtlasSearchError> {
    for (key, value) in [("relationship_generation", snapshot.generation),
        ("index_generation", snapshot.index_generation), ("capture_manifest", snapshot.manifest)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    Ok(())
}
fn contract(out: &mut Output) -> Result<(), AtlasSearchError> {
    out.literal(",\"projection\":\"lexical-source-imports\",\"relationship_kind\":\"lexical-import\",\"evidence_precision\":\"exact-import-source-range\",\"resolution_domain\":\"declared-catalog-candidates\",\"compiler_resolved\":false,\"semantic_complete\":false,\"extractor\":")?;
    out.quoted(IMPORT_EXTRACTOR)?;
    out.literal(",\"resolver\":\"fcb-closed-catalog-imports-v3\",\"extraction_policy\":")?; out.quoted(IMPORT_POLICY)?;
    Ok(())
}
fn summary(out: &mut Output, atlas: &AtlasSession, index: &RetainedIndex, snapshot: &ImportsSnapshot)
    -> Result<(), AtlasSearchError> {
    identity(out, snapshot)?; contract(out)?;
    index.capture_scope.encode(out)?;
    out.literal(",\"source_observation\":\"retained-index-captures\"")?;
    for (key, value) in [("max_links", snapshot.max_links as u64),
        ("max_parse_bytes", snapshot.max_parse_bytes), ("max_source_bytes", MAX_IMPORT_SOURCE_BYTES as u64),
        ("max_metadata_bytes", MAX_ATLAS_IMPORT_METADATA_BYTES as u64), ("metadata_bytes", snapshot.metadata_bytes as u64),
        ("source_bytes_read", 0), ("read_calls", 0), ("parsed_source_bytes", snapshot.parsed_bytes),
        ("hashed_source_bytes", snapshot.hashed_bytes), ("source_files", snapshot.eligible as u64),
        ("captured_source_files", snapshot.sources.len() as u64),
        ("capture_unavailable_source_files", snapshot.capture_unavailable as u64),
        ("outside_capture_scope_source_files", snapshot.outside_scope as u64),
        ("uncaptured_source_files", snapshot.uncaptured as u64),
        ("examined_source_files", snapshot.examined as u64),
        ("pending_source_files", (snapshot.sources.len() - snapshot.examined) as u64),
        ("scanned_source_files", snapshot.scanned as u64), ("unsupported_source_files", snapshot.unsupported as u64),
        ("source_limited_source_files", snapshot.source_limited as u64),
        ("parser_diagnostics", snapshot.diagnostics), ("links_seen", snapshot.links_seen),
        ("retained_links", snapshot.links.len() as u64), ("steps", snapshot.steps), ("last_step_files", snapshot.last_files as u64)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    for (key, value) in [("relationships_in_progress", snapshot.stop.is_none()),
        ("relationships_complete", snapshot.complete), ("discovery_complete", atlas.atlas().discovery_complete()),
        ("capture_region_complete", index.capture_scope.all_requested_catalogued()), ("truncated", snapshot.truncated)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.boolean(value)?;
    }
    out.literal(",\"stop_reason\":")?;
    if let Some(stop) = snapshot.stop { out.quoted(stop.name())?; } else { out.literal("null")?; }
    Ok(())
}
pub(super) fn page(out: &mut Output, atlas: &AtlasSession, index: &RetainedIndex, snapshot: &ImportsSnapshot,
    filter: Option<FileId>, direction: AtlasImportDirection, start: usize, limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
    let matching = snapshot.links.iter().filter(|link| link.matches(filter, direction)).count();
    if start > matching { return Err(AtlasSearchError::InvalidLimits); }
    summary(out, atlas, index, snapshot)?;
    out.literal(",\"direction\":")?; out.quoted(direction.name())?;
    out.literal(",\"filter_file_id\":")?;
    if let Some(file) = filter { out.integer(file.get())?; } else { out.literal("null")?; }
    out.literal(",\"filter_path\":")?;
    if let Some(file) = filter { path(out, atlas, file)?; } else { out.literal("null")?; }
    for (key, value) in [("offset", start), ("limit", limit), ("matching_links", matching)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
    }
    out.literal(",\"links\":[")?;
    let mut emitted = 0;
    for (ordinal, link) in snapshot.links.iter().enumerate().filter(|(_, link)| link.matches(filter, direction))
        .skip(start).take(limit) {
        check(canceled)?;
        let checkpoint = out.checkpoint();
        let encoded = (|| {
            if emitted != 0 { out.literal(",")?; }
            row(out, atlas, link, ordinal as u64 + 1)?;
            if out.remaining_bytes() < 128 { return Err(AtlasSearchError::from(OutputError::Limit)); }
            Ok(())
        })();
        match encoded {
            Ok(()) => emitted += 1,
            Err(AtlasSearchError::App(AppError::Output(OutputError::Limit))) if emitted > 0 => { out.restore(checkpoint)?; break; }
            Err(error) => return Err(error),
        }
    }
    out.literal("],\"next_offset\":")?;
    let next = start.checked_add(emitted).ok_or(AppError::Admission)?;
    if next < matching { out.integer(next as u64)?; } else { out.literal("null")?; }
    out.literal("}\n")?;
    Ok(())
}
fn row(out: &mut Output, atlas: &AtlasSession, link: &Link, id: u64) -> Result<(), AtlasSearchError> {
    out.literal("{\"link_id\":")?; out.integer(id)?;
    out.literal(",\"source_file_id\":")?; out.integer(link.source.file.get())?;
    out.literal(",\"source_revision\":")?; out.integer(link.source.revision.get())?;
    out.literal(",\"source_path\":")?; path(out, atlas, link.source.file)?;
    out.literal(",\"source_sha256\":")?; out.quoted(&link.source.digest.to_hex())?;
    out.literal(",\"source_bytes\":")?; out.integer(link.source.bytes)?;
    out.literal(",\"source_link_id\":")?; out.integer(link.source_link)?;
    out.literal(",\"evidence_range\":")?; out.range(link.evidence)?;
    out.literal(",\"specifier_range\":")?; out.range(link.specifier)?;
    out.literal(",\"language\":")?; out.quoted(link.language.name())?;
    out.literal(",\"lexical_import_kind\":")?; out.quoted(link.kind.name())?;
    out.literal(",\"destination\":")?; out.quoted(&link.destination)?;
    out.literal(",\"resolution\":")?; out.quoted(link.resolution.name())?;
    out.literal(",\"resolution_basis\":")?; out.quoted(link.basis)?;
    out.literal(",\"target_file_id\":")?;
    if let Some(file) = link.target_file { out.integer(file.get())?; } else { out.literal("null")?; }
    out.literal(",\"target_path\":")?;
    if let Some(file) = link.target_file { path(out, atlas, file)?; } else { out.literal("null")?; }
    out.literal(",\"target_revision\":")?;
    if let Some(target) = link.target { out.integer(target.revision.get())?; } else { out.literal("null")?; }
    out.literal(",\"target_sha256\":")?;
    if let Some(target) = link.target { out.quoted(&target.digest.to_hex())?; } else { out.literal("null")?; }
    out.literal(",\"target_bytes\":")?;
    if let Some(target) = link.target { out.integer(target.bytes)?; } else { out.literal("null")?; }
    out.literal("}")?;
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(super) fn reader(out: &mut Output, atlas: &AtlasSession, snapshot: &ImportsSnapshot,
    link: &Link, id: u64, side: AtlasImportSide, selected: SourceProof, reader_owner: ArenaOwnerId,
    info: &HostResponse) -> Result<(), AtlasSearchError> {
    identity(out, snapshot)?; contract(out)?;
    out.literal(",\"link_id\":")?; out.integer(id)?;
    out.literal(",\"side\":")?; out.quoted(side.name())?;
    out.literal(",\"relationship\":")?; row(out, atlas, link, id)?;
    out.literal(",\"evidence_range\":")?; out.range(link.evidence)?;
    out.literal(",\"file_id\":")?; out.integer(selected.file.get())?;
    out.literal(",\"source_revision\":")?; out.integer(selected.revision.get())?;
    out.literal(",\"path\":")?; path(out, atlas, selected.file)?;
    out.literal(",\"source_sha256\":")?; out.quoted(&selected.digest.to_hex())?;
    out.literal(",\"source_bytes\":")?; out.integer(selected.bytes)?;
    out.literal(",\"source_observation\":\"retained-index-capture\",\"source_reopened\":false,\"additional_source_bytes_read\":\"0\",\"initial_original_offset\":")?;
    out.integer(match side { AtlasImportSide::Evidence => link.evidence.start().get(), AtlasImportSide::Target => 0 })?;
    out.literal(",\"reader_owner\":")?; out.integer(reader_owner.get())?;
    out.literal(",\"reader\":")?; out.literal(info.as_str())?; out.literal("}\n")?;
    Ok(())
}
fn path(out: &mut Output, atlas: &AtlasSession, file: FileId) -> Result<(), AtlasSearchError> {
    let entry = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
    out.path(&entry.path().raw().to_path_buf())?;
    Ok(())
}
