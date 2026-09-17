//! Old-capture anchor resolution against immutable captures (FCB-082.A).
//!
//! When a source file is captured, text anchors (byte offsets) are recorded.
//! When the file changes, those anchors become stale unless either:
//! 1. The original capture is retained (retained backing), or
//! 2. The current bytes at the anchor offsets are byte-verified equal.
//!
//! Otherwise, the anchor is stale and the caller must explicitly choose to
//! open the current version. No false visual-exact claims are made.
//!
//! §10.7, §10.9: Unicode's bidi and grapheme algorithms operate on their defined context,
//! not arbitrary byte tiles. For pathological lines whose required context exceeds the active
//! work budget (e.g. 500 MB line, bidi paragraph, long combining sequences), state `ContextPending`
//! or offer an explicitly labeled logical/escaped view. Never silently substitute live bytes!

#![forbid(unsafe_code)]

/// An anchor recorded against a specific capture revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OldAnchor {
    /// Byte offset within the capture.
    pub offset: usize,
    /// The capture revision this anchor was recorded against.
    pub revision: u64,
}

impl OldAnchor {
    pub const fn new(offset: usize, revision: u64) -> Self {
        Self { offset, revision }
    }
}

/// The resolution of an old anchor against current state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OldAnchorResolution {
    /// The original capture is retained; the anchor is valid as-is.
    Retained,
    /// The original capture was evicted, but the current bytes at this
    /// anchor's offset are byte-verified equal to the recorded digest.
    ByteVerified,
    /// The anchor is stale: neither retained backing nor byte-verified
    /// equality. The caller must deliberately open the current version.
    Stale,
}

impl OldAnchorResolution {
    /// Stable machine-readable code.
    pub const fn code(self) -> &'static str {
        match self {
            Self::Retained => "RETAINED",
            Self::ByteVerified => "BYTE_VERIFIED",
            Self::Stale => "STALE",
        }
    }

    /// Whether the anchor is usable without user intervention.
    pub const fn is_usable(self) -> bool {
        !matches!(self, Self::Stale)
    }
}

/// Whether a capture is retained in the snapshot store.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CaptureBacking {
    /// The capture is pinned/retained in the store.
    Retained,
    /// The capture was evicted; only the digest remains.
    Evicted,
}

/// Visual context readiness for an anchor or line.
///
/// §10.9: Unicode's bidi and grapheme algorithms operate on their defined context,
/// not arbitrary byte tiles. A run cannot be labeled exact because it merely looks
/// plausible. For pathological lines whose required context exceeds active work budget,
/// state `ContextPending` or offer `LogicalEscaped` view.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum VisualContextReadiness {
    /// Context is fully prepared and visual runs are exact.
    ExactVisual,
    /// Required context exceeds work budget or is currently preparing;
    /// visual layout is pending. Exact byte access remains available.
    ContextPending,
    /// Explicit logical / escaped view fallback (e.g. for giant lines or unshaped bidi).
    LogicalEscaped,
}

impl VisualContextReadiness {
    pub const fn code(self) -> &'static str {
        match self {
            Self::ExactVisual => "EXACT_VISUAL",
            Self::ContextPending => "CONTEXT_PENDING",
            Self::LogicalEscaped => "LOGICAL_ESCAPED",
        }
    }

    pub const fn is_exact(self) -> bool {
        matches!(self, Self::ExactVisual)
    }
}

/// Line characteristics that impact visual shaping context.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LineContextProperties {
    /// Total byte length of the enclosing line / paragraph.
    pub line_byte_length: u64,
    /// Whether the line contains bidirectional text runs.
    pub has_bidi: bool,
    /// Number of combining characters in the longest sequence.
    pub max_combining_sequence: usize,
    /// Work budget in bytes or iterations for context preparation.
    pub context_work_budget: u64,
}

impl LineContextProperties {
    pub const DEFAULT_MAX_CONTEXT_BUDGET: u64 = 64 * 1024; // 64 KiB
    pub const MAX_SAFE_COMBINING_SEQUENCE: usize = 32;
    pub const HUGE_LINE_THRESHOLD_BYTES: u64 = 500 * 1024 * 1024; // 500 MB

    pub const fn new(
        line_byte_length: u64,
        has_bidi: bool,
        max_combining_sequence: usize,
        context_work_budget: u64,
    ) -> Self {
        Self {
            line_byte_length,
            has_bidi,
            max_combining_sequence,
            context_work_budget,
        }
    }

