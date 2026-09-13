//! Owner-qualified generational handles for session and device-local resources.
//!
//! The slot and generation identify a resource only within one owner domain.
//! These wrappers keep the domain identity in every handle and make lookup
//! validate the receiving table before it inspects the slot.  A generation
//! reaching its configured limit retires the slot; it is never wrapped back to
//! an earlier generation.

use crate::{ArenaOwnerId, CoreError, DeviceGeneration, DeviceId};
use std::sync::atomic::{AtomicU64, Ordering};

const DEFAULT_MAX_SLOTS: u64 = u64::MAX;
const DEFAULT_MAX_GENERATION: u64 = u64::MAX;

/// Bounds for one handle domain.
///
/// The default is the representable `u64` domain.  Smaller bounds are useful
/// for hosts that reserve a finite identity budget and make exhaustion a
/// deterministic, testable state transition rather than an implicit wrap.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct HandleLimits {
    max_slots: u64,
    max_generation: u64,
}

impl HandleLimits {
    /// Create bounds with at least one slot and one usable generation.
    pub const fn new(max_slots: u64, max_generation: u64) -> Result<Self, CoreError> {
        if max_slots == 0 || max_generation == 0 {
            Err(CoreError::InvalidId)
        } else {
            Ok(Self {
                max_slots,
                max_generation,
            })
        }
    }

    pub const fn max_slots(self) -> u64 {
        self.max_slots
    }

    pub const fn max_generation(self) -> u64 {
        self.max_generation
    }
}

impl Default for HandleLimits {
    fn default() -> Self {
        Self {
            max_slots: DEFAULT_MAX_SLOTS,
            max_generation: DEFAULT_MAX_GENERATION,
        }
    }
}

/// Identity of the receiving arena/table, separate from its browser and device
/// identities.  A fresh value is allocated for every table, including a table
/// recreated for the same owner and device.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct TableDomainId(u64);

static NEXT_TABLE_DOMAIN: AtomicU64 = AtomicU64::new(1);

impl TableDomainId {
    /// Validate an explicitly transported table-domain value.
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

    fn allocate() -> Result<Self, CoreError> {
        let mut current = NEXT_TABLE_DOMAIN.load(Ordering::Relaxed);
        loop {
            if current == 0 {
                return Err(CoreError::Exhausted);
            }
            let next = current.checked_add(1).unwrap_or(0);
            match NEXT_TABLE_DOMAIN.compare_exchange_weak(
                current,
                next,
                Ordering::Relaxed,
                Ordering::Relaxed,
            ) {
                Ok(_) => return Ok(Self(current)),
                Err(observed) => current = observed,
            }
        }
    }
}

/// A slot/generation handle qualified by its owning instance or arena.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArenaHandle {
    owner: ArenaOwnerId,
    domain: TableDomainId,
    slot: u64,
    generation: u64,
}

impl ArenaHandle {
    /// Construct an unbound handle for legacy/raw validation paths.
    ///
    /// Handles returned by an [`ArenaTable`] carry its non-zero table domain;
    /// an unbound handle is intentionally rejected by every table.
    pub const fn new(
        owner: ArenaOwnerId,
        slot: u64,
        generation: u64,
    ) -> Result<Self, CoreError> {
        Self::with_domain(owner, TableDomainId(0), slot, generation)
    }

    /// Construct a handle from a transported table-domain value.
    pub const fn from_parts(
        owner: ArenaOwnerId,
        domain: TableDomainId,
        slot: u64,
        generation: u64,
    ) -> Result<Self, CoreError> {
        Self::with_domain(owner, domain, slot, generation)
    }

