#![forbid(unsafe_code)]

//! Focus management and host input transparency (FCB-053.B).
//!
//! In accordance with plan §13.6, §15.6, and §23.4:
//! - Focus transitions return cleanly to the prior active focus node when
//!   transient modals, search fields, or external actions dismiss/complete.
//! - Host input is NEVER intercepted: unhandled key events (such as system
//!   shortcuts Cmd+Q, Cmd+H, Cmd+M, Cmd+Tab, or host-registered bindings)
//!   are explicitly returned as `PassedToHost` so the host application
//!   responder chain processes them without dropped modifiers or stuck states.

/// Identifies a UI region or component capable of holding user input focus.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FocusNode {
    Atlas,
    SourceViewer,
    SearchField,
    TerminalPanel,
    OutlineView,
    Dialog,
}

/// Tracks focus transitions and restores focus cleanly on dismissal or completion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FocusReturnTracker {
    current: FocusNode,
    history: Vec<FocusNode>,
    default_fallback: FocusNode,
    max_history_depth: usize,
}

impl FocusReturnTracker {
    pub fn new(default_fallback: FocusNode) -> Self {
        Self {
            current: default_fallback,
            history: Vec::new(),
            default_fallback,
            max_history_depth: 32,
        }
    }

    pub fn current(&self) -> FocusNode {
        self.current
    }

    pub fn history_depth(&self) -> usize {
        self.history.len()
    }

    /// Moves focus to `target`, recording the prior node in the history stack.
    pub fn focus(&mut self, target: FocusNode) {
        if self.current != target {
            self.history.push(self.current);
            if self.history.len() > self.max_history_depth {
                self.history.remove(0);
            }
            self.current = target;
        }
    }

    /// Returns focus to the immediately preceding focus node, or to `default_fallback`
    /// if no prior node exists.
    pub fn return_focus(&mut self) -> FocusNode {
        if let Some(prev) = self.history.pop() {
            self.current = prev;
        } else {
            self.current = self.default_fallback;
        }
        self.current
    }

    /// Clears history and resets focus to the default fallback.
    pub fn reset(&mut self) {
        self.history.clear();
        self.current = self.default_fallback;
    }
}

/// Keyboard modifier keys held during an input event.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct KeyModifiers {
    pub cmd: bool,
    pub ctrl: bool,
    pub alt: bool,
    pub shift: bool,
}

impl KeyModifiers {
    pub const NONE: Self = Self {
        cmd: false,
        ctrl: false,
        alt: false,
        shift: false,
    };

    pub fn cmd() -> Self {
        Self {
            cmd: true,
            ..Default::default()
        }
    }

    pub fn shift() -> Self {
        Self {
            shift: true,
            ..Default::default()
        }
    }

    pub fn cmd_shift() -> Self {
        Self {
            cmd: true,
            shift: true,
            ..Default::default()
        }
    }
}

/// Logical key stroke action.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyAction {
    Character(char),
    ArrowUp,
    ArrowDown,
    ArrowLeft,
    ArrowRight,
    PageUp,
    PageDown,
    Home,
    End,
    Enter,
    Escape,
    Tab,
    Backspace,
    Delete,
    FKey(u8),
    Other(String),
}

/// Outcome of dispatching a key event through the FCB responder chain.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InputDispatchResult {
    /// Handled by the focused FCB component.
    Handled {
        target: FocusNode,
        action: String,
    },
    /// Not handled by FCB; passed through to host OS responder chain without interception.
    PassedToHost {
        reason: String,
    },
}

