//! Tests for owned cache namespaces and pinned generations (FCB-081.A).
//!
//! Package oracle coverage: wrong-root markers, symlink replacement of the
//! root and of entries, pinned readers across generation advance, hot/cold
//! equality, exclusive creation, and the absence of any arbitrary deletion
//! path.

use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fcb_store::{
    CacheError, CacheNamespace, EntryName, GenerationError, IdentityError, MARKER_NAME,
    NamespaceIdentity,
    MAX_ENTRY_BYTES,
};

fn counter() -> &'static AtomicU64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    &COUNTER
}

/// One owned temp parent per test. Cleanup covers only the directory this
/// test created (the approved own-fixture policy); nothing peer-owned is
/// ever touched.
fn temp_parent(tag: &str) -> PathBuf {
    let unique = counter().fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "fcb-store-test-{tag}-{}-{unique}",
        std::process::id()
    ));
    std::fs::create_dir_all(&dir).expect("temp parent created");
    dir
}

fn identity(consumer: &str) -> NamespaceIdentity {
    NamespaceIdentity::new(consumer).expect("valid identity")
}

#[test]
fn create_write_and_hot_cold_reads_are_equal() {
    let parent = temp_parent("hot-cold");
    let mut namespace =
        CacheNamespace::create(&parent, identity("fcb-store-tests"), owner()).expect("create");

    let written = namespace
        .write_entry("app-main.rs", b"fn main() {}")
        .expect("write succeeds");
    assert_eq!(written.generation, 1);
    assert_eq!(written.len, 12);

    // Hot read: retained from this session.
    let hot = namespace.read_hot("app-main.rs", 1).expect("hot copy");
    assert_eq!(hot.as_slice(), b"fn main() {}");

    // Cold read: through the confined pipeline from disk.
    let cold = namespace
        .read_cold("app-main.rs", 1)
        .expect("cold read succeeds");
    assert_eq!(cold, b"fn main() {}");

    // The unified read prefers hot but must agree with cold exactly.
    assert_eq!(
        namespace.read_entry("app-main.rs", 1).expect("read"),
        b"fn main() {}".to_vec()
    );
    assert_eq!(namespace.current_generation(), 1);
}

#[test]
fn reopen_preserves_identity_entries_and_generation() {
    let parent = temp_parent("reopen");
    let mut namespace =
        CacheNamespace::create(&parent, identity("reopening-consumer"), owner()).expect("create");
    namespace
        .write_entry("kept.bin", b"persistent bytes")
        .expect("write");
    namespace.advance_generation().expect("advance");

    // Drop and reopen with the same identity: the marker must match and
    // the generation counter must resume at the highest existing one.
    drop(namespace);
    let reopened =
        CacheNamespace::open(&parent, identity("reopening-consumer"), owner()).expect("reopen");
    assert_eq!(reopened.current_generation(), 2);
    assert_eq!(
        reopened.read_entry("kept.bin", 1).expect("old entry cold"),
        b"persistent bytes".to_vec()
    );
}

#[test]
fn wrong_identity_marker_is_refused() {
    let parent = temp_parent("wrong-marker");
    let namespace =
        CacheNamespace::create(&parent, identity("namespace-owner-a"), owner()).expect("create");

    // Tamper the marker so the root claims a different consumer. Reopening
    // under identity A must refuse: the marker no longer matches.
    let marker = namespace.root().join(MARKER_NAME);
    std::fs::write(&marker, "fcb-store-cache.v1\nconsumer: namespace-owner-b\n")
        .expect("tamper written");

    let error = CacheNamespace::open(&parent, identity("namespace-owner-a"), owner()).unwrap_err();
    assert_eq!(error, CacheError::WrongRoot);
}

#[test]
fn symlink_swapped_root_is_refused_on_open() {
    let parent = temp_parent("symlink-root");
    let namespace =
        CacheNamespace::create(&parent, identity("swap-target"), owner()).expect("create");
    let root_path = namespace.root().to_path_buf();
    drop(namespace);

    // Swap the real root for a symlink pointing at an innocent directory.
    let decoy = parent.join("decoy-other-namespace");
    std::fs::create_dir_all(&decoy).expect("decoy created");
    std::fs::remove_dir_all(&root_path).expect("test removes its own fixture root");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&decoy, &root_path).expect("symlink swap");
    #[cfg(not(unix))]
    std::fs::rename(&decoy, &root_path).expect("swap");

    let error = CacheNamespace::open(&parent, identity("swap-target"), owner()).unwrap_err();
    assert_eq!(error, CacheError::RootUnavailable);
}

#[test]
fn symlink_swapped_entry_is_refused_on_cold_read() {
    let parent = temp_parent("symlink-entry");
    let mut namespace =
        CacheNamespace::create(&parent, identity("entry-swap"), owner()).expect("create");
    namespace
        .write_entry("victim.bin", b"original bytes")
        .expect("write");

    // Replace the entry file with a symlink to attacker-controlled bytes.
    let entry_path = namespace
        .root()
        .join("gen-000001")
        .join("victim.bin");
    std::fs::remove_file(&entry_path).expect("test removes its own fixture entry");
    let foreign = parent.join("foreign-payload.bin");
    std::fs::write(&foreign, b"attacker bytes").expect("foreign payload written");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&foreign, &entry_path).expect("symlink swap");

    let error = namespace.read_cold("victim.bin", 1).unwrap_err();
    assert_eq!(
        error,
        CacheError::Source(fcb_source::SourceError::SymlinkForbidden)
    );
    assert_eq!(
        namespace.read_entry("victim.bin", 1).expect("hot read unaffected"),
        b"original bytes".to_vec()
    );
}

