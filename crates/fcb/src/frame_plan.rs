#![forbid(unsafe_code)]

use std::{collections::VecDeque, sync::Arc};

use fcb_core::{
    AcceptedLayoutSnapshot, ArenaOwnerId, ByteRange, CameraGeneration, ClockDomainId,
    DisplayGeneration, DisplayMetrics, FileId, InteractionGeneration, LayoutRevision, Point2D,
    PresentedFrameId, SceneGeneration, SemanticNodeId, SourceRevision,
};

use crate::FcbError;

/// Presentation mode describing whether the frame authority was identified
/// explicitly by ID or conservatively by timestamp.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PresentationMode {
    /// Exact host-reported frame presentation.
    Exact(PresentedFrameId),
    /// Conservative fallback presentation to the latest known eligible frame.
    Conservative {
        frame_id: PresentedFrameId,
        timestamp_nanos: u64,
    },
}

impl PresentationMode {
    pub const fn frame_id(&self) -> PresentedFrameId {
        match self {
            Self::Exact(id) | Self::Conservative { frame_id: id, .. } => *id,
        }
    }

    pub const fn is_conservative(&self) -> bool {
        matches!(self, Self::Conservative { .. })
    }
}

/// Monotonic timing records associated with a frame plan.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct FrameTimestamps {
    clock_domain: ClockDomainId,
    submitted_nanos: u64,
    presented_nanos: Option<u64>,
}

impl FrameTimestamps {
    pub const fn new(clock_domain: ClockDomainId, submitted_nanos: u64) -> Self {
        Self {
            clock_domain,
            submitted_nanos,
            presented_nanos: None,
        }
    }

    pub const fn with_presented(
        clock_domain: ClockDomainId,
        submitted_nanos: u64,
        presented_nanos: u64,
    ) -> Self {
        Self {
            clock_domain,
            submitted_nanos,
            presented_nanos: Some(presented_nanos),
        }
    }

    pub const fn clock_domain(&self) -> ClockDomainId {
        self.clock_domain
    }

    pub const fn submitted_nanos(&self) -> u64 {
        self.submitted_nanos
    }

    pub const fn presented_nanos(&self) -> Option<u64> {
        self.presented_nanos
    }
}

/// Bounded structured event representing frame presentation and interaction evidence.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FrameEvidenceEvent {
    FrameSubmitted {
        frame_id: PresentedFrameId,
        display_generation: DisplayGeneration,
        submitted_nanos: u64,
    },
    FramePresented {
        frame_id: PresentedFrameId,
        is_conservative: bool,
        presented_nanos: Option<u64>,
    },
    InteractionResolved {
        frame_id: PresentedFrameId,
        target_node: Option<SemanticNodeId>,
        point: Point2D,
        latency_nanos: Option<u64>,
    },
    ResizeRefused {
        presented_display: DisplayGeneration,
        claimed_display: DisplayGeneration,
    },
    QueueFullRefused {
        attempted_frame_id: PresentedFrameId,
        current_pending: usize,
    },
}

/// Fixed-capacity circular ring for frame and presentation evidence.
#[derive(Clone, Debug, PartialEq)]
pub struct FrameEvidenceRing {
    events: Vec<FrameEvidenceEvent>,
    capacity: usize,
    head: usize,
    count: usize,
    total_submitted: u64,
    total_presented: u64,
    total_interactions: u64,
    total_resize_refusals: u64,
}

impl FrameEvidenceRing {
    pub const DEFAULT_CAPACITY: usize = 64;

    pub fn new(capacity: usize) -> Self {
        let capacity = if capacity == 0 {
            Self::DEFAULT_CAPACITY
        } else {
            capacity.max(16)
        };
        Self {
            events: Vec::with_capacity(capacity),
            capacity,
            head: 0,
            count: 0,
            total_submitted: 0,
            total_presented: 0,
            total_interactions: 0,
            total_resize_refusals: 0,
        }
    }

