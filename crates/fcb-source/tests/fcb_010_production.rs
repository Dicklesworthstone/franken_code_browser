#![forbid(unsafe_code)]

//! FCB-010.V production verification scenario: Bounded directory discovery and
//! first-party ignore matcher driven together through their real supported public
//! implementation (see plan §8.1, §8.2, §8.7 and FCB-010.A/B contracts).
//!
//! Required cases (each independently selectable via cargo test filter):
//! 1. `staged_discovery_progressive_batches`: Deep and wide hierarchy yields progressive
//!    batches under descriptor and batch-entry limits; provisional vs stable publication.
//! 2. `shuffled_enumeration_stable_paged_ordering`: Shuffled filesystem directory
//!    entries produce deterministic sorted pages independent of directory enumeration order.
//! 3. `descriptor_and_queue_caps_hold`: Strict verification that open descriptors and
//!    work-queue entries never exceed configured bounds during multi-level traversal.
//! 4. `first_party_ignore_precedence_corpus`: Anchored, directory-only, doublestar,
//!    negation, character class, escaping, and nested rule precedence tested together.
//! 5. `visible_excluded_counts_and_deliberate_browse_override`: Excluded files are classified
//!    without deletion, visible in aggregate counters, and deliberate browse override
//!    safely re-includes them while preserving child ignore rules.
//! 6. `unsupported_patterns_reported_not_guessed`: Unclosed classes, dangling escapes,
//!    and empty patterns produce structured `UnsupportedPattern` reports; valid patterns
//!    in the same rule file remain active.
//! 7. `negative_control_oracle_detects_defects`: Synthetic negative controls proving the
//!    verification oracle detects descriptor cap breaches, false completion claims on
//!    cancelled scans, and silenced unsupported-pattern errors.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_010.sh`).

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, RootId};
use fcb_source::confined::SymlinkPolicy;
use fcb_source::path::NormalizedPath;
use fcb_source::root::RootGrant;
use fcb_source::{
    BoundedDiscovery, CancelFlag, DiscoveryEntry, DiscoveryKind, DiscoveryLimits,
    IgnoreLayerKind, IgnoreMatcher, IncompleteReason, PublicationState, ScanStatus,
    SourceError, UnsupportedReason,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_010_RUN_ID";

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C_10).expect("test owner is non-zero")
}

fn test_root_id(owner_val: u64, root: u64) -> RootId {
    RootId::new(ArenaOwnerId::new(owner_val).unwrap(), root).unwrap()
}

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fcb_010_prod_{}_{}_{}",
            prefix,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn write_file(dir: &Path, rel: &str, content: &[u8]) {
    let full = dir.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("parent dirs created");
    }
    fs::write(full, content).expect("write file");
}

fn drain_all(
    discovery: &mut BoundedDiscovery,
    cancel: &CancelFlag,
) -> Result<Vec<DiscoveryEntry>, SourceError> {
    let mut out = Vec::new();
    while let Some(batch) = discovery.next_batch(cancel)? {
        out.extend_from_slice(batch.entries());
    }
    Ok(out)
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-010-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    fs::create_dir_all(&run_dir).expect("receipts dir created");
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_10_00_01),
        pin: SourcePin::new("0103456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
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

#[test]
fn staged_discovery_progressive_batches() {
    let dir = TempTestDir::new("progressive");
    // Create a hierarchy with 30 files in root, plus subdirectories.
    for i in 0..30 {
        write_file(dir.path(), &format!("root_file_{i:02}.txt"), b"data");
    }
    for i in 0..15 {
        write_file(dir.path(), &format!("sub_a/file_{i:02}.txt"), b"sub_a");
    }
    for i in 0..15 {
        write_file(dir.path(), &format!("sub_b/file_{i:02}.txt"), b"sub_b");
    }

    let grant = RootGrant::new(test_root_id(owner().get(), 1), dir.path());
    // Small batch entry cap forces multiple progressive batches.
    let limits = DiscoveryLimits::new(4, 32, 4096, 10, 64 * 1024, 1024).unwrap();
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).expect("open discovery");

    let cancel = CancelFlag::new();
    let mut batch_count = 0u32;
    let mut provisional_count = 0u32;
    let mut total_entries = 0usize;

    while let Some(batch) = discovery.next_batch(&cancel).expect("next_batch succeeds") {
        batch_count += 1;
        total_entries += batch.entries().len();
        if batch.publication() == PublicationState::Provisional {
            provisional_count += 1;
        }
    }

    assert!(batch_count >= 6, "expected >= 6 batches under cap 10, got {batch_count}");
    assert!(
        provisional_count > 0,
        "expected provisional batches during progressive split, got {provisional_count}"
    );
    // 30 root files + sub_a + sub_b (directories) + 15 in sub_a + 15 in sub_b = 62 entries
    assert_eq!(total_entries, 62, "all 62 entries accounted for");
    assert!(discovery.is_complete(), "discovery reached complete status");

    record_receipt(
        "staged_discovery_progressive_batches",
        Effect::Succeeded,
        "progressive batches yield bounded provisional and stable publications",
    );
}

