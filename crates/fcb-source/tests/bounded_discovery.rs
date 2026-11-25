#![forbid(unsafe_code)]

//! Bounded discovery and stable paged publication (FCB-010.A / fcb-hh2.1).
//!
//! Verifies:
//! 1. Iterative work-queue walking with descriptor, depth, path, batch, and queue caps.
//! 2. Deterministic sorted pages for directories that fit in one batch.
//! 3. Provisional publication for oversized directories; no million-entry allocation.
//! 4. Incomplete scans (cancel, revoke, queue saturation) never tombstone unseen entries.
//! 5. Special objects are classified without being opened; FIFOs do not block.
//! 6. Planted negative: a complete-after-cancel oracle would fail.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, RootId};
use fcb_source::confined::SymlinkPolicy;
use fcb_source::ignore::IgnoreMatcher;
use fcb_source::path::NormalizedPath;
use fcb_source::root::RootGrant;
use fcb_source::{
    BoundedDiscovery, CancelFlag, DiscoveryKind, DiscoveryLimits, IncompleteReason,
    PublicationState, ScanStatus, SourceError,
};

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
            "fcb_disc_{}_{}_{}",
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

fn test_root_id(owner: u64, root: u64) -> RootId {
    RootId::new(ArenaOwnerId::new(owner).unwrap(), root).unwrap()
}

fn write_file(dir: &Path, rel: &str, bytes: &[u8]) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = File::create(&path).unwrap();
    file.write_all(bytes).unwrap();
}

fn drain_all(
    discovery: &mut BoundedDiscovery,
    cancel: &CancelFlag,
) -> Result<Vec<(String, DiscoveryKind, u32)>, SourceError> {
    let mut out = Vec::new();
    while let Some(batch) = discovery.next_batch(cancel)? {
        for entry in batch.entries() {
            out.push((
                entry.path().as_str().unwrap().to_string(),
                entry.kind(),
                entry.depth(),
            ));
        }
        if !batch.more() {
            break;
        }
    }
    Ok(out)
}

fn modest_limits() -> DiscoveryLimits {
    DiscoveryLimits::new(2, 8, 256, 32, 4096, 64).unwrap()
}

#[test]
fn zero_limits_are_rejected() {
    assert_eq!(
        DiscoveryLimits::new(0, 8, 256, 32, 4096, 64).unwrap_err(),
        SourceError::InvalidRange
    );
}

