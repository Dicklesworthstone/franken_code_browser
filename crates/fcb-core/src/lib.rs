#![forbid(unsafe_code)]

use std::{
    collections::{BTreeSet, VecDeque},
    marker::PhantomData,
    sync::Arc,
};

pub mod handles;
pub mod resources;
pub mod tracing;

pub use handles::{ArenaHandle, ArenaTable, DeviceHandle, DeviceTable, HandleLimits};
pub use resources::{
    ResourceAccounting, ResourceAllocationId, ResourceBudget, ResourceKind, ResourceLease,
    ResourceLeaseInfo,
};

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArenaOwnerId(u64);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum CoreError {
    InvalidId,
    RangeReversed,
    ArithmeticOverflow,
    ArithmeticUnderflow,
    LimitExceeded,
    Exhausted,
    DuplicateId,
    OwnershipMismatch,
    NativeSentinel,
    InvalidUtf8Boundary,
    InvalidUtf16,
    InvalidBidiBoundary,
    StalePublication,
    StaleRequestGeneration,
    StaleSourceRevision,
    StaleDisplayGeneration,
}

impl CoreError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidId => "INVALID_ID",
            Self::RangeReversed => "RANGE_REVERSED",
            Self::ArithmeticOverflow => "ARITHMETIC_OVERFLOW",
            Self::ArithmeticUnderflow => "ARITHMETIC_UNDERFLOW",
            Self::LimitExceeded => "LIMIT_EXCEEDED",
            Self::Exhausted => "IDENTITY_EXHAUSTED",
            Self::DuplicateId => "DUPLICATE_ID",
            Self::OwnershipMismatch => "OWNERSHIP_MISMATCH",
            Self::NativeSentinel => "NATIVE_SENTINEL",
            Self::InvalidUtf8Boundary => "INVALID_UTF8_BOUNDARY",
            Self::InvalidUtf16 => "INVALID_UTF16",
            Self::InvalidBidiBoundary => "INVALID_BIDI_BOUNDARY",
            Self::StalePublication => "STALE_PUBLICATION",
            Self::StaleRequestGeneration => "STALE_REQUEST_GENERATION",
            Self::StaleSourceRevision => "STALE_SOURCE_REVISION",
            Self::StaleDisplayGeneration => "STALE_DISPLAY_GENERATION",
        }
    }
}

impl std::fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.code())
    }
}

impl std::error::Error for CoreError {}

impl ArenaOwnerId {
    pub const fn new(value: u64) -> Result<Self, CoreError> {
        if value == 0 {
            Err(CoreError::InvalidId)
        } else {
            Ok(Self(value))
        }
    }

    pub const fn get(self) -> u64 {
        self.0
    }
}

macro_rules! owner_qualified_id {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name {
            owner: ArenaOwnerId,
            value: u64,
        }

        impl $name {
            pub const fn new(owner: ArenaOwnerId, value: u64) -> Result<Self, CoreError> {
                if value == 0 {
                    Err(CoreError::InvalidId)
                } else {
                    Ok(Self { owner, value })
                }
            }

            pub const fn owner(self) -> ArenaOwnerId {
                self.owner
            }

            pub const fn get(self) -> u64 {
                self.value
            }

            pub const fn validate_for(self, owner: ArenaOwnerId) -> Result<(), CoreError> {
                if self.owner.get() == owner.get() {
                    Ok(())
                } else {
                    Err(CoreError::OwnershipMismatch)
                }
            }
        }
    };
}

owner_qualified_id!(BrowserInstanceId);
owner_qualified_id!(WorkspaceId);
owner_qualified_id!(RootId);
owner_qualified_id!(FileId);
owner_qualified_id!(SourceRevision);
owner_qualified_id!(CaptureExtentId);
owner_qualified_id!(AnalysisRevision);
owner_qualified_id!(LayoutRevision);
owner_qualified_id!(QueryGeneration);
owner_qualified_id!(WindowGeneration);
owner_qualified_id!(DeviceId);
owner_qualified_id!(DeviceGeneration);
owner_qualified_id!(DisplayGeneration);
owner_qualified_id!(PresentedFrameId);
owner_qualified_id!(SemanticNodeId);
owner_qualified_id!(ClockDomainId);

/// Compatibility name for integrations that refer to semantic nodes as
/// stable nodes. The owner-qualified ID is the stable identity; tree position
/// is not part of the identity.
pub type StableNodeId = SemanticNodeId;

