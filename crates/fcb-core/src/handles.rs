//! Owner-qualified generational handles for session and device-local resources.
//!
//! The slot and generation identify a resource only within one owner domain.
//! These wrappers keep the domain identity in every handle and make lookup
//! validate the receiving table before it inspects the slot.  A generation
//! reaching its configured limit retires the slot; it is never wrapped back to
//! an earlier generation.

use crate::{ArenaOwnerId, CoreError, DeviceGeneration, DeviceId};

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

/// A slot/generation handle qualified by its owning instance or arena.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ArenaHandle {
    owner: ArenaOwnerId,
    slot: u64,
    generation: u64,
}

impl ArenaHandle {
    pub const fn new(
        owner: ArenaOwnerId,
        slot: u64,
        generation: u64,
    ) -> Result<Self, CoreError> {
        if generation == 0 {
            Err(CoreError::InvalidId)
        } else {
            Ok(Self {
                owner,
                slot,
                generation,
            })
        }
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
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
    device: DeviceId,
    device_generation: DeviceGeneration,
    slot: u64,
    generation: u64,
}

impl DeviceHandle {
    pub fn new(
        owner: ArenaOwnerId,
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
            device,
            device_generation,
            slot,
            generation,
        })
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
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

enum SlotState<T> {
    Occupied { generation: u64, value: T },
    Vacant { generation: u64 },
    Retired,
}

struct SlotTable<T> {
    limits: HandleLimits,
    slots: Vec<SlotState<T>>,
}

impl<T> SlotTable<T> {
    fn new(limits: HandleLimits) -> Self {
        Self {
            limits,
            slots: Vec::new(),
        }
    }

    fn insert(&mut self, value: T) -> Result<(u64, u64), CoreError> {
        let mut reusable = None;
        for (slot, state) in self.slots.iter_mut().enumerate() {
            let SlotState::Vacant { generation } = state else {
                continue;
            };
            let Some(next_generation) = generation.checked_add(1) else {
                *state = SlotState::Retired;
                continue;
            };
            if next_generation > self.limits.max_generation() {
                *state = SlotState::Retired;
                continue;
            }
            reusable = Some((slot, next_generation));
            break;
        }

        let (slot, generation) = if let Some((slot, generation)) = reusable {
            (slot, generation)
        } else {
            let slot_count = u64::try_from(self.slots.len()).map_err(|_| CoreError::Exhausted)?;
            if slot_count >= self.limits.max_slots() {
                return Err(CoreError::Exhausted);
            }
            self.slots.push(SlotState::Vacant { generation: 0 });
            (self.slots.len() - 1, 1)
        };

        self.slots[slot] = SlotState::Occupied { generation, value };
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
        let state = self.state_mut(slot)?;
        let previous = std::mem::replace(state, SlotState::Retired);
        match previous {
            SlotState::Occupied {
                generation: current,
                value,
            } if current == generation => {
                *state = SlotState::Vacant { generation };
                Ok(value)
            }
            previous => {
                *state = previous;
                Err(CoreError::StalePublication)
            }
        }
    }

    fn len(&self) -> usize {
        self.slots
            .iter()
            .filter(|state| matches!(state, SlotState::Occupied { .. }))
            .count()
    }
}

/// Owner-qualified arena-local resource table.
pub struct ArenaTable<T> {
    owner: ArenaOwnerId,
    slots: SlotTable<T>,
}

impl<T> ArenaTable<T> {
    pub fn new(owner: ArenaOwnerId) -> Self {
        Self::with_limits(owner, HandleLimits::default())
    }

    pub fn with_limits(owner: ArenaOwnerId, limits: HandleLimits) -> Self {
        Self {
            owner,
            slots: SlotTable::new(limits),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn insert(&mut self, value: T) -> Result<ArenaHandle, CoreError> {
        let (slot, generation) = self.slots.insert(value)?;
        ArenaHandle::new(self.owner, slot, generation)
    }

    pub fn validate(&self, handle: ArenaHandle) -> Result<(), CoreError> {
        if handle.owner() != self.owner {
            return Err(CoreError::OwnershipMismatch);
        }
        self.slots.lookup(handle.slot(), handle.generation()).map(|_| ())
    }

    pub fn lookup(&self, handle: ArenaHandle) -> Result<&T, CoreError> {
        self.validate(handle)?;
        self.slots.lookup(handle.slot(), handle.generation())
    }

    pub fn lookup_mut(&mut self, handle: ArenaHandle) -> Result<&mut T, CoreError> {
        if handle.owner() != self.owner {
            return Err(CoreError::OwnershipMismatch);
        }
        self.slots.lookup_mut(handle.slot(), handle.generation())
    }

    pub fn remove(&mut self, handle: ArenaHandle) -> Result<T, CoreError> {
        if handle.owner() != self.owner {
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
            device,
            device_generation,
            slots: SlotTable::new(limits),
        })
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn device(&self) -> DeviceId {
        self.device
    }

    pub const fn device_generation(&self) -> DeviceGeneration {
        self.device_generation
    }

    pub fn insert(&mut self, value: T) -> Result<DeviceHandle, CoreError> {
        let (slot, generation) = self.slots.insert(value)?;
        DeviceHandle::new(
            self.owner,
            self.device,
            self.device_generation,
            slot,
            generation,
        )
    }

    pub fn validate(&self, handle: DeviceHandle) -> Result<(), CoreError> {
        if handle.owner() != self.owner || handle.device() != self.device {
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