    const fn with_domain(
        owner: ArenaOwnerId,
        domain: TableDomainId,
        slot: u64,
        generation: u64,
    ) -> Result<Self, CoreError> {
        if generation == 0 {
            Err(CoreError::InvalidId)
        } else {
            Ok(Self {
                owner,
                domain,
                slot,
                generation,
            })
        }
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn domain(self) -> TableDomainId {
        self.domain
    }

    pub const fn slot(self) -> u64 {
        self.slot
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }
}

/// A device resource handle qualified by arena owner, physical device, and
/// device resource-lifetime generation in addition to its reused slot.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct DeviceHandle {
    owner: ArenaOwnerId,
    domain: TableDomainId,
    device: DeviceId,
    device_generation: DeviceGeneration,
    slot: u64,
    generation: u64,
}

impl DeviceHandle {
    /// Construct an unbound handle for legacy/raw validation paths.
    ///
    /// Handles returned by a [`DeviceTable`] carry its non-zero table domain;
    /// an unbound handle is intentionally rejected by every table.
    pub fn new(
        owner: ArenaOwnerId,
        device: DeviceId,
        device_generation: DeviceGeneration,
        slot: u64,
        generation: u64,
    ) -> Result<Self, CoreError> {
        Self::with_domain(
            owner,
            TableDomainId(0),
            device,
            device_generation,
            slot,
            generation,
        )
    }

    /// Construct a handle from a transported table-domain value.
    pub fn from_parts(
        owner: ArenaOwnerId,
        domain: TableDomainId,
        device: DeviceId,
        device_generation: DeviceGeneration,
        slot: u64,
        generation: u64,
    ) -> Result<Self, CoreError> {
        Self::with_domain(
            owner,
            domain,
            device,
            device_generation,
            slot,
            generation,
        )
    }

    fn with_domain(
        owner: ArenaOwnerId,
        domain: TableDomainId,
        device: DeviceId,
        device_generation: DeviceGeneration,
        slot: u64,
        generation: u64,
    ) -> Result<Self, CoreError> {
        if device.owner() != owner || device_generation.owner() != owner {
            return Err(CoreError::OwnershipMismatch);
        }
        if generation == 0 {
            return Err(CoreError::InvalidId);
        }
        Ok(Self {
            owner,
            domain,
            device,
            device_generation,
            slot,
            generation,
        })
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn domain(self) -> TableDomainId {
        self.domain
    }

    pub const fn device(self) -> DeviceId {
        self.device
    }

    pub const fn device_generation(self) -> DeviceGeneration {
        self.device_generation
    }

    pub const fn slot(self) -> u64 {
        self.slot
    }

    pub const fn generation(self) -> u64 {
        self.generation
    }
}

// This narrow FCB adapter intentionally mirrors the checked generation and
// free-slot semantics of FrankenThreeD's f3d-core::Arena.  Direct reuse is not
// possible here: f3d-core has no receiving-table domain, uses u32/non-zero
// handles and HandleError, and has no FCB HandleLimits/CoreError contract.
enum SlotState<T> {
    Occupied { generation: u64, value: T },
    Vacant { generation: u64 },
    Retired,
}

struct SlotTable<T> {
    limits: HandleLimits,
    slots: Vec<SlotState<T>>,
    free_slots: Vec<u64>,
    live_count: usize,
}

impl<T> SlotTable<T> {
    fn new(limits: HandleLimits) -> Self {
        Self {
            limits,
            slots: Vec::new(),
            free_slots: Vec::new(),
            live_count: 0,
        }
    }

    fn insert(&mut self, value: T) -> Result<(u64, u64), CoreError> {
        let (slot, generation) = if let Some(slot) = self.free_slots.pop() {
            let slot_index = usize::try_from(slot).map_err(|_| CoreError::Exhausted)?;
            let state = self
                .slots
                .get(slot_index)
                .ok_or(CoreError::Exhausted)?;
            let SlotState::Vacant { generation } = state else {
                return Err(CoreError::Exhausted);
            };
            let generation = generation.checked_add(1).ok_or(CoreError::Exhausted)?;
            if generation > self.limits.max_generation() {
                return Err(CoreError::Exhausted);
            }
            (slot_index, generation)
        } else {
            let slot_count = u64::try_from(self.slots.len()).map_err(|_| CoreError::Exhausted)?;
            if slot_count >= self.limits.max_slots() {
                return Err(CoreError::Exhausted);
            }
            self.slots.push(SlotState::Vacant { generation: 0 });
            (self.slots.len() - 1, 1)
        };

        self.slots[slot] = SlotState::Occupied { generation, value };
        self.live_count = self.live_count.checked_add(1).ok_or(CoreError::Exhausted)?;
        Ok((u64::try_from(slot).map_err(|_| CoreError::Exhausted)?, generation))
    }