pub trait AllocatedId: Copy {
    fn from_parts(owner: ArenaOwnerId, value: u64) -> Result<Self, CoreError>;
}

macro_rules! allocated_id {
    ($name:ident) => {
        impl AllocatedId for $name {
            fn from_parts(owner: ArenaOwnerId, value: u64) -> Result<Self, CoreError> {
                Self::new(owner, value)
            }
        }
    };
}

allocated_id!(BrowserInstanceId);
allocated_id!(WorkspaceId);
allocated_id!(RootId);
allocated_id!(SourceRevision);
allocated_id!(CaptureExtentId);
allocated_id!(AnalysisRevision);
allocated_id!(LayoutRevision);
allocated_id!(QueryGeneration);
allocated_id!(WindowGeneration);
allocated_id!(DeviceId);
allocated_id!(DeviceGeneration);
allocated_id!(DisplayGeneration);
allocated_id!(PresentedFrameId);
allocated_id!(SemanticNodeId);
allocated_id!(ClockDomainId);

allocated_id!(FileId);

/// Receiving authority for persisted owner-qualified identities.
///
/// This state is deliberately owned by the logical store or consumer that
/// receives IDs. It rejects duplicate full identities and IDs from another
/// owner without conflating equal raw counters from independent owners.
pub struct PersistedIdAuthority {
    owner: ArenaOwnerId,
    accepted: BTreeSet<FileId>,
}

impl PersistedIdAuthority {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            accepted: BTreeSet::new(),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn accept(&mut self, id: FileId) -> Result<(), CoreError> {
        if id.owner() != self.owner {
            return Err(CoreError::OwnershipMismatch);
        }
        if self.accepted.insert(id) {
            Ok(())
        } else {
            Err(CoreError::DuplicateId)
        }
    }

    pub fn contains(&self, id: FileId) -> bool {
        self.accepted.contains(&id)
    }

    pub fn len(&self) -> usize {
        self.accepted.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accepted.is_empty()
    }
}

pub struct IdAllocator<I> {
    owner: ArenaOwnerId,
    next: Option<u64>,
    marker: PhantomData<fn() -> I>,
}

impl<I: AllocatedId> IdAllocator<I> {
    pub fn new(owner: ArenaOwnerId, first: u64) -> Result<Self, CoreError> {
        if first == 0 {
            return Err(CoreError::InvalidId);
        }
        Ok(Self {
            owner,
            next: Some(first),
            marker: PhantomData,
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn allocate(&mut self) -> Result<I, CoreError> {
        let value = self.next.ok_or(CoreError::Exhausted)?;
        self.next = value.checked_add(1);
        I::from_parts(self.owner, value)
    }

}

impl IdAllocator<FileId> {
    pub fn allocate_into(
        &mut self,
        authority: &mut PersistedIdAuthority,
    ) -> Result<FileId, CoreError> {
        if authority.owner() != self.owner {
            return Err(CoreError::OwnershipMismatch);
        }
        let id = self.allocate()?;
        authority.accept(id)?;
        Ok(id)
    }
}

macro_rules! offset_domain {
    ($name:ident) => {
        #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
        pub struct $name(u64);

        impl $name {
            pub const fn new(value: u64) -> Self {
                Self(value)
            }

            pub const fn get(self) -> u64 {
                self.0
            }

            pub const fn checked_add(self, amount: u64) -> Result<Self, CoreError> {
                match self.0.checked_add(amount) {
                    Some(value) => Ok(Self(value)),
                    None => Err(CoreError::ArithmeticOverflow),
                }
            }

            pub const fn checked_sub(self, amount: u64) -> Result<Self, CoreError> {
                match self.0.checked_sub(amount) {
                    Some(value) => Ok(Self(value)),
                    None => Err(CoreError::ArithmeticUnderflow),
                }
            }
        }
    };
}

offset_domain!(ByteOffset);
offset_domain!(DecodedUtf8Offset);
offset_domain!(Utf16CodeUnitOffset);
offset_domain!(ScalarIndex);
offset_domain!(GraphemeBoundary);
offset_domain!(VisualPosition);

/// The native text-range sentinel used by APIs that encode “not found” in an
/// unsigned offset. It is never a valid source position.
pub const NATIVE_NOT_FOUND: u64 = u64::MAX;

/// Compatibility aliases retained for the original generic range vocabulary.
pub type Utf8Offset = DecodedUtf8Offset;
pub type Utf16Offset = Utf16CodeUnitOffset;
pub type ScalarOffset = ScalarIndex;
pub type GraphemeOffset = GraphemeBoundary;
pub type VisualOffset = VisualPosition;

impl Utf16CodeUnitOffset {
    pub const fn from_native(value: u64) -> Result<Self, CoreError> {
        if value == NATIVE_NOT_FOUND {
            Err(CoreError::NativeSentinel)
        } else {
            Ok(Self::new(value))
        }
    }

    pub const fn to_native(self) -> u64 {
        self.get()
    }
}

/// A caret may have two valid logical positions at one visual boundary in a
/// bidirectional run. Affinity makes that choice explicit instead of
/// pretending visual and source order are one-to-one.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum CaretAffinity {
    Upstream,
    Downstream,
}

impl CaretAffinity {
    pub const fn from_native(value: u8) -> Result<Self, CoreError> {
        match value {
            0 => Ok(Self::Upstream),
            1 => Ok(Self::Downstream),
            _ => Err(CoreError::InvalidBidiBoundary),
        }
    }

    pub const fn to_native(self) -> u8 {
        match self {
            Self::Upstream => 0,
            Self::Downstream => 1,
        }
    }
}

/// A source/visual boundary association for accessibility and hit testing.
/// Multiple logical boundaries may share a visual position; affinity retains
/// the distinction needed to move the caret without losing source order.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct BidiBoundary {
    logical: Utf16CodeUnitOffset,
    visual: VisualPosition,
    affinity: CaretAffinity,
}

impl BidiBoundary {
    pub const fn new(
        logical: Utf16CodeUnitOffset,
        visual: VisualPosition,
        affinity: CaretAffinity,
    ) -> Self {
        Self {
            logical,
            visual,
            affinity,
        }
    }

