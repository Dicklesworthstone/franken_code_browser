//! Instance-safe native callback registration and owned callback state.
//!
//! This module is the safe state machinery that a native trampoline layer
//! (qualified separately under FCB-003.v with real callbacks and the ABI
//! ledger) is built on. It owns three obligations from the bridge safety
//! contract:
//!
//! 1. **Instance-safe class naming.** Every registered callback class gets a
//!    process-unique name minted from a consumer tag, an embedding-instance
//!    identity, and a monotonic nonce. Issued names are tombstoned for the
//!    process lifetime: repeated embedding can never collide on a
//!    process-global class name, and shutting one instance down can never
//!    release a name another instance could be tricked into reusing.
//! 2. **Owned callback state with reentrancy guards.** Callback state lives
//!    in the [`CallbackCell`], not on a native stack frame. Reentrant
//!    invocation is rejected for the duration of an invocation and the guard
//!    releases on unwind, so a panic can never wedge the callback.
//! 3. **Late-shutdown rejection.** [`CallbackCell::shutdown`] retires the
//!    state; any callback arriving afterwards is answered with
//!    [`CallbackError::Closed`] instead of touching dropped state.
//!
//! Everything here is safe Rust: the native trampoline that calls
//! [`CallbackCell::invoke`] from an Objective-C method lands behind the
//! audited FFI in a later qualification step. The primitives are generic
//! over the owned state, so neither FCB nor `fmd-font-macos` needs to host
//! the other's types. Aggregate invocation counters (accepted and rejected,
//! per rejection class) are retained for verifier reconciliation.

use std::cell::{Cell, RefCell};
use std::collections::HashSet;
use std::fmt;
use std::rc::Rc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{LazyLock, Mutex};

use crate::MainThreadToken;

/// Longest accepted consumer tag embedded in generated class names.
pub const MAX_CONSUMER_BYTES: usize = 32;

static CLASS_NAME_NONCE: AtomicU64 = AtomicU64::new(1);
static INSTANCE_NONCE: AtomicU64 = AtomicU64::new(1);
static ISSUED_CLASS_NAMES: LazyLock<Mutex<HashSet<String>>> =
    LazyLock::new(|| Mutex::new(HashSet::new()));

fn issued_class_names() -> &'static Mutex<HashSet<String>> {
    &ISSUED_CLASS_NAMES
}

/// Identity of one embedding instance. Two independent embeddings of the
/// library in one process hold distinct [`InstanceId`]s, which is what makes
/// class registration instance-safe.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct InstanceId(u64);

impl InstanceId {
    /// Mint the next process-unique embedding-instance identity.
    pub fn next() -> Self {
        Self(INSTANCE_NONCE.fetch_add(1, Ordering::Relaxed))
    }
}

/// Errors surfaced by the callback state machinery.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CallbackError {
    /// The callback state was shut down: late callbacks are rejected, not
    /// served from dropped state.
    Closed,
    /// A callback attempted to reenter the same cell during an active
    /// invocation, which would conflict with the in-flight mutable access.
    Reentrant,
    /// The caller is not on the owning thread.
    WrongThread,
    /// A generated or claimed class name is already issued. Defensive: the
    /// nonce makes generation collisions unreachable.
    ClassNameCollision,
    /// The consumer tag is empty, too long, or uses forbidden bytes.
    InvalidConsumerName,
}

impl fmt::Display for CallbackError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Closed => f.write_str("callback state is shut down"),
            Self::Reentrant => f.write_str("reentrant callback invocation rejected"),
            Self::WrongThread => f.write_str("callback invoked from the wrong thread"),
            Self::ClassNameCollision => f.write_str("class name already issued"),
            Self::InvalidConsumerName => f.write_str("consumer tag is empty, too long, or invalid"),
        }
    }
}

impl std::error::Error for CallbackError {}