    fn state(&self, slot: u64) -> Result<&SlotState<T>, CoreError> {
        if slot > usize::MAX as u64 {
            return Err(CoreError::InvalidId);
        }
        self.slots
            .get(slot as usize)
            .ok_or(CoreError::StalePublication)
    }

    fn state_mut(&mut self, slot: u64) -> Result<&mut SlotState<T>, CoreError> {
        if slot > usize::MAX as u64 {
            return Err(CoreError::InvalidId);
        }
        self.slots
            .get_mut(slot as usize)
            .ok_or(CoreError::StalePublication)
    }

    fn lookup(&self, slot: u64, generation: u64) -> Result<&T, CoreError> {
        if generation == 0 {
            return Err(CoreError::InvalidId);
        }
        match self.state(slot)? {
            SlotState::Occupied {
                generation: current,
                value,
            } if *current == generation => Ok(value),
            _ => Err(CoreError::StalePublication),
        }
    }

    fn lookup_mut(&mut self, slot: u64, generation: u64) -> Result<&mut T, CoreError> {
        if generation == 0 {
            return Err(CoreError::InvalidId);
        }
        match self.state_mut(slot)? {
            SlotState::Occupied {
                generation: current,
                value,
            } if *current == generation => Ok(value),
            _ => Err(CoreError::StalePublication),
        }
    }

    fn remove(&mut self, slot: u64, generation: u64) -> Result<T, CoreError> {
        if generation == 0 {
            return Err(CoreError::InvalidId);
        }
        let previous = {
            let state = self.state_mut(slot)?;
            std::mem::replace(state, SlotState::Retired)
        };
        match previous {
            SlotState::Occupied {
                generation: current,
                value,
            } if current == generation => {
                let next_generation = generation.checked_add(1);
                match next_generation {
                    Some(next) if next <= self.limits.max_generation() => {
                        let state = self.state_mut(slot)?;
                        *state = SlotState::Vacant { generation };
                        self.free_slots.push(slot);
                    }
                    _ => {
                        let state = self.state_mut(slot)?;
                        *state = SlotState::Retired;
                    }
                }
                self.live_count -= 1;
                Ok(value)
            }
            previous => {
                let state = self.state_mut(slot)?;
                *state = previous;
                Err(CoreError::StalePublication)
            }
        }
    }

    fn len(&self) -> usize {
        self.live_count
    }
}

/// Owner-qualified arena-local resource table.
pub struct ArenaTable<T> {
    owner: ArenaOwnerId,
    domain: TableDomainId,
    slots: SlotTable<T>,
}

impl<T> ArenaTable<T> {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self::with_limits(owner, HandleLimits::default())
    }

    pub fn try_new(owner: ArenaOwnerId) -> Result<Self, CoreError> {
        Self::try_with_limits(owner, HandleLimits::default())
    }

    pub fn with_limits(owner: ArenaOwnerId, limits: HandleLimits) -> Self {
        Self::try_with_limits(owner, limits)
            .expect("table-domain identity exhausted while creating arena table")
    }

