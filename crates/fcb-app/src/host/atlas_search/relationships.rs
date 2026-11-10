#![forbid(unsafe_code)]

//! Parsed Markdown relationships in the retained project index. The existing
//! upstream parser and resolver own link semantics; the original atlas owns
//! exact native-path membership. No working-tree source is opened here.
//! One step parses at most one captured Markdown member. Whole-document
//! evidence is explicit because the parser does not qualify link-token spans.

use fcb::document::links::{CapturedDocumentLinks, DocumentLinkError,
    MAX_DOCUMENT_LINK_SOURCE_BYTES, MAX_DOCUMENT_LINKS, MAX_DOCUMENT_LINK_DESTINATION_BYTES,
    resolve_document_destination};
use fcb::store::Sha256Digest;
use crate::host::document_fragment::markdown;
use super::*;

#[path = "relationships_wire.rs"]
mod wire;

pub const MAX_ATLAS_RELATIONSHIPS: usize = MAX_DOCUMENT_LINKS;
pub const MAX_ATLAS_RELATIONSHIP_PARSE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_ATLAS_RELATIONSHIP_METADATA_BYTES: usize = 8 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasRelationshipDirection { All, Outgoing, Incoming }
impl AtlasRelationshipDirection {
    pub const fn name(self) -> &'static str {
        match self { Self::All => "all", Self::Outgoing => "outgoing", Self::Incoming => "incoming" }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasRelationshipSide { Evidence, Target }
impl AtlasRelationshipSide {
    pub const fn name(self) -> &'static str { match self { Self::Evidence => "evidence", Self::Target => "target" } }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Resolution { Captured, Unavailable, Uncaptured, OutsideScope, Missing, External, Unresolved }
impl Resolution {
    fn name(self) -> &'static str {
        match self { Self::Captured => "captured", Self::Unavailable => "unavailable", Self::Uncaptured => "uncaptured",
            Self::OutsideScope => "outside-capture-scope", Self::Missing => "missing",
            Self::External => "external", Self::Unresolved => "unresolved" }
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum Stop { All, ParseBytes }
impl Stop { fn name(self) -> &'static str { match self { Self::All => "all-markdown-examined", Self::ParseBytes => "parse-byte-limit" } } }

#[derive(Clone, Copy)]
struct SourceProof { file: FileId, revision: SourceRevision, digest: Sha256Digest, bytes: u64 }
struct Link {
    source: SourceProof, source_link: u64, block: usize, evidence: ByteRange,
    destination: String, suffix: String, resolution: Resolution,
    target_file: Option<FileId>, target: Option<SourceProof>,
}
impl Link {
    fn matches(&self, file: Option<FileId>, direction: AtlasRelationshipDirection) -> bool {
        match (file, direction) {
            (None, AtlasRelationshipDirection::All) => true,
            (Some(file), AtlasRelationshipDirection::All) => self.source.file == file || self.target_file == Some(file),
            (Some(file), AtlasRelationshipDirection::Outgoing) => self.source.file == file,
            (Some(file), AtlasRelationshipDirection::Incoming) => self.target_file == Some(file),
            _ => false,
        }
    }
}

pub(crate) struct RelationshipSnapshot {
    generation: u64, index_generation: u64, manifest: u64,
    max_links: usize, max_parse_bytes: u64, metadata_bytes: usize,
    // Captured Markdown IDs and source witnesses are bounded by the prepared
    // index, not by the total discovery catalog. Witnesses hash each retained
    // source at most once across the build; paging never reads or hashes bytes.
    sources: Vec<FileId>, proofs: Vec<SourceProof>, links: Vec<Link>,
    markdown: usize, capture_unavailable: usize, outside_scope: usize, uncaptured: usize,
    examined: usize, scanned: usize, unsupported: usize, source_limited: usize,
    diagnostics: u64, links_seen: u64, parsed_bytes: u64, hashed_bytes: u64,
    truncated: bool, stop: Option<Stop>, complete: bool, steps: u64, last_files: usize,
    _lease: ResourceLease,
}
pub(crate) struct RelationshipsWork {
    snapshot: RelationshipSnapshot,
    document_id: ResourceAllocationId,
    scratch_id: ResourceAllocationId,
}
impl From<DocumentLinkError> for AtlasSearchError {
    fn from(error: DocumentLinkError) -> Self {
        match error { DocumentLinkError::Canceled => Self::Canceled, error => Self::Document(error) }
    }
}

impl RetainedAtlasSearch {
    pub fn relationship_generation(&self) -> Option<u64> { self.relationships.as_ref().map(|s| s.generation) }
    pub fn pending_relationship_generation(&self) -> Option<u64> {
        self.relationships_pending.as_ref().map(|work| work.snapshot.generation)
    }
    /// Worker-side retirement preserves accepted metadata and every delivered
    /// reader. Search and index attempt generations are not consumed here.
    pub fn cancel_relationships_pending(&mut self) -> bool { self.relationships_pending.take().is_some() }

