#![forbid(unsafe_code)]

//! Explicit native-worker acquisition of local image bytes. A reader's path
//! remains a label: authority requires a separately supplied current root and
//! relative source path, followed by an exact comparison with retained source.
//! The upstream display supplies every image occurrence and destination.
//! Encoded captures are immutable; native ImageIO owns real decoding and its
//! independent dimension/frame/pixel/thumbnail budgets. No synthetic decoder,
//! network client, filesystem include, source mutation or layout engine exists
//! here. Path checks retain the existing source service's documented limit:
//! observed symlinks are refused, but hostile ancestor races are not qualified.

use std::{fs, mem::size_of, path::{Component, Path, PathBuf}};
use fcb::ByteLength;
use fcb::document::{ImageCodecValidator, ImageFormat, resolve_document_destination};
use fcb::source::{CaptureRequest, DirectoryId, NormalizedPath, RawPath, RootGrant, SourceError};
use fcb::store::Sha256Digest;
use fcb_core::{ResourceLease, RootId};
use crate::{input, workspace, AppError};
use crate::host::{HostBytes, HostError, HostResponse, read_bytes_with_limit};
use super::{check, source_sha256, Output, ReaderSession, ReaderSessionError, EXIT_OK};

pub const MAX_DOCUMENT_IMAGE_BYTES: usize = 4 * 1024 * 1024;
pub const MAX_DOCUMENT_IMAGE_CACHE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_DOCUMENT_IMAGE_CHUNK: usize = 64 * 1024;
pub const MAX_DOCUMENT_IMAGES: usize = 16;
const MAX_ASSET_PATH_BYTES: usize = 16_384;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DocumentImageError {
    NotAuthorized, OriginMismatch, OriginChanged, DifferentAuthority,
    UnsupportedDestination, UnsupportedFormat, CacheLimit, MissingImage,
    InvalidLimits, InvalidRange,
}
impl DocumentImageError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::NotAuthorized => "DOCUMENT_IMAGE_NOT_AUTHORIZED",
            Self::OriginMismatch => "DOCUMENT_IMAGE_ORIGIN_MISMATCH",
            Self::OriginChanged => "DOCUMENT_IMAGE_ORIGIN_CHANGED",
            Self::DifferentAuthority => "DOCUMENT_IMAGE_DIFFERENT_AUTHORITY",
            Self::UnsupportedDestination => "DOCUMENT_IMAGE_DESTINATION_UNSUPPORTED",
            Self::UnsupportedFormat => "DOCUMENT_IMAGE_FORMAT_UNSUPPORTED",
            Self::CacheLimit => "DOCUMENT_IMAGE_CACHE_LIMIT",
            Self::MissingImage => "DOCUMENT_IMAGE_NOT_PREPARED",
            Self::InvalidLimits => "DOCUMENT_IMAGE_INVALID_LIMITS",
            Self::InvalidRange => "DOCUMENT_IMAGE_INVALID_RANGE",
        }
    }
}
impl std::fmt::Display for DocumentImageError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for DocumentImageError {}
impl From<DocumentImageError> for ReaderSessionError {
    fn from(error: DocumentImageError) -> Self { Self::DocumentImage(error) }
}

struct RetainedImage {
    item_index: usize,
    request_id: u64,
    data: HostBytes,
    digest: Sha256Digest,
    format: ImageFormat,
}

