#![forbid(unsafe_code)]

//! Interruptible navigation history, semantic endpoints, and bounded camera flight.
//!
//! Camera history records intentional navigation endpoints (such as fit-selection,
//! fit-project, or jump-to-bookmark), not raw mouse wheel ticks or subpixel drags.
//! Navigation flights provide smooth, bounded interpolation that clamps extreme elapsed
//! time deltas after system sleep and immediately yields control back to user gestures
//! upon interruption. In reduced-motion mode, flights are replaced with direct transitions.

use std::collections::VecDeque;

use fcb_core::{ArenaOwnerId, Point2D};
use crate::camera::{Camera2D, CameraError};

/// Default flight duration in nanoseconds (250 milliseconds).
pub const DEFAULT_FLIGHT_DURATION_NANOS: u64 = 250_000_000;

/// Default maximum step delta in nanoseconds (100 milliseconds) to clamp sleep pauses.
pub const DEFAULT_MAX_STEP_DELTA_NANOS: u64 = 100_000_000;

/// Default maximum navigation history capacity.
pub const DEFAULT_MAX_HISTORY_CAPACITY: usize = 64;

/// Semantic reasons for camera navigation.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
#[non_exhaustive]
pub enum NavigationReason {
    FitProject,
    FitParent,
    FitSelection,
    FocusNode,
    JumpToBookmark,
    HistoryBack,
    HistoryForward,
    KeyboardReplay,
    ExplicitTarget,
}

impl NavigationReason {
    pub const fn name(self) -> &'static str {
        match self {
            Self::FitProject => "fit-project",
            Self::FitParent => "fit-parent",
            Self::FitSelection => "fit-selection",
            Self::FocusNode => "focus-node",
            Self::JumpToBookmark => "jump-to-bookmark",
            Self::HistoryBack => "history-back",
            Self::HistoryForward => "history-forward",
            Self::KeyboardReplay => "keyboard-replay",
            Self::ExplicitTarget => "explicit-target",
        }
    }
}

/// Motion preference governing transitions.
#[derive(Clone, Copy, Debug, Default, Eq, Hash, PartialEq)]
pub enum MotionPreference {
    #[default]
    Normal,
    ReducedMotion,
}

/// Errors originating from camera navigation and history operations.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
#[non_exhaustive]
pub enum NavigationError {
    Camera(CameraError),
    EmptyHistory,
    InvalidDuration,
    InvalidGeometry,
    OwnerMismatch,
}

impl std::fmt::Display for NavigationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Camera(err) => write!(f, "NAVIGATION_CAMERA_ERROR: {err}"),
            Self::EmptyHistory => f.write_str("NAVIGATION_EMPTY_HISTORY"),
            Self::InvalidDuration => f.write_str("NAVIGATION_INVALID_DURATION"),
            Self::InvalidGeometry => f.write_str("NAVIGATION_INVALID_GEOMETRY"),
            Self::OwnerMismatch => f.write_str("NAVIGATION_OWNER_MISMATCH"),
        }
    }
}

impl std::error::Error for NavigationError {}

impl From<CameraError> for NavigationError {
    fn from(err: CameraError) -> Self {
        Self::Camera(err)
    }
}

/// Configuration parameters for camera navigation flights.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct NavigationFlightConfig {
    pub duration_nanos: u64,
    pub max_step_delta_nanos: u64,
    pub motion_preference: MotionPreference,
}

impl Default for NavigationFlightConfig {
    fn default() -> Self {
        Self {
            duration_nanos: DEFAULT_FLIGHT_DURATION_NANOS,
            max_step_delta_nanos: DEFAULT_MAX_STEP_DELTA_NANOS,
            motion_preference: MotionPreference::Normal,
        }
    }
}

impl NavigationFlightConfig {
    pub fn new(
        duration_nanos: u64,
        max_step_delta_nanos: u64,
        motion_preference: MotionPreference,
    ) -> Result<Self, NavigationError> {
        if duration_nanos == 0 || max_step_delta_nanos == 0 {
            return Err(NavigationError::InvalidDuration);
        }
        Ok(Self {
            duration_nanos,
            max_step_delta_nanos,
            motion_preference,
        })
    }
}

/// One semantic navigation history entry.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct HistoryEntry {
    camera: Camera2D,
    reason: NavigationReason,
    timestamp_nanos: u64,
}