#[test]
fn generation_lifecycle_advance_pin_unpin() {
    let parent = temp_parent("generations");
    let mut namespace =
        CacheNamespace::create(&parent, identity("generations"), owner()).expect("create");
    namespace
        .write_entry("gen1.txt", b"first generation")
        .expect("write in generation one");

    let generation_two = namespace.advance_generation().expect("advance");
    assert_eq!(generation_two.get(), 2);
    assert!(!namespace.is_pinned(1));

    namespace.pin(1).expect("pin generation one");
    assert!(namespace.is_pinned(1));
    assert_eq!(
        namespace.pin(1).unwrap_err(),
        CacheError::AlreadyPinned,
        "pinning is exclusive per generation"
    );

    // A pinned reader can still read the pinned generation explicitly.
    assert_eq!(
        namespace.read_entry("gen1.txt", 1).expect("pinned reader"),
        b"first generation".to_vec()
    );

    namespace.unpin(1).expect("unpin");
    assert_eq!(namespace.unpin(1).unwrap_err(), CacheError::NotPinned);

    // Generation zero and beyond-bound generations are refused.
    assert_eq!(
        fcb_store::Generation::new(0).unwrap_err(),
        GenerationError::Zero
    );
    assert_eq!(
        fcb_store::Generation::new(u64::from(fcb_store::MAX_GENERATIONS) + 1).unwrap_err(),
        GenerationError::Exhausted
    );
}

#[test]
fn entry_names_reject_traversal_and_reserved_forms() {
    assert!(EntryName::new("").is_err());
    assert!(EntryName::new(".").is_err());
    assert!(EntryName::new("..").is_err());
    assert!(EntryName::new("nested/name").is_err());
    assert!(EntryName::new("nested\\name").is_err());
    assert!(EntryName::new("space name").is_err());
    assert!(EntryName::new(&"x".repeat(129)).is_err());
    assert!(EntryName::new("valid-entry_1.bin").is_ok());
}

#[test]
fn oversized_entries_are_refused_and_counted() {
    let parent = temp_parent("oversized");
    let mut namespace =
        CacheNamespace::create(&parent, identity("sizing"), owner()).expect("create");
    let oversized = vec![b'x'; usize::try_from(MAX_ENTRY_BYTES).unwrap() + 1];
    let error = namespace.write_entry("big.bin", &oversized).unwrap_err();
    assert_eq!(error, CacheError::EntryTooLarge);
    assert_eq!(namespace.counters().writes_rejected_size, 1);
    assert_eq!(namespace.counters().entries_written, 0);
}

#[test]
fn duplicate_writes_in_one_generation_are_refused() {
    let parent = temp_parent("duplicate");
    let mut namespace =
        CacheNamespace::create(&parent, identity("dupes"), owner()).expect("create");
    namespace
        .write_entry("same.bin", b"first")
        .expect("first write wins");
    let error = namespace.write_entry("same.bin", b"second").unwrap_err();
    assert_eq!(error, CacheError::EntryExists);

    // The first bytes survive the refused overwrite: entries are immutable.
    assert_eq!(
        namespace.read_entry("same.bin", 1).expect("read"),
        b"first".to_vec()
    );
}

#[test]
fn double_create_reports_already_exists() {
    let parent = temp_parent("exclusive");
    CacheNamespace::create(&parent, identity("exclusive-root"), owner()).expect("first create");
    let error =
        CacheNamespace::create(&parent, identity("exclusive-root"), owner()).unwrap_err();
    assert_eq!(error, CacheError::AlreadyExists);
}

#[test]
fn invalid_identity_is_refused() {
    assert_eq!(
        NamespaceIdentity::new("").unwrap_err(),
        IdentityError::InvalidConsumer
    );
    assert_eq!(
        NamespaceIdentity::new("control\u{7f}char").unwrap_err(),
        IdentityError::InvalidConsumer
    );
}

#[test]
fn no_reclamation_or_arbitrary_deletion_happens_in_this_child() {
    // Structural guard: after unpinning, entries remain on disk and
    // readable — this child performs no reclamation, so there is nothing
    // through which a caller could delete an arbitrary path.
    let parent = temp_parent("no-deletion");
    let mut namespace =
        CacheNamespace::create(&parent, identity("no-deletion"), owner()).expect("create");
    namespace
        .write_entry("pinned-then-unpinned.bin", b"still here")
        .expect("write");
    namespace.pin(1).expect("pin");
    namespace.unpin(1).expect("unpin");

    assert_eq!(
        namespace
            .read_cold("pinned-then-unpinned.bin", 1)
            .expect("cold read after unpin"),
        b"still here".to_vec()
    );
}

#[test]
fn counters_reconcile_attempts_against_outcomes() {
    let parent = temp_parent("counters");
    let mut namespace =
        CacheNamespace::create(&parent, identity("counters"), owner()).expect("create");
    namespace.write_entry("counted.bin", b"counted").expect("write");
    let _ = namespace.write_entry("../traversal.bin", b"nope");
    let _ = namespace.write_entry("too-big.bin", &vec![0u8; usize::try_from(MAX_ENTRY_BYTES).unwrap() + 1]);
    namespace.advance_generation().expect("advance");
    namespace.pin(2).expect("pin");
    namespace.unpin(2).expect("unpin");

    let counters = namespace.counters();
    assert_eq!(counters.entries_written, 1);
    assert_eq!(counters.writes_rejected_name, 1);
    assert_eq!(counters.writes_rejected_size, 1);
    assert_eq!(counters.generations_advanced, 1);
    assert_eq!(counters.pins, 1);
    assert_eq!(counters.unpins, 1);
}

fn owner() -> fcb_core::ArenaOwnerId {
    fcb_core::ArenaOwnerId::new(8_141).expect("test owner is non-zero")
}

