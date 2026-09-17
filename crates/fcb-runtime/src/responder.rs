#![forbid(unsafe_code)]

//! Host-compliant responder chain hierarchy (§23.5).
//!
//! Preserves the embedding host's menu, responder chain, and focus hierarchy.
//! Attaching an FCB view dispatches input and keyboard commands along a strictly
//! bounded chain without installing application-global event monitors that could
//! intercept unrelated host input.

use fcb_core::ArenaOwnerId;

/// Actions dispatchable through the host responder chain.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum ResponderAction {
    Copy,
    SelectAll,
    Find,
    MoveUp,
    MoveDown,
    MoveLeft,
    MoveRight,
    PageUp,
    PageDown,
    ReturnFocus,
    DismissOverlay,
}

/// Identifies a responder in the hierarchy.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct ResponderId {
    owner: ArenaOwnerId,
    id: u64,
}

impl ResponderId {
    pub const fn new(owner: ArenaOwnerId, id: u64) -> Self {
        Self { owner, id }
    }

    pub const fn owner(self) -> ArenaOwnerId {
        self.owner
    }

    pub const fn id(self) -> u64 {
        self.id
    }
}

/// A node in the responder chain.
pub struct ResponderNode {
    id: ResponderId,
    name: &'static str,
    handler: Box<dyn Fn(ResponderAction) -> bool + Send + Sync>,
}

impl std::fmt::Debug for ResponderNode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResponderNode")
            .field("id", &self.id)
            .field("name", &self.name)
            .finish()
    }
}

/// An ordered responder chain modeling macOS `NSResponder`.
///
/// Dispatches commands starting from the first responder (deepest focused view)
/// up through containing panels, windows, and host delegates. Stops at the first
/// node that handles the action.
#[derive(Debug)]
pub struct HostResponderChain {
    owner: ArenaOwnerId,
    responders: Vec<ResponderNode>,
}

impl HostResponderChain {
    pub const fn new(owner: ArenaOwnerId) -> Self {
        Self {
            owner,
            responders: Vec::new(),
        }
    }

    pub const fn owner(&self) -> ArenaOwnerId {
        self.owner
    }

    pub fn len(&self) -> usize {
        self.responders.len()
    }

    pub fn is_empty(&self) -> bool {
        self.responders.is_empty()
    }

    /// Registers a responder at the head (first responder) of the chain.
    pub fn push_first_responder(
        &mut self,
        id: ResponderId,
        name: &'static str,
        handler: impl Fn(ResponderAction) -> bool + Send + Sync + 'static,
    ) {
        if id.owner() != self.owner {
            return;
        }
        self.responders.insert(
            0,
            ResponderNode {
                id,
                name,
                handler: Box::new(handler),
            },
        );
    }

    /// Registers a responder at the tail (fallback/window/host) of the chain.
    pub fn push_fallback_responder(
        &mut self,
        id: ResponderId,
        name: &'static str,
        handler: impl Fn(ResponderAction) -> bool + Send + Sync + 'static,
    ) {
        if id.owner() != self.owner {
            return;
        }
        self.responders.push(ResponderNode {
            id,
            name,
            handler: Box::new(handler),
        });
    }

    /// Removes a responder by ID.
    pub fn remove_responder(&mut self, id: ResponderId) -> bool {
        if let Some(pos) = self.responders.iter().position(|r| r.id == id) {
            self.responders.remove(pos);
            true
        } else {
            false
        }
    }

    /// Dispatches an action along the chain.
    ///
    /// Returns the [`ResponderId`] of the first node that handled the action,
    /// or `None` if unhandled by all responders.
    pub fn dispatch(&self, action: ResponderAction) -> Option<ResponderId> {
        for responder in &self.responders {
            if (responder.handler)(action) {
                return Some(responder.id);
            }
        }
        None
    }
}