impl HistoryEntry {
    pub fn new(camera: Camera2D, reason: NavigationReason, timestamp_nanos: u64) -> Self {
        Self {
            camera,
            reason,
            timestamp_nanos,
        }
    }

    pub fn camera(&self) -> Camera2D {
        self.camera
    }

    pub fn reason(&self) -> NavigationReason {
        self.reason
    }

    pub fn timestamp_nanos(&self) -> u64 {
        self.timestamp_nanos
    }
}

/// Smoothstep cubic easing: 3t^2 - 2t^3.
pub fn smooth_step(t: f64) -> f64 {
    let clamped = t.clamp(0.0, 1.0);
    clamped * clamped * (3.0 - 2.0 * clamped)
}

/// An active or interrupted camera flight between two explicit states.
#[derive(Clone, Debug, PartialEq)]
pub struct NavigationFlight {
    start: Camera2D,
    target: Camera2D,
    elapsed_nanos: u64,
    config: NavigationFlightConfig,
    reason: NavigationReason,
    interrupted: bool,
    interrupted_camera: Option<Camera2D>,
}

impl NavigationFlight {
    pub fn new(
        start: Camera2D,
        target: Camera2D,
        reason: NavigationReason,
        config: NavigationFlightConfig,
    ) -> Result<Self, NavigationError> {
        if start.generation().owner() != target.generation().owner() {
            return Err(NavigationError::OwnerMismatch);
        }
        if config.duration_nanos == 0 || config.max_step_delta_nanos == 0 {
            return Err(NavigationError::InvalidDuration);
        }

        let is_reduced = config.motion_preference == MotionPreference::ReducedMotion;
        let elapsed = if is_reduced {
            config.duration_nanos
        } else {
            0
        };

        Ok(Self {
            start,
            target,
            elapsed_nanos: elapsed,
            config,
            reason,
            interrupted: false,
            interrupted_camera: None,
        })
    }

    pub fn start(&self) -> Camera2D {
        self.start
    }

    pub fn target(&self) -> Camera2D {
        self.target
    }

    pub fn reason(&self) -> NavigationReason {
        self.reason
    }

    pub fn elapsed_nanos(&self) -> u64 {
        self.elapsed_nanos
    }

    pub fn is_complete(&self) -> bool {
        !self.interrupted && self.elapsed_nanos >= self.config.duration_nanos
    }

    pub fn is_interrupted(&self) -> bool {
        self.interrupted
    }

    /// Progress normalized to [0.0, 1.0].
    pub fn progress(&self) -> f64 {
        if self.config.duration_nanos == 0 {
            1.0
        } else {
            (self.elapsed_nanos as f64 / self.config.duration_nanos as f64).clamp(0.0, 1.0)
        }
    }

    /// Returns the current camera state during flight.
    ///
    /// When complete, returns the exact target camera without rounding or interpolation drift.
    /// When interrupted, returns the exact snapshot captured at the moment of interruption.
    pub fn current_camera(&self) -> Result<Camera2D, NavigationError> {
        if self.interrupted {
            if let Some(cam) = self.interrupted_camera {
                return Ok(cam);
            }
        }
        if self.is_complete() {
            return Ok(self.target);
        }
        if self.elapsed_nanos == 0 {
            return Ok(self.start);
        }

        let p = self.progress();
        let s = smooth_step(p);

        // Interpolate origin smoothly in local atlas space
        let x0 = self.start.origin().x();
        let y0 = self.start.origin().y();
        let x1 = self.target.origin().x();
        let y1 = self.target.origin().y();

        let cur_x = (1.0 - s) * x0 + s * x1;
        let cur_y = (1.0 - s) * y0 + s * y1;

        // Interpolate scale log-linearly so zoom remains perceptually uniform
        let scale0 = self.start.points_per_unit();
        let scale1 = self.target.points_per_unit();
        let cur_scale = if (scale0 - scale1).abs() < 1e-12 {
            scale0
        } else {
            let ln0 = scale0.ln();
            let ln1 = scale1.ln();
            ((1.0 - s) * ln0 + s * ln1).exp()
        };

        let next_gen = self.start.next_generation().map_err(NavigationError::Camera)?;
        let pt = Point2D::new(cur_x, cur_y).map_err(|_| NavigationError::InvalidGeometry)?;
        Camera2D::new(next_gen, self.target.display(), pt, cur_scale)
            .map_err(NavigationError::Camera)
    }

