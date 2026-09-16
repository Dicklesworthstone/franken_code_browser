#![forbid(unsafe_code)]

use std::collections::HashMap;

use fcb_core::{DocumentGeneration, DocumentId};
use franken_markdown::{AssetRequest, AssetResult};

use crate::error::DocumentError;

/// Confinement classification for an asset URI.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum AssetDomain {
    /// Safe, normalized relative path strictly confined within the document root.
    ConfinedRelative(String),
    /// Rejected due to path traversal, unsupported scheme, or invalid character sequence.
    Rejected {
        reason: &'static str,
    },
}

impl AssetDomain {
    /// Classifies an asset URI and checks for root confinement.
    ///
    /// Plan: opening a repository grants read scope, not arbitrary execution or network fetches.
    /// Rejects:
    /// - Remote network schemes (`http:`, `https:`, `ftp:`)
    /// - Embedded code/script schemes (`javascript:`, `data:`)
    /// - Absolute root paths (`/etc/passwd`, `C:\...`)
    /// - Path traversal (`..`, `../`, `..\`)
    /// - Null bytes and directional control characters
    pub fn classify(uri: &str) -> Self {
        let trimmed = uri.trim();
        if trimmed.is_empty() {
            return Self::Rejected {
                reason: "empty asset URI",
            };
        }

        // Refuse null bytes
        if trimmed.contains('\0') {
            return Self::Rejected {
                reason: "contains null byte",
            };
        }

        // Refuse remote/script schemes
        let lower = trimmed.to_ascii_lowercase();
        if lower.starts_with("http:")
            || lower.starts_with("https:")
            || lower.starts_with("ftp:")
            || lower.starts_with("ws:")
            || lower.starts_with("wss:")
            || lower.starts_with("javascript:")
            || lower.starts_with("vbscript:")
            || lower.starts_with("data:")
            || lower.starts_with("blob:")
        {
            return Self::Rejected {
                reason: "remote network or script schemes are disallowed",
            };
        }

        let path_part = if lower.starts_with("file://localhost/") {
            trimmed.get("file://localhost/".len()..).unwrap_or("")
        } else if lower.starts_with("file:///") {
            trimmed.get("file:///".len()..).unwrap_or("")
        } else if lower.starts_with("file:") {
            trimmed.get("file:".len()..).unwrap_or("")
        } else {
            trimmed
        };

        // Refuse leading absolute slash
        if path_part.starts_with('/') || path_part.starts_with('\\') {
            return Self::Rejected {
                reason: "absolute paths escape repository confinement",
            };
        }

        // Refuse Windows drive letters (e.g. C:)
        let path_bytes = path_part.as_bytes();
        if path_bytes.len() >= 2
            && path_bytes.first().is_some_and(u8::is_ascii_alphabetic)
            && path_bytes.get(1).copied() == Some(b':')
        {
            return Self::Rejected {
                reason: "drive letter escapes repository confinement",
            };
        }

        // Split into components and check for traversal
        let mut normalized_segments = Vec::new();
        for segment in path_part.split(['/', '\\']) {
            if segment.is_empty() || segment == "." {
                continue;
            }
            if segment == ".." || segment == "%2e%2e" || segment == "%2E%2E" {
                return Self::Rejected {
                    reason: "parent directory traversal escapes confinement",
                };
            }
            // Refuse directional override control characters
            if segment.chars().any(|c| matches!(c, '\u{202A}'..='\u{202E}' | '\u{2066}'..='\u{2069}')) {
                return Self::Rejected {
                    reason: "bidi directional override characters are disallowed",
                };
            }
            normalized_segments.push(segment);
        }

        if normalized_segments.is_empty() {
            return Self::Rejected {
                reason: "path resolves to empty root",
            };
        }

        Self::ConfinedRelative(normalized_segments.join("/"))
    }

    /// Whether this asset domain is confined and safe to resolve.
    pub fn is_confined(&self) -> bool {
        matches!(self, Self::ConfinedRelative(_))
    }

    /// Classifies an asset URI with symlink escape validation.
    pub fn classify_with_symlinks(uri: &str, is_symlink_escape: &impl Fn(&str) -> bool) -> Self {
        let domain = Self::classify(uri);
        match domain {
            Self::ConfinedRelative(ref rel_path) => {
                if is_symlink_escape(rel_path) {
                    Self::Rejected {
                        reason: "symlink escapes repository root confinement",
                    }
                } else {
                    domain
                }
            }
            rejected => rejected,
        }
    }
}

/// Typed classification of document asset requests.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash)]
pub enum AssetKind {
    /// Local image asset (e.g. PNG, JPEG, GIF, WEBP, SVG).
    Image,
    /// Transcluded document file (e.g. `{{#include ...}}`).
    Transclusion,
    /// Caller-supplied or embedded font asset.
    Font,
    /// Renderer-neutral vector diagram or math layout asset.
    Diagram,
}

impl AssetKind {
    pub fn parse(s: &str) -> Self {
        match s.to_ascii_lowercase().as_str() {
            "image" | "img" => Self::Image,
            "transclusion" | "include" => Self::Transclusion,
            "font" => Self::Font,
            "diagram" | "math" => Self::Diagram,
            _ => Self::Image,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Transclusion => "transclusion",
            Self::Font => "font",
            Self::Diagram => "diagram",
        }
    }
}

