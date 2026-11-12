#![forbid(unsafe_code)]

//! Source-import navigation over the retained project's exact captured index.
//! Extraction and finite catalog resolution use the public analysis facade.
//! This independent projection does not alter Markdown relationships. No step,
//! page or activation opens working-tree source or evaluates project tooling.

use fcb::search::imports::{CapturedImports, ImportError, ImportLanguage, ImportKind,
    ImportResolutionKind, resolve_import, MAX_IMPORT_SOURCE_BYTES, MAX_IMPORT_ITEMS,
    IMPORT_EXTRACTOR, IMPORT_POLICY};
use fcb::store::Sha256Digest;
use super::*;

#[path = "imports_wire.rs"]
mod wire;

pub const MAX_ATLAS_IMPORTS: usize = MAX_IMPORT_ITEMS;
pub const MAX_ATLAS_IMPORT_PARSE_BYTES: u64 = 32 * 1024 * 1024;
pub const MAX_ATLAS_IMPORT_METADATA_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ATLAS_IMPORT_SOURCE_BYTES: usize = MAX_IMPORT_SOURCE_BYTES;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasImportDirection { All, Outgoing, Incoming }
impl AtlasImportDirection {
    pub const fn name(self) -> &'static str {
        match self { Self::All => "all", Self::Outgoing => "outgoing", Self::Incoming => "incoming" }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasImportSide { Evidence, Target }
impl AtlasImportSide {
    pub const fn name(self) -> &'static str { match self { Self::Evidence => "evidence", Self::Target => "target" } }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum Resolution { Captured, Unavailable, Uncaptured, OutsideScope, Missing, Ambiguous, External, Unresolved }
impl Resolution {
    fn name(self) -> &'static str {
        match self {
            Self::Captured => "captured", Self::Unavailable => "unavailable", Self::Uncaptured => "uncaptured",
            Self::OutsideScope => "outside-capture-scope", Self::Missing => "missing", Self::Ambiguous => "ambiguous",
            Self::External => "external", Self::Unresolved => "unresolved",
        }
    }
}
#[derive(Clone, Copy, Eq, PartialEq)]
enum Stop { All, ParseBytes }
impl Stop {
    fn name(self) -> &'static str { match self { Self::All => "all-source-files-examined", Self::ParseBytes => "parse-byte-limit" } }
}
#[derive(Clone, Copy)]
struct SourceProof { file: FileId, revision: SourceRevision, digest: Sha256Digest, bytes: u64 }
struct Link {
    source: SourceProof, source_link: u64, evidence: ByteRange, specifier: ByteRange,
    language: ImportLanguage, kind: ImportKind, destination: String, resolution: Resolution,
    basis: &'static str, target_file: Option<FileId>, target: Option<SourceProof>,
}
impl Link {
    fn matches(&self, file: Option<FileId>, direction: AtlasImportDirection) -> bool {
        match (file, direction) {
            (None, AtlasImportDirection::All) => true,
            (Some(file), AtlasImportDirection::All) => self.source.file == file || self.target_file == Some(file),
            (Some(file), AtlasImportDirection::Outgoing) => self.source.file == file,
            (Some(file), AtlasImportDirection::Incoming) => self.target_file == Some(file),
            _ => false,
        }
    }
}
pub(crate) struct ImportsSnapshot {
    generation: u64, index_generation: u64, manifest: u64,
    max_links: usize, max_parse_bytes: u64, metadata_bytes: usize,
    sources: Vec<FileId>, proofs: Vec<SourceProof>, links: Vec<Link>,
    eligible: usize, capture_unavailable: usize, outside_scope: usize, uncaptured: usize,
    examined: usize, scanned: usize, unsupported: usize, source_limited: usize,
    diagnostics: u64, links_seen: u64, parsed_bytes: u64, hashed_bytes: u64,
    truncated: bool, stop: Option<Stop>, complete: bool, steps: u64, last_files: usize,
    _lease: ResourceLease,
}
pub(crate) struct ImportsWork { snapshot: ImportsSnapshot, parser_id: ResourceAllocationId }
impl From<ImportError> for AtlasSearchError {
    fn from(error: ImportError) -> Self {
        match error { ImportError::Canceled => Self::Canceled, error => Self::Import(error) }
    }
}
impl RetainedAtlasSearch {
    pub fn import_generation(&self) -> Option<u64> { self.imports.as_ref().map(|s| s.generation) }
    pub fn pending_import_generation(&self) -> Option<u64> { self.imports_pending.as_ref().map(|work| work.snapshot.generation) }
    pub fn cancel_imports_pending(&mut self) -> bool { self.imports_pending.take().is_some() }