    /// Freeze the prepared index and catalog coverage without parsing or source
    /// I/O. Relative filters used later refer to this atlas's own namespace.
    pub fn begin_relationships(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, max_links: usize, max_parse_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        if generation == 0 || generation <= self.last_relationship_attempt { return Err(AtlasSearchError::StaleRelationships); }
        self.last_relationship_attempt = generation;
        self.relationships_pending = None;
        if !(1..=MAX_ATLAS_RELATIONSHIPS).contains(&max_links)
            || max_parse_bytes > MAX_ATLAS_RELATIONSHIP_PARSE_BYTES { return Err(AtlasSearchError::InvalidLimits); }
        check(&mut canceled)?;
        let [state_id, document_id, scratch_id] = [self.next_id()?, self.next_id()?, self.next_id()?];
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if index.generation != index_generation { return Err(AtlasSearchError::StaleIndex); }
        let capacity = index.engine.captured_files();
        let fixed = max_links.checked_mul(size_of::<Link>())
            .and_then(|n| capacity.checked_mul(size_of::<FileId>() + size_of::<SourceProof>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(size_of::<RelationshipsWork>() + 4096)).ok_or(AppError::Admission)?;
        if fixed > MAX_ATLAS_RELATIONSHIP_METADATA_BYTES { return Err(AtlasSearchError::InvalidLimits); }
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id,
            ByteLength::new(MAX_ATLAS_RELATIONSHIP_METADATA_BYTES as u64)).map_err(|_| AppError::Admission)?;
        let mut snapshot = RelationshipSnapshot { generation, index_generation, manifest: index.engine.id().revision(),
            max_links, max_parse_bytes, metadata_bytes: fixed, sources: reserve(capacity)?, proofs: reserve(capacity)?,
            links: reserve(max_links)?, markdown: 0, capture_unavailable: 0, outside_scope: 0, uncaptured: 0,
            examined: 0, scanned: 0, unsupported: 0, source_limited: 0, diagnostics: 0, links_seen: 0,
            parsed_bytes: 0, hashed_bytes: 0, truncated: false, stop: None, complete: false,
            steps: 0, last_files: 0, _lease: lease };
        let catalog = atlas.atlas().catalog();
        for (ordinal, entry) in catalog.entries().iter().enumerate() {
            check(&mut canceled)?;
            if !markdown(entry.path().raw().as_bytes()) { continue; }
            snapshot.markdown += 1;
            let file = catalog.file_id(ordinal).ok_or(AtlasSearchError::WrongAtlas)?;
            if index.engine.capture(file).is_some() {
                if snapshot.sources.len() == capacity { return Err(AtlasSearchError::WrongAtlas); }
                snapshot.sources.push(file);
            } else if index.engine.unavailable_files().binary_search(&file).is_ok() {
                snapshot.capture_unavailable += 1;
            } else if !index.capture_scope.scope.includes_path(entry.path().raw().as_bytes()) {
                snapshot.outside_scope += 1;
            } else { snapshot.uncaptured += 1; }
        }
        self.validate(atlas)?; check(&mut canceled)?;
        let work = RelationshipsWork { snapshot, document_id, scratch_id };
        let response = self.relationship_response(atlas, &work.snapshot, "links-begin", &mut canceled)?;
        self.relationships_pending = Some(work);
        Ok(response)
    }

    /// Parse one captured Markdown member and append stable occurrence IDs.
    /// Failed/canceled replacements leave the older accepted inventory intact.
    pub fn step_relationships(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        if self.pending_relationship_generation() != Some(generation) {
            if self.relationships_pending.is_none() && self.relationship_generation() == Some(generation)
                && self.last_relationship_attempt == generation {
                let mut out = self.relationship_output("links-step")?;
                let snapshot = self.relationship_snapshot(generation)?;
                let index = self.relationship_index(snapshot)?;
                wire::page(&mut out, atlas, index, snapshot, None, AtlasRelationshipDirection::All, 0, 64, &mut canceled)?;
                let partial = !snapshot.complete;
                return self.finish(atlas, out, partial, &mut canceled);
            }
            return Err(AtlasSearchError::StaleRelationships);
        }
        let mut work = self.relationships_pending.take().ok_or(AtlasSearchError::MissingRelationships)?;
        self.advance_relationships(atlas, &mut work, &mut canceled)?;
        let response = self.relationship_response(atlas, &work.snapshot, "links-step", &mut canceled)?;
        if work.snapshot.stop.is_some() { self.relationships = Some(work.snapshot); }
        else { self.relationships_pending = Some(work); }
        Ok(response)
    }