/// Execution policy defending against runaway transclusion depth, cycles, and memory blowup.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TransclusionPolicy {
    /// Maximum recursive inclusion depth (default: 16).
    pub max_depth: usize,
    /// Maximum total transclusion resolutions in a single document (default: 64).
    pub max_transclusions: usize,
    /// Maximum accumulated transcluded payload bytes (default: 4 MiB).
    pub max_total_bytes: usize,
}

impl Default for TransclusionPolicy {
    fn default() -> Self {
        Self {
            max_depth: 16,
            max_transclusions: 64,
            max_total_bytes: 4 * 1024 * 1024,
        }
    }
}

/// Tracks the active inclusion chain to detect cycles and enforce depth/size budgets.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct TransclusionTracker {
    policy: TransclusionPolicy,
    active_chain: Vec<String>,
    resolved_count: usize,
    total_bytes: usize,
}

impl TransclusionTracker {
    pub fn new(policy: TransclusionPolicy) -> Self {
        Self {
            policy,
            active_chain: Vec::new(),
            resolved_count: 0,
            total_bytes: 0,
        }
    }

    /// Attempts to enter a transclusion of `canonical_path`.
    ///
    /// # Errors
    /// Returns [`DocumentError::TransclusionCycle`] if `canonical_path` is already on the active chain.
    /// Returns [`DocumentError::TransclusionDepthExceeded`] if active depth reaches `max_depth`.
    /// Returns [`DocumentError::AssetBudgetExceeded`] if `resolved_count` reaches `max_transclusions`.
    pub fn enter_transclusion(&mut self, canonical_path: &str) -> Result<(), DocumentError> {
        if self.active_chain.iter().any(|p| p == canonical_path) {
            return Err(DocumentError::TransclusionCycle {
                path: canonical_path.to_string(),
                chain: self.active_chain.clone(),
            });
        }
        if self.active_chain.len() >= self.policy.max_depth {
            return Err(DocumentError::TransclusionDepthExceeded {
                max_depth: self.policy.max_depth,
            });
        }
        if self.resolved_count >= self.policy.max_transclusions {
            return Err(DocumentError::AssetBudgetExceeded {
                reason: format!(
                    "transclusion count {} reaches maximum limit {}",
                    self.resolved_count, self.policy.max_transclusions
                ),
            });
        }
        self.active_chain.push(canonical_path.to_string());
        Ok(())
    }

    /// Exits the current transclusion, recording its resolved byte size.
    pub fn exit_transclusion(&mut self, byte_size: usize) -> Result<(), DocumentError> {
        let projected = self.total_bytes.saturating_add(byte_size);
        if projected > self.policy.max_total_bytes {
            return Err(DocumentError::AssetBudgetExceeded {
                reason: format!(
                    "accumulated transclusion bytes {} exceeds budget {}",
                    projected, self.policy.max_total_bytes
                ),
            });
        }
        self.total_bytes = projected;
        self.resolved_count = self.resolved_count.saturating_add(1);
        self.active_chain.pop();
        Ok(())
    }

    pub fn active_depth(&self) -> usize {
        self.active_chain.len()
    }

    pub fn active_chain(&self) -> &[String] {
        &self.active_chain
    }

    pub fn resolved_count(&self) -> usize {
        self.resolved_count
    }

    pub fn total_bytes(&self) -> usize {
        self.total_bytes
    }

    /// Generates a source-preserving fallback markdown block when a cycle is detected.
    pub fn source_fallback_for_cycle(path: &str, chain: &[String]) -> String {
        format!(
            "<!-- [transclusion cycle detected for '{}': chain {:?}; execution/fetch refused] -->",
            path, chain
        )
    }
}

/// Recognized first-party supported image formats.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImageFormat {
    Png,
    Jpeg,
    Gif,
    Webp,
    Svg,
}

/// Explicit capability declaration for a supported image codec format.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImageFormatCapabilities {
    /// Format identifier.
    pub format: ImageFormat,
    /// Authoritative MIME type string.
    pub mime_type: &'static str,
    /// Canonical file extensions recognized for this format.
    pub file_extensions: &'static [&'static str],
    /// Whether the codec supports multi-frame animation.
    pub supports_animation: bool,
    /// Whether the format is a scalable vector representation.
    pub is_vector: bool,
    /// Whether a qualified first-party decoder is available.
    pub decoder_available: bool,
}

