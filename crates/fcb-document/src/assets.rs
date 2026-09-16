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
            || lower.starts_with("javascript:")
            || lower.starts_with("data:")
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
}

impl Default for BoundedAssetBudgets {
    fn default() -> Self {
        Self {
            max_pending_requests: 128,
            max_total_asset_bytes: 16 * 1024 * 1024,   // 16 MiB
            max_single_asset_bytes: 4 * 1024 * 1024,   // 4 MiB
        }
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

    /// Advances the active generation, draining pending requests from older generations.
    pub fn advance_generation(&mut self, next_generation: DocumentGeneration) {
        if next_generation != self.current_generation {
            self.current_generation = next_generation;
            self.pending.clear();
            self.resolved.clear();
            self.total_resolved_bytes = 0;
        }
    }
}