    /// Exact catalog path lookup precedes filtering. A missing path is an
    /// explicit error, never an empty successful backlink result or a new grant.
    pub fn relationships_page(&mut self, atlas: &AtlasSession, generation: u64,
        direction: AtlasRelationshipDirection, relative_path: Option<&[u8]>, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        if !(1..=MAX_ATLAS_SEARCH_PAGE).contains(&limit)
            || (relative_path.is_none() && direction != AtlasRelationshipDirection::All) {
            return Err(AtlasSearchError::InvalidLimits);
        }
        let filter = match relative_path {
            Some(path) => {
                if path.is_empty() || path.len() > MAX_SEARCH_SCOPE_PATH_BYTES || path.contains(&0) {
                    return Err(AtlasSearchError::InvalidScope);
                }
                Some(find_file(atlas, path).ok_or(AtlasSearchError::MissingRelationshipPath)?)
            }
            None => None,
        };
        let mut out = self.relationship_output("links-page")?;
        let snapshot = self.relationship_snapshot(generation)?;
        let index = self.relationship_index(snapshot)?;
        wire::page(&mut out, atlas, index, snapshot, filter, direction, start, limit, &mut canceled)?;
        let partial = !snapshot.complete;
        self.finish(atlas, out, partial, &mut canceled)
    }

    /// Open an independently owned exact capture. Fragment suffixes remain
    /// unqualified and select no heading/line; the ordinary reader starts at 0.
    /// Metadata/source replacement or index clearing invalidates old edges.
    pub fn open_relationship_reader(&mut self, atlas: &AtlasSession, reader_owner: ArenaOwnerId,
        generation: u64, id: u64, side: AtlasRelationshipSide, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        if reader_owner == self.manifest.owner() { return Err(AtlasSearchError::WrongAtlas); }
        let mut out = self.relationship_output("links-open-reader")?;
        let snapshot = self.relationship_snapshot(generation)?;
        let index = self.relationship_index(snapshot)?;
        let ordinal = id.checked_sub(1).and_then(|id| usize::try_from(id).ok()).ok_or(AtlasSearchError::MissingRelationship)?;
        let link = snapshot.links.get(ordinal).ok_or(AtlasSearchError::MissingRelationship)?;
        let proof = match side {
            AtlasRelationshipSide::Evidence => link.source,
            AtlasRelationshipSide::Target => link.target.ok_or(AtlasSearchError::RelationshipTargetUnavailable)?,
        };
        let source = index.engine.capture(proof.file).ok_or(AtlasSearchError::StaleIndex)?;
        if source.request().revision() != proof.revision || source.bytes().len() as u64 != proof.bytes {
            return Err(AtlasSearchError::StaleIndex);
        }
        let label = atlas.atlas().catalog().entry(proof.file).ok_or(AtlasSearchError::WrongAtlas)?.path().raw().to_path_buf();
        let mut stop = || canceled() || atlas.validate_active().is_err();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, source.bytes(), &mut stop)?;
        if reader.capture_sha256() != proof.digest { return Err(AtlasSearchError::StaleIndex); }
        let info = reader.info(&mut stop)?;
        wire::reader(&mut out, atlas, snapshot, link, id, side, proof, reader_owner, &info)?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        Ok((reader, response))
    }