    /// Assess visual context readiness without making false visual-exact claims.
    pub fn assess_readiness(&self) -> VisualContextReadiness {
        // Long combining sequences exceeding safe limits fall back to logical/escaped view
        if self.max_combining_sequence > Self::MAX_SAFE_COMBINING_SEQUENCE {
            return VisualContextReadiness::LogicalEscaped;
        }

        // Pathological line exceeding work budget (e.g. 500MB line or unbounded bidi)
        if self.line_byte_length > self.context_work_budget {
            return VisualContextReadiness::ContextPending;
        }

        VisualContextReadiness::ExactVisual
    }
}

/// Resolve an old anchor against current state.
///
/// # Arguments
/// - `anchor`: the old anchor (offset + revision)
/// - `backing`: whether the original capture is still retained
/// - `current_bytes`: the current content at the anchor's offset
/// - `recorded_digest`: the digest recorded when the anchor was created
///
/// # Returns
/// - `Retained` if the capture backing is retained (anchor is valid)
/// - `ByteVerified` if the current bytes hash to the recorded digest
/// - `Stale` otherwise
pub fn resolve_old_anchor(
    _anchor: &OldAnchor,
    backing: CaptureBacking,
    current_bytes: &[u8],
    recorded_digest: u64,
) -> OldAnchorResolution {
    match backing {
        CaptureBacking::Retained => OldAnchorResolution::Retained,
        CaptureBacking::Evicted => {
            let current_digest = fnv1a(current_bytes);
            if current_digest == recorded_digest {
                OldAnchorResolution::ByteVerified
            } else {
                OldAnchorResolution::Stale
            }
        }
    }
}

/// Compute the FNV-1a digest used for byte verification.
pub fn fnv1a(bytes: &[u8]) -> u64 {
    let mut hash: u64 = 0xCBF2_9CE4_8422_2325;
    for byte in bytes {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01B3);
    }
    hash
}

/// The deliberate action a caller takes when an anchor is stale.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OpenCurrentAction {
    /// Open the current version of the file at the same line/column.
    OpenCurrent,
    /// Open the retained capture in a read-only historical view.
    OpenHistorical,
    /// Do nothing; the anchor is unavailable.
    Dismiss,
}

impl OpenCurrentAction {
    pub const fn code(self) -> &'static str {
        match self {
            Self::OpenCurrent => "OPEN_CURRENT",
            Self::OpenHistorical => "OPEN_HISTORICAL",
            Self::Dismiss => "DISMISS",
        }
    }
}

/// The full resolution result including the deliberate action and visual context readiness.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnchorResolutionResult {
    pub resolution: OldAnchorResolution,
    pub action: OpenCurrentAction,
    pub visual_readiness: VisualContextReadiness,
}

impl AnchorResolutionResult {
    /// The default action and visual readiness for a given resolution.
    pub const fn with_default_action(resolution: OldAnchorResolution) -> Self {
        let (action, visual_readiness) = match resolution {
            OldAnchorResolution::Retained | OldAnchorResolution::ByteVerified => {
                (OpenCurrentAction::OpenCurrent, VisualContextReadiness::ExactVisual)
            }
            OldAnchorResolution::Stale => {
                (OpenCurrentAction::Dismiss, VisualContextReadiness::ContextPending)
            }
        };
        Self {
            resolution,
            action,
            visual_readiness,
        }
    }

    /// Explicitly set the visual context readiness.
    pub const fn with_visual_readiness(mut self, visual_readiness: VisualContextReadiness) -> Self {
        self.visual_readiness = visual_readiness;
        self
    }

