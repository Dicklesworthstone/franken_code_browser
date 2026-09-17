#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, RootId};
use fcb_source::{BoundedDiscovery, CancelFlag, DiscoveryEntry, DiscoveryKind, DiscoveryLimits,
    IgnoreMatcher, IncompleteReason, RawPath, RootGrant, ScanStatus, SymlinkPolicy};
use fcb_source::ignore::repository::{RuleError, RuleLimits};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-rules-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap(); Self(path)
    }
    fn file(&self, path: impl AsRef<Path>, bytes: &[u8]) {
        let path = self.0.join(path); fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn grant(&self) -> RootGrant { RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&self.0)) }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(5129).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn id() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
fn discovery(tree: &Tree, rules: RuleLimits, b: &ResourceBudget, page: u32) -> BoundedDiscovery {
    BoundedDiscovery::open_rule_aware(tree.grant(), DiscoveryLimits::new(1, 32, 2048, page, 4096, 64).unwrap(), rules, b, id()).unwrap()
}
fn drain(discovery: &mut BoundedDiscovery) -> Vec<DiscoveryEntry> {
    let mut all = Vec::new();
    for _ in 0..10_000 {
        match discovery.next_batch(&CancelFlag::new()).unwrap() {
            Some(page) => { all.extend_from_slice(page.entries()); if !page.more() { break; } }
            None => break,
        }
    }
    assert!(!matches!(discovery.status(), ScanStatus::InProgress));
    all.sort_by(|a, b| a.path().as_bytes().cmp(b.path().as_bytes())); all
}
fn included(entries: &[DiscoveryEntry]) -> Vec<Vec<u8>> {
    entries.iter().filter(|e| e.kind() == DiscoveryKind::File && !e.is_excluded())
        .map(|e| e.path().as_bytes().to_vec()).collect()
}

#[test]
fn root_nested_and_application_rule_precedence_survive_one_entry_pages() {
    for reverse in [false, true] {
        let tree = Tree::new();
        let mut files = vec![
            (".gitignore", b"*.tmp\r\n/root-only.rs\r\nignored/\r\n".as_slice()),
            (".fcbignore", b"!keep.tmp\n"),
            ("nested/.gitignore", b"*.rs\n!keep.rs\n"),
            ("nested/.fcbignore", b"keep.rs\n!restore.rs\n"),
            ("root-only.rs", b"excluded"), ("drop.tmp", b"excluded"), ("keep.tmp", b"kept"),
            ("nested/drop.rs", b"excluded"), ("nested/keep.rs", b"excluded"), ("nested/restore.rs", b"kept"),
            ("other/root-only.rs", b"kept"), ("other/keep.rs", b"kept"),
            ("ignored/.gitignore", b"[broken"), ("ignored/secret.rs", b"must not be visited"),
        ];
        if reverse { files.reverse(); }
        for (path, bytes) in files { tree.file(path, bytes); }
        let b = budget(); let mut walker = discovery(&tree, RuleLimits::default(), &b, 1);
        let entries = drain(&mut walker);
        let kept = included(&entries);
        for path in [b"keep.tmp".as_slice(), b"nested/restore.rs", b"other/root-only.rs", b"other/keep.rs"] {
            assert!(kept.contains(&path.to_vec()), "missing {path:?}");
        }
        for path in [b"root-only.rs".as_slice(), b"drop.tmp", b"nested/keep.rs", b"nested/drop.rs"] {
            assert!(!kept.contains(&path.to_vec()), "leaked {path:?}");
        }
        assert!(!entries.iter().any(|e| e.path().as_bytes().starts_with(b"ignored/")));
        assert!(walker.is_complete());
        let rules = walker.repository_rules().unwrap();
        assert_eq!(rules.stats().files_loaded, 4);
        assert!(rules.stats().is_complete());
        assert!(rules.observations().windows(2).all(|p| p[0].path() < p[1].path()));
        assert!(walker.peaks().open_descriptors <= 1);
    }
}

#[test]
fn metadata_only_route_remains_inert_with_respect_to_rule_contents() {
    let tree = Tree::new(); tree.file(".gitignore", b"*\n"); tree.file("visible.rs", b"bytes");
    let mut walker = BoundedDiscovery::open_metadata_only(tree.grant(), SymlinkPolicy::DisallowAll,
        DiscoveryLimits::modest(), IgnoreMatcher::product_defaults()).unwrap();
    assert!(!walker.reads_rule_files());
    assert!(included(&drain(&mut walker)).contains(&b"visible.rs".to_vec()));
    assert!(walker.repository_rules().is_none());
}

#[test]
fn invalid_nested_rules_withhold_only_the_affected_subtree_and_mark_scope_partial() {
    for bad in [b"[broken".as_slice(), b"good.rs\n[broken", b"\xff", b"[[:alpha:]].rs", b"bad\0pattern"] {
        let tree = Tree::new(); tree.file("safe.rs", b"kept");
        tree.file("bad/.gitignore", bad); tree.file("bad/secret.rs", b"do not include under unknown policy");
        let b = budget(); let mut walker = discovery(&tree, RuleLimits::default(), &b, 4);
        let entries = drain(&mut walker);
        assert!(included(&entries).contains(&b"safe.rs".to_vec()));
        assert!(!entries.iter().any(|e| e.path().as_bytes().starts_with(b"bad/")));
        assert_eq!(walker.status(), ScanStatus::Incomplete { reason: IncompleteReason::RulePolicyUnavailable });
        let rules = walker.repository_rules().unwrap();
        assert_eq!(rules.stats().failed_files, 1); assert!(!rules.stats().is_complete());
        assert_eq!(rules.diagnostics()[0].path(), b"bad/.gitignore");
        assert_eq!(rules.stats().files_loaded, 0, "a valid prefix cannot escape a rejected file");
    }
}

