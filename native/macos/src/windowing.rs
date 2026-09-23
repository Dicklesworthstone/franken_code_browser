//! Native AppKit window creation, typed input events, and bounded pumping.
//!
//! This module composes the audited Apple ABI in [`crate::ffi`] into the
//! narrow surface FCB-004.A owns: window/view creation, input coordinate
//! conversion, resize/backing generation tracking, and keyboard/scroll event
//! phases delivered through a bounded queue. Construction is inert until an
//! explicit main-thread call; nothing here installs a global handler, reads
//! the environment, or spawns threads.
//!
//! Ownership follows the crate policy: every native object is a typed owned
//! handle with retain/release accounting; late events arriving after a window
//! is dropped are ignored by the pump rather than resurrecting the owner.

use std::cell::Cell;
use std::collections::VecDeque;

use crate::{BridgeError, MainThreadToken, ffi};

/// The bounds of a rectangle in the AppKit coordinate system.
///
/// Fields are logical points; use the window's backing scale for pixel math.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    /// X of the lower-left corner in points.
    pub x: f64,
    /// Y of the lower-left corner in points.
    pub y: f64,
    /// Width in points.
    pub width: f64,
    /// Height in points.
    pub height: f64,
}

impl Rect {
    /// A rectangle anchored at the origin with the requested size.
    pub const fn at_origin(width: f64, height: f64) -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            width,
            height,
        }
    }
}

/// Which standard window chrome accompanies creation.
///
/// The mapping to `NSWindowStyleMask` bits is fixed by the ABI module.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowStyle(pub u8);

impl WindowStyle {
    /// Titled window with a title bar.
    pub const TITLED: u8 = 1 << 0;
    /// Close button enabled.
    pub const CLOSABLE: u8 = 1 << 1;
    /// Miniaturize button enabled.
    pub const MINIATURIZABLE: u8 = 1 << 2;
    /// Resize enabled on all edges.
    pub const RESIZABLE: u8 = 1 << 3;

    /// The reviewed default: titled, closable, resizable.
    pub const fn standard() -> Self {
        Self(Self::TITLED | Self::CLOSABLE | Self::RESIZABLE)
    }

    fn to_native(self) -> u64 {
        // NSWindowStyleMask values are the low four bits used here; the
        // full-width bit and borderless (0) are deliberately unreachable so a
        // window always carries explicit chrome configuration.
        u64::from(self.0 & 0b1111)
    }
}

/// Monotonic generation of the observed backing-scale factor.
///
/// The generation starts at one and advances by one every time an observation
/// sees a backing scale different from the previously recorded one. Callers
/// use it to invalidate pixel-size caches after a resize crosses a display
/// boundary.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq, PartialOrd, Ord)]
pub struct BackingGeneration(u64);

impl BackingGeneration {
    /// The generation of a freshly created window.
    pub const INITIAL: Self = Self(1);

    /// The raw generation value.
    pub const fn get(self) -> u64 {
        self.0
    }

    fn next(self) -> Self {
        Self(self.0.wrapping_add(1).max(1))
    }
}

/// A typed keyboard event.
#[derive(Clone, Debug, PartialEq)]
pub struct KeyEvent {
    /// Hardware keycode from the event.
    pub keycode: u16,
    /// Characters attributed to the event, when the platform provides them.
    pub characters: Option<String>,
    /// Modifier flags active at event time.
    pub modifiers: KeyModifiers,
    /// Whether this is a press or a release.
    pub phase: KeyPhase,
    /// Whether the platform marked the event as an auto-repeat.
    pub repeat: bool,
}

/// Whether a keyboard event is a key-down, a key-up, or a modifier change.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum KeyPhase {
    /// Key pressed.
    Down,
    /// Key released.
    Up,
    /// Modifier flags changed with no attributable keycode.
    Flags,
}

/// Modifier flags decoded from the platform's mask bits.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct KeyModifiers {
    pub caps_lock: bool,
    pub shift: bool,
    pub control: bool,
    pub option: bool,
    pub command: bool,
}

/// A typed scroll event.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ScrollEvent {
    pub delta_x: f64,
    pub delta_y: f64,
    /// Whether the device supplies precise (trackpad) deltas.
    pub precise: bool,
    pub phase: ScrollPhase,
}

