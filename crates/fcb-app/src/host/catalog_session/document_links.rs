#![forbid(unsafe_code)]

//! Explicit local navigation from a captured document under its catalog grant.
//! A reader label never authorizes filesystem access. The catalog remembers a
//! bounded receipt only after it has captured an authorized member successfully;
//! activation must present that reader's exact request, digest and raw path.
//! Source receipts outlive metadata pages. Explicit target paths use the same
//! active root grant and no-follow capture service even before discovery reaches
//! them. No implicit traversal, URI open, supplied-source fallback or new grant
//! occurs; direct captures occupy a separate identity range and never add rows.

use fcb::{ArenaOwnerId, FileId};
use fcb::document::resolve_document_destination;
use fcb::source::{CaptureRequest, NormalizedPath, RawPath};
use fcb::store::Sha256Digest;
use super::{CatalogSession, CatalogSessionError, HostResponse, ReaderSession,
    FIRST_WINDOW_BYTES, MAX_CATALOG_PATH_BYTES, MAX_READER_SOURCE_BYTES, check};

pub const MAX_CATALOG_READER_RECEIPTS: usize = 8;
/// Page zero denotes an explicit root-granted capture, never a discovery page.
/// Its file identities cannot alias monotonically numbered metadata members.
pub const FIRST_CATALOG_LINK_FILE_ID: u64 = 1 << 63;

/// The catalog selection or page-zero direct receipt that produced the origin
/// capture, plus a parser occurrence in its CURRENT document generation.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CatalogDocumentLinkSelection {
    pub source_page: u64,
    pub source_file: FileId,
    pub document_generation: u64,
    pub link_id: u64,
}

#[derive(Clone)]
struct CatalogReaderReceipt {
    owner: ArenaOwnerId,
    page: u64,
    file: FileId,
    request: CaptureRequest,
    digest: Sha256Digest,
    path: RawPath,
}
impl CatalogReaderReceipt {
    fn new(page: u64, file: FileId, path: &NormalizedPath, reader: &ReaderSession)
        -> Result<Self, CatalogSessionError> {
        if path.as_bytes().len() > MAX_CATALOG_PATH_BYTES {
            return Err(CatalogSessionError::InvalidLimits);
        }
        Ok(Self {
            owner: reader.capture().request().file().owner(), page, file,
            request: *reader.capture().request(), digest: reader.capture_sha256(),
            path: path.raw().clone(),
        })
    }
    fn path(&self) -> &[u8] { self.path.as_bytes() }
}

/// A completed catalog capture whose receipt has not yet been accepted by its
/// reader owner. Fields are private, and the exclusive borrow keeps this
/// catalog's receipt state stable while the registry installs its candidate.
/// Dropping an unaccepted capture leaves every existing receipt unchanged.
pub struct CatalogReaderAcceptance<'catalog> {
    catalog: &'catalog mut CatalogSession,
    origin: Option<CatalogReaderReceipt>,
    captured: CatalogReaderReceipt,
    next_link_file: u64,
}
impl CatalogReaderAcceptance<'_> {
    /// Publish only after the receiving owner has installed the captured reader.
    /// There is no allocation, cancellation callback or fallible operation here.
    pub fn accept(self) {
        if let Some(origin) = self.origin { self.catalog.reader_receipts.remember(origin); }
        self.catalog.reader_receipts.remember(self.captured);
        self.catalog.next_link_file = self.next_link_file;
    }
}

/// The fixed receipt slots and their bounded shared raw paths are included in
/// the catalog's original state admission. More readers never grow metadata;
/// receipt eviction disables only link activation,
/// not retained source reading. Successful activation refreshes its origin.
#[derive(Default)]
pub(super) struct CatalogReaderReceipts {
    recent: [Option<CatalogReaderReceipt>; MAX_CATALOG_READER_RECEIPTS],
}
impl CatalogReaderReceipts {
    fn remember(&mut self, receipt: CatalogReaderReceipt) {
        let position = self.recent.iter().position(|entry| entry.as_ref().is_some_and(|entry| entry.owner == receipt.owner))
            .unwrap_or(MAX_CATALOG_READER_RECEIPTS - 1);
        for index in (1..=position).rev() { self.recent[index] = self.recent[index - 1].take(); }
        self.recent[0] = Some(receipt);
    }
}

struct Target {
    page: u64,
    file: FileId,
    path: NormalizedPath,
    observed_bytes: Option<u64>,
    next_link_file: u64,
}
impl Target {
    fn resolution(&self) -> &'static str {
        if self.page == 0 { "root-grant-path" } else { "retained-catalog-entry" }
    }
}