    fn relationship_snapshot(&self, generation: u64) -> Result<&RelationshipSnapshot, AtlasSearchError> {
        if let Some(work) = &self.relationships_pending {
            if work.snapshot.generation == generation { return Ok(&work.snapshot); }
        }
        let accepted = self.relationships.as_ref().ok_or(AtlasSearchError::MissingRelationships)?;
        if accepted.generation != generation { return Err(AtlasSearchError::StaleRelationships); }
        Ok(accepted)
    }
    fn relationship_index(&self, snapshot: &RelationshipSnapshot) -> Result<&RetainedIndex, AtlasSearchError> {
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if index.generation != snapshot.index_generation || index.engine.id().revision() != snapshot.manifest {
            return Err(AtlasSearchError::StaleIndex);
        }
        Ok(index)
    }
    fn relationship_output(&mut self, command: &str) -> Result<Output, AtlasSearchError> {
        let id = self.next_id()?;
        // Native relationship transports admit at most four MiB. The page
        // encoder shortens rows before this bound while retaining continuation.
        let mut out = Output::new(self.manifest.owner(), 4 * 1024 * 1024, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.atlas-links/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.manifest.owner().get())?;
        out.literal(",\"source_manifest\":")?; out.integer(self.manifest.revision())?;
        out.literal(",\"layout_revision\":")?; out.integer(self.layout.get())?;
        Ok(out)
    }
    fn relationship_response(&mut self, atlas: &AtlasSession, snapshot: &RelationshipSnapshot,
        command: &str, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let mut out = self.relationship_output(command)?;
        let index = self.relationship_index(snapshot)?;
        wire::page(&mut out, atlas, index, snapshot, None, AtlasRelationshipDirection::All, 0, 64, canceled)?;
        self.finish(atlas, out, !snapshot.complete, canceled)
    }

    fn advance_relationships(&self, atlas: &AtlasSession, work: &mut RelationshipsWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        self.validate(atlas)?; check(canceled)?;
        let index = self.relationship_index(&work.snapshot)?;
        let before = work.snapshot.examined;
        if work.snapshot.examined < work.snapshot.sources.len() {
            self.relationship_one_document(atlas, index, work, canceled)?;
        }
        let snapshot = &mut work.snapshot;
        if snapshot.examined == snapshot.sources.len() { snapshot.stop = Some(Stop::All); }
        snapshot.complete = snapshot.stop == Some(Stop::All) && atlas.atlas().discovery_complete()
            && index.capture_scope.all_requested_catalogued() && snapshot.capture_unavailable == 0
            && snapshot.outside_scope == 0 && snapshot.uncaptured == 0 && snapshot.unsupported == 0
            && snapshot.source_limited == 0 && snapshot.diagnostics == 0 && !snapshot.truncated;
        snapshot.steps = snapshot.steps.checked_add(1).ok_or(AtlasSearchError::IdentityExhausted)?;
        snapshot.last_files = snapshot.examined - before;
        self.validate(atlas)?; check(canceled)
    }