    pub fn push(&mut self, event: FrameEvidenceEvent) {
        match event {
            FrameEvidenceEvent::FrameSubmitted { .. } => self.total_submitted += 1,
            FrameEvidenceEvent::FramePresented { .. } => self.total_presented += 1,
            FrameEvidenceEvent::InteractionResolved { .. } => self.total_interactions += 1,
            FrameEvidenceEvent::ResizeRefused { .. } => self.total_resize_refusals += 1,
            FrameEvidenceEvent::QueueFullRefused { .. } => {}
        }

        if self.events.len() < self.capacity {
            self.events.push(event);
        } else {
            self.events[self.head] = event;
            self.head = (self.head + 1) % self.capacity;
        }
        self.count += 1;
    }

    pub const fn total_submitted(&self) -> u64 {
        self.total_submitted
    }

    pub const fn total_presented(&self) -> u64 {
        self.total_presented
    }

    pub const fn total_interactions(&self) -> u64 {
        self.total_interactions
    }

    pub const fn total_resize_refusals(&self) -> u64 {
        self.total_resize_refusals
    }

    pub const fn total_events(&self) -> usize {
        self.count
    }

    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    pub fn recent_events(&self) -> &[FrameEvidenceEvent] {
        &self.events
    }
}

/// Immutable, renderer-neutral drawing and interaction plan bundling all
/// multi-dimensional generations with the exact accepted layout snapshot.
#[derive(Clone, Debug, PartialEq)]
pub struct FramePlan {
    owner: ArenaOwnerId,
    file: FileId,
    source: SourceRevision,
    bytes: ByteRange,
    frame_id: Option<PresentedFrameId>,
    camera: Option<CameraGeneration>,
    scene: Option<SceneGeneration>,
    layout: Option<LayoutRevision>,
    display: Option<DisplayGeneration>,
    interaction: Option<InteractionGeneration>,
    metrics: Option<DisplayMetrics>,
    layout_snapshot: Option<Arc<AcceptedLayoutSnapshot>>,
    timestamps: Option<FrameTimestamps>,
}

impl FramePlan {
    /// Baseline frame plan for simple in-memory capture without full scene layout.
    pub const fn new(
        owner: ArenaOwnerId,
        file: FileId,
        source: SourceRevision,
        bytes: ByteRange,
    ) -> Self {
        Self {
            owner,
            file,
            source,
            bytes,
            frame_id: None,
            camera: None,
            scene: None,
            layout: None,
            display: None,
            interaction: None,
            metrics: None,
            layout_snapshot: None,
            timestamps: None,
        }
    }

    /// Full bundled constructor verifying generational and ownership invariants.
    #[allow(clippy::too_many_arguments)]
    pub fn bundle(
        owner: ArenaOwnerId,
        file: FileId,
        source: SourceRevision,
        bytes: ByteRange,
        frame_id: PresentedFrameId,
        camera: CameraGeneration,
        scene: SceneGeneration,
        layout: LayoutRevision,
        display: DisplayGeneration,
        interaction: InteractionGeneration,
        metrics: DisplayMetrics,
        layout_snapshot: Arc<AcceptedLayoutSnapshot>,
    ) -> Result<Self, FcbError> {
        // Validate owners
        if file.owner() != owner
            || source.owner() != owner
            || frame_id.owner() != owner
            || camera.owner() != owner
            || scene.owner() != owner
            || layout.owner() != owner
            || display.owner() != owner
            || interaction.owner() != owner
        {
            return Err(FcbError::OwnerMismatch);
        }

        // Validate display metrics generation
        if metrics.generation() != display {
            return Err(FcbError::StaleGeneration);
        }

        // Validate layout snapshot identity matches bundled generations
        let snapshot_ident = layout_snapshot.identity();
        if snapshot_ident.owner() != owner {
            return Err(FcbError::OwnerMismatch);
        }
        if snapshot_ident.layout_revision() != layout {
            return Err(FcbError::StaleGeneration);
        }
        if snapshot_ident.source_revision() != source {
            return Err(FcbError::StaleGeneration);
        }
        if snapshot_ident.display_generation() != display {
            return Err(FcbError::StaleGeneration);
        }

        if snapshot_ident
            .presented_frame()
            .is_some_and(|id| id != frame_id)
        {
            return Err(FcbError::StaleGeneration);
        }

        Ok(Self {
            owner,
            file,
            source,
            bytes,
            frame_id: Some(frame_id),
            camera: Some(camera),
            scene: Some(scene),
            layout: Some(layout),
            display: Some(display),
            interaction: Some(interaction),
            metrics: Some(metrics),
            layout_snapshot: Some(layout_snapshot),
            timestamps: None,
        })
    }

