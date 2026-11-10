#![forbid(unsafe_code)]

//! Bounded receipts over retained relationship metadata. Pages preserve global
//! occurrence IDs; a filter never renumbers links or reinterprets source bytes.

use super::*;

fn identity(out: &mut Output, snapshot: &RelationshipSnapshot) -> Result<(), AtlasSearchError> {
    for (key, value) in [("relationship_generation", snapshot.generation),
        ("index_generation", snapshot.index_generation), ("capture_manifest", snapshot.manifest)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    Ok(())
}
fn summary(out: &mut Output, atlas: &AtlasSession, index: &RetainedIndex, snapshot: &RelationshipSnapshot)
    -> Result<(), AtlasSearchError> {
    identity(out, snapshot)?;
    index.capture_scope.encode(out)?;
    out.literal(",\"source_observation\":\"retained-index-captures\",\"evidence_precision\":\"whole-captured-markdown-document\",\"relationship_kind\":\"parsed-markdown-link\",\"resolution_domain\":\"exact-catalog-relative-path\"")?;
    for (key, value) in [("max_links", snapshot.max_links as u64),
        ("max_parse_bytes", snapshot.max_parse_bytes), ("max_markdown_source_bytes", MAX_DOCUMENT_LINK_SOURCE_BYTES as u64),
        ("max_metadata_bytes", MAX_ATLAS_RELATIONSHIP_METADATA_BYTES as u64), ("metadata_bytes", snapshot.metadata_bytes as u64),
        ("source_bytes_read", 0), ("read_calls", 0), ("parsed_source_bytes", snapshot.parsed_bytes),
        ("hashed_source_bytes", snapshot.hashed_bytes), ("markdown_files", snapshot.markdown as u64),
        ("captured_markdown_files", snapshot.sources.len() as u64),
        ("capture_unavailable_markdown_files", snapshot.capture_unavailable as u64),
        ("outside_capture_scope_markdown_files", snapshot.outside_scope as u64),
        ("uncaptured_markdown_files", snapshot.uncaptured as u64),
        ("examined_markdown_files", snapshot.examined as u64),
        ("pending_markdown_files", (snapshot.sources.len() - snapshot.examined) as u64),
        ("scanned_markdown_files", snapshot.scanned as u64), ("unsupported_markdown_files", snapshot.unsupported as u64),
        ("source_limited_markdown_files", snapshot.source_limited as u64), ("parser_diagnostics", snapshot.diagnostics),
        ("links_seen", snapshot.links_seen), ("retained_links", snapshot.links.len() as u64),
        ("steps", snapshot.steps), ("last_step_files", snapshot.last_files as u64)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    for (key, value) in [("relationships_in_progress", snapshot.stop.is_none()),
        ("relationships_complete", snapshot.complete), ("discovery_complete", atlas.atlas().discovery_complete()),
        ("capture_region_complete", index.capture_scope.all_requested_catalogued()), ("truncated", snapshot.truncated)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.boolean(value)?;
    }
    out.literal(",\"stop_reason\":")?;
    match snapshot.stop { Some(stop) => out.quoted(stop.name())?, None => out.literal("null")? }
    Ok(())
}

pub(super) fn page(out: &mut Output, atlas: &AtlasSession, index: &RetainedIndex, snapshot: &RelationshipSnapshot,
    filter: Option<FileId>, direction: AtlasRelationshipDirection, start: usize, limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
    let matching = snapshot.links.iter().filter(|link| link.matches(filter, direction)).count();
    if start > matching { return Err(AtlasSearchError::InvalidLimits); }
    summary(out, atlas, index, snapshot)?;
    out.literal(",\"direction\":")?; out.quoted(direction.name())?;
    out.literal(",\"filter_file_id\":")?;
    match filter { Some(file) => out.integer(file.get())?, None => out.literal("null")? }
    out.literal(",\"filter_path\":")?;
    match filter { Some(file) => path(out, atlas, file)?, None => out.literal("null")? }
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
            // Keep the bounded footer publishable even for long escaped paths.
            if out.remaining_bytes() < 128 { return Err(AtlasSearchError::from(OutputError::Limit)); }
            Ok(())
        })();
        match encoded {
            Ok(()) => emitted += 1,
            Err(AtlasSearchError::App(AppError::Output(OutputError::Limit))) if emitted > 0 => {
                out.restore(checkpoint)?; break;
            }
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
    out.literal(",\"block_index\":")?; out.integer(link.block as u64)?;
    out.literal(",\"evidence_range\":")?; out.range(link.evidence)?;
    out.literal(",\"destination\":")?; out.quoted(&link.destination)?;
    out.literal(",\"resolution\":")?; out.quoted(link.resolution.name())?;
    out.literal(",\"target_file_id\":")?;
    match link.target_file { Some(file) => out.integer(file.get())?, None => out.literal("null")? }
    out.literal(",\"target_path\":")?;
    match link.target_file { Some(file) => path(out, atlas, file)?, None => out.literal("null")? }
    out.literal(",\"target_revision\":")?;
    match link.target { Some(target) => out.integer(target.revision.get())?, None => out.literal("null")? }
    out.literal(",\"target_sha256\":")?;
    match link.target { Some(target) => out.quoted(&target.digest.to_hex())?, None => out.literal("null")? }
    out.literal(",\"target_bytes\":")?;
    match link.target { Some(target) => out.integer(target.bytes)?, None => out.literal("null")? }
    fragment(out, link)?;
    out.literal("}")?;
    Ok(())
}

pub(super) fn reader(out: &mut Output, atlas: &AtlasSession, snapshot: &RelationshipSnapshot, link: &Link,
    id: u64, side: AtlasRelationshipSide, source: SourceProof, reader_owner: ArenaOwnerId, info: &HostResponse)
    -> Result<(), AtlasSearchError> {
    identity(out, snapshot)?;
    out.literal(",\"link_id\":")?; out.integer(id)?;
    out.literal(",\"side\":")?; out.quoted(side.name())?;
    out.literal(",\"file_id\":")?; out.integer(source.file.get())?;
    out.literal(",\"source_revision\":")?; out.integer(source.revision.get())?;
    out.literal(",\"path\":")?; path(out, atlas, source.file)?;
    out.literal(",\"source_sha256\":")?; out.quoted(&source.digest.to_hex())?;
    out.literal(",\"source_bytes\":")?; out.integer(source.bytes)?;
    out.literal(",\"source_observation\":\"retained-index-capture\",\"source_reopened\":false,\"additional_source_bytes_read\":\"0\",\"evidence_precision\":\"whole-captured-markdown-document\",\"reader_owner\":")?;
    out.integer(reader_owner.get())?;
    fragment(out, link)?;
    out.literal(",\"reader\":")?; out.literal(info.as_str().trim_end())?;
    out.literal("}\n")?;
    Ok(())
}
fn fragment(out: &mut Output, link: &Link) -> Result<(), AtlasSearchError> {
    out.literal(",\"destination_suffix\":")?; out.quoted(&link.suffix)?;
    out.literal(",\"fragment_resolution\":")?; out.quoted(if link.suffix.is_empty() { "none" } else { "unqualified" })?;
    Ok(())
}
fn path(out: &mut Output, atlas: &AtlasSession, file: FileId) -> Result<(), AtlasSearchError> {
    let entry = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
    out.path(&entry.path().raw().to_path_buf())?;
    Ok(())
}