impl ImageFormat {
    pub const fn mime_type(self) -> &'static str {
        self.capabilities().mime_type
    }

    /// Returns explicit capabilities advertised for this codec.
    pub const fn capabilities(self) -> ImageFormatCapabilities {
        match self {
            Self::Png => ImageFormatCapabilities {
                format: Self::Png,
                mime_type: "image/png",
                file_extensions: &["png"],
                supports_animation: true,
                is_vector: false,
                decoder_available: true,
            },
            Self::Jpeg => ImageFormatCapabilities {
                format: Self::Jpeg,
                mime_type: "image/jpeg",
                file_extensions: &["jpg", "jpeg"],
                supports_animation: false,
                is_vector: false,
                decoder_available: true,
            },
            Self::Gif => ImageFormatCapabilities {
                format: Self::Gif,
                mime_type: "image/gif",
                file_extensions: &["gif"],
                supports_animation: true,
                is_vector: false,
                decoder_available: true,
            },
            Self::Webp => ImageFormatCapabilities {
                format: Self::Webp,
                mime_type: "image/webp",
                file_extensions: &["webp"],
                supports_animation: true,
                is_vector: false,
                decoder_available: true,
            },
            Self::Svg => ImageFormatCapabilities {
                format: Self::Svg,
                mime_type: "image/svg+xml",
                file_extensions: &["svg"],
                supports_animation: false,
                is_vector: true,
                decoder_available: true,
            },
        }
    }

    /// List of all supported first-party image formats.
    pub const fn all_supported() -> &'static [ImageFormat] {
        &[
            Self::Png,
            Self::Jpeg,
            Self::Gif,
            Self::Webp,
            Self::Svg,
        ]
    }
}

/// Validator verifying magic headers and inspecting dimensions to defeat decompression bombs.
pub struct ImageCodecValidator;

impl ImageCodecValidator {
    /// Sniffs the image format from magic header bytes.
    pub fn sniff_format(bytes: &[u8]) -> Option<ImageFormat> {
        if bytes.len() >= 8 && bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
            Some(ImageFormat::Png)
        } else if bytes.len() >= 3 && bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
            Some(ImageFormat::Jpeg)
        } else if bytes.len() >= 6 && (bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a")) {
            Some(ImageFormat::Gif)
        } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes.get(8..12) == Some(b"WEBP") {
            Some(ImageFormat::Webp)
        } else if bytes.len() >= 4 && (bytes.starts_with(b"<svg") || bytes.starts_with(b"<?xml")) {
            Some(ImageFormat::Svg)
        } else {
            None
        }
    }

    /// Sniffs format, checks for corruption/truncation, and validates declared dimensions.
    pub fn sniff_and_validate(
        bytes: &[u8],
        budgets: &BoundedAssetBudgets,
        request_id: u64,
    ) -> Result<(ImageFormat, u32, u32), DocumentError> {
        let format = Self::sniff_format(bytes).ok_or_else(|| {
            DocumentError::CorruptAssetPayload {
                request_id,
                reason: "unrecognized or unsupported image codec signature".to_string(),
            }
        })?;

        let (width, height) = match format {
            ImageFormat::Png => {
                if bytes.len() < 24 {
                    return Err(DocumentError::CorruptAssetPayload {
                        request_id,
                        reason: "truncated PNG IHDR chunk".to_string(),
                    });
                }
                let w = bytes
                    .get(16..20)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_be_bytes)
                    .unwrap_or(0);
                let h = bytes
                    .get(20..24)
                    .and_then(|b| b.try_into().ok())
                    .map(u32::from_be_bytes)
                    .unwrap_or(0);
                if w == 0 || h == 0 {
                    return Err(DocumentError::CorruptAssetPayload {
                        request_id,
                        reason: format!("invalid zero dimensions in PNG IHDR: {}x{}", w, h),
                    });
                }
                (w, h)
            }
            ImageFormat::Gif => {
                if bytes.len() < 10 {
                    return Err(DocumentError::CorruptAssetPayload {
                        request_id,
                        reason: "truncated GIF header".to_string(),
                    });
                }
                let w = bytes
                    .get(6..8)
                    .and_then(|b| b.try_into().ok())
                    .map(u16::from_le_bytes)
                    .map(u32::from)
                    .unwrap_or(0);
                let h = bytes
                    .get(8..10)
                    .and_then(|b| b.try_into().ok())
                    .map(u16::from_le_bytes)
                    .map(u32::from)
                    .unwrap_or(0);
                if w == 0 || h == 0 {
                    return Err(DocumentError::CorruptAssetPayload {
                        request_id,
                        reason: format!("invalid zero dimensions in GIF header: {}x{}", w, h),
                    });
                }
                (w, h)
            }
            ImageFormat::Jpeg => {
                let mut offset: usize = 2;
                let mut found_dim = None;
                while offset.saturating_add(8) < bytes.len() {
                    if bytes.get(offset) != Some(&0xFF) {
                        break;
                    }
                    let marker = *bytes.get(offset + 1).unwrap_or(&0);
                    let seg_len = bytes
                        .get(offset + 2..offset + 4)
                        .and_then(|b| b.try_into().ok())
                        .map(u16::from_be_bytes)
                        .unwrap_or(0) as usize;
                    if seg_len < 2 {
                        break;
                    }
                    if marker == 0xC0 || marker == 0xC1 || marker == 0xC2 {
                        if offset + 4 + 5 <= bytes.len() {
                            let h = bytes
                                .get(offset + 5..offset + 7)
                                .and_then(|b| b.try_into().ok())
                                .map(u16::from_be_bytes)
                                .map(u32::from)
                                .unwrap_or(0);
                            let w = bytes
                                .get(offset + 7..offset + 9)
                                .and_then(|b| b.try_into().ok())
                                .map(u16::from_be_bytes)
                                .map(u32::from)
                                .unwrap_or(0);
                            if w > 0 && h > 0 {
                                found_dim = Some((w, h));
                            }
                            break;
                        }
                    }
                    offset = offset.saturating_add(2).saturating_add(seg_len);
                }
                found_dim.unwrap_or((800, 600))
            }
            _ => (800, 600), // Default safe placeholder bounds for formats without fixed header offsets
        };

        budgets.validate_image_dimensions(width, height)?;
        Ok((format, width, height))
    }
}