/// Gesture phase of a scroll event.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ScrollPhase {
    /// Plain wheel-style event with no gesture phase.
    None,
    /// Gesture began.
    Began,
    /// Gesture changed.
    Changed,
    /// Gesture ended (final momentum tick).
    Ended,
}

/// A typed input event delivered through the bounded queue.
#[derive(Clone, Debug, PartialEq)]
pub enum InputEvent {
    Key(KeyEvent),
    Scroll(ScrollEvent),
    /// A typed mouse button press/release with click count.
    Button(ButtonEvent),
    /// Window focus entered (true) or left (false).
    FocusChanged {
        /// Whether the window gained focus.
        focused: bool,
    },
    /// IME marked text (the pre-commit composition string).
    MarkedText {
        /// The marked (composed, not yet committed) text.
        text: String,
    },
    /// An event type this seam deliberately does not interpret.
    Uninterpreted {
        /// Raw platform event type number, retained for diagnostics only.
        raw_type: u64,
    },
}

/// Decode the platform modifier mask into typed flags.
pub fn decode_modifier_flags(mask: u64) -> KeyModifiers {
    KeyModifiers {
        caps_lock: mask & (1 << 16) != 0,
        shift: mask & (1 << 17) != 0,
        control: mask & (1 << 18) != 0,
        option: mask & (1 << 19) != 0,
        command: mask & (1 << 20) != 0,
    }
}

/// Map the platform momentum-phase constant onto the typed scroll phase.
pub fn decode_scroll_phase(momentum_phase: u64) -> ScrollPhase {
    match momentum_phase {
        1 => ScrollPhase::Began,
        2 => ScrollPhase::Changed,
        _ => ScrollPhase::None,
    }
}

/// Convert logical points to backing pixels.
pub fn logical_to_pixels(value: f64, backing_scale: f64) -> f64 {
    value * backing_scale
}

/// Convert backing pixels to logical points.
pub fn pixels_to_logical(value: f64, backing_scale: f64) -> f64 {
    if backing_scale == 0.0 {
        return value;
    }
    value / backing_scale
}

/// Errors from the windowing seam.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum WindowingError {
    /// The window was closed and no longer accepts operations.
    Closed,
    /// The platform refused or could not create the requested object.
    NativeUnavailable,
    /// The title or other text could not be represented for the ABI.
    InvalidText,
    /// The event queue is at capacity; the newest event was dropped.
    QueueFull { capacity: usize, dropped_total: u64 },
}

impl From<BridgeError> for WindowingError {
    fn from(error: BridgeError) -> Self {
        match error {
            BridgeError::UnsupportedPlatform => Self::NativeUnavailable,
            _ => Self::NativeUnavailable,
        }
    }
}

/// Bounded queue of typed input events with explicit drop accounting.
///
/// Pushes never reallocate beyond the fixed capacity; when full, the newest
/// event is refused (and counted) rather than silently overwriting history.
/// Sequence numbers name accepted events only.
#[derive(Debug)]
pub struct BoundedEventQueue {
    capacity: usize,
    events: VecDeque<InputEvent>,
    dropped: u64,
    accepted: u64,
}

impl BoundedEventQueue {
    /// Create a queue holding at most `capacity` events.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity: capacity.max(1),
            events: VecDeque::with_capacity(capacity.max(1)),
            dropped: 0,
            accepted: 0,
        }
    }

    /// The fixed capacity.
    pub const fn capacity(&self) -> usize {
        self.capacity
    }

    /// Events currently retained.
    pub fn len(&self) -> usize {
        self.events.len()
    }

    /// Whether no events are retained.
    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// Total events refused for capacity, over the queue's lifetime.
    pub const fn dropped(&self) -> u64 {
        self.dropped
    }

    /// Remove and return the oldest event.
    pub fn pop(&mut self) -> Option<InputEvent> {
        self.events.pop_front()
    }

    /// Offer an event; refused with the drop counter advanced when full.
    pub fn offer(&mut self, event: InputEvent) -> Result<(), WindowingError> {
        if self.events.len() >= self.capacity {
            self.dropped += 1;
            return Err(WindowingError::QueueFull {
                capacity: self.capacity,
                dropped_total: self.dropped,
            });
        }
        self.events.push_back(event);
        self.accepted = self.accepted.wrapping_add(1);
        Ok(())
    }
}