    pub fn try_with_limits(owner: ArenaOwnerId, limits: HandleLimits) -> Result<Self, CoreError> {
        Ok(Self {
            owner,
            domain: TableDomainId::allocate()?,
            slots: SlotTable::new(limits),
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn domain(&self) -> TableDomainId {
        self.domain
    }

    pub fn insert(&mut self, value: T) -> Result<ArenaHandle, CoreError> {
        let (slot, generation) = self.slots.insert(value)?;
        ArenaHandle::with_domain(self.owner, self.domain, slot, generation)
    }

    pub fn validate(&self, handle: ArenaHandle) -> Result<(), CoreError> {
        if handle.owner() != self.owner || handle.domain() != self.domain {
            return Err(CoreError::OwnershipMismatch);
        }
        self.slots.lookup(handle.slot(), handle.generation()).map(|_| ())
    }

    pub fn lookup(&self, handle: ArenaHandle) -> Result<&T, CoreError> {
        self.validate(handle)?;
        self.slots.lookup(handle.slot(), handle.generation())
    }

    pub fn lookup_mut(&mut self, handle: ArenaHandle) -> Result<&mut T, CoreError> {
        if handle.owner() != self.owner || handle.domain() != self.domain {
            return Err(CoreError::OwnershipMismatch);
        }
        self.slots.lookup_mut(handle.slot(), handle.generation())
    }

    pub fn remove(&mut self, handle: ArenaHandle) -> Result<T, CoreError> {
        if handle.owner() != self.owner || handle.domain() != self.domain {
            return Err(CoreError::OwnershipMismatch);
        }
        self.slots.remove(handle.slot(), handle.generation())
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Device-local resource table with explicit device-generation authority.
pub struct DeviceTable<T> {
    owner: ArenaOwnerId,
    domain: TableDomainId,
    device: DeviceId,
    device_generation: DeviceGeneration,
    slots: SlotTable<T>,
}

impl<T> DeviceTable<T> {
    pub fn new(
        owner: ArenaOwnerId,
        device: DeviceId,
        device_generation: DeviceGeneration,
    ) -> Result<Self, CoreError> {
        Self::with_limits(owner, device, device_generation, HandleLimits::default())
    }

    pub fn with_limits(
        owner: ArenaOwnerId,
        device: DeviceId,
        device_generation: DeviceGeneration,
        limits: HandleLimits,
    ) -> Result<Self, CoreError> {
        if device.owner() != owner || device_generation.owner() != owner {
            return Err(CoreError::OwnershipMismatch);
        }
        Ok(Self {
            owner,
            domain: TableDomainId::allocate()?,
            device,
            device_generation,
            slots: SlotTable::new(limits),
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn domain(&self) -> TableDomainId {
        self.domain
    }

    pub const fn device(&self) -> DeviceId {
        self.device
    }

    pub const fn device_generation(&self) -> DeviceGeneration {
        self.device_generation
    }

    pub fn insert(&mut self, value: T) -> Result<DeviceHandle, CoreError> {
        let (slot, generation) = self.slots.insert(value)?;
        DeviceHandle::with_domain(
            self.owner,
            self.domain,
            self.device,
            self.device_generation,
            slot,
            generation,
        )
    }

    pub fn validate(&self, handle: DeviceHandle) -> Result<(), CoreError> {
        if handle.owner() != self.owner
            || handle.domain() != self.domain
            || handle.device() != self.device
        {
            return Err(CoreError::OwnershipMismatch);
        }
        if handle.device_generation() != self.device_generation {
            return Err(CoreError::StalePublication);
        }
        self.slots.lookup(handle.slot(), handle.generation()).map(|_| ())
    }

    pub fn lookup(&self, handle: DeviceHandle) -> Result<&T, CoreError> {
        self.validate(handle)?;
        self.slots.lookup(handle.slot(), handle.generation())
    }

    pub fn lookup_mut(&mut self, handle: DeviceHandle) -> Result<&mut T, CoreError> {
        self.validate(handle)?;
        self.slots.lookup_mut(handle.slot(), handle.generation())
    }

    pub fn remove(&mut self, handle: DeviceHandle) -> Result<T, CoreError> {
        self.validate(handle)?;
        self.slots.remove(handle.slot(), handle.generation())
    }

    pub fn len(&self) -> usize {
        self.slots.len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