    fn relationship_one_document(&self, atlas: &AtlasSession, index: &RetainedIndex, work: &mut RelationshipsWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        let snapshot = &mut work.snapshot;
        let file = snapshot.sources[snapshot.examined];
        let capture = index.engine.capture(file).ok_or(AtlasSearchError::StaleIndex)?;
        if capture.bytes().len() > MAX_DOCUMENT_LINK_SOURCE_BYTES {
            snapshot.source_limited += 1; snapshot.examined += 1; return Ok(());
        }
        let entry = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
        let path = match entry.path().as_str() {
            Ok(path) if valid_source_path(path) => path,
            _ => { snapshot.unsupported += 1; snapshot.examined += 1; return Ok(()); }
        };
        let bytes = capture.bytes().len() as u64;
        if bytes > snapshot.max_parse_bytes.saturating_sub(snapshot.parsed_bytes) {
            snapshot.stop = Some(Stop::ParseBytes); return Ok(());
        }
        // Admit resolver scratch before invoking the upstream path policy. It
        // can temporarily retain the current parent path and decoded target.
        let scratch = path.len().checked_mul(16).and_then(|n| n.checked_add(16 * MAX_DOCUMENT_LINK_DESTINATION_BYTES + 4096))
            .ok_or(AppError::Admission)?;
        let _scratch = self.budget.try_reserve_managed(self.manifest.owner(), work.scratch_id,
            ByteLength::new(scratch as u64)).map_err(|_| AppError::Admission)?;
        let qgen = QueryGeneration::new(self.manifest.owner(), snapshot.generation)
            .map_err(|_| AtlasSearchError::IdentityExhausted)?;
        snapshot.parsed_bytes += bytes;
        let document = match CapturedDocumentLinks::build(capture.bytes(), *capture.request(), qgen,
            snapshot.max_links - snapshot.links.len(), &self.budget, work.document_id,
            || canceled() || atlas.validate_active().is_err()) {
            Ok(document) => document,
            Err(DocumentLinkError::InvalidUtf8 | DocumentLinkError::InvalidEvidence | DocumentLinkError::ComplexityLimit) => {
                snapshot.unsupported += 1; snapshot.examined += 1; return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        snapshot.scanned += 1;
        snapshot.diagnostics = snapshot.diagnostics.checked_add(document.diagnostics() as u64)
            .ok_or(AtlasSearchError::IdentityExhausted)?;
        snapshot.links_seen = snapshot.links_seen.checked_add(document.links_seen()).ok_or(AtlasSearchError::IdentityExhausted)?;
        snapshot.truncated |= document.truncated();
        for link in document.links() {
            check(canceled)?;
            let (resolution, target_file, suffix) = resolve(atlas, index, path, file, link.destination())?;
            let text_bytes = link.destination().len().checked_add(suffix.len()).ok_or(AppError::Admission)?;
            if text_bytes > MAX_ATLAS_RELATIONSHIP_METADATA_BYTES - snapshot.metadata_bytes {
                snapshot.truncated = true; continue;
            }
            let source = proof(snapshot, index, file, canceled)?;
            let target = if resolution == Resolution::Captured {
                Some(proof(snapshot, index, target_file.ok_or(AtlasSearchError::WrongAtlas)?, canceled)?)
            } else { None };
            let destination = copy_text(link.destination())?;
            let suffix = copy_text(&suffix)?;
            snapshot.metadata_bytes += text_bytes;
            snapshot.links.push(Link { source, source_link: link.id(), block: link.block_index(),
                evidence: link.original_document_range(), destination, suffix, resolution, target_file, target });
        }
        snapshot.examined += 1;
        Ok(())
    }
}

fn proof(snapshot: &mut RelationshipSnapshot, index: &RetainedIndex, file: FileId,
    canceled: &mut impl FnMut() -> bool) -> Result<SourceProof, AtlasSearchError> {
    let position = match snapshot.proofs.binary_search_by_key(&file, |proof| proof.file) {
        Ok(position) => return Ok(snapshot.proofs[position]), Err(position) => position,
    };
    if snapshot.proofs.len() == snapshot.proofs.capacity() { return Err(AtlasSearchError::WrongAtlas); }
    let source = index.engine.capture(file).ok_or(AtlasSearchError::StaleIndex)?;
    let witness = CaptureWitness::new(source.bytes(), canceled)?;
    let proof = SourceProof { file, revision: source.request().revision(), digest: witness.digest(), bytes: witness.byte_length() };
    snapshot.hashed_bytes = snapshot.hashed_bytes.checked_add(proof.bytes)
        .filter(|&bytes| bytes <= index.engine.source_bytes()).ok_or(AtlasSearchError::WrongAtlas)?;
    snapshot.proofs.insert(position, proof);
    Ok(proof)
}

fn find_file(atlas: &AtlasSession, path: &[u8]) -> Option<FileId> {
    // WorkspaceCatalog freezes entries by exact native-path byte order before
    // assigning file IDs. This lookup never grants authority or normalizes text.
    let catalog = atlas.atlas().catalog();
    let ordinal = catalog.entries().binary_search_by(|entry| entry.path().raw().as_bytes().cmp(path)).ok()?;
    catalog.file_id(ordinal)
}
fn resolve(atlas: &AtlasSession, index: &RetainedIndex, source_path: &str, source_file: FileId, destination: &str)
    -> Result<(Resolution, Option<FileId>, String), AtlasSearchError> {
    let trimmed = destination.trim();
    let (file, suffix) = if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('?') {
        (Some(source_file), copy_text(trimmed)?)
    } else if let Some((path, suffix)) = resolve_document_destination(source_path, destination) {
        if path.len() > MAX_SEARCH_SCOPE_PATH_BYTES || suffix.len() > MAX_DOCUMENT_LINK_DESTINATION_BYTES {
            return Ok((Resolution::Unresolved, None, String::new()));
        }
        (find_file(atlas, path.as_bytes()), suffix)
    } else {
        return Ok((if external(trimmed) { Resolution::External } else { Resolution::Unresolved }, None, String::new()));
    };
    let Some(file) = file else { return Ok((Resolution::Missing, None, suffix)); };
    let resolution = if index.engine.capture(file).is_some() { Resolution::Captured }
        else if index.engine.unavailable_files().binary_search(&file).is_ok() { Resolution::Unavailable }
        else {
            let path = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?.path().raw().as_bytes();
            if index.capture_scope.scope.includes_path(path) { Resolution::Uncaptured } else { Resolution::OutsideScope }
        };
    Ok((resolution, Some(file), suffix))
}
fn valid_source_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_SEARCH_SCOPE_PATH_BYTES && !path.contains('\\')
        && !path.chars().any(char::is_control) && !external(path)
}
fn external(destination: &str) -> bool {
    destination.starts_with("//") || destination.split_once(':').is_some_and(|(scheme, _)| {
        scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && scheme.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'+' | b'-' | b'.'))
    })
}
