//! Standard menu command routing over a bounded editor state (fcb-8bqc.1).
//!
//! Translates native menu actions (`copy:`, `paste:`, `cut:`,
//! `selectAll:`, undo/redo, find) into deterministic effects against the
//! editor state, with the same enabled/disabled predicates AppKit computes
//! for menu validation. Commands that would shell out are structurally
//! absent: the router can only mutate bounded editor state.

use fcb_core::{CoreError, Utf16CodeUnitOffset, Utf16CodeUnitRange};

/// Standard edit commands the native menus dispatch.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EditorCommand {
    Copy,
    Cut,
    Paste,
    SelectAll,
    Delete,
    Undo,
    Redo,
    Find,
}

/// A bounded pasteboard: the clipboard payload FCB writes and reads.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Pasteboard {
    content: Option<String>,
}

impl Pasteboard {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn set(&mut self, text: &str) {
        self.content = (!text.is_empty()).then(|| text.to_string());
    }

    pub fn get(&self) -> Option<&str> {
        self.content.as_deref()
    }

    pub fn clear(&mut self) {
        self.content = None;
    }
}

/// One undoable editing step.
#[derive(Clone, Debug, PartialEq, Eq)]
struct UndoStep {
    /// Document text before the step.
    before: String,
    /// Document text after the step.
    after: String,
    /// Selection before the step.
    selection_before: (u64, u64),
    /// Selection after the step.
    selection_after: (u64, u64),
    /// Human-facing label (`Undo Cut` / `Redo Paste`).
    action: &'static str,
}

/// Bounded editor state the command router mutates.
pub struct EditorState {
    text: String,
    selection: Utf16CodeUnitRange,
    clipboard: Pasteboard,
    undo: Vec<UndoStep>,
    redo: Vec<UndoStep>,
    max_undo_steps: usize,
    max_text_bytes: usize,
}

/// The outcome of routing one command.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum CommandEffect {
    /// The command applied; describes what changed.
    Applied {
        action: &'static str,
        changed_text: bool,
    },
    /// The command is currently disabled (e.g. Copy with empty selection).
    Disabled,
}

impl EditorState {
    pub fn new(max_text_bytes: usize, max_undo_steps: usize) -> Self {
        Self {
            text: String::new(),
            selection: range(0, 0),
            clipboard: Pasteboard::new(),
            undo: Vec::new(),
            redo: Vec::new(),
            max_undo_steps: max_undo_steps.max(1),
            max_text_bytes,
        }
    }

    pub fn set_document(&mut self, text: &str) -> Result<(), CoreError> {
        if text.len() > self.max_text_bytes {
            return Err(CoreError::LimitExceeded);
        }
        self.text = text.to_string();
        self.selection = range(0, 0);
        Ok(())
    }

    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn selection(&self) -> Utf16CodeUnitRange {
        self.selection
    }

    pub fn set_selection(&mut self, selection: Utf16CodeUnitRange) -> Result<(), CoreError> {
        let (start, end) = ordered(selection)?;
        let len = units_of(&self.text);
        if start > len || end > len {
            return Err(CoreError::LimitExceeded);
        }
        self.selection = range(start, end);
        Ok(())
    }

    pub fn pasteboard(&self) -> &Pasteboard {
        &self.clipboard
    }

    /// The same predicate a native menu validator would compute.
    pub fn is_enabled(&self, command: EditorCommand) -> bool {
        let (sel_start, sel_end) = (
            self.selection.start().get(),
            self.selection.end().get(),
        );
        match command {
            EditorCommand::Copy | EditorCommand::Cut => sel_end > sel_start,
            EditorCommand::Paste => self.clipboard.get().is_some(),
            EditorCommand::SelectAll | EditorCommand::Find => true,
            EditorCommand::Delete => sel_end > sel_start,
            EditorCommand::Undo => !self.undo.is_empty(),
            EditorCommand::Redo => !self.redo.is_empty(),
        }
    }

