#![forbid(unsafe_code)]

use fcb_core::{DocumentGeneration, FileId, SourceRevision};
use fcb_source::ObservationDigest;
use franken_markdown::{FlowDisplayError, FlowError, ProvenanceError, SourceMapError};

/// Failure conditions for document integration, flow consumption, and display translation.
#[derive(Debug, PartialEq, Eq)]
pub enum DocumentError {
    /// Document request has an outdated or mismatched generation.
    StaleRequest {
        expected: DocumentGeneration,
        actual: DocumentGeneration,
    },
    /// Document request has a mismatched source revision.
    StaleRevision {
        expected: SourceRevision,
        actual: SourceRevision,
    },
    /// Document request has a mismatched observation digest.
    MismatchedDigest {
        expected: ObservationDigest,
        actual: ObservationDigest,
    },
    /// Document request has a mismatched file identity.
    MismatchedFile {
        expected: FileId,
        actual: FileId,
    },
    /// Source capture bytes are not valid UTF-8.
    InvalidUtf8,
    /// Requested range is out of bounds or invalid.
    InvalidRange,
    /// Upstream flow layout error.
    Flow(FlowError),
    /// Upstream resumable flow display error.
    FlowDisplay(FlowDisplayError),
    /// Provenance oracle verification failure.
    Provenance(ProvenanceError),
    /// Source map operation error.
    SourceMap(SourceMapError),
    /// Operation was canceled.
    Canceled,
    /// Resource limit exceeded.
    LimitExceeded,
    /// Document is empty.
    EmptyDocument,
    /// Owner mismatch between request and session.
    OwnerMismatch,
    /// Asset reference attempts path traversal or unconfined escape.
    AssetEscape {
        uri: String,
        reason: String,
    },
    /// Asset request or memory budget exceeded.
    AssetBudgetExceeded {
        reason: String,
    },
    /// Target asset request identifier was not found or is unknown.
    UnknownAssetRequest {
        request_id: u64,
    },
    /// Asset result was provided for an outdated or mismatched generation.
    StaleAssetGeneration {
        expected: DocumentGeneration,
        actual: DocumentGeneration,
    },
    /// Asset resolution failed.
    AssetResolutionFailed {
        request_id: u64,
        reason: String,
    },
    /// Image dimensions or decoded memory exceeds decompression bomb thresholds.
    DecompressionBomb {
        width: u32,
        height: u32,
        reason: String,
    },
    /// Asset payload is corrupt, truncated, or fails magic header validation.
    CorruptAssetPayload {
        request_id: u64,
        reason: String,
    },
    /// Transclusion cycle detected in document inclusion tree.
    TransclusionCycle {
        path: String,
        chain: Vec<String>,
    },
    /// Transclusion nesting depth exceeds maximum permitted limit.
    TransclusionDepthExceeded {
        max_depth: usize,
    },
    /// Host capability or policy denied access to the requested asset.
    AssetDenied {
        request_id: u64,
        reason: String,
    },
    /// Remote network fetch attempted while network is disabled by default.
    NetworkDisabled {
        uri: String,
    },
}

impl DocumentError {
    pub const fn code(&self) -> &'static str {
        match self {
            Self::StaleRequest { .. } => "DOCUMENT_STALE_REQUEST",
            Self::StaleRevision { .. } => "DOCUMENT_STALE_REVISION",
            Self::MismatchedDigest { .. } => "DOCUMENT_MISMATCHED_DIGEST",
            Self::MismatchedFile { .. } => "DOCUMENT_MISMATCHED_FILE",
            Self::InvalidUtf8 => "DOCUMENT_INVALID_UTF8",
            Self::InvalidRange => "DOCUMENT_INVALID_RANGE",
            Self::Flow(_) => "DOCUMENT_FLOW_ERROR",
            Self::FlowDisplay(_) => "DOCUMENT_FLOW_DISPLAY_ERROR",
            Self::Provenance(_) => "DOCUMENT_PROVENANCE_ERROR",
            Self::SourceMap(_) => "DOCUMENT_SOURCE_MAP_ERROR",
            Self::Canceled => "DOCUMENT_CANCELED",
            Self::LimitExceeded => "DOCUMENT_LIMIT_EXCEEDED",
            Self::EmptyDocument => "DOCUMENT_EMPTY",
            Self::OwnerMismatch => "DOCUMENT_OWNER_MISMATCH",
            Self::AssetEscape { .. } => "DOCUMENT_ASSET_ESCAPE",
            Self::AssetBudgetExceeded { .. } => "DOCUMENT_ASSET_BUDGET_EXCEEDED",
            Self::UnknownAssetRequest { .. } => "DOCUMENT_UNKNOWN_ASSET_REQUEST",
            Self::StaleAssetGeneration { .. } => "DOCUMENT_STALE_ASSET_GENERATION",
            Self::AssetResolutionFailed { .. } => "DOCUMENT_ASSET_RESOLUTION_FAILED",
            Self::DecompressionBomb { .. } => "DOCUMENT_DECOMPRESSION_BOMB",
            Self::CorruptAssetPayload { .. } => "DOCUMENT_CORRUPT_ASSET_PAYLOAD",
            Self::TransclusionCycle { .. } => "DOCUMENT_TRANSCLUSION_CYCLE",
            Self::TransclusionDepthExceeded { .. } => "DOCUMENT_TRANSCLUSION_DEPTH_EXCEEDED",
            Self::AssetDenied { .. } => "DOCUMENT_ASSET_DENIED",
            Self::NetworkDisabled { .. } => "DOCUMENT_NETWORK_DISABLED",
        }
    }
}