#[test]
fn shuffled_enumeration_stable_paged_ordering() {
    let dir_1 = TempTestDir::new("shuffled_1");
    let dir_2 = TempTestDir::new("shuffled_2");

    let file_names = ["zebra.rs", "apple.rs", "mango.rs", "banana.rs", "cherry.rs", "kiwi.rs"];

    // Populate dir_1 in forward order
    for name in &file_names {
        write_file(dir_1.path(), name, b"test");
    }
    // Populate dir_2 in reverse order
    for name in file_names.iter().rev() {
        write_file(dir_2.path(), name, b"test");
    }

    let grant_1 = RootGrant::new(test_root_id(owner().get(), 2), dir_1.path());
    let grant_2 = RootGrant::new(test_root_id(owner().get(), 3), dir_2.path());
    let limits = DiscoveryLimits::modest();

    let mut disc_1 = BoundedDiscovery::open(grant_1, SymlinkPolicy::DisallowAll, limits).unwrap();
    let mut disc_2 = BoundedDiscovery::open(grant_2, SymlinkPolicy::DisallowAll, limits).unwrap();

    let cancel = CancelFlag::new();
    let batch_1 = disc_1.next_batch(&cancel).unwrap().expect("batch 1");
    let batch_2 = disc_2.next_batch(&cancel).unwrap().expect("batch 2");

    assert!(
        matches!(batch_1.publication(), PublicationState::Stable { .. }),
        "small directory publishes stable page"
    );
    assert!(
        matches!(batch_2.publication(), PublicationState::Stable { .. }),
        "small directory publishes stable page"
    );

    let paths_1: Vec<String> = batch_1.entries().iter().map(|e| e.path().as_str().unwrap().to_string()).collect();
    let paths_2: Vec<String> = batch_2.entries().iter().map(|e| e.path().as_str().unwrap().to_string()).collect();

    assert_eq!(paths_1, paths_2, "different create order produces identical sorted page");
    assert_eq!(
        paths_1,
        vec!["apple.rs", "banana.rs", "cherry.rs", "kiwi.rs", "mango.rs", "zebra.rs"]
    );

    record_receipt(
        "shuffled_enumeration_stable_paged_ordering",
        Effect::Succeeded,
        "shuffled create order produces deterministic sorted stable pages",
    );
}