#[test]
fn small_directory_publishes_stable_sorted_page_independent_of_create_order() {
    let dir = TempTestDir::new("stable_sort");
    write_file(dir.path(), "z.rs", b"z");
    write_file(dir.path(), "m.rs", b"m");
    write_file(dir.path(), "a.rs", b"a");

    let grant = RootGrant::new(test_root_id(1, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, modest_limits()).unwrap();
    let cancel = CancelFlag::new();
    let batch = discovery.next_batch(&cancel).unwrap().expect("page");

    let names: Vec<&str> = batch
        .entries()
        .iter()
        .map(|entry| entry.path().as_str().unwrap())
        .collect();
    assert_eq!(names, ["a.rs", "m.rs", "z.rs"]);
    assert!(matches!(batch.publication(), PublicationState::Stable { .. }));
    assert!(!batch.more());
    assert!(discovery.is_complete());
    assert_eq!(batch.aggregate().files, 3);
}

#[test]
fn oversized_directory_is_provisional_and_never_allocates_past_batch_cap() {
    let dir = TempTestDir::new("provisional_page");
    for idx in 0..10 {
        write_file(dir.path(), &format!("f{idx:02}.txt"), b"x");
    }
    let limits = DiscoveryLimits::new(1, 4, 256, 3, 4096, 16).unwrap();
    let grant = RootGrant::new(test_root_id(2, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let cancel = CancelFlag::new();

    let first = discovery.next_batch(&cancel).unwrap().expect("first page");
    assert_eq!(first.entries().len(), 3);
    assert_eq!(first.publication(), PublicationState::Provisional);
    assert!(first.more());
    assert!(first.entries().windows(2).all(|pair| {
        pair[0].path().as_bytes() <= pair[1].path().as_bytes()
    }));
    assert!(discovery.peaks().batch_entries <= 3);

    let rest = drain_all(&mut discovery, &cancel).unwrap();
    assert_eq!(first.entries().len() + rest.len(), 10);
    assert!(discovery.is_complete());
}

#[test]
fn byte_bounded_pages_preserve_every_nested_entry_once() {
    let dir = TempTestDir::new("byte_pages");
    for name in ["a.rs", "midname.rs", "longer_name.rs", "nested/x", "nested/deeper/y"] {
        write_file(dir.path(), name, b"x");
    }
    let limits = DiscoveryLimits::new(1, 8, 256, 32, 16, 16).unwrap();
    let grant = RootGrant::new(test_root_id(31, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let cancel = CancelFlag::new();
    let mut names = Vec::new();
    let mut provisional = false;
    for _ in 0..32 {
        let Some(batch) = discovery.next_batch(&cancel).unwrap() else { break; };
        let bytes: usize = batch.entries().iter().map(|entry| entry.path().as_bytes().len()).sum();
        assert!(bytes <= 16, "page exceeded its byte admission: {bytes}");
        assert!(batch.entries().len() <= 32);
        assert!(batch.entries().windows(2).all(|pair| pair[0].path() <= pair[1].path()));
        provisional |= batch.publication() == PublicationState::Provisional;
        names.extend(batch.entries().iter().map(|entry| entry.path().as_str().unwrap().to_string()));
        if !batch.more() { break; }
    }
    names.sort();
    assert_eq!(names, [
        "a.rs", "longer_name.rs", "midname.rs", "nested",
        "nested/deeper", "nested/deeper/y", "nested/x",
    ]);
    assert!(provisional, "a byte-split directory cannot publish stable child order");
    assert!(discovery.is_complete(), "bounded pages must eventually drain");
    assert_eq!(discovery.aggregate().files, 5);
    assert_eq!(discovery.aggregate().directories, 2);
    assert!(discovery.peaks().batch_bytes <= 16);
    assert!(discovery.peaks().open_descriptors <= 1);
    assert!(discovery.peaks().queue_entries <= 16);
}

#[test]
fn too_small_page_retains_observations_and_retries_without_reobserving() {
    // Workspace traversal first returns its already collected parent entry.
    // Exact-file discovery drains its directory queue before retaining the
    // oversized entry, so only that retained entry keeps the scan in progress.
    for exact_file in [false, true] {
        let dir = TempTestDir::new("byte_retry");
        let name = if exact_file { "long_name.txt" } else { "d/long_name.txt" };
        write_file(dir.path(), name, b"x");
        let initial_bytes = if exact_file { name.len() as u64 } else { 4 };
        let limits = DiscoveryLimits::new(1, 8, 256, 32, initial_bytes, 16).unwrap();
        let grant = RootGrant::new(test_root_id(32, 1), dir.path());
        let mut discovery = if exact_file {
            BoundedDiscovery::open_metadata_only_for_file(
                grant, NormalizedPath::new(name).unwrap(), limits, IgnoreMatcher::include_all(),
            ).unwrap()
        } else {
            BoundedDiscovery::open_metadata_only(
                grant, SymlinkPolicy::DisallowAll, limits, IgnoreMatcher::include_all(),
            ).unwrap()
        };
        let cancel = CancelFlag::new();
        if exact_file {
            discovery.set_max_batch_bytes(4).unwrap();
        } else {
            let first = discovery.next_batch(&cancel).unwrap().expect("parent page");
            assert_eq!(first.entries().len(), 1);
            assert_eq!(first.entries()[0].path().as_str(), Ok("d"));
            assert!(first.more());
            assert_eq!(first.publication(), PublicationState::Provisional);
        }
        for _ in 0..3 {
            assert_eq!(discovery.next_batch(&cancel).unwrap_err(), SourceError::PayloadTooLarge);
            assert_eq!(discovery.status(), ScanStatus::InProgress);
            assert_eq!(discovery.aggregate().files, 1);
        }
        assert_eq!(discovery.set_max_batch_bytes(0), Err(SourceError::InvalidRange));
        assert_eq!(discovery.limits().max_batch_bytes(), 4);
        assert!(discovery.peaks().batch_bytes <= 4);

        // A retry publishes the original observation, including its length,
        // rather than silently replacing it with a later filesystem state.
        write_file(dir.path(), name, b"changed after observation");
        discovery.set_max_batch_bytes(name.len() as u64).unwrap();
        let page = discovery.next_batch(&cancel).unwrap().expect("retained entry");
        assert_eq!(page.entries().len(), 1);
        assert_eq!(page.entries()[0].path().as_str(), Ok(name));
        assert_eq!(page.entries()[0].observed_len(), Some(1));
        assert!(drain_all(&mut discovery, &cancel).unwrap().is_empty());
        assert!(discovery.is_complete());
        assert_eq!(discovery.aggregate().files, 1);
        assert_eq!(discovery.aggregate().directories, if exact_file { 0 } else { 1 });
        assert_eq!(discovery.peaks().batch_bytes, name.len() as u64);
    }
}

#[test]
fn retained_oversized_entry_still_obeys_cancellation_and_revocation() {
    for revoke in [false, true] {
        let dir = TempTestDir::new("byte_interrupt");
        write_file(dir.path(), "long_name.txt", b"x");
        let grant = RootGrant::new(test_root_id(33, 1), dir.path());
        let limits = DiscoveryLimits::new(1, 8, 256, 32, 4, 16).unwrap();
        let mut discovery = BoundedDiscovery::open_metadata_only(
            grant.clone(), SymlinkPolicy::DisallowAll, limits, IgnoreMatcher::include_all(),
        ).unwrap();
        let cancel = CancelFlag::new();
        assert_eq!(discovery.next_batch(&cancel).unwrap_err(), SourceError::PayloadTooLarge);
        discovery.set_max_batch_bytes(64).unwrap();
        let (error, reason) = if revoke {
            grant.revoke();
            (SourceError::GrantRevoked, IncompleteReason::GrantRevoked)
        } else {
            cancel.cancel();
            (SourceError::Canceled, IncompleteReason::Canceled)
        };
        assert_eq!(discovery.next_batch(&cancel).unwrap_err(), error);
        assert_eq!(discovery.status(), ScanStatus::Incomplete { reason });
        assert!(!discovery.is_complete());
        assert_eq!(discovery.aggregate().files, 1);
        assert_eq!(discovery.peaks().batch_entries, 0);
    }
}

#[cfg(unix)]
#[test]
fn byte_pages_count_authoritative_raw_filename_bytes() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let dir = TempTestDir::new("raw_byte_pages");
    let mut expected = vec![b"\xffa.rs".to_vec(), b"\xfeb.rs".to_vec(), b"a\\b.rs".to_vec()];
    for name in &expected {
        fs::write(dir.path().join(OsStr::from_bytes(name)), b"x").unwrap();
    }
    let limits = DiscoveryLimits::new(1, 8, 256, 32, 7, 16).unwrap();
    let grant = RootGrant::new(test_root_id(34, 1), dir.path());
    let mut discovery = BoundedDiscovery::open_metadata_only(
        grant, SymlinkPolicy::DisallowAll, limits, IgnoreMatcher::include_all(),
    ).unwrap();
    let cancel = CancelFlag::new();
    let mut names = Vec::new();
    for _ in 0..8 {
        let Some(batch) = discovery.next_batch(&cancel).unwrap() else { break; };
        assert_eq!(batch.entries().len(), 1);
        assert!(batch.entries()[0].path().as_bytes().len() <= 7);
        names.push(batch.entries()[0].path().as_bytes().to_vec());
        if !batch.more() { break; }
    }
    names.sort();
    expected.sort();
    assert_eq!(names, expected);
    assert_eq!(discovery.aggregate().files, 3);
    assert!(discovery.is_complete());
    assert!(discovery.peaks().batch_bytes <= 7);
}

#[test]
fn depth_limit_lists_the_boundary_directory_but_does_not_walk_it() {
    let dir = TempTestDir::new("depth");
    write_file(dir.path(), "keep.txt", b"k");
    write_file(dir.path(), "a/b/c/secret.txt", b"nope");

    let limits = DiscoveryLimits::new(2, 2, 256, 32, 4096, 16).unwrap();
    let grant = RootGrant::new(test_root_id(3, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let observed = drain_all(&mut discovery, &CancelFlag::new()).unwrap();

    let paths: Vec<&str> = observed.iter().map(|(path, _, _)| path.as_str()).collect();
    assert!(paths.contains(&"keep.txt"));
    assert!(paths.contains(&"a"));
    assert!(paths.contains(&"a/b"));
    assert!(!paths.iter().any(|path| path.contains("secret")));
    assert!(discovery.aggregate().depth_limited >= 1);
    assert_eq!(discovery.status(), ScanStatus::Incomplete { reason: IncompleteReason::TraversalLimited });
}

#[test]
fn path_byte_limit_continues_with_explicit_incomplete_coverage() {
    let dir = TempTestDir::new("pathlen");
    write_file(dir.path(), "ok.txt", b"ok");
    write_file(dir.path(), "this_name_is_too_long.txt", b"nope");

    let limits = DiscoveryLimits::new(1, 4, 8, 16, 4096, 8).unwrap();
    let grant = RootGrant::new(test_root_id(4, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let observed = drain_all(&mut discovery, &CancelFlag::new()).unwrap();
    let paths: Vec<&str> = observed.iter().map(|(path, _, _)| path.as_str()).collect();
    assert_eq!(paths, ["ok.txt"]);
    assert!(discovery.aggregate().path_limited >= 1);
    assert_eq!(discovery.status(), ScanStatus::Incomplete { reason: IncompleteReason::TraversalLimited });
}

#[test]
fn deep_tree_walks_with_a_queue_not_a_call_stack() {
    let dir = TempTestDir::new("deep");
    let mut rel = String::new();
    for level in 0..48 {
        if !rel.is_empty() {
            rel.push('/');
        }
        rel.push_str(&format!("d{level}"));
        fs::create_dir_all(dir.path().join(&rel)).unwrap();
    }
    write_file(dir.path(), &format!("{rel}/leaf.txt"), b"leaf");

    let limits = DiscoveryLimits::new(1, 64, 4096, 8, 4096, 64).unwrap();
    let grant = RootGrant::new(test_root_id(5, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let observed = drain_all(&mut discovery, &CancelFlag::new()).unwrap();
    assert!(
        observed
            .iter()
            .any(|(path, kind, _)| path.ends_with("leaf.txt") && *kind == DiscoveryKind::File)
    );
    assert!(discovery.peaks().open_descriptors <= 1);
    assert!(discovery.is_complete());
}

#[test]
fn cancel_does_not_tombstone_unseen_entries() {
    let dir = TempTestDir::new("cancel");
    write_file(dir.path(), "visible.txt", b"v");
    write_file(dir.path(), "nested/hidden.txt", b"h");

    let limits = DiscoveryLimits::new(1, 8, 256, 1, 4096, 8).unwrap();
    let grant = RootGrant::new(test_root_id(6, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let cancel = CancelFlag::new();
    let first = discovery.next_batch(&cancel).unwrap().expect("first");
    assert!(!first.entries().is_empty());
    cancel.cancel();
    let err = discovery.next_batch(&cancel).unwrap_err();
    assert_eq!(err, SourceError::Canceled);
    assert_eq!(
        discovery.status(),
        ScanStatus::Incomplete {
            reason: IncompleteReason::Canceled
        }
    );
    assert!(!discovery.is_complete());
    let seen: Vec<&str> = first
        .entries()
        .iter()
        .map(|entry| entry.path().as_str().unwrap())
        .collect();
    assert!(
        !seen.iter().any(|path| path.contains("hidden")),
        "canceled scan must not invent unseen entries"
    );
}

#[test]
fn planted_negative_complete_after_cancel_is_refused() {
    let dir = TempTestDir::new("neg_complete");
    write_file(dir.path(), "a.txt", b"a");
    write_file(dir.path(), "sub/b.txt", b"b");
    let grant = RootGrant::new(test_root_id(7, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, modest_limits()).unwrap();
    let cancel = CancelFlag::new();
    let _ = discovery.next_batch(&cancel).unwrap();
    cancel.cancel();
    let _ = discovery.next_batch(&cancel);
    // Planted negative: treating cancel as a successful closed scan.
    let naive_complete = matches!(discovery.status(), ScanStatus::Completed { .. });
    assert!(
        !naive_complete,
        "a walker that marks cancel as completed would hide unseen files"
    );
}

#[test]
fn grant_revocation_stops_new_pages_without_deleting_prior_observations() {
    let dir = TempTestDir::new("revoke");
    write_file(dir.path(), "one.txt", b"1");
    write_file(dir.path(), "two/three.txt", b"3");
    let grant = RootGrant::new(test_root_id(8, 1), dir.path());
    let limits = DiscoveryLimits::new(1, 8, 256, 1, 4096, 8).unwrap();
    let mut discovery =
        BoundedDiscovery::open(grant.clone(), SymlinkPolicy::DisallowAll, limits).unwrap();
    let cancel = CancelFlag::new();
    let first = discovery.next_batch(&cancel).unwrap().expect("first");
    assert!(!first.entries().is_empty());
    grant.revoke();
    assert_eq!(
        discovery.next_batch(&cancel).unwrap_err(),
        SourceError::GrantRevoked
    );
    assert!(!discovery.is_complete());
}

#[test]
fn queue_saturation_is_incomplete_and_does_not_claim_full_coverage() {
    let dir = TempTestDir::new("queue");
    for idx in 0..8 {
        write_file(dir.path(), &format!("d{idx}/f.txt"), b"x");
    }
    let limits = DiscoveryLimits::new(1, 8, 256, 32, 4096, 2).unwrap();
    let grant = RootGrant::new(test_root_id(9, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let _ = drain_all(&mut discovery, &CancelFlag::new());
    assert!(discovery.aggregate().queue_refused >= 1);
    assert_eq!(
        discovery.status(),
        ScanStatus::Incomplete {
            reason: IncompleteReason::QueueSaturated
        }
    );
    assert!(discovery.peaks().queue_entries <= 2);
}

#[test]
fn descriptor_cap_is_respected() {
    let dir = TempTestDir::new("desc");
    write_file(dir.path(), "a/x.txt", b"x");
    write_file(dir.path(), "b/y.txt", b"y");
    write_file(dir.path(), "c/z.txt", b"z");
    let limits = DiscoveryLimits::new(1, 8, 256, 4, 4096, 16).unwrap();
    let grant = RootGrant::new(test_root_id(10, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();
    let _ = drain_all(&mut discovery, &CancelFlag::new()).unwrap();
    assert!(discovery.peaks().open_descriptors <= 1);
}

#[test]
fn two_roots_do_not_alias_namespace_entries() {
    let left = TempTestDir::new("left");
    let right = TempTestDir::new("right");
    write_file(left.path(), "same.txt", b"L");
    write_file(right.path(), "same.txt", b"R");
    let cancel = CancelFlag::new();

    let mut first = BoundedDiscovery::open(
        RootGrant::new(test_root_id(11, 1), left.path()),
        SymlinkPolicy::DisallowAll,
        modest_limits(),
    )
    .unwrap();
    let mut second = BoundedDiscovery::open(
        RootGrant::new(test_root_id(11, 2), right.path()),
        SymlinkPolicy::DisallowAll,
        modest_limits(),
    )
    .unwrap();
    let left_entries = drain_all(&mut first, &cancel).unwrap();
    let right_entries = drain_all(&mut second, &cancel).unwrap();
    assert_eq!(left_entries.len(), 1);
    assert_eq!(right_entries.len(), 1);
    assert_eq!(left_entries[0].0, "same.txt");
    assert_eq!(right_entries[0].0, "same.txt");
    assert!(first.is_complete());
    assert!(second.is_complete());
}

#[cfg(unix)]
#[test]
fn fifo_is_classified_special_without_opening() {
    let dir = TempTestDir::new("fifo");
    write_file(dir.path(), "ok.txt", b"ok");
    let fifo_path = dir.path().join("pipe.fifo");
    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status();
    if !matches!(status, Ok(exit) if exit.success()) {
        return;
    }
    let grant = RootGrant::new(test_root_id(12, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, modest_limits()).unwrap();
    let observed = drain_all(&mut discovery, &CancelFlag::new()).unwrap();
    assert!(
        observed
            .iter()
            .any(|(path, kind, _)| path == "pipe.fifo" && *kind == DiscoveryKind::Special)
    );
    assert!(
        observed
            .iter()
            .any(|(path, kind, _)| path == "ok.txt" && *kind == DiscoveryKind::File)
    );
}

#[cfg(unix)]
#[test]
fn symlink_is_published_and_not_followed_when_disallowed() {
    let dir = TempTestDir::new("symlink");
    write_file(dir.path(), "real/file.txt", b"x");
    std::os::unix::fs::symlink(dir.path().join("real"), dir.path().join("link")).unwrap();
    let grant = RootGrant::new(test_root_id(13, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, modest_limits()).unwrap();
    let observed = drain_all(&mut discovery, &CancelFlag::new()).unwrap();
    assert!(
        observed
            .iter()
            .any(|(path, kind, _)| path == "link" && *kind == DiscoveryKind::Symlink)
    );
    assert!(
        !observed
            .iter()
            .any(|(path, _, _)| path.starts_with("link/"))
    );
    assert_eq!(discovery.aggregate().excluded, 1, "disallowed link traversal is an explicit policy exclusion");
    assert!(discovery.is_complete(), "the policy-scoped namespace was enumerated");
}

#[test]
fn missing_root_is_unavailable() {
    let grant = RootGrant::new(
        test_root_id(14, 1),
        Path::new("/definitely/not/an/fcb/discovery/root"),
    );
    let err = BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, modest_limits()).unwrap_err();
    assert_eq!(err, SourceError::RootUnavailable);
}
