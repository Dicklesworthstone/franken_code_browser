#![forbid(unsafe_code)]

//! Metadata-only, retained City worker service. The host supplies the exact
//! current atlas footprints, never paths or source grants. The render engine
//! owns metric heights, projection and hierarchical picking. Camera candidates
//! are separate from the frame the host declares it actually displayed.

use std::mem::size_of;
use fcb_core::{ArenaOwnerId, ByteLength, LayoutRevision, PresentedFrameId,
    Rect2D, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_render::city::{Camera, CityError, Frame, Record, Scene,
    MAX_CITY_RECORDS, METRIC_NAME, METRIC_CAP_BYTES};
use crate::{EXIT_OK, EXIT_PARTIAL, output::{Output, OutputError}};
use super::HostResponse;

pub const MAX_CITY_FILES: usize = MAX_CITY_RECORDS;
pub const MAX_CITY_PENDING_VIEWS: usize = 8;
const MANAGED_BYTES: u64 = 64 * 1024 * 1024;
const SCENE_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
const VIEW_RESPONSE_BYTES: usize = 8192;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CitySessionError {
    Geometry(CityError), Output(OutputError), InvalidInput, Admission,
    IdentityExhausted, StaleGeneration, MissingView, StaleFrame, Canceled,
}
impl std::fmt::Display for CitySessionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Geometry(error) => write!(f, "{error}"),
            Self::Output(error) => write!(f, "{error}"),
            other => f.write_str(match other {
                Self::InvalidInput => "CITY_SESSION_INVALID_INPUT",
                Self::Admission => "CITY_SESSION_ADMISSION",
                Self::IdentityExhausted => "CITY_SESSION_IDENTITY_EXHAUSTED",
                Self::StaleGeneration => "CITY_SESSION_STALE_GENERATION",
                Self::MissingView => "CITY_SESSION_MISSING_VIEW",
                Self::StaleFrame => "CITY_SESSION_STALE_FRAME",
                Self::Canceled => "CITY_SESSION_CANCELED",
                _ => unreachable!(),
            }),
        }
    }
}
impl std::error::Error for CitySessionError {}
impl From<CityError> for CitySessionError { fn from(e: CityError) -> Self { Self::Geometry(e) } }
impl From<OutputError> for CitySessionError { fn from(e: OutputError) -> Self { Self::Output(e) } }

/// Borrowed parallel numeric arrays; no path, decoder, parser or source access.
/// An unknown observation has known[i] == 0; zero bytes with known[i] == 1 is a
/// real empty-file observation. Rectangles are the EXISTING world footprints.
pub struct CityInput<'a> {
    pub ids: &'a [u64],
    pub rects: &'a [f64],
    pub observed_bytes: &'a [u64],
    pub known: &'a [u8],
}

#[derive(Clone, Copy, Debug)]
pub enum CityViewAction {
    View { width: f64, height: f64, center_x: f64, center_y: f64,
        scale: f64, yaw: f64, tilt: f64 },
    Fit { width: f64, height: f64 },
    Pan { base: u64, dx: f64, dy: f64 },
    Zoom { base: u64, x: f64, y: f64, factor: f64 },
    Orbit { base: u64, yaw: f64, tilt: f64 },
    Resize { base: u64, width: f64, height: f64 },
}

#[derive(Clone)]
struct Candidate { generation: u64, frame: Frame }
#[derive(Clone)]
struct Presented { candidate: Candidate, frame: u64, display: u64 }

pub struct CitySession {
    owner: ArenaOwnerId,
    layout: LayoutRevision,
    scene: Scene,
    discovery_complete: bool,
    pending: [Option<Candidate>; MAX_CITY_PENDING_VIEWS],
    next_pending: usize,
    presented: Option<Presented>,
    last_attempt: u64,
    next_allocation: u64,
    budget: ResourceBudget,
    _lease: ResourceLease,
}

