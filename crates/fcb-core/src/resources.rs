//! Owned managed-resource and queue-byte admission leases.
//!
//! `ResourceBudget` is an accounting domain, not an operating-system memory
//! limit. Callers pass the allocated capacity they are about to retain, and
//! the budget rejects a reservation before construction when the shared
//! domain would exceed its configured ceiling. A lease remains charged until
//! its last handle is dropped, including when the same allocation is shared by
//! more than one logical owner.

use std::{
    collections::BTreeMap,
    fmt,
    sync::{Arc, Mutex},
};

use crate::{ArenaOwnerId, ByteLength, CoreError};

/// Stable key for one allocation within a [`ResourceBudget`] domain.
///
/// A raw key identifies a new allocation reservation. Reusing an active key
/// through [`ResourceBudget::try_reserve`] is rejected; shared ownership must
/// be obtained from an existing validated [`ResourceLease`]. The budget then
/// verifies that the resource kind and allocated capacity agree across all
/// references to that key.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResourceAllocationId(u64);

impl ResourceAllocationId {
    /// Construct a non-zero allocation key.
    pub const fn new(value: u64) -> Result<Self, CoreError> {
        if value == 0 {
            Err(CoreError::InvalidId)
        } else {
            Ok(Self(value))
        }
    }

    /// Return the stable allocation key.
    pub const fn get(self) -> u64 {
        self.0
    }
}

/// Accounting class for a reservation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceKind {
    /// Retained or in-flight managed allocation capacity.
    Managed,
    /// Bytes owned by a bounded work or completion queue payload.
    Queue,
}

/// The protected part of a resource budget reserved for reclamation progress.
///
/// Ordinary work cannot consume terminal or retirement capacity. Keeping the
/// two classes separate prevents a full candidate queue from making it
/// impossible to record completion or retire an old generation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceReservationClass {
    /// Capacity available to ordinary managed allocations and queue payloads.
    Ordinary,
    /// Capacity reserved for an admitted terminal/completion record.
    Terminal,
    /// Capacity reserved for an admitted retirement/reclamation record.
    Retirement,
}

/// Global order for acquiring resource-admission permits.
///
/// A [`ResourceAcquisitionSequence`] rejects a request that moves backwards
/// in this order. Callers must therefore acquire publication, byte, pin, and
/// service capacity in the same order across all subsystems.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum ResourceAcquisitionOrder {
    Publication,
    Bytes,
    SourcePin,
    AssetPin,
    Service,
}

/// Typed refusal from an ordered or protected resource admission operation.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResourceAdmissionError {
    /// A zero-byte reservation or reconciliation is not a valid lease.
    InvalidBytes,
    /// The allocation key is already active; sharing requires a lease.
    AllocationConflict,
    /// The requested class has no remaining capacity.
    CapacityExhausted {
        class: ResourceReservationClass,
        requested: ByteLength,
        available: ByteLength,
    },
    /// The request would acquire resources out of the global order.
    AcquisitionOrderViolation {
        held: ResourceAcquisitionOrder,
        requested: ResourceAcquisitionOrder,
    },
    /// The actual allocation is larger than its reservation and the extra
    /// bytes cannot be admitted without changing the accounting first.
    ReconciliationDenied {
        class: ResourceReservationClass,
        reserved: ByteLength,
        actual: ByteLength,
        available: ByteLength,
    },
    /// Internal checked arithmetic could not represent the next counters.
    ArithmeticOverflow,
}

impl ResourceAdmissionError {
    fn into_core_error(self) -> CoreError {
        match self {
            Self::InvalidBytes => CoreError::InvalidId,
            Self::AllocationConflict => CoreError::OwnershipMismatch,
            Self::CapacityExhausted { .. } | Self::ReconciliationDenied { .. } => {
                CoreError::LimitExceeded
            }
            Self::AcquisitionOrderViolation { .. } => CoreError::OwnershipMismatch,
            Self::ArithmeticOverflow => CoreError::ArithmeticOverflow,
        }
    }
}

