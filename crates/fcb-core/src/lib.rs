#![forbid(unsafe_code)]

use std::{
    collections::BTreeSet,
    marker::PhantomData,
    sync::{Arc, Mutex, OnceLock},
};

pub mod handles;
pub mod resources;

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
            Self::StalePublication => "STALE_PUBLICATION",
            Self::StaleRequestGeneration => "STALE_REQUEST_GENERATION",
            Self::StaleSourceRevision => "STALE_SOURCE_REVISION",
            Self::StaleDisplayGeneration => "STALE_DISPLAY_GENERATION",
        }
    }
}

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

pub trait AllocatedId: Copy {
    /// Whether the raw counter belongs to a process-persistent identity domain.
    /// Persistent counters are reserved once and cannot be reused by another
    /// owner, preventing a duplicate persisted identity after allocator loss.
    const GLOBAL_PERSISTED_COUNTERS: bool = false;

    fn from_parts(owner: ArenaOwnerId, value: u64) -> Result<Self, CoreError>;
}

static PERSISTED_COUNTERS: OnceLock<Mutex<BTreeSet<u64>>> = OnceLock::new();

fn reserve_persisted_counter(value: u64) -> Result<(), CoreError> {
    let registry = PERSISTED_COUNTERS.get_or_init(|| Mutex::new(BTreeSet::new()));
    let mut counters = registry
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if counters.insert(value) {
        Ok(())
    } else {
        Err(CoreError::DuplicateId)
    }
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

impl AllocatedId for FileId {
    const GLOBAL_PERSISTED_COUNTERS: bool = true;

    fn from_parts(owner: ArenaOwnerId, value: u64) -> Result<Self, CoreError> {
        Self::new(owner, value)
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
        if I::GLOBAL_PERSISTED_COUNTERS {
            reserve_persisted_counter(value)?;
        }
        I::from_parts(self.owner, value)
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
offset_domain!(Utf8Offset);
offset_domain!(Utf16Offset);
offset_domain!(ScalarOffset);
offset_domain!(GraphemeOffset);
offset_domain!(VisualOffset);

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ByteLength(u64);

impl ByteLength {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub const fn get(self) -> u64 {
        self.0
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

pub type ByteRange = OffsetRange<ByteOffset>;
pub type Utf8Range = OffsetRange<Utf8Offset>;
pub type Utf16Range = OffsetRange<Utf16Offset>;
pub type ScalarRange = OffsetRange<ScalarOffset>;
pub type GraphemeRange = OffsetRange<GraphemeOffset>;
pub type VisualRange = OffsetRange<VisualOffset>;

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
}