    pub const fn logical(self) -> Utf16CodeUnitOffset {
        self.logical
    }

    pub const fn visual(self) -> VisualPosition {
        self.visual
    }

    pub const fn affinity(self) -> CaretAffinity {
        self.affinity
    }
}

/// Bounded, source-free evidence for range validation outcomes.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum RangeEvidenceKind {
    NativeUtf16,
    Utf8Boundary,
    Utf16Boundary,
    BidiBoundary,
    SemanticNode,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct RangeEvidenceEvent {
    kind: RangeEvidenceKind,
    outcome: Result<(), CoreError>,
}

impl RangeEvidenceEvent {
    pub const fn new(kind: RangeEvidenceKind, outcome: Result<(), CoreError>) -> Self {
        Self { kind, outcome }
    }

    pub const fn kind(self) -> RangeEvidenceKind {
        self.kind
    }

    pub const fn outcome(self) -> Result<(), CoreError> {
        self.outcome
    }
}

pub struct RangeEvidenceRing<const CAPACITY: usize> {
    events: VecDeque<RangeEvidenceEvent>,
    accepted: u64,
    rejected: u64,
}

impl<const CAPACITY: usize> RangeEvidenceRing<CAPACITY> {
    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(CAPACITY),
            accepted: 0,
            rejected: 0,
        }
    }

    pub const fn capacity(&self) -> usize {
        CAPACITY
    }

    pub fn record(&mut self, event: RangeEvidenceEvent) {
        if event.outcome().is_ok() {
            self.accepted = self.accepted.saturating_add(1);
        } else {
            self.rejected = self.rejected.saturating_add(1);
        }
        if CAPACITY != 0 && self.events.len() == CAPACITY {
            let _ = self.events.pop_front();
        }
        if CAPACITY != 0 {
            self.events.push_back(event);
        }
    }

    pub const fn accepted(&self) -> u64 {
        self.accepted
    }

    pub const fn rejected(&self) -> u64 {
        self.rejected
    }

    pub fn events(&self) -> &VecDeque<RangeEvidenceEvent> {
        &self.events
    }
}

impl<const CAPACITY: usize> Default for RangeEvidenceRing<CAPACITY> {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteLength(u64);

impl ByteLength {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
    }

    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Limits {
    max_bytes: ByteLength,
    max_items: u64,
}

impl Limits {
    pub const fn new(max_bytes: ByteLength, max_items: u64) -> Result<Self, CoreError> {
        if max_bytes.get() == 0 || max_items == 0 {
            Err(CoreError::InvalidId)
        } else {
            Ok(Self {
                max_bytes,
                max_items,
            })
        }
    }

