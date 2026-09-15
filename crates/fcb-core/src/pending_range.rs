#![forbid(unsafe_code)]

use crate::{
    focus::AcceptedLayoutSnapshot, utf16_to_scalar_boundary, ArenaOwnerId, BidiBoundary,
    ByteOffset, ByteRange, CoreError, LayoutRevision, ScalarRange, SemanticNodeId,
    Utf16CodeUnitOffset, Utf16CodeUnitRange, NATIVE_NOT_FOUND,
};

/// A request token issued by a native text/accessibility adapter when requesting
/// a slice of source text for virtualized presentation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct RangeRequestToken {
    owner: ArenaOwnerId,
    request_id: u64,
    node_id: SemanticNodeId,
    layout_revision: LayoutRevision,
}

impl RangeRequestToken {
    pub fn new(
        owner: ArenaOwnerId,
        request_id: u64,
        node_id: SemanticNodeId,
        layout_revision: LayoutRevision,
    ) -> Result<Self, CoreError> {
        if node_id.owner() != owner || layout_revision.owner() != owner {
            return Err(CoreError::OwnershipMismatch);
        }
        if request_id == 0 {
            return Err(CoreError::InvalidId);
        }
        Ok(Self {
            owner,
            request_id,
            node_id,
            layout_revision,
        })
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn request_id(self) -> u64 {
        self.request_id
    }

    pub const fn node_id(self) -> SemanticNodeId {
        self.node_id
    }

    pub const fn layout_revision(self) -> LayoutRevision {
        self.layout_revision
    }
}

/// The fully resolved, verified text range corresponding to an accessibility or text query.
#[derive(Clone, Debug, PartialEq)]
pub struct ResolvedTextRange {
    range: Utf16CodeUnitRange,
    byte_range: ByteRange,
    scalar_range: ScalarRange,
    text: String,
    bidi_boundaries: Vec<BidiBoundary>,
}

impl ResolvedTextRange {
    pub fn new(
        range: Utf16CodeUnitRange,
        byte_range: ByteRange,
        scalar_range: ScalarRange,
        text: String,
        bidi_boundaries: Vec<BidiBoundary>,
    ) -> Self {
        Self {
            range,
            byte_range,
            scalar_range,
            text,
            bidi_boundaries,
        }
    }

    pub const fn range(&self) -> Utf16CodeUnitRange {
        self.range
    }

    pub const fn byte_range(&self) -> ByteRange {
        self.byte_range
    }

    pub const fn scalar_range(&self) -> ScalarRange {
        self.scalar_range
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn bidi_boundaries(&self) -> &[BidiBoundary] {
        &self.bidi_boundaries
    }
}

/// The explicit pending/ready/refused state of a virtualized range request.
/// Native adapters must handle `Pending` asynchronously instead of synchronously
/// shaping giant paragraphs inside AppKit / NSAccessibility callbacks.
#[derive(Clone, Debug, PartialEq)]
pub enum PendingRangeStatus {
    /// Context preparation is scheduled; caller must defer rendering/accessibility return.
    Pending {
        token: RangeRequestToken,
        requested_range: Utf16CodeUnitRange,
    },
    /// Text range has been verified, sliced at scalar boundaries, and resolved.
    Ready(ResolvedTextRange),
    /// The request generation, source revision, or layout revision is stale.
    Stale(CoreError),
    /// The requested range was refused (e.g., native sentinel, surrogate split, out of bounds).
    Refused(CoreError),
}

/// Resolver for native adapter range queries, providing checked UTF-16 surrogate
/// validation, sentinel checks, and explicit pending state.
pub struct PendingTextRangeResolver;

impl PendingTextRangeResolver {
    /// Validates a raw UTF-16 code unit offset against the native sentinel and bounds.
    pub fn validate_offset(offset: Utf16CodeUnitOffset, max_code_units: u64) -> Result<(), CoreError> {
        if offset.get() == NATIVE_NOT_FOUND {
            return Err(CoreError::NativeSentinel);
        }
        if offset.get() > max_code_units {
            return Err(CoreError::LimitExceeded);
        }
        Ok(())
    }

    /// Verifies that an offset does not land inside a UTF-16 surrogate pair in `text`.
    /// Slicing within a surrogate pair is rejected with `InvalidUtf16`.
    pub fn check_surrogate_boundary(
        text: &str,
        offset: Utf16CodeUnitOffset,
    ) -> Result<(), CoreError> {
        if offset.get() == NATIVE_NOT_FOUND {
            return Err(CoreError::NativeSentinel);
        }
        let target = offset.get();
        let mut code_units = 0_u64;

        for character in text.chars() {
            if code_units == target {
                return Ok(());
            }
            let char_units = u64::from(character.len_utf16() as u16);
            let next_units = code_units
                .checked_add(char_units)
                .ok_or(CoreError::ArithmeticOverflow)?;

            if target > code_units && target < next_units {
                // Lands strictly inside this multi-unit character (surrogate pair)
                return Err(CoreError::InvalidUtf16);
            }
            code_units = next_units;
        }

        if code_units == target {
            Ok(())
        } else {
            Err(CoreError::LimitExceeded)
        }
    }