impl CitySession {
    pub fn open(owner: ArenaOwnerId, layout: u64, input: CityInput<'_>,
        discovery_complete: bool, mut canceled: impl FnMut() -> bool) -> Result<Self, CitySessionError> {
        check(&mut canceled)?;
        let count = input.ids.len();
        if count > MAX_CITY_FILES || input.rects.len() != count * 4
            || input.observed_bytes.len() != count || input.known.len() != count {
            return Err(CitySessionError::InvalidInput);
        }
        let layout = LayoutRevision::new(owner, layout).map_err(|_| CitySessionError::InvalidInput)?;
        let budget = ResourceBudget::new(owner, ByteLength::new(MANAGED_BYTES))
            .map_err(|_| CitySessionError::Admission)?;
        let lease = budget.try_reserve_managed(owner, allocation(1)?,
            ByteLength::new((size_of::<Self>() + 4096) as u64)).map_err(|_| CitySessionError::Admission)?;
        let input_charge = count.checked_mul(size_of::<Record>()).and_then(|n| n.checked_add(4096))
            .ok_or(CitySessionError::Admission)?;
        let _input_lease = budget.try_reserve_managed(owner, allocation(2)?, ByteLength::new(input_charge as u64))
            .map_err(|_| CitySessionError::Admission)?;
        let mut records = Vec::new();
        records.try_reserve_exact(count).map_err(|_| CitySessionError::Admission)?;
        if records.capacity() > count { return Err(CitySessionError::Admission); }
        for i in 0..count {
            check(&mut canceled)?;
            if input.known[i] > 1 { return Err(CitySessionError::InvalidInput); }
            let r = &input.rects[i * 4..i * 4 + 4];
            records.push(Record { file_id: input.ids[i],
                footprint: Rect2D::from_xywh(r[0], r[1], r[2], r[3]).map_err(|_| CitySessionError::InvalidInput)?,
                observed_bytes: (input.known[i] == 1).then_some(input.observed_bytes[i]) });
        }
        let scene = Scene::new(layout, &records, &budget, allocation(3)?, &mut canceled)?;
        check(&mut canceled)?;
        Ok(Self { owner, layout, scene, discovery_complete, pending: std::array::from_fn(|_| None),
            next_pending: 0, presented: None, last_attempt: 0, next_allocation: 4, budget, _lease: lease })
    }