    /// Freeze supported source membership without parsing or source I/O. The
    /// requested generation is independent of text and Markdown inventories.
    pub fn begin_imports(&mut self, atlas: &AtlasSession, generation: u64,
        index_generation: u64, max_links: usize, max_parse_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        if generation == 0 || generation <= self.last_import_attempt { return Err(AtlasSearchError::StaleImports); }
        self.last_import_attempt = generation;
        self.imports_pending = None;
        if !(1..=MAX_ATLAS_IMPORTS).contains(&max_links) || max_parse_bytes > MAX_ATLAS_IMPORT_PARSE_BYTES {
            return Err(AtlasSearchError::InvalidLimits);
        }
        check(&mut canceled)?;
        let [state_id, parser_id] = [self.next_id()?, self.next_id()?];
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if index.generation != index_generation { return Err(AtlasSearchError::StaleIndex); }
        let capacity = index.engine.captured_files();
        let fixed = max_links.checked_mul(size_of::<Link>())
            .and_then(|n| capacity.checked_mul(size_of::<FileId>() + size_of::<SourceProof>()).and_then(|m| n.checked_add(m)))
            .and_then(|n| n.checked_add(size_of::<ImportsWork>() + 4096)).ok_or(AppError::Admission)?;
        if fixed > MAX_ATLAS_IMPORT_METADATA_BYTES { return Err(AtlasSearchError::InvalidLimits); }
        let lease = self.budget.try_reserve_managed(self.manifest.owner(), state_id,
            ByteLength::new(MAX_ATLAS_IMPORT_METADATA_BYTES as u64)).map_err(|_| AppError::Admission)?;
        let mut snapshot = ImportsSnapshot { generation, index_generation, manifest: index.engine.id().revision(),
            max_links, max_parse_bytes, metadata_bytes: fixed, sources: reserve(capacity)?, proofs: reserve(capacity)?,
            links: reserve(max_links)?, eligible: 0, capture_unavailable: 0, outside_scope: 0, uncaptured: 0,
            examined: 0, scanned: 0, unsupported: 0, source_limited: 0, diagnostics: 0, links_seen: 0,
            parsed_bytes: 0, hashed_bytes: 0, truncated: false, stop: None, complete: false,
            steps: 0, last_files: 0, _lease: lease };
        let catalog = atlas.atlas().catalog();
        for (ordinal, entry) in catalog.entries().iter().enumerate() {
            check(&mut canceled)?;
            if ImportLanguage::from_path(entry.path().raw().as_bytes()).is_none() { continue; }
            snapshot.eligible += 1;
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
        let response = self.import_response(atlas, &snapshot, "imports-begin", &mut canceled)?;
        self.imports_pending = Some(ImportsWork { snapshot, parser_id });
        Ok(response)
    }

    /// At most one captured file is parsed per step. A canceled/failed replacement
    /// withdraws its own work and preserves the older accepted inventory.
    pub fn step_imports(&mut self, atlas: &AtlasSession, generation: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?;
        if self.pending_import_generation() != Some(generation) {
            if self.imports_pending.is_none() && self.import_generation() == Some(generation)
                && self.last_import_attempt == generation {
                let mut out = self.import_output("imports-step")?;
                let snapshot = self.import_snapshot(generation)?;
                let index = self.import_index(snapshot)?;
                wire::page(&mut out, atlas, index, snapshot, None, AtlasImportDirection::All, 0, 64, &mut canceled)?;
                return self.finish(atlas, out, !snapshot.complete, &mut canceled);
            }
            return Err(AtlasSearchError::StaleImports);
        }
        let mut work = self.imports_pending.take().ok_or(AtlasSearchError::MissingImports)?;
        self.advance_imports(atlas, &mut work, &mut canceled)?;
        let response = self.import_response(atlas, &work.snapshot, "imports-step", &mut canceled)?;
        if work.snapshot.stop.is_some() { self.imports = Some(work.snapshot); }
        else { self.imports_pending = Some(work); }
        Ok(response)
    }

    pub fn imports_page(&mut self, atlas: &AtlasSession, generation: u64,
        direction: AtlasImportDirection, relative_path: Option<&[u8]>, start: usize, limit: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        if !(1..=MAX_ATLAS_SEARCH_PAGE).contains(&limit)
            || (relative_path.is_none() && direction != AtlasImportDirection::All) {
            return Err(AtlasSearchError::InvalidLimits);
        }
        let filter = match relative_path {
            Some(path) => {
                if path.is_empty() || path.len() > MAX_SEARCH_SCOPE_PATH_BYTES || path.contains(&0) {
                    return Err(AtlasSearchError::InvalidScope);
                }
                Some(find_file(atlas, path).ok_or(AtlasSearchError::MissingImportPath)?)
            }
            None => None,
        };
        let mut out = self.import_output("imports-page")?;
        let snapshot = self.import_snapshot(generation)?; let index = self.import_index(snapshot)?;
        wire::page(&mut out, atlas, index, snapshot, filter, direction, start, limit, &mut canceled)?;
        self.finish(atlas, out, !snapshot.complete, &mut canceled)
    }

    /// Selected exact source is copied to an independent reader namespace.
    /// Evidence opens at the mapped import range; target opens at byte zero.
    /// A target is a declared catalog candidate, not proof of compiler binding.
    pub fn open_import_reader(&mut self, atlas: &AtlasSession, reader_owner: ArenaOwnerId,
        generation: u64, id: u64, side: AtlasImportSide, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), AtlasSearchError> {
        self.validate(atlas)?; check(&mut canceled)?;
        if reader_owner == self.manifest.owner() { return Err(AtlasSearchError::WrongAtlas); }
        let mut out = self.import_output("imports-open-reader")?;
        let snapshot = self.import_snapshot(generation)?; let index = self.import_index(snapshot)?;
        let ordinal = id.checked_sub(1).and_then(|n| usize::try_from(n).ok()).ok_or(AtlasSearchError::MissingImport)?;
        let link = snapshot.links.get(ordinal).ok_or(AtlasSearchError::MissingImport)?;
        let selected = match side {
            AtlasImportSide::Evidence => link.source,
            AtlasImportSide::Target => link.target.ok_or(AtlasSearchError::ImportTargetUnavailable)?,
        };
        let source = index.engine.capture(selected.file).ok_or(AtlasSearchError::StaleIndex)?;
        if source.request().revision() != selected.revision || source.bytes().len() as u64 != selected.bytes {
            return Err(AtlasSearchError::StaleIndex);
        }
        let label = atlas.atlas().catalog().entry(selected.file).ok_or(AtlasSearchError::WrongAtlas)?.path().raw().to_path_buf();
        let mut stop = || canceled() || atlas.validate_active().is_err();
        let mut reader = ReaderSession::from_bytes(reader_owner, &label, source.bytes(), &mut stop)?;
        if reader.capture_sha256() != selected.digest { return Err(AtlasSearchError::StaleIndex); }
        let info = reader.info(&mut stop)?;
        wire::reader(&mut out, atlas, snapshot, link, id, side, selected, reader_owner, &info)?;
        let response = self.finish(atlas, out, false, &mut canceled)?;
        Ok((reader, response))
    }

    fn import_snapshot(&self, generation: u64) -> Result<&ImportsSnapshot, AtlasSearchError> {
        if let Some(work) = &self.imports_pending {
            if work.snapshot.generation == generation { return Ok(&work.snapshot); }
        }
        let snapshot = self.imports.as_ref().ok_or(AtlasSearchError::MissingImports)?;
        if snapshot.generation != generation { return Err(AtlasSearchError::StaleImports); }
        Ok(snapshot)
    }
    fn import_index(&self, snapshot: &ImportsSnapshot) -> Result<&RetainedIndex, AtlasSearchError> {
        let index = self.index.as_ref().ok_or(AtlasSearchError::MissingIndex)?;
        if index.generation != snapshot.index_generation || index.engine.id().revision() != snapshot.manifest {
            return Err(AtlasSearchError::StaleIndex);
        }
        Ok(index)
    }
    fn import_output(&mut self, command: &str) -> Result<Output, AtlasSearchError> {
        let id = self.next_id()?;
        let mut out = Output::new(self.manifest.owner(), 4 * 1024 * 1024, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.atlas-imports/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.manifest.owner().get())?;
        out.literal(",\"source_manifest\":")?; out.integer(self.manifest.revision())?;
        out.literal(",\"layout_revision\":")?; out.integer(self.layout.get())?;
        Ok(out)
    }
    fn import_response(&mut self, atlas: &AtlasSession, snapshot: &ImportsSnapshot,
        command: &str, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasSearchError> {
        let mut out = self.import_output(command)?;
        let index = self.import_index(snapshot)?;
        wire::page(&mut out, atlas, index, snapshot, None, AtlasImportDirection::All, 0, 64, canceled)?;
        self.finish(atlas, out, !snapshot.complete, canceled)
    }
    fn advance_imports(&self, atlas: &AtlasSession, work: &mut ImportsWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        self.validate(atlas)?; check(canceled)?;
        let index = self.import_index(&work.snapshot)?;
        let before = work.snapshot.examined;
        if work.snapshot.examined < work.snapshot.sources.len() { self.import_one_source(atlas, index, work, canceled)?; }
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
    fn import_one_source(&self, atlas: &AtlasSession, index: &RetainedIndex, work: &mut ImportsWork,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        let snapshot = &mut work.snapshot;
        let file = snapshot.sources[snapshot.examined];
        let capture = index.engine.capture(file).ok_or(AtlasSearchError::StaleIndex)?;
        if capture.bytes().len() > MAX_IMPORT_SOURCE_BYTES {
            snapshot.source_limited += 1; snapshot.examined += 1; return Ok(());
        }
        let entry = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?;
        let path = entry.path().raw().as_bytes();
        let language = ImportLanguage::from_path(path).ok_or(AtlasSearchError::WrongAtlas)?;
        let bytes = capture.bytes().len() as u64;
        if bytes > snapshot.max_parse_bytes.saturating_sub(snapshot.parsed_bytes) {
            snapshot.stop = Some(Stop::ParseBytes); return Ok(());
        }
        let qgen = QueryGeneration::new(self.manifest.owner(), snapshot.generation).map_err(|_| AtlasSearchError::IdentityExhausted)?;
        snapshot.parsed_bytes += bytes;
        let imports = match CapturedImports::build(capture.bytes(), *capture.request(), qgen,
            language, snapshot.max_links - snapshot.links.len(), &self.budget, work.parser_id,
            || canceled() || atlas.validate_active().is_err()) {
            Ok(imports) => imports,
            Err(ImportError::UnsupportedEncoding | ImportError::InvalidEvidence | ImportError::SourceLimit) => {
                snapshot.unsupported += 1; snapshot.examined += 1; return Ok(());
            }
            Err(error) => return Err(error.into()),
        };
        snapshot.scanned += 1;
        snapshot.diagnostics = snapshot.diagnostics.checked_add(imports.diagnostics() as u64).ok_or(AtlasSearchError::IdentityExhausted)?;
        snapshot.links_seen = snapshot.links_seen.checked_add(imports.links_seen()).ok_or(AtlasSearchError::IdentityExhausted)?;
        snapshot.truncated |= imports.truncated();
        for fact in imports.facts() {
            check(canceled)?;
            let resolved = resolve_import(path, fact, |candidate| find_file(atlas, candidate));
            let (resolution, target_file) = match resolved.kind {
                ImportResolutionKind::Local => {
                    let target = resolved.target.ok_or(AtlasSearchError::WrongAtlas)?;
                    (target_resolution(atlas, index, target)?, Some(target))
                }
                ImportResolutionKind::Missing => (Resolution::Missing, None),
                ImportResolutionKind::Ambiguous => (Resolution::Ambiguous, None),
                ImportResolutionKind::External => (Resolution::External, None),
                ImportResolutionKind::Unresolved => (Resolution::Unresolved, None),
            };
            if fact.destination().len() > MAX_ATLAS_IMPORT_METADATA_BYTES - snapshot.metadata_bytes {
                snapshot.truncated = true; continue;
            }
            let source = proof(snapshot, index, file, canceled)?;
            let target = if resolution == Resolution::Captured {
                Some(proof(snapshot, index, target_file.ok_or(AtlasSearchError::WrongAtlas)?, canceled)?)
            } else { None };
            let destination = copy_text(fact.destination())?;
            snapshot.metadata_bytes += destination.len();
            snapshot.links.push(Link { source, source_link: fact.id(), evidence: fact.original_range(),
                specifier: fact.specifier_range(), language, kind: fact.kind(), destination, resolution,
                basis: resolved.basis, target_file, target });
        }
        snapshot.examined += 1;
        Ok(())
    }
}
fn proof(snapshot: &mut ImportsSnapshot, index: &RetainedIndex, file: FileId,
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
    let catalog = atlas.atlas().catalog();
    let ordinal = catalog.entries().binary_search_by(|entry| entry.path().raw().as_bytes().cmp(path)).ok()?;
    catalog.file_id(ordinal)
}
fn target_resolution(atlas: &AtlasSession, index: &RetainedIndex, file: FileId) -> Result<Resolution, AtlasSearchError> {
    Ok(if index.engine.capture(file).is_some() { Resolution::Captured }
        else if index.engine.unavailable_files().binary_search(&file).is_ok() { Resolution::Unavailable }
        else {
            let path = atlas.atlas().catalog().entry(file).ok_or(AtlasSearchError::WrongAtlas)?.path().raw().as_bytes();
            if index.capture_scope.scope.includes_path(path) { Resolution::Uncaptured } else { Resolution::OutsideScope }
        })
}