    pub fn with_timestamps(mut self, timestamps: FrameTimestamps) -> Result<Self, FcbError> {
        if timestamps.clock_domain().owner() != self.owner {
            return Err(FcbError::OwnerMismatch);
        }
        self.timestamps = Some(timestamps);
        Ok(self)
    }

    pub fn set_timestamps(&mut self, timestamps: FrameTimestamps) -> Result<(), FcbError> {
        if timestamps.clock_domain().owner() != self.owner {
            return Err(FcbError::OwnerMismatch);
        }
        self.timestamps = Some(timestamps);
        Ok(())
    }

    pub const fn timestamps(&self) -> Option<&FrameTimestamps> {
        self.timestamps.as_ref()
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn file(&self) -> FileId {
        self.file
    }

    pub const fn source(&self) -> SourceRevision {
        self.source
    }

    pub const fn bytes(&self) -> ByteRange {
        self.bytes
    }

    pub const fn frame_id(&self) -> Option<PresentedFrameId> {
        self.frame_id
    }

    pub const fn camera(&self) -> Option<CameraGeneration> {
        self.camera
    }

    pub const fn scene(&self) -> Option<SceneGeneration> {
        self.scene
    }

    pub const fn layout(&self) -> Option<LayoutRevision> {
        self.layout
    }

    pub const fn display(&self) -> Option<DisplayGeneration> {
        self.display
    }

    pub const fn interaction(&self) -> Option<InteractionGeneration> {
        self.interaction
    }

    pub const fn metrics(&self) -> Option<&DisplayMetrics> {
        self.metrics.as_ref()
    }

    pub fn layout_snapshot(&self) -> Option<&Arc<AcceptedLayoutSnapshot>> {
        self.layout_snapshot.as_ref()
    }
}

/// The result of resolving an interaction against a confirmed presented frame.
#[derive(Clone, Debug, PartialEq)]
pub struct InteractionResolution {
    frame_id: PresentedFrameId,
    target_node: Option<SemanticNodeId>,
    point: Point2D,
    layout_revision: LayoutRevision,
    display_generation: DisplayGeneration,
    interaction_generation: InteractionGeneration,
}

impl InteractionResolution {
    pub const fn frame_id(&self) -> PresentedFrameId {
        self.frame_id
    }

    pub const fn target_node(&self) -> Option<SemanticNodeId> {
        self.target_node
    }

    pub const fn point(&self) -> Point2D {
        self.point
    }

    pub const fn layout_revision(&self) -> LayoutRevision {
        self.layout_revision
    }

    pub const fn display_generation(&self) -> DisplayGeneration {
        self.display_generation
    }

    pub const fn interaction_generation(&self) -> InteractionGeneration {
        self.interaction_generation
    }
}

/// Tracks the presentation lifecycle, conservative acceptance, coordinate domains,
/// and interaction authority.
///
/// Ensures clicks and accessibility queries are resolved against the frame
/// the user actually saw, even if background models have moved or reflowed
/// ahead of GPU presentation.
#[derive(Debug)]
pub struct PresentedFrameTracker {
    owner: ArenaOwnerId,
    last_presented: Option<Arc<FramePlan>>,
    last_mode: Option<PresentationMode>,
    pending_queue: VecDeque<Arc<FramePlan>>,
    history: VecDeque<Arc<FramePlan>>,
    max_pending: usize,
    max_history: usize,
    evidence_ring: FrameEvidenceRing,
}

impl PresentedFrameTracker {
    pub const DEFAULT_MAX_PENDING: usize = 16;
    pub const DEFAULT_MAX_HISTORY: usize = 32;