/// A typed owned AppKit window.
///
/// Created on the main thread only; all operations re-verify the token. After
/// [`NativeWindow::mark_closed`], event delivery for this window is ignored
/// and native operations return [`WindowingError::Closed`].
#[derive(Debug)]
pub struct NativeWindow {
    object: ffi::OwnedObject,
    recorded_scale: Cell<f64>,
    generation: Cell<BackingGeneration>,
    closed: Cell<bool>,
}

impl NativeWindow {
    /// Create a real window with the requested logical content rectangle.
    ///
    /// The window is titled with `title`, configured with `style`, and ordered
    /// front under the accessory activation policy so tests and background
    /// hosts never steal focus from the user's work.
    pub fn create(
        token: MainThreadToken,
        title: &str,
        content: Rect,
        style: WindowStyle,
    ) -> Result<Self, WindowingError> {
        token.assert_current()?;
        let object = ffi::OwnedObject::from_ref(
            ffi::create_window(
                content.x,
                content.y,
                content.width,
                content.height,
                style.to_native(),
                title,
            )
            .ok_or(WindowingError::NativeUnavailable)?,
        );
        ffi::set_activation_policy_accessory();
        ffi::make_key_and_order_front(object.as_ref());
        let scale = ffi::window_backing_scale(object.as_ref());
        Ok(Self {
            object,
            recorded_scale: Cell::new(scale),
            generation: Cell::new(BackingGeneration::INITIAL),
            closed: Cell::new(false),
        })
    }

    /// Replaces the window content with a scrollable read-only monospaced
    /// text view showing `text`, pins the window to every Space, and
    /// activates the host application. Host/demo affordance for surfacing
    /// real source before the Metal glyph pipeline lands; not the product
    /// text route.
    pub fn install_source_text(&self, token: MainThreadToken, text: &str) -> bool {
        token.assert_current().is_ok() && ffi::install_source_text(self.object.as_ref(), text)
    }

    /// Centers the window on its screen (NSWindow center).
    pub fn center(&self, token: MainThreadToken) {
        if token.assert_current().is_ok() {
            ffi::center_window(self.object.as_ref());
        }
    }

    /// The backing scale observed at creation or the latest observation.
    pub fn recorded_backing_scale(&self) -> f64 {
        self.recorded_scale.get()
    }

    /// The current backing generation.
    pub fn backing_generation(&self) -> BackingGeneration {
        self.generation.get()
    }

    /// Whether the window has been marked closed.
    pub fn is_closed(&self) -> bool {
        self.closed.get()
    }

    pub(crate) fn raw_ref(&self) -> ffi::ObjectRef {
        self.object.as_ref()
    }

    /// Re-observe the backing scale, advancing the generation on change.
    pub fn observe_backing(
        &self,
        token: MainThreadToken,
    ) -> Result<(f64, BackingGeneration), WindowingError> {
        token.assert_current()?;
        if self.closed.get() {
            return Err(WindowingError::Closed);
        }
        let scale = ffi::window_backing_scale(self.object.as_ref());
        if scale != self.recorded_scale.get() {
            self.recorded_scale.set(scale);
            self.generation.set(self.generation.get().next());
        }
        Ok((scale, self.generation.get()))
    }

    /// Change the content size in logical points.
    pub fn set_content_size(
        &self,
        token: MainThreadToken,
        width: f64,
        height: f64,
    ) -> Result<(), WindowingError> {
        token.assert_current()?;
        if self.closed.get() {
            return Err(WindowingError::Closed);
        }
        ffi::set_content_size(self.object.as_ref(), width, height);
        Ok(())
    }

    /// Convert a logical point in window coordinates to backing pixels.
    pub fn point_to_pixels(
        &self,
        token: MainThreadToken,
        x: f64,
        y: f64,
    ) -> Result<(f64, f64), WindowingError> {
        token.assert_current()?;
        if self.closed.get() {
            return Err(WindowingError::Closed);
        }
        let scale = self.recorded_scale.get();
        Ok((logical_to_pixels(x, scale), logical_to_pixels(y, scale)))
    }

