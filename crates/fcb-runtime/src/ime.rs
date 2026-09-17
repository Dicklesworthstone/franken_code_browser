#![forbid(unsafe_code)]

//! Native text input and IME multi-stage composition state machine (§13.6).
//!
//! Models macOS `NSTextInputClient` protocol mechanics. Search fields and input
//! surfaces implement composition ranges and marked text without prematurely
//! issuing a new finalized search query for every intermediate keystroke in a
//! multi-stage input method (e.g. Japanese Kanji conversion, Chinese Pinyin,
//! accent dead keys). Source views remain strictly read-only.

use fcb_core::{
    geometry::{Point2D, Rect2D},
    ArenaOwnerId, CoreError, Utf16CodeUnitOffset, Utf16CodeUnitRange, NATIVE_NOT_FOUND,
};

/// The internal state of an IME composition session.
#[derive(Clone, Debug, PartialEq)]
pub enum ImeState {
    /// No marked text; ordinary typing or navigation.
    Idle,
    /// Active multi-stage composition.
    Composing {
        marked_text: String,
        /// Selection or caret within the marked text (character indices 0..len).
        selection_in_marked: (usize, usize),
        /// Range of existing text being replaced by this composition, if any.
        replacement_range: Option<Utf16CodeUnitRange>,
        /// Generation sequence for this composition turn.
        sequence: u64,
    },
}

/// An incoming event from the platform's text input subsystem (`NSTextInputClient`).
#[derive(Clone, Debug, PartialEq)]
pub enum ImeEvent {
    /// Update or start composition with new candidate marked text.
    SetMarkedText {
        text: String,
        selection_in_marked: (usize, usize),
        replacement_range: Option<Utf16CodeUnitRange>,
    },
    /// Discard the marked state and accept the currently marked text.
    UnmarkText,
    /// Finalize the composition and commit the inserted text.
    InsertText {
        text: String,
        replacement_range: Option<Utf16CodeUnitRange>,
    },
    /// Discard and cancel the active composition without committing text.
    CancelComposition,
}

/// The outcome of processing an [`ImeEvent`].
#[derive(Clone, Debug, PartialEq)]
pub enum ImeOutcome {
    /// Marked text was updated.
    ///
    /// The `query_suppressed` flag indicates that no new search query should be
    /// fired for this intermediate state (§13.6).
    CompositionUpdated {
        marked_text: String,
        query_suppressed: bool,
    },
    /// A finalized string was committed. Search queries or text insertion
    /// may now proceed.
    FinalizedTextInserted {
        text: String,
        replacement_range: Option<Utf16CodeUnitRange>,
    },
    /// Composition was cancelled or cleared without inserting text.
    CompositionCancelled,
}

/// Client managing native text composition and IME state for an input surface.
#[derive(Debug)]
pub struct ImeClient {
    owner: ArenaOwnerId,
    state: ImeState,
    sequence: u64,
}

impl ImeClient {
    pub const fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            state: ImeState::Idle,
            sequence: 0,
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    /// Whether an IME composition session is actively in progress.
    pub const fn has_marked_text(&self) -> bool {
        matches!(self.state, ImeState::Composing { .. })
    }

    /// The currently active marked text, if any.
    pub fn marked_text(&self) -> Option<&str> {
        match &self.state {
            ImeState::Composing { marked_text, .. } => Some(marked_text.as_str()),
            ImeState::Idle => None,
        }
    }

    /// The UTF-16 code unit range of the active marked text, anchored at `base_offset`.
    pub fn marked_range(&self, base_offset: Utf16CodeUnitOffset) -> Option<Utf16CodeUnitRange> {
        match &self.state {
            ImeState::Composing { marked_text, .. } => {
                let len: u64 = marked_text.chars().map(|c| c.len_utf16() as u64).sum();
                let end = Utf16CodeUnitOffset::new(base_offset.get() + len);
                Utf16CodeUnitRange::new(base_offset, end).ok()
            }
            ImeState::Idle => None,
        }
    }

    /// Process a platform text input event according to the `NSTextInputClient` contract.
    pub fn handle_event(&mut self, event: ImeEvent) -> ImeOutcome {
        match event {
            ImeEvent::SetMarkedText {
                text,
                selection_in_marked,
                replacement_range,
            } => {
                self.sequence = self.sequence.wrapping_add(1);
                let marked = text.clone();
                self.state = ImeState::Composing {
                    marked_text: text,
                    selection_in_marked,
                    replacement_range,
                    sequence: self.sequence,
                };
                ImeOutcome::CompositionUpdated {
                    marked_text: marked,
                    query_suppressed: true,
                }
            }
            ImeEvent::UnmarkText => match std::mem::replace(&mut self.state, ImeState::Idle) {
                ImeState::Composing {
                    marked_text,
                    replacement_range,
                    ..
                } => ImeOutcome::FinalizedTextInserted {
                    text: marked_text,
                    replacement_range,
                },
                ImeState::Idle => ImeOutcome::CompositionCancelled,
            },
            ImeEvent::InsertText {
                text,
                replacement_range,
            } => {
                self.state = ImeState::Idle;
                ImeOutcome::FinalizedTextInserted {
                    text,
                    replacement_range,
                }
            }
            ImeEvent::CancelComposition => {
                self.state = ImeState::Idle;
                ImeOutcome::CompositionCancelled
            }
        }
    }

    /// Computes the visual bounding rectangle for candidate window positioning
    /// (`firstRectForCharacterRange:actualRange:` in AppKit).
    pub fn candidate_window_rect(
        &self,
        caret_origin: Point2D,
        line_height: f64,
        char_width: f64,
    ) -> Result<Rect2D, CoreError> {
        let (offset_chars, width_chars) = match &self.state {
            ImeState::Composing {
                selection_in_marked,
                ..
            } => {
                let start = selection_in_marked.0 as f64;
                let len = (selection_in_marked.1.saturating_sub(selection_in_marked.0)).max(1) as f64;
                (start, len)
            }
            ImeState::Idle => (0.0, 1.0),
        };

        let x = caret_origin.x() + (offset_chars * char_width);
        let y = caret_origin.y();
        let width = (width_chars * char_width).max(1.0);
        let height = line_height.max(1.0);

        Rect2D::from_xywh(x, y, width, height)
    }

    /// Maps a click coordinate to a UTF-16 code unit offset
    /// (`characterIndexForPoint:` in AppKit).
    pub fn character_index_for_point(
        &self,
        click_point: Point2D,
        text_origin: Point2D,
        char_width: f64,
        max_units: u64,
    ) -> Result<Utf16CodeUnitOffset, CoreError> {
        if char_width <= 0.0 {
            return Err(CoreError::InvalidGeometry);
        }
        let rel_x = click_point.x() - text_origin.x();
        if rel_x < 0.0 {
            return Ok(Utf16CodeUnitOffset::new(0));
        }

        let chars = (rel_x / char_width).round() as u64;
        let clamped = chars.min(max_units);
        if clamped == NATIVE_NOT_FOUND {
            return Err(CoreError::NativeSentinel);
        }
        Ok(Utf16CodeUnitOffset::new(clamped))
    }
}
