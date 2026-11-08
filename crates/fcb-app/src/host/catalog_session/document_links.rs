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
use fcb::source::{CaptureRequest, NormalizedPath, RawPath, SourceError};
use fcb::store::Sha256Digest;
use super::{AppError, CatalogSession, CatalogSessionError, HostError, HostResponse, ReaderSession, ReaderSessionError,
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
    // Relative resolution and exact reader-label identity are separate. A
    // verified ordinary reader can have an absolute label under this root.
    label: RawPath,
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
            path: path.raw().clone(), label: path.raw().clone(),
        })
    }
    fn path(&self) -> &[u8] { self.path.as_bytes() }
    fn validate(&self, origin: &ReaderSession, selection: CatalogDocumentLinkSelection)
        -> Result<(), CatalogSessionError> {
        if self.page != selection.source_page || self.file != selection.source_file
            || self.request != *origin.capture().request() || self.digest != origin.capture_sha256()
            || origin.path().as_os_str() != self.label.to_path_buf().as_os_str() {
            return Err(CatalogSessionError::ReaderReceiptMismatch);
        }
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum SourceAuthority {
    Retained,
    Verified { bytes_read: u64, read_calls: u64 },
}
impl SourceAuthority {
    fn name(self) -> &'static str {
        match self { Self::Retained => "retained-catalog-capture", Self::Verified { .. } => "verified-current-source" }
    }
}

