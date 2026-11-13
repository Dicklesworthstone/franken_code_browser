#![forbid(unsafe_code)]

//! Lexical imports and reverse importers in one closed saved manifest.
//! The shared capture extractor and finite catalog resolver own language rules.
//! This adapter verifies one archived source at a time and retains only bounded
//! occurrence metadata. No member pathname grants access to a live source.
//! Accepted projections survive failed/canceled replacements independently of
//! Markdown relationships, text queries, structure searches and posting indexes.

use std::mem::size_of;
use fcb::ByteRange;
use fcb::search::{CaptureRequest, imports::{CapturedImports, ImportError, ImportKind,
    ImportLanguage, ImportResolutionKind, resolve_import, IMPORT_EXTRACTOR, IMPORT_POLICY,
    MAX_IMPORT_ITEMS, MAX_IMPORT_SOURCE_BYTES}};
use super::{ArenaOwnerId, AppError, ByteLength, FileId, HostResponse, Output, OutputError,
    PagedCapture, PagedMemberData, PagedSearchError, QueryGeneration, RawPath,
    ReaderSession, ReaderSessionError, ResourceLease, SavedRepositoryError,
    SavedRepositorySession, Sha256Digest, SourceRevision, MAX_SAVED_PAGE, check, copy_text, optional};

pub const MAX_SAVED_IMPORTS: usize = MAX_IMPORT_ITEMS;
pub const MAX_SAVED_IMPORT_SOURCE_BYTES: u64 = 32 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedImportDirection { All, Outgoing, Incoming }
impl SavedImportDirection {
    pub const fn name(self) -> &'static str {
        match self { Self::All => "all", Self::Outgoing => "outgoing", Self::Incoming => "incoming" }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedImportSide { Evidence, Target }
impl SavedImportSide {
    pub const fn name(self) -> &'static str { match self { Self::Evidence => "evidence", Self::Target => "target" } }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedImportError {
    Saved(SavedRepositoryError), Import(ImportError), InvalidLimits, MissingImports,
    StaleGeneration, MissingImport, UnresolvedTarget, TargetUnavailable, OwnerMismatch,
}
impl std::fmt::Display for SavedImportError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Saved(error) => write!(f, "{error}"), Self::Import(error) => write!(f, "{error}"),
            Self::InvalidLimits => f.write_str("SAVED_IMPORT_INVALID_LIMITS"),
            Self::MissingImports => f.write_str("SAVED_IMPORT_NOT_PREPARED"),
            Self::StaleGeneration => f.write_str("SAVED_IMPORT_STALE_GENERATION"),
            Self::MissingImport => f.write_str("SAVED_IMPORT_NOT_FOUND"),
            Self::UnresolvedTarget => f.write_str("SAVED_IMPORT_TARGET_UNRESOLVED"),
            Self::TargetUnavailable => f.write_str("SAVED_IMPORT_TARGET_UNAVAILABLE"),
            Self::OwnerMismatch => f.write_str("SAVED_IMPORT_OWNER_MISMATCH"),
        }
    }
}
impl std::error::Error for SavedImportError {}
impl From<SavedRepositoryError> for SavedImportError { fn from(error: SavedRepositoryError) -> Self { Self::Saved(error) } }
impl From<ImportError> for SavedImportError { fn from(error: ImportError) -> Self { Self::Import(error) } }
impl From<OutputError> for SavedImportError { fn from(error: OutputError) -> Self { Self::Saved(error.into()) } }
impl From<AppError> for SavedImportError { fn from(error: AppError) -> Self { Self::Saved(error.into()) } }
impl From<PagedSearchError> for SavedImportError { fn from(error: PagedSearchError) -> Self { Self::Saved(error.into()) } }
impl From<ReaderSessionError> for SavedImportError { fn from(error: ReaderSessionError) -> Self { Self::Saved(error.into()) } }
impl SavedImportError {
    pub fn is_canceled(self) -> bool {
        match self { Self::Saved(error) => error.is_canceled(), Self::Import(ImportError::Canceled) => true, _ => false }
    }
}