/// Dispatches a key event given the currently focused node and modifiers.
///
/// Ensures strict adherence to the contract:
/// - Host system shortcuts (Cmd+Q, Cmd+H, Cmd+M, Cmd+Tab) are NEVER intercepted.
/// - Unhandled function keys and host hotkeys pass through to host.
/// - Focused component actions are processed and reported cleanly.
pub fn dispatch_key_input(
    focused: FocusNode,
    key: &KeyAction,
    modifiers: KeyModifiers,
) -> InputDispatchResult {
    // 1. Mandatory host non-interception for system shortcuts:
    if modifiers.cmd && !modifiers.ctrl && !modifiers.alt {
        match key {
            KeyAction::Character('q' | 'Q') => {
                return InputDispatchResult::PassedToHost {
                    reason: "Application quit (Cmd+Q) passed to host".to_string(),
                };
            }
            KeyAction::Character('h' | 'H') => {
                return InputDispatchResult::PassedToHost {
                    reason: "Application hide (Cmd+H) passed to host".to_string(),
                };
            }
            KeyAction::Character('m' | 'M') => {
                return InputDispatchResult::PassedToHost {
                    reason: "Window minimize (Cmd+M) passed to host".to_string(),
                };
            }
            KeyAction::Tab => {
                return InputDispatchResult::PassedToHost {
                    reason: "Application switcher (Cmd+Tab) passed to host".to_string(),
                };
            }
            _ => {}
        }
    }

    // 2. Global application-level commands with Cmd modifier:
    if modifiers.cmd && !modifiers.ctrl && !modifiers.alt {
        match key {
            KeyAction::Character('c' | 'C') => {
                return InputDispatchResult::Handled {
                    target: focused,
                    action: "Copy".to_string(),
                };
            }
            KeyAction::Character('v' | 'V') => {
                return InputDispatchResult::Handled {
                    target: focused,
                    action: "Paste".to_string(),
                };
            }
            KeyAction::Character('x' | 'X') => {
                return InputDispatchResult::Handled {
                    target: focused,
                    action: "Cut".to_string(),
                };
            }
            KeyAction::Character('a' | 'A') => {
                return InputDispatchResult::Handled {
                    target: focused,
                    action: "Select All".to_string(),
                };
            }
            KeyAction::Character('z' | 'Z') => {
                return InputDispatchResult::Handled {
                    target: focused,
                    action: if modifiers.shift { "Redo" } else { "Undo" }.to_string(),
                };
            }
            KeyAction::Character('f' | 'F') => {
                return InputDispatchResult::Handled {
                    target: FocusNode::SearchField,
                    action: "Find".to_string(),
                };
            }
            _ => {}
        }
    }

    // 3. Node-specific input handling:
    match focused {
        FocusNode::SearchField => match key {
            KeyAction::Escape => InputDispatchResult::Handled {
                target: FocusNode::SearchField,
                action: "Dismiss Search".to_string(),
            },
            KeyAction::Enter => InputDispatchResult::Handled {
                target: FocusNode::SearchField,
                action: "Submit Query".to_string(),
            },
            KeyAction::Backspace => InputDispatchResult::Handled {
                target: FocusNode::SearchField,
                action: "Delete Backward".to_string(),
            },
            KeyAction::Character(c) if !modifiers.cmd && !modifiers.ctrl => {
                InputDispatchResult::Handled {
                    target: FocusNode::SearchField,
                    action: format!("Type '{c}'"),
                }
            }
            _ => InputDispatchResult::PassedToHost {
                reason: "Unhandled key in search field passed to host".to_string(),
            },
        },
        FocusNode::SourceViewer => match key {
            KeyAction::ArrowUp | KeyAction::ArrowDown | KeyAction::PageUp | KeyAction::PageDown => {
                InputDispatchResult::Handled {
                    target: FocusNode::SourceViewer,
                    action: "Navigate Source Lines".to_string(),
                }
            }
            KeyAction::ArrowLeft | KeyAction::ArrowRight => InputDispatchResult::Handled {
                target: FocusNode::SourceViewer,
                action: "Move Caret Horizontal".to_string(),
            },
            _ => InputDispatchResult::PassedToHost {
                reason: "Unhandled key in source viewer passed to host".to_string(),
            },
        },
        FocusNode::Atlas => match key {
            KeyAction::ArrowUp
            | KeyAction::ArrowDown
            | KeyAction::ArrowLeft
            | KeyAction::ArrowRight => InputDispatchResult::Handled {
                target: FocusNode::Atlas,
                action: "Pan Atlas".to_string(),
            },
            _ => InputDispatchResult::PassedToHost {
                reason: "Unhandled key in atlas passed to host".to_string(),
            },
        },
        FocusNode::TerminalPanel | FocusNode::OutlineView | FocusNode::Dialog => {
            if matches!(key, KeyAction::Escape) {
                InputDispatchResult::Handled {
                    target: focused,
                    action: "Dismiss/Cancel".to_string(),
                }
            } else {
                InputDispatchResult::PassedToHost {
                    reason: "Unhandled key passed to host".to_string(),
                }
            }
        }
    }
}