    /// Routes one command. Side effects stay in bounded memory: cut/paste
    /// mutate the document through the same budgeted replacement path.
    pub fn route(&mut self, command: EditorCommand) -> Result<CommandEffect, CoreError> {
        if !self.is_enabled(command) {
            return Ok(CommandEffect::Disabled);
        }
        match command {
            EditorCommand::Copy => {
                let text = self.selected_text();
                self.clipboard.set(&text);
                Ok(CommandEffect::Applied { action: "copy", changed_text: false })
            }
            EditorCommand::Cut => {
                let text = self.selected_text();
                self.clipboard.set(&text);
                self.push_undo("Cut")?;
                let (start, end) = self.selection_bounds();
                self.replace(start, end, "")?;
                self.finish_step("Cut");
                Ok(CommandEffect::Applied { action: "cut", changed_text: true })
            }
            EditorCommand::Paste => {
                let Some(payload) = self.clipboard.get().map(str::to_string) else {
                    return Ok(CommandEffect::Disabled);
                };
                self.push_undo("Paste")?;
                let (start, end) = self.selection_bounds();
                self.replace(start, end, &payload)?;
                self.finish_step("Paste");
                Ok(CommandEffect::Applied { action: "paste", changed_text: true })
            }
            EditorCommand::SelectAll => {
                let len = units_of(&self.text);
                self.selection = range(0, len);
                Ok(CommandEffect::Applied { action: "selectAll", changed_text: false })
            }
            EditorCommand::Delete => {
                self.push_undo("Delete")?;
                let (start, end) = self.selection_bounds();
                self.replace(start, end, "")?;
                self.finish_step("Delete");
                Ok(CommandEffect::Applied { action: "delete", changed_text: true })
            }
            EditorCommand::Undo => {
                let Some(step) = self.undo.pop() else {
                    return Ok(CommandEffect::Disabled);
                };
                self.text = step.before.clone();
                self.selection = range(step.selection_before.0, step.selection_before.1);
                self.redo.push(step);
                Ok(CommandEffect::Applied { action: "undo", changed_text: true })
            }
            EditorCommand::Redo => {
                let Some(step) = self.redo.pop() else {
                    return Ok(CommandEffect::Disabled);
                };
                self.text = step.after.clone();
                self.selection = range(step.selection_after.0, step.selection_after.1);
                self.undo.push(step);
                Ok(CommandEffect::Applied { action: "redo", changed_text: true })
            }
            EditorCommand::Find => Ok(CommandEffect::Applied { action: "find", changed_text: false }),
        }
    }

    fn selected_text(&self) -> String {
        let (start, end) = self.selection_bounds();
        let b_start = units_to_byte(&self.text, start);
        let b_end = units_to_byte(&self.text, end);
        self.text[b_start..b_end].to_string()
    }

    fn selection_bounds(&self) -> (u64, u64) {
        (
            self.selection.start().get(),
            self.selection.end().get(),
        )
    }

    fn push_undo(&mut self, action: &'static str) -> Result<(), CoreError> {
        let (start, end) = self.selection_bounds();
        self.undo.push(UndoStep {
            before: self.text.clone(),
            after: String::new(),
            selection_before: (start, end),
            selection_after: (0, 0),
            action,
        });
        if self.undo.len() > self.max_undo_steps {
            self.undo.remove(0);
        }
        Ok(())
    }

    fn finish_step(&mut self, action: &'static str) {
        let after = self.text.clone();
        let selection_after = self.selection_bounds();
        if let Some(step) = self.undo.last_mut() {
            step.after = after;
            step.selection_after = selection_after;
        }
        let _ = action;
        self.redo.clear();
    }

    fn replace(&mut self, start: u64, end: u64, text: &str) -> Result<(), CoreError> {
        let len = units_of(&self.text);
        if start > len || end > len || start > end {
            return Err(CoreError::LimitExceeded);
        }
        let byte_start = units_to_byte(&self.text, start);
        let byte_end = units_to_byte(&self.text, end);
        let mut next = String::with_capacity(self.text.len() + text.len());
        next.push_str(&self.text[..byte_start]);
        next.push_str(text);
        next.push_str(&self.text[byte_end..]);
        if next.len() > self.max_text_bytes {
            return Err(CoreError::LimitExceeded);
        }
        self.text = next;
        self.selection = range(start, start + units_of(text));
        Ok(())
    }
}

/// Total UTF-16 code units of `text`.
fn units_of(text: &str) -> u64 {
    text.chars().map(|ch| u64::from(ch.len_utf16() as u16)).sum()
}

/// Byte offset of the UTF-16 unit boundary `units`.
fn units_to_byte(text: &str, units: u64) -> usize {
    let mut seen = 0_u64;
    let mut bytes = 0_usize;
    for ch in text.chars() {
        if seen >= units {
            break;
        }
        seen += u64::from(ch.len_utf16() as u16);
        bytes += ch.len_utf8();
    }
    bytes
}

fn ordered(range: Utf16CodeUnitRange) -> Result<(u64, u64), CoreError> {
    let (start, end) = (range.start().get(), range.end().get());
    if start > end {
        return Err(CoreError::LimitExceeded);
    }
    Ok((start, end))
}

fn range(start: u64, end: u64) -> Utf16CodeUnitRange {
    Utf16CodeUnitRange::new(Utf16CodeUnitOffset::new(start), Utf16CodeUnitOffset::new(end))
        .expect("ordered range valid")
}