fn consumer_bytes_valid(consumer: &str) -> bool {
    !consumer.is_empty()
        && consumer.len() <= MAX_CONSUMER_BYTES
        && consumer
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Mint a process-unique Objective-C class name for `consumer` on
/// `instance`. The name shape is `{consumer}_{instance:03}_{nonce:016x}`.
/// Minting is pure; [`claim_class_name`] performs the tombstoning.
pub fn generate_class_name(consumer: &str, instance: InstanceId) -> Result<String, CallbackError> {
    if !consumer_bytes_valid(consumer) {
        return Err(CallbackError::InvalidConsumerName);
    }
    let nonce = CLASS_NAME_NONCE.fetch_add(1, Ordering::Relaxed);
    Ok(format!("{consumer}_{:03}_{nonce:016x}", instance.0))
}

/// Claim a class name for the process lifetime. Issued names are tombstoned:
/// a second claim of the same name fails with
/// [`CallbackError::ClassNameCollision`] even after the owning instance shuts
/// down, so a late registrant can never quietly take over another
/// registration's identity.
pub fn claim_class_name(name: String) -> Result<(), CallbackError> {
    let mut issued = issued_class_names()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if issued.contains(&name) {
        return Err(CallbackError::ClassNameCollision);
    }
    issued.insert(name);
    Ok(())
}

/// Whether a class name has been issued in this process.
pub fn class_name_is_issued(name: &str) -> bool {
    issued_class_names()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .contains(name)
}

/// A registered class name bound to one embedding instance.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RegisteredClassName {
    instance: InstanceId,
    name: String,
}

impl RegisteredClassName {
    /// Generate and claim a fresh, instance-scoped class name.
    pub fn issue(consumer: &str, instance: InstanceId) -> Result<Self, CallbackError> {
        let name = generate_class_name(consumer, instance)?;
        claim_class_name(name.clone())?;
        Ok(Self { instance, name })
    }

    pub const fn instance(&self) -> InstanceId {
        self.instance
    }

    pub fn as_str(&self) -> &str {
        &self.name
    }
}

/// Aggregate invocation counters for one callback cell, mirroring the
/// bridge's `OwnershipSnapshot` style. Counters are monotonic, saturate at
/// `u64::MAX`, are shared across clones, and record rejected as well as
/// accepted attempts so a verifier can reconcile attempts against outcomes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CallbackCounters {
    pub invocations_accepted: u64,
    pub rejected_closed: u64,
    pub rejected_reentrant: u64,
    pub rejected_wrong_thread: u64,
    pub shutdowns: u64,
}

#[derive(Debug)]
struct CallbackCountersCell {
    invocations_accepted: Cell<u64>,
    rejected_closed: Cell<u64>,
    rejected_reentrant: Cell<u64>,
    rejected_wrong_thread: Cell<u64>,
    shutdowns: Cell<u64>,
}

impl CallbackCountersCell {
    const fn new() -> Self {
        Self {
            invocations_accepted: Cell::new(0),
            rejected_closed: Cell::new(0),
            rejected_reentrant: Cell::new(0),
            rejected_wrong_thread: Cell::new(0),
            shutdowns: Cell::new(0),
        }
    }

    fn bump(field: &Cell<u64>) {
        field.set(field.get().saturating_add(1));
    }

    fn snapshot(&self) -> CallbackCounters {
        CallbackCounters {
            invocations_accepted: self.invocations_accepted.get(),
            rejected_closed: self.rejected_closed.get(),
            rejected_reentrant: self.rejected_reentrant.get(),
            rejected_wrong_thread: self.rejected_wrong_thread.get(),
            shutdowns: self.shutdowns.get(),
        }
    }
}

/// Owned callback state for one registered class. `O` is the state the
/// native callback is allowed to touch; it lives here, never on a native
/// stack frame. Clones share the same state, the same reentrancy guard, and
/// the same aggregate counters.
///
/// The cell is thread-affine: every operation requires the owning
/// [`MainThreadToken`], matching the rest of the bridge.
#[derive(Debug)]
pub struct CallbackCell<O> {
    owner: MainThreadToken,
    class_name: RegisteredClassName,
    state: Rc<RefCell<Option<O>>>,
    active: Rc<Cell<bool>>,
    counters: Rc<CallbackCountersCell>,
}