    /// Replayable immutable scene metadata, also the recovery path after an
    /// open response was lost. No camera candidate or selection is changed.
    pub fn info(&mut self, mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CitySessionError> {
        check(&mut canceled)?;
        let mut out = self.output("open", SCENE_RESPONSE_BYTES)?;
        out.literal(",\"metric\":")?; out.quoted(METRIC_NAME)?;
        out.literal(",\"metric_cap_bytes\":")?; out.integer(METRIC_CAP_BYTES)?;
        out.literal(",\"maximum_height\":")?; number(&mut out, self.scene.max_height())?;
        out.literal(",\"origin\":")?; numbers(&mut out, &self.scene.origin())?;
        let stats = self.scene.stats();
        out.literal(",\"file_count\":")?; out.integer(stats.records as u64)?;
        out.literal(",\"unknown_count\":")?; out.integer(stats.unknown_files as u64)?;
        out.literal(",\"capped_count\":")?; out.integer(stats.capped_files as u64)?;
        out.literal(",\"discovery_complete\":")?; out.boolean(self.discovery_complete)?;
        out.literal(",\"instances\":[")?;
        for (index, instance) in self.scene.instances().iter().enumerate() {
            check(&mut canceled)?;
            if index > 0 { out.literal(",")?; }
            out.literal("{\"file_id\":")?; out.integer(instance.file_id())?;
            out.literal(",\"rect\":")?; numbers(&mut out, &instance.rect().map(f64::from))?;
            out.literal(",\"height\":")?; number(&mut out, f64::from(instance.height()))?;
            out.literal(",\"observed_bytes\":")?;
            match instance.observed_bytes() { Some(n) => out.integer(n)?, None => out.literal("null")? }
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish(out, &mut canceled)
    }

    /// Generation is consumed before evaluating the action. Failed and canceled
    /// attempts cannot be replayed later as a different camera under that ID.
    pub fn prepare(&mut self, generation: u64, action: CityViewAction,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CitySessionError> {
        if generation == 0 || generation <= self.last_attempt { return Err(CitySessionError::StaleGeneration); }
        self.last_attempt = generation;
        check(&mut canceled)?;
        let camera = match action {
            CityViewAction::View { width, height, center_x, center_y, scale, yaw, tilt } =>
                Camera::new(width, height, center_x, center_y, scale, yaw, tilt)?,
            CityViewAction::Fit { width, height } => self.scene.fit_camera(width, height, 24.0)?,
            CityViewAction::Pan { base, dx, dy } => self.candidate(base)?.frame.camera().pan(dx, dy)?,
            CityViewAction::Zoom { base, x, y, factor } => self.candidate(base)?.frame.camera().zoom_at(x, y, factor)?,
            CityViewAction::Orbit { base, yaw, tilt } => self.candidate(base)?.frame.camera().orbit(yaw, tilt)?,
            CityViewAction::Resize { base, width, height } => self.candidate(base)?.frame.camera().with_viewport(width, height)?,
        };
        let id = PresentedFrameId::new(self.owner, generation).map_err(|_| CitySessionError::IdentityExhausted)?;
        let candidate = Candidate { generation, frame: self.scene.frame(id, camera)? };
        let mut out = self.output("view", VIEW_RESPONSE_BYTES)?;
        out.literal(",\"generation\":")?; out.integer(generation)?;
        out.literal(",\"projection\":\"orthographic\",\"width\":")?; number(&mut out, camera.width())?;
        out.literal(",\"height\":")?; number(&mut out, camera.height())?;
        out.literal(",\"matrix\":")?; numbers(&mut out, &candidate.frame.matrix().map(f64::from))?;
        out.literal(",\"camera\":{\"center_x\":")?; number(&mut out, camera.center()[0])?;
        out.literal(",\"center_y\":")?; number(&mut out, camera.center()[1])?;
        out.literal(",\"scale\":")?; number(&mut out, camera.scale())?;
        out.literal(",\"yaw\":")?; number(&mut out, camera.yaw())?;
        out.literal(",\"tilt\":")?; number(&mut out, camera.tilt())?;
        out.literal("}}\n")?;
        let response = self.finish(out, &mut canceled)?;
        self.pending[self.next_pending] = Some(candidate);
        self.next_pending = (self.next_pending + 1) % MAX_CITY_PENDING_VIEWS;
        Ok(response)
    }

    /// The host calls this only after its actual native presentation callback.
    /// Eight retained candidates allow bounded GPU overlap. A newer candidate
    /// never changes picking; only an acknowledged presentation does. Replaying
    /// the exact acknowledgment is idempotent after a lost response.
    pub fn present(&mut self, generation: u64, frame: u64, display: u64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CitySessionError> {
        check(&mut canceled)?;
        if frame == 0 || display == 0 { return Err(CitySessionError::InvalidInput); }
        let candidate = self.candidate(generation)?;
        if let Some(previous) = self.presented.as_ref() {
            let same = previous.frame == frame && previous.display == display
                && previous.candidate.generation == generation;
            if !same && (frame <= previous.frame || display < previous.display
                || generation < previous.candidate.generation) { return Err(CitySessionError::StaleFrame); }
        }
        let mut out = self.output("present", VIEW_RESPONSE_BYTES)?;
        stamp(&mut out, generation, frame, display)?;
        out.literal(",\"acknowledgment\":\"host-declared\"}\n")?;
        let response = self.finish(out, &mut canceled)?;
        self.presented = Some(Presented { candidate, frame, display });
        Ok(response)
    }

    /// Exact cuboid identity, NOT a source position or file-open grant. The host
    /// maps the ID through the same retained layout and uses its existing reader
    /// action for a NEW source observation.
    pub fn pick(&mut self, frame: u64, display: u64, x: f64, y: f64,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, CitySessionError> {
        check(&mut canceled)?;
        let accepted = self.presented.as_ref().ok_or(CitySessionError::MissingView)?.clone();
        if accepted.frame != frame || accepted.display != display { return Err(CitySessionError::StaleFrame); }
        let pick = self.scene.pick(&accepted.candidate.frame, x, y)?;
        let mut out = self.output("pick", VIEW_RESPONSE_BYTES)?;
        stamp(&mut out, accepted.candidate.generation, frame, display)?;
        out.literal(",\"picking\":\"frontmost-cuboid-not-source-location\",\"file_id\":")?;
        match pick { Some(hit) => out.integer(hit.file_id())?, None => out.literal("null")? }
        out.literal("}\n")?;
        self.finish(out, &mut canceled)
    }

    /// Cancellation revokes unpublished candidates, while the already visible
    /// frame remains usable. Generation counters never move backward.
    pub fn cancel_pending(&mut self) { self.pending.fill(None); }

    fn candidate(&self, generation: u64) -> Result<Candidate, CitySessionError> {
        self.pending.iter().flatten().find(|c| c.generation == generation).cloned()
            .or_else(|| self.presented.as_ref().filter(|p| p.candidate.generation == generation).map(|p| p.candidate.clone()))
            .ok_or(CitySessionError::MissingView)
    }
    fn next_id(&mut self) -> Result<ResourceAllocationId, CitySessionError> {
        let value = self.next_allocation;
        self.next_allocation = value.checked_add(1).ok_or(CitySessionError::IdentityExhausted)?;
        allocation(value)
    }
    fn output(&mut self, command: &str, limit: usize) -> Result<Output, CitySessionError> {
        let id = self.next_id()?;
        let mut out = Output::new(self.owner, limit, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.city/1\",\"status\":\"ok\",\"command\":")?; out.quoted(command)?;
        out.literal(",\"owner\":")?; out.integer(self.owner.get())?;
        out.literal(",\"layout\":")?; out.integer(self.layout.get())?;
        out.literal(",\"source_payload_read\":false,\"native_presented\":false")?;
        Ok(out)
    }
    fn finish(&mut self, out: Output, canceled: &mut impl FnMut() -> bool) -> Result<HostResponse, CitySessionError> {
        check(canceled)?;
        let length = out.as_bytes().len();
        // String plus C handoff overlap. No unmanaged full-scene JSON copy is
        // created before this response reservation succeeds.
        let charge = length.checked_mul(3).and_then(|n| n.checked_add(4096)).ok_or(CitySessionError::Admission)?;
        let id = self.next_id()?;
        let lease = self.budget.try_reserve_managed(self.owner, id, ByteLength::new(charge as u64))
            .map_err(|_| CitySessionError::Admission)?;
        let mut text = String::new();
        text.try_reserve_exact(length).map_err(|_| CitySessionError::Admission)?;
        if text.capacity() > length { return Err(CitySessionError::Admission); }
        text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| CitySessionError::InvalidInput)?);
        check(canceled)?;
        Ok(HostResponse { text, exit_code: if self.discovery_complete { EXIT_OK } else { EXIT_PARTIAL }, _lease: lease })
    }
}

fn allocation(n: u64) -> Result<ResourceAllocationId, CitySessionError> {
    ResourceAllocationId::new(n).map_err(|_| CitySessionError::IdentityExhausted)
}
fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), CitySessionError> {
    if canceled() { Err(CitySessionError::Canceled) } else { Ok(()) }
}
fn number(out: &mut Output, value: f64) -> Result<(), CitySessionError> {
    if !value.is_finite() { return Err(CitySessionError::InvalidInput); }
    out.literal(&value.to_string())?; Ok(())
}
fn numbers(out: &mut Output, values: &[f64]) -> Result<(), CitySessionError> {
    out.literal("[")?;
    for (index, value) in values.iter().enumerate() {
        if index > 0 { out.literal(",")?; } number(out, *value)?;
    }
    out.literal("]")?; Ok(())
}
fn stamp(out: &mut Output, generation: u64, frame: u64, display: u64) -> Result<(), CitySessionError> {
    out.literal(",\"generation\":")?; out.integer(generation)?;
    out.literal(",\"frame\":")?; out.integer(frame)?;
    out.literal(",\"display\":")?; out.integer(display)?;
    Ok(())
}

#[cfg(test)]
#[path = "city_tests.rs"]
mod tests;
