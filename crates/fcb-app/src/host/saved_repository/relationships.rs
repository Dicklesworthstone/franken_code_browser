#![forbid(unsafe_code)]

//! Documentation links and reverse backlinks in one closed saved manifest.
//! The upstream Markdown parser supplies destination semantics. Evidence is
//! the WHOLE original Markdown capture, because the pinned upstream block-span
//! collector is not reliable for every AST shape. The archive alone supplies
//! membership and source identity.
//! No live path, network URL, package resolver or inferred source grant is used.
//! One source member is resident during extraction; the accepted index owns
//! bounded metadata only. Publication preserves the old index on failure.

use std::mem::size_of;
use fcb::ByteRange;
use fcb::document::links::{CapturedDocumentLinks, DocumentLinkError,
    MAX_DOCUMENT_LINK_DESTINATION_BYTES, MAX_DOCUMENT_LINK_SOURCE_BYTES,
    MAX_DOCUMENT_LINKS, resolve_document_destination};
use fcb::search::CaptureRequest;
use super::{ArenaOwnerId, AppError, ByteLength, FileId, HostResponse, Output,
    OutputError, PagedCapture, PagedMemberData, PagedSearchError, QueryGeneration,
    RawPath, ReaderSession, ReaderSessionError, ResourceLease, SavedRepositoryError,
    SavedRepositorySession, Sha256Digest, SourceRevision, MAX_SAVED_PAGE, check, copy_text, optional};

pub const MAX_SAVED_RELATIONSHIPS: usize = MAX_DOCUMENT_LINKS;
pub const MAX_RELATIONSHIP_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationshipDirection { All, Outgoing, Incoming }
impl RelationshipDirection {
    pub const fn name(self) -> &'static str {
        match self { Self::All => "all", Self::Outgoing => "outgoing", Self::Incoming => "incoming" }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RelationshipSide { Evidence, Target }
impl RelationshipSide {
    pub const fn name(self) -> &'static str { match self { Self::Evidence => "evidence", Self::Target => "target" } }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedRelationshipError {
    Saved(SavedRepositoryError), Document(DocumentLinkError), InvalidLimits,
    MissingRelationships, StaleGeneration, MissingLink, UnresolvedTarget,
    TargetUnavailable, OwnerMismatch,
}
impl std::fmt::Display for SavedRelationshipError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saved(error) => write!(f, "{error}"), Self::Document(error) => write!(f, "{error}"),
            Self::InvalidLimits => f.write_str("SAVED_RELATIONSHIP_INVALID_LIMITS"),
            Self::MissingRelationships => f.write_str("SAVED_RELATIONSHIP_NOT_PREPARED"),
            Self::StaleGeneration => f.write_str("SAVED_RELATIONSHIP_STALE_GENERATION"),
            Self::MissingLink => f.write_str("SAVED_RELATIONSHIP_NOT_FOUND"),
            Self::UnresolvedTarget => f.write_str("SAVED_RELATIONSHIP_TARGET_UNRESOLVED"),
            Self::TargetUnavailable => f.write_str("SAVED_RELATIONSHIP_TARGET_UNAVAILABLE"),
            Self::OwnerMismatch => f.write_str("SAVED_RELATIONSHIP_OWNER_MISMATCH"),
        }
    }
}
impl std::error::Error for SavedRelationshipError {}
impl From<SavedRepositoryError> for SavedRelationshipError { fn from(e: SavedRepositoryError) -> Self { Self::Saved(e) } }
impl From<DocumentLinkError> for SavedRelationshipError { fn from(e: DocumentLinkError) -> Self { Self::Document(e) } }
impl From<OutputError> for SavedRelationshipError { fn from(e: OutputError) -> Self { Self::Saved(e.into()) } }
impl From<AppError> for SavedRelationshipError { fn from(e: AppError) -> Self { Self::Saved(e.into()) } }
impl From<PagedSearchError> for SavedRelationshipError { fn from(e: PagedSearchError) -> Self { Self::Saved(e.into()) } }
impl From<ReaderSessionError> for SavedRelationshipError { fn from(e: ReaderSessionError) -> Self { Self::Saved(e.into()) } }
impl SavedRelationshipError {
    pub fn is_canceled(self) -> bool {
        match self { Self::Saved(e) => e.is_canceled(), Self::Document(DocumentLinkError::Canceled) => true, _ => false }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Resolution { Captured, Unavailable, Missing, External, Unresolved }
impl Resolution {
    const fn name(self) -> &'static str {
        match self { Self::Captured => "captured", Self::Unavailable => "unavailable",
            Self::Missing => "missing", Self::External => "external", Self::Unresolved => "unresolved" }
    }
}
struct Link {
    source_member: usize, source_link_id: u64, block_index: usize,
    evidence: ByteRange, source_digest: Sha256Digest, destination: String,
    resolution: Resolution, target_member: Option<usize>, target_digest: Option<Sha256Digest>,
    suffix: String, _text_lease: ResourceLease,
}
impl Link {
    fn matches(&self, member: Option<usize>, direction: RelationshipDirection) -> bool {
        match (member, direction) {
            (None, RelationshipDirection::All) => true,
            (Some(id), RelationshipDirection::All) => self.source_member == id || self.target_member == Some(id),
            (Some(id), RelationshipDirection::Outgoing) => self.source_member == id,
            (Some(id), RelationshipDirection::Incoming) => self.target_member == Some(id),
            _ => false,
        }
    }
    fn fragment_unresolved(&self) -> bool { self.suffix.contains('#') }
}
pub(super) struct AcceptedRelationships {
    generation: u64, owner: ArenaOwnerId, archive: Sha256Digest, links: Vec<Link>,
    markdown: u64, scanned: u64, unavailable: u64, unsupported: u64,
    source_limited: u64, diagnostic: u64, links_seen: u64, truncated: bool,
    source_bytes: u64, read_calls: u64, complete: bool, _lease: ResourceLease,
}

/// Internal identity obtained only from an accepted relationship occurrence.
/// A byte-equal member in another repository is not interchangeable authority.
#[derive(Clone, Copy)]
pub(super) struct RelationshipTarget {
    pub member: usize, pub file: FileId, pub revision: SourceRevision,
    pub digest: Sha256Digest, pub selection: Option<ByteRange>, pub fragment_unresolved: bool,
}

impl SavedRepositorySession {
    pub fn relationship_generation(&self) -> Option<u64> {
        self.relationships.as_ref().map(|index| index.generation)
    }