impl fmt::Display for ResourceAdmissionError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidBytes => formatter.write_str("invalid resource byte length"),
            Self::AllocationConflict => formatter.write_str("resource allocation conflict"),
            Self::CapacityExhausted { class, requested, available } => {
                write!(
                    formatter,
                    "resource capacity exhausted for class {class:?}: requested {} bytes, available {} bytes",
                    requested.get(),
                    available.get()
                )
            }
            Self::AcquisitionOrderViolation { held, requested } => {
                write!(
                    formatter,
                    "resource acquisition order violation: held {held:?}, requested {requested:?}"
                )
            }
            Self::ReconciliationDenied { class, reserved, actual, available } => {
                write!(
                    formatter,
                    "resource reconciliation denied for class {class:?}: reserved {} bytes, actual {} bytes, available {} bytes",
                    reserved.get(),
                    actual.get(),
                    available.get()
                )
            }
            Self::ArithmeticOverflow => formatter.write_str("resource accounting arithmetic overflow"),
        }
    }
}

impl std::error::Error for ResourceAdmissionError {}

/// A per-operation acquisition cursor enforcing the global resource order.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResourceAcquisitionSequence {
    owner: ArenaOwnerId,
    last: Option<ResourceAcquisitionOrder>,
}

impl ResourceAcquisitionSequence {
    /// Start an ordered admission sequence for one owner.
    pub const fn new(owner: ArenaOwnerId) -> Self {
        Self { owner, last: None }
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn last(self) -> Option<ResourceAcquisitionOrder> {
        self.last
    }

    /// Reserve a resource while advancing this sequence's acquisition order.
    pub fn try_reserve(
        &mut self,
        budget: &ResourceBudget,
        allocation: ResourceAllocationId,
        kind: ResourceKind,
        class: ResourceReservationClass,
        order: ResourceAcquisitionOrder,
        bytes: ByteLength,
    ) -> Result<ResourceLease, ResourceAdmissionError> {
        if let Some(held) = self.last {
            if order < held {
                return Err(ResourceAdmissionError::AcquisitionOrderViolation {
                    held,
                    requested: order,
                });
            }
        }
        let lease = budget.try_reserve_class(self.owner, allocation, kind, class, bytes)?;
        self.last = Some(order);
        Ok(lease)
    }
}

/// Immutable identity and charge carried by a resource lease.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResourceLeaseInfo {
    domain: ArenaOwnerId,
    owner: ArenaOwnerId,
    allocation: ResourceAllocationId,
    kind: ResourceKind,
    bytes: ByteLength,
}

impl ResourceLeaseInfo {
    pub const fn domain(self) -> ArenaOwnerId {
        self.domain
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn allocation(self) -> ResourceAllocationId {
        self.allocation
    }

    pub const fn kind(self) -> ResourceKind {
        self.kind
    }

    pub const fn bytes(self) -> ByteLength {
        self.bytes
    }
}

/// Current conservation counters for one resource domain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ResourceAccounting {
    capacity: ByteLength,
    terminal_capacity: ByteLength,
    retirement_capacity: ByteLength,
    reserved: ByteLength,
    terminal_reserved: ByteLength,
    retirement_reserved: ByteLength,
    managed: ByteLength,
    queue: ByteLength,
    peak_reserved: ByteLength,
    active_allocations: u64,
    active_leases: u64,
}

impl ResourceAccounting {
    pub const fn capacity(self) -> ByteLength {
        self.capacity
    }

    pub const fn terminal_capacity(self) -> ByteLength {
        self.terminal_capacity
    }

    pub const fn retirement_capacity(self) -> ByteLength {
        self.retirement_capacity
    }

    pub const fn reserved(self) -> ByteLength {
        self.reserved
    }

    pub const fn terminal_reserved(self) -> ByteLength {
        self.terminal_reserved
    }

    pub const fn retirement_reserved(self) -> ByteLength {
        self.retirement_reserved
    }

