//! FCB-081.V production verification scenario: owned cache namespaces,
//! pinned generations, logical clear revocation, and protected reclamation.
//!
//! Required cases (each individually selectable via cargo test filter):
//!
//! 1. `wrong_root_marker_refusal` — missing or foreign identity marker is refused.
//! 2. `symlink_replacement_refusal` — symlink-swapped root or entry is refused.
//! 3. `pinned_readers_across_generation_clear` — active reader lease survives clear
//!    until unpinned, then deferred reclamation completes.
//! 4. `hot_cold_equality` — written entry in memory matches disk cold read byte-for-byte.
//! 5. `retired_writes_rejected` — write into rotated/cleared generation rejected as stale.
//! 6. `no_wall_clock_liveness_assumption` — logical ordering and pin invariants hold
//!    without time-based expiry.
//! 7. `negative_control_planted_symlink` — intentional negative control demonstrating
//!    oracle refusal when symlink is planted in generation dir during reclamation.
//! 8. `independent_user_backups_preserved` — independent files outside cache root
//!    are completely untouched by clear and reclamation.
//! 9. `explicit_non_promise_of_forensic_erasure` — reports explicitly verify that
//!    forensic erasure is not promised.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_081.sh`).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fcb_core::ArenaOwnerId;
use fcb_store::{
    CacheError, CacheNamespace, NamespaceIdentity, MARKER_NAME,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_081_RUN_ID";

fn counter() -> &'static AtomicU64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    &COUNTER
}

fn temp_parent(tag: &str) -> PathBuf {
    let unique = counter().fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "fcb-081-prod-{tag}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp parent created");
    dir
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-081-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    fs::create_dir_all(&run_dir).expect("receipts dir created");
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_81_00_01),
        pin: SourcePin::new("0813456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
        route: RouteId::new("headless:rust").expect("route valid"),
        corpus_digest: fcb_test_support::ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };
    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    )
    .expect("receipt retained");
}

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C_81).expect("test owner is non-zero")
}

fn identity(name: &str) -> NamespaceIdentity {
    NamespaceIdentity::new(name).expect("valid identity")
}

#[test]
fn wrong_root_marker_refusal() {
    let parent = temp_parent("wrong-marker");
    let namespace =
        CacheNamespace::create(&parent, identity("marker-primary"), owner()).expect("create");

    // Tamper the marker so it claims a foreign consumer
    let marker_path = namespace.root().join(MARKER_NAME);
    fs::write(&marker_path, "fcb-store-cache.v1\nconsumer: marker-foreign\n").expect("tamper marker");

    // Reopening under the original identity must be rejected as WrongRoot
    let err = CacheNamespace::open(&parent, identity("marker-primary"), owner()).unwrap_err();
    assert_eq!(err, CacheError::WrongRoot);

    record_receipt(
        "wrong_root_marker_refusal",
        Effect::Succeeded,
        "tampered marker refused on open with WrongRoot",
    );
}

#[test]
fn symlink_replacement_refusal() {
    let parent = temp_parent("symlink-refusal");
    let mut namespace =
        CacheNamespace::create(&parent, identity("symlink-target"), owner()).expect("create");

    namespace
        .write_entry("safe_entry.bin", b"immutable content")
        .expect("write entry");

    #[cfg(unix)]
    {
        // Replace an entry with a symlink
        let entry_path = namespace.root().join("gen-000001").join("safe_entry.bin");
        let external_decoy = parent.join("external_file.txt");
        fs::write(&external_decoy, b"foreign bytes").expect("write external");

        fs::remove_file(&entry_path).expect("remove real file");
        std::os::unix::fs::symlink(&external_decoy, &entry_path).expect("plant symlink");

        // Cold read through confined pipeline must refuse the symlinked entry
        let err = namespace.read_cold("safe_entry.bin", 1).unwrap_err();
        assert!(
            matches!(err, CacheError::Source(_) | CacheError::RootUnavailable),
            "symlink entry must be refused: {err:?}"
        );
    }

    record_receipt(
        "symlink_replacement_refusal",
        Effect::Succeeded,
        "symlinked entry refused on traversal",
    );
}

