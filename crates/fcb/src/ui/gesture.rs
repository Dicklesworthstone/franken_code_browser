//! Explicit gesture arbitration table and gesture ownership model.
//!
//! Under §15.6 and §5.3:
//! - The hit region selected at gesture start owns the gesture exclusively until
//!   completion or cancellation.
//! - Scrolling inside a reading lens body or code block scrolls that content
//!   and must not also pan the atlas.
//! - Dragging a reading lens title bar moves the pane on screen without altering atlas layout.
//! - Dragging inside a reading lens body performs text selection.
//! - Dragging on the atlas background pans the camera (or orbits in City mode with explicit modifier).
//! - Escape cancels an active gesture before changing workspace scope.

#![forbid(unsafe_code)]

/// Screen hit regions that can originate or receive gestures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum HitRegion {
    /// Spatial atlas 2D/3D surface.
    AtlasBackground,
    /// Draggable title bar of a specific reading pane.
    ReadingLensTitleBar(u64),
    /// Content / text body of a specific reading pane.
    ReadingLensBody(u64),
    /// Vertical splitter between sidebar and atlas.
    SidebarSplitter,
    /// Sidebar panel content (Inspector, Results, History, Outline).
    SidebarContent,
    /// Scope breadcrumb trail.
    Breadcrumbs,
    /// Modal search palette.
    SearchPalette,
    /// Conventional tree projection view.
    TreeProjection,
}

/// Pointer buttons.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PointerButton {
    Left,
    Middle,
    Right,
    Other(u16),
}

/// Keyboard modifier keys active during a gesture or input.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct ModifierKeys {
    pub shift: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub cmd: bool,
    pub space: bool,
}

impl ModifierKeys {
    pub const fn none() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: false,
            cmd: false,
            space: false,
        }
    }

    pub const fn space() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: false,
            cmd: false,
            space: true,
        }
    }

    pub const fn alt() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: true,
            cmd: false,
            space: false,
        }
    }

    pub const fn cmd() -> Self {
        Self {
            shift: false,
            ctrl: false,
            alt: false,
            cmd: true,
            space: false,
        }
    }
}

/// Active gesture category.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum GestureKind {
    /// Panning the camera over the spatial atlas.
    CameraPan,
    /// Zooming the camera around a gesture centroid.
    PinchZoom,
    /// Orbiting the camera in City mode (requires deliberate modifier drag).
    CameraOrbit,
    /// Translating a reading lens floating pane on screen.
    LensDrag { pane_id: u64 },
    /// Selecting text within a reading lens content body.
    TextSelect { pane_id: u64 },
    /// Resizing the sidebar via the splitter bar.
    SidebarResize,
}

/// Routing decision for scroll / wheel events.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum ScrollRouting {
    /// Isolated scroll inside a reading lens; atlas is untouched.
    LensScroll { pane_id: u64, dx: f32, dy: f32 },
    /// Panning the spatial atlas.
    AtlasPan { dx: f32, dy: f32 },
    /// Zooming the spatial atlas camera.
    AtlasZoom { factor: f32 },
    /// Scrolling inside sidebar panels.
    SidebarScroll { dy: f32 },
    /// Scrolling inside conventional tree projection.
    TreeScroll { dy: f32 },
    /// Scroll was ignored or consumed without action.
    Ignored,
}

/// Current lifecycle state of gesture ownership.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum GestureState {
    /// No gesture is actively owning the pointer.
    Idle,
    /// A gesture has claimed ownership of pointer motion.
    Active {
        kind: GestureKind,
        owner_region: HitRegion,
        start_pos: (f32, f32),
        current_pos: (f32, f32),
        total_delta: (f32, f32),
    },
    /// Gesture was interrupted / cancelled by Escape or blur.
    Cancelled,
}

/// Explicit arbitration table that enforces single gesture ownership.
#[derive(Clone, Debug, PartialEq)]
pub struct GestureArbitrator {
    state: GestureState,
}

impl GestureArbitrator {
    pub const fn new() -> Self {
        Self {
            state: GestureState::Idle,
        }
    }

    pub const fn state(&self) -> GestureState {
        self.state
    }

    pub const fn is_active(&self) -> bool {
        matches!(self.state, GestureState::Active { .. })
    }

    pub const fn owner_region(&self) -> Option<HitRegion> {
        match self.state {
            GestureState::Active { owner_region, .. } => Some(owner_region),
            _ => None,
        }
    }

    pub const fn current_kind(&self) -> Option<GestureKind> {
        match self.state {
            GestureState::Active { kind, .. } => Some(kind),
            _ => None,
        }
    }