    /// Advance elapsed flight time, clamping extreme elapsed deltas after system sleep.
    pub fn step(&mut self, dt_nanos: u64) -> Result<Camera2D, NavigationError> {
        if self.interrupted || self.is_complete() {
            return self.current_camera();
        }

        // Clamp extreme intervals after system sleep
        let effective_dt = dt_nanos.min(self.config.max_step_delta_nanos);
        self.elapsed_nanos = self
            .elapsed_nanos
            .saturating_add(effective_dt)
            .min(self.config.duration_nanos);

        self.current_camera()
    }

    /// Immediately interrupt flight, handing control back to user gestures without fighting input.
    pub fn interrupt(&mut self) -> Result<Camera2D, NavigationError> {
        if !self.interrupted {
            let cam = self.current_camera()?;
            self.interrupted = true;
            self.interrupted_camera = Some(cam);
            Ok(cam)
        } else {
            self.current_camera()
        }
    }
}

/// Bounded navigation history stack with back/forward support and interruptible flight transitions.
#[derive(Clone, Debug, PartialEq)]
pub struct NavigationHistory {
    owner: ArenaOwnerId,
    past: VecDeque<HistoryEntry>,
    current: Option<HistoryEntry>,
    future: VecDeque<HistoryEntry>,
    max_capacity: usize,
    config: NavigationFlightConfig,
    active_flight: Option<NavigationFlight>,
}

impl NavigationHistory {
    pub fn new(
        owner: ArenaOwnerId,
        max_capacity: usize,
        config: NavigationFlightConfig,
    ) -> Self {
        let max_capacity = if max_capacity == 0 {
            DEFAULT_MAX_HISTORY_CAPACITY
        } else {
            max_capacity
        };
        Self {
            owner,
            past: VecDeque::with_capacity(max_capacity),
            current: None,
            future: VecDeque::with_capacity(max_capacity),
            max_capacity,
            config,
            active_flight: None,
        }
    }

    pub fn with_initial_camera(
        owner: ArenaOwnerId,
        camera: Camera2D,
        config: NavigationFlightConfig,
    ) -> Result<Self, NavigationError> {
        if camera.generation().owner() != owner {
            return Err(NavigationError::OwnerMismatch);
        }
        let mut history = Self::new(owner, DEFAULT_MAX_HISTORY_CAPACITY, config);
        history.current = Some(HistoryEntry::new(
            camera,
            NavigationReason::FitProject,
            0,
        ));
        Ok(history)
    }

    pub fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn max_capacity(&self) -> usize {
        self.max_capacity
    }

    pub fn config(&self) -> &NavigationFlightConfig {
        &self.config
    }

    pub fn set_motion_preference(&mut self, preference: MotionPreference) {
        self.config.motion_preference = preference;
        if preference == MotionPreference::ReducedMotion {
            if let Some(flight) = self.active_flight.take() {
                // Instantly complete flight at exact target
                self.current = Some(HistoryEntry::new(
                    flight.target,
                    flight.reason,
                    flight.elapsed_nanos,
                ));
            }
        }
    }

    pub fn active_flight(&self) -> Option<&NavigationFlight> {
        self.active_flight.as_ref()
    }

    pub fn can_go_back(&self) -> bool {
        !self.past.is_empty()
    }

    pub fn can_go_forward(&self) -> bool {
        !self.future.is_empty()
    }

    pub fn past_count(&self) -> usize {
        self.past.len()
    }

    pub fn future_count(&self) -> usize {
        self.future.len()
    }

    /// The current effective camera. If a flight is active, returns its current interpolated position.
    pub fn current_camera(&self) -> Result<Camera2D, NavigationError> {
        if let Some(ref flight) = self.active_flight {
            flight.current_camera()
        } else if let Some(ref curr) = self.current {
            Ok(curr.camera)
        } else {
            Err(NavigationError::EmptyHistory)
        }
    }

