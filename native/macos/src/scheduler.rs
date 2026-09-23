//! Demand-driven frame scheduling and pre-submission camera coalescing (FCB-006.B).
//!
//! Enforces:
//! 1. Demand-driven rendering: renders only occur when explicitly requested
//!    (camera motion, cursor blink, content change, resize, animation).
//! 2. Static and occluded pausing: hidden or occluded windows pause render loops
//!    while keeping asynchronous GPU completions live (§15.4).
//! 3. Obsolete camera frame dropping: intermediate camera states during rapid
//!    navigation (e.g. 120 Hz panning) can be dropped before GPU submission,
//!    preventing queue latency bloat without losing discrete user commands (§15.2).
//! 4. Resilient handling of unavailable drawables and display scale migrations (§14.7).
//! 5. Bounded structured event logging and aggregate counters.

#![forbid(unsafe_code)]

use std::cell::{Cell, RefCell};

use crate::MainThreadToken;
use crate::pacing::{DisplayPacingEngine, PacedFrame, PacingError, PacingOutcome};
use crate::view_binding::NativeViewBinding;

/// Capacity of the scheduler event ring.
pub const SCHEDULER_EVENT_RING_CAPACITY: usize = 64;

/// Reason why a frame render was requested.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FrameRequestReason {
    /// Continuous camera movement (e.g. mouse drag, inertia, zooming). Coalescable.
    CameraMotion,
    /// Discrete user command or typing. Must not be arbitrarily discarded.
    UserInput,
    /// Cursor blink timer tick. Viewport geometry unchanged.
    CursorBlink,
    /// Background indexing or source analysis completed.
    ContentUpdate,
    /// Window was resized or moved across displays.
    WindowResize,
    /// Display backing scale or color profile changed.
    DisplayMigration,
    /// Active UI animation running.
    Animation,
}

impl FrameRequestReason {
    /// Whether frames requested for this reason can be superseded by newer camera states.
    pub const fn is_coalescable(&self) -> bool {
        matches!(
            self,
            Self::CameraMotion | Self::CursorBlink | Self::Animation
        )
    }
}

/// Visibility and occlusion state of the host window.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowVisibility {
    /// Window is visible and actively focused. Full pacing allowed.
    VisibleActive,
    /// Window is visible in background. Full pacing allowed on demand.
    VisibleInactive,
    /// Window is completely occluded by other windows. Renders are paused.
    Occluded,
    /// Window is minimized or hidden. Renders are paused.
    Hidden,
}

impl WindowVisibility {
    pub const fn should_render(&self) -> bool {
        matches!(self, Self::VisibleActive | Self::VisibleInactive)
    }
}

/// Snapshot of camera position and viewport generation.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CameraSnapshot {
    pub generation: u64,
    pub offset_x: f64,
    pub offset_y: f64,
    pub zoom: f64,
}

impl Default for CameraSnapshot {
    fn default() -> Self {
        Self {
            generation: 1,
            offset_x: 0.0,
            offset_y: 0.0,
            zoom: 1.0,
        }
    }
}

/// Structured events emitted by the frame scheduler.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SchedulerEventType {
    FrameRequested,
    FrameAcquired,
    FrameSubmitted,
    ObsoleteFrameDropped,
    OccludedPause,
    ResumedFromOcclusion,
    DrawableUnavailableDeferred,
    DisplayMigrationHandled,
}

/// One record in the structured scheduler event ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SchedulerEventRecord {
    pub frame_id: u64,
    pub camera_generation: u64,
    pub in_flight: usize,
    pub event_type: SchedulerEventType,
}

#[derive(Debug)]
struct EventRing {
    entries: Vec<SchedulerEventRecord>,
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

    fn record(&mut self, record: SchedulerEventRecord) {
        self.total_events = self.total_events.saturating_add(1);
        if self.entries.len() < self.capacity {
            self.entries.push(record);
        } else {
            self.entries[self.write_pos] = record;
            self.write_pos = (self.write_pos + 1) % self.capacity;
        }
    }

