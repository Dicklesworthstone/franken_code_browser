//! Motion coalescing mailbox and bounded discrete input queue.
//!
//! Under §15.6:
//! - Motion is coalesced into a latest-state mailbox so high-frequency continuous
//!   events (mouse moves, scroll ticks, pinch deltas) never overflow event rings
//!   or cause interaction stalls.
//! - Ordered discrete commands (pointer down, pointer up, key down, key up, focus changes,
//!   marked-text commits) are preserved in order in a bounded queue.
//! - Per-frame processing is bounded, with explicit backpressure tracking.

#![forbid(unsafe_code)]

use std::collections::VecDeque;

use crate::ui::focus::FocusTarget;
use crate::ui::gesture::{HitRegion, ModifierKeys, PointerButton};

/// Discrete input events whose order must be strictly preserved.
#[derive(Clone, Debug, PartialEq)]
pub enum DiscreteInputEvent {
    /// Pointer press at specific coordinates.
    PointerDown {
        region: HitRegion,
        pos: (f32, f32),
        button: PointerButton,
        modifiers: ModifierKeys,
    },
    /// Pointer release at specific coordinates.
    PointerUp {
        pos: (f32, f32),
        button: PointerButton,
    },
    /// Discrete key press.
    KeyDown {
        key: String,
        modifiers: ModifierKeys,
    },
    /// Discrete key release.
    KeyUp {
        key: String,
    },
    /// User clicked or focused a specific UI target.
    FocusTargetSelected(FocusTarget),
    /// Native IME marked-text commit.
    MarkedTextCommit(String),
}

/// Coalesced continuous motion data consumed in a single frame.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CoalescedMotion {
    /// Accumulated camera/pointer delta `(dx, dy)` from move events.
    pub pan_delta: (f32, f32),
    /// Accumulated scroll delta `(dx, dy)` from wheel/trackpad events.
    pub scroll_delta: (f32, f32),
    /// Accumulated pinch magnification scale factor.
    pub pinch_magnification: f32,
    /// Latest reported pointer coordinate, if any move occurred.
    pub latest_pointer: Option<(f32, f32)>,
    /// Number of high-frequency ticks coalesced into this frame.
    pub coalesced_count: usize,
}

/// Mailbox that coalesces high-frequency continuous motion while queueing discrete inputs.
#[derive(Clone, Debug, PartialEq)]
pub struct MotionMailbox {
    discrete_queue: VecDeque<DiscreteInputEvent>,
    capacity: usize,
    accumulated_pan: (f32, f32),
    accumulated_scroll: (f32, f32),
    accumulated_pinch: f32,
    latest_pointer: Option<(f32, f32)>,
    coalesced_move_ticks: u64,
    dropped_discrete_events: u64,
}

impl MotionMailbox {
    pub const DEFAULT_QUEUE_CAPACITY: usize = 128;

    pub fn new(capacity: usize) -> Self {
        let cap = if capacity == 0 {
            Self::DEFAULT_QUEUE_CAPACITY
        } else {
            capacity
        };
        Self {
            discrete_queue: VecDeque::with_capacity(cap),
            capacity: cap,
            accumulated_pan: (0.0, 0.0),
            accumulated_scroll: (0.0, 0.0),
            accumulated_pinch: 0.0,
            latest_pointer: None,
            coalesced_move_ticks: 0,
            dropped_discrete_events: 0,
        }
    }

    /// Enqueue a discrete event. If the queue is full, the oldest event is dropped
    /// to provide backpressure, and the drop counter is incremented.
    pub fn push_discrete(&mut self, event: DiscreteInputEvent) -> bool {
        if self.discrete_queue.len() >= self.capacity {
            self.discrete_queue.pop_front();
            self.dropped_discrete_events = self.dropped_discrete_events.saturating_add(1);
            self.discrete_queue.push_back(event);
            false
        } else {
            self.discrete_queue.push_back(event);
            true
        }
    }

    /// Record a high-frequency continuous pointer move delta.
    pub fn record_move(&mut self, pos: (f32, f32), delta: (f32, f32)) {
        self.accumulated_pan.0 += delta.0;
        self.accumulated_pan.1 += delta.1;
        self.latest_pointer = Some(pos);
        self.coalesced_move_ticks = self.coalesced_move_ticks.saturating_add(1);
    }

    /// Record a high-frequency continuous scroll/wheel delta.
    pub fn record_scroll(&mut self, delta: (f32, f32)) {
        self.accumulated_scroll.0 += delta.0;
        self.accumulated_scroll.1 += delta.1;
        self.coalesced_move_ticks = self.coalesced_move_ticks.saturating_add(1);
    }

    /// Record a continuous pinch magnification delta.
    pub fn record_pinch(&mut self, magnification: f32) {
        self.accumulated_pinch += magnification;
        self.coalesced_move_ticks = self.coalesced_move_ticks.saturating_add(1);
    }

    /// Check if any motion or discrete events are pending.
    pub fn has_pending(&self) -> bool {
        !self.discrete_queue.is_empty()
            || self.accumulated_pan.0.abs() > f32::EPSILON
            || self.accumulated_pan.1.abs() > f32::EPSILON
            || self.accumulated_scroll.0.abs() > f32::EPSILON
            || self.accumulated_scroll.1.abs() > f32::EPSILON
            || self.accumulated_pinch.abs() > f32::EPSILON
    }

    /// Take and reset the coalesced continuous motion for this frame.
    pub fn take_motion(&mut self) -> Option<CoalescedMotion> {
        let has_motion = self.accumulated_pan.0.abs() > f32::EPSILON
            || self.accumulated_pan.1.abs() > f32::EPSILON
            || self.accumulated_scroll.0.abs() > f32::EPSILON
            || self.accumulated_scroll.1.abs() > f32::EPSILON
            || self.accumulated_pinch.abs() > f32::EPSILON
            || self.latest_pointer.is_some();

        if !has_motion {
            return None;
        }

        let motion = CoalescedMotion {
            pan_delta: self.accumulated_pan,
            scroll_delta: self.accumulated_scroll,
            pinch_magnification: self.accumulated_pinch,
            latest_pointer: self.latest_pointer,
            coalesced_count: self.coalesced_move_ticks as usize,
        };

        self.accumulated_pan = (0.0, 0.0);
        self.accumulated_scroll = (0.0, 0.0);
        self.accumulated_pinch = 0.0;
        self.latest_pointer = None;
        self.coalesced_move_ticks = 0;

        Some(motion)
    }

    /// Drain at most `max_events` discrete events from the queue for bounded per-frame consumption.
    pub fn drain_discrete_batch(&mut self, max_events: usize) -> Vec<DiscreteInputEvent> {
        let count = max_events.min(self.discrete_queue.len());
        let mut batch = Vec::with_capacity(count);
        for _ in 0..count {
            if let Some(ev) = self.discrete_queue.pop_front() {
                batch.push(ev);
            }
        }
        batch
    }

    pub fn discrete_len(&self) -> usize {
        self.discrete_queue.len()
    }

    pub fn dropped_discrete_count(&self) -> u64 {
        self.dropped_discrete_events
    }

    pub fn reset(&mut self) {
        self.discrete_queue.clear();
        self.accumulated_pan = (0.0, 0.0);
        self.accumulated_scroll = (0.0, 0.0);
        self.accumulated_pinch = 0.0;
        self.latest_pointer = None;
        self.coalesced_move_ticks = 0;
    }
}

impl Default for MotionMailbox {
    fn default() -> Self {
        Self::new(Self::DEFAULT_QUEUE_CAPACITY)
    }
}