    pub const fn managed(self) -> ByteLength {
        self.managed
    }

    pub const fn queue(self) -> ByteLength {
        self.queue
    }

    pub const fn available(self) -> ByteLength {
        ByteLength::new(self.capacity.get() - self.reserved.get())
    }

    /// Capacity still available to ordinary work, excluding protected slots.
    pub const fn ordinary_available(self) -> ByteLength {
        let protected = self
            .terminal_capacity
            .get()
            .saturating_add(self.retirement_capacity.get());
        let ordinary_capacity = self.capacity.get().saturating_sub(protected);
        let ordinary_reserved = self
            .reserved
            .get()
            .saturating_sub(self.terminal_reserved.get())
            .saturating_sub(self.retirement_reserved.get());
        ByteLength::new(ordinary_capacity.saturating_sub(ordinary_reserved))
    }

    pub const fn terminal_available(self) -> ByteLength {
        ByteLength::new(
            self.terminal_capacity
                .get()
                .saturating_sub(self.terminal_reserved.get()),
        )
    }

    pub const fn retirement_available(self) -> ByteLength {
        ByteLength::new(
            self.retirement_capacity
                .get()
                .saturating_sub(self.retirement_reserved.get()),
        )
    }

    pub const fn protected_reserved(self) -> ByteLength {
        ByteLength::new(
            self.terminal_reserved
                .get()
                .saturating_add(self.retirement_reserved.get()),
        )
    }

    pub const fn peak_reserved(self) -> ByteLength {
        self.peak_reserved
    }

    pub const fn active_allocations(self) -> u64 {
        self.active_allocations
    }

    pub const fn active_leases(self) -> u64 {
        self.active_leases
    }
}

#[derive(Debug)]
struct AllocationRecord {
    kind: ResourceKind,
    class: ResourceReservationClass,
    bytes: u64,
    references: u64,
    owners: BTreeMap<ArenaOwnerId, u64>,
}

#[derive(Debug)]
struct ResourceLedger {
    domain: ArenaOwnerId,
    capacity: u64,
    terminal_capacity: u64,
    retirement_capacity: u64,
    reserved: u64,
    terminal_reserved: u64,
    retirement_reserved: u64,
    managed: u64,
    queue: u64,
    peak_reserved: u64,
    active_leases: u64,
    allocations: BTreeMap<ResourceAllocationId, AllocationRecord>,
}

impl ResourceLedger {
    fn accounting(&self) -> ResourceAccounting {
        ResourceAccounting {
            capacity: ByteLength::new(self.capacity),
            terminal_capacity: ByteLength::new(self.terminal_capacity),
            retirement_capacity: ByteLength::new(self.retirement_capacity),
            reserved: ByteLength::new(self.reserved),
            terminal_reserved: ByteLength::new(self.terminal_reserved),
            retirement_reserved: ByteLength::new(self.retirement_reserved),
            managed: ByteLength::new(self.managed),
            queue: ByteLength::new(self.queue),
            peak_reserved: ByteLength::new(self.peak_reserved),
            active_allocations: self.allocations.len() as u64,
            active_leases: self.active_leases,
        }
    }

    fn class_available(&self, class: ResourceReservationClass) -> u64 {
        match class {
            ResourceReservationClass::Ordinary => self
                .capacity
                .saturating_sub(self.terminal_capacity)
                .saturating_sub(self.retirement_capacity)
                .saturating_sub(
                    self.reserved
                        .saturating_sub(self.terminal_reserved)
                        .saturating_sub(self.retirement_reserved),
                ),
            ResourceReservationClass::Terminal => {
                self.terminal_capacity.saturating_sub(self.terminal_reserved)
            }
            ResourceReservationClass::Retirement => self
                .retirement_capacity
                .saturating_sub(self.retirement_reserved),
        }
    }