    /// Evaluates a range request for a semantic node against an accepted layout snapshot.
    /// If full text is not provided (None), produces `PendingRangeStatus::Pending`.
    /// If provided, validates surrogate boundaries and converts to scalar and byte ranges.
    pub fn resolve_range(
        token: RangeRequestToken,
        layout: &AcceptedLayoutSnapshot,
        full_text: Option<&str>,
        requested_range: Utf16CodeUnitRange,
        bidi_boundaries: &[BidiBoundary],
    ) -> PendingRangeStatus {
        // Validate token against layout snapshot
        if token.owner() != layout.identity().owner() {
            return PendingRangeStatus::Stale(CoreError::OwnershipMismatch);
        }
        if token.layout_revision() != layout.identity().layout_revision() {
            return PendingRangeStatus::Stale(CoreError::StaleLayoutRevision);
        }

        // Validate node exists
        if layout.node(token.node_id()).is_none() {
            return PendingRangeStatus::Stale(CoreError::NodeNotFound);
        }

        // Check for native sentinel in start or end
        if requested_range.start().get() == NATIVE_NOT_FOUND
            || requested_range.end().get() == NATIVE_NOT_FOUND
        {
            return PendingRangeStatus::Refused(CoreError::NativeSentinel);
        }

        // If text is not loaded or giant line is being prepared, return explicit Pending state
        let Some(text) = full_text else {
            return PendingRangeStatus::Pending {
                token,
                requested_range,
            };
        };

        // Check surrogate boundaries
        if let Err(err) = Self::check_surrogate_boundary(text, requested_range.start()) {
            return PendingRangeStatus::Refused(err);
        }
        if let Err(err) = Self::check_surrogate_boundary(text, requested_range.end()) {
            return PendingRangeStatus::Refused(err);
        }

        // Map UTF-16 range to scalar indices
        let start_scalar = match utf16_to_scalar_boundary(text, requested_range.start()) {
            Ok(s) => s,
            Err(err) => return PendingRangeStatus::Refused(err),
        };
        let end_scalar = match utf16_to_scalar_boundary(text, requested_range.end()) {
            Ok(s) => s,
            Err(err) => return PendingRangeStatus::Refused(err),
        };

        let scalar_range = match ScalarRange::new(start_scalar, end_scalar) {
            Ok(r) => r,
            Err(err) => return PendingRangeStatus::Refused(err),
        };

        // Map scalar range to byte range
        let mut byte_start = None;
        let mut byte_end = None;
        let mut scalar_idx = 0_u64;

        for (byte_offset, _) in text.char_indices() {
            let b = match u64::try_from(byte_offset) {
                Ok(v) => ByteOffset::new(v),
                Err(_) => return PendingRangeStatus::Refused(CoreError::ArithmeticOverflow),
            };
            if scalar_idx == start_scalar.get() {
                byte_start = Some(b);
            }
            if scalar_idx == end_scalar.get() {
                byte_end = Some(b);
            }
            scalar_idx = scalar_idx.saturating_add(1);
        }
        if scalar_idx == start_scalar.get() {
            byte_start = Some(ByteOffset::new(text.len() as u64));
        }
        if scalar_idx == end_scalar.get() {
            byte_end = Some(ByteOffset::new(text.len() as u64));
        }

        let (Some(b_start), Some(b_end)) = (byte_start, byte_end) else {
            return PendingRangeStatus::Refused(CoreError::LimitExceeded);
        };

        let byte_range = match ByteRange::new(b_start, b_end) {
            Ok(r) => r,
            Err(err) => return PendingRangeStatus::Refused(err),
        };

        // Extract slice safely using validated byte range
        let start_usize = b_start.get() as usize;
        let end_usize = b_end.get() as usize;
        let slice = &text[start_usize..end_usize];

        // Filter relevant bidi boundaries within the requested UTF-16 range
        let relevant_bidi: Vec<BidiBoundary> = bidi_boundaries
            .iter()
            .copied()
            .filter(|b| {
                b.logical() >= requested_range.start() && b.logical() <= requested_range.end()
            })
            .collect();

        PendingRangeStatus::Ready(ResolvedTextRange::new(
            requested_range,
            byte_range,
            scalar_range,
            slice.to_string(),
            relevant_bidi,
        ))
    }

    /// Oracle helper: Verifies that visual glyph positions and logical boundaries
    /// are not assumed to be 1:1. Multiple logical boundaries can map to the same visual
    /// position (ligatures, bidi run reordering, combined accents).
    pub fn has_non_one_to_one_bidi_mapping(boundaries: &[BidiBoundary]) -> bool {
        let mut seen_visuals = std::collections::BTreeSet::new();
        for b in boundaries {
            if !seen_visuals.insert(b.visual().get()) {
                // Visual position was seen more than once with different logical offsets / affinities
                return true;
            }
        }
        false
    }
}