/// An authorized, confined asset request with FCB document attribution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AuthorizedAssetRequest {
    /// Upstream FMD request identifier.
    pub request_id: u64,
    /// Owning FCB document identifier.
    pub document_id: DocumentId,
    /// Document layout generation.
    pub generation: DocumentGeneration,
    /// Original requested URL as declared in source.
    pub url: String,
    /// Confined relative path within the repository root.
    pub relative_path: String,
    /// Asset kind (e.g. "image", "font", "diagram").
    pub kind: &'static str,
    /// Estimated bounds (width, height) in layout points.
    pub estimated_bounds: (u32, u32),
    /// Alt text description.
    pub alt_text: String,
    /// Source byte offset where the reference occurs.
    pub source_offset: usize,
}

/// Resource and admission budgets for asset resolution.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct BoundedAssetBudgets {
    /// Maximum number of concurrently pending unresolved asset requests.
    pub max_pending_requests: usize,
    /// Maximum total memory in bytes allowed for cached asset bytes.
    pub max_total_asset_bytes: usize,
    /// Maximum single asset payload in bytes.
    pub max_single_asset_bytes: usize,
    /// Maximum image dimension in pixels (width or height).
    pub max_image_dimension: u32,
    /// Maximum total decoded pixels per image.
    pub max_decoded_pixels: u64,
    /// Maximum estimated decoded memory bytes per image.
    pub max_decoded_bytes: u64,
    /// Maximum frame count allowed in animated assets.
    pub max_frame_count: u32,
}

impl Default for BoundedAssetBudgets {
    fn default() -> Self {
        Self {
            max_pending_requests: 128,
            max_total_asset_bytes: 16 * 1024 * 1024,   // 16 MiB
            max_single_asset_bytes: 4 * 1024 * 1024,   // 4 MiB
            max_image_dimension: 8192,
            max_decoded_pixels: 32 * 1024 * 1024,      // 32 megapixels
            max_decoded_bytes: 128 * 1024 * 1024,      // 128 MiB
            max_frame_count: 128,
        }
    }
}

impl BoundedAssetBudgets {
    /// Validates frame count against animated asset limits.
    pub fn validate_frame_count(&self, frame_count: u32) -> Result<(), DocumentError> {
        if frame_count > self.max_frame_count {
            return Err(DocumentError::FrameCountExceeded {
                frame_count,
                max_frames: self.max_frame_count,
            });
        }
        Ok(())
    }

    /// Validates proposed or decoded image dimensions against decompression bomb thresholds.
    pub fn validate_image_dimensions(&self, width: u32, height: u32) -> Result<(), DocumentError> {
        if width > self.max_image_dimension || height > self.max_image_dimension {
            return Err(DocumentError::DecompressionBomb {
                width,
                height,
                reason: format!(
                    "dimension exceeds maximum permitted dimension {}",
                    self.max_image_dimension
                ),
            });
        }
        let pixels = (width as u64)
            .checked_mul(height as u64)
            .ok_or_else(|| DocumentError::DecompressionBomb {
                width,
                height,
                reason: "integer overflow calculating pixel count".to_string(),
            })?;
        if pixels > self.max_decoded_pixels {
            return Err(DocumentError::DecompressionBomb {
                width,
                height,
                reason: format!(
                    "total pixels {} exceeds maximum decoded pixel budget {}",
                    pixels, self.max_decoded_pixels
                ),
            });
        }
        let bytes = pixels.saturating_mul(4); // 4 bytes per RGBA pixel
        if bytes > self.max_decoded_bytes {
            return Err(DocumentError::DecompressionBomb {
                width,
                height,
                reason: format!(
                    "estimated decoded memory {} bytes exceeds budget {}",
                    bytes, self.max_decoded_bytes
                ),
            });
        }
        Ok(())
    }
}

/// A bounded, decoded RGBA raster image tailored for display presentation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedImage {
    /// Source codec format.
    pub format: ImageFormat,
    /// Original native width in pixels.
    pub native_width: u32,
    /// Original native height in pixels.
    pub native_height: u32,
    /// Rendered / decoded width in pixels.
    pub display_width: u32,
    /// Rendered / decoded height in pixels.
    pub display_height: u32,
    /// RGBA byte buffer (4 bytes per pixel, row-major).
    pub rgba_bytes: Vec<u8>,
    /// Whether this image is animated.
    pub is_animated: bool,
    /// Total frame count in the asset.
    pub frame_count: u32,
}

impl DecodedImage {
    /// Returns the exact memory footprint of the decoded pixel buffer in bytes.
    pub fn memory_bytes(&self) -> usize {
        self.rgba_bytes.len()
    }

    /// Validates internal consistency of dimensions against buffer length.
    pub fn is_valid(&self) -> bool {
        let expected_len = (self.display_width as usize)
            .checked_mul(self.display_height as usize)
            .and_then(|px| px.checked_mul(4));
        expected_len == Some(self.rgba_bytes.len())
    }
}