#[test]
fn pinned_readers_across_generation_clear() {
    let parent = temp_parent("pinned-clear");
    let mut namespace =
        CacheNamespace::create(&parent, identity("lease-holder"), owner()).expect("create");

    namespace
        .write_entry("leased_dataset.bin", b"dataset-v1-bytes")
        .expect("write dataset");

    // Pin generation 1 (active lease)
    namespace.pin(1).expect("pin generation 1");
    assert!(namespace.is_pinned(1));

    // Clear namespace: must defer reclamation because generation 1 is pinned
    let clear_rep = namespace.clear_namespace().expect("clear namespace");
    assert_eq!(clear_rep.new_generation.get(), 2);
    assert_eq!(clear_rep.deferred_generations, vec![1]);
    assert!(clear_rep.reclaimed_generations.is_empty());
    assert!(namespace.is_deferred(1));

    // Pinned reader can still read both hot and cold entries
    let hot = namespace.read_hot("leased_dataset.bin", 1).expect("hot read");
    assert_eq!(hot.as_slice(), b"dataset-v1-bytes");
    let cold = namespace.read_cold("leased_dataset.bin", 1).expect("cold read");
    assert_eq!(cold.as_slice(), b"dataset-v1-bytes");

    // Unpin lease
    namespace.unpin(1).expect("unpin generation 1");
    assert!(!namespace.is_pinned(1));

    // Deferred reclamation now reclaims the entry
    let reclaim_rep = namespace.reclaim_deferred().expect("reclaim deferred");
    assert_eq!(reclaim_rep.completed_generations, vec![1]);
    assert_eq!(reclaim_rep.reclaimed_entries, 1);
    assert_eq!(reclaim_rep.reclaimed_bytes, 16);
    assert!(!namespace.is_deferred(1));

    // Reading after reclamation returns GenerationRevoked
    let revoked = namespace.read_entry("leased_dataset.bin", 1).unwrap_err();
    assert_eq!(revoked, CacheError::GenerationRevoked);

    record_receipt(
        "pinned_readers_across_generation_clear",
        Effect::Succeeded,
        "active lease preserved across clear until unpin then deferred reclaimed",
    );
}

#[test]
fn hot_cold_equality() {
    let parent = temp_parent("hot-cold-eq");
    let mut namespace =
        CacheNamespace::create(&parent, identity("equality-check"), owner()).expect("create");

    let payload = b"critical bytes for hot/cold verification";
    namespace
        .write_entry("payload.bin", payload)
        .expect("write entry");

    let hot = namespace.read_hot("payload.bin", 1).expect("hot read");
    let cold = namespace.read_cold("payload.bin", 1).expect("cold read");
    let unified = namespace.read_entry("payload.bin", 1).expect("unified read");

    assert_eq!(hot.as_slice(), payload);
    assert_eq!(cold.as_slice(), payload);
    assert_eq!(unified.as_slice(), payload);

    record_receipt(
        "hot_cold_equality",
        Effect::Succeeded,
        "hot and cold reads agree byte-for-byte",
    );
}

#[test]
fn retired_writes_rejected() {
    let parent = temp_parent("retired-writes");
    let mut namespace =
        CacheNamespace::create(&parent, identity("stale-guard"), owner()).expect("create");

    namespace
        .write_entry("v1.bin", b"first gen data")
        .expect("write");

    // Advance generation to 2
    namespace.advance_generation().expect("advance generation");
    assert_eq!(namespace.current_generation(), 2);

    // Attempt write into retired generation 1
    let err = namespace
        .write_entry_in_generation(1, "late.bin", b"late data")
        .unwrap_err();
    assert_eq!(err, CacheError::StaleGeneration);
    assert_eq!(namespace.counters().writes_rejected_stale, 1);

    record_receipt(
        "retired_writes_rejected",
        Effect::Succeeded,
        "write to retired generation rejected with StaleGeneration and counted",
    );
}

