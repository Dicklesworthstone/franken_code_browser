//! Display-link pacing and single drawable ownership (FCB-006.A).
//!
//! Apple's `CAMetalDisplayLink` provides display-synchronized updates and
//! frame-rate/latency controls on macOS 14+. Exactly one presentation owner
//! acquires and presents each drawable.
//!
//! Pacing enforces:
//! 1. Verified SDK callback route and availability (CAMetalDisplayLink vs qualified fallback).
//! 2. Two-frame starting policy (at most 2 frames in-flight, preventing latency spikes).
//! 3. Single drawable owner with controlled acquisition and competing presentation rejection.
//! 4. Non-blocking teardown: invalidation never stalls or waits on the event thread.
//! 5. Stationary pausing: static source windows avoid continuous idle redraw.

#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};

use crate::view_binding::{
    NativeDrawable, NativeViewBinding, PresentationOwner, ViewBindingError, ViewBindingId,
};
use crate::{BridgeError, MainThreadToken, ffi};

static PACING_NONCE: AtomicU64 = AtomicU64::new(1);

/// Default maximum in-flight frames per the two-frame starting policy (§15.2).
pub const DEFAULT_MAX_IN_FLIGHT: usize = 2;

/// Structured event ring capacity.
pub const PACING_EVENT_RING_CAPACITY: usize = 64;

/// Display link delivery route.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DisplayLinkRoute {
    /// Apple's native `CAMetalDisplayLink` on macOS 14+.
    CAMetalDisplayLink,
    /// Qualified synthetic pacing for headless or controlled testing.
    ManualPacing,
}

/// Report on SDK callback route and availability.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DisplayLinkAvailability {
    pub is_available: bool,
    pub route: DisplayLinkRoute,
}

impl DisplayLinkAvailability {
    /// Probe SDK availability on the host platform.
    pub fn probe() -> Self {
        if ffi::display_link_available() {
            Self {
                is_available: true,
                route: DisplayLinkRoute::CAMetalDisplayLink,
            }
        } else {
            Self {
                is_available: false,
                route: DisplayLinkRoute::ManualPacing,
            }
        }
    }
}

/// Pacing engine configuration.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PacingConfig {
    pub max_in_flight: usize,
    pub target_fps: u32,
    pub preferred_latency_frames: u32,
}

impl Default for PacingConfig {
    fn default() -> Self {
        Self {
            max_in_flight: DEFAULT_MAX_IN_FLIGHT,
            target_fps: 60,
            preferred_latency_frames: 2,
        }
    }
}

impl PacingConfig {
    pub fn validate(&self) -> Result<(), PacingError> {
        if self.max_in_flight == 0 || self.max_in_flight > 3 {
            return Err(PacingError::InvalidConfig);
        }
        if self.target_fps == 0 || self.target_fps > 240 {
            return Err(PacingError::InvalidConfig);
        }
        if self.preferred_latency_frames == 0 || self.preferred_latency_frames > 3 {
            return Err(PacingError::InvalidConfig);
        }
        Ok(())
    }
}

/// Structured events emitted by the display-link pacing engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacingEventType {
    Created,
    Tick,
    FrameAcquired,
    FramePresented,
    FrameDropped,
    DeferredFullQueue,
    Paused,
    Resumed,
    Invalidated,
    RejectedCompetingOwner,
    RejectedStaleOwner,
}

/// One record in the structured pacing event ring.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct PacingEventRecord {
    pub pacing_id: u64,
    pub frame_id: u64,
    pub in_flight: usize,
    pub event_type: PacingEventType,
}

#[derive(Debug)]
struct EventRing {
    entries: Vec<PacingEventRecord>,
    capacity: usize,
    write_pos: usize,
    total_events: u64,
}

impl EventRing {
    fn new(capacity: usize) -> Self {
        Self {
            entries: Vec::with_capacity(capacity),
            capacity,
            write_pos: 0,
            total_events: 0,
        }
    }

    fn record(&mut self, record: PacingEventRecord) {
        self.total_events = self.total_events.saturating_add(1);
        if self.entries.len() < self.capacity {
            self.entries.push(record);
        } else {
            self.entries[self.write_pos] = record;
            self.write_pos = (self.write_pos + 1) % self.capacity;
        }
    }