    fn add_class(
        &mut self,
        class: ResourceReservationClass,
        bytes: u64,
    ) -> Result<(), ResourceAdmissionError> {
        match class {
            ResourceReservationClass::Ordinary => Ok(()),
            ResourceReservationClass::Terminal => {
                self.terminal_reserved = self
                    .terminal_reserved
                    .checked_add(bytes)
                    .ok_or(ResourceAdmissionError::ArithmeticOverflow)?;
                Ok(())
            }
            ResourceReservationClass::Retirement => {
                self.retirement_reserved = self
                    .retirement_reserved
                    .checked_add(bytes)
                    .ok_or(ResourceAdmissionError::ArithmeticOverflow)?;
                Ok(())
            }
        }
    }

    fn subtract_class(&mut self, class: ResourceReservationClass, bytes: u64) {
        match class {
            ResourceReservationClass::Ordinary => {}
            ResourceReservationClass::Terminal => self.terminal_reserved -= bytes,
            ResourceReservationClass::Retirement => self.retirement_reserved -= bytes,
        }
    }

    fn add_kind(&mut self, kind: ResourceKind, bytes: u64) -> Result<(), CoreError> {
        match kind {
            ResourceKind::Managed => {
                self.managed = self
                    .managed
                    .checked_add(bytes)
                    .ok_or(CoreError::ArithmeticOverflow)?;
            }
            ResourceKind::Queue => {
                self.queue = self
                    .queue
                    .checked_add(bytes)
                    .ok_or(CoreError::ArithmeticOverflow)?;
            }
        }
        Ok(())
    }

    fn subtract_kind(&mut self, kind: ResourceKind, bytes: u64) {
        match kind {
            ResourceKind::Managed => self.managed -= bytes,
            ResourceKind::Queue => self.queue -= bytes,
        }
    }

    fn add_shared_reference(
        &mut self,
        owner: ArenaOwnerId,
        allocation: ResourceAllocationId,
        kind: ResourceKind,
    ) -> Result<(), CoreError> {
        let next_active_leases = self
            .active_leases
            .checked_add(1)
            .ok_or(CoreError::ArithmeticOverflow)?;
        let record = self
            .allocations
            .get_mut(&allocation)
            .ok_or(CoreError::OwnershipMismatch)?;
        if record.kind != kind {
            return Err(CoreError::OwnershipMismatch);
        }
        let next_references = record
            .references
            .checked_add(1)
            .ok_or(CoreError::ArithmeticOverflow)?;
        let next_owner_references = record
            .owners
            .get(&owner)
            .copied()
            .unwrap_or(0)
            .checked_add(1)
            .ok_or(CoreError::ArithmeticOverflow)?;
        record.references = next_references;
        record.owners.insert(owner, next_owner_references);
        self.active_leases = next_active_leases;
        Ok(())
    }

    fn remove_reference(&mut self, owner: ArenaOwnerId, allocation: ResourceAllocationId) {
        let should_remove = {
            let record = self
                .allocations
                .get_mut(&allocation)
                .expect("resource lease must correspond to an active allocation");
            record.references -= 1;
            let owner_references = record
                .owners
                .get_mut(&owner)
                .expect("resource lease owner must be registered");
            *owner_references -= 1;
            if *owner_references == 0 {
                record.owners.remove(&owner);
            }
            record.references == 0
        };

        self.active_leases -= 1;
        if should_remove {
            let record = self
                .allocations
                .remove(&allocation)
                .expect("resource allocation disappeared during release");
            self.reserved -= record.bytes;
            self.subtract_class(record.class, record.bytes);
            self.subtract_kind(record.kind, record.bytes);
        }
    }