    /// Push an intentional semantic endpoint (e.g. FitSelection, FocusNode), clearing forward history.
    pub fn push_semantic_endpoint(
        &mut self,
        target_camera: Camera2D,
        reason: NavigationReason,
        timestamp_nanos: u64,
    ) -> Result<(), NavigationError> {
        if target_camera.generation().owner() != self.owner {
            return Err(NavigationError::OwnerMismatch);
        }

        // Interrupt any existing flight immediately
        let start_camera = if let Some(mut flight) = self.active_flight.take() {
            flight.interrupt()?
        } else if let Some(ref curr) = self.current {
            curr.camera
        } else {
            target_camera
        };

        if let Some(prev) = self.current.take() {
            if self.past.len() >= self.max_capacity {
                let _ = self.past.pop_front();
            }
            self.past.push_back(prev);
        }

        // Clearing forward history when a new branch is created
        self.future.clear();

        let new_entry = HistoryEntry::new(target_camera, reason, timestamp_nanos);
        self.current = Some(new_entry);

        // Start flight unless reduced motion is selected or start == target
        if self.config.motion_preference == MotionPreference::ReducedMotion
            || start_camera == target_camera
        {
            self.active_flight = None;
        } else {
            self.active_flight = Some(NavigationFlight::new(
                start_camera,
                target_camera,
                reason,
                self.config,
            )?);
        }

        Ok(())
    }

    /// Navigate back in semantic history.
    pub fn navigate_back(&mut self, timestamp_nanos: u64) -> Result<Camera2D, NavigationError> {
        let prev_entry = self.past.pop_back().ok_or(NavigationError::EmptyHistory)?;

        let start_camera = if let Some(mut flight) = self.active_flight.take() {
            flight.interrupt()?
        } else if let Some(ref curr) = self.current {
            curr.camera
        } else {
            prev_entry.camera
        };

        if let Some(curr) = self.current.take() {
            if self.future.len() >= self.max_capacity {
                let _ = self.future.pop_back();
            }
            self.future.push_front(curr);
        }

        self.current = Some(HistoryEntry::new(
            prev_entry.camera,
            NavigationReason::HistoryBack,
            timestamp_nanos,
        ));

        if self.config.motion_preference == MotionPreference::ReducedMotion
            || start_camera == prev_entry.camera
        {
            self.active_flight = None;
            Ok(prev_entry.camera)
        } else {
            let flight = NavigationFlight::new(
                start_camera,
                prev_entry.camera,
                NavigationReason::HistoryBack,
                self.config,
            )?;
            let target = flight.target;
            self.active_flight = Some(flight);
            Ok(target)
        }
    }

    /// Navigate forward in semantic history.
    pub fn navigate_forward(&mut self, timestamp_nanos: u64) -> Result<Camera2D, NavigationError> {
        let next_entry = self.future.pop_front().ok_or(NavigationError::EmptyHistory)?;

        let start_camera = if let Some(mut flight) = self.active_flight.take() {
            flight.interrupt()?
        } else if let Some(ref curr) = self.current {
            curr.camera
        } else {
            next_entry.camera
        };

        if let Some(curr) = self.current.take() {
            if self.past.len() >= self.max_capacity {
                let _ = self.past.pop_front();
            }
            self.past.push_back(curr);
        }

        self.current = Some(HistoryEntry::new(
            next_entry.camera,
            NavigationReason::HistoryForward,
            timestamp_nanos,
        ));

        if self.config.motion_preference == MotionPreference::ReducedMotion
            || start_camera == next_entry.camera
        {
            self.active_flight = None;
            Ok(next_entry.camera)
        } else {
            let flight = NavigationFlight::new(
                start_camera,
                next_entry.camera,
                NavigationReason::HistoryForward,
                self.config,
            )?;
            let target = flight.target;
            self.active_flight = Some(flight);
            Ok(target)
        }
    }

    /// Advance active flight by `dt_nanos`, returning current position or `None` if idle.
    pub fn step_flight(&mut self, dt_nanos: u64) -> Result<Option<Camera2D>, NavigationError> {
        let Some(ref mut flight) = self.active_flight else {
            return Ok(None);
        };

        let cam = flight.step(dt_nanos)?;
        if flight.is_complete() {
            let target = flight.target;
            self.active_flight = None;
            return Ok(Some(target));
        }

        Ok(Some(cam))
    }