    /// Construct resolution result accounting for line context properties.
    pub fn with_context(resolution: OldAnchorResolution, context: &LineContextProperties) -> Self {
        let action = match resolution {
            OldAnchorResolution::Retained | OldAnchorResolution::ByteVerified => {
                OpenCurrentAction::OpenCurrent
            }
            OldAnchorResolution::Stale => OpenCurrentAction::Dismiss,
        };
        let visual_readiness = match resolution {
            OldAnchorResolution::Stale => VisualContextReadiness::ContextPending,
            OldAnchorResolution::Retained | OldAnchorResolution::ByteVerified => {
                context.assess_readiness()
            }
        };
        Self {
            resolution,
            action,
            visual_readiness,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn retained_backing_resolves_immediately() {
        let anchor = OldAnchor { offset: 42, revision: 1 };
        let result = resolve_old_anchor(
            &anchor,
            CaptureBacking::Retained,
            b"anything",
            0,
        );
        assert_eq!(result, OldAnchorResolution::Retained);
        assert!(result.is_usable());
        assert_eq!(result.code(), "RETAINED");
    }

    #[test]
    fn evicted_backing_byte_verifies_matching_digest() {
        let anchor = OldAnchor { offset: 0, revision: 2 };
        let data = b"the actual bytes";
        let digest = fnv1a(data);
        let result = resolve_old_anchor(
            &anchor,
            CaptureBacking::Evicted,
            data,
            digest,
        );
        assert_eq!(result, OldAnchorResolution::ByteVerified);
        assert!(result.is_usable());
        assert_eq!(result.code(), "BYTE_VERIFIED");
    }

    #[test]
    fn evicted_backing_stale_when_digest_differs() {
        let anchor = OldAnchor { offset: 0, revision: 2 };
        let result = resolve_old_anchor(
            &anchor,
            CaptureBacking::Evicted,
            b"changed bytes",
            0, // a digest that won't match
        );
        assert_eq!(result, OldAnchorResolution::Stale);
        assert!(!result.is_usable());
        assert_eq!(result.code(), "STALE");
    }

    #[test]
    fn stale_resolution_defaults_to_dismiss() {
        let result = AnchorResolutionResult::with_default_action(
            OldAnchorResolution::Stale,
        );
        assert_eq!(result.action, OpenCurrentAction::Dismiss);
        assert_eq!(result.visual_readiness, VisualContextReadiness::ContextPending);
        assert_eq!(result.action.code(), "DISMISS");
    }

    #[test]
    fn retained_resolution_defaults_to_open_current() {
        let result = AnchorResolutionResult::with_default_action(
            OldAnchorResolution::Retained,
        );
        assert_eq!(result.action, OpenCurrentAction::OpenCurrent);
        assert_eq!(result.visual_readiness, VisualContextReadiness::ExactVisual);
        assert_eq!(result.action.code(), "OPEN_CURRENT");
    }

    #[test]
    fn pathological_500mb_line_marks_context_pending() {
        let ctx = LineContextProperties::new(
            500 * 1024 * 1024,
            false,
            0,
            LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
        );
        assert_eq!(ctx.assess_readiness(), VisualContextReadiness::ContextPending);

        let res = AnchorResolutionResult::with_context(OldAnchorResolution::Retained, &ctx);
        assert_eq!(res.action, OpenCurrentAction::OpenCurrent);
        assert_eq!(res.visual_readiness, VisualContextReadiness::ContextPending);
        assert!(!res.visual_readiness.is_exact());
    }

    #[test]
    fn bidi_paragraph_within_budget_is_exact() {
        let ctx = LineContextProperties::new(
            1024,
            true,
            0,
            LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
        );
        assert_eq!(ctx.assess_readiness(), VisualContextReadiness::ExactVisual);

        let res = AnchorResolutionResult::with_context(OldAnchorResolution::ByteVerified, &ctx);
        assert_eq!(res.visual_readiness, VisualContextReadiness::ExactVisual);
        assert!(res.visual_readiness.is_exact());
    }

    #[test]
    fn bidi_paragraph_exceeding_budget_marks_context_pending() {
        let ctx = LineContextProperties::new(
            128 * 1024, // 128 KiB > 64 KiB budget
            true,
            0,
            LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
        );
        assert_eq!(ctx.assess_readiness(), VisualContextReadiness::ContextPending);
    }

    #[test]
    fn long_combining_sequence_falls_back_to_logical_escaped() {
        let ctx = LineContextProperties::new(
            256,
            false,
            64, // > 32 max safe
            LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
        );
        assert_eq!(ctx.assess_readiness(), VisualContextReadiness::LogicalEscaped);

        let res = AnchorResolutionResult::with_context(OldAnchorResolution::Retained, &ctx);
        assert_eq!(res.visual_readiness, VisualContextReadiness::LogicalEscaped);
        assert_eq!(res.visual_readiness.code(), "LOGICAL_ESCAPED");
    }

    #[test]
    fn stale_resolution_never_claims_exact_visual() {
        let ctx = LineContextProperties::new(
            64,
            false,
            0,
            LineContextProperties::DEFAULT_MAX_CONTEXT_BUDGET,
        );
        let res = AnchorResolutionResult::with_context(OldAnchorResolution::Stale, &ctx);
        assert_eq!(res.action, OpenCurrentAction::Dismiss);
        assert_eq!(res.visual_readiness, VisualContextReadiness::ContextPending);
    }
}