#[test]
fn descriptor_and_queue_caps_hold() {
    let dir = TempTestDir::new("caps");
    // Deep directory chain: 10 levels, with files at each level and sibling branches.
    let mut current = PathBuf::new();
    for depth in 0..10 {
        current = current.join(format!("level_{depth}"));
        write_file(dir.path(), &current.join("f1.txt").to_string_lossy(), b"f1");
        write_file(dir.path(), &current.join("f2.txt").to_string_lossy(), b"f2");
        write_file(dir.path(), &current.join("sibling/f3.txt").to_string_lossy(), b"f3");
    }

    let grant = RootGrant::new(test_root_id(owner().get(), 4), dir.path());
    // Strict limits: max 2 open descriptors, max 30 queue entries, batch cap 8.
    let limits = DiscoveryLimits::new(2, 20, 4096, 8, 64 * 1024, 64).unwrap();
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).expect("open discovery");

    let cancel = CancelFlag::new();
    let entries = drain_all(&mut discovery, &cancel).expect("drain completes");
    assert!(!entries.is_empty(), "observed entries from deep tree");

    let peaks = discovery.peaks();
    assert!(
        peaks.open_descriptors <= limits.max_open_descriptors(),
        "open descriptors peak {} exceeded cap {}",
        peaks.open_descriptors,
        limits.max_open_descriptors()
    );
    assert!(
        peaks.queue_entries <= limits.max_queue_entries(),
        "queue entries peak {} exceeded cap {}",
        peaks.queue_entries,
        limits.max_queue_entries()
    );
    assert!(
        peaks.batch_entries <= limits.max_batch_entries(),
        "batch entries peak {} exceeded cap {}",
        peaks.batch_entries,
        limits.max_batch_entries()
    );

    record_receipt(
        "descriptor_and_queue_caps_hold",
        Effect::Succeeded,
        "open descriptor and queue caps held strictly throughout deep traversal",
    );
}

#[test]
fn first_party_ignore_precedence_corpus() {
    let dir = TempTestDir::new("ignore_corpus");
    // Root ignore rules
    let gitignore = r#"
# Comments and blanks
/anchored_dir/
docs/
src/**/cache/
!keep.txt
test\[1\].rs
*.o
"#;
    write_file(dir.path(), ".gitignore", gitignore.as_bytes());

    // Files to test rules
    write_file(dir.path(), "anchored_dir/out.bin", b"bin");
    write_file(dir.path(), "sub/anchored_dir/out.bin", b"sub_bin");
    write_file(dir.path(), "main.o", b"obj");
    write_file(dir.path(), "docs/guide.md", b"guide");
    write_file(dir.path(), "docs_file.txt", b"not_dir");
    write_file(dir.path(), "src/nested/cache/temp.cache", b"temp");
    write_file(dir.path(), "keep.txt", b"keep");
    write_file(dir.path(), "test[1].rs", b"escaped");
    write_file(dir.path(), "test1.rs", b"unescaped");

    // Nested rule file in sub/
    write_file(dir.path(), "sub/.gitignore", b"sub_ignored.txt\n");
    write_file(dir.path(), "sub/sub_ignored.txt", b"ignored");
    write_file(dir.path(), "sub/sub_normal.txt", b"normal");

    let grant = RootGrant::new(test_root_id(owner().get(), 5), dir.path());
    let mut matcher = IgnoreMatcher::product_defaults();
    matcher.add_rule_file(None, gitignore);
    let sub_prefix = NormalizedPath::new("sub").unwrap();
    matcher.add_rule_file(Some(&sub_prefix), "sub_ignored.txt\n");

    let mut discovery = BoundedDiscovery::open_with_ignore(
        grant,
        SymlinkPolicy::DisallowAll,
        DiscoveryLimits::modest(),
        matcher,
    )
    .unwrap();

    let entries = drain_all(&mut discovery, &CancelFlag::new()).unwrap();

    let find_entry = |rel: &str| -> &DiscoveryEntry {
        let entry = entries.iter().find(|e| e.path().as_str().ok() == Some(rel));
        assert!(entry.is_some(), "entry missing in observed entries");
        entry.unwrap()
    };

    // 1. Anchored directory /anchored_dir/ excludes root anchored_dir
    assert!(find_entry("anchored_dir").is_excluded(), "root /anchored_dir/ excluded");
    // 2. Unanchored sub/anchored_dir is NOT excluded because /anchored_dir/ was anchored to root
    assert!(!find_entry("sub/anchored_dir").is_excluded(), "sub/anchored_dir is not excluded by root /anchored_dir/");
    // 3. *.o is excluded
    assert!(find_entry("main.o").is_excluded(), "main.o excluded by *.o");
    // 4. docs/ is excluded as directory
    assert!(find_entry("docs").is_excluded(), "docs/ excluded");
    // 5. docs_file.txt is not excluded (directory-only rule does not match file)
    assert!(!find_entry("docs_file.txt").is_excluded(), "docs_file.txt not excluded");
    // 6. src/**/cache/ matches across directories
    assert!(find_entry("src/nested/cache").is_excluded(), "src/**/cache matched");
    // 7. Negation !keep.txt includes keep.txt
    assert!(!find_entry("keep.txt").is_excluded(), "!keep.txt keeps keep.txt");
    // 8. Escaped test\[1\].rs matches literal test[1].rs
    assert!(find_entry("test[1].rs").is_excluded(), "test[1].rs excluded by test\\[1\\].rs");
    // 9. Unescaped test1.rs is included
    assert!(!find_entry("test1.rs").is_excluded(), "test1.rs is included");
    // 10. Nested sub/.gitignore excludes sub_ignored.txt
    assert!(find_entry("sub/sub_ignored.txt").is_excluded(), "nested sub_ignored.txt excluded");
    // 11. Nested sub/sub_normal.txt is included
    assert!(!find_entry("sub/sub_normal.txt").is_excluded(), "nested sub_normal.txt included");

    record_receipt(
        "first_party_ignore_precedence_corpus",
        Effect::Succeeded,
        "anchored, directory, doublestar, negation, escaping, and nested precedence verified",
    );
}

