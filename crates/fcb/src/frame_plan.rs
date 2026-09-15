#![forbid(unsafe_code)]

use std::{collections::VecDeque, sync::Arc};

use fcb_core::{
    AcceptedLayoutSnapshot, ArenaOwnerId, ByteRange, CameraGeneration, DisplayGeneration,
    DisplayMetrics, FileId, InteractionGeneration, LayoutRevision, Point2D, PresentedFrameId,
    SceneGeneration, SemanticNodeId, SourceRevision,
};

use crate::FcbError;

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
        })
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

/// Tracks the presentation lifecycle and interaction authority.
///
/// Ensures clicks and accessibility queries are resolved against the frame
/// the user actually saw, even if background models have moved or reflowed
/// ahead of GPU presentation.
#[derive(Debug)]
pub struct PresentedFrameTracker {
    owner: ArenaOwnerId,
    last_presented: Option<Arc<FramePlan>>,
    pending_queue: VecDeque<Arc<FramePlan>>,
    history: VecDeque<Arc<FramePlan>>,
    max_pending: usize,
    max_history: usize,
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
            pending_queue: VecDeque::with_capacity(max_pending),
            history: VecDeque::with_capacity(max_history),
            max_pending,
            max_history,
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
        if plan.frame_id().is_none() {
            return Err(FcbError::FrameNotFound);
        }
        if self.pending_queue.len() >= self.max_pending {
            return Err(FcbError::FrameQueueExhausted);
        }
        self.pending_queue.push_back(plan);
        Ok(())
    }

    /// Confirms that a frame has been presented by the host/GPU.
    ///
    /// Finds the frame in the pending queue, retires any older pending frames,
    /// moves the previous last_presented frame into bounded history, and
    /// sets the confirmed frame as the active interaction authority.
    pub fn confirm_presented(&mut self, frame_id: PresentedFrameId) -> Result<(), FcbError> {
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

        let target = target_frame.ok_or(FcbError::FrameNotFound)?;

        if let Some(prev) = self.last_presented.take() {
            if self.history.len() >= self.max_history {
                let _ = self.history.pop_front();
            }
            self.history.push_back(prev);
        }

        self.last_presented = Some(target);
        Ok(())
    }

    /// Resolves an interaction point against the currently visible frame.
    ///
    /// The oracle contract guarantees that even if newer frames have been submitted
    /// to pending_queue (model moved or reflowed), interactions resolve exclusively
    /// against `last_presented` until confirmed by presentation.
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
}