    /// Extract every admitted Markdown member, with a total source-read quota.
    /// Missing/unsupported/oversized members and parser diagnostics are visible
    /// partial coverage; corrupt archive reads and cancellation fail replacement.
    /// Zero source bytes is a valid metadata-only partial probe.
    pub fn prepare_relationships(&mut self, generation: u64, max_links: usize,
        max_source_bytes: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRelationshipError> {
        relationship_attempt(&mut self.last_relationship_attempt, generation)?;
        check(&mut canceled)?;
        if !(1..=MAX_SAVED_RELATIONSHIPS).contains(&max_links)
            || max_source_bytes > MAX_RELATIONSHIP_SOURCE_BYTES { return Err(SavedRelationshipError::InvalidLimits); }
        let owner = self.owner();
        let qgen = QueryGeneration::new(owner, generation).map_err(|_| SavedRelationshipError::StaleGeneration)?;
        let [index_id] = self.allocations()?;
        let charge = max_links.checked_mul(size_of::<Link>())
            .and_then(|n| n.checked_add(size_of::<AcceptedRelationships>() + 4096)).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(owner, index_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut links = Vec::new(); links.try_reserve_exact(max_links).map_err(|_| AppError::Admission)?;
        if links.capacity() > max_links { return Err(AppError::Admission.into()); }
        let mut candidate = AcceptedRelationships { generation, owner, archive: self.archive_digest(), links,
            markdown: 0, scanned: 0, unavailable: 0, unsupported: 0, source_limited: 0,
            diagnostic: 0, links_seen: 0, truncated: false, source_bytes: 0, read_calls: 0,
            complete: false, _lease: lease };
        let before = self.archive.load_stats();
        let mut admitted_source_bytes = 0u64;
        for ordinal in 0..self.archive.directory().len() {
            check(&mut canceled)?;
            let member = self.archive.directory().member(ordinal).ok_or(SavedRelationshipError::MissingLink)?;
            if !markdown(member.path) { continue; }
            candidate.markdown += 1;
            let (length, digest) = match member.data {
                PagedMemberData::Captured { byte_length, digest, .. } => (byte_length, digest),
                PagedMemberData::Unavailable(_) => { candidate.unavailable += 1; continue; }
            };
            if length > MAX_DOCUMENT_LINK_SOURCE_BYTES
                || length as u64 > max_source_bytes.saturating_sub(admitted_source_bytes) {
                candidate.source_limited += 1; continue;
            }
            // FMD treats backslashes as separators in source paths. A raw FCBS
            // member can instead contain a literal backslash, so this adapter
            // refuses that context rather than silently changing its meaning.
            if !std::str::from_utf8(member.path).is_ok_and(valid_source_path) {
                candidate.unsupported += 1; continue;
            }
            let path_len = member.path.len();
            let [load_id, pin_id, document_id, path_id] = self.allocations()?;
            // This transient envelope admits upstream path resolution scratch,
            // which may include a long parent path; it is released per source.
            let path_charge = path_len.checked_mul(16).and_then(|n| n.checked_add(
                16 * MAX_DOCUMENT_LINK_DESTINATION_BYTES + 4096)).ok_or(AppError::Admission)?;
            let _path_lease = self.budget.try_reserve_managed(owner, path_id, ByteLength::new(path_charge as u64))
                .map_err(|_| AppError::Admission)?;
            let path = copy_text(std::str::from_utf8(self.archive.directory().member(ordinal)
                .ok_or(SavedRelationshipError::MissingLink)?.path).map_err(|_| AppError::InvalidRange)?)?;
            let value = member_id(ordinal)?;
            let file = FileId::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
            let revision = SourceRevision::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
            admitted_source_bytes += length as u64;
            let capture = PagedCapture::load(&mut self.archive, ordinal, file, revision,
                &self.budget, [load_id, pin_id], &mut canceled)?;
            let request = CaptureRequest::new(file, revision).map_err(|_| AppError::InvalidRange)?;
            let document = match CapturedDocumentLinks::build(capture.bytes(), request, qgen,
                max_links - candidate.links.len(), &self.budget, document_id, &mut canceled) {
                Ok(document) => document,
                Err(DocumentLinkError::InvalidUtf8 | DocumentLinkError::InvalidEvidence
                    | DocumentLinkError::ComplexityLimit) => { candidate.unsupported += 1; continue; }
                Err(error) => return Err(error.into()),
            };
            candidate.scanned += 1;
            if document.diagnostics() > 0 { candidate.diagnostic += 1; }
            candidate.links_seen += document.links_seen();
            candidate.truncated |= document.truncated();
            for link in document.links() {
                check(&mut canceled)?;
                let [text_id] = self.allocations()?;
                let charge = link.destination().len().checked_mul(4).and_then(|n| n.checked_add(512))
                    .ok_or(AppError::Admission)?;
                let text_lease = self.budget.try_reserve_managed(owner, text_id, ByteLength::new(charge as u64))
                    .map_err(|_| AppError::Admission)?;
                let destination = copy_text(link.destination())?;
                let (resolution, target_member, target_digest, suffix) = self.resolve_relationship(&path, ordinal, &destination)?;
                if suffix.capacity() > destination.len() { return Err(AppError::Admission.into()); }
                candidate.links.push(Link { source_member: ordinal, source_link_id: link.id(),
                    block_index: link.block_index(), evidence: link.original_document_range(),
                    source_digest: digest, destination, resolution, target_member, target_digest,
                    suffix, _text_lease: text_lease });
            }
        }
        let after = self.archive.load_stats();
        candidate.source_bytes = after.bytes_read - before.bytes_read;
        candidate.read_calls = after.read_calls - before.read_calls;
        candidate.complete = self.archive.directory().discovery_complete()
            && candidate.scanned == candidate.markdown && candidate.diagnostic == 0 && !candidate.truncated;
        let mut out = self.output("relationships-prepare")?;
        self.encode_relationship_page(&mut out, &candidate, None, RelationshipDirection::All,
            0, 64.min(MAX_SAVED_PAGE), &mut canceled)?;
        let response = self.finish(out, !candidate.complete, &mut canceled)?;
        self.relationships = Some(candidate);
        Ok(response)
    }

    /// Paginates filtered occurrence identities without rereading or reparsing.
    /// All+member includes either endpoint. Incoming/outgoing require a member.
    pub fn relationship_page(&mut self, generation: u64, member: Option<usize>,
        direction: RelationshipDirection, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRelationshipError> {
        check(&mut canceled)?;
        self.accepted_relationships(generation)?;
        if member.is_some_and(|id| id >= self.archive.directory().len())
            || (member.is_none() && direction != RelationshipDirection::All)
            || !(1..=MAX_SAVED_PAGE).contains(&limit) { return Err(SavedRelationshipError::InvalidLimits); }
        let mut out = self.output("relationships")?;
        let index = self.accepted_relationships(generation)?;
        self.encode_relationship_page(&mut out, index, member, direction, start, limit, &mut canceled)?;
        let partial = !index.complete;
        Ok(self.finish(out, partial, &mut canceled)?)
    }

    pub fn clear_relationships(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRelationshipError> {
        relationship_attempt(&mut self.last_relationship_attempt, generation)?;
        check(&mut canceled)?;
        let mut out = self.output("relationships-clear")?;
        out.literal(",\"accepted_relationship_generation\":null,\"retained_links\":\"0\"}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.relationships = None;
        Ok(response)
    }

    pub fn open_relationship_reader(&mut self, owner: ArenaOwnerId, generation: u64,
        link_id: u64, side: RelationshipSide, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedRelationshipError> {
        if owner == self.owner() { return Err(SavedRelationshipError::OwnerMismatch); }
        check(&mut canceled)?;
        let target = self.relationship_target(generation, link_id, side)?;
        let before = self.archive.load_stats();
        let capture = self.load_relationship_target(target, &mut canceled)?;
        let member = self.archive.directory().member(target.member).ok_or(SavedRelationshipError::MissingLink)?;
        let label = RawPath::from_bytes(member.path).to_path_buf();
        let mut reader = ReaderSession::from_bytes(owner, &label, capture.bytes(), &mut canceled)?;
        let info = reader.info(&mut canceled)?;
        let offset = target.selection.map_or(0, |range| range.start().get());
        let window = reader.read_window(offset.saturating_sub(128), 16 * 1024, &mut canceled)?;
        let after = self.archive.load_stats();
        let mut out = self.output("relationships-open-reader")?;
        out.literal(",\"relationship_generation\":")?; out.integer(generation)?;
        out.literal(",\"link_id\":")?; out.integer(link_id)?;
        out.literal(",\"side\":")?; out.quoted(side.name())?;
        out.literal(",\"member\":")?; out.integer(target.member as u64)?;
        out.literal(",\"file_id\":")?; out.integer(target.file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(target.revision.get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&target.digest.to_hex())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"live_source_reopened\":false,\"offset\":")?;
        out.integer(offset)?;
        out.literal(",\"selection\":")?;
        match target.selection { Some(range) => out.range(range)?, None => out.literal("null")? }
        out.literal(",\"evidence_precision\":\"whole-captured-markdown-document\",\"fragment_resolution\":")?;
        out.quoted(fragment_label(target.fragment_unresolved))?;
        out.literal(",\"member_bytes_read\":")?; out.integer(after.bytes_read - before.bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(after.read_calls - before.read_calls)?;
        out.literal(",\"reader\":")?; out.literal(info.as_str().trim_end())?;
        out.literal(",\"window\":")?; out.literal(window.as_str().trim_end())?;
        out.literal("}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        Ok((reader, response))
    }

    fn accepted_relationships(&self, generation: u64) -> Result<&AcceptedRelationships, SavedRelationshipError> {
        let index = self.relationships.as_ref().ok_or(SavedRelationshipError::MissingRelationships)?;
        if index.owner != self.owner() || index.archive != self.archive_digest() || index.generation != generation {
            return Err(SavedRelationshipError::StaleGeneration);
        }
        Ok(index)
    }
    pub(super) fn relationship_target(&self, generation: u64, id: u64, side: RelationshipSide)
        -> Result<RelationshipTarget, SavedRelationshipError> {
        let position = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(SavedRelationshipError::MissingLink)?;
        let link = self.accepted_relationships(generation)?.links.get(position).ok_or(SavedRelationshipError::MissingLink)?;
        let (member, digest, selection) = match side {
            RelationshipSide::Evidence => (link.source_member, link.source_digest, Some(link.evidence)),
            RelationshipSide::Target => match link.resolution {
                Resolution::Captured => (link.target_member.ok_or(SavedRelationshipError::UnresolvedTarget)?,
                    link.target_digest.ok_or(SavedRelationshipError::UnresolvedTarget)?, None),
                Resolution::Unavailable => return Err(SavedRelationshipError::TargetUnavailable),
                _ => return Err(SavedRelationshipError::UnresolvedTarget),
            },
        };
        let value = member_id(member)?;
        let file = FileId::new(self.owner(), value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let revision = SourceRevision::new(self.owner(), value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        Ok(RelationshipTarget { member, file, revision, digest, selection, fragment_unresolved: link.fragment_unresolved() })
    }
    pub(super) fn load_relationship_target(&mut self, target: RelationshipTarget,
        canceled: &mut impl FnMut() -> bool) -> Result<PagedCapture, SavedRelationshipError> {
        check(canceled)?;
        let ids = self.allocations()?;
        let capture = PagedCapture::load(&mut self.archive, target.member, target.file, target.revision,
            &self.budget, ids, &mut *canceled)?;
        if capture.source_digest() != target.digest { return Err(PagedSearchError::InvalidHit.into()); }
        if let Some(range) = target.selection {
            let (start, end) = range.as_usize_bounds().map_err(|_| AppError::InvalidRange)?;
            capture.bytes().get(start..end).ok_or(AppError::InvalidRange)?;
        }
        Ok(capture)
    }

    fn resolve_relationship(&self, source_path: &str, source_member: usize, destination: &str)
        -> Result<(Resolution, Option<usize>, Option<Sha256Digest>, String), SavedRelationshipError> {
        let trimmed = destination.trim();
        let (member, suffix) = if trimmed.starts_with('#') || trimmed.starts_with('?') {
            (Some(source_member), copy_text(trimmed)?)
        } else if let Some((path, suffix)) = resolve_document_destination(source_path, destination) {
            (self.archive.directory().find_path(path.as_bytes()), suffix)
        } else {
            return Ok((if external(trimmed) { Resolution::External } else { Resolution::Unresolved },
                None, None, String::new()));
        };
        let Some(ordinal) = member else { return Ok((Resolution::Missing, None, None, suffix)); };
        let entry = self.archive.directory().member(ordinal).ok_or(SavedRelationshipError::MissingLink)?;
        Ok(match entry.data {
            PagedMemberData::Captured { digest, .. } => (Resolution::Captured, Some(ordinal), Some(digest), suffix),
            PagedMemberData::Unavailable(_) => (Resolution::Unavailable, Some(ordinal), None, suffix),
        })
    }

    fn encode_relationship_page(&self, out: &mut Output, index: &AcceptedRelationships,
        member: Option<usize>, direction: RelationshipDirection, start: usize, limit: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), SavedRelationshipError> {
        let count = index.links.iter().filter(|link| link.matches(member, direction)).count();
        if start > count { return Err(SavedRelationshipError::InvalidLimits); }
        out.literal(",\"relationship_generation\":")?; out.integer(index.generation)?;
        out.literal(",\"projection\":\"markdown-document-links\",\"evidence_precision\":\"whole-captured-markdown-document\",\"resolver\":\"fmd-exact-member-v1\",\"extractor\":\"franken-markdown-d6fbd1d5/document-evidence-v1\",\"completeness_scope\":\"parsed-document-links-in-saved-manifest\",\"relationships_complete\":")?;
        out.boolean(index.complete)?;
        out.literal(",\"truncated\":")?; out.boolean(index.truncated)?;
        for (key, value) in [("markdown_members", index.markdown), ("scanned_markdown_members", index.scanned),
            ("unavailable_markdown_members", index.unavailable), ("unsupported_markdown_members", index.unsupported),
            ("source_limited_members", index.source_limited), ("diagnostic_markdown_members", index.diagnostic),
            ("retained_links", index.links.len() as u64), ("links_seen", index.links_seen),
            ("member_bytes_read", index.source_bytes), ("member_read_calls", index.read_calls), ("matching_links", count as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"retained_source_payload_bytes\":\"0\",\"direction\":")?; out.quoted(direction.name())?;
        out.literal(",\"filter_member\":")?; optional(out, member.map(|n| n as u64))?;
        out.literal(",\"links\":[")?;
        for (position, (id, link)) in index.links.iter().enumerate()
            .filter(|(_, link)| link.matches(member, direction)).skip(start).take(limit).enumerate() {
            check(canceled)?;
            if position > 0 { out.literal(",")?; }
            self.encode_relationship(out, id as u64 + 1, link)?;
        }
        let end = start.saturating_add(limit).min(count);
        out.literal("],\"next_offset\":")?; optional(out, (end < count).then_some(end as u64))?;
        out.literal("}\n")?;
        Ok(())
    }
    fn encode_relationship(&self, out: &mut Output, id: u64, link: &Link) -> Result<(), SavedRelationshipError> {
        let source = self.archive.directory().member(link.source_member).ok_or(SavedRelationshipError::MissingLink)?;
        out.literal("{\"link_id\":")?; out.integer(id)?;
        out.literal(",\"source_member\":")?; out.integer(link.source_member as u64)?;
        out.literal(",\"source_file_id\":")?; out.integer(member_id(link.source_member)?)?;
        out.literal(",\"source_revision\":")?; out.integer(member_id(link.source_member)?)?;
        out.literal(",\"source_digest\":")?; out.quoted(&link.source_digest.to_hex())?;
        out.literal(",\"source_path\":")?; out.path(&RawPath::from_bytes(source.path).to_path_buf())?;
        out.literal(",\"evidence_range\":")?; out.range(link.evidence)?;
        out.literal(",\"block_index\":")?; out.integer(link.block_index as u64)?;
        out.literal(",\"source_link_id\":")?; out.integer(link.source_link_id)?;
        out.literal(",\"destination\":")?; out.quoted(&link.destination)?;
        out.literal(",\"resolution\":")?; out.quoted(link.resolution.name())?;
        out.literal(",\"target_member\":")?; optional(out, link.target_member.map(|n| n as u64))?;
        out.literal(",\"target_path\":")?;
        if let Some(ordinal) = link.target_member {
            let target = self.archive.directory().member(ordinal).ok_or(SavedRelationshipError::MissingLink)?;
            out.path(&RawPath::from_bytes(target.path).to_path_buf())?;
        } else { out.literal("null")?; }
        out.literal(",\"target_suffix\":")?; out.quoted(&link.suffix)?;
        out.literal(",\"fragment_resolution\":")?; out.quoted(fragment_label(link.fragment_unresolved()))?;
        out.literal("}")?;
        Ok(())
    }
}

fn relationship_attempt(last: &mut u64, next: u64) -> Result<(), SavedRelationshipError> {
    if next == 0 || next <= *last { return Err(SavedRelationshipError::StaleGeneration); }
    *last = next; Ok(())
}
fn member_id(ordinal: usize) -> Result<u64, SavedRelationshipError> {
    u64::try_from(ordinal).ok().and_then(|n| n.checked_add(1))
        .ok_or(SavedRepositoryError::IdentityExhausted.into())
}
fn markdown(path: &[u8]) -> bool {
    let name = path.rsplit(|&b| b == b'/').next().unwrap_or(path);
    let extension = name.iter().rposition(|&b| b == b'.').map(|i| &name[i + 1..]).unwrap_or(&[]);
    extension.eq_ignore_ascii_case(b"md") || extension.eq_ignore_ascii_case(b"markdown")
}
fn valid_source_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= 16_384 && !path.contains('\\')
        && !path.chars().any(char::is_control) && !external(path)
}
fn external(destination: &str) -> bool {
    destination.starts_with("//") || destination.split_once(':').is_some_and(|(scheme, _)| {
        scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    })
}
pub(super) const fn fragment_label(unresolved: bool) -> &'static str { if unresolved { "unresolved" } else { "none" } }
