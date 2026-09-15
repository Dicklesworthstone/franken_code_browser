//! Tests for logical clear revocation and deferred reclamation (FCB-081.B).
//!
//! Verifies:
//! - Generation rotation before writes to block delayed stale repopulation.
//! - Pinned generations deferred during clear until active leases are unpinned.
//! - Rejection of protected filesystem roots and ancestors.
//! - Preservation of independent user backups outside namespace root.
//! - Negative control: symlink planted in generation dir is refused during reclamation.
//! - Explicit non-promise of forensic erasure.
//! - Monotonic counter reconciliation for clears, reclamations, and stale rejections.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use fcb_store::{
    is_protected_path, CacheError, CacheNamespace, NamespaceIdentity,
};

fn counter() -> &'static AtomicU64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    &COUNTER
}

fn temp_parent(tag: &str) -> PathBuf {
    let unique = counter().fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "fcb-store-reclaim-test-{tag}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp parent created");
    dir
}

fn identity(consumer: &str) -> NamespaceIdentity {
    NamespaceIdentity::new(consumer).expect("valid identity")
}

fn owner() -> fcb_core::ArenaOwnerId {
    fcb_core::ArenaOwnerId::new(8_142).expect("test owner is non-zero")
}

#[test]
fn rotate_generation_before_writes_blocks_stale_repopulation() {
    let parent = temp_parent("stale-block");
    let mut namespace =
        CacheNamespace::create(&parent, identity("stale-defense"), owner()).expect("create");

    // Write entry in generation 1
    let written = namespace
        .write_entry("active-doc.bin", b"original generation 1 content")
        .expect("write gen 1");
    assert_eq!(written.generation, 1);

    // Perform logical clear: must rotate generation BEFORE any new writes
    let report = namespace.clear_namespace().expect("clear namespace succeeds");
    assert_eq!(report.new_generation.get(), 2);
    assert_eq!(report.reclaimed_generations, vec![1]);
    assert!(report.deferred_generations.is_empty());
    assert_eq!(report.reclaimed_entries, 1);
    assert_eq!(report.reclaimed_bytes, 29);
    assert!(!report.forensic_erasure_guaranteed());

    // Attempting delayed write back into old generation 1 MUST be rejected as StaleGeneration
    let stale_err = namespace
        .write_entry_in_generation(1, "late-arrival.bin", b"delayed repopulation bytes")
        .unwrap_err();
    assert_eq!(stale_err, CacheError::StaleGeneration);

    // Counter must record the rejected stale write
    assert_eq!(namespace.counters().writes_rejected_stale, 1);

    // New write into current generation (2) succeeds cleanly
    let new_write = namespace
        .write_entry("fresh-doc.bin", b"generation 2 content")
        .expect("write in new gen");
    assert_eq!(new_write.generation, 2);

    // Reading cleared generation 1 returns GenerationRevoked
    let read_err = namespace.read_entry("active-doc.bin", 1).unwrap_err();
    assert_eq!(read_err, CacheError::GenerationRevoked);
}

#[test]
fn pinned_generation_deferred_until_unpinned() {
    let parent = temp_parent("deferred-pin");
    let mut namespace =
        CacheNamespace::create(&parent, identity("pin-retention"), owner()).expect("create");

    // Populate generation 1
    namespace
        .write_entry("critical-model.bin", b"active model weights")
        .expect("write");

    // Pin generation 1 (active lease)
    namespace.pin(1).expect("pin generation 1");
    assert!(namespace.is_pinned(1));

    // Clear namespace: generation 1 is pinned, so reclamation MUST be deferred
    let report = namespace.clear_namespace().expect("clear with pin");
    assert_eq!(report.new_generation.get(), 2);
    assert!(report.reclaimed_generations.is_empty());
    assert_eq!(report.deferred_generations, vec![1]);
    assert_eq!(namespace.counters().deferred_reclamations, 1);
    assert!(namespace.is_deferred(1));

    // Active lease continues to read both hot and cold entries successfully
    let hot_read = namespace
        .read_hot("critical-model.bin", 1)
        .expect("hot read remains valid while deferred");
    assert_eq!(hot_read.as_slice(), b"active model weights");

    let cold_read = namespace
        .read_cold("critical-model.bin", 1)
        .expect("cold read remains valid while deferred");
    assert_eq!(cold_read.as_slice(), b"active model weights");

    let unified_read = namespace
        .read_entry("critical-model.bin", 1)
        .expect("unified read succeeds");
    assert_eq!(unified_read.as_slice(), b"active model weights");

    // Calling reclaim_deferred while still pinned does NOT reclaim generation 1
    let early_reclaim = namespace.reclaim_deferred().expect("reclaim attempt");
    assert!(early_reclaim.completed_generations.is_empty());
    assert_eq!(early_reclaim.remaining_deferred, vec![1]);
    assert_eq!(early_reclaim.reclaimed_entries, 0);

    // Unpin generation 1 to release the lease
    namespace.unpin(1).expect("unpin generation 1");
    assert!(!namespace.is_pinned(1));

    // Now deferred reclamation succeeds and frees disk entries
    let deferred_report = namespace.reclaim_deferred().expect("reclaim after unpin");
    assert_eq!(deferred_report.completed_generations, vec![1]);
    assert!(deferred_report.remaining_deferred.is_empty());
    assert_eq!(deferred_report.reclaimed_entries, 1);
    assert_eq!(deferred_report.reclaimed_bytes, 20);
    assert!(!deferred_report.forensic_erasure_guaranteed());
    assert!(!namespace.is_deferred(1));

    // Subsequent read returns GenerationRevoked
    let revoked = namespace.read_entry("critical-model.bin", 1).unwrap_err();
    assert_eq!(revoked, CacheError::GenerationRevoked);
}