    fn records(&self) -> Vec<PacingEventRecord> {
        if self.entries.len() < self.capacity {
            self.entries.clone()
        } else {
            let mut out = Vec::with_capacity(self.capacity);
            for i in 0..self.capacity {
                let idx = (self.write_pos + i) % self.capacity;
                out.push(self.entries[idx]);
            }
            out
        }
    }
}

/// Aggregate counters for pacing operations.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct PacingCounters {
    pub ticks: u64,
    pub frames_acquired: u64,
    pub frames_presented: u64,
    pub frames_dropped: u64,
    pub deferred_full_queue: u64,
    pub pauses: u64,
    pub resumes: u64,
    pub invalidations: u64,
    pub rejected_competing_owner: u64,
    pub rejected_stale_owner: u64,
}

/// Errors from the display-link pacing engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PacingError {
    WrongThread,
    NotMainThread,
    InvalidConfig,
    NativeUnavailable,
    StalePresentationOwner,
    CompetingPresentationOwner,
    QueueFull,
    Invalidated,
    DrawableUnavailable,
    BindingNotAttached,
}

impl From<ViewBindingError> for PacingError {
    fn from(error: ViewBindingError) -> Self {
        match error {
            ViewBindingError::WrongThread => Self::WrongThread,
            ViewBindingError::NotMainThread => Self::NotMainThread,
            ViewBindingError::InvalidConfig => Self::InvalidConfig,
            ViewBindingError::StalePresentationOwner => Self::StalePresentationOwner,
            ViewBindingError::PresentationOwnerTaken => Self::CompetingPresentationOwner,
            ViewBindingError::NotBound => Self::BindingNotAttached,
            ViewBindingError::DrawableUnavailable => Self::DrawableUnavailable,
            ViewBindingError::NativeUnavailable => Self::NativeUnavailable,
            _ => Self::NativeUnavailable,
        }
    }
}

impl From<BridgeError> for PacingError {
    fn from(error: BridgeError) -> Self {
        match error {
            BridgeError::NotMainThread => Self::NotMainThread,
            BridgeError::WrongThread => Self::WrongThread,
            BridgeError::UnsupportedPlatform
            | BridgeError::NullNativeObject
            | BridgeError::RetainFailed => Self::NativeUnavailable,
        }
    }
}

/// Outcome of a display-link tick step.
#[derive(Debug)]
pub enum PacingOutcome {
    /// A new frame was successfully acquired and started.
    FrameStarted(PacedFrame),
    /// Frame start was deferred because in-flight frames reached maximum capacity.
    DeferredFullQueue,
    /// Pacing was skipped because the engine is paused.
    Paused,
    /// Pacing was skipped because no render/update was requested.
    Idle,
}

/// An in-flight paced frame tied to exactly one presentation owner.
///
/// If dropped before calling [`present`], it decrements the in-flight
/// count cleanly without deadlocking the pacing queue.
#[derive(Debug)]
pub struct PacedFrame {
    frame_id: u64,
    pacing_id: u64,
    target_timestamp: f64,
    target_presentation_timestamp: f64,
    drawable: Option<NativeDrawable>,
    presented: bool,
    in_flight_ref: Rc<Cell<usize>>,
}

impl PacedFrame {
    pub const fn frame_id(&self) -> u64 {
        self.frame_id
    }

    pub const fn target_timestamp(&self) -> f64 {
        self.target_timestamp
    }

    pub const fn target_presentation_timestamp(&self) -> f64 {
        self.target_presentation_timestamp
    }

    pub fn is_presented(&self) -> bool {
        self.presented
    }

    /// Present the drawable to display. Only the live presentation owner
    /// bound to this engine may present.
    pub fn present(mut self, engine: &DisplayPacingEngine) -> Result<(), PacingError> {
        if self.presented {
            return Ok(());
        }
        if engine.id != self.pacing_id {
            engine.bump_counter(|c| c.rejected_competing_owner += 1);
            engine.record_event(self.frame_id, PacingEventType::RejectedCompetingOwner);
            return Err(PacingError::CompetingPresentationOwner);
        }
        if let Some(drawable) = self.drawable.take() {
            drawable.present();
            self.presented = true;
            self.in_flight_ref
                .set(self.in_flight_ref.get().saturating_sub(1));
            engine.record_frame_presented(self.frame_id);
        }
        Ok(())
    }
}