impl<O> Clone for CallbackCell<O> {
    fn clone(&self) -> Self {
        Self {
            owner: self.owner,
            class_name: self.class_name.clone(),
            state: Rc::clone(&self.state),
            active: Rc::clone(&self.active),
            counters: Rc::clone(&self.counters),
        }
    }
}

impl<O> CallbackCell<O> {
    /// Register a callback cell with owned initial state.
    pub fn register(owner: MainThreadToken, class_name: RegisteredClassName, state: O) -> Self {
        Self {
            owner,
            class_name,
            state: Rc::new(RefCell::new(Some(state))),
            active: Rc::new(Cell::new(false)),
            counters: Rc::new(CallbackCountersCell::new()),
        }
    }

    /// The registered class name.
    pub fn class_name(&self) -> &RegisteredClassName {
        &self.class_name
    }

    /// Whether the cell still holds its state (not shut down).
    pub fn is_live(&self) -> bool {
        self.state.borrow().is_some()
    }

    /// Current aggregate counters. Monotonic; shared across clones.
    pub fn counters(&self) -> CallbackCounters {
        self.counters.snapshot()
    }

    /// Invoke the callback body with exclusive access to the owned state.
    ///
    /// Rejects, in order: wrong owning thread, reentrant invocation
    /// (including through a clone), and late invocation after shutdown. The
    /// reentrancy guard releases on unwind, so a panicking body cannot wedge
    /// the cell. Every attempt is counted.
    pub fn invoke<R>(
        &self,
        token: MainThreadToken,
        body: impl FnOnce(&mut O) -> R,
    ) -> Result<R, CallbackError> {
        if token.thread != self.owner.thread {
            CallbackCountersCell::bump(&self.counters.rejected_wrong_thread);
            return Err(CallbackError::WrongThread);
        }
        if self.active.get() {
            CallbackCountersCell::bump(&self.counters.rejected_reentrant);
            return Err(CallbackError::Reentrant);
        }
        let guard = ReentrancyGuard::acquire(&self.active);
        let mut slot = self.state.borrow_mut();
        let state = match slot.as_mut() {
            Some(state) => state,
            None => {
                drop(slot);
                drop(guard);
                CallbackCountersCell::bump(&self.counters.rejected_closed);
                return Err(CallbackError::Closed);
            }
        };
        let result = body(state);
        drop(slot);
        guard.release();
        CallbackCountersCell::bump(&self.counters.invocations_accepted);
        Ok(result)
    }

    /// Retire the callback state. Late invocations after this point fail
    /// with [`CallbackError::Closed`]; the state is dropped here, so any
    /// cleanup it owns (native wrappers, counters) happens at shutdown, not
    /// at some later callback firing. A second shutdown is itself a closed
    /// error. Other instances' cells are untouched.
    pub fn shutdown(&mut self, token: MainThreadToken) -> Result<O, CallbackError> {
        if token.thread != self.owner.thread {
            CallbackCountersCell::bump(&self.counters.rejected_wrong_thread);
            return Err(CallbackError::WrongThread);
        }
        let retired = self.state.borrow_mut().take();
        match retired {
            Some(state) => {
                CallbackCountersCell::bump(&self.counters.shutdowns);
                Ok(state)
            }
            None => Err(CallbackError::Closed),
        }
    }
}

/// RAII reentrancy guard. Acquisition happens at the call site (the cell
/// checks `active` first); `Drop` always releases, including on unwind.
struct ReentrancyGuard<'a> {
    flag: &'a Cell<bool>,
}

impl<'a> ReentrancyGuard<'a> {
    fn acquire(flag: &'a Cell<bool>) -> Self {
        flag.set(true);
        Self { flag }
    }

    fn release(self) {
        self.flag.set(false);
    }
}