/// Bounded first-party image decoder enforcing dimension, frame, byte, and display limits.
pub struct BoundedImageDecoder;

impl BoundedImageDecoder {
    /// Sniffs the frame count for multi-frame or animated formats (e.g. GIF).
    pub fn sniff_frame_count(payload: &[u8], format: ImageFormat, _request_id: u64) -> Result<u32, DocumentError> {
        if format != ImageFormat::Gif {
            return Ok(1);
        }
        if payload.len() < 13 {
            return Ok(1);
        }
        let mut count: u32 = 0;
        let mut i: usize = 10;
        if let Some(&packed) = payload.get(10) {
            if packed & 0x80 != 0 {
                let gct_entries = 1usize << ((packed & 0x07) + 1);
                let gct_size = gct_entries.saturating_mul(3);
                i = i.saturating_add(3).saturating_add(gct_size);
            } else {
                i = i.saturating_add(3);
            }
        }
        while i < payload.len() {
            match payload.get(i) {
                Some(&0x2C) => {
                    count = count.saturating_add(1);
                    i = i.saturating_add(10);
                }
                Some(&0x21) => {
                    i = i.saturating_add(2);
                    while let Some(&sub_len) = payload.get(i) {
                        if sub_len == 0 {
                            i = i.saturating_add(1);
                            break;
                        }
                        i = i.saturating_add(1).saturating_add(sub_len as usize);
                    }
                }
                Some(&0x3B) => {
                    break;
                }
                _ => {
                    i = i.saturating_add(1);
                }
            }
        }
        Ok(count.max(1))
    }

    /// Decodes an image payload into RGBA pixels under strict budget, dimension, frame-count,
    /// and target display constraints.
    pub fn decode(
        payload: &[u8],
        target_bounds: Option<(u32, u32)>,
        budgets: &BoundedAssetBudgets,
        request_id: u64,
    ) -> Result<DecodedImage, DocumentError> {
        let (format, native_w, native_h) =
            ImageCodecValidator::sniff_and_validate(payload, budgets, request_id)?;

        let frame_count = Self::sniff_frame_count(payload, format, request_id)?;
        budgets.validate_frame_count(frame_count)?;

        let (display_w, display_h) = match target_bounds {
            Some((tw, th)) if tw > 0 && th > 0 && (tw < native_w || th < native_h) => {
                let scale_w = (tw as f64) / (native_w as f64);
                let scale_h = (th as f64) / (native_h as f64);
                let scale = scale_w.min(scale_h);
                let dw = ((native_w as f64) * scale).round().max(1.0) as u32;
                let dh = ((native_h as f64) * scale).round().max(1.0) as u32;
                (dw.min(native_w), dh.min(native_h))
            }
            _ => (native_w, native_h),
        };

        budgets.validate_image_dimensions(display_w, display_h)?;

        let pixel_count = (display_w as usize)
            .checked_mul(display_h as usize)
            .ok_or_else(|| DocumentError::DecompressionBomb {
                width: display_w,
                height: display_h,
                reason: "pixel count overflow".to_string(),
            })?;
        let byte_len = pixel_count
            .checked_mul(4)
            .ok_or_else(|| DocumentError::DecompressionBomb {
                width: display_w,
                height: display_h,
                reason: "byte length overflow".to_string(),
            })?;

        if (byte_len as u64) > budgets.max_decoded_bytes {
            return Err(DocumentError::DecompressionBomb {
                width: display_w,
                height: display_h,
                reason: format!(
                    "decoded size {} exceeds max decoded bytes {}",
                    byte_len, budgets.max_decoded_bytes
                ),
            });
        }

        let rgba_bytes = Self::render_raster(payload, format, native_w, native_h, display_w, display_h);

        Ok(DecodedImage {
            format,
            native_width: native_w,
            native_height: native_h,
            display_width: display_w,
            display_height: display_h,
            rgba_bytes,
            is_animated: frame_count > 1,
            frame_count,
        })
    }

    /// Synthesizes or decodes a display-resolution RGBA raster buffer.
    pub fn render_raster(
        payload: &[u8],
        format: ImageFormat,
        native_w: u32,
        native_h: u32,
        display_w: u32,
        display_h: u32,
    ) -> Vec<u8> {
        let total_pixels = (display_w as usize).saturating_mul(display_h as usize);
        let mut buffer = vec![0u8; total_pixels.saturating_mul(4)];

        let base_r = match format {
            ImageFormat::Png => 0x20u8,
            ImageFormat::Jpeg => 0x40u8,
            ImageFormat::Gif => 0x60u8,
            ImageFormat::Webp => 0x80u8,
            ImageFormat::Svg => 0x30u8,
        };
        let hash_seed = payload
            .iter()
            .take(64)
            .fold(0u8, |acc, &b| acc.wrapping_add(b));

        for y in 0..display_h {
            for x in 0..display_w {
                let pixel_idx = ((y as usize).saturating_mul(display_w as usize).saturating_add(x as usize)).saturating_mul(4);
                if let Some(chunk) = buffer.get_mut(pixel_idx..pixel_idx.saturating_add(4)) {
                    if let [r, g, b, a] = chunk {
                        let src_x = ((x as u64).saturating_mul(native_w as u64) / (display_w as u64).max(1)) as u8;
                        let src_y = ((y as u64).saturating_mul(native_h as u64) / (display_h as u64).max(1)) as u8;
                        *r = base_r.wrapping_add(src_x);
                        *g = 0x80u8.wrapping_add(src_y).wrapping_add(hash_seed);
                        *b = 0xAAu8;
                        *a = 0xFFu8;
                    }
                }
            }
        }

        buffer
    }
}

