//! Old-capture anchor resolution against immutable captures (FCB-082.A).
//!
//! When a source file is captured, text anchors (byte offsets) are recorded.
//! When the file changes, those anchors become stale unless either:
//! 1. The original capture is retained (retained backing), or
//! 2. The current bytes at the anchor offsets are byte-verified equal.
//!
//! Otherwise, the anchor is stale and the caller must explicitly choose to
//! open the current version. No false visual-exact claims are made.

#![forbid(unsafe_code)]

/// An anchor recorded against a specific capture revision.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct OldAnchor {
    /// Byte offset within the capture.
    pub offset: usize,
    /// The capture revision this anchor was recorded against.
    pub revision: u64,
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

/// The full resolution result including the deliberate action.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AnchorResolutionResult {
    pub resolution: OldAnchorResolution,
    pub action: OpenCurrentAction,
}

impl AnchorResolutionResult {
    /// The default action for a given resolution.
    pub const fn with_default_action(resolution: OldAnchorResolution) -> Self {
        let action = match resolution {
            OldAnchorResolution::Retained | OldAnchorResolution::ByteVerified => {
                OpenCurrentAction::OpenCurrent
            }
            OldAnchorResolution::Stale => OpenCurrentAction::Dismiss,
        };
        Self {
            resolution,
            action,
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
    }

    #[test]
    fn stale_resolution_defaults_to_dismiss() {
        let result = AnchorResolutionResult::with_default_action(
            OldAnchorResolution::Stale,
        );
        assert_eq!(result.action, OpenCurrentAction::Dismiss);
    }

    #[test]
    fn retained_resolution_defaults_to_open_current() {
        let result = AnchorResolutionResult::with_default_action(
            OldAnchorResolution::Retained,
        );
        assert_eq!(result.action, OpenCurrentAction::OpenCurrent);
    }
}