impl Drop for ReentrancyGuard<'_> {
    fn drop(&mut self) {
        self.flag.set(false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    fn main_token() -> MainThreadToken {
        MainThreadToken {
            thread: std::thread::current().id(),
        }
    }

    fn affine_token() -> MainThreadToken {
        // The kernel is thread-affine rather than literally main-thread in
        // unit tests (CI workers have no AppKit main thread); invoke checks
        // owner affinity the same way the lib.rs tests do.
        main_token()
    }

    #[test]
    fn generated_class_names_are_unique_and_consumer_scoped() {
        let instance = InstanceId::next();
        let first = generate_class_name("fcb", instance).unwrap();
        let second = generate_class_name("fcb", instance).unwrap();
        let other = generate_class_name("fmd-font", instance).unwrap();
        assert_ne!(first, second);
        assert!(first.starts_with("fcb_"));
        assert!(other.starts_with("fmd-font_"));
    }

    #[test]
    fn consumer_names_are_validated() {
        let instance = InstanceId::next();
        assert_eq!(
            generate_class_name("", instance),
            Err(CallbackError::InvalidConsumerName)
        );
        assert_eq!(
            generate_class_name("has space", instance),
            Err(CallbackError::InvalidConsumerName)
        );
        let long = "x".repeat(MAX_CONSUMER_BYTES + 1);
        assert_eq!(
            generate_class_name(&long, instance),
            Err(CallbackError::InvalidConsumerName)
        );
        assert!(generate_class_name("fmd-font_macOS-2", instance).is_ok());
    }

    #[test]
    fn issued_names_are_tombstoned_for_the_process_lifetime() {
        let name = generate_class_name("tombstone", InstanceId::next()).unwrap();
        claim_class_name(name.clone()).unwrap();
        assert_eq!(
            claim_class_name(name.clone()),
            Err(CallbackError::ClassNameCollision)
        );
        assert!(class_name_is_issued(&name));
    }

    #[test]
    fn registered_class_names_carry_their_instance() {
        let instance = InstanceId::next();
        let registered = RegisteredClassName::issue("fcb", instance).unwrap();
        assert_eq!(registered.instance(), instance);
        assert!(registered.as_str().starts_with("fcb_"));
    }

    #[test]
    fn invoke_grants_exclusive_access_to_owned_state() {
        let cell = CallbackCell::register(
            main_token(),
            RegisteredClassName::issue("fcb", InstanceId::next()).unwrap(),
            Vec::<String>::new(),
        );
        let pushed: Result<(), CallbackError> = cell.invoke(affine_token(), |state| {
            state.push("first".to_string());
            state.push("second".to_string());
        });
        pushed.unwrap();
        let seen: Result<usize, CallbackError> = cell.invoke(affine_token(), |state| state.len());
        assert_eq!(seen.unwrap(), 2);
        let counters = cell.counters();
        assert_eq!(counters.invocations_accepted, 2);
        assert_eq!(counters.rejected_closed, 0);
    }

    #[test]
    fn reentrant_invocation_is_rejected_and_recovers() {
        let cell = CallbackCell::register(
            main_token(),
            RegisteredClassName::issue("fcb", InstanceId::next()).unwrap(),
            0_u64,
        );
        let reentrant: Result<(), CallbackError> = cell.invoke(affine_token(), |_state| {
            let nested: Result<(), CallbackError> = cell.invoke(affine_token(), |state| {
                *state += 1;
            });
            assert_eq!(nested, Err(CallbackError::Reentrant));
        });
        reentrant.unwrap();
        // The guard released once the outer body returned.
        let after: Result<(), CallbackError> = cell.invoke(affine_token(), |state| {
            *state += 1;
        });
        after.unwrap();
        let counters = cell.counters();
        assert_eq!(counters.rejected_reentrant, 1);
        assert_eq!(counters.invocations_accepted, 2);
    }

    #[test]
    fn reentrancy_guard_releases_on_unwind() {
        let cell = CallbackCell::register(
            main_token(),
            RegisteredClassName::issue("fcb", InstanceId::next()).unwrap(),
            0_u64,
        );
        let panicked: std::thread::Result<()> =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                let _ = cell.invoke(affine_token(), |_state| -> () {
                    panic!("callback body panics");
                });
            }));
        assert!(panicked.is_err());
        // The guard released during unwind; the cell is usable again.
        let recovered: Result<(), CallbackError> = cell.invoke(affine_token(), |state| {
            *state += 1;
        });
        recovered.unwrap();
    }

    #[test]
    fn cloned_cells_share_state_and_reentrancy_guard() {
        let cell = CallbackCell::register(
            main_token(),
            RegisteredClassName::issue("fcb", InstanceId::next()).unwrap(),
            0_u64,
        );
        let mut clone = cell.clone();
        let outer: Result<(), CallbackError> = cell.invoke(affine_token(), |_state| {
            let nested: Result<(), CallbackError> = clone.invoke(affine_token(), |state| {
                *state += 1;
            });
            assert_eq!(nested, Err(CallbackError::Reentrant));
        });
        outer.unwrap();
        let shutdown = clone.shutdown(main_token());
        assert!(matches!(shutdown, Ok(0_u64)));
        // Shutdown through the clone retires the shared state for both.
        assert!(!cell.is_live());
        assert_eq!(
            cell.invoke::<()>(affine_token(), |_| {}),
            Err(CallbackError::Closed)
        );
    }

    #[test]
    fn late_callbacks_after_shutdown_are_rejected_not_served() {
        let dropped_count = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        struct DropObserver(Arc<std::sync::atomic::AtomicUsize>);
        impl Drop for DropObserver {
            fn drop(&mut self) {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            }
        }

        let mut cell = CallbackCell::register(
            main_token(),
            RegisteredClassName::issue("fcb", InstanceId::next()).unwrap(),
            DropObserver(Arc::clone(&dropped_count)),
        );
        assert!(cell.is_live());
        let retired = cell.shutdown(main_token()).unwrap();
        drop(retired);
        assert_eq!(dropped_count.load(std::sync::atomic::Ordering::Relaxed), 1);
        assert!(!cell.is_live());
        assert_eq!(
            cell.invoke::<()>(main_token(), |_| {}),
            Err(CallbackError::Closed)
        );
        // A second shutdown is also a closed error, not a double drop.
        assert!(matches!(
            cell.shutdown(main_token()),
            Err(CallbackError::Closed)
        ));
        assert_eq!(dropped_count.load(std::sync::atomic::Ordering::Relaxed), 1);
        let counters = cell.counters();
        assert_eq!(counters.rejected_closed, 1);
        assert_eq!(counters.shutdowns, 1);
    }

    #[test]
    fn shutting_down_one_instance_never_touches_another() {
        let instance_a = InstanceId::next();
        let instance_b = InstanceId::next();
        let mut cell_a = CallbackCell::register(
            main_token(),
            RegisteredClassName::issue("fcb", instance_a).unwrap(),
            'a',
        );
        let cell_b = CallbackCell::register(
            main_token(),
            RegisteredClassName::issue("fmd-font", instance_b).unwrap(),
            'b',
        );
        cell_a.shutdown(main_token()).unwrap();
        // Instance B is fully live after instance A's shutdown.
        let letter: Result<char, CallbackError> = cell_b.invoke(main_token(), |state| *state);
        assert_eq!(letter.unwrap(), 'b');
        // Instance A's name stays tombstoned: no new registration can
        // quietly reuse it.
        assert!(class_name_is_issued(cell_a.class_name().as_str()));
        assert!(class_name_is_issued(cell_b.class_name().as_str()));
        assert_ne!(cell_a.class_name(), cell_b.class_name());
    }

    #[test]
    fn wrong_affinity_invocation_is_rejected_and_counted() {
        let owner = main_token();
        let mut cell = CallbackCell::register(
            owner,
            RegisteredClassName::issue("fcb", InstanceId::next()).unwrap(),
            0_u64,
        );
        let foreign = std::thread::spawn(|| std::thread::current().id())
            .join()
            .unwrap();
        let foreign_token = MainThreadToken { thread: foreign };
        assert_eq!(
            cell.invoke::<()>(foreign_token, |_| {}),
            Err(CallbackError::WrongThread)
        );
        assert_eq!(
            cell.shutdown(foreign_token),
            Err(CallbackError::WrongThread)
        );
        // The cell is intact after both rejected operations, and both
        // rejections are counted.
        assert!(cell.is_live());
        let counters = cell.counters();
        assert_eq!(counters.rejected_wrong_thread, 2);
    }
}
