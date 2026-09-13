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
    reserved: ByteLength,
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

    pub const fn reserved(self) -> ByteLength {
        self.reserved
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
    bytes: u64,
    references: u64,
    owners: BTreeMap<ArenaOwnerId, u64>,
}

#[derive(Debug)]
struct ResourceLedger {
    domain: ArenaOwnerId,
    capacity: u64,
    reserved: u64,
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
            reserved: ByteLength::new(self.reserved),
            managed: ByteLength::new(self.managed),
            queue: ByteLength::new(self.queue),
            peak_reserved: ByteLength::new(self.peak_reserved),
            active_allocations: self.allocations.len() as u64,
            active_leases: self.active_leases,
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
        bytes: u64,
    ) -> Result<(), CoreError> {
        let next_active_leases = self
            .active_leases
            .checked_add(1)
            .ok_or(CoreError::ArithmeticOverflow)?;
        let record = self
            .allocations
            .get_mut(&allocation)
            .ok_or(CoreError::OwnershipMismatch)?;
        if record.kind != kind || record.bytes != bytes {
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
            self.subtract_kind(record.kind, record.bytes);
        }
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
        if capacity.get() == 0 {
            return Err(CoreError::InvalidId);
        }
        Ok(Self {
            ledger: Arc::new(Mutex::new(ResourceLedger {
                domain,
                capacity: capacity.get(),
                reserved: 0,
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
        if bytes.get() == 0 {
            return Err(CoreError::InvalidId);
        }

        let mut ledger = self.lock();
        if ledger.allocations.contains_key(&allocation) {
            return Err(CoreError::OwnershipMismatch);
        }
        let next_active_leases = ledger
            .active_leases
            .checked_add(1)
            .ok_or(CoreError::ArithmeticOverflow)?;
        let reserved = ledger
            .reserved
            .checked_add(bytes.get())
            .ok_or(CoreError::ArithmeticOverflow)?;
        if reserved > ledger.capacity {
            return Err(CoreError::LimitExceeded);
        }
        ledger.add_kind(kind, bytes.get())?;
        ledger.reserved = reserved;
        ledger.peak_reserved = ledger.peak_reserved.max(reserved);
        ledger.active_leases = next_active_leases;
        let mut owners = BTreeMap::new();
        owners.insert(owner, 1);
        ledger.allocations.insert(
            allocation,
            AllocationRecord {
                kind,
                bytes: bytes.get(),
                references: 1,
                owners,
            },
        );

        Ok(ResourceLease {
            inner: Arc::new(LeaseInner {
                info: ResourceLeaseInfo {
                    domain: ledger.domain,
                    owner,
                    allocation,
                    kind,
                    bytes,
                },
                ledger: Arc::clone(&self.ledger),
            }),
        })
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
        let info = source.info();
        let mut ledger = self.lock();
        ledger.add_shared_reference(owner, info.allocation, info.kind, info.bytes.get())?;
        Ok(ResourceLease {
            inner: Arc::new(LeaseInner {
                info: ResourceLeaseInfo {
                    domain: ledger.domain,
                    owner,
                    allocation: info.allocation,
                    kind: info.kind,
                    bytes: info.bytes,
                },
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
            .field("info", &self.inner.info)
            .finish()
    }
}

impl ResourceLease {
    pub fn info(&self) -> ResourceLeaseInfo {
        self.inner.info
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

    /// Explicitly release this handle. Other clones or owners remain charged.
    pub fn release(self) {
        drop(self);
    }
}

struct LeaseInner {
    info: ResourceLeaseInfo,
    ledger: Arc<Mutex<ResourceLedger>>,
}

impl Drop for LeaseInner {
    fn drop(&mut self) {
        let mut ledger = self
            .ledger
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        ledger.remove_reference(self.info.owner, self.info.allocation);
    }
}