impl Drop for PacedFrame {
    fn drop(&mut self) {
        if !self.presented {
            self.in_flight_ref
                .set(self.in_flight_ref.get().saturating_sub(1));
        }
    }
}

/// Host-owned display link pacing engine.
///
/// Binds display pacing to exactly one presentation owner with an explicit
/// two-frame starting policy (§15.2). Teardown is non-blocking.
#[derive(Debug)]
pub struct DisplayPacingEngine {
    id: u64,
    token: MainThreadToken,
    binding_id: ViewBindingId,
    config: PacingConfig,
    route: DisplayLinkRoute,
    link_object: Option<ffi::OwnedObject>,
    owner: Cell<PresentationOwner>,
    in_flight: Rc<Cell<usize>>,
    frame_sequence: Cell<u64>,
    paused: Cell<bool>,
    needs_render: Cell<bool>,
    invalidated: Cell<bool>,
    counters: Cell<PacingCounters>,
    event_ring: RefCell<EventRing>,
}

impl DisplayPacingEngine {
    /// Bind display pacing to a native view binding with exclusive presentation ownership.
    pub fn bind(
        token: MainThreadToken,
        binding: &NativeViewBinding,
        owner: PresentationOwner,
        config: PacingConfig,
    ) -> Result<Self, PacingError> {
        token.assert_current()?;
        config.validate()?;
        if owner.binding_id() != binding.id() || owner.generation() != binding.generation() {
            return Err(PacingError::StalePresentationOwner);
        }
        if !binding.is_attached() {
            return Err(PacingError::BindingNotAttached);
        }

        let availability = DisplayLinkAvailability::probe();
        let link_object = None;

        let id = PACING_NONCE.fetch_add(1, Ordering::Relaxed);
        let mut event_ring = EventRing::new(PACING_EVENT_RING_CAPACITY);
        event_ring.record(PacingEventRecord {
            pacing_id: id,
            frame_id: 0,
            in_flight: 0,
            event_type: PacingEventType::Created,
        });

        Ok(Self {
            id,
            token,
            binding_id: binding.id(),
            config,
            route: availability.route,
            link_object,
            owner: Cell::new(owner),
            in_flight: Rc::new(Cell::new(0)),
            frame_sequence: Cell::new(0),
            paused: Cell::new(false),
            needs_render: Cell::new(true),
            invalidated: Cell::new(false),
            counters: Cell::new(PacingCounters::default()),
            event_ring: RefCell::new(event_ring),
        })
    }

    pub const fn id(&self) -> u64 {
        self.id
    }

    pub const fn route(&self) -> DisplayLinkRoute {
        self.route
    }

    pub const fn config(&self) -> PacingConfig {
        self.config
    }

    pub fn is_paused(&self) -> bool {
        self.paused.get()
    }

    pub fn is_invalidated(&self) -> bool {
        self.invalidated.get()
    }

    pub fn in_flight_count(&self) -> usize {
        self.in_flight.get()
    }

    pub fn counters(&self) -> PacingCounters {
        self.counters.get()
    }

    pub fn event_records(&self) -> Vec<PacingEventRecord> {
        self.event_ring.borrow().records()
    }

    pub fn event_count(&self) -> u64 {
        self.event_ring.borrow().total_events
    }

    /// Mark that new content or interaction requires a rendered frame.
    pub fn request_frame(&self) {
        self.needs_render.set(true);
    }

    /// Pause the display link (e.g. window occluded or static source).
    pub fn pause(&self) {
        if !self.paused.get() {
            self.paused.set(true);
            if let Some(ref link) = self.link_object {
                ffi::display_link_set_paused(link.as_ref(), true);
            }
            self.bump_counter(|c| c.pauses += 1);
            self.record_event(0, PacingEventType::Paused);
        }
    }

    /// Resume the display link from stationary pause.
    pub fn resume(&self) {
        if self.paused.get() {
            self.paused.set(false);
            self.needs_render.set(true);
            if let Some(ref link) = self.link_object {
                ffi::display_link_set_paused(link.as_ref(), false);
            }
            self.bump_counter(|c| c.resumes += 1);
            self.record_event(0, PacingEventType::Resumed);
        }
    }