#[derive(Clone, Copy, Eq, PartialEq)]
enum Resolution { Captured, Unavailable, Missing, Ambiguous, External, Unresolved }
impl Resolution {
    fn name(self) -> &'static str {
        match self {
            Self::Captured => "captured", Self::Unavailable => "unavailable", Self::Missing => "missing",
            Self::Ambiguous => "ambiguous", Self::External => "external", Self::Unresolved => "unresolved",
        }
    }
}
#[derive(Clone, Copy)]
struct SourceProof { member: usize, file: FileId, revision: SourceRevision, digest: Sha256Digest, bytes: u64 }
/// A selected endpoint from one accepted projection, never a supplied path.
/// Both independent readers and Desk imports reverify this same proof.
#[derive(Clone, Copy)]
pub(super) struct ImportTarget { source: SourceProof, pub selection: Option<ByteRange> }
impl ImportTarget { pub fn member(self) -> usize { self.source.member } }
struct Link {
    source: SourceProof, source_link: u64, evidence: ByteRange, specifier: ByteRange,
    language: ImportLanguage, kind: ImportKind, destination: String, resolution: Resolution,
    basis: &'static str, target_member: Option<usize>, target: Option<SourceProof>, _text_lease: ResourceLease,
}
impl Link {
    fn matches(&self, member: Option<usize>, direction: SavedImportDirection) -> bool {
        match (member, direction) {
            (None, SavedImportDirection::All) => true,
            (Some(id), SavedImportDirection::All) => self.source.member == id || self.target_member == Some(id),
            (Some(id), SavedImportDirection::Outgoing) => self.source.member == id,
            (Some(id), SavedImportDirection::Incoming) => self.target_member == Some(id),
            _ => false,
        }
    }
}
pub(super) struct AcceptedImports {
    generation: u64, owner: ArenaOwnerId, archive: Sha256Digest, links: Vec<Link>,
    max_links: usize, max_source_bytes: u64, sources: u64, scanned: u64, unavailable: u64,
    unsupported: u64, source_limited: u64, diagnostic_sources: u64, diagnostics: u64,
    links_seen: u64, truncated: bool, source_bytes: u64, read_calls: u64,
    peak_source_bytes: u64, complete: bool, _lease: ResourceLease,
}

impl SavedRepositorySession {
    pub fn import_generation(&self) -> Option<u64> { self.imports.as_ref().map(|index| index.generation) }
    pub const fn last_import_attempt(&self) -> u64 { self.last_import_attempt }

