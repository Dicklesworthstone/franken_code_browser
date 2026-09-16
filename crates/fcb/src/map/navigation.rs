#![forbid(unsafe_code)]

//! Semantic atlas navigation with separately controlled selection and camera.
//!
//! Explicit focus commands create history entries. Pointer pan/zoom do not add
//! wheel-tick entries. History stores fixed-size values, never captured source,
//! layout copies or GPU resources; its capacity is admitted at construction.
//! Back/Forward restores spatial values under a FRESH camera generation so an
//! old asynchronous result cannot become current merely because we returned.

use std::{collections::VecDeque, mem::size_of};
use fcb_core::{ByteLength, CameraGeneration, DisplayMetrics, Point2D, ResourceAllocationId,
    ResourceBudget, ResourceLease};
use super::{AtlasError, AtlasHit, AtlasIndex, AtlasNodeId, Camera2D, CameraError, PresentedAtlas};

pub const MAX_NAVIGATION_HISTORY: usize = 1024;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct AtlasLocation {
    focus: AtlasNodeId,
    camera: Camera2D,
    selection: Option<AtlasNodeId>,
}
impl AtlasLocation {
    pub const fn focus(self) -> AtlasNodeId { self.focus }
    pub const fn camera(self) -> Camera2D { self.camera }
    pub const fn selection(self) -> Option<AtlasNodeId> { self.selection }
}