    pub fn new(owner: ArenaOwnerId, max_pending: usize, max_history: usize) -> Self {
        let max_pending = if max_pending == 0 {
            Self::DEFAULT_MAX_PENDING
        } else {
            max_pending
        };
        let max_history = if max_history == 0 {
            Self::DEFAULT_MAX_HISTORY
        } else {
            max_history
        };
        Self {
            owner,
            last_presented: None,
            last_mode: None,
            pending_queue: VecDeque::with_capacity(max_pending),
            history: VecDeque::with_capacity(max_history),
            max_pending,
            max_history,
            evidence_ring: FrameEvidenceRing::new(FrameEvidenceRing::DEFAULT_CAPACITY),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn pending_count(&self) -> usize {
        self.pending_queue.len()
    }

    pub fn history_count(&self) -> usize {
        self.history.len()
    }

    pub fn pending_frames(&self) -> &VecDeque<Arc<FramePlan>> {
        &self.pending_queue
    }

    pub fn history_frames(&self) -> &VecDeque<Arc<FramePlan>> {
        &self.history
    }

    pub fn last_presented(&self) -> Option<&Arc<FramePlan>> {
        self.last_presented.as_ref()
    }

    pub fn last_presented_frame_id(&self) -> Option<PresentedFrameId> {
        self.last_presented.as_ref().and_then(|f| f.frame_id())
    }

    pub const fn last_presentation_mode(&self) -> Option<PresentationMode> {
        self.last_mode
    }

    pub const fn evidence_ring(&self) -> &FrameEvidenceRing {
        &self.evidence_ring
    }

    pub fn evidence_ring_mut(&mut self) -> &mut FrameEvidenceRing {
        &mut self.evidence_ring
    }

    /// Returns the layout snapshot of the currently visible frame for accessibility or hit-testing.
    ///
    /// The oracle contract guarantees that even if newer frames have been submitted
    /// to pending_queue (model moved or reflowed), queries resolve exclusively
    /// against `last_presented` until confirmed by presentation.
    pub fn visible_layout_snapshot(&self) -> Result<&Arc<AcceptedLayoutSnapshot>, FcbError> {
        let frame = self.last_presented.as_ref().ok_or(FcbError::FrameUnpresented)?;
        frame.layout_snapshot().ok_or(FcbError::FrameNotFound)
    }

    /// Resolves an accessibility hit against the currently visible frame.
    pub fn resolve_accessibility_hit(&self, point: Point2D) -> Result<Option<SemanticNodeId>, FcbError> {
        let snapshot = self.visible_layout_snapshot()?;
        Ok(snapshot.hit_test(point))
    }

    /// Submits a newly constructed frame plan into the pending presentation queue.
    pub fn submit_frame(&mut self, plan: Arc<FramePlan>) -> Result<(), FcbError> {
        if plan.owner() != self.owner {
            return Err(FcbError::OwnerMismatch);
        }
        let frame_id = plan.frame_id().ok_or(FcbError::FrameNotFound)?;
        if self.pending_queue.len() >= self.max_pending {
            self.evidence_ring.push(FrameEvidenceEvent::QueueFullRefused {
                attempted_frame_id: frame_id,
                current_pending: self.pending_queue.len(),
            });
            return Err(FcbError::FrameQueueExhausted);
        }

        let submitted_nanos = plan.timestamps().map_or(0, |ts| ts.submitted_nanos());
        let display_gen = plan
            .display()
            .unwrap_or_else(|| DisplayGeneration::new(self.owner, 1).unwrap());

        self.evidence_ring.push(FrameEvidenceEvent::FrameSubmitted {
            frame_id,
            display_generation: display_gen,
            submitted_nanos,
        });

        self.pending_queue.push_back(plan);
        Ok(())
    }

    /// Confirms that a frame has been presented by the host/GPU with an exact frame ID.
    pub fn confirm_presented_exact(
        &mut self,
        frame_id: PresentedFrameId,
        presented_nanos: Option<u64>,
    ) -> Result<(), FcbError> {
        if frame_id.owner() != self.owner {
            return Err(FcbError::OwnerMismatch);
        }

        let pos = self
            .pending_queue
            .iter()
            .position(|f| f.frame_id() == Some(frame_id))
            .ok_or(FcbError::FrameNotFound)?;

        // Remove all pending frames up to and including this one
        let mut target_frame = None;
        for _ in 0..=pos {
            let f = self.pending_queue.pop_front().unwrap();
            if f.frame_id() == Some(frame_id) {
                target_frame = Some(f);
            } else {
                // Older pending frame was superseded before presentation
                if self.history.len() >= self.max_history {
                    let _ = self.history.pop_front();
                }
                self.history.push_back(f);
            }
        }

        let raw_target = target_frame.ok_or(FcbError::FrameNotFound)?;
        let mut target = (*raw_target).clone();
        if let Some(p_nanos) = presented_nanos
            && let Some(ts) = target.timestamps().copied()
        {
            let updated =
                FrameTimestamps::with_presented(ts.clock_domain(), ts.submitted_nanos(), p_nanos);
            let _ = target.set_timestamps(updated);
        }
        let target = Arc::new(target);

        if let Some(prev) = self.last_presented.take() {
            if self.history.len() >= self.max_history {
                let _ = self.history.pop_front();
            }
            self.history.push_back(prev);
        }

        self.last_mode = Some(PresentationMode::Exact(frame_id));
        self.evidence_ring.push(FrameEvidenceEvent::FramePresented {
            frame_id,
            is_conservative: false,
            presented_nanos,
        });

        self.last_presented = Some(target);
        Ok(())
    }

    /// Convenience wrapper for exact presentation without explicit presentation timestamp.
    pub fn confirm_presented(&mut self, frame_id: PresentedFrameId) -> Result<(), FcbError> {
        self.confirm_presented_exact(frame_id, None)
    }

    /// Conservatively confirms frame presentation when the host platform cannot
    /// identify the exact PresentedFrameId.
    ///
    /// Selects the latest frame submitted on or before `timestamp_nanos` within
    /// the matching `clock_domain`. Retires older pending frames without inventing
    /// false nanosecond precision.
    pub fn confirm_presented_conservative(
        &mut self,
        clock_domain: ClockDomainId,
        timestamp_nanos: u64,
    ) -> Result<PresentedFrameId, FcbError> {
        if clock_domain.owner() != self.owner {
            return Err(FcbError::OwnerMismatch);
        }

        // Look for the latest eligible frame in pending queue
        let eligible_idx = self.pending_queue.iter().rposition(|f| {
            f.timestamps().is_some_and(|ts| {
                ts.clock_domain() == clock_domain && ts.submitted_nanos() <= timestamp_nanos
            })
        });

        match eligible_idx {
            Some(idx) => {
                let frame_id = self.pending_queue[idx]
                    .frame_id()
                    .ok_or(FcbError::FrameNotFound)?;

                let mut target_frame = None;
                for _ in 0..=idx {
                    let f = self.pending_queue.pop_front().unwrap();
                    if f.frame_id() == Some(frame_id) {
                        target_frame = Some(f);
                    } else {
                        if self.history.len() >= self.max_history {
                            let _ = self.history.pop_front();
                        }
                        self.history.push_back(f);
                    }
                }

                let raw_target = target_frame.ok_or(FcbError::FrameNotFound)?;
                let mut target = (*raw_target).clone();
                if let Some(ts) = target.timestamps().copied() {
                    let updated = FrameTimestamps::with_presented(
                        ts.clock_domain(),
                        ts.submitted_nanos(),
                        timestamp_nanos,
                    );
                    let _ = target.set_timestamps(updated);
                }
                let target = Arc::new(target);

                if let Some(prev) = self.last_presented.take() {
                    if self.history.len() >= self.max_history {
                        let _ = self.history.pop_front();
                    }
                    self.history.push_back(prev);
                }

                self.last_mode = Some(PresentationMode::Conservative {
                    frame_id,
                    timestamp_nanos,
                });
                self.evidence_ring.push(FrameEvidenceEvent::FramePresented {
                    frame_id,
                    is_conservative: true,
                    presented_nanos: Some(timestamp_nanos),
                });

                self.last_presented = Some(target);
                Ok(frame_id)
            }
            None => {
                // If pending queue has no eligible frame, check if last_presented is eligible
                if let Some(last) = self.last_presented.as_ref() {
                    let eligible = last.timestamps().is_some_and(|ts| {
                        ts.clock_domain() == clock_domain && ts.submitted_nanos() <= timestamp_nanos
                    });
                    if eligible {
                        let fid = last.frame_id().ok_or(FcbError::FrameNotFound)?;
                        self.last_mode = Some(PresentationMode::Conservative {
                            frame_id: fid,
                            timestamp_nanos,
                        });
                        self.evidence_ring.push(FrameEvidenceEvent::FramePresented {
                            frame_id: fid,
                            is_conservative: true,
                            presented_nanos: Some(timestamp_nanos),
                        });
                        return Ok(fid);
                    }
                }
                Err(FcbError::FrameNotFound)
            }
        }
    }

    /// Resolves an interaction point against the currently visible frame (in logical points).
    pub fn resolve_interaction(&self, point: Point2D) -> Result<InteractionResolution, FcbError> {
        let frame = self.last_presented.as_ref().ok_or(FcbError::FrameUnpresented)?;

        let frame_id = frame.frame_id().ok_or(FcbError::FrameNotFound)?;
        let layout_rev = frame.layout().ok_or(FcbError::StaleGeneration)?;
        let display_gen = frame.display().ok_or(FcbError::StaleGeneration)?;
        let interaction_gen = frame.interaction().ok_or(FcbError::StaleGeneration)?;

        let layout_snapshot = frame.layout_snapshot().ok_or(FcbError::FrameNotFound)?;

        let target_node = layout_snapshot.hit_test(point);

        Ok(InteractionResolution {
            frame_id,
            target_node,
            point,
            layout_revision: layout_rev,
            display_generation: display_gen,
            interaction_generation: interaction_gen,
        })
    }

    /// Resolves an interaction arriving in physical pixels with verified coordinate
    /// domain protection and clock conversion.
    ///
    /// Refuses immediately if `claimed_display` does not match the visible frame's
    /// display generation, preventing resize races from mixing old pixel coordinates
    /// with new logical geometry.
    pub fn resolve_interaction_physical(
        &mut self,
        physical_point: Point2D,
        claimed_display: DisplayGeneration,
        event_clock: Option<(ClockDomainId, u64)>,
    ) -> Result<InteractionResolution, FcbError> {
        let frame = self.last_presented.as_ref().ok_or(FcbError::FrameUnpresented)?;

        let frame_id = frame.frame_id().ok_or(FcbError::FrameNotFound)?;
        let layout_rev = frame.layout().ok_or(FcbError::StaleGeneration)?;
        let display_gen = frame.display().ok_or(FcbError::StaleGeneration)?;
        let interaction_gen = frame.interaction().ok_or(FcbError::StaleGeneration)?;
        let metrics = frame.metrics().ok_or(FcbError::FrameNotFound)?;
        let layout_snapshot = frame.layout_snapshot().ok_or(FcbError::FrameNotFound)?;

        // COORDINATE DOMAIN / RESIZE PROTECTION:
        // Input events sampled against a different display generation (e.g. window resize
        // or backing scale change in flight) must NOT be applied to visible geometry!
        if claimed_display != display_gen {
            self.evidence_ring.push(FrameEvidenceEvent::ResizeRefused {
                presented_display: display_gen,
                claimed_display,
            });
            return Err(FcbError::CoordinateDomainMismatch);
        }

        // CLOCK DOMAIN & MONOTONIC CONVERSION:
        let mut latency_nanos = None;
        if let Some((clk_domain, event_nanos)) = event_clock {
            if clk_domain.owner() != self.owner {
                return Err(FcbError::OwnerMismatch);
            }
            if let Some(ts) = frame.timestamps() {
                if ts.clock_domain() != clk_domain {
                    return Err(FcbError::ClockDomainMismatch);
                }
                if let Some(pres_nanos) = ts.presented_nanos()
                    && event_nanos >= pres_nanos
                {
                    latency_nanos = Some(event_nanos - pres_nanos);
                }
            }
        }

        // Convert physical pixel coordinates to logical points using presented frame metrics
        let logical_point = metrics.physical_to_logical_point(physical_point.x(), physical_point.y())?;

        let target_node = layout_snapshot.hit_test(logical_point);

        self.evidence_ring.push(FrameEvidenceEvent::InteractionResolved {
            frame_id,
            target_node,
            point: logical_point,
            latency_nanos,
        });

        Ok(InteractionResolution {
            frame_id,
            target_node,
            point: logical_point,
            layout_revision: layout_rev,
            display_generation: display_gen,
            interaction_generation: interaction_gen,
        })
    }
}