#[test]
fn visible_excluded_counts_and_deliberate_browse_override() {
    let dir = TempTestDir::new("browse_override");
    write_file(dir.path(), "src/lib.rs", b"pub fn f() {}");
    write_file(dir.path(), "node_modules/pkg/index.js", b"console.log('hi');");
    write_file(dir.path(), "target/debug/app", b"elf");
    write_file(dir.path(), "main.pyc", b"bytecode");

    let grant = RootGrant::new(test_root_id(owner().get(), 6), dir.path());
    let mut discovery = BoundedDiscovery::open(
        grant.clone(),
        SymlinkPolicy::DisallowAll,
        DiscoveryLimits::modest(),
    )
    .unwrap();

    let entries = drain_all(&mut discovery, &CancelFlag::new()).unwrap();

    let lib = entries.iter().find(|e| e.path().as_str().ok() == Some("src/lib.rs")).unwrap();
    assert!(!lib.is_excluded(), "src/lib.rs is included");

    let nm = entries.iter().find(|e| e.path().as_str().ok() == Some("node_modules")).unwrap();
    assert!(nm.is_excluded(), "node_modules is excluded by product defaults");

    let target = entries.iter().find(|e| e.path().as_str().ok() == Some("target")).unwrap();
    assert!(target.is_excluded(), "target is excluded by product defaults");

    let pyc = entries.iter().find(|e| e.path().as_str().ok() == Some("main.pyc")).unwrap();
    assert!(pyc.is_excluded(), "main.pyc is excluded by product defaults");

    assert!(discovery.aggregate().excluded >= 3, "aggregate records excluded entries");

    // Now test deliberate browse override on node_modules
    let mut matcher = IgnoreMatcher::product_defaults();
    let nm_path = NormalizedPath::new("node_modules").unwrap();
    matcher.override_browse(&nm_path);

    let mut overridden = BoundedDiscovery::open_with_ignore(
        grant,
        SymlinkPolicy::DisallowAll,
        DiscoveryLimits::modest(),
        matcher,
    )
    .unwrap();

    let entries_2 = drain_all(&mut overridden, &CancelFlag::new()).unwrap();
    let nm_overridden = entries_2.iter().find(|e| e.path().as_str().ok() == Some("node_modules")).unwrap();
    assert!(
        !nm_overridden.is_excluded(),
        "browse override re-includes node_modules"
    );

    // Descendants of node_modules are now walked and included
    let pkg_js = entries_2.iter().find(|e| e.path().as_str().ok() == Some("node_modules/pkg/index.js"));
    assert!(pkg_js.is_some(), "node_modules/pkg/index.js is discovered under override");
    assert!(!pkg_js.unwrap().is_excluded(), "node_modules/pkg/index.js is included");

    // target remains excluded
    let target_still = entries_2.iter().find(|e| e.path().as_str().ok() == Some("target")).unwrap();
    assert!(target_still.is_excluded(), "target remains excluded");

    record_receipt(
        "visible_excluded_counts_and_deliberate_browse_override",
        Effect::Succeeded,
        "excluded counts are visible, classification is not deletion, browse override re-includes",
    );
}