pub(super) struct DocumentImages {
    generation: u64,
    width_points: u32,
    source: CaptureRequest,
    source_digest: Sha256Digest,
    requested_root: PathBuf,
    root: PathBuf,
    root_identity: DirectoryId,
    grant: RootGrant,
    source_relative: NormalizedPath,
    images: Vec<RetainedImage>,
    bytes: usize,
    _lease: ResourceLease,
}
impl DocumentImages {
    fn validate_active(&self) -> Result<(), ReaderSessionError> {
        self.grant.validate_active()?;
        let metadata = fs::symlink_metadata(&self.root).map_err(|_| AppError::SourceChanged)?;
        if metadata.file_type().is_symlink() || !metadata.is_dir()
            || DirectoryId::from_path(&self.root).map_err(|_| AppError::SourceChanged)? != self.root_identity
            || fs::canonicalize(&self.root).map_err(|_| AppError::SourceChanged)? != self.root {
            return Err(AppError::SourceChanged.into());
        }
        self.grant.validate_active()?;
        Ok(())
    }
    fn image(&self, item_index: usize, request_id: u64) -> Option<&RetainedImage> {
        self.images.iter().find(|image| image.item_index == item_index && image.request_id == request_id)
    }
}

impl ReaderSession {
    /// The caller retains its current native root access lease and combines
    /// revocation/reader epochs with the cancellation callback for this worker.
    /// Root and relative path are explicit capabilities, never inferred from the
    /// reader's label. The captured Markdown must still equal a no-follow current
    /// observation. Authorization is immutable for one document generation;
    /// repeated calls with the SAME input perform no further source I/O.
    pub fn authorize_document_images(&mut self, generation: u64, root: &Path,
        source_relative: &[u8], mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        let width_points = self.display_snapshot(generation)?.width_points();
        if !root.is_absolute() || root.as_os_str().len() > MAX_ASSET_PATH_BYTES
            || source_relative.is_empty() || source_relative.len() > MAX_ASSET_PATH_BYTES {
            return Err(DocumentImageError::InvalidLimits.into());
        }
        if let Some(assets) = &self.document_images {
            self.image_snapshot(generation)?;
            if assets.requested_root.as_os_str() != root.as_os_str()
                || assets.source_relative.as_bytes() != source_relative {
                return Err(DocumentImageError::DifferentAuthority.into());
            }
            return self.image_authorization_response(generation, None, 0, 0, &mut canceled);
        }
        // Admission precedes path copies, normalization, source verification and
        // the fixed cache vector. Retained bytes have both this aggregate reader
        // envelope and the existing file reader's acquisition/handoff lease.
        let [allocation] = self.allocations()?;
        let charge = MAX_DOCUMENT_IMAGE_CACHE_BYTES + 256 * 1024 + size_of::<DocumentImages>();
        let lease = self.budget.try_reserve_managed(self.owner(), allocation,
            ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let requested_root = root.to_path_buf();
        let clean: PathBuf = root.components().collect();
        if clean.as_os_str() != root.as_os_str()
            || root.components().any(|part| matches!(part, Component::ParentDir | Component::CurDir)) {
            return Err(DocumentImageError::OriginMismatch.into());
        }
        let relative = NormalizedPath::new(RawPath::from_bytes(source_relative))?;
        if relative.as_bytes() != source_relative || std::str::from_utf8(relative.as_bytes()).is_err() {
            return Err(DocumentImageError::OriginMismatch.into());
        }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let metadata = fs::symlink_metadata(root).map_err(|_| AppError::Io)?;
        if metadata.file_type().is_symlink() { return Err(AppError::Symlink.into()); }
        if !metadata.is_dir() { return Err(AppError::Directory.into()); }
        let canonical_root = fs::canonicalize(root).map_err(|_| AppError::Io)?;
        if canonical_root.as_os_str().len() > MAX_ASSET_PATH_BYTES {
            return Err(DocumentImageError::InvalidLimits.into());
        }
        let relative_native = relative.raw().to_path_buf();
        let origin_matches = if self.path.is_absolute() {
            [root, canonical_root.as_path()].into_iter().any(|label_root|
                label_root.join(&relative_native).as_os_str() == self.path.as_os_str())
        } else { self.path.as_os_str() == relative_native.as_os_str() };
        if !origin_matches { return Err(DocumentImageError::OriginMismatch.into()); }
        let root_identity = DirectoryId::from_path(&canonical_root)?;
        let grant = RootGrant::new(RootId::new(self.owner(), generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?, RawPath::from_path(&canonical_root));
        let mut images = Vec::new();
        images.try_reserve_exact(MAX_DOCUMENT_IMAGES).map_err(|_| AppError::Admission)?;
        if images.capacity() > MAX_DOCUMENT_IMAGES { return Err(AppError::Admission.into()); }
        let candidate = DocumentImages { generation, width_points, source: *self.capture.request(),
            source_digest: self.capture_sha256, requested_root, root: canonical_root, root_identity,
            grant, source_relative: relative, images, bytes: 0, _lease: lease };
        candidate.validate_active()?; check(&mut canceled)?;
        let source = workspace::checked_source_path(&candidate.root, &candidate.source_relative)?;
        let (bytes_read, read_calls) = {
            let result = {
                let mut stop = || canceled() || candidate.validate_active().is_err();
                read_bytes_with_limit(&source, self.capture.bytes().len().max(1),
                    fcb::document::native_display::MAX_DISPLAY_SOURCE_BYTES, &mut stop)
            };
            candidate.validate_active()?; check(&mut canceled)?;
            let raw = result.map_err(|error| match error {
                HostError::App(AppError::InputLimit | AppError::SourceChanged) =>
                    ReaderSessionError::DocumentImage(DocumentImageError::OriginChanged),
                other => ReaderSessionError::Host(other),
            })?;
            if raw.bytes.len() != self.capture.bytes().len() {
                return Err(DocumentImageError::OriginChanged.into());
            }
            for (current, captured) in raw.bytes.chunks(64 * 1024).zip(self.capture.bytes().chunks(64 * 1024)) {
                check(&mut canceled)?;
                if current != captured { return Err(DocumentImageError::OriginChanged.into()); }
            }
            (raw.bytes.len() as u64, raw.read_calls)
        };
        let response = self.image_authorization_response(generation, Some(&candidate),
            bytes_read, read_calls, &mut canceled)?;
        candidate.validate_active()?; check(&mut canceled)?;
        self.document_images = Some(candidate);
        Ok(response)
    }

    fn image_authorization_response(&mut self, generation: u64, candidate: Option<&DocumentImages>,
        bytes_read: u64, read_calls: u64, canceled: &mut impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        let mut out = self.output_with_source_reads("document-images-authorize", bytes_read)?;
        let assets = match candidate { Some(assets) => assets, None => self.image_snapshot(generation)? };
        image_summary(&mut out, assets, assets.bytes, assets.images.len())?;
        out.literal(",\"images_authorized\":true,\"source_authority\":\"verified-current-source\",\"authorization_reused\":")?;
        out.boolean(candidate.is_none())?;
        out.literal(",\"source_verification_bytes_read\":")?; out.integer(bytes_read)?;
        out.literal(",\"source_verification_read_calls\":")?; out.integer(read_calls)?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, canceled)
    }

    /// Captures only a source-qualified upstream image destination under the
    /// accepted root. The response contains its first bounded encoded chunk.
    /// A repeated request replays the SAME bytes, even if the asset later changes.
    /// Acquisition is a current file observation, not a historical image paired
    /// with the Markdown capture. A native decoder must validate actual pixels.
    pub fn prepare_document_image(&mut self, generation: u64, item_index: usize, request_id: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(&mut canceled)?;
        let image = self.display_snapshot(generation)?.image_request(item_index, request_id)?;
        let assets = self.image_snapshot(generation)?;
        if assets.image(item_index, request_id).is_some() {
            return self.image_response("document-image-prepare", generation, item_index, request_id,
                None, 0, MAX_DOCUMENT_IMAGE_CHUNK, &mut canceled);
        }
        if assets.images.len() >= MAX_DOCUMENT_IMAGES || assets.bytes >= MAX_DOCUMENT_IMAGE_CACHE_BYTES {
            return Err(DocumentImageError::CacheLimit.into());
        }
        let destination = image.destination.trim();
        if destination.is_empty() || destination.starts_with('/') || destination.starts_with('\\')
            || destination.chars().any(char::is_control) {
            return Err(DocumentImageError::UnsupportedDestination.into());
        }
        let source = std::str::from_utf8(assets.source_relative.as_bytes())
            .map_err(|_| DocumentImageError::UnsupportedDestination)?;
        let (path, suffix) = resolve_document_destination(source, image.destination)
            .ok_or(DocumentImageError::UnsupportedDestination)?;
        // Query/fragment semantics are not silently discarded to read a nearby
        // filename. The upstream resolver decodes the path exactly once.
        if !suffix.is_empty() || path.len() > MAX_ASSET_PATH_BYTES {
            return Err(DocumentImageError::UnsupportedDestination.into());
        }
        let relative = NormalizedPath::new(RawPath::from_bytes(path.as_bytes()))?;
        if relative.as_bytes() != path.as_bytes() { return Err(SourceError::PathEscape.into()); }
        assets.validate_active()?; check(&mut canceled)?;
        let native = workspace::checked_source_path(&assets.root, &relative)?;
        let limit = MAX_DOCUMENT_IMAGE_BYTES.min(MAX_DOCUMENT_IMAGE_CACHE_BYTES - assets.bytes);
        let result = {
            let mut stop = || canceled() || assets.validate_active().is_err();
            read_bytes_with_limit(&native, limit, MAX_DOCUMENT_IMAGE_BYTES, &mut stop)
        };
        assets.validate_active()?; check(&mut canceled)?;
        let data = result?;
        // Signature admission is deliberately NOT codec validation. In
        // particular, never use the legacy header helper's fallback dimensions
        // or the synthetic BoundedImageDecoder as a real image decode.
        let format = match ImageCodecValidator::sniff_format(&data.bytes) {
            Some(format @ (ImageFormat::Png | ImageFormat::Jpeg)) => format,
            _ => return Err(DocumentImageError::UnsupportedFormat.into()),
        };
        let digest = source_sha256(&data.bytes, &mut canceled)?;
        let candidate = RetainedImage { item_index, request_id, data, digest, format };
        let response = self.image_response("document-image-prepare", generation, item_index, request_id,
            Some(&candidate), 0, MAX_DOCUMENT_IMAGE_CHUNK, &mut canceled)?;
        self.image_snapshot(generation)?.validate_active()?; check(&mut canceled)?;
        let assets = self.document_images.as_mut().ok_or(DocumentImageError::NotAuthorized)?;
        assets.bytes += candidate.data.bytes.len();
        assets.images.push(candidate);
        Ok(response)
    }

    /// Exact immutable chunks; no path lookup, source reread, asset reread or
    /// decoding occurs. Stale documents and mismatched item/request pairs fail
    /// before any bytes can cross the native boundary.
    pub fn document_image_chunk(&mut self, generation: u64, item_index: usize, request_id: u64,
        offset: usize, count: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        self.image_response("document-image-chunk", generation, item_index, request_id,
            None, offset, count, &mut canceled)
    }

    fn image_snapshot(&self, generation: u64) -> Result<&DocumentImages, ReaderSessionError> {
        let display = self.display_snapshot(generation)?;
        let assets = self.document_images.as_ref().ok_or(DocumentImageError::NotAuthorized)?;
        if assets.generation != generation || assets.width_points != display.width_points()
            || assets.source != *self.capture.request() || assets.source_digest != self.capture_sha256 {
            return Err(DocumentImageError::OriginMismatch.into());
        }
        assets.grant.validate_active()?;
        Ok(assets)
    }

    fn image_response(&mut self, command: &str, generation: u64, item_index: usize, request_id: u64,
        candidate: Option<&RetainedImage>, offset: usize, count: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        check(canceled)?;
        if !(1..=MAX_DOCUMENT_IMAGE_CHUNK).contains(&count) {
            return Err(DocumentImageError::InvalidLimits.into());
        }
        self.display_snapshot(generation)?.image_request(item_index, request_id)?;
        self.image_snapshot(generation)?;
        let mut out = self.output(command)?;
        let request = self.display_snapshot(generation)?.image_request(item_index, request_id)?;
        let assets = self.image_snapshot(generation)?;
        let image = candidate.or_else(|| assets.image(item_index, request_id))
            .ok_or(DocumentImageError::MissingImage)?;
        if image.item_index != item_index || image.request_id != request_id || offset > image.data.bytes.len() {
            return Err(DocumentImageError::InvalidRange.into());
        }
        let end = offset.saturating_add(count).min(image.data.bytes.len());
        image_summary(&mut out, assets,
            assets.bytes + candidate.map_or(0, |image| image.data.bytes.len()),
            assets.images.len() + usize::from(candidate.is_some()))?;
        out.literal(",\"item_index\":")?; out.integer(item_index as u64)?;
        out.literal(",\"request_id\":")?; out.integer(request_id)?;
        out.literal(",\"destination\":")?; out.quoted(request.destination)?;
        out.literal(",\"enclosing_original_range\":")?; out.range(request.enclosing_original_range)?;
        out.literal(",\"image_sha256\":")?; out.quoted(&image.digest.to_hex())?;
        out.literal(",\"image_format\":")?;
        out.quoted(match image.format { ImageFormat::Png => "png", ImageFormat::Jpeg => "jpeg",
            _ => return Err(DocumentImageError::UnsupportedFormat.into()) })?;
        out.literal(",\"format_validation\":\"signature-only\",\"decoded\":false,\"image_observation\":\"current-root-capture\"")?;
        out.literal(",\"total_image_bytes\":")?; out.integer(image.data.bytes.len() as u64)?;
        out.literal(",\"image_offset\":")?; out.integer(offset as u64)?;
        out.literal(",\"image_bytes\":")?; out.integer((end - offset) as u64)?;
        out.literal(",\"image_hex\":")?; out.hex(&image.data.bytes[offset..end])?;
        out.literal(",\"next_image_offset\":")?;
        if end < image.data.bytes.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
        out.literal(",\"image_bytes_read\":")?;
        out.integer(candidate.map_or(0, |image| image.data.bytes.len() as u64))?;
        out.literal(",\"image_read_calls\":")?;
        out.integer(candidate.map_or(0, |image| image.data.read_calls))?;
        out.literal("}\n")?;
        self.finish_output(out, EXIT_OK, canceled)
    }
}

fn image_summary(out: &mut Output, assets: &DocumentImages, retained_bytes: usize, retained_images: usize)
    -> Result<(), ReaderSessionError> {
    out.literal(",\"document_generation\":")?; out.integer(assets.generation)?;
    out.literal(",\"display_width_points\":")?; out.integer(assets.width_points as u64)?;
    out.literal(",\"capture_sha256\":")?; out.quoted(&assets.source_digest.to_hex())?;
    out.literal(",\"requested_root\":")?; out.path(&assets.requested_root)?;
    out.literal(",\"asset_root\":")?; out.path(&assets.root)?;
    out.literal(",\"source_relative\":")?; out.path(&assets.source_relative.raw().to_path_buf())?;
    for (key, value) in [("max_image_bytes", MAX_DOCUMENT_IMAGE_BYTES),
        ("max_image_cache_bytes", MAX_DOCUMENT_IMAGE_CACHE_BYTES), ("max_images", MAX_DOCUMENT_IMAGES),
        ("max_image_chunk_bytes", MAX_DOCUMENT_IMAGE_CHUNK), ("retained_image_bytes", retained_bytes),
        ("retained_images", retained_images)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value as u64)?;
    }
    Ok(())
}