    fn records(&self) -> Vec<SchedulerEventRecord> {
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

/// Aggregate performance and diagnostic counters for the scheduler.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct SchedulerCounters {
    pub frames_requested: u64,
    pub frames_started: u64,
    pub frames_submitted: u64,
    pub obsolete_frames_dropped: u64,
    pub deferred_occluded: u64,
    pub deferred_full_queue: u64,
    pub drawables_unavailable: u64,
    pub display_migrations: u64,
    pub completions_drained: u64,
}

/// Outcome of a scheduler tick step.
#[derive(Debug)]
pub enum SchedulerOutcome {
    /// A new frame was acquired and is ready for drawing / submission.
    FrameReady(ScheduledFrame),
    /// Frame was skipped because the window is stationary with no pending work.
    StationaryIdle,
    /// Frame was deferred because the window is hidden or occluded.
    PausedOccluded,
    /// Frame was deferred because in-flight queue is saturated (§15.2).
    DeferredQueueFull,
    /// Frame acquisition failed because native drawable was unavailable.
    DrawableUnavailable,
}

/// A scheduled frame ready for GPU encoding and presentation.
#[derive(Debug)]
pub struct ScheduledFrame {
    frame_id: u64,
    camera_generation: u64,
    reason: FrameRequestReason,
    paced_frame: PacedFrame,
}

impl ScheduledFrame {
    pub const fn frame_id(&self) -> u64 {
        self.frame_id
    }

    pub const fn camera_generation(&self) -> u64 {
        self.camera_generation
    }

    pub const fn reason(&self) -> FrameRequestReason {
        self.reason
    }

    /// Check if this frame is obsolete compared to the current camera generation.
    pub const fn is_obsolete(&self, current_camera_gen: u64) -> bool {
        self.reason.is_coalescable() && self.camera_generation < current_camera_gen
    }

    /// Present this frame through the pacing engine.
    pub fn present(self, engine: &DisplayPacingEngine) -> Result<(), PacingError> {
        self.paced_frame.present(engine)
    }

    /// Explicitly drop this frame before GPU submission (e.g. superseded camera state).
    ///
    /// Cleanly releases the underlying drawable lease without submitting GPU work.
    pub fn drop_obsolete(self, scheduler: &FrameScheduler) {
        scheduler.record_obsolete_dropped(self.frame_id, self.camera_generation);
        drop(self.paced_frame);
    }
}

/// Demand-driven frame scheduler.
///
/// Binds user interaction and camera velocity to display link pacing.
#[derive(Debug)]
pub struct FrameScheduler {
    visibility: Cell<WindowVisibility>,
    latest_camera: Cell<CameraSnapshot>,
    pending_reason: Cell<Option<FrameRequestReason>>,
    counters: Cell<SchedulerCounters>,
    event_ring: RefCell<EventRing>,
}

impl FrameScheduler {
    /// Create a new demand-driven frame scheduler.
    pub fn new() -> Self {
        Self {
            visibility: Cell::new(WindowVisibility::VisibleActive),
            latest_camera: Cell::new(CameraSnapshot::default()),
            pending_reason: Cell::new(None),
            counters: Cell::new(SchedulerCounters::default()),
            event_ring: RefCell::new(EventRing::new(SCHEDULER_EVENT_RING_CAPACITY)),
        }
    }

    pub fn visibility(&self) -> WindowVisibility {
        self.visibility.get()
    }

    pub fn latest_camera(&self) -> CameraSnapshot {
        self.latest_camera.get()
    }

    pub fn counters(&self) -> SchedulerCounters {
        self.counters.get()
    }

    pub fn event_records(&self) -> Vec<SchedulerEventRecord> {
        self.event_ring.borrow().records()
    }

    /// Update window visibility (e.g. from AppKit occlusion notifications).
    pub fn set_visibility(&mut self, visibility: WindowVisibility, pacing: &DisplayPacingEngine) {
        let old = self.visibility.get();
        if old == visibility {
            return;
        }
        self.visibility.set(visibility);
        if !visibility.should_render() {
            pacing.pause();
            self.bump_counter(|c| c.deferred_occluded += 1);
            self.record_event(
                0,
                0,
                pacing.in_flight_count(),
                SchedulerEventType::OccludedPause,
            );
        } else if !old.should_render() {
            pacing.resume();
            self.record_event(
                0,
                0,
                pacing.in_flight_count(),
                SchedulerEventType::ResumedFromOcclusion,
            );
        }
    }

    /// Update the latest camera snapshot during pan / zoom gestures.
    pub fn update_camera(&self, camera: CameraSnapshot, pacing: &DisplayPacingEngine) {
        self.latest_camera.set(camera);
        self.request_frame(FrameRequestReason::CameraMotion, pacing);
    }