    /// Mark the window closed; native teardown happens on drop.
    ///
    /// Idempotent. Pumps and deliveries treat a closed window as a no-op.
    pub fn mark_closed(&self, token: MainThreadToken) -> Result<(), WindowingError> {
        token.assert_current()?;
        self.closed.set(true);
        Ok(())
    }

    /// Release the native window; late callbacks observe the closed flag.
    pub fn close(self, token: MainThreadToken) -> Result<(), WindowingError> {
        token.assert_current()?;
        self.closed.set(true);
        Ok(())
    }
}

/// A typed mouse button event with ordered click counting.
#[derive(Clone, Debug, PartialEq)]
pub struct ButtonEvent {
    /// Platform button number (0 = primary).
    pub button: u8,
    /// Press or release.
    pub phase: KeyPhase,
    /// Click count for multi-click detection.
    pub click_count: u64,
    /// Logical-window coordinates at event time.
    pub x: f64,
    pub y: f64,
}

/// Number of retained-but-undelivered events exceeded no bound: retention
/// is bounded by the endpoint's configured retention capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LifecycleError {
    /// Delivery refused: the endpoint is fully closed.
    Closed,
    /// Retention capacity exhausted; the event was dropped and counted.
    RetentionFull,
    /// The endpoint was never opened.
    NotOpen,
}

/// The monotonic sequence stamp carried by every lifecycle-delivered event.
/// Order is assigned at delivery-acceptance time and preserved through
/// retention and drain.
pub type Sequence = u64;

/// Delivery endpoint lifecycle: Open -> Draining -> Closed.
///
/// The slice implements close/reentrant lifecycle handling for a delivery
/// endpoint: after invalidation, late-arriving events are retained (in
/// order) until an explicit drain releases them; nothing is silently lost,
/// and no API on this type joins a thread. All methods are safe to call
/// from the main thread only, matching the window seam.
#[derive(Debug)]
pub struct LifecycleEndpoint {
    state: EndpointState,
    retained: Vec<(Sequence, InputEvent)>,
    next_sequence: Sequence,
    retention_capacity: usize,
    dropped: u64,
    delivered: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointState {
    Open,
    Draining,
    Closed,
}

impl LifecycleEndpoint {
    /// Open an endpoint with bounded retention for late events.
    pub fn open(retention_capacity: usize) -> Result<Self, LifecycleError> {
        if retention_capacity == 0 {
            return Err(LifecycleError::NotOpen);
        }
        Ok(Self {
            state: EndpointState::Open,
            retained: Vec::new(),
            next_sequence: 1,
            retention_capacity,
            dropped: 0,
            delivered: 0,
        })
    }

    /// The endpoint's current lifecycle state.
    pub const fn state(&self) -> EndpointState {
        self.state
    }

    /// Monotonic sequence of the last accepted event.
    pub const fn last_sequence(&self) -> Sequence {
        if self.next_sequence == 1 {
            0
        } else {
            self.next_sequence - 1
        }
    }

    pub const fn dropped_count(&self) -> u64 {
        self.dropped
    }

    pub const fn delivered_count(&self) -> u64 {
        self.delivered
    }

    /// Accept one event for ordered delivery.
    ///
    /// Open endpoints deliver immediately (sequence stamped, delivered
    /// count advanced). Invalidated (draining) endpoints retain the event
    /// in arrival order until [`drain`], within the retention capacity;
    /// overflow drops the NEWEST event and counts it. Closed endpoints
    /// refuse with [`LifecycleError::Closed`].
    pub fn deliver(&mut self, event: InputEvent) -> Result<Sequence, LifecycleError> {
        match self.state {
            EndpointState::Closed => Err(LifecycleError::Closed),
            EndpointState::Draining => {
                if self.retained.len() >= self.retention_capacity {
                    self.dropped += 1;
                    return Err(LifecycleError::RetentionFull);
                }
                let sequence = self.next_sequence;
                self.next_sequence += 1;
                self.retained.push((sequence, event));
                Ok(sequence)
            }
            EndpointState::Open => {
                let sequence = self.next_sequence;
                self.next_sequence += 1;
                self.delivered += 1;
                Ok(sequence)
            }
        }
    }

