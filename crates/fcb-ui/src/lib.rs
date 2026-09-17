#![forbid(unsafe_code)]

//! Deterministic FCB UI core (fcb-ui, FCB-053.A).
//!
//! This crate is the headless, fully deterministic seam between native
//! AppKit behavior and the FCB engines: standard menu command routing, the
//! IME text-composition state machine a native `NSTextInputClient`
//! implementation drives, and open/drop request validation. It performs no
//! I/O, no GPU waits, no logging, and never executes anything: per the plan
//! (§23.4) opening source is read-only and shell execution is refused by
//! construction.
//!
//! The native side (franken_macos) translates `NSTextInputClient`,
//! `NSMenuItem` actions, `NSOpenPanel` outcomes, and drag-and-drop pasteboard
//! payloads into calls here; the adjacent FCB-053.V child qualifies the live
//! responder chain end to end.

pub mod clipboard;
pub mod commands;
pub mod composition;
pub mod editor_link;
pub mod focus_responder;
pub mod open_drop;
pub mod scope_disclosure;

pub use clipboard::{
    ClipboardError, ClipboardFlavor, ClipboardPayload, NativePasteboard,
};
pub use commands::{CommandEffect, EditorCommand, EditorState, Pasteboard};
pub use composition::{CompositionError, CompositionState, MarkedRegion};
pub use editor_link::{
    ActionRefusalReason, StructuredOsCommand, validate_editor_handoff, validate_web_link_handoff,
};
pub use focus_responder::{
    dispatch_key_input, FocusNode, FocusReturnTracker, InputDispatchResult, KeyAction, KeyModifiers,
};
pub use open_drop::{OpenDecision, OpenRequest, RefusalReason, canceled_dialog, validate_open_request};
pub use scope_disclosure::{
    evaluate_open_file, normalize_lexical_path, validate_symlink_confinement, GrantedRoot,
    ScopeDisclosure, ScopeEvaluation, ScopeRefusal,
};