    /// Attempt to initiate a drag gesture at `start_pos` on `region`.
    ///
    /// The hit region selected at gesture start owns the gesture until completion.
    /// Returns the assigned `GestureKind`, or `None` if a gesture is already active
    /// or if the region does not support dragging with the given inputs.
    pub fn start_pointer_drag(
        &mut self,
        region: HitRegion,
        start_pos: (f32, f32),
        button: PointerButton,
        modifiers: ModifierKeys,
        is_city_mode: bool,
    ) -> Option<GestureKind> {
        // If already active, the existing gesture owns the interaction.
        if self.is_active() {
            return None;
        }

        let kind = match (region, button) {
            // Dragging reading lens title bar translates the pane
            (HitRegion::ReadingLensTitleBar(pane_id), PointerButton::Left) => {
                GestureKind::LensDrag { pane_id }
            }

            // Dragging inside reading lens body selects text
            (HitRegion::ReadingLensBody(pane_id), PointerButton::Left) => {
                GestureKind::TextSelect { pane_id }
            }

            // Dragging splitter handle resizes sidebar
            (HitRegion::SidebarSplitter, PointerButton::Left) => GestureKind::SidebarResize,

            // Dragging atlas background
            (HitRegion::AtlasBackground, PointerButton::Left) => {
                if is_city_mode && modifiers.alt {
                    // Deliberate modifier required for orbit in City mode
                    GestureKind::CameraOrbit
                } else {
                    GestureKind::CameraPan
                }
            }

            // Space + Left drag or Middle drag pans the atlas from anywhere on atlas
            (HitRegion::AtlasBackground, PointerButton::Middle) => GestureKind::CameraPan,

            _ => return None,
        };

        self.state = GestureState::Active {
            kind,
            owner_region: region,
            start_pos,
            current_pos: start_pos,
            total_delta: (0.0, 0.0),
        };

        Some(kind)
    }

    /// Update pointer position during an active gesture.
    ///
    /// Returns the gesture kind and the incremental `(dx, dy)` delta since last update.
    pub fn update_pointer_move(&mut self, new_pos: (f32, f32)) -> Option<(GestureKind, (f32, f32))> {
        match &mut self.state {
            GestureState::Active {
                kind,
                current_pos,
                total_delta,
                ..
            } => {
                let dx = new_pos.0 - current_pos.0;
                let dy = new_pos.1 - current_pos.1;
                *current_pos = new_pos;
                total_delta.0 += dx;
                total_delta.1 += dy;
                Some((*kind, (dx, dy)))
            }
            _ => None,
        }
    }

    /// Complete the active gesture on pointer release.
    pub fn complete_pointer(&mut self) -> Option<GestureKind> {
        match self.state {
            GestureState::Active { kind, .. } => {
                self.state = GestureState::Idle;
                Some(kind)
            }
            _ => None,
        }
    }

    /// Cancel the active gesture (e.g. on Escape or window focus lost).
    ///
    /// Returns `true` if an active gesture was cancelled.
    pub fn cancel(&mut self) -> bool {
        if self.is_active() {
            self.state = GestureState::Cancelled;
            true
        } else {
            false
        }
    }

    /// Reset cancelled or finished state back to Idle.
    pub fn reset_idle(&mut self) {
        self.state = GestureState::Idle;
    }

    /// Route a scroll/wheel event faithfully according to the hit region arbitration table.
    ///
    /// Scrolling inside a reading lens scrolls that lens and MUST NOT pan the atlas!
    pub fn route_scroll(
        &self,
        region: HitRegion,
        delta: (f32, f32),
        modifiers: ModifierKeys,
    ) -> ScrollRouting {
        match region {
            HitRegion::ReadingLensBody(pane_id) | HitRegion::ReadingLensTitleBar(pane_id) => {
                // Isolated scroll inside reading lens; atlas is never panned!
                ScrollRouting::LensScroll {
                    pane_id,
                    dx: delta.0,
                    dy: delta.1,
                }
            }

            HitRegion::AtlasBackground => {
                if modifiers.cmd || modifiers.ctrl {
                    // Zoom around pointer
                    let factor = 1.0 + (delta.1 * 0.005);
                    ScrollRouting::AtlasZoom { factor }
                } else {
                    // Two-finger scroll pans atlas by default
                    ScrollRouting::AtlasPan {
                        dx: delta.0,
                        dy: delta.1,
                    }
                }
            }

            HitRegion::SidebarContent => ScrollRouting::SidebarScroll { dy: delta.1 },

            HitRegion::TreeProjection => ScrollRouting::TreeScroll { dy: delta.1 },

            HitRegion::SidebarSplitter
            | HitRegion::Breadcrumbs
            | HitRegion::SearchPalette => ScrollRouting::Ignored,
        }
    }

    /// Route a pinch zoom gesture.
    pub fn route_pinch(&self, region: HitRegion, magnification: f32) -> Option<f32> {
        match region {
            HitRegion::AtlasBackground => Some(1.0 + magnification),
            _ => None,
        }
    }
}

impl Default for GestureArbitrator {
    fn default() -> Self {
        Self::new()
    }
}