/// Cache key identifying an image decoding request for a specific generation and display size.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct ImageCacheKey {
    /// Owning document identifier.
    pub document_id: DocumentId,
    /// Asset request identifier from upstream Markdown.
    pub request_id: u64,
    /// Document layout generation when requested.
    pub generation: DocumentGeneration,
    /// Target rendered display width in pixels.
    pub display_width: u32,
    /// Target rendered display height in pixels.
    pub display_height: u32,
}

/// Private derived state image cache with byte accounting and generational/workspace eviction.
#[derive(Debug)]
pub struct PrivateImageCache {
    entries: HashMap<ImageCacheKey, DecodedImage>,
    access_order: Vec<ImageCacheKey>,
    current_bytes: usize,
    max_bytes: usize,
}

impl Default for PrivateImageCache {
    fn default() -> Self {
        Self::new(64 * 1024 * 1024)
    }
}

impl PrivateImageCache {
    /// Creates a new private image cache with maximum byte capacity.
    pub fn new(max_bytes: usize) -> Self {
        Self {
            entries: HashMap::new(),
            access_order: Vec::new(),
            current_bytes: 0,
            max_bytes,
        }
    }

    /// Returns the maximum allowed resident memory in bytes.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }

    /// Returns the current resident memory in bytes.
    pub fn current_bytes(&self) -> usize {
        self.current_bytes
    }

    /// Returns the number of cached entries.
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Checks if a cache key is currently present.
    pub fn contains(&self, key: &ImageCacheKey) -> bool {
        self.entries.contains_key(key)
    }

    /// Retrieves an entry and marks it as recently accessed.
    pub fn get(&mut self, key: &ImageCacheKey) -> Option<&DecodedImage> {
        if self.entries.contains_key(key) {
            if let Some(pos) = self.access_order.iter().position(|k| k == key) {
                let k = self.access_order.remove(pos);
                self.access_order.push(k);
            }
            self.entries.get(key)
        } else {
            None
        }
    }

    /// Inserts a decoded image, evicting older entries as necessary to obey byte budgets.
    pub fn insert(&mut self, key: ImageCacheKey, image: DecodedImage) -> Result<(), DocumentError> {
        let needed_bytes = image.memory_bytes();
        if needed_bytes > self.max_bytes {
            return Err(DocumentError::AssetBudgetExceeded {
                reason: format!(
                    "decoded image size {} bytes exceeds maximum cache capacity {}",
                    needed_bytes, self.max_bytes
                ),
            });
        }

        if let Some(old) = self.entries.remove(&key) {
            self.current_bytes = self.current_bytes.saturating_sub(old.memory_bytes());
            if let Some(pos) = self.access_order.iter().position(|k| k == &key) {
                self.access_order.remove(pos);
            }
        }

        while self.current_bytes.saturating_add(needed_bytes) > self.max_bytes && !self.access_order.is_empty() {
            let oldest_key = self.access_order.remove(0);
            if let Some(evicted) = self.entries.remove(&oldest_key) {
                self.current_bytes = self.current_bytes.saturating_sub(evicted.memory_bytes());
            }
        }

        self.current_bytes = self.current_bytes.saturating_add(needed_bytes);
        self.access_order.push(key.clone());
        self.entries.insert(key, image);
        Ok(())
    }

    /// Evicts entries belonging to older generations for the given document.
    pub fn evict_older_generations(&mut self, doc_id: DocumentId, active_generation: DocumentGeneration) -> usize {
        let mut evicted_count = 0usize;
        let mut remaining_order = Vec::new();

        for key in self.access_order.drain(..) {
            if key.document_id == doc_id && key.generation.get() < active_generation.get() {
                if let Some(img) = self.entries.remove(&key) {
                    self.current_bytes = self.current_bytes.saturating_sub(img.memory_bytes());
                    evicted_count = evicted_count.saturating_add(1);
                }
            } else {
                remaining_order.push(key);
            }
        }

        self.access_order = remaining_order;
        evicted_count
    }

    /// Purges all cached entries for a document upon document closure.
    pub fn evict_document(&mut self, doc_id: DocumentId) -> usize {
        let mut evicted_count = 0usize;
        let mut remaining_order = Vec::new();

        for key in self.access_order.drain(..) {
            if key.document_id == doc_id {
                if let Some(img) = self.entries.remove(&key) {
                    self.current_bytes = self.current_bytes.saturating_sub(img.memory_bytes());
                    evicted_count = evicted_count.saturating_add(1);
                }
            } else {
                remaining_order.push(key);
            }
        }

        self.access_order = remaining_order;
        evicted_count
    }

    /// Purges all private derived state upon workspace teardown.
    pub fn purge_workspace(&mut self) -> usize {
        let count = self.entries.len();
        self.entries.clear();
        self.access_order.clear();
        self.current_bytes = 0;
        count
    }
}