    fn reconcile(
        &mut self,
        allocation: ResourceAllocationId,
        actual: ByteLength,
    ) -> Result<(), ResourceAdmissionError> {
        if actual.get() == 0 {
            return Err(ResourceAdmissionError::InvalidBytes);
        }
        let (class, kind, reserved) = {
            let record = self
                .allocations
                .get(&allocation)
                .ok_or(ResourceAdmissionError::AllocationConflict)?;
            (record.class, record.kind, record.bytes)
        };
        if actual.get() == reserved {
            return Ok(());
        }

        if actual.get() > reserved {
            let additional = actual.get() - reserved;
            let available = self.class_available(class);
            if additional > available {
                return Err(ResourceAdmissionError::ReconciliationDenied {
                    class,
                    reserved: ByteLength::new(reserved),
                    actual,
                    available: ByteLength::new(available),
                });
            }
            let next_reserved = self
                .reserved
                .checked_add(additional)
                .ok_or(ResourceAdmissionError::ArithmeticOverflow)?;
            let next_terminal = match class {
                ResourceReservationClass::Terminal => Some(
                    self.terminal_reserved
                        .checked_add(additional)
                        .ok_or(ResourceAdmissionError::ArithmeticOverflow)?,
                ),
                _ => None,
            };
            let next_retirement = match class {
                ResourceReservationClass::Retirement => Some(
                    self.retirement_reserved
                        .checked_add(additional)
                        .ok_or(ResourceAdmissionError::ArithmeticOverflow)?,
                ),
                _ => None,
            };
            let (next_managed, next_queue) = match kind {
                ResourceKind::Managed => (
                    Some(
                        self.managed
                            .checked_add(additional)
                            .ok_or(ResourceAdmissionError::ArithmeticOverflow)?,
                    ),
                    None,
                ),
                ResourceKind::Queue => (
                    None,
                    Some(
                        self.queue
                            .checked_add(additional)
                            .ok_or(ResourceAdmissionError::ArithmeticOverflow)?,
                    ),
                ),
            };
            self.reserved = next_reserved;
            if let Some(terminal) = next_terminal {
                self.terminal_reserved = terminal;
            }
            if let Some(retirement) = next_retirement {
                self.retirement_reserved = retirement;
            }
            if let Some(managed) = next_managed {
                self.managed = managed;
            }
            if let Some(queue) = next_queue {
                self.queue = queue;
            }
            self.peak_reserved = self.peak_reserved.max(self.reserved);
        } else {
            let released = reserved - actual.get();
            self.reserved -= released;
            self.subtract_class(class, released);
            self.subtract_kind(kind, released);
        }
        self.allocations
            .get_mut(&allocation)
            .expect("allocation checked above")
            .bytes = actual.get();
        Ok(())
    }
}

/// Shared accounting domain for managed and queue-byte reservations.
#[derive(Clone)]
pub struct ResourceBudget {
    ledger: Arc<Mutex<ResourceLedger>>,
}

impl std::fmt::Debug for ResourceBudget {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceBudget")
            .field("accounting", &self.accounting())
            .finish()
    }
}

impl ResourceBudget {
    /// Create a non-empty accounting domain with one shared hard ceiling.
    pub fn new(domain: ArenaOwnerId, capacity: ByteLength) -> Result<Self, CoreError> {
        Self::new_with_protected_capacity(domain, capacity, ByteLength::new(0), ByteLength::new(0))
    }

    /// Create a budget with independent terminal and retirement capacity.
    ///
    /// The protected capacities are part of the supplied total, but ordinary
    /// work cannot consume them. This makes terminal completion and retirement
    /// progress possible while the ordinary pool is saturated.
    pub fn new_with_protected_capacity(
        domain: ArenaOwnerId,
        capacity: ByteLength,
        terminal_capacity: ByteLength,
        retirement_capacity: ByteLength,
    ) -> Result<Self, CoreError> {
        if capacity.get() == 0 {
            return Err(CoreError::InvalidId);
        }
        let protected = terminal_capacity
            .get()
            .checked_add(retirement_capacity.get())
            .ok_or(CoreError::ArithmeticOverflow)?;
        if protected > capacity.get() {
            return Err(CoreError::LimitExceeded);
        }
        Ok(Self {
            ledger: Arc::new(Mutex::new(ResourceLedger {
                domain,
                capacity: capacity.get(),
                terminal_capacity: terminal_capacity.get(),
                retirement_capacity: retirement_capacity.get(),
                reserved: 0,
                terminal_reserved: 0,
                retirement_reserved: 0,
                managed: 0,
                queue: 0,
                peak_reserved: 0,
                active_leases: 0,
                allocations: BTreeMap::new(),
            })),
        })
    }