    /// Immediately interrupt the active flight and hand control back to the user.
    pub fn interrupt_flight(&mut self) -> Result<Option<Camera2D>, NavigationError> {
        if let Some(mut flight) = self.active_flight.take() {
            let cam = flight.interrupt()?;
            if let Some(ref mut curr) = self.current {
                curr.camera = cam;
            }
            Ok(Some(cam))
        } else {
            Ok(None)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{CameraGeneration, DisplayColorConfig, DisplayGeneration, DisplayMetrics, Size2D};

    fn owner() -> ArenaOwnerId {
        ArenaOwnerId::new(0x0C15).unwrap()
    }

    fn foreign_owner() -> ArenaOwnerId {
        ArenaOwnerId::new(0xDEAD).unwrap()
    }

    fn test_display(owner_id: ArenaOwnerId, id: u64) -> DisplayMetrics {
        DisplayMetrics::new(
            2.0,
            Size2D::new(800.0, 600.0).unwrap(),
            DisplayColorConfig::Srgb,
            DisplayGeneration::new(owner_id, id).unwrap(),
        )
        .unwrap()
    }

    fn make_camera(owner_id: ArenaOwnerId, generation: u64, x: f64, y: f64, scale: f64) -> Camera2D {
        Camera2D::new(
            CameraGeneration::new(owner_id, generation).unwrap(),
            test_display(owner_id, generation),
            Point2D::new(x, y).unwrap(),
            scale,
        )
        .unwrap()
    }

    #[test]
    fn flight_interpolates_smoothly_and_reaches_exact_endpoint() {
        let own = owner();
        let cam_a = make_camera(own, 1, 0.0, 0.0, 1.0);
        let cam_b = make_camera(own, 2, 200.0, 100.0, 4.0);

        let config = NavigationFlightConfig::new(
            200_000_000,
            100_000_000,
            MotionPreference::Normal,
        )
        .unwrap();
        let mut flight =
            NavigationFlight::new(cam_a, cam_b, NavigationReason::FitSelection, config).unwrap();

        assert_eq!(flight.progress(), 0.0);
        assert_eq!(flight.current_camera().unwrap().origin(), cam_a.origin());

        // Halfway step
        let mid_cam = flight.step(config.duration_nanos / 2).unwrap();
        assert!(!flight.is_complete());
        assert!(mid_cam.origin().x() > 0.0 && mid_cam.origin().x() < 200.0);
        assert!(mid_cam.origin().y() > 0.0 && mid_cam.origin().y() < 100.0);
        assert!(mid_cam.points_per_unit() > 1.0 && mid_cam.points_per_unit() < 4.0);

        // Complete step
        let final_cam = flight.step(config.duration_nanos / 2).unwrap();
        assert!(flight.is_complete());
        assert_eq!(final_cam, cam_b, "flight reaches exact endpoint bit-for-bit");
    }

    #[test]
    fn sleep_delta_clamping_prevents_wild_jumps() {
        let own = owner();
        let cam_a = make_camera(own, 1, 0.0, 0.0, 1.0);
        let cam_b = make_camera(own, 2, 1000.0, 1000.0, 8.0);

        let config = NavigationFlightConfig::new(
            300_000_000, // 300ms duration
            50_000_000,  // 50ms max step delta
            MotionPreference::Normal,
        )
        .unwrap();

        let mut flight =
            NavigationFlight::new(cam_a, cam_b, NavigationReason::FocusNode, config).unwrap();

        // Simulate a 10-second system sleep jump (10_000_000_000 ns)
        let jumped_cam = flight.step(10_000_000_000).unwrap();

        // Elapsed time must be clamped to 50ms, NOT 10s!
        assert_eq!(flight.elapsed_nanos(), 50_000_000);
        assert!(!flight.is_complete());
        assert!(jumped_cam.origin().x() < 200.0);
    }

    #[test]
    fn reduced_motion_transitions_directly_with_zero_flight_frames() {
        let own = owner();
        let cam_a = make_camera(own, 1, 0.0, 0.0, 1.0);
        let cam_b = make_camera(own, 2, 500.0, -300.0, 2.5);

        let config = NavigationFlightConfig::new(
            DEFAULT_FLIGHT_DURATION_NANOS,
            DEFAULT_MAX_STEP_DELTA_NANOS,
            MotionPreference::ReducedMotion,
        )
        .unwrap();

        let flight =
            NavigationFlight::new(cam_a, cam_b, NavigationReason::FitProject, config).unwrap();
        assert!(flight.is_complete());
        assert_eq!(flight.current_camera().unwrap(), cam_b);
    }

    #[test]
    fn interrupt_flight_hands_control_back_immediately() {
        let own = owner();
        let cam_a = make_camera(own, 1, 0.0, 0.0, 1.0);
        let cam_b = make_camera(own, 2, 800.0, 600.0, 5.0);

        let mut history = NavigationHistory::with_initial_camera(
            own,
            cam_a,
            NavigationFlightConfig::default(),
        )
        .unwrap();

        history
            .push_semantic_endpoint(cam_b, NavigationReason::FitSelection, 1_000_000)
            .unwrap();
        assert!(history.active_flight().is_some());

        // Advance 25% of flight
        let step_cam = history.step_flight(62_500_000).unwrap().unwrap();
        assert!(step_cam.origin().x() > 0.0);

        // User gesture interrupts the flight
        let interrupted_cam = history.interrupt_flight().unwrap().unwrap();
        assert_eq!(interrupted_cam, step_cam);
        assert!(history.active_flight().is_none());

        // History current camera is now the interrupted camera position
        assert_eq!(history.current_camera().unwrap(), interrupted_cam);

        // User can pan from this exact state without fighting the previous flight
        let pan_delta = Point2D::new(10.0, -5.0).unwrap();
        let panned = interrupted_cam.pan(pan_delta).unwrap();
        assert_eq!(
            panned.origin().x(),
            interrupted_cam.origin().x() - pan_delta.x() / interrupted_cam.points_per_unit()
        );
    }

    #[test]
    fn back_and_forward_navigation_with_history_truncation() {
        let own = owner();
        let cam1 = make_camera(own, 1, 10.0, 10.0, 1.0);
        let cam2 = make_camera(own, 2, 20.0, 20.0, 2.0);
        let cam3 = make_camera(own, 3, 30.0, 30.0, 3.0);
        let cam4 = make_camera(own, 4, 40.0, 40.0, 4.0);

        let config = NavigationFlightConfig::new(
            DEFAULT_FLIGHT_DURATION_NANOS,
            DEFAULT_MAX_STEP_DELTA_NANOS,
            MotionPreference::ReducedMotion, // Direct transitions for clean state assertion
        )
        .unwrap();

        let mut history = NavigationHistory::with_initial_camera(own, cam1, config).unwrap();
        assert!(!history.can_go_back());
        assert!(!history.can_go_forward());

        history
            .push_semantic_endpoint(cam2, NavigationReason::FitSelection, 100)
            .unwrap();
        history
            .push_semantic_endpoint(cam3, NavigationReason::FocusNode, 200)
            .unwrap();

        assert_eq!(history.current_camera().unwrap(), cam3);
        assert!(history.can_go_back());
        assert!(!history.can_go_forward());

        // Back to cam2
        let b1 = history.navigate_back(300).unwrap();
        assert_eq!(b1, cam2);
        assert_eq!(history.current_camera().unwrap(), cam2);
        assert!(history.can_go_back());
        assert!(history.can_go_forward());

        // Back to cam1
        let b2 = history.navigate_back(400).unwrap();
        assert_eq!(b2, cam1);
        assert_eq!(history.current_camera().unwrap(), cam1);
        assert!(!history.can_go_back());
        assert!(history.can_go_forward());

        // Forward to cam2
        let f1 = history.navigate_forward(500).unwrap();
        assert_eq!(f1, cam2);
        assert_eq!(history.current_camera().unwrap(), cam2);

        // Push new semantic endpoint cam4 while at cam2 -> truncates forward history (cam3 dropped)
        history
            .push_semantic_endpoint(cam4, NavigationReason::JumpToBookmark, 600)
            .unwrap();
        assert_eq!(history.current_camera().unwrap(), cam4);
        assert!(!history.can_go_forward(), "forward branch truncated");
        assert_eq!(history.past_count(), 2); // cam1, cam2

        // Back goes to cam2
        assert_eq!(history.navigate_back(700).unwrap(), cam2);
    }

    #[test]
    fn inverse_projection_holds_across_all_flight_interpolation_steps() {
        let own = owner();
        let cam_a = make_camera(own, 1, -100.0, 50.0, 0.5);
        let cam_b = make_camera(own, 2, 450.0, -220.0, 16.0);

        let config = NavigationFlightConfig::default();
        let flight =
            NavigationFlight::new(cam_a, cam_b, NavigationReason::FitProject, config).unwrap();

        let test_points = [
            Point2D::new(0.0, 0.0).unwrap(),
            Point2D::new(400.0, 300.0).unwrap(),
            Point2D::new(799.0, 599.0).unwrap(),
            Point2D::new(123.45, 456.78).unwrap(),
        ];

        // Test at 10 discrete interpolation steps
        for step_idx in 0..=10 {
            let dt = (config.duration_nanos / 10) * step_idx;
            let mut f_step = flight.clone();
            let cam = f_step.step(dt).unwrap();

            for &pt in &test_points {
                let local = cam.logical_to_local(pt).unwrap();
                let round_trip = cam.local_to_logical(local).unwrap();
                let dx = (round_trip.x() - pt.x()).abs();
                let dy = (round_trip.y() - pt.y()).abs();
                assert!(
                    dx < 1e-8 && dy < 1e-8,
                    "inverse projection oracle failed at step {step_idx}: dx={dx}, dy={dy}"
                );
            }
        }
    }

    #[test]
    fn deep_zoom_and_repeated_pinch_drift_verification() {
        let own = owner();
        let mut cam = make_camera(own, 1, 100.0, 100.0, 1.0);
        let anchor = Point2D::new(400.0, 300.0).unwrap();

        // Repeated zoom in by 2x for 20 iterations (scale factor 2^20 ~ 1,048,576)
        for _ in 0..20 {
            cam = cam.zoom_at(anchor, 2.0).unwrap();
        }
        assert!(cam.points_per_unit() >= 1_000_000.0);

        // Repeated zoom out by 0.5x for 20 iterations
        for _ in 0..20 {
            cam = cam.zoom_at(anchor, 0.5).unwrap();
        }

        // Must return to origin with sub-micro-point drift
        let diff_x = (cam.origin().x() - 100.0).abs();
        let diff_y = (cam.origin().y() - 100.0).abs();
        assert!(
            diff_x < 1e-6 && diff_y < 1e-6,
            "accumulated zoom drift exceeded bound: dx={diff_x}, dy={diff_y}"
        );
    }

    #[test]
    fn deterministic_keyboard_replay_reaches_exact_endpoints() {
        let own = owner();
        let initial = make_camera(own, 1, 0.0, 0.0, 1.0);
        let c1 = make_camera(own, 2, 50.0, 50.0, 1.5);
        let c2 = make_camera(own, 3, 150.0, 100.0, 3.0);

        let run_commands = || -> Camera2D {
            let mut h = NavigationHistory::with_initial_camera(
                own,
                initial,
                NavigationFlightConfig::new(
                    100_000_000,
                    20_000_000,
                    MotionPreference::ReducedMotion,
                )
                .unwrap(),
            )
            .unwrap();

            h.push_semantic_endpoint(c1, NavigationReason::KeyboardReplay, 10)
                .unwrap();
            h.push_semantic_endpoint(c2, NavigationReason::KeyboardReplay, 20)
                .unwrap();
            let _ = h.navigate_back(30).unwrap();
            let final_cam = h.navigate_forward(40).unwrap();
            final_cam
        };

        let result1 = run_commands();
        let result2 = run_commands();
        assert_eq!(
            result1, result2,
            "deterministic keyboard replay must yield identical state"
        );
        assert_eq!(result1, c2);
    }

    #[test]
    fn negative_controls_refuse_foreign_owner_and_empty_history() {
        let own = owner();
        let foreign = foreign_owner();

        let local_cam = make_camera(own, 1, 0.0, 0.0, 1.0);
        let foreign_cam = make_camera(foreign, 1, 0.0, 0.0, 1.0);

        // 1. Cross-owner flight creation is rejected
        let flight_err = NavigationFlight::new(
            local_cam,
            foreign_cam,
            NavigationReason::FitSelection,
            NavigationFlightConfig::default(),
        );
        assert_eq!(flight_err, Err(NavigationError::OwnerMismatch));

        // 2. History push with foreign owner is rejected
        let mut history = NavigationHistory::with_initial_camera(
            own,
            local_cam,
            NavigationFlightConfig::default(),
        )
        .unwrap();

        let push_err = history.push_semantic_endpoint(
            foreign_cam,
            NavigationReason::FocusNode,
            100,
        );
        assert_eq!(push_err, Err(NavigationError::OwnerMismatch));

        // 3. Navigation back/forward when history is empty is rejected
        assert_eq!(history.navigate_back(200), Err(NavigationError::EmptyHistory));
        assert_eq!(history.navigate_forward(300), Err(NavigationError::EmptyHistory));

        // 4. Zero duration is rejected
        assert_eq!(
            NavigationFlightConfig::new(0, 100, MotionPreference::Normal),
            Err(NavigationError::InvalidDuration)
        );
        assert_eq!(
            NavigationFlightConfig::new(100, 0, MotionPreference::Normal),
            Err(NavigationError::InvalidDuration)
        );
    }
}