    /// Request a new frame render with an explicit reason.
    pub fn request_frame(&self, reason: FrameRequestReason, pacing: &DisplayPacingEngine) {
        self.pending_reason.set(Some(reason));
        self.bump_counter(|c| c.frames_requested += 1);
        pacing.request_frame();
        self.record_event(
            0,
            self.latest_camera.get().generation,
            pacing.in_flight_count(),
            SchedulerEventType::FrameRequested,
        );
    }

    /// Handle display backing scale or display migration.
    pub fn handle_display_migration(&self, pacing: &DisplayPacingEngine) {
        self.bump_counter(|c| c.display_migrations += 1);
        self.record_event(
            0,
            self.latest_camera.get().generation,
            pacing.in_flight_count(),
            SchedulerEventType::DisplayMigrationHandled,
        );
        self.request_frame(FrameRequestReason::DisplayMigration, pacing);
    }

    /// Record a completion drained while stationary or occluded.
    pub fn record_completion_drained(&self) {
        self.bump_counter(|c| c.completions_drained += 1);
    }

    /// Execute one scheduler tick synchronized with the display link.
    pub fn tick(
        &self,
        token: MainThreadToken,
        pacing: &DisplayPacingEngine,
        binding: &NativeViewBinding,
        target_timestamp: f64,
        target_presentation_timestamp: f64,
    ) -> Result<SchedulerOutcome, PacingError> {
        // 1. Occlusion guard: never acquire drawables or render when occluded (§15.4).
        if !self.visibility.get().should_render() {
            self.bump_counter(|c| c.deferred_occluded += 1);
            return Ok(SchedulerOutcome::PausedOccluded);
        }

        // 2. Stationary guard: if no frame was requested, avoid idle redraw.
        let Some(reason) = self.pending_reason.get() else {
            return Ok(SchedulerOutcome::StationaryIdle);
        };

        // 3. Tick the pacing engine with two-frame starting policy:
        let pacing_outcome = pacing.tick(
            token,
            binding,
            target_timestamp,
            target_presentation_timestamp,
        );

        match pacing_outcome {
            Ok(PacingOutcome::FrameStarted(paced_frame)) => {
                // Clear pending request now that frame has started:
                self.pending_reason.set(None);
                self.bump_counter(|c| c.frames_started += 1);
                let camera_gen = self.latest_camera.get().generation;
                let frame_id = paced_frame.frame_id();
                self.record_event(
                    frame_id,
                    camera_gen,
                    pacing.in_flight_count(),
                    SchedulerEventType::FrameAcquired,
                );

                Ok(SchedulerOutcome::FrameReady(ScheduledFrame {
                    frame_id,
                    camera_generation: camera_gen,
                    reason,
                    paced_frame,
                }))
            }
            Ok(PacingOutcome::DeferredFullQueue) => {
                self.bump_counter(|c| c.deferred_full_queue += 1);
                Ok(SchedulerOutcome::DeferredQueueFull)
            }
            Ok(PacingOutcome::Idle) => Ok(SchedulerOutcome::StationaryIdle),
            Ok(PacingOutcome::Paused) => Ok(SchedulerOutcome::PausedOccluded),
            Err(PacingError::DrawableUnavailable) => {
                self.bump_counter(|c| c.drawables_unavailable += 1);
                self.record_event(
                    0,
                    self.latest_camera.get().generation,
                    pacing.in_flight_count(),
                    SchedulerEventType::DrawableUnavailableDeferred,
                );
                Ok(SchedulerOutcome::DrawableUnavailable)
            }
            Err(err) => Err(err),
        }
    }

    fn record_obsolete_dropped(&self, frame_id: u64, camera_generation: u64) {
        self.bump_counter(|c| c.obsolete_frames_dropped += 1);
        self.record_event(
            frame_id,
            camera_generation,
            0,
            SchedulerEventType::ObsoleteFrameDropped,
        );
    }

    fn bump_counter(&self, f: impl FnOnce(&mut SchedulerCounters)) {
        let mut counters = self.counters.get();
        f(&mut counters);
        self.counters.set(counters);
    }

    fn record_event(
        &self,
        frame_id: u64,
        camera_generation: u64,
        in_flight: usize,
        event_type: SchedulerEventType,
    ) {
        self.event_ring.borrow_mut().record(SchedulerEventRecord {
            frame_id,
            camera_generation,
            in_flight,
            event_type,
        });
    }
}

impl Default for FrameScheduler {
    fn default() -> Self {
        Self::new()
    }
}