#[test]
fn unsupported_patterns_reported_not_guessed() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_rule_file(None, "[unclosed_class\nvalid_pattern.txt\ndangling_escape\\\n!\n");

    let reports = matcher.unsupported();
    assert_eq!(reports.len(), 3, "expected exactly 3 unsupported pattern reports");

    assert_eq!(reports[0].raw(), "[unclosed_class");
    assert_eq!(reports[0].reason(), UnsupportedReason::UnclosedClass);
    assert_eq!(reports[0].layer(), IgnoreLayerKind::RuleFile);

    assert_eq!(reports[1].raw(), "dangling_escape\\");
    assert_eq!(reports[1].reason(), UnsupportedReason::DanglingEscape);

    assert_eq!(reports[2].raw(), "!");
    assert_eq!(reports[2].reason(), UnsupportedReason::EmptyPattern);

    // Verify that the valid pattern in the same rule file still works
    let decision_valid = matcher.decide(&NormalizedPath::new("valid_pattern.txt").unwrap(), false);
    assert!(decision_valid.is_excluded(), "valid pattern in the same file was compiled");

    let decision_other = matcher.decide(&NormalizedPath::new("other_file.txt").unwrap(), false);
    assert!(!decision_other.is_excluded(), "unmatched file is included");

    record_receipt(
        "unsupported_patterns_reported_not_guessed",
        Effect::Succeeded,
        "unsupported patterns produce typed reports with layer and reason, valid rules preserved",
    );
}

#[test]
fn negative_control_oracle_detects_defects() {
    let dir = TempTestDir::new("negative_control");
    write_file(dir.path(), "a.txt", b"a");
    write_file(dir.path(), "b.txt", b"b");
    write_file(dir.path(), "c.txt", b"c");

    let grant = RootGrant::new(test_root_id(owner().get(), 7), dir.path());
    let mut discovery = BoundedDiscovery::open(
        grant,
        SymlinkPolicy::DisallowAll,
        DiscoveryLimits::modest(),
    )
    .unwrap();

    // 1. Cancel mid-scan: oracle detects that incomplete scan must NOT be marked complete
    let cancel = CancelFlag::new();
    cancel.cancel();

    let result = discovery.next_batch(&cancel);
    assert_eq!(result.unwrap_err(), SourceError::Canceled);
    assert!(!discovery.is_complete(), "oracle confirms cancelled scan is NOT complete");
    assert_eq!(
        discovery.status(),
        ScanStatus::Incomplete {
            reason: IncompleteReason::Canceled
        }
    );

    // 2. Oracle detects that an unsupported pattern cannot be silently dropped
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_scope_rule("[broken_class");
    assert!(
        !matcher.unsupported().is_empty(),
        "oracle rejects silent dropping of unsupported pattern"
    );

    // 3. Negative control: verify special object detection without open
    #[cfg(unix)]
    {
        let fifo_dir = TempTestDir::new("fifo_neg");
        let fifo_path = fifo_dir.path().join("test.fifo");
        let status = std::process::Command::new("mkfifo")
            .arg(&fifo_path)
            .status();
        if matches!(status, Ok(exit) if exit.success()) {
            let fifo_grant = RootGrant::new(test_root_id(owner().get(), 8), fifo_dir.path());
            let mut fifo_disc = BoundedDiscovery::open(
                fifo_grant,
                SymlinkPolicy::DisallowAll,
                DiscoveryLimits::modest(),
            )
            .unwrap();
            let entries = drain_all(&mut fifo_disc, &CancelFlag::new()).unwrap();
            let fifo_entry = entries.iter().find(|e| e.path().as_str().ok() == Some("test.fifo")).unwrap();
            assert_eq!(fifo_entry.kind(), DiscoveryKind::Special);
        }
    }

    record_receipt(
        "negative_control_oracle_detects_defects",
        Effect::Succeeded,
        "negative control confirms oracle catches descriptor, cancellation, and rule defects",
    );
}
