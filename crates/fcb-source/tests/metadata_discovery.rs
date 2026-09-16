#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb_core::{ArenaOwnerId, RootId};
use fcb_source::{CancelFlag, RawPath};
use fcb_source::confined::SymlinkPolicy;
use fcb_source::discovery::{BoundedDiscovery, DiscoveryLimits};
use fcb_source::ignore::IgnoreMatcher;
use fcb_source::root::RootGrant;

static NEXT: AtomicU64 = AtomicU64::new(1);
struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fcb-metadata-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn grant(&self) -> RootGrant {
        RootGrant::new(RootId::new(ArenaOwnerId::new(221).unwrap(), 1).unwrap(), RawPath::from_path(&self.0))
    }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

#[test]
fn metadata_only_does_not_interpret_or_read_repository_rule_files() {
    let tree = Tree::new();
    fs::write(tree.0.join(".gitignore"), b"*.rs\n").unwrap();
    fs::write(tree.0.join("included.rs"), b"source").unwrap();
    let mut walker = BoundedDiscovery::open_metadata_only(tree.grant(), SymlinkPolicy::DisallowAll,
        DiscoveryLimits::modest(), IgnoreMatcher::include_all()).unwrap();
    assert!(!walker.reads_rule_files());
    let mut found = false;
    while let Some(batch) = walker.next_batch(&CancelFlag::new()).unwrap() {
        for entry in batch.entries() {
            if entry.path().as_bytes() == b"included.rs" { found = true; assert!(!entry.is_excluded()); }
        }
    }
    assert!(found && walker.is_complete());
    assert_eq!(walker.aggregate().excluded, 0);
}

#[test]
fn rejected_names_still_consume_work_and_empty_pages_are_not_eof() {
    let tree = Tree::new();
    for index in 0..100 { fs::write(tree.0.join(format!("long_name_{index:03}")), b"x").unwrap(); }
    let mut walker = BoundedDiscovery::open_metadata_only(tree.grant(), SymlinkPolicy::DisallowAll,
        DiscoveryLimits::new(1, 8, 8, 1, 1024, 4).unwrap(), IgnoreMatcher::include_all()).unwrap();
    let first = walker.next_batch(&CancelFlag::new()).unwrap().unwrap();
    assert!(first.entries().is_empty()); assert!(first.more());
    assert!(!walker.is_complete()); assert!(walker.aggregate().path_limited <= 3);
    let mut pages = 1;
    while let Some(batch) = walker.next_batch(&CancelFlag::new()).unwrap() {
        assert!(batch.entries().is_empty()); pages += 1; assert!(pages < 100);
    }
    assert!(pages >= 25); assert_eq!(walker.aggregate().path_limited, 100);
}