    /// Return the host/accounting domain that owns this budget.
    pub fn domain(&self) -> ArenaOwnerId {
        self.lock().domain
    }

    /// Read current counters without changing the accounting domain.
    pub fn accounting(&self) -> ResourceAccounting {
        self.lock().accounting()
    }

    /// Reserve allocated capacity for one managed or queue allocation.
    ///
    /// A raw allocation key can be reserved only once while active. A second
    /// key is an old/new overlap and is charged independently. To represent
    /// true shared ownership, pass an existing lease to [`Self::try_share`];
    /// equal integers alone are not a sharing capability. Capacity is checked
    /// before the ledger is changed, so a failed request leaves every counter
    /// and existing lease untouched.
    pub fn try_reserve(
        &self,
        owner: ArenaOwnerId,
        allocation: ResourceAllocationId,
        kind: ResourceKind,
        bytes: ByteLength,
    ) -> Result<ResourceLease, CoreError> {
        self.try_reserve_class(
            owner,
            allocation,
            kind,
            ResourceReservationClass::Ordinary,
            bytes,
        )
        .map_err(ResourceAdmissionError::into_core_error)
    }

    /// Reserve capacity from a specific ordinary, terminal, or retirement pool.
    pub fn try_reserve_class(
        &self,
        owner: ArenaOwnerId,
        allocation: ResourceAllocationId,
        kind: ResourceKind,
        class: ResourceReservationClass,
        bytes: ByteLength,
    ) -> Result<ResourceLease, ResourceAdmissionError> {
        if bytes.get() == 0 {
            return Err(ResourceAdmissionError::InvalidBytes);
        }

        let mut ledger = self.lock();
        if ledger.allocations.contains_key(&allocation) {
            return Err(ResourceAdmissionError::AllocationConflict);
        }
        let available = ledger.class_available(class);
        if bytes.get() > available {
            return Err(ResourceAdmissionError::CapacityExhausted {
                class,
                requested: bytes,
                available: ByteLength::new(available),
            });
        }
        let next_active_leases = ledger
            .active_leases
            .checked_add(1)
            .ok_or(ResourceAdmissionError::ArithmeticOverflow)?;
        let reserved = ledger
            .reserved
            .checked_add(bytes.get())
            .ok_or(ResourceAdmissionError::ArithmeticOverflow)?;
        ledger.add_kind(kind, bytes.get())
            .map_err(|_| ResourceAdmissionError::ArithmeticOverflow)?;
        ledger.add_class(class, bytes.get())?;
        ledger.reserved = reserved;
        ledger.peak_reserved = ledger.peak_reserved.max(reserved);
        ledger.active_leases = next_active_leases;
        let mut owners = BTreeMap::new();
        owners.insert(owner, 1);
        ledger.allocations.insert(
            allocation,
            AllocationRecord {
                kind,
                class,
                bytes: bytes.get(),
                references: 1,
                owners,
            },
        );

        Ok(ResourceLease {
            inner: Arc::new(LeaseInner {
                domain: ledger.domain,
                owner,
                allocation,
                kind,
                ledger: Arc::clone(&self.ledger),
            }),
        })
    }

    pub fn try_reserve_terminal(
        &self,
        owner: ArenaOwnerId,
        allocation: ResourceAllocationId,
        kind: ResourceKind,
        bytes: ByteLength,
    ) -> Result<ResourceLease, ResourceAdmissionError> {
        self.try_reserve_class(
            owner,
            allocation,
            kind,
            ResourceReservationClass::Terminal,
            bytes,
        )
    }

    pub fn try_reserve_retirement(
        &self,
        owner: ArenaOwnerId,
        allocation: ResourceAllocationId,
        kind: ResourceKind,
        bytes: ByteLength,
    ) -> Result<ResourceLease, ResourceAdmissionError> {
        self.try_reserve_class(
            owner,
            allocation,
            kind,
            ResourceReservationClass::Retirement,
            bytes,
        )
    }

