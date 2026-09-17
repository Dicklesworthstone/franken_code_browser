#![forbid(unsafe_code)]

//! Explicit catalog publication and cold reopening of saved observations.
//!
//! First open with `PagedSnapshot::open`, then encode its directory with
//! `encode_catalog`. Retain the returned full-artifact digest in trusted host
//! state, independently of catalog bytes. `PinnedCatalog::decode_pinned` validates
//! that pin and all metadata. `PagedSnapshot::open_pinned` attaches a host-admitted
//! reader with header/footer checks instead of hashing the whole source archive.
//!
//! The resulting PagedSnapshot supports the SAME PagedQuery, indexed query,
//! PagedCapture and reader APIs. Loaded members still undergo digest verification.
//! Consult `fully_verified_on_open` before claiming the entire archive was read.
//! A complete saved-scope search is not a current archive-health certificate.
//!
//! This module creates no runtime, performs no pathname lookup, restores no grant,
//! and does not automatically find or trust a catalog beside an archive. Source
//! filenames and digest-derived facts can be sensitive even without source bytes.

pub use fcb_store::paged_snapshot::catalog::{CatalogArtifact, CatalogError, PinnedCatalog,
    CATALOG_SCHEMA, MAX_CATALOG_BYTES};
