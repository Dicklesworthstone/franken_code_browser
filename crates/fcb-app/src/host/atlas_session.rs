#![forbid(unsafe_code)]

//! Retained native-host atlas navigation. Discover/pack/index once; subsequent
//! plans use the same spatial index. Candidate plans are NOT presented frames.
//! Explicit host acknowledgment selects the immutable plan used for picking.
//! All methods are worker operations; this module creates no thread or runtime.

use std::{fs, mem::size_of, path::{Path, PathBuf}};
use fcb::{ArenaOwnerId, ByteLength, CameraGeneration, DisplayGeneration, DisplayMetrics,
    FileId, Point2D, Rect2D, Size2D};
use fcb::map::{AtlasDetail, AtlasError, AtlasHit, AtlasNodeId, Camera2D, CameraError,
    DisplayColorConfig, LayoutOptions, LayoutRevision, LodThresholds, VisibleLimits,
use fcb::map::workspace::{WorkspaceAtlasError, WorkspaceAtlasLimits};
use fcb::map::workspace::retained::RetainedWorkspaceAtlas;
use fcb::search::{QueryGeneration, RawPath, ResourceAllocationId, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceError, WorkspaceLimits, WorkspaceStage};
use fcb::source::CancelFlag;
use fcb_core::{PresentedFrameId, ResourceLease};
use crate::{AppError, EXIT_OK, EXIT_PARTIAL, MANAGED_BYTES, input, workspace};
use crate::output::{Output, OutputError, MAX_RESPONSE_BYTES};
use super::{HostResponse, reader::{ReaderSession, ReaderSessionError}};

pub const MAX_ATLAS_SESSION_FILES: usize = 20_000;
pub const MAX_ATLAS_SESSION_ITEMS: usize = 4096;
pub const MAX_ATLAS_SESSION_VISITS: usize = 131_072;
pub const MAX_ATLAS_HISTORY: usize = 64;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AtlasSessionError {
    App(AppError), Atlas(AtlasError), Workspace(WorkspaceAtlasError), Reader(ReaderSessionError),
    InvalidLimits, StaleGeneration, NoPendingPlan, NoPresentedPlan, StaleFrame,
    EmptyHistory, HistoryFull, NoFile, Canceled, IdentityExhausted,
}
impl std::fmt::Display for AtlasSessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::App(e) => write!(f, "{e}"), Self::Atlas(e) => write!(f, "{e}"),
            Self::Workspace(e) => write!(f, "{e}"), Self::Reader(e) => write!(f, "{e}"),
            other => f.write_str(match other {
                Self::InvalidLimits => "ATLAS_SESSION_INVALID_LIMITS", Self::StaleGeneration => "ATLAS_SESSION_STALE_GENERATION",
                Self::NoPendingPlan => "ATLAS_SESSION_NO_PENDING_PLAN", Self::NoPresentedPlan => "ATLAS_SESSION_NOT_PRESENTED",
                Self::StaleFrame => "ATLAS_SESSION_STALE_FRAME", Self::EmptyHistory => "ATLAS_SESSION_EMPTY_HISTORY",
                Self::HistoryFull => "ATLAS_SESSION_HISTORY_FULL", Self::NoFile => "ATLAS_SESSION_NOT_A_FILE",
                Self::Canceled => "ATLAS_SESSION_CANCELED", Self::IdentityExhausted => "ATLAS_SESSION_IDENTITY_EXHAUSTED",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for AtlasSessionError {}
impl From<AppError> for AtlasSessionError { fn from(e: AppError) -> Self { Self::App(e) } }
impl From<AtlasError> for AtlasSessionError { fn from(e: AtlasError) -> Self { Self::Atlas(e) } }
impl From<CameraError> for AtlasSessionError { fn from(e: CameraError) -> Self { Self::Atlas(e.into()) } }
impl From<WorkspaceAtlasError> for AtlasSessionError { fn from(e: WorkspaceAtlasError) -> Self { Self::Workspace(e) } }
impl From<WorkspaceError> for AtlasSessionError { fn from(e: WorkspaceError) -> Self { Self::Workspace(e.into()) } }
impl From<ReaderSessionError> for AtlasSessionError { fn from(e: ReaderSessionError) -> Self { Self::Reader(e) } }
impl From<OutputError> for AtlasSessionError { fn from(e: OutputError) -> Self { Self::App(e.into()) } }

#[derive(Clone, Copy, Debug)]
pub struct AtlasSessionOptions {
    pub max_files: usize,
    pub visible: VisibleLimits,
    pub width: f64,
    pub height: f64,
    pub scale: f64,
}
impl Default for AtlasSessionOptions {
    fn default() -> Self { Self { max_files: 4096, visible: VisibleLimits::default(), width: 1024.0, height: 768.0, scale: 1.0 } }
}
#[derive(Clone, Copy, Debug)]
pub enum AtlasAction {
    View,
    Pan(Point2D),
    Zoom { anchor: Point2D, factor: f64 },
    Focus(u32),
    Back,
    Resize { width: f64, height: f64, scale: f64 },
    /// Explicit file-type display scope. An accepted change may repack
    /// geometry from the frozen shared catalog on the worker; camera ticks
    /// never repack and never read or parse source. Restoring All reuses the
    /// retained base layout, so earlier All-frame geometry stays valid.
    Scope(AtlasScope),
}
#[derive(Clone, Copy)]
struct Position { focus: AtlasNodeId, camera: Camera2D }

/// Metadata selection from a host-acknowledged frame. The subsequent read is a
/// NEW observation, not source bytes captured by metadata discovery or drawing.
struct AtlasReadTarget {
    node: AtlasNodeId,
    file: FileId,
    frame: u64,
    display: u64,
    path: PathBuf,
}
impl AtlasReadTarget {
    fn path(&self) -> &Path { &self.path }
}
pub struct AtlasSession {
    atlas: RetainedWorkspaceAtlas,
    scoped: Option<(AtlasScope, RetainedWorkspaceAtlas)>,
    scope: AtlasScope,
    next_layout: u64,
    position: Position,
    selected: Option<AtlasNodeId>,
    history: Vec<Position>,
    pending: Option<VisiblePlan>,
    presented: Option<VisiblePlan>,
    frame: Option<PresentedFrameId>,
    last_attempt: u64,
    last_display_generation: u64,
    next_allocation: u64,
    visible: VisibleLimits,
    budget: ResourceBudget,
    _lease: ResourceLease,
}
impl AtlasSession {
    pub fn open(owner: ArenaOwnerId, root: &Path, options: AtlasSessionOptions,
        mut canceled: impl FnMut() -> bool) -> Result<Self, AtlasSessionError> {
        check(&mut canceled)?;
        if root.as_os_str().is_empty() || root.as_os_str().len() > 16_384 || !(1..=MAX_ATLAS_SESSION_FILES).contains(&options.max_files)
            || !(1..=MAX_ATLAS_SESSION_ITEMS).contains(&options.visible.max_items)
            || !(1..=MAX_ATLAS_SESSION_VISITS).contains(&options.visible.max_visits) {
            return Err(AtlasSessionError::InvalidLimits);
        }
        let display = display(owner, 1, options.width, options.height, options.scale)?;
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES)).map_err(|_| AppError::Admission)?;
        let lease = budget.try_reserve_managed(owner, allocation(4)?,
            ByteLength::new((256 * 1024 + MAX_ATLAS_HISTORY * size_of::<Position>() + size_of::<Self>()) as u64))
            .map_err(|_| AppError::Admission)?;
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let root = input::absolute(root)?;
        let metadata = fs::symlink_metadata(&root).map_err(|_| AppError::Io)?;
        if metadata.file_type().is_symlink() { return Err(AppError::Symlink.into()); }
        if !metadata.is_dir() { return Err(AppError::InvalidRange.into()); }
        let root = fs::canonicalize(root).map_err(|_| AppError::Io)?;
        let grant = RootGrant::new(RootId::new(owner, 1).map_err(|_| AtlasSessionError::IdentityExhausted)?, RawPath::from_path(&root));
        let limits = WorkspaceLimits { max_files: options.max_files, max_file_bytes: 0, max_source_bytes: 0,
            ..WorkspaceLimits::default() };
        let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner, 1).map_err(AppError::from)?,
            FileId::new(owner, 1).map_err(|_| AtlasSessionError::IdentityExhausted)?, limits, false, &budget, allocation(1)?)?;
        while catalog.stage() == WorkspaceStage::Discovering {
            check(&mut canceled)?;
            catalog.step(&CancelFlag::new())?;
        }
        let atlas = RetainedWorkspaceAtlas::build(catalog, LayoutRevision::new(owner, 1).map_err(|_| AtlasSessionError::IdentityExhausted)?,
            Size2D::new(4096.0, 4096.0).map_err(|_| AtlasSessionError::InvalidLimits)?, LayoutOptions::modest(),
            WorkspaceAtlasLimits::default(), &budget, [allocation(2)?, allocation(3)?], &mut canceled)?;
        let index = atlas.index()?;
        let focus = index.root_node();
        let camera = index.focus_camera(focus, camera_generation(owner, 1)?, display, 12.0)?;
        drop(index);
        let mut history = Vec::new();
        history.try_reserve_exact(MAX_ATLAS_HISTORY).map_err(|_| AppError::Admission)?;
        if history.capacity() > MAX_ATLAS_HISTORY { return Err(AppError::Admission.into()); }
        check(&mut canceled)?;
        Ok(Self { atlas, scoped: None, scope: AtlasScope::All, next_layout: 2,
            position: Position { focus, camera }, selected: None, history, pending: None,
            presented: None, frame: None, last_attempt: 0, last_display_generation: 1, next_allocation: 10,
            visible: options.visible, budget, _lease: lease })
    }
    /// The workspace-wide (All) atlas: the identity/search lane bound. Search
    /// and path queries validate against this layout so an explicit scope
    /// change never retires retained workspace-wide results or re-reads source.
    pub fn atlas(&self) -> &RetainedWorkspaceAtlas { &self.atlas }
    /// The atlas whose geometry the displayed frame describes: the All layout,
    /// or the retained scope layout while an extension scope is active.
    pub fn view(&self) -> &RetainedWorkspaceAtlas {
        if let Some((scope, atlas)) = &self.scoped {
            if *scope == self.scope { return atlas; }
        }
        &self.atlas
    }
    pub fn scope(&self) -> &AtlasScope { &self.scope }
    /// Displayed node of a workspace file in the ACTIVE layout, for focusing
    /// workspace-wide hits without re-reading source. Out-of-scope files have
    /// no displayed node.
    pub fn focus_target_for_file(&self, file: FileId) -> Option<AtlasNodeId> {
        self.view().node_for_file(file).ok()
    }
    /// Metadata-only scope membership for a workspace file: catalog path
    /// lookup and extension comparison. Never reads or parses source bytes.
    pub fn scope_matches_file(&self, file: FileId) -> bool {
        match self.atlas.catalog().entry(file) {
            Some(entry) => self.scope.matches_path(entry.path().as_bytes()),
            None => false,
        }
    }
    /// Wire identity of the active scope plus the workspace-wide file count.
    /// Extension tokens were validated UTF-8 at construction.
    pub fn encode_scope(&self, out: &mut Output) -> Result<(), AtlasSessionError> {
        out.literal(",\"scope\":")?;
        match &self.scope {
            AtlasScope::All => out.literal("\"all\"")?,
            AtlasScope::Extensions(extensions) => {
                out.literal("{\"kind\":\"extensions\",\"extensions\":[")?;
                for (i, token) in extensions.extensions().enumerate() {
                    if i != 0 { out.literal(",")?; }
                    out.quoted(std::str::from_utf8(token).unwrap_or(""))?;
                }
                out.literal("]}")?;
            }
        }
        out.literal(",\"workspace_files\":")?;
        out.integer(self.atlas.file_count() as u64)?;
        Ok(())
    }
    /// Named scope presets are pure extension sets; product naming lives at
    /// the host boundary, not in the layout engine.
    pub fn preset_scope(name: &str) -> Option<AtlasScope> {
        match name {
            "all" => Some(AtlasScope::All),
            "markdown" => Some(extension_scope(&["md"])?),
            "python" => Some(extension_scope(&["py"])?),
            "rust" => Some(extension_scope(&["rs"])?),
            _ => None,
        }
    }
    /// Switch the display scope. All is always served by the retained base
    /// layout, so returning to it is a cache hit by construction. One
    /// alternative scope layout is retained, so alternating between two
    /// extension scopes rebuilds the evicted one. Every rebuild mints a fresh
    /// layout revision; node identity is never reused across layouts.
    /// Reuse captured documents only: discovery is frozen, no source is read.
    fn change_scope(&mut self, next: AtlasScope, canceled: &mut impl FnMut() -> bool)
        -> Result<bool, AtlasSessionError> {
        if next == self.scope { return Ok(false); }
        if next.is_all() {
            self.scope = AtlasScope::All;
            return Ok(true);
        }
        let cached = self.scoped.as_ref().is_some_and(|(scope, _)| *scope == next);
        if !cached {
            let revision = LayoutRevision::new(self.owner(), self.next_layout)
                .map_err(|_| AtlasSessionError::IdentityExhausted)?;
            self.next_layout = self.next_layout.checked_add(1)
                .ok_or(AtlasSessionError::IdentityExhausted)?;
            let layout_alloc = self.next_id()?;
            let index_alloc = self.next_id()?;
            let catalog = self.atlas.catalog_shared();
            let world = Size2D::new(4096.0, 4096.0).map_err(|_| AtlasSessionError::InvalidLimits)?;
            let scoped = RetainedWorkspaceAtlas::build_scoped(catalog, &next, revision, world,
                LayoutOptions::modest(), WorkspaceAtlasLimits::default(), &self.budget,
                [layout_alloc, index_alloc],
                &mut || canceled() || self.atlas.validate_active().is_err())?;
            self.scoped = Some((next, scoped));
        }
        self.scope = next;
        Ok(true)
    }
    pub fn validate_active(&self) -> Result<(), AtlasSessionError> { Ok(self.atlas.validate_active()?) }
    pub fn pending_plan(&self) -> Option<&VisiblePlan> { self.pending.as_ref() }
    pub fn presented_plan(&self) -> Option<&VisiblePlan> { self.presented.as_ref() }

    /// A candidate replaces only the previous candidate, never the accepted
    /// picking frame. Failed/canceled preparation preserves camera/history/plan.
    /// Generation attempts are monotone even when work fails after admission.
    pub fn prepare(&mut self, generation: u64, action: AtlasAction, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSessionError> {
        self.validate_active()?;
        if generation == 0 || generation <= self.last_attempt { return Err(AtlasSessionError::StaleGeneration); }
        self.last_attempt = generation;
        check(&mut canceled)?;
        // Camera 1 belongs to the initial info state, before any plan exists.
        // Keep plan/query generations distinct from that initial camera identity.
        let camera_id = camera_generation(self.owner(), generation.checked_add(1)
            .ok_or(AtlasSessionError::IdentityExhausted)?)?;
        let plan_allocation = self.next_id()?;
        // An explicit scope change may repack from the frozen shared catalog
        // BEFORE the response header is encoded, so this plan always describes
        // the scope it displays. A failed change consumes its generation and
        // preserves the previous scope, camera, history and selection.
        let mut scope_changed = false;
        if let AtlasAction::Scope(next) = action {
            scope_changed = self.change_scope(next, &mut canceled)?;
        }
        let mut out = self.output("plan")?;
        let index = self.view().index()?;
        let old = self.position;
        let mut next = old;
        let mut selected = self.selected;
        let mut push = false;
        let mut pop = false;
        match action {
            AtlasAction::Scope(_) => {},
            AtlasAction::View => {},
            AtlasAction::Pan(delta) => next.camera = old.camera.pan(delta)?,
            AtlasAction::Zoom { anchor, factor } => next.camera = old.camera.zoom_at(anchor, factor)?,
            AtlasAction::Focus(ordinal) => {
                let node = AtlasNodeId::new(index.root(), index.revision(), ordinal);
                index.node(node)?;
                if node != old.focus {
                    if self.history.len() == MAX_ATLAS_HISTORY { return Err(AtlasSessionError::HistoryFull); }
                    push = true;
                }
                next.focus = node;
                next.camera = index.focus_camera(node, camera_id, old.camera.display(), 12.0)?;
                if self.view().file(node).is_ok() { selected = Some(node); }
            }
            AtlasAction::Back => {
                next = *self.history.last().ok_or(AtlasSessionError::EmptyHistory)?;
                next.camera = next.camera.with_display(old.camera.display())?;
                pop = true;
            }
            AtlasAction::Resize { width, height, scale } => {
                let dg = self.last_display_generation.checked_add(1).ok_or(AtlasSessionError::IdentityExhausted)?;
                self.last_display_generation = dg;
                next.camera = old.camera.with_display(display(self.owner(), dg, width, height, scale)?)?;
            }
        }
        if scope_changed {
            // History entries and selection identify nodes of the PREVIOUS
            // layout revision; carrying them across a repack would alias a
            // different file. A scope change is an explicit navigation reset
            // to the new layout's root.
            next.focus = index.root_node();
            next.camera = index.focus_camera(next.focus, camera_id, old.camera.display(), 12.0)?;
            selected = None;
            self.history.clear();
        }
            }
            AtlasAction::Back => {
                next = *self.history.last().ok_or(AtlasSessionError::EmptyHistory)?;
                next.camera = next.camera.with_display(old.camera.display())?;
                pop = true;
            }
            AtlasAction::Resize { width, height, scale } => {
                let dg = self.last_display_generation.checked_add(1).ok_or(AtlasSessionError::IdentityExhausted)?;
                self.last_display_generation = dg;
                next.camera = old.camera.with_display(display(self.owner(), dg, width, height, scale)?)?;
            }
        }
        // History restores transforms, not old camera identities.
        next.camera = Camera2D::new(camera_id, next.camera.display(),
            next.camera.origin(), next.camera.points_per_unit())?;
        let query_generation = QueryGeneration::new(self.owner(), generation).map_err(|_| AtlasSessionError::IdentityExhausted)?;
        let previous = self.pending.as_ref().or(self.presented.as_ref());
        let mut query = VisibleQuery::new(&index, next.focus, next.camera, query_generation, LodThresholds::default(),
            self.visible, previous, &self.budget, plan_allocation)?;
        while query.state() == VisibleState::Pending {
            self.validate_active()?;
            query.step(256, query_generation, &mut canceled)?;
        }
        let plan = query.finish()?;
        let limited = plan.stats().budget_aggregates != 0 || plan.stats().precision_aggregates != 0;
        out.literal(",\"plan_generation\":")?; out.integer(generation)?;
        out.literal(",\"host_acknowledgment_required\":true,\"native_presented\":false,\"detail_limited\":")?; out.boolean(limited)?;
        self.position_json(&mut out, next, selected)?;
        out.literal(",\"parcels\":[")?;
        for (i, parcel) in plan.parcels().iter().enumerate() {
            check(&mut canceled)?;
            if i != 0 { out.literal(",")?; }
            out.literal("{\"node\":")?; self.node_json(&mut out, parcel.node())?;
            out.literal(",\"detail\":")?; out.quoted(detail(parcel.detail()))?;
            out.literal(",\"logical_rect\":")?; rectangle(&mut out, parcel.logical_rect())?;
            out.literal(",\"represented_leaves\":")?; out.integer(parcel.represented_leaves() as u64)?;
            out.literal(",\"reason\":")?; out.quoted(&format!("{:?}", parcel.reason()))?;
            out.literal("}")?;
        }
        out.literal("],\"visited_nodes\":")?; out.integer(plan.stats().visited_nodes as u64)?;
        out.literal(",\"max_items\":")?; out.integer(self.visible.max_items as u64)?;
        out.literal(",\"max_visits\":")?; out.integer(self.visible.max_visits as u64)?;
        out.literal("}\n")?;
        drop(index);
        let response = self.finish(out, limited, &mut canceled)?;
        if push { self.history.push(old); }
        if pop { self.history.pop(); }
        self.position = next; self.selected = selected; self.pending = Some(plan);
        Ok(response)
    }

    /// The host declares what was actually displayed; this method cannot
    /// observe a native/GPU presentation. Old picking remains active while a
    /// newer candidate is prepared. Superseded unacknowledged plans are refused.
    pub fn acknowledge(&mut self, generation: u64, frame: u64, actual_display: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSessionError> {
        self.validate_active()?; check(&mut canceled)?;
        if self.frame.is_some_and(|old| frame <= old.get()) { return Err(AtlasSessionError::StaleFrame); }
        let frame = PresentedFrameId::new(self.owner(), frame).map_err(|_| AtlasSessionError::StaleFrame)?;
        let candidate = self.pending.as_ref().is_some_and(|plan| plan.generation().get() == generation);
        let plan = if candidate { self.pending.as_ref() } else { self.presented.as_ref().filter(|p| p.generation().get() == generation) }
            .ok_or(AtlasSessionError::NoPendingPlan)?;
        if plan.camera().display().generation().get() != actual_display { return Err(AtlasError::DisplayMismatch.into()); }
        plan.acknowledge_presented(frame, plan.camera().display())?;
        let mut out = self.output("acknowledge")?;
        out.literal(",\"plan_generation\":")?; out.integer(generation)?;
        out.literal(",\"frame\":")?; out.integer(frame.get())?;
        out.literal(",\"display_generation\":")?; out.integer(actual_display)?;
        out.literal(",\"presentation_evidence\":\"host-declared-not-observed\"}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        if candidate { self.presented = self.pending.take(); }
        self.frame = Some(frame);
        Ok(response)
    }
    pub fn pick(&mut self, frame: u64, actual_display: u64, point: Point2D,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSessionError> {
        check(&mut canceled)?;
        let hit = self.hit(frame, actual_display, point)?;
        let mut out = self.output("pick")?;
        out.literal(",\"frame\":")?; out.integer(frame)?;
        out.literal(",\"hit\":")?;
        if let Some(hit) = hit {
            out.literal("{\"node\":")?; self.node_json(&mut out, hit.node())?;
            out.literal(",\"detail\":")?; out.quoted(detail(hit.detail()))?;
            out.literal(",\"can_open_source\":")?; out.boolean(hit.detail() == AtlasDetail::File)?;
            out.literal("}")?;
        } else { out.literal("null")?; }
        out.literal("}\n")?;
        self.finish(out, false, &mut canceled)
    }
    fn hit(&self, frame: u64, actual_display: u64, point: Point2D) -> Result<Option<AtlasHit>, AtlasSessionError> {
        self.validate_active()?;
        let accepted = self.frame.ok_or(AtlasSessionError::NoPresentedPlan)?;
        if accepted.get() != frame { return Err(AtlasSessionError::StaleFrame); }
        let plan = self.presented.as_ref().ok_or(AtlasSessionError::NoPresentedPlan)?;
        let display = DisplayGeneration::new(self.owner(), actual_display).map_err(|_| AtlasError::DisplayMismatch)?;
        Ok(plan.acknowledge_presented(accepted, plan.camera().display())?.hit_test(point, display)?)
    }

    /// Explicit source action, separate from metadata-only picking. Revalidate
    /// the existing application path policy; do not open a label supplied by UI.
    /// The caller must include validate_active in its read cancellation scope.
    fn reader_target(&self, frame: u64, actual_display: u64, point: Point2D) -> Result<AtlasReadTarget, AtlasSessionError> {
        let hit = self.hit(frame, actual_display, point)?.ok_or(AtlasSessionError::NoFile)?;
        if hit.detail() != AtlasDetail::File { return Err(AtlasSessionError::NoFile); }
        let entry = self.view().entry(hit.node())?;
        let root = self.view().catalog().grant().root_path().to_path_buf();
        let path = workspace::checked_source_path(&root, entry.path()).map_err(|_| AppError::SourceChanged)?;
        self.validate_active()?;
        Ok(AtlasReadTarget { node: hit.node(), file: self.view().file(hit.node())?, frame, display: actual_display, path })
    }
    /// Safe host composition of acknowledged file selection and retained source.
    /// Reader ownership is distinct from atlas metadata identity and is reported
    /// explicitly. A failed activation never replaces the caller's older reader.
    pub fn open_reader(&mut self, reader_owner: ArenaOwnerId, frame: u64, actual_display: u64,
        point: Point2D, max_source_bytes: usize, mut canceled: impl FnMut() -> bool)
        -> Result<(ReaderSession, HostResponse), AtlasSessionError> {
        check(&mut canceled)?;
        if reader_owner == self.owner() { return Err(AtlasError::OwnerMismatch.into()); }
        let target = self.reader_target(frame, actual_display, point)?;
        let mut stop = || canceled() || self.validate_active().is_err();
        let mut reader = ReaderSession::open(reader_owner, target.path(), max_source_bytes, &mut stop)?;
        let info = reader.info(&mut stop)?;
        let response = self.reader_link(&target, reader_owner.get(), &info, &mut canceled)?;
        Ok((reader, response))
    }
    // Internal: called only with the info of the reader just captured above.
    // Foreign callers cannot relabel arbitrary HostResponse values as readers.
    fn reader_link(&mut self, target: &AtlasReadTarget, reader_owner: u64, reader_info: &HostResponse,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSessionError> {
        self.validate_active()?;
        if target.node.root().owner() != self.owner() || self.frame.map(|f| f.get()) != Some(target.frame)
            || self.presented.as_ref().map(|p| p.camera().display().generation().get()) != Some(target.display) {
            return Err(AtlasSessionError::StaleFrame);
        }
        if self.view().file(target.node)? != target.file { return Err(AtlasSessionError::NoFile); }
        let mut out = self.output("open-reader")?;
        out.literal(",\"frame\":")?; out.integer(target.frame)?;
        out.literal(",\"selected_node\":")?; self.node_json(&mut out, target.node)?;
        out.literal(",\"reader_owner\":")?; out.integer(reader_owner)?;
        out.literal(",\"source_observation\":\"new-capture-after-metadata-selection\",\"reader\":")?;
        out.literal(reader_info.as_str())?; out.literal("}\n")?;
        let response = self.finish(out, false, &mut canceled)?;
        self.selected = Some(target.node);
        Ok(response)
    }

    /// Conventional tree access for keyboard/nonspatial navigation. Rows retain
    /// raw paths and stable layout ordinals; no source payload or disk walk.
    pub fn children(&mut self, parent: u32, start: usize, limit: usize, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, AtlasSessionError> {
        self.validate_active()?; check(&mut canceled)?;
        if limit == 0 || limit > 1024 || start > self.view().layout().nodes().len() { return Err(AtlasSessionError::InvalidLimits); }
        let mut out = self.output("children")?;
        let index = self.view().index()?;
        let parent = AtlasNodeId::new(index.root(), index.revision(), parent);
        let mut children = index.children(parent)?.skip(start);
        out.literal(",\"parent\":")?; self.node_json(&mut out, parent)?;
        out.literal(",\"rows\":[")?;
        let mut count = 0usize;
        for node in children.by_ref().take(limit) {
            check(&mut canceled)?;
            if count != 0 { out.literal(",")?; }
            self.node_json(&mut out, node)?; count += 1;
        }
        let more = children.next().is_some();
        out.literal("],\"next_offset\":")?;
        if more { out.integer((start + count) as u64)?; } else { out.literal("null")?; }
        out.literal("}\n")?;
        drop(children); drop(index);
        self.finish(out, more, &mut canceled)
    }
    pub fn info(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSessionError> {
        self.validate_active()?; check(&mut canceled)?;
        let mut out = self.output("info")?;
        self.position_json(&mut out, self.position, self.selected)?;
        out.literal(",\"history_depth\":")?; out.integer(self.history.len() as u64)?;
        out.literal(",\"pending_plan_generation\":")?; optional(&mut out, self.pending.as_ref().map(|p| p.generation().get()))?;
        out.literal(",\"presented_plan_generation\":")?; optional(&mut out, self.presented.as_ref().map(|p| p.generation().get()))?;
        out.literal(",\"frame\":")?; optional(&mut out, self.frame.map(|f| f.get()))?;
        out.literal(",\"last_attempted_generation\":")?; out.integer(self.last_attempt)?;
        out.literal("}\n")?;
        self.finish(out, false, &mut canceled)
    }
    fn owner(&self) -> ArenaOwnerId { self.atlas.layout().owner() }
    fn next_id(&mut self) -> Result<ResourceAllocationId, AtlasSessionError> {
        let value = self.next_allocation;
        self.next_allocation = value.checked_add(1).ok_or(AtlasSessionError::IdentityExhausted)?;
        allocation(value)
    }
    fn output(&mut self, command: &str) -> Result<Output, AtlasSessionError> {
        let id = self.next_id()?;
        let mut out = Output::new(self.owner(), MAX_RESPONSE_BYTES, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.atlas-session/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.owner().get())?;
        out.literal(",\"layout_revision\":")?; out.integer(self.view().layout().revision().get())?;
        out.literal(",\"discovery_complete\":")?; out.boolean(self.view().discovery_complete())?;
        out.literal(",\"catalogued_files\":")?; out.integer(self.view().file_count() as u64)?;
        self.encode_scope(&mut out)?;
        out.literal(",\"retention\":\"frozen-catalog-and-spatial-index\"")?;
        Ok(out)
    }
    fn finish(&mut self, out: Output, partial: bool, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, AtlasSessionError> {
        self.validate_active()?; check(canceled)?;
        let charge = out.as_bytes().len().checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(AppError::Admission)?;
        let id = self.next_id()?;
        let lease = self.budget.try_reserve_managed(self.owner(), id, ByteLength::new(charge as u64)).map_err(|_| AppError::Admission)?;
        let mut text = String::new();
        text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
        if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
        self.validate_active()?; check(canceled)?;
        Ok(HostResponse { text, exit_code: if partial || !self.atlas.discovery_complete() { EXIT_PARTIAL } else { EXIT_OK }, _lease: lease })
    }
    fn position_json(&self, out: &mut Output, position: Position, selected: Option<AtlasNodeId>) -> Result<(), AtlasSessionError> {
        out.literal(",\"focus\":")?; self.node_json(out, position.focus)?;
        out.literal(",\"selection\":")?;
        if let Some(node) = selected { self.node_json(out, node)?; } else { out.literal("null")?; }
        let c = position.camera;
        out.literal(",\"camera\":{\"generation\":")?; out.integer(c.generation().get())?;
        out.literal(",\"display_generation\":")?; out.integer(c.display().generation().get())?;
        out.literal(",\"origin_x\":")?; number(out, c.origin().x())?;
        out.literal(",\"origin_y\":")?; number(out, c.origin().y())?;
        out.literal(",\"points_per_unit\":")?; number(out, c.points_per_unit())?;
        out.literal(",\"width\":")?; number(out, c.display().logical_size().width())?;
        out.literal(",\"height\":")?; number(out, c.display().logical_size().height())?;
        out.literal(",\"scale\":")?; number(out, c.display().scale_factor())?;
        out.literal("}")?; Ok(())
    }
    fn node_json(&self, out: &mut Output, key: AtlasNodeId) -> Result<(), AtlasSessionError> {
        let index = self.view().index()?;
        let node = index.node(key)?;
        out.literal("{\"ordinal\":")?; out.integer(u64::from(key.ordinal()))?;
        out.literal(",\"path\":")?; out.path(&RawPath::from_bytes(node.path()).to_path_buf())?;
        out.literal(",\"file_id\":")?;
        match self.view().file(key) { Ok(file) => out.integer(file.get())?, Err(_) => out.literal("null")? }
        out.literal(",\"kind\":")?; out.quoted(match node.kind() {
            fcb::map::NodeKind::File => "file", fcb::map::NodeKind::Directory => "directory", fcb::map::NodeKind::Placeholder => "placeholder" })?;
        out.literal("}")?; Ok(())
    }
}

/// Re-exported for host FFI marshaling; the scope type is engine vocabulary
/// and directly usable inside this module.
pub use fcb::map::workspace::{AtlasExtensionScope, AtlasScope, AtlasScopeError};
fn extension_scope(tokens: &[&str]) -> Option<AtlasScope> {
    AtlasExtensionScope::from_extensions(tokens.iter().map(|token| token.as_bytes()))
        .ok().map(AtlasScope::Extensions)
}
fn allocation(value: u64) -> Result<ResourceAllocationId, AtlasSessionError> {
    ResourceAllocationId::new(value).map_err(|_| AtlasSessionError::IdentityExhausted)
}
fn camera_generation(owner: ArenaOwnerId, value: u64) -> Result<CameraGeneration, AtlasSessionError> {
    CameraGeneration::new(owner, value).map_err(|_| AtlasSessionError::IdentityExhausted)
}
fn display(owner: ArenaOwnerId, generation: u64, width: f64, height: f64, scale: f64) -> Result<DisplayMetrics, AtlasSessionError> {
    if !width.is_finite() || !height.is_finite() || !scale.is_finite()
        || !(64.0..=16_384.0).contains(&width) || !(64.0..=16_384.0).contains(&height) || !(0.25..=8.0).contains(&scale) {
        return Err(AtlasSessionError::InvalidLimits);
    }
    DisplayMetrics::new(scale, Size2D::new(width, height).map_err(|_| AtlasSessionError::InvalidLimits)?, DisplayColorConfig::Srgb,
        DisplayGeneration::new(owner, generation).map_err(|_| AtlasSessionError::IdentityExhausted)?)
        .map_err(|_| AtlasSessionError::InvalidLimits)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSessionError> {
    if canceled() { Err(AtlasSessionError::Canceled) } else { Ok(()) }
}
fn optional(out: &mut Output, value: Option<u64>) -> Result<(), OutputError> {
    match value { Some(value) => out.integer(value), None => out.literal("null") }
}
fn number(out: &mut Output, value: f64) -> Result<(), AtlasSessionError> {
    if !value.is_finite() { return Err(AtlasSessionError::InvalidLimits); }
    out.literal(&value.to_string())?; Ok(())
}
fn rectangle(out: &mut Output, rect: Rect2D) -> Result<(), AtlasSessionError> {
    out.literal("{\"x\":")?; number(out, rect.min_x())?;
    out.literal(",\"y\":")?; number(out, rect.min_y())?;
    out.literal(",\"width\":")?; number(out, rect.size().width())?;
    out.literal(",\"height\":")?; number(out, rect.size().height())?;
    out.literal("}")?; Ok(())
}
fn detail(detail: AtlasDetail) -> &'static str {
    match detail { AtlasDetail::File => "file", AtlasDetail::Placeholder => "placeholder", AtlasDetail::Directory => "directory", AtlasDetail::SiblingGroup => "sibling-group" }
}