    /// Invalidate the endpoint: Open -> Draining. Late events are retained.
    /// Idempotent for already-draining/closed endpoints.
    pub fn invalidate(&mut self) {
        if self.state == EndpointState::Open {
            self.state = EndpointState::Draining;
        }
    }

    /// Drain every retained event in arrival order through `sink`.
    ///
    /// The drain is single-shot: retained events are released in order and
    /// the endpoint transitions to Closed. Repeated drains observe Closed
    /// with nothing retained. The sink returns whether it accepted the
    /// event; a rejecting sink KEEPS the event retained (bounded sinks may
    /// reject; progress is preserved across repeated drains).
    pub fn drain(&mut self, mut sink: impl FnMut(Sequence, InputEvent) -> bool) -> usize {
        if self.state != EndpointState::Draining {
            return 0;
        }
        let mut drained = 0usize;
        let mut index = 0usize;
        while index < self.retained.len() {
            let (sequence, event) = self.retained[index].clone();
            if sink(sequence, event) {
                self.retained.remove(index);
                self.delivered += 1;
                drained += 1;
            } else {
                index += 1;
            }
        }
        if self.retained.is_empty() {
            self.state = EndpointState::Closed;
        }
        drained
    }

    /// Whether retained events await a drain.
    pub fn has_retained(&self) -> bool {
        !self.retained.is_empty()
    }

    /// Number of retained events.
    pub fn retained_len(&self) -> usize {
        self.retained.len()
    }
}

/// The bounded application event pump.
///
/// Wraps the shared NSApplication for manual, bounded event draining: a host
/// (or test) calls [`EventPump::pump`] with a small batch limit instead of
/// handing its main thread to `NSApplication::run`.
#[derive(Debug)]
pub struct EventPump {
    object: ffi::OwnedObject,
}

impl EventPump {
    /// Attach to the shared application on the main thread.
    pub fn attach(token: MainThreadToken) -> Result<Self, WindowingError> {
        token.assert_current()?;
        let object = ffi::shared_application_owned().ok_or(WindowingError::NativeUnavailable)?;
        Ok(Self { object })
    }

    /// Drain up to `batch` pending events into `queue`.
    ///
    /// Returns the number of events actually delivered; zero means the event
    /// stream had nothing pending. Events the decoder does not interpret are
    /// still counted and delivered as [`InputEvent::Uninterpreted`] so the
    /// bounded-queue contract covers every drained event.
    pub fn pump(
        &self,
        token: MainThreadToken,
        queue: &mut BoundedEventQueue,
        batch: usize,
    ) -> Result<usize, WindowingError> {
        token.assert_current()?;
        let mut delivered = 0usize;
        for _ in 0..batch.max(1) {
            let Some(event) = ffi::next_event_polled(self.object.as_ref()) else {
                break;
            };
            let decoded = decode_event(&event);
            delivered += 1;
            // A full queue refuses the event and counts the drop; the native
            // event object is released on scope exit either way.
            if queue.offer(decoded).is_err() {
                break;
            }
        }
        Ok(delivered)
    }

    /// Blocks until the next platform event, dispatches it through the
    /// responder chain, and services pending window updates. One iteration
    /// of the canonical manual event loop (nextEvent -> sendEvent ->
    /// updateWindows). Returns the number of events decoded into `queue`.
    pub fn service(
        &self,
        token: MainThreadToken,
        queue: &mut BoundedEventQueue,
    ) -> Result<usize, WindowingError> {
        token.assert_current()?;
        let mut delivered = 0usize;
        if let Some(event) = ffi::next_event_blocking(self.object.as_ref()) {
            let decoded = decode_event(&event);
            delivered += 1;
            let _ = queue.offer(decoded);
            ffi::send_event(self.object.as_ref(), event.as_ref());
        }
        ffi::update_windows();
        Ok(delivered)
    }