pub struct AtlasNavigation<'index, 'layout> {
    index: &'index AtlasIndex<'layout>,
    current: AtlasLocation,
    back: VecDeque<AtlasLocation>,
    forward: VecDeque<AtlasLocation>,
    capacity: usize,
    _lease: ResourceLease,
}
impl<'index, 'layout> AtlasNavigation<'index, 'layout> {
    pub fn new(index: &'index AtlasIndex<'layout>, camera: Camera2D, history_capacity: usize,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, AtlasError> {
        if !(1..=MAX_NAVIGATION_HISTORY).contains(&history_capacity) { return Err(AtlasError::InvalidLimits); }
        if camera.generation().owner() != index.owner() { return Err(AtlasError::OwnerMismatch); }
        let charge = history_capacity.checked_mul(2 * size_of::<AtlasLocation>())
            .and_then(|n| n.checked_add(size_of::<Self>())).ok_or(AtlasError::InvalidLimits)?;
        let lease = budget.try_reserve_managed(index.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AtlasError::ResourceDenied)?;
        let mut back = VecDeque::new();
        let mut forward = VecDeque::new();
        back.try_reserve_exact(history_capacity).map_err(|_| AtlasError::AllocationFailed)?;
        forward.try_reserve_exact(history_capacity).map_err(|_| AtlasError::AllocationFailed)?;
        if back.capacity() > history_capacity || forward.capacity() > history_capacity {
            return Err(AtlasError::ResourceDenied);
        }
        Ok(Self { index, current: AtlasLocation { focus: index.root_node(), camera, selection: None },
            back, forward, capacity: history_capacity, _lease: lease })
    }
    pub const fn location(&self) -> AtlasLocation { self.current }
    pub fn back_len(&self) -> usize { self.back.len() }
    pub fn forward_len(&self) -> usize { self.forward.len() }
    pub fn can_go_back(&self) -> bool { !self.back.is_empty() }
    pub fn can_go_forward(&self) -> bool { !self.forward.is_empty() }

    /// Selection is not spatial focus: selecting does not pan, repack, add a
    /// history entry or load source. It may name a file outside the current view.
    pub fn select(&mut self, selection: Option<AtlasNodeId>) -> Result<(), AtlasError> {
        if let Some(node) = selection { self.index.node(node)?; }
        self.current.selection = selection;
        Ok(())
    }
    pub fn select_hit(&mut self, presented: PresentedAtlas<'_>, hit: AtlasHit) -> Result<(), AtlasError> {
        hit.validate(presented)?;
        if presented.plan().camera().display() != self.current.camera.display() {
            return Err(AtlasError::DisplayMismatch);
        }
        self.select(Some(hit.node()))
    }

    /// Direct, reduced-motion-compatible transition to a focus-local island.
    /// An application may animate between these explicit endpoint values, but
    /// no animation timer or autonomous simulation is started by the library.
    pub fn focus(&mut self, target: AtlasNodeId, padding_points: f64) -> Result<(), AtlasError> {
        let camera = self.index.focus_camera(target, self.next_generation()?,
            self.current.camera.display(), padding_points)?;
        let next = AtlasLocation { focus: target, camera, selection: self.current.selection };
        self.commit_navigation(next);
        Ok(())
    }
    pub fn fit_selection(&mut self, padding_points: f64) -> Result<bool, AtlasError> {
        let Some(selected) = self.current.selection else { return Ok(false); };
        self.focus(selected, padding_points)?;
        Ok(true)
    }
    pub fn fit_parent(&mut self, padding_points: f64) -> Result<bool, AtlasError> {
        let Some(parent) = self.index.parent(self.current.focus)? else { return Ok(false); };
        self.focus(parent, padding_points)?;
        Ok(true)
    }
    pub fn fit_project(&mut self, padding_points: f64) -> Result<(), AtlasError> {
        self.focus(self.index.root_node(), padding_points)
    }
    pub fn pan(&mut self, delta_points: Point2D) -> Result<(), AtlasError> {
        self.current.camera = self.current.camera.pan(delta_points)?;
        Ok(())
    }
    pub fn zoom_at(&mut self, point: Point2D, factor: f64) -> Result<(), AtlasError> {
        self.current.camera = self.current.camera.zoom_at(point, factor)?;
        Ok(())
    }
    pub fn set_display(&mut self, display: DisplayMetrics) -> Result<(), AtlasError> {
        self.current.camera = self.current.camera.with_display(display)?;
        Ok(())
    }

    pub fn go_back(&mut self) -> Result<bool, AtlasError> {
        let Some(saved) = self.back.back().copied() else { return Ok(false); };
        let next = self.restore(saved)?; // All fallible work before changing stacks.
        self.back.pop_back();
        push_bounded(&mut self.forward, self.capacity, self.current);
        self.current = next;
        Ok(true)
    }
    pub fn go_forward(&mut self) -> Result<bool, AtlasError> {
        let Some(saved) = self.forward.back().copied() else { return Ok(false); };
        let next = self.restore(saved)?;
        self.forward.pop_back();
        push_bounded(&mut self.back, self.capacity, self.current);
        self.current = next;
        Ok(true)
    }
    fn commit_navigation(&mut self, next: AtlasLocation) {
        push_bounded(&mut self.back, self.capacity, self.current);
        // At most MAX_NAVIGATION_HISTORY fixed-size values, no nested releases.
        self.forward.clear();
        self.current = next;
    }
    fn next_generation(&self) -> Result<CameraGeneration, AtlasError> {
        let generation = self.current.camera.generation();
        let value = generation.get().checked_add(1).ok_or(AtlasError::Camera(CameraError::GenerationExhausted))?;
        CameraGeneration::new(generation.owner(), value).map_err(|_| AtlasError::Camera(CameraError::GenerationExhausted))
    }
    fn restore(&self, saved: AtlasLocation) -> Result<AtlasLocation, AtlasError> {
        self.index.node(saved.focus)?;
        let camera = saved.camera;
        let display = self.current.camera.display();
        // Restore the saved local center using the CURRENT display's dimensions.
        let old_size = camera.display().logical_size();
        let new_size = display.logical_size();
        let origin = Point2D::new(camera.origin().x() + (old_size.width() - new_size.width()) * 0.5 / camera.points_per_unit(),
            camera.origin().y() + (old_size.height() - new_size.height()) * 0.5 / camera.points_per_unit())
            .map_err(|_| AtlasError::Camera(CameraError::InvalidGeometry))?;
        let camera = Camera2D::new(self.next_generation()?, display, origin, camera.points_per_unit())?;
        Ok(AtlasLocation { camera, ..saved })
    }
}
fn push_bounded(stack: &mut VecDeque<AtlasLocation>, capacity: usize, item: AtlasLocation) {
    if stack.len() == capacity { stack.pop_front(); }
    stack.push_back(item);
}