impl CatalogSession {
    /// Only produced after this catalog's successful capture and response
    /// preparation. The token cannot register an arbitrary supplied reader,
    /// fork, saved member or caller-selected path as a catalog capture.
    pub(super) fn reader_acceptance<'catalog>(&'catalog mut self, page: u64,
        file: FileId, path: &NormalizedPath, reader: &ReaderSession)
        -> Result<CatalogReaderAcceptance<'catalog>, CatalogSessionError> {
        let captured = CatalogReaderReceipt::new(page, file, path, reader)?;
        let next_link_file = self.next_link_file;
        Ok(CatalogReaderAcceptance { catalog: self, origin: None, captured, next_link_file })
    }

    /// Open a CURRENT granted target into an independent reader. The origin
    /// capture and its document state are borrowed and never replaced. The
    /// bridge additionally retains both reader cells and combines cancellation
    /// epochs with this catalog's active grant for the entire operation.
    ///
    /// A successful target is a new file observation, not a historical capture
    /// of the link destination. Bounded Markdown fragments use qualified
    /// canonical heading ranges; other outcomes explicitly start at zero.
    pub fn open_document_link(&mut self, origin: &ReaderSession, reader_owner: ArenaOwnerId,
        selection: CatalogDocumentLinkSelection, max_source_bytes: usize,
        canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), CatalogSessionError> {
        let (reader, response, acceptance) = self.prepare_document_link_capture(origin,
            reader_owner, selection, max_source_bytes, canceled)?;
        acceptance.accept();
        Ok((reader, response))
    }

    /// Prepare a linked capture for a registry that can still reject its empty
    /// destination at a final cancellation/admission boundary. Receipt acceptance
    /// must occur immediately after that registry installs the returned reader.
    pub fn prepare_document_link_capture<'catalog>(&'catalog mut self,
        origin: &ReaderSession, reader_owner: ArenaOwnerId,
        selection: CatalogDocumentLinkSelection, max_source_bytes: usize,
        mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse, CatalogReaderAcceptance<'catalog>), CatalogSessionError> {
        check(&mut canceled)?;
        let origin_owner = origin.capture().request().file().owner();
        if selection.source_file.owner() != self.owner() || reader_owner == self.owner()
            || origin_owner == self.owner() || reader_owner == origin_owner {
            return Err(CatalogSessionError::ForeignOwner);
        }
        if max_source_bytes == 0 || max_source_bytes > MAX_READER_SOURCE_BYTES {
            return Err(CatalogSessionError::InvalidLimits);
        }
        // Reject stale/forged source receipts and link generations before any
        // root metadata or target read. Equal labels or equal digests alone do
        // not grant this reader the receipt of a different source owner.
        let receipt = self.link_origin(origin, selection)?;
        let destination = origin.document_link_destination(selection.document_generation, selection.link_id)?;
        let source_path = std::str::from_utf8(receipt.path())
            .map_err(|_| CatalogSessionError::UnsupportedDocumentLink)?;
        let (path, suffix) = resolve_document_destination(source_path, destination)
            .ok_or(CatalogSessionError::UnsupportedDocumentLink)?;
        let target = self.link_target(path.as_bytes(), &mut canceled)?;
        self.validate_active()?; check(&mut canceled)?;
        let (mut reader, info, initial_window, source_bytes, source_calls) =
            self.capture_reader(reader_owner, &target.path, max_source_bytes, &mut canceled)?;
        let (fragment_resolution, fragment_slug) = reader.resolve_document_fragment(&suffix, &mut canceled)?;
        let initial_original_offset = fragment_resolution.range().map(|range| range.start().get()).unwrap_or(0);
        let window = if initial_original_offset == 0 { initial_window } else {
            drop(initial_window);
            reader.read_window(initial_original_offset, FIRST_WINDOW_BYTES, &mut canceled)?
        };
        // Explicitly revalidate both authorities after acquisition. The source
        // reader is immutable under this borrow, and the bridge's cancellation
        // closure also covers its close/replacement epoch.
        self.link_origin(origin, selection)?;
        origin.document_link_destination(selection.document_generation, selection.link_id)?;
        self.validate_active()?; check(&mut canceled)?;
        let mut out = self.output("open-document-link")?;
        for (key, value) in [("source_page_id", selection.source_page),
            ("source_file_id", selection.source_file.get()), ("source_reader_owner", origin_owner.get()),
            ("document_generation", selection.document_generation), ("link_id", selection.link_id),
            ("target_page_id", target.page), ("reader_owner", reader_owner.get())] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"source_authority\":\"retained-catalog-capture\",\"target_resolution\":")?;
        out.quoted(target.resolution())?;
        out.literal(",\"source_capture_sha256\":")?; out.quoted(&receipt.digest.to_hex())?;
        out.literal(",\"target_file\":{\"file_id\":")?; out.integer(target.file.get())?;
        out.literal(",\"path\":")?; out.path(&target.path.raw().to_path_buf())?;
        out.literal(",\"observed_bytes\":")?; out.integer(target.observed_bytes.unwrap_or(source_bytes))?; out.literal("}")?;
        out.literal(",\"source_observation\":\"new-capture-after-document-link\",\"source_bytes_read\":")?;
        out.integer(source_bytes)?;
        out.literal(",\"source_read_calls\":")?; out.integer(source_calls)?;
        out.literal(",\"destination\":")?; out.quoted(destination)?;
        out.literal(",\"destination_suffix\":")?; out.quoted(&suffix)?;
        let fragment = suffix.find('#').map(|start| &suffix[start..]).unwrap_or("");
        out.literal(",\"fragment_status\":")?;
        out.quoted(fragment_resolution.name())?;
        out.literal(",\"fragment_suffix\":")?; out.quoted(fragment)?;
        out.literal(",\"fragment_slug\":")?;
        match &fragment_slug { Some(slug) => out.quoted(slug)?, None => out.literal("null")? }
        out.literal(",\"target_range\":")?;
        match fragment_resolution.range() { Some(range) => out.range(range)?, None => out.literal("null")? }
        out.literal(",\"initial_original_offset\":")?; out.integer(initial_original_offset)?;
        out.literal(",\"reader\":")?; out.literal(info.as_str())?;
        out.literal(",\"window\":")?; out.literal(window.as_str())?; out.literal("}\n")?;
        let response = self.finish(out, &mut canceled)?;
        self.validate_active()?;
        // Refresh the origin only when the receiving owner accepts this reader.
        // A rejected candidate must neither grant a reusable empty handle nor
        // evict a receipt belonging to an already accepted capture.
        let mut acceptance = self.reader_acceptance(target.page, target.file, &target.path, &reader)?;
        acceptance.origin = Some(receipt);
        acceptance.next_link_file = target.next_link_file;
        Ok((reader, response, acceptance))
    }

    fn link_origin(&self, origin: &ReaderSession, selection: CatalogDocumentLinkSelection)
        -> Result<CatalogReaderReceipt, CatalogSessionError> {
        let owner = origin.capture().request().file().owner();
        let receipt = self.reader_receipts.recent.iter().flatten().find(|receipt| receipt.owner == owner)
            .cloned().ok_or(CatalogSessionError::StaleReaderReceipt)?;
        if receipt.page != selection.source_page || receipt.file != selection.source_file
            || receipt.request != *origin.capture().request() || receipt.digest != origin.capture_sha256()
            || origin.path().as_os_str() != receipt.path.to_path_buf().as_os_str() {
            return Err(CatalogSessionError::ReaderReceiptMismatch);
        }
        Ok(receipt)
    }

    fn link_target(&self, path: &[u8], canceled: &mut impl FnMut() -> bool)
        -> Result<Target, CatalogSessionError> {
        let mut target = None;
        for page in &self.pages {
            for file in &page.files {
                check(canceled)?;
                if file.path.as_bytes() != path { continue; }
                if target.is_some() { return Err(CatalogSessionError::AmbiguousLinkTarget); }
                target = Some(Target { page: page.id, file: file.id,
                    path: file.path.clone(), observed_bytes: Some(file.observed_bytes),
                    next_link_file: self.next_link_file });
            }
        }
        if let Some(target) = target { return Ok(target); }
        if path.len() > MAX_CATALOG_PATH_BYTES { return Err(CatalogSessionError::InvalidLimits); }
        let next_link_file = self.next_link_file.checked_add(1).ok_or(CatalogSessionError::IdentityExhausted)?;
        Ok(Target { page: 0,
            file: FileId::new(self.owner(), self.next_link_file).map_err(|_| CatalogSessionError::IdentityExhausted)?,
            path: NormalizedPath::new(RawPath::from_bytes(path))?, observed_bytes: None, next_link_file })
    }
}
