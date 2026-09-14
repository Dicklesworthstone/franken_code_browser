#![forbid(unsafe_code)]

//! Owned cache namespaces with pinned generations (FCB-081.A).
//!
//! A [`CacheNamespace`] is a private, root-confined directory holding
//! immutable entry bytes, addressed by generation. The namespace owns its
//! root exclusively: creation is exclusive, the root carries a marker file
//! binding it to one namespace identity, and every later validation refuses
//! a root whose marker is missing, foreign, or has been swapped for a
//! symlink.
//!
//! Structural guarantees of this child (FCB-081.A):
//!
//! - **No arbitrary deletion paths.** The public API exposes no operation
//!   that removes an arbitrary path. Entry removal, logical clear,
//!   revocation, and deferred reclamation belong to the adjacent child
//!   (FCB-081.B); this child only writes, reads, and pins generations. The
//!   only removals performed internally are this namespace's own pin
//!   markers under its own `pins/` directory.
//! - **No forensic-erasure promise.** Entries are plain files; nothing here
//!   claims secure erasure, and documentation never promises one.
//! - **Race-safe authority where claimed.** Roots are created exclusively
//!   and re-validated through `fs::symlink_metadata` (a symlink-swapped
//!   root is refused); entry and pin writes use exclusive creation
//!   (`create_new`), which refuses a symlink planted at the target path;
//!   entry reads traverse through `fcb_source`'s qualified confined reader
//!   under `SymlinkPolicy::DisallowAll`. These are the claims std supports;
//!   they are documented, not overstated.
//! - **Hot/cold equality.** Entries written in this session are retained
//!   in memory (hot); re-reading from disk (cold) must yield identical
//!   bytes, and the tests assert it.
//!
//! Reads and confinement are delegated to `fcb_source`'s qualified
//! confined-reader pipeline; this crate never re-implements traversal.

pub mod namespace;

pub use namespace::{
    CacheError, CacheNamespace, EntryName, EntryNameError, EntryWrite, Generation,
    GenerationError, IdentityError, NamespaceCounters, NamespaceIdentity, MARKER_NAME,
    MAX_ENTRY_BYTES, MAX_ENTRY_NAME, MAX_GENERATIONS, PINS_DIR,
};