    /// Worker operation over the exact opened archive. Each eligible source is
    /// independently verified before extraction. An output cap does not stop
    /// occurrence counting in other admitted members. Zero source quota permits
    /// empty members; unavailable/unsupported/oversized members remain explicit.
    pub fn prepare_imports(&mut self, generation: u64, max_links: usize, max_source_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedImportError> {
        import_attempt(&mut self.last_import_attempt, generation)?;
        check(&mut canceled)?;
        if !(1..=MAX_SAVED_IMPORTS).contains(&max_links) || max_source_bytes > MAX_SAVED_IMPORT_SOURCE_BYTES {
            return Err(SavedImportError::InvalidLimits);
        }
        let owner = self.owner();
        let qgen = QueryGeneration::new(owner, generation).map_err(|_| SavedImportError::StaleGeneration)?;
        let [index_id] = self.allocations()?;
        let charge = max_links.checked_mul(size_of::<Link>())
            .and_then(|n| n.checked_add(size_of::<AcceptedImports>() + 4096)).ok_or(AppError::Admission)?;
        let lease = self.budget.try_reserve_managed(owner, index_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut links = Vec::new();
        links.try_reserve_exact(max_links).map_err(|_| AppError::Admission)?;
        if links.capacity() > max_links { return Err(AppError::Admission.into()); }
        let mut candidate = AcceptedImports { generation, owner, archive: self.archive_digest(), links,
            max_links, max_source_bytes, sources: 0, scanned: 0, unavailable: 0, unsupported: 0,
            source_limited: 0, diagnostic_sources: 0, diagnostics: 0, links_seen: 0, truncated: false,
            source_bytes: 0, read_calls: 0, peak_source_bytes: 0, complete: false, _lease: lease };
        let before = self.archive.load_stats();
        let mut admitted_bytes = 0u64;
        for ordinal in 0..self.archive.directory().len() {
            check(&mut canceled)?;
            let member = self.archive.directory().member(ordinal).ok_or(SavedImportError::MissingImport)?;
            let Some(language) = ImportLanguage::from_path(member.path) else { continue; };
            candidate.sources += 1;
            let (length, digest) = match member.data {
                PagedMemberData::Captured { byte_length, digest, .. } => (byte_length, digest),
                PagedMemberData::Unavailable(_) => { candidate.unavailable += 1; continue; }
            };
            if length > MAX_IMPORT_SOURCE_BYTES || length as u64 > max_source_bytes.saturating_sub(admitted_bytes) {
                candidate.source_limited += 1;
                continue;
            }
            let source = source_proof(owner, ordinal, digest, length as u64)?;
            let [load_id, pin_id, parser_id] = self.allocations()?;
            admitted_bytes += length as u64;
            let capture = PagedCapture::load(&mut self.archive, ordinal, source.file, source.revision,
                &self.budget, [load_id, pin_id], &mut canceled)?;
            candidate.peak_source_bytes = candidate.peak_source_bytes.max(capture.bytes().len() as u64);
            let request = CaptureRequest::new(source.file, source.revision).map_err(|_| AppError::InvalidRange)?;
            let imports = match CapturedImports::build(capture.bytes(), request, qgen, language,
                max_links - candidate.links.len(), &self.budget, parser_id, &mut canceled) {
                Ok(imports) => imports,
                Err(ImportError::UnsupportedEncoding | ImportError::InvalidEvidence | ImportError::SourceLimit) => {
                    candidate.unsupported += 1;
                    continue;
                }
                Err(error) => return Err(error.into()),
            };
            candidate.scanned += 1;
            if imports.diagnostics() > 0 { candidate.diagnostic_sources += 1; }
            candidate.diagnostics = candidate.diagnostics.checked_add(imports.diagnostics() as u64)
                .ok_or(SavedRepositoryError::IdentityExhausted)?;
            candidate.links_seen = candidate.links_seen.checked_add(imports.links_seen())
                .ok_or(SavedRepositoryError::IdentityExhausted)?;
            candidate.truncated |= imports.truncated();
            for fact in imports.facts() {
                check(&mut canceled)?;
                let [text_id] = self.allocations()?;
                let text_charge = fact.destination().len().checked_add(128).ok_or(AppError::Admission)?;
                let text_lease = self.budget.try_reserve_managed(owner, text_id, ByteLength::new(text_charge as u64))
                    .map_err(|_| AppError::Admission)?;
                let path = self.archive.directory().member(ordinal).ok_or(SavedImportError::MissingImport)?.path;
                let resolved = resolve_import(path, fact, |candidate| self.archive.directory().find_path(candidate));
                let (resolution, target_member, target) = match resolved.kind {
                    ImportResolutionKind::Local => {
                        let ordinal = resolved.target.ok_or(SavedImportError::UnresolvedTarget)?;
                        let member = self.archive.directory().member(ordinal).ok_or(SavedImportError::MissingImport)?;
                        match member.data {
                            PagedMemberData::Captured { byte_length, digest, .. } =>
                                (Resolution::Captured, Some(ordinal), Some(source_proof(owner, ordinal, digest, byte_length as u64)?)),
                            PagedMemberData::Unavailable(_) => (Resolution::Unavailable, Some(ordinal), None),
                        }
                    }
                    ImportResolutionKind::Missing => (Resolution::Missing, None, None),
                    ImportResolutionKind::Ambiguous => (Resolution::Ambiguous, None, None),
                    ImportResolutionKind::External => (Resolution::External, None, None),
                    ImportResolutionKind::Unresolved => (Resolution::Unresolved, None, None),
                };
                candidate.links.push(Link { source, source_link: fact.id(), evidence: fact.original_range(),
                    specifier: fact.specifier_range(), language, kind: fact.kind(), destination: copy_text(fact.destination())?,
                    resolution, basis: resolved.basis, target_member, target, _text_lease: text_lease });
            }
        }
        let after = self.archive.load_stats();
        candidate.source_bytes = after.bytes_read - before.bytes_read;
        candidate.read_calls = after.read_calls - before.read_calls;
        candidate.complete = self.archive.directory().discovery_complete()
            && candidate.scanned == candidate.sources && candidate.diagnostics == 0 && !candidate.truncated;
        let mut out = self.output("imports-prepare")?;
        self.encode_import_page(&mut out, &candidate, None, SavedImportDirection::All, 0, 64, &mut canceled)?;
        let response = self.finish(out, !candidate.complete, &mut canceled)?;
        self.imports = Some(candidate);
        Ok(response)
    }

    /// Metadata-only filtering and pagination. Occurrence IDs do not change
    /// under incoming/outgoing filters, and no archived source is loaded again.
    pub fn import_page(&mut self, generation: u64, member: Option<usize>, direction: SavedImportDirection,
        start: usize, limit: usize, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedImportError> {
        check(&mut canceled)?;
        self.accepted_imports(generation)?;
        if member.is_some_and(|id| id >= self.archive.directory().len())
            || (member.is_none() && direction != SavedImportDirection::All)
            || !(1..=MAX_SAVED_PAGE).contains(&limit) { return Err(SavedImportError::InvalidLimits); }
        let mut out = self.output("imports")?;
        let index = self.accepted_imports(generation)?;
        self.encode_import_page(&mut out, index, member, direction, start, limit, &mut canceled)?;
        let partial = !index.complete;
        Ok(self.finish(out, partial, &mut canceled)?)
    }

    pub fn clear_imports(&mut self, generation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedImportError> {
        import_attempt(&mut self.last_import_attempt, generation)?;
        check(&mut canceled)?;
        let mut out = self.output("imports-clear")?;
        out.literal(",\"accepted_import_generation\":null,\"last_import_attempt\":")?;
        out.integer(generation)?;
        out.literal(",\"retained_links\":\"0\"}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.imports = None;
        Ok(response)
    }

    /// The selection is copied from one accepted occurrence and the member is
    /// reverified against its archived digest. The returned independent reader
    /// survives projection replacement, archive detachment and path replacement.
    pub fn open_import_reader(&mut self, owner: ArenaOwnerId, generation: u64, link_id: u64,
        side: SavedImportSide, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), SavedImportError> {
        if owner == self.owner() { return Err(SavedImportError::OwnerMismatch); }
        check(&mut canceled)?;
        let target = self.import_target(generation, link_id, side)?;
        let selected = target.source;
        let selection = target.selection;
        let before = self.archive.load_stats();
        let capture = self.load_import_target(target, &mut canceled)?;
        let path = self.archive.directory().member(selected.member).ok_or(SavedImportError::MissingImport)?.path;
        let label = RawPath::from_bytes(path).to_path_buf();
        let mut reader = ReaderSession::from_bytes(owner, &label, capture.bytes(), &mut canceled)?;
        let info = reader.info(&mut canceled)?;
        let offset = selection.map_or(0, |range| range.start().get());
        let window = reader.read_window(offset.saturating_sub(128), 16 * 1024, &mut canceled)?;
        let after = self.archive.load_stats();
        let mut out = self.output("imports-open-reader")?;
        contract(&mut out)?;
        out.literal(",\"import_generation\":")?; out.integer(generation)?;
        out.literal(",\"link_id\":")?; out.integer(link_id)?;
        out.literal(",\"side\":")?; out.quoted(side.name())?;
        out.literal(",\"relationship\":")?; self.encode_import(&mut out, link_id, self.import_link(generation, link_id)?)?;
        out.literal(",\"member\":")?; out.integer(selected.member as u64)?;
        out.literal(",\"file_id\":")?; out.integer(selected.file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(selected.revision.get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&selected.digest.to_hex())?;
        out.literal(",\"path\":")?; out.path(&label)?;
        out.literal(",\"source_bytes\":")?; out.integer(selected.bytes)?;
        out.literal(",\"reader_owner\":")?; out.integer(owner.get())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"live_source_reopened\":false,\"offset\":")?;
        out.integer(offset)?;
        out.literal(",\"selection\":")?;
        if let Some(range) = selection { out.range(range)?; } else { out.literal("null")?; }
        out.literal(",\"member_bytes_read\":")?; out.integer(after.bytes_read - before.bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(after.read_calls - before.read_calls)?;
        out.literal(",\"reader\":")?; out.literal(info.as_str().trim_end())?;
        out.literal(",\"window\":")?; out.literal(window.as_str().trim_end())?;
        out.literal("}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        Ok((reader, response))
    }

    pub(super) fn import_target(&self, generation: u64, link_id: u64, side: SavedImportSide)
        -> Result<ImportTarget, SavedImportError> {
        let link = self.import_link(generation, link_id)?;
        let (source, selection) = match side {
            SavedImportSide::Evidence => (link.source, Some(link.evidence)),
            SavedImportSide::Target => match link.resolution {
                Resolution::Captured => (link.target.ok_or(SavedImportError::UnresolvedTarget)?, None),
                Resolution::Unavailable => return Err(SavedImportError::TargetUnavailable),
                _ => return Err(SavedImportError::UnresolvedTarget),
            }
        };
        Ok(ImportTarget { source, selection })
    }
    pub(super) fn load_import_target(&mut self, target: ImportTarget,
        canceled: &mut impl FnMut() -> bool) -> Result<PagedCapture, SavedImportError> {
        check(canceled)?;
        let selected = target.source;
        let ids = self.allocations()?;
        let capture = PagedCapture::load(&mut self.archive, selected.member, selected.file, selected.revision,
            &self.budget, ids, &mut *canceled)?;
        if capture.source_digest() != selected.digest || capture.bytes().len() as u64 != selected.bytes {
            return Err(PagedSearchError::InvalidHit.into());
        }
        if let Some(range) = target.selection {
            let (start, end) = range.as_usize_bounds().map_err(|_| AppError::InvalidRange)?;
            capture.bytes().get(start..end).ok_or(AppError::InvalidRange)?;
        }
        Ok(capture)
    }

    fn accepted_imports(&self, generation: u64) -> Result<&AcceptedImports, SavedImportError> {
        let index = self.imports.as_ref().ok_or(SavedImportError::MissingImports)?;
        if index.owner != self.owner() || index.archive != self.archive_digest() || index.generation != generation {
            return Err(SavedImportError::StaleGeneration);
        }
        Ok(index)
    }
    fn import_link(&self, generation: u64, id: u64) -> Result<&Link, SavedImportError> {
        let position = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(SavedImportError::MissingImport)?;
        self.accepted_imports(generation)?.links.get(position).ok_or(SavedImportError::MissingImport)
    }
    #[allow(clippy::too_many_arguments)]
    fn encode_import_page(&self, out: &mut Output, index: &AcceptedImports, member: Option<usize>,
        direction: SavedImportDirection, start: usize, limit: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), SavedImportError> {
        let count = index.links.iter().filter(|link| link.matches(member, direction)).count();
        if start > count { return Err(SavedImportError::InvalidLimits); }
        contract(out)?;
        out.literal(",\"import_generation\":")?; out.integer(index.generation)?;
        out.literal(",\"completeness_scope\":\"parsed-source-imports-in-saved-manifest\",\"imports_complete\":")?;
        out.boolean(index.complete)?;
        out.literal(",\"truncated\":")?; out.boolean(index.truncated)?;
        out.literal(",\"source_observation\":\"verified-saved-members\"")?;
        for (key, value) in [("source_members", index.sources), ("scanned_source_members", index.scanned),
            ("unavailable_source_members", index.unavailable), ("unsupported_source_members", index.unsupported),
            ("source_limited_members", index.source_limited), ("diagnostic_source_members", index.diagnostic_sources),
            ("parser_diagnostics", index.diagnostics), ("retained_links", index.links.len() as u64),
            ("links_seen", index.links_seen), ("member_bytes_read", index.source_bytes), ("member_read_calls", index.read_calls),
            ("peak_source_bytes", index.peak_source_bytes), ("max_links", index.max_links as u64),
            ("max_source_bytes", index.max_source_bytes), ("max_member_source_bytes", MAX_IMPORT_SOURCE_BYTES as u64),
            ("offset", start as u64), ("limit", limit as u64), ("matching_links", count as u64)] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"retained_source_payload_bytes\":\"0\",\"direction\":")?; out.quoted(direction.name())?;
        out.literal(",\"filter_member\":")?; optional(out, member.map(|n| n as u64))?;
        out.literal(",\"links\":[")?;
        let mut emitted = 0usize;
        for (id, link) in index.links.iter().enumerate().filter(|(_, link)| link.matches(member, direction))
            .skip(start).take(limit) {
            check(canceled)?;
            let checkpoint = out.checkpoint();
            let encoded = (|| {
                if emitted != 0 { out.literal(",")?; }
                self.encode_import(out, id as u64 + 1, link)?;
                if out.remaining_bytes() < 128 { return Err(SavedImportError::from(OutputError::Limit)); }
                Ok(())
            })();
            match encoded {
                Ok(()) => emitted += 1,
                Err(SavedImportError::Saved(SavedRepositoryError::App(AppError::Output(OutputError::Limit))))
                    if emitted > 0 => { out.restore(checkpoint)?; break; }
                Err(error) => return Err(error),
            }
        }
        let next = start.checked_add(emitted).ok_or(AppError::Admission)?;
        out.literal("],\"next_offset\":")?; optional(out, (next < count).then_some(next as u64))?;
        out.literal("}\n")?;
        Ok(())
    }
    fn encode_import(&self, out: &mut Output, id: u64, link: &Link) -> Result<(), SavedImportError> {
        let source = self.archive.directory().member(link.source.member).ok_or(SavedImportError::MissingImport)?;
        out.literal("{\"link_id\":")?; out.integer(id)?;
        out.literal(",\"source_member\":")?; out.integer(link.source.member as u64)?;
        out.literal(",\"source_file_id\":")?; out.integer(link.source.file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(link.source.revision.get())?;
        out.literal(",\"source_digest\":")?; out.quoted(&link.source.digest.to_hex())?;
        out.literal(",\"source_path\":")?; out.path(&RawPath::from_bytes(source.path).to_path_buf())?;
        out.literal(",\"source_bytes\":")?; out.integer(link.source.bytes)?;
        out.literal(",\"source_link_id\":")?; out.integer(link.source_link)?;
        out.literal(",\"evidence_range\":")?; out.range(link.evidence)?;
        out.literal(",\"specifier_range\":")?; out.range(link.specifier)?;
        out.literal(",\"language\":")?; out.quoted(link.language.name())?;
        out.literal(",\"lexical_import_kind\":")?; out.quoted(link.kind.name())?;
        out.literal(",\"destination\":")?; out.quoted(&link.destination)?;
        out.literal(",\"resolution\":")?; out.quoted(link.resolution.name())?;
        out.literal(",\"resolution_basis\":")?; out.quoted(link.basis)?;
        out.literal(",\"target_member\":")?; optional(out, link.target_member.map(|n| n as u64))?;
        out.literal(",\"target_file_id\":")?;
        optional(out, link.target_member.map(member_id).transpose()?)?;
        out.literal(",\"target_path\":")?;
        if let Some(ordinal) = link.target_member {
            let target = self.archive.directory().member(ordinal).ok_or(SavedImportError::MissingImport)?;
            out.path(&RawPath::from_bytes(target.path).to_path_buf())?;
        } else { out.literal("null")?; }
        out.literal(",\"target_revision\":")?; optional(out, link.target.map(|target| target.revision.get()))?;
        out.literal(",\"target_digest\":")?;
        if let Some(target) = link.target { out.quoted(&target.digest.to_hex())?; } else { out.literal("null")?; }
        out.literal(",\"target_bytes\":")?; optional(out, link.target.map(|target| target.bytes))?;
        out.literal("}")?;
        Ok(())
    }
}

fn contract(out: &mut Output) -> Result<(), SavedImportError> {
    out.literal(",\"projection\":\"lexical-source-imports\",\"relationship_kind\":\"lexical-import\",\"evidence_precision\":\"exact-import-source-range\",\"resolution_domain\":\"declared-catalog-candidates\",\"compiler_resolved\":false,\"semantic_complete\":false,\"extractor\":")?;
    out.quoted(IMPORT_EXTRACTOR)?;
    out.literal(",\"resolver\":\"fcb-closed-catalog-imports-v3\",\"extraction_policy\":")?; out.quoted(IMPORT_POLICY)?;
    Ok(())
}
fn import_attempt(last: &mut u64, next: u64) -> Result<(), SavedImportError> {
    if next == 0 || next <= *last { return Err(SavedImportError::StaleGeneration); }
    *last = next;
    Ok(())
}
fn member_id(ordinal: usize) -> Result<u64, SavedImportError> {
    u64::try_from(ordinal).ok().and_then(|n| n.checked_add(1))
        .ok_or(SavedRepositoryError::IdentityExhausted.into())
}
fn source_proof(owner: ArenaOwnerId, member: usize, digest: Sha256Digest, bytes: u64)
    -> Result<SourceProof, SavedImportError> {
    let value = member_id(member)?;
    let file = FileId::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
    let revision = SourceRevision::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
    Ok(SourceProof { member, file, revision, digest, bytes })
}