struct LinkOrigin {
    receipt: CatalogReaderReceipt,
    selection: CatalogDocumentLinkSelection,
    authority: SourceAuthority,
    next_link_file: u64,
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
        let link = LinkOrigin { receipt, selection, authority: SourceAuthority::Retained,
            next_link_file: self.next_link_file };
        self.prepare_link_target_capture(origin, reader_owner, link, max_source_bytes, &mut canceled)
    }

    /// Establish authority for an existing supplied, search or pinned reader by
    /// comparing its exact immutable bytes with a CURRENT observation under this
    /// root. The label alone conveys no authority. Verification does not replace
    /// the origin, and both receipts remain unpublished until target acceptance.
    pub fn prepare_verified_document_link_capture<'catalog>(&'catalog mut self,
        origin: &ReaderSession, reader_owner: ArenaOwnerId, document_generation: u64,
        link_id: u64, max_source_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse, CatalogReaderAcceptance<'catalog>), CatalogSessionError> {
        check(&mut canceled)?;
        let origin_owner = origin.capture().request().file().owner();
        if reader_owner == self.owner() || origin_owner == self.owner() || reader_owner == origin_owner {
            return Err(CatalogSessionError::ForeignOwner);
        }
        if max_source_bytes == 0 || max_source_bytes > MAX_READER_SOURCE_BYTES
            || origin.capture().bytes().len() > max_source_bytes {
            return Err(CatalogSessionError::InvalidLimits);
        }
        // Reject stale parser occurrences before even root metadata is touched.
        let destination = origin.document_link_destination(document_generation, link_id)?;
        let root = self.grant.root_path().to_path_buf();
        let requested_root = self.requested_root.to_path_buf();
        let (relative, label_root) = if origin.path().is_absolute() {
            [root.as_path(), requested_root.as_path()].into_iter().filter_map(|label_root| {
                // Permit only the canonical root or the exact originally
                // admitted spelling (for example /var versus /private/var).
                // Never canonicalize a caller's label or follow its aliases.
                // Prefer the longest approved prefix when these names overlap.
                let clean: std::path::PathBuf = label_root.components().collect();
                if clean.as_os_str() != label_root.as_os_str()
                    || label_root.components().any(|part| matches!(part, std::path::Component::ParentDir | std::path::Component::CurDir)) {
                    return None;
                }
                let relative = origin.path().strip_prefix(label_root).ok()?;
                if label_root.join(relative).as_os_str() != origin.path().as_os_str() { return None; }
                Some((relative, label_root))
            }).max_by_key(|(_, label_root)| label_root.as_os_str().len()).ok_or(SourceError::PathEscape)?
        } else { (origin.path(), root.as_path()) };
        let raw_relative = RawPath::from_path(relative);
        if raw_relative.len() > MAX_CATALOG_PATH_BYTES { return Err(CatalogSessionError::InvalidLimits); }
        let path = NormalizedPath::new(raw_relative.clone())?;
        // Neither external separator normalization nor Path's component-wise
        // equality may reinterpret a native label into another filesystem name.
        if path.as_bytes() != raw_relative.as_bytes()
            || (origin.path().is_absolute()
                && label_root.join(path.raw().to_path_buf()).as_os_str() != origin.path().as_os_str()) {
            return Err(SourceError::PathEscape.into());
        }
        let source_path = std::str::from_utf8(path.as_bytes())
            .map_err(|_| CatalogSessionError::UnsupportedDocumentLink)?;
        let (destination_path, _) = resolve_document_destination(source_path, destination)
            .ok_or(CatalogSessionError::UnsupportedDocumentLink)?;
        if destination_path.len() > MAX_CATALOG_PATH_BYTES { return Err(CatalogSessionError::InvalidLimits); }
        self.validate_active()?; check(&mut canceled)?;
        let source = crate::workspace::checked_source_path(&root, &path)?;
        let (bytes_read, read_calls) = {
            let raw = {
                let mut stop = || canceled() || self.validate_active().is_err();
                // A larger current file cannot equal the retained capture. Limit
                // acquisition to that original length; never create a second
                // ReaderSession or keep verification bytes during target capture.
                let result = super::super::read_bytes_with_limit(&source,
                    origin.capture().bytes().len().max(1), MAX_READER_SOURCE_BYTES, &mut stop);
                self.validate_active()?; check(&mut canceled)?;
                result.map_err(|error| match error {
                    HostError::App(AppError::InputLimit | AppError::SourceChanged) => CatalogSessionError::OriginCaptureChanged,
                    error => CatalogSessionError::Reader(ReaderSessionError::Host(error)),
                })?
            };
            if raw.bytes.len() != origin.capture().bytes().len() {
                return Err(CatalogSessionError::OriginCaptureChanged);
            }
            for (current, retained) in raw.bytes.chunks(64 * 1024).zip(origin.capture().bytes().chunks(64 * 1024)) {
                check(&mut canceled)?;
                if current != retained { return Err(CatalogSessionError::OriginCaptureChanged); }
            }
            self.validate_active()?; check(&mut canceled)?;
            (raw.bytes.len() as u64, raw.read_calls)
        };
        let file = FileId::new(self.owner(), self.next_link_file).map_err(|_| CatalogSessionError::IdentityExhausted)?;
        let next_link_file = self.next_link_file.checked_add(1).ok_or(CatalogSessionError::IdentityExhausted)?;
        let mut receipt = CatalogReaderReceipt::new(0, file, &path, origin)?;
        receipt.label = RawPath::from_path(origin.path());
        let selection = CatalogDocumentLinkSelection { source_page: 0, source_file: file, document_generation, link_id };
        let link = LinkOrigin { receipt, selection, authority: SourceAuthority::Verified { bytes_read, read_calls }, next_link_file };
        self.prepare_link_target_capture(origin, reader_owner, link, max_source_bytes, &mut canceled)
    }

    fn prepare_link_target_capture<'catalog>(&'catalog mut self, origin: &ReaderSession,
        reader_owner: ArenaOwnerId, link: LinkOrigin, max_source_bytes: usize,
        canceled: &mut impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse, CatalogReaderAcceptance<'catalog>), CatalogSessionError> {
        let LinkOrigin { receipt, selection, authority, next_link_file } = link;
        let origin_owner = origin.capture().request().file().owner();
        let destination = origin.document_link_destination(selection.document_generation, selection.link_id)?;
        let source_path = std::str::from_utf8(receipt.path())
            .map_err(|_| CatalogSessionError::UnsupportedDocumentLink)?;
        let (path, suffix) = resolve_document_destination(source_path, destination)
            .ok_or(CatalogSessionError::UnsupportedDocumentLink)?;
        let target = self.link_target(path.as_bytes(), next_link_file, canceled)?;
        self.validate_active()?; check(canceled)?;
        let (mut reader, info, initial_window, source_bytes, source_calls) =
            self.capture_reader(reader_owner, &target.path, max_source_bytes, canceled)?;
        let (fragment_resolution, fragment_slug) = reader.resolve_document_fragment(&suffix, &mut *canceled)?;
        let initial_original_offset = fragment_resolution.range().map(|range| range.start().get()).unwrap_or(0);
        let window = if initial_original_offset == 0 { initial_window } else {
            drop(initial_window);
            reader.read_window(initial_original_offset, FIRST_WINDOW_BYTES, &mut *canceled)?
        };
        // Explicitly revalidate both authorities after acquisition. The source
        // reader is immutable under this borrow, and the bridge's cancellation
        // closure also covers its close/replacement epoch.
        match authority {
            SourceAuthority::Retained => { self.link_origin(origin, selection)?; }
            SourceAuthority::Verified { .. } => receipt.validate(origin, selection)?,
        }
        origin.document_link_destination(selection.document_generation, selection.link_id)?;
        self.validate_active()?; check(canceled)?;
        let mut out = self.output("open-document-link")?;
        for (key, value) in [("source_page_id", selection.source_page),
            ("source_file_id", selection.source_file.get()), ("source_reader_owner", origin_owner.get()),
            ("document_generation", selection.document_generation), ("link_id", selection.link_id),
            ("target_page_id", target.page), ("reader_owner", reader_owner.get())] {
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
        }
        out.literal(",\"source_authority\":")?; out.quoted(authority.name())?;
        if let SourceAuthority::Verified { bytes_read, read_calls } = authority {
            out.literal(",\"source_verification_bytes_read\":")?; out.integer(bytes_read)?;
            out.literal(",\"source_verification_read_calls\":")?; out.integer(read_calls)?;
        }
        out.literal(",\"target_resolution\":")?; out.quoted(target.resolution())?;
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
        let response = self.finish(out, canceled)?;
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
        receipt.validate(origin, selection)?;
        Ok(receipt)
    }

    fn link_target(&self, path: &[u8], next_link_file: u64, canceled: &mut impl FnMut() -> bool)
        -> Result<Target, CatalogSessionError> {
        let mut target = None;
        for page in &self.pages {
            for file in &page.files {
                check(canceled)?;
                if file.path.as_bytes() != path { continue; }
                if target.is_some() { return Err(CatalogSessionError::AmbiguousLinkTarget); }
                target = Some(Target { page: page.id, file: file.id,
                    path: file.path.clone(), observed_bytes: Some(file.observed_bytes),
                    next_link_file });
            }
        }
        if let Some(target) = target { return Ok(target); }
        if path.len() > MAX_CATALOG_PATH_BYTES { return Err(CatalogSessionError::InvalidLimits); }
        let file = FileId::new(self.owner(), next_link_file).map_err(|_| CatalogSessionError::IdentityExhausted)?;
        let next_link_file = next_link_file.checked_add(1).ok_or(CatalogSessionError::IdentityExhausted)?;
        Ok(Target { page: 0, file,
            path: NormalizedPath::new(RawPath::from_bytes(path))?, observed_bytes: None, next_link_file })
    }
}