    pub const fn max_bytes(self) -> ByteLength {
        self.max_bytes
    }

    pub const fn max_items(self) -> u64 {
        self.max_items
    }

    pub const fn check_bytes(self, length: ByteLength) -> Result<ByteLength, CoreError> {
        if length.get() > self.max_bytes.get() {
            Err(CoreError::LimitExceeded)
        } else {
            Ok(length)
        }
    }

    pub const fn check_items(self, count: u64) -> Result<u64, CoreError> {
        if count > self.max_items {
            Err(CoreError::LimitExceeded)
        } else {
            Ok(count)
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct OffsetRange<O> {
    start: O,
    end: O,
}

impl<O: Copy + Ord> OffsetRange<O> {
    pub fn new(start: O, end: O) -> Result<Self, CoreError> {
        if start > end {
            Err(CoreError::RangeReversed)
        } else {
            Ok(Self { start, end })
        }
    }

    pub const fn start(self) -> O {
        self.start
    }

    pub const fn end(self) -> O {
        self.end
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }

    pub fn checked_within(self, limit: O) -> Result<Self, CoreError> {
        if self.end > limit {
            Err(CoreError::LimitExceeded)
        } else {
            Ok(self)
        }
    }
}

impl OffsetRange<ByteOffset> {
    pub const fn len(self) -> ByteLength {
        ByteLength::new(self.end.get() - self.start.get())
    }
}

impl OffsetRange<Utf8Offset> {
    pub const fn len(self) -> u64 {
        self.end.get() - self.start.get()
    }
}

impl OffsetRange<Utf16Offset> {
    pub const fn len(self) -> u64 {
        self.end.get() - self.start.get()
    }
}

impl OffsetRange<ScalarIndex> {
    pub const fn len(self) -> u64 {
        self.end.get() - self.start.get()
    }
}

impl OffsetRange<GraphemeBoundary> {
    pub const fn len(self) -> u64 {
        self.end.get() - self.start.get()
    }
}

impl OffsetRange<VisualPosition> {
    pub const fn len(self) -> u64 {
        self.end.get() - self.start.get()
    }
}

pub type ByteRange = OffsetRange<ByteOffset>;
pub type Utf8Range = OffsetRange<Utf8Offset>;
pub type Utf16Range = OffsetRange<Utf16Offset>;
pub type ScalarRange = OffsetRange<ScalarOffset>;
pub type GraphemeRange = OffsetRange<GraphemeOffset>;
pub type VisualRange = OffsetRange<VisualOffset>;
pub type DecodedUtf8Range = OffsetRange<DecodedUtf8Offset>;
pub type Utf16CodeUnitRange = OffsetRange<Utf16CodeUnitOffset>;
pub type ScalarIndexRange = OffsetRange<ScalarIndex>;
pub type GraphemeBoundaryRange = OffsetRange<GraphemeBoundary>;
pub type VisualPositionRange = OffsetRange<VisualPosition>;

impl OffsetRange<ByteOffset> {
    pub fn as_usize_bounds(self) -> Result<(usize, usize), CoreError> {
        let start = usize::try_from(self.start.get()).map_err(|_| CoreError::ArithmeticOverflow)?;
        let end = usize::try_from(self.end.get()).map_err(|_| CoreError::ArithmeticOverflow)?;
        Ok((start, end))
    }
}

/// Convert an offset in decoded UTF-8 text to a byte boundary without
/// allowing an interior multi-byte character to become a slice index.
pub fn decoded_utf8_to_byte_boundary(
    text: &str,
    offset: DecodedUtf8Offset,
) -> Result<ByteOffset, CoreError> {
    let length = u64::try_from(text.len()).map_err(|_| CoreError::ArithmeticOverflow)?;
    if offset.get() > length {
        return Err(CoreError::LimitExceeded);
    }
    let index = usize::try_from(offset.get()).map_err(|_| CoreError::ArithmeticOverflow)?;
    if !text.is_char_boundary(index) {
        return Err(CoreError::InvalidUtf8Boundary);
    }
    Ok(ByteOffset::new(offset.get()))
}

/// Convert a scalar boundary to a UTF-16 code-unit boundary.
pub fn scalar_to_utf16_boundary(
    text: &str,
    scalar: ScalarIndex,
) -> Result<Utf16CodeUnitOffset, CoreError> {
    let target = scalar.get();
    let mut scalar_index = 0_u64;
    let mut code_units = 0_u64;
    for character in text.chars() {
        if scalar_index == target {
            return Ok(Utf16CodeUnitOffset::new(code_units));
        }
        scalar_index = scalar_index
            .checked_add(1)
            .ok_or(CoreError::ArithmeticOverflow)?;
        code_units = code_units
            .checked_add(u64::from(character.len_utf16() as u16))
            .ok_or(CoreError::ArithmeticOverflow)?;
    }
    if scalar_index == target {
        Ok(Utf16CodeUnitOffset::new(code_units))
    } else {
        Err(CoreError::LimitExceeded)
    }
}

/// Convert a UTF-16 code-unit boundary to a scalar boundary. An offset inside
/// a surrogate pair is rejected rather than rounded to a neighboring scalar.
pub fn utf16_to_scalar_boundary(
    text: &str,
    offset: Utf16CodeUnitOffset,
) -> Result<ScalarIndex, CoreError> {
    let target = offset.get();
    let mut scalar_index = 0_u64;
    let mut code_units = 0_u64;
    for character in text.chars() {
        if code_units == target {
            return Ok(ScalarIndex::new(scalar_index));
        }
        let next_units = code_units
            .checked_add(u64::from(character.len_utf16() as u16))
            .ok_or(CoreError::ArithmeticOverflow)?;
        if target < next_units {
            return Err(CoreError::InvalidUtf16);
        }
        code_units = next_units;
        scalar_index = scalar_index
            .checked_add(1)
            .ok_or(CoreError::ArithmeticOverflow)?;
    }
    if code_units == target {
        Ok(ScalarIndex::new(scalar_index))
    } else {
        Err(CoreError::LimitExceeded)
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublicationContext {
    owner: ArenaOwnerId,
    request: QueryGeneration,
    source: SourceRevision,
    display: DisplayGeneration,
}

impl PublicationContext {
    pub fn new(
        owner: ArenaOwnerId,
        request: QueryGeneration,
        source: SourceRevision,
        display: DisplayGeneration,
    ) -> Result<Self, CoreError> {
        if request.owner() != owner || source.owner() != owner || display.owner() != owner {
            return Err(CoreError::OwnershipMismatch);
        }
        Ok(Self {
            owner,
            request,
            source,
            display,
        })
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn request(self) -> QueryGeneration {
        self.request
    }

    pub const fn source(self) -> SourceRevision {
        self.source
    }

    pub const fn display(self) -> DisplayGeneration {
        self.display
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct PublicationToken {
    context: PublicationContext,
}

impl PublicationToken {
    pub const fn new(context: PublicationContext) -> Self {
        Self { context }
    }

    pub const fn context(self) -> PublicationContext {
        self.context
    }

    pub fn validate_against(self, current: PublicationContext) -> Result<(), CoreError> {
        if self.context.owner() != current.owner() {
            return Err(CoreError::OwnershipMismatch);
        }
        if self.context.request() != current.request() {
            return Err(CoreError::StaleRequestGeneration);
        }
        if self.context.source() != current.source() {
            return Err(CoreError::StaleSourceRevision);
        }
        if self.context.display() != current.display() {
            return Err(CoreError::StaleDisplayGeneration);
        }
        Ok(())
    }
}

pub struct ImmutableSnapshot<T> {
    owner: ArenaOwnerId,
    revision: SourceRevision,
    value: Arc<T>,
}

impl<T> Clone for ImmutableSnapshot<T> {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner,
            revision: self.revision,
            value: Arc::clone(&self.value),
        }
    }
}

impl<T> ImmutableSnapshot<T> {
    pub fn new(owner: ArenaOwnerId, revision: SourceRevision, value: T) -> Result<Self, CoreError> {
        if revision.owner() != owner {
            return Err(CoreError::OwnershipMismatch);
        }
        Ok(Self {
            owner,
            revision,
            value: Arc::new(value),
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn revision(&self) -> SourceRevision {
        self.revision
    }

    pub fn get(&self) -> &T {
        self.value.as_ref()
    }

    pub fn shared(&self) -> Arc<T> {
        Arc::clone(&self.value)
    }
}

pub struct SnapshotCell<T> {
    owner: ArenaOwnerId,
    head: Option<ImmutableSnapshot<T>>,
}

impl<T> SnapshotCell<T> {
    pub const fn new(owner: ArenaOwnerId) -> Self {
        Self { owner, head: None }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn publish(&mut self, snapshot: ImmutableSnapshot<T>) -> Result<(), CoreError> {
        if snapshot.owner() != self.owner {
            return Err(CoreError::OwnershipMismatch);
        }
        if let Some(current) = self.head.as_ref() {
            if snapshot.revision().get() <= current.revision().get() {
                return Err(CoreError::StalePublication);
            }
        }
        self.head = Some(snapshot);
        Ok(())
    }

    pub fn publish_if_current(
        &mut self,
        token: PublicationToken,
        current: PublicationContext,
        snapshot: ImmutableSnapshot<T>,
    ) -> Result<(), CoreError> {
        token.validate_against(current)?;
        if snapshot.revision() != token.context().source() {
            return Err(CoreError::StaleSourceRevision);
        }
        self.publish(snapshot)
    }

    pub fn head(&self) -> Option<&ImmutableSnapshot<T>> {
        self.head.as_ref()
    }
}

pub struct SnapshotDelta<T> {
    owner: ArenaOwnerId,
    base: SourceRevision,
    target: SourceRevision,
    max_items: u64,
    items: Vec<T>,
    count: u64,
}

impl<T> SnapshotDelta<T> {
    pub fn new(
        owner: ArenaOwnerId,
        base: SourceRevision,
        target: SourceRevision,
        max_items: u64,
    ) -> Result<Self, CoreError> {
        if base.owner() != owner || target.owner() != owner {
            return Err(CoreError::OwnershipMismatch);
        }
        if target.get() <= base.get() {
            return Err(CoreError::StalePublication);
        }
        if max_items == 0 {
            return Err(CoreError::LimitExceeded);
        }
        Ok(Self {
            owner,
            base,
            target,
            max_items,
            items: Vec::new(),
            count: 0,
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn base(&self) -> SourceRevision {
        self.base
    }

    pub const fn target(&self) -> SourceRevision {
        self.target
    }

    pub const fn max_items(&self) -> u64 {
        self.max_items
    }

    pub fn push(&mut self, item: T) -> Result<(), CoreError> {
        if self.count >= self.max_items {
            return Err(CoreError::LimitExceeded);
        }
        self.items.push(item);
        self.count = self.count.checked_add(1).ok_or(CoreError::ArithmeticOverflow)?;
        Ok(())
    }

    pub fn len(&self) -> u64 {
        self.count
    }

    pub fn is_empty(&self) -> bool {
        self.count == 0
    }

    pub fn items(&self) -> &[T] {
        &self.items
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocator_retires_after_u64_max_without_wrapping() {
        let owner = ArenaOwnerId::new(7).unwrap();
        let mut allocator = IdAllocator::<FileId>::new(owner, u64::MAX).unwrap();
        assert_eq!(allocator.allocate().unwrap().get(), u64::MAX);
        assert_eq!(allocator.allocate(), Err(CoreError::Exhausted));
    }

    #[test]
    fn ranges_reject_reversal_overflow_and_limits() {
        let max = ByteOffset::new(u64::MAX);
        assert_eq!(max.checked_add(1), Err(CoreError::ArithmeticOverflow));
        assert_eq!(ByteOffset::new(0).checked_sub(1), Err(CoreError::ArithmeticUnderflow));
        let reversed = ByteRange::new(ByteOffset::new(9), ByteOffset::new(8));
        assert_eq!(reversed, Err(CoreError::RangeReversed));
        let range = ByteRange::new(ByteOffset::new(2), ByteOffset::new(8)).unwrap();
        assert_eq!(range.checked_within(ByteOffset::new(7)), Err(CoreError::LimitExceeded));
        assert_eq!(range.len().get(), 6);
        let limits = Limits::new(ByteLength::new(8), 2).unwrap();
        assert_eq!(limits.check_bytes(ByteLength::new(9)), Err(CoreError::LimitExceeded));
        assert_eq!(limits.check_items(3), Err(CoreError::LimitExceeded));
    }

    #[test]
    fn stale_publications_are_rejected_without_replacing_the_head() {
        let owner = ArenaOwnerId::new(11).unwrap();
        let mut revisions = IdAllocator::<SourceRevision>::new(owner, 1).unwrap();
        let first = ImmutableSnapshot::new(owner, revisions.allocate().unwrap(), "first").unwrap();
        let second = ImmutableSnapshot::new(owner, revisions.allocate().unwrap(), "second").unwrap();
        let mut cell = SnapshotCell::new(owner);
        cell.publish(second).unwrap();
        assert_eq!(cell.publish(first), Err(CoreError::StalePublication));
        assert_eq!(cell.head().unwrap().get(), &"second");
    }

    #[test]
    fn equal_slot_values_from_independent_owners_cannot_alias() {
        let owner_a = ArenaOwnerId::new(1).unwrap();
        let owner_b = ArenaOwnerId::new(2).unwrap();
        let revision_a = SourceRevision::new(owner_a, 1).unwrap();
        let revision_b = SourceRevision::new(owner_b, 1).unwrap();
        assert_ne!(revision_a, revision_b);
        let foreign = ImmutableSnapshot::new(owner_b, revision_b, 42).unwrap();
        let mut cell = SnapshotCell::new(owner_a);
        assert_eq!(cell.publish(foreign), Err(CoreError::OwnershipMismatch));
    }

    #[test]
    fn publication_token_rejects_each_stale_dimension() {
        let owner = ArenaOwnerId::new(31).unwrap();
        let request = QueryGeneration::new(owner, 1).unwrap();
        let source = SourceRevision::new(owner, 2).unwrap();
        let display = DisplayGeneration::new(owner, 3).unwrap();
        let token = PublicationToken::new(PublicationContext::new(owner, request, source, display).unwrap());
        assert_eq!(token.validate_against(PublicationContext::new(owner, QueryGeneration::new(owner, 2).unwrap(), source, display).unwrap()), Err(CoreError::StaleRequestGeneration));
        assert_eq!(token.validate_against(PublicationContext::new(owner, request, SourceRevision::new(owner, 4).unwrap(), display).unwrap()), Err(CoreError::StaleSourceRevision));
        assert_eq!(token.validate_against(PublicationContext::new(owner, request, source, DisplayGeneration::new(owner, 5).unwrap()).unwrap()), Err(CoreError::StaleDisplayGeneration));
        assert_eq!(token.validate_against(token.context()), Ok(()));
    }

    #[test]
    fn snapshot_delta_is_bounded_and_owner_qualified() {
        let owner = ArenaOwnerId::new(41).unwrap();
        let base = SourceRevision::new(owner, 1).unwrap();
        let target = SourceRevision::new(owner, 2).unwrap();
        let mut delta = SnapshotDelta::new(owner, base, target, 1).unwrap();
        delta.push("change").unwrap();
        assert_eq!(delta.push("overflow"), Err(CoreError::LimitExceeded));
        assert_eq!(delta.items(), &["change"]);
    }

    #[test]
    fn core_error_implements_display_and_std_error() {
        use std::error::Error;
        let err = CoreError::InvalidId;
        assert_eq!(format!("{err}"), "INVALID_ID");
        let trait_obj: &dyn Error = &err;
        assert_eq!(trait_obj.to_string(), "INVALID_ID");
    }

    #[test]
    fn persisted_id_authority_query_methods_are_accurate() {
        let owner = ArenaOwnerId::new(51).unwrap();
        let mut authority = PersistedIdAuthority::new(owner);
        assert!(authority.is_empty());
        assert_eq!(authority.len(), 0);

        let id = FileId::new(owner, 10).unwrap();
        assert!(!authority.contains(id));

        authority.accept(id).unwrap();
        assert!(!authority.is_empty());
        assert_eq!(authority.len(), 1);
        assert!(authority.contains(id));
    }

    #[test]
    fn range_and_byte_length_is_empty_are_consistent() {
        assert!(ByteLength::new(0).is_empty());
        assert!(!ByteLength::new(1).is_empty());

        let empty_range = ByteRange::new(ByteOffset::new(5), ByteOffset::new(5)).unwrap();
        assert!(empty_range.is_empty());
        assert_eq!(empty_range.len().get(), 0);

        let non_empty = ByteRange::new(ByteOffset::new(5), ByteOffset::new(10)).unwrap();
        assert!(!non_empty.is_empty());
        assert_eq!(non_empty.len().get(), 5);
    }
}