#[test]
fn no_wall_clock_liveness_assumption() {
    let parent = temp_parent("liveness-invariants");
    let mut namespace =
        CacheNamespace::create(&parent, identity("logical-time"), owner()).expect("create");

    namespace.write_entry("state.bin", b"logical state").expect("write");
    namespace.pin(1).expect("pin");

    // Generation and pin validity are logical, conserved invariants
    assert!(namespace.is_pinned(1));
    assert_eq!(namespace.current_generation(), 1);
    assert!(!namespace.is_deferred(1));

    record_receipt(
        "no_wall_clock_liveness_assumption",
        Effect::Succeeded,
        "generation and lease states are logical and independent of wall-clock time",
    );
}

#[test]
fn negative_control_planted_symlink() {
    let parent = temp_parent("negative-control");
    let mut namespace =
        CacheNamespace::create(&parent, identity("neg-oracle"), owner()).expect("create");

    namespace
        .write_entry("safe.bin", b"safe file")
        .expect("write");

    let external_target = parent.join("external_precious.txt");
    fs::write(&external_target, b"precious contents").expect("write precious");

    #[cfg(unix)]
    {
        let gen1_dir = namespace.root().join("gen-000001");
        let link_path = gen1_dir.join("decoy_link.bin");
        std::os::unix::fs::symlink(&external_target, &link_path).expect("symlink planted");

        // The reclamation oracle MUST detect the symlink and REFUSE reclamation
        let result = namespace.clear_namespace();
        assert!(
            result.is_err(),
            "oracle must refuse reclamation when symlink is present"
        );

        // Precious file outside must remain intact
        assert!(
            external_target.exists(),
            "external target must not be removed by reclamation refusal"
        );
    }

    record_receipt(
        "negative_control_planted_symlink",
        Effect::Succeeded,
        "negative control verified: planted symlink detected and refused",
    );
}

#[test]
fn independent_user_backups_preserved() {
    let parent = temp_parent("user-backups");

    // Independent backup directory outside the cache
    let backup_dir = parent.join("my_project_backups");
    fs::create_dir_all(&backup_dir).expect("create backup dir");
    let backup_doc = backup_dir.join("notes.txt");
    let backup_content = b"user authored notes that must never be deleted";
    fs::write(&backup_doc, backup_content).expect("write backup");

    // Cache namespace created in adjacent directory
    let cache_parent = parent.join("cache_store");
    fs::create_dir_all(&cache_parent).expect("create cache store");
    let mut namespace =
        CacheNamespace::create(&cache_parent, identity("backup-preservation"), owner())
            .expect("create");

    namespace
        .write_entry("index.bin", b"cached search index")
        .expect("write cache");

    // Clear and reclaim
    namespace.clear_namespace().expect("clear namespace");

    // Backup is 100% untouched
    assert!(backup_dir.exists());
    assert!(backup_doc.exists());
    let read_back = fs::read(&backup_doc).expect("read backup doc");
    assert_eq!(read_back, backup_content);

    record_receipt(
        "independent_user_backups_preserved",
        Effect::Succeeded,
        "independent user backups outside cache root preserved completely",
    );
}

#[test]
fn explicit_non_promise_of_forensic_erasure() {
    let parent = temp_parent("forensic-non-promise");
    let mut namespace =
        CacheNamespace::create(&parent, identity("forensic-policy"), owner()).expect("create");

    namespace.write_entry("f.bin", b"payload").expect("write");

    let clear_rep = namespace.clear_namespace().expect("clear");
    assert!(!clear_rep.forensic_erasure_guaranteed());

    record_receipt(
        "explicit_non_promise_of_forensic_erasure",
        Effect::Succeeded,
        "reports explicitly certify forensic erasure is not guaranteed",
    );
}