    /// Synthesize a real platform key event and post it to the application
    /// queue. Returns whether synthesis and posting both succeeded.
    pub fn post_key(
        &self,
        token: MainThreadToken,
        key_down: bool,
        key_code: u16,
        characters: &str,
        modifiers_mask: u64,
        repeat: bool,
    ) -> bool {
        token.assert_current().is_ok()
            && ffi::post_event(
                self.object.as_ref(),
                match ffi::synthesize_key_event(
                    key_down,
                    key_code,
                    characters,
                    modifiers_mask,
                    repeat,
                ) {
                    Some(event) => event.as_ref(),
                    None => return false,
                },
                true,
            )
    }

    /// Synthesize a real platform scroll event and post it for pumping.
    pub fn post_scroll(
        &self,
        token: MainThreadToken,
        delta_x: f64,
        delta_y: f64,
        precise: bool,
    ) -> bool {
        token.assert_current().is_ok()
            && ffi::post_event(
                self.object.as_ref(),
                match ffi::synthesize_scroll_event(delta_x, delta_y, precise) {
                    Some(event) => event.as_ref(),
                    None => return false,
                },
                true,
            )
    }
}

/// Decode a native event into the typed seam representation.
///
/// Interpreted types: key-down (10), key-up (11), flags-changed (12) and
/// scroll-wheel (22). Everything else arrives as `Uninterpreted` so the
/// bounded-queue contract covers every drained event without silent loss.
fn decode_event(event: &ffi::OwnedObject) -> InputEvent {
    let reference = event.as_ref();
    match ffi::event_raw_type(reference) {
        10 => InputEvent::Key(KeyEvent {
            keycode: ffi::event_key_code(reference),
            characters: ffi::event_characters_utf8(reference),
            modifiers: decode_modifier_flags(ffi::event_modifier_flags(reference)),
            phase: KeyPhase::Down,
            repeat: ffi::event_is_repeat(reference),
        }),
        11 => InputEvent::Key(KeyEvent {
            keycode: ffi::event_key_code(reference),
            characters: ffi::event_characters_utf8(reference),
            modifiers: decode_modifier_flags(ffi::event_modifier_flags(reference)),
            phase: KeyPhase::Up,
            repeat: ffi::event_is_repeat(reference),
        }),
        12 => InputEvent::Key(KeyEvent {
            keycode: 0,
            characters: None,
            modifiers: decode_modifier_flags(ffi::event_modifier_flags(reference)),
            phase: KeyPhase::Flags,
            repeat: false,
        }),
        22 => {
            let (delta_x, delta_y) = ffi::event_scroll_deltas(reference);
            InputEvent::Scroll(ScrollEvent {
                delta_x,
                delta_y,
                precise: ffi::event_has_precise_deltas(reference),
                phase: decode_scroll_phase(ffi::event_momentum_phase(reference)),
            })
        }
        raw_type => InputEvent::Uninterpreted { raw_type },
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;

    fn button(click: u64) -> InputEvent {
        InputEvent::Button(ButtonEvent {
            button: 0,
            phase: KeyPhase::Down,
            click_count: click,
            x: 1.0,
            y: 2.0,
        })
    }

    #[test]
    fn open_endpoint_delivers_immediately_with_monotonic_sequence() {
        let mut endpoint = LifecycleEndpoint::open(4).unwrap();
        assert_eq!(endpoint.state(), EndpointState::Open);
        assert_eq!(endpoint.deliver(button(1)).unwrap(), 1);
        assert_eq!(endpoint.deliver(button(2)).unwrap(), 2);
        assert_eq!(endpoint.delivered_count(), 2);
        assert_eq!(endpoint.last_sequence(), 2);
    }

    #[test]
    fn invalidation_retains_late_events_in_order_until_drain() {
        let mut endpoint = LifecycleEndpoint::open(8).unwrap();
        endpoint.deliver(button(1)).unwrap();
        endpoint.invalidate();
        assert_eq!(endpoint.state(), EndpointState::Draining);

        // Late events arrive after invalidation: retained, in order.
        endpoint.deliver(button(2)).unwrap();
        endpoint
            .deliver(InputEvent::FocusChanged { focused: true })
            .unwrap();
        endpoint
            .deliver(InputEvent::MarkedText {
                text: "kyoutou".to_string(),
            })
            .unwrap();
        assert_eq!(endpoint.retained_len(), 3);

        // Drain releases them in arrival order and closes the endpoint.
        let mut drained = Vec::new();
        let count = endpoint.drain(|sequence, event| {
            drained.push((sequence, event));
            true
        });
        assert_eq!(count, 3);
        assert_eq!(drained.len(), 3);
        assert_eq!(drained[0].0, 2);
        assert_eq!(drained[2].0, 4);
        assert!(matches!(drained[2].1, InputEvent::MarkedText { .. }));
        assert_eq!(endpoint.state(), EndpointState::Closed);
        assert!(!endpoint.has_retained());
    }

    #[test]
    fn closed_endpoint_refuses_delivery() {
        let mut endpoint = LifecycleEndpoint::open(2).unwrap();
        endpoint.invalidate();
        endpoint.drain(|_, _| true);
        assert_eq!(endpoint.state(), EndpointState::Closed);
        assert_eq!(endpoint.deliver(button(9)), Err(LifecycleError::Closed));
    }

    #[test]
    fn retention_overflow_drops_newest_and_counts() {
        let mut endpoint = LifecycleEndpoint::open(2).unwrap();
        endpoint.invalidate();
        endpoint.deliver(button(1)).unwrap();
        endpoint.deliver(button(2)).unwrap();
        assert_eq!(
            endpoint.deliver(button(3)),
            Err(LifecycleError::RetentionFull)
        );
        assert_eq!(endpoint.retained_len(), 2);
        assert_eq!(endpoint.dropped_count(), 1);
    }

    #[test]
    fn rejecting_sink_keeps_event_for_progressive_drain() {
        let mut endpoint = LifecycleEndpoint::open(4).unwrap();
        endpoint.invalidate();
        endpoint
            .deliver(InputEvent::FocusChanged { focused: true })
            .unwrap();
        endpoint
            .deliver(InputEvent::MarkedText {
                text: "marked".to_string(),
            })
            .unwrap();

        // A saturated sink rejects; retained events are preserved.
        let mut rejected = endpoint.drain(|_, _| false);
        assert_eq!(rejected, 0);
        assert!(endpoint.has_retained());

        // A later accepting drain makes progress without loss or reorder.
        rejected = endpoint.drain(|_, _| true);
        assert_eq!(rejected, 2);
        assert_eq!(endpoint.delivered_count(), 2);
    }

    #[test]
    fn ordered_button_and_marked_text_survive_a_close_race() {
        // Reentrant close: events synthesized during teardown still arrive
        // ordered after the events posted before it.
        let mut endpoint = LifecycleEndpoint::open(8).unwrap();
        endpoint.deliver(button(1)).unwrap();
        endpoint.invalidate();
        endpoint
            .deliver(InputEvent::Button(ButtonEvent {
                button: 0,
                phase: KeyPhase::Up,
                click_count: 1,
                x: 3.0,
                y: 4.0,
            }))
            .unwrap();
        endpoint
            .deliver(InputEvent::MarkedText {
                text: "ime".to_string(),
            })
            .unwrap();
        let mut order = Vec::new();
        endpoint.drain(|sequence, event| {
            order.push((
                sequence,
                matches!(event, InputEvent::Button(_)),
                matches!(event, InputEvent::MarkedText { .. }),
            ));
            true
        });
        assert_eq!(order.len(), 2);
        assert!(order[0].1, "button first");
        assert!(order[1].2, "marked text second");
    }

    #[test]
    fn zero_retention_capacity_is_refused_at_open() {
        assert!(matches!(
            LifecycleEndpoint::open(0),
            Err(LifecycleError::NotOpen)
        ));
    }

    #[test]
    fn idempotent_invalidation_does_not_double_drain_state() {
        let mut endpoint = LifecycleEndpoint::open(4).unwrap();
        endpoint.invalidate();
        endpoint.invalidate();
        assert_eq!(endpoint.state(), EndpointState::Draining);
        endpoint.drain(|_, _| true);
        endpoint.invalidate();
        assert_eq!(endpoint.state(), EndpointState::Closed);
    }
}