impl std::fmt::Display for DocumentError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::StaleRequest { expected, actual } => {
                write!(
                    f,
                    "{}: expected generation {}, got {}",
                    self.code(),
                    expected.get(),
                    actual.get()
                )
            }
            Self::StaleRevision { expected, actual } => {
                write!(
                    f,
                    "{}: expected revision {}, got {}",
                    self.code(),
                    expected.get(),
                    actual.get()
                )
            }
            Self::MismatchedDigest { expected, actual } => {
                write!(
                    f,
                    "{}: expected digest {:?}, got {:?}",
                    self.code(),
                    expected,
                    actual
                )
            }
            Self::MismatchedFile { expected, actual } => {
                write!(
                    f,
                    "{}: expected file {}, got {}",
                    self.code(),
                    expected.get(),
                    actual.get()
                )
            }
            Self::InvalidUtf8 => write!(f, "{}: capture bytes are not valid UTF-8", self.code()),
            Self::InvalidRange => write!(f, "{}: range is invalid or out of bounds", self.code()),
            Self::Flow(err) => write!(f, "{}: upstream flow layout failed: {:?}", self.code(), err),
            Self::FlowDisplay(err) => {
                write!(f, "{}: upstream flow display failed: {:?}", self.code(), err)
            }
            Self::Provenance(err) => {
                write!(f, "{}: provenance verification failed: {:?}", self.code(), err)
            }
            Self::SourceMap(err) => {
                write!(f, "{}: source map operation failed: {:?}", self.code(), err)
            }
            Self::Canceled => write!(f, "{}: document operation was canceled", self.code()),
            Self::LimitExceeded => write!(f, "{}: document resource limits exceeded", self.code()),
            Self::EmptyDocument => write!(f, "{}: document source is empty", self.code()),
            Self::OwnerMismatch => write!(f, "{}: owner ID mismatch", self.code()),
            Self::AssetEscape { uri, reason } => {
                write!(f, "{}: asset escape rejected for '{}': {}", self.code(), uri, reason)
            }
            Self::AssetBudgetExceeded { reason } => {
                write!(f, "{}: asset budget exceeded: {}", self.code(), reason)
            }
            Self::UnknownAssetRequest { request_id } => {
                write!(f, "{}: unknown asset request ID {}", self.code(), request_id)
            }
            Self::StaleAssetGeneration { expected, actual } => {
                write!(
                    f,
                    "{}: stale asset generation: expected {}, got {}",
                    self.code(),
                    expected.get(),
                    actual.get()
                )
            }
            Self::AssetResolutionFailed { request_id, reason } => {
                write!(f, "{}: asset resolution failed for request {}: {}", self.code(), request_id, reason)
            }
            Self::DecompressionBomb { width, height, reason } => {
                write!(f, "{}: decompression bomb rejected for {}x{}: {}", self.code(), width, height, reason)
            }
            Self::CorruptAssetPayload { request_id, reason } => {
                write!(f, "{}: corrupt asset payload for request {}: {}", self.code(), request_id, reason)
            }
            Self::TransclusionCycle { path, chain } => {
                write!(f, "{}: transclusion cycle detected for '{}', chain: {:?}", self.code(), path, chain)
            }
            Self::TransclusionDepthExceeded { max_depth } => {
                write!(f, "{}: transclusion nesting depth exceeds limit {}", self.code(), max_depth)
            }
            Self::AssetDenied { request_id, reason } => {
                write!(f, "{}: asset request {} denied: {}", self.code(), request_id, reason)
            }
            Self::NetworkDisabled { uri } => {
                write!(f, "{}: network disabled by default, refused fetch for '{}'", self.code(), uri)
            }
        }
    }
}

impl std::error::Error for DocumentError {}

impl From<FlowError> for DocumentError {
    fn from(err: FlowError) -> Self {
        Self::Flow(err)
    }
}

impl From<FlowDisplayError> for DocumentError {
    fn from(err: FlowDisplayError) -> Self {
        Self::FlowDisplay(err)
    }
}

impl From<ProvenanceError> for DocumentError {
    fn from(err: ProvenanceError) -> Self {
        Self::Provenance(err)
    }
}

impl From<SourceMapError> for DocumentError {
    fn from(err: SourceMapError) -> Self {
        Self::SourceMap(err)
    }
}