#[test]
fn protected_paths_and_ancestors_are_refused() {
    // Structural check: root "/" and standard OS ancestor roots are protected
    assert!(is_protected_path(Path::new("/")));
    assert!(is_protected_path(Path::new("/etc")));
    assert!(is_protected_path(Path::new("/var")));
    assert!(is_protected_path(Path::new("/usr")));
    assert!(is_protected_path(Path::new("/bin")));

    // Safe temporary directories are not protected
    let temp = temp_parent("not-protected");
    assert!(!is_protected_path(&temp));
}

#[test]
fn independent_user_backups_remain_undisturbed() {
    let parent = temp_parent("backup-safety");

    // Establish an independent user backup directory adjacent to the cache
    let backup_dir = parent.join("independent-user-backups");
    fs::create_dir_all(&backup_dir).expect("create backup dir");
    let important_file = backup_dir.join("user_notes.md");
    let backup_data = b"# User Research Notes\nAuthoritative, not rebuildable cache.";
    fs::write(&important_file, backup_data).expect("write user backup");

    // Establish the cache namespace in a sibling directory
    let cache_parent = parent.join("cache-storage");
    fs::create_dir_all(&cache_parent).expect("create cache storage dir");
    let mut namespace =
        CacheNamespace::create(&cache_parent, identity("backup-test"), owner()).expect("create");

    namespace
        .write_entry("transient-index.bin", b"rebuildable index data")
        .expect("write entry");

    // Clear and reclaim the cache
    let report = namespace.clear_namespace().expect("clear namespace");
    assert_eq!(report.reclaimed_entries, 1);

    // Verify user backup is 100% untouched and byte-identical
    assert!(backup_dir.exists(), "backup directory must exist");
    assert!(important_file.exists(), "backup file must exist");
    let retained_bytes = fs::read(&important_file).expect("read backup file");
    assert_eq!(retained_bytes, backup_data);
}

#[test]
fn negative_control_symlink_in_generation_dir_is_refused_during_reclamation() {
    let parent = temp_parent("symlink-negative-control");
    let mut namespace =
        CacheNamespace::create(&parent, identity("symlink-guard"), owner()).expect("create");

    // Write an entry in generation 1
    namespace
        .write_entry("real-entry.bin", b"real entry bytes")
        .expect("write");

    // Create an external decoy target outside the namespace
    let decoy_dir = parent.join("decoy-external-dir");
    fs::create_dir_all(&decoy_dir).expect("create decoy dir");
    let decoy_file = decoy_dir.join("must_not_be_deleted.txt");
    fs::write(&decoy_file, b"treasured external file").expect("write decoy");

    // Plant a symlink inside gen-000001 pointing to decoy_file
    #[cfg(unix)]
    {
        let gen1_dir = namespace.root().join("gen-000001");
        let symlink_path = gen1_dir.join("planted_link.bin");
        std::os::unix::fs::symlink(&decoy_file, &symlink_path).expect("plant symlink");

        // Attempting clear_namespace must encounter the symlink and REFUSE with RootUnavailable
        let result = namespace.clear_namespace();
        assert!(
            result.is_err(),
            "reclamation must refuse symlink planted inside generation directory"
        );

        // Decoy file outside the namespace must NOT be deleted
        assert!(
            decoy_file.exists(),
            "external decoy target must remain completely untouched"
        );
        assert_eq!(
            fs::read(&decoy_file).expect("read decoy"),
            b"treasured external file"
        );
    }
}

#[test]
fn explicit_non_promise_of_forensic_erasure() {
    // Both ClearReport and ReclamationReport explicitly certify that forensic erasure
    // is NOT guaranteed (plain file deletions on modern filesystems with wear-leveling / APFS)
    let parent = temp_parent("forensic-check");
    let mut namespace =
        CacheNamespace::create(&parent, identity("forensic-non-promise"), owner()).expect("create");
    namespace.write_entry("item.bin", b"bytes").expect("write");

    let clear_report = namespace.clear_namespace().expect("clear");
    assert!(!clear_report.forensic_erasure_guaranteed());

    namespace.write_entry("pinned.bin", b"bytes2").expect("write");
    let gen2 = namespace.current_generation();
    namespace.pin(gen2).expect("pin");
    namespace.clear_namespace().expect("clear with pin");
    namespace.unpin(gen2).expect("unpin");

    let reclaim_report = namespace.reclaim_deferred().expect("reclaim");
    assert!(!reclaim_report.forensic_erasure_guaranteed());
}

#[test]
fn counters_track_clears_reclamations_and_stale_rejections() {
    let parent = temp_parent("counter-reconcile");
    let mut namespace =
        CacheNamespace::create(&parent, identity("counters-reconcile"), owner()).expect("create");

    namespace.write_entry("doc1.bin", b"first doc").expect("write");
    namespace.write_entry("doc2.bin", b"second doc").expect("write");

    let initial_counters = namespace.counters();
    assert_eq!(initial_counters.entries_written, 2);
    assert_eq!(initial_counters.clears_completed, 0);
    assert_eq!(initial_counters.entries_reclaimed, 0);

    // Clear unpinned generation 1
    namespace.clear_namespace().expect("clear");

    let mid_counters = namespace.counters();
    assert_eq!(mid_counters.clears_completed, 1);
    assert_eq!(mid_counters.entries_reclaimed, 2);
    assert_eq!(mid_counters.bytes_reclaimed, 19);

    // Attempt stale write into generation 1
    let _ = namespace.write_entry_in_generation(1, "stale.bin", b"stale");
    assert_eq!(namespace.counters().writes_rejected_stale, 1);
}