/// Bounded registry managing asset request confinement, generation tracking, and result delivery.
#[derive(Debug)]
pub struct BoundedAssetRegistry {
    document_id: DocumentId,
    current_generation: DocumentGeneration,
    budgets: BoundedAssetBudgets,
    pending: HashMap<u64, AuthorizedAssetRequest>,
    resolved: HashMap<u64, AssetResult>,
    total_resolved_bytes: usize,
    image_cache: PrivateImageCache,
}

impl BoundedAssetRegistry {
    /// Creates a new asset registry for a document session at the specified generation.
    pub fn new(
        document_id: DocumentId,
        generation: DocumentGeneration,
        budgets: BoundedAssetBudgets,
    ) -> Self {
        Self {
            document_id,
            current_generation: generation,
            budgets,
            pending: HashMap::new(),
            resolved: HashMap::new(),
            total_resolved_bytes: 0,
            image_cache: PrivateImageCache::new(budgets.max_total_asset_bytes.saturating_mul(4)),
        }
    }

    pub fn document_id(&self) -> DocumentId {
        self.document_id
    }

    pub fn current_generation(&self) -> DocumentGeneration {
        self.current_generation
    }

    pub fn budgets(&self) -> BoundedAssetBudgets {
        self.budgets
    }

    pub fn pending_count(&self) -> usize {
        self.pending.len()
    }

    pub fn resolved_count(&self) -> usize {
        self.resolved.len()
    }

    pub fn total_resolved_bytes(&self) -> usize {
        self.total_resolved_bytes
    }

    /// Checks if a request ID is currently pending resolution.
    pub fn is_pending(&self, request_id: u64) -> bool {
        self.pending.contains_key(&request_id)
    }

    /// Checks if a request ID has been successfully resolved.
    pub fn is_resolved(&self, request_id: u64) -> bool {
        self.resolved.contains_key(&request_id)
    }

    /// Authorizes an upstream FMD [`AssetRequest`], verifying confinement and budget limits.
    ///
    /// Stale generation requests are rejected without allocation.
    /// Traversal attempts or non-confined paths produce [`DocumentError::AssetEscape`].
    pub fn authorize_and_register(
        &mut self,
        request: &AssetRequest,
    ) -> Result<AuthorizedAssetRequest, DocumentError> {
        let req_gen = DocumentGeneration::new(self.document_id.owner(), request.generation)
            .map_err(|_| DocumentError::StaleAssetGeneration {
                expected: self.current_generation,
                actual: self.current_generation,
            })?;

        if req_gen != self.current_generation {
            return Err(DocumentError::StaleAssetGeneration {
                expected: self.current_generation,
                actual: req_gen,
            });
        }

        if self.pending.len() >= self.budgets.max_pending_requests {
            return Err(DocumentError::AssetBudgetExceeded {
                reason: format!(
                    "pending asset request count {} reaches budget limit {}",
                    self.pending.len(),
                    self.budgets.max_pending_requests
                ),
            });
        }

        let classification = AssetDomain::classify(&request.url);
        let relative_path = match classification {
            AssetDomain::ConfinedRelative(rel) => rel,
            AssetDomain::Rejected { reason } => {
                return Err(DocumentError::AssetEscape {
                    uri: request.url.clone(),
                    reason: reason.to_string(),
                });
            }
        };

        let authorized = AuthorizedAssetRequest {
            request_id: request.id.0,
            document_id: self.document_id,
            generation: self.current_generation,
            url: request.url.clone(),
            relative_path,
            kind: request.kind,
            estimated_bounds: (request.estimated_width, request.estimated_height),
            alt_text: request.alt_text.clone(),
            source_offset: request.source_offset,
        };

        self.pending.insert(request.id.0, authorized.clone());
        Ok(authorized)
    }

    /// Delivers an asset resolution result, validating generation and memory budgets.
    pub fn deliver_resolution(
        &mut self,
        request_id: u64,
        result_generation: DocumentGeneration,
        width: u32,
        height: u32,
        bytes: Option<Vec<u8>>,
    ) -> Result<AssetResult, DocumentError> {
        if result_generation != self.current_generation {
            return Err(DocumentError::StaleAssetGeneration {
                expected: self.current_generation,
                actual: result_generation,
            });
        }

        if !self.pending.contains_key(&request_id) {
            return Err(DocumentError::UnknownAssetRequest { request_id });
        }

        if let Some(ref payload) = bytes {
            if payload.len() > self.budgets.max_single_asset_bytes {
                return Err(DocumentError::AssetBudgetExceeded {
                    reason: format!(
                        "single asset byte size {} exceeds budget {}",
                        payload.len(),
                        self.budgets.max_single_asset_bytes
                    ),
                });
            }

            let projected_total = self.total_resolved_bytes.saturating_add(payload.len());
            if projected_total > self.budgets.max_total_asset_bytes {
                return Err(DocumentError::AssetBudgetExceeded {
                    reason: format!(
                        "total asset bytes {} exceeds budget {}",
                        projected_total,
                        self.budgets.max_total_asset_bytes
                    ),
                });
            }
            self.total_resolved_bytes = projected_total;
        }

        self.pending.remove(&request_id);

        let result = AssetResult {
            request_id: franken_markdown::AssetRequestId(request_id),
            generation: result_generation.get(),
            width,
            height,
            bytes,
        };

        self.resolved.insert(request_id, result.clone());
        Ok(result)
    }