    /// Share an existing allocation through a validated lease capability.
    ///
    /// The source lease must belong to this exact budget ledger, not merely to
    /// a budget with an equal domain ID. This prevents independent budgets or
    /// guessed raw allocation keys from bypassing accounting. The shared
    /// allocation remains charged until every reservation and lease clone is
    /// released.
    pub fn try_share(
        &self,
        owner: ArenaOwnerId,
        source: &ResourceLease,
    ) -> Result<ResourceLease, CoreError> {
        if !Arc::ptr_eq(&self.ledger, &source.inner.ledger) {
            return Err(CoreError::OwnershipMismatch);
        }
        let allocation = source.inner.allocation;
        let kind = source.inner.kind;
        let mut ledger = self.lock();
        ledger.add_shared_reference(owner, allocation, kind)?;
        Ok(ResourceLease {
            inner: Arc::new(LeaseInner {
                domain: ledger.domain,
                owner,
                allocation,
                kind,
                ledger: Arc::clone(&self.ledger),
            }),
        })
    }

    /// Reserve managed allocation capacity.
    pub fn try_reserve_managed(
        &self,
        owner: ArenaOwnerId,
        allocation: ResourceAllocationId,
        bytes: ByteLength,
    ) -> Result<ResourceLease, CoreError> {
        self.try_reserve(owner, allocation, ResourceKind::Managed, bytes)
    }

    /// Reserve bounded queue-payload capacity.
    pub fn try_reserve_queue_bytes(
        &self,
        owner: ArenaOwnerId,
        allocation: ResourceAllocationId,
        bytes: ByteLength,
    ) -> Result<ResourceLease, CoreError> {
        self.try_reserve(owner, allocation, ResourceKind::Queue, bytes)
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ResourceLedger> {
        self.ledger.lock().unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

/// Owned reservation that keeps its allocation charged until its final clone
/// is dropped. Dropping a lease cannot affect another budget domain.
pub struct ResourceLease {
    inner: Arc<LeaseInner>,
}

impl Clone for ResourceLease {
    fn clone(&self) -> Self {
        Self {
            inner: Arc::clone(&self.inner),
        }
    }
}

impl std::fmt::Debug for ResourceLease {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResourceLease")
            .field("info", &self.info())
            .finish()
    }
}

impl ResourceLease {
    pub fn info(&self) -> ResourceLeaseInfo {
        let ledger = self
            .inner
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let bytes = ledger
            .allocations
            .get(&self.inner.allocation)
            .expect("resource lease must correspond to an active allocation")
            .bytes;
        ResourceLeaseInfo {
            domain: self.inner.domain,
            owner: self.inner.owner,
            allocation: self.inner.allocation,
            kind: self.inner.kind,
            bytes: ByteLength::new(bytes),
        }
    }

    /// Read the authoritative accounting while this lease keeps its domain
    /// alive, even if every [`ResourceBudget`] handle has been dropped.
    pub fn accounting(&self) -> ResourceAccounting {
        self.inner
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .accounting()
    }

    /// Reconcile the reservation with the allocation's actual retained
    /// capacity before publication. Shrinking returns bytes; growing requires
    /// checked capacity in the same reservation class. A denied growth leaves
    /// both the lease and every accounting counter unchanged.
    pub fn reconcile(&self, actual: ByteLength) -> Result<(), ResourceAdmissionError> {
        self.inner
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .reconcile(self.inner.allocation, actual)
    }

    /// Explicitly release this handle. Other clones or owners remain charged.
    pub fn release(self) {
        drop(self);
    }
}

struct LeaseInner {
    domain: ArenaOwnerId,
    owner: ArenaOwnerId,
    allocation: ResourceAllocationId,
    kind: ResourceKind,
    ledger: Arc<Mutex<ResourceLedger>>,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger.remove_reference(self.owner, self.allocation);
    }
}