#[test]
fn byte_pattern_compiler_check_and_match_quotas_produce_explicit_unknown_evidence() {
    for limits in [RuleLimits { max_file_bytes: 1, ..Default::default() },
        RuleLimits { max_total_bytes: 1, ..Default::default() },
        RuleLimits { max_files: 0, ..Default::default() },
        RuleLimits { max_checks: 0, ..Default::default() },
        RuleLimits { max_read_calls: 0, ..Default::default() },
        RuleLimits { max_rules: 0, ..Default::default() },
        RuleLimits { max_pattern_bytes: 1, ..Default::default() },
        RuleLimits { max_compiled_bytes: 0, ..Default::default() },
        RuleLimits { max_match_steps: 0, ..Default::default() },
        RuleLimits { max_total_match_steps: 0, ..Default::default() }] {
        let tree = Tree::new(); tree.file(".gitignore", b"*.tmp\n"); tree.file("secret.tmp", b"never visible");
        let b = budget(); let mut walker = discovery(&tree, limits, &b, 1);
        assert!(included(&drain(&mut walker)).is_empty());
        assert!(!walker.is_complete());
        let stats = walker.repository_rules().unwrap().stats();
        assert!(!stats.is_complete()); assert!(stats.match_steps <= limits.max_total_match_steps);
        assert!(stats.read_calls <= limits.max_read_calls);
        drop(walker); assert_eq!(b.accounting().reserved().get(), 0);
    }
}

#[test]
fn raw_directory_names_and_escaped_patterns_keep_native_identity() {
    use std::os::unix::ffi::OsStringExt;
    let tree = Tree::new();
    let name = std::ffi::OsString::from_vec(b"raw\\dir\xff".to_vec());
    tree.file(Path::new(&name).join(".gitignore"), b"*.tmp\n\\!literal\n\\#literal\nname\\ ");
    for file in ["bad.tmp", "!literal", "#literal", "name ", "kept.rs"] { tree.file(Path::new(&name).join(file), b"x"); }
    let b = budget(); let mut walker = discovery(&tree, RuleLimits::default(), &b, 2);
    let kept = included(&drain(&mut walker));
    assert!(kept.contains(&b"raw\\dir\xff/kept.rs".to_vec()));
    for suffix in [b"bad.tmp".as_slice(), b"!literal", b"#literal", b"name "] {
        let mut path = b"raw\\dir\xff/".to_vec(); path.extend_from_slice(suffix); assert!(!kept.contains(&path));
    }
    assert!(walker.is_complete());
}

#[test]
fn symlink_rule_files_are_not_followed_and_do_not_enable_default_inclusion() {
    use std::os::unix::fs::symlink;
    let tree = Tree::new(); tree.file("outside-rules", b"*.rs\n"); tree.file("nested/secret.rs", b"secret");
    symlink(tree.0.join("outside-rules"), tree.0.join("nested/.gitignore")).unwrap();
    let b = budget(); let mut walker = discovery(&tree, RuleLimits::default(), &b, 2);
    let entries = drain(&mut walker);
    assert!(!entries.iter().any(|e| e.path().as_bytes().starts_with(b"nested/")));
    assert_eq!(walker.repository_rules().unwrap().diagnostics()[0].error(), RuleError::SpecialObject);
    assert_eq!(walker.repository_rules().unwrap().stats().bytes_read, 0);
}

#[test]
fn diagnostics_and_rule_bytes_have_session_wide_not_per_directory_caps() {
    let tree = Tree::new();
    for i in 0..40 { tree.file(format!("d{i:02}/.gitignore"), b"[bad"); tree.file(format!("d{i:02}/secret"), b"x"); }
    let b = budget(); let mut walker = discovery(&tree, RuleLimits::default(), &b, 64);
    drain(&mut walker);
    let rules = walker.repository_rules().unwrap();
    assert_eq!(rules.diagnostics().len(), 32); assert_eq!(rules.stats().diagnostics_omitted, 8);
    assert_eq!(rules.stats().failed_files, 40); assert_eq!(rules.stats().bytes_read, 160);
}

#[test]
fn preadmission_and_cancellation_do_not_publish_pages_or_leak_rule_capacity() {
    let tree = Tree::new(); tree.file(".gitignore", b"*.rs\n");
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(matches!(BoundedDiscovery::open_rule_aware(tree.grant(), DiscoveryLimits::modest(),
        RuleLimits::default(), &tiny, id()), Err(RuleError::ResourceDenied)));
    let b = budget(); let mut walker = discovery(&tree, RuleLimits::default(), &b, 1);
    let cancel = CancelFlag::new(); cancel.cancel();
    assert!(walker.next_batch(&cancel).is_err());
    assert_eq!(walker.repository_rules().unwrap().stats().bytes_read, 0);
    drop(walker); assert_eq!(b.accounting().reserved().get(), 0);
}