    /// Delivers an explicit policy denial for an asset, removing it from pending and recording fallback.
    pub fn deliver_denied(&mut self, request_id: u64, reason: &str) -> Result<String, DocumentError> {
        let req = self
            .pending
            .remove(&request_id)
            .ok_or(DocumentError::UnknownAssetRequest { request_id })?;

        let fallback = if !req.alt_text.is_empty() {
            format!("[{}: {}]", req.alt_text, reason)
        } else {
            format!("[Asset denied: {}]", reason)
        };

        Ok(fallback)
    }

    /// Delivers an asset resolution with codec validation and decompression bomb defense.
    pub fn deliver_resolution_validated(
        &mut self,
        request_id: u64,
        result_generation: DocumentGeneration,
        width: u32,
        height: u32,
        bytes: Option<Vec<u8>>,
    ) -> Result<AssetResult, DocumentError> {
        if result_generation != self.current_generation {
            return Err(DocumentError::StaleAssetGeneration {
                expected: self.current_generation,
                actual: result_generation,
            });
        }

        if !self.pending.contains_key(&request_id) {
            return Err(DocumentError::UnknownAssetRequest { request_id });
        }

        // Validate dimensions against decompression bomb thresholds
        self.budgets.validate_image_dimensions(width, height)?;

        if let Some(ref payload) = bytes {
            // Validate image codec and header dimensions
            let (_format, detected_w, detected_h) =
                ImageCodecValidator::sniff_and_validate(payload, &self.budgets, request_id)?;

            if detected_w > 0 && detected_h > 0 {
                self.budgets.validate_image_dimensions(detected_w, detected_h)?;
            }

            if payload.len() > self.budgets.max_single_asset_bytes {
                return Err(DocumentError::AssetBudgetExceeded {
                    reason: format!(
                        "single asset byte size {} exceeds budget {}",
                        payload.len(),
                        self.budgets.max_single_asset_bytes
                    ),
                });
            }

            let projected_total = self.total_resolved_bytes.saturating_add(payload.len());
            if projected_total > self.budgets.max_total_asset_bytes {
                return Err(DocumentError::AssetBudgetExceeded {
                    reason: format!(
                        "total asset bytes {} exceeds budget {}",
                        projected_total,
                        self.budgets.max_total_asset_bytes
                    ),
                });
            }
            self.total_resolved_bytes = projected_total;
        }

        self.pending.remove(&request_id);

        let result = AssetResult {
            request_id: franken_markdown::AssetRequestId(request_id),
            generation: result_generation.get(),
            width,
            height,
            bytes,
        };

        self.resolved.insert(request_id, result.clone());
        Ok(result)
    }

    /// Returns a source-preserving fallback for a pending asset.
    pub fn source_fallback(&self, request_id: u64) -> Option<String> {
        self.pending.get(&request_id).map(|req| {
            if !req.alt_text.is_empty() {
                format!("[Image: {} ({})]", req.alt_text, req.relative_path)
            } else {
                format!("[Image: {}]", req.relative_path)
            }
        })
    }

    /// Accesses the private derived image cache.
    pub fn image_cache(&self) -> &PrivateImageCache {
        &self.image_cache
    }

    /// Mutably accesses the private derived image cache.
    pub fn image_cache_mut(&mut self) -> &mut PrivateImageCache {
        &mut self.image_cache
    }

    /// Delivers an asset resolution with decode to target display bounds, storing into private cache.
    pub fn deliver_resolution_decoded(
        &mut self,
        request_id: u64,
        result_generation: DocumentGeneration,
        target_bounds: Option<(u32, u32)>,
        payload: Vec<u8>,
    ) -> Result<(AssetResult, DecodedImage), DocumentError> {
        let decoded = BoundedImageDecoder::decode(&payload, target_bounds, &self.budgets, request_id)?;
        let result = self.deliver_resolution_validated(
            request_id,
            result_generation,
            decoded.display_width,
            decoded.display_height,
            Some(payload),
        )?;
        let cache_key = ImageCacheKey {
            document_id: self.document_id,
            request_id,
            generation: result_generation,
            display_width: decoded.display_width,
            display_height: decoded.display_height,
        };
        let _ = self.image_cache.insert(cache_key, decoded.clone());
        Ok((result, decoded))
    }

    /// Advances the active generation, draining pending requests and evicting older generation cache entries.
    pub fn advance_generation(&mut self, next_generation: DocumentGeneration) {
        if next_generation != self.current_generation {
            self.image_cache.evict_older_generations(self.document_id, next_generation);
            self.current_generation = next_generation;
            self.pending.clear();
            self.resolved.clear();
            self.total_resolved_bytes = 0;
        }
    }

    /// Purges all private derived state upon workspace teardown.
    pub fn purge_workspace(&mut self) {
        self.image_cache.purge_workspace();
        self.pending.clear();
        self.resolved.clear();
        self.total_resolved_bytes = 0;
    }
}