    /// Update the presentation owner (e.g. after re-attachment with new generation).
    pub fn update_owner(
        &self,
        owner: PresentationOwner,
        binding: &NativeViewBinding,
    ) -> Result<(), PacingError> {
        if owner.binding_id() != self.binding_id
            || owner.binding_id() != binding.id()
            || owner.generation() != binding.generation()
        {
            self.bump_counter(|c| c.rejected_competing_owner += 1);
            self.record_event(0, PacingEventType::RejectedCompetingOwner);
            return Err(PacingError::CompetingPresentationOwner);
        }
        self.owner.set(owner);
        Ok(())
    }

    /// Non-blocking teardown: invalidates the display link without stalling the event thread.
    pub fn invalidate(&self) {
        if !self.invalidated.get() {
            self.invalidated.set(true);
            if let Some(ref link) = self.link_object {
                ffi::display_link_invalidate(link.as_ref());
            }
            self.bump_counter(|c| c.invalidations += 1);
            self.record_event(0, PacingEventType::Invalidated);
        }
    }

    /// Execute one pacing tick.
    ///
    /// Implements the two-frame starting policy (§15.2): if `in_flight >= max_in_flight`,
    /// frame start is deferred without acquiring a drawable or touching the GPU.
    pub fn tick(
        &self,
        token: MainThreadToken,
        binding: &NativeViewBinding,
        target_timestamp: f64,
        target_presentation_timestamp: f64,
    ) -> Result<PacingOutcome, PacingError> {
        token.assert_current()?;
        if token != self.token {
            return Err(PacingError::WrongThread);
        }
        if self.invalidated.get() {
            return Err(PacingError::Invalidated);
        }

        self.bump_counter(|c| c.ticks += 1);
        let frame_id = self.frame_sequence.get().wrapping_add(1).max(1);

        if self.paused.get() {
            return Ok(PacingOutcome::Paused);
        }

        if !self.needs_render.get() {
            return Ok(PacingOutcome::Idle);
        }

        // Two-frame starting policy check:
        if self.in_flight.get() >= self.config.max_in_flight {
            self.bump_counter(|c| c.deferred_full_queue += 1);
            self.record_event(frame_id, PacingEventType::DeferredFullQueue);
            return Ok(PacingOutcome::DeferredFullQueue);
        }

        // Single drawable owner check:
        let owner = self.owner.get();
        if owner.binding_id() != binding.id() || owner.generation() != binding.generation() {
            self.bump_counter(|c| c.rejected_stale_owner += 1);
            self.record_event(frame_id, PacingEventType::RejectedStaleOwner);
            return Err(PacingError::StalePresentationOwner);
        }

        // Controlled acquisition via the single presentation owner:
        let drawable = binding.acquire_drawable(token, owner)?;
        self.in_flight.set(self.in_flight.get() + 1);
        self.frame_sequence.set(frame_id);
        self.needs_render.set(false);

        self.bump_counter(|c| c.frames_acquired += 1);
        self.record_event(frame_id, PacingEventType::FrameAcquired);

        Ok(PacingOutcome::FrameStarted(PacedFrame {
            frame_id,
            pacing_id: self.id,
            target_timestamp,
            target_presentation_timestamp,
            drawable: Some(drawable),
            presented: false,
            in_flight_ref: Rc::clone(&self.in_flight),
        }))
    }

    /// Record a frame drop (e.g. obsolete frame discarded before submission).
    pub fn record_frame_dropped(&self, frame_id: u64) {
        self.bump_counter(|c| c.frames_dropped += 1);
        self.record_event(frame_id, PacingEventType::FrameDropped);
    }

    fn record_frame_presented(&self, frame_id: u64) {
        self.bump_counter(|c| c.frames_presented += 1);
        self.record_event(frame_id, PacingEventType::FramePresented);
    }

    fn bump_counter(&self, f: impl FnOnce(&mut PacingCounters)) {
        let mut counters = self.counters.get();
        f(&mut counters);
        self.counters.set(counters);
    }

    fn record_event(&self, frame_id: u64, event_type: PacingEventType) {
        self.event_ring.borrow_mut().record(PacingEventRecord {
            pacing_id: self.id,
            frame_id,
            in_flight: self.in_flight.get(),
            event_type,
        });
    }
}

impl Drop for DisplayPacingEngine {
    fn drop(&mut self) {
        self.invalidate();
    }
}
