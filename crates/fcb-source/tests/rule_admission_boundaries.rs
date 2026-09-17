#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, RootId};
use fcb_source::{BoundedDiscovery, CancelFlag, DiscoveryLimits, RawPath, RootGrant};
use fcb_source::ignore::repository::{RuleError, RuleLimits};

struct Fixture(PathBuf);
impl Fixture {
    fn new(rule: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let path = std::env::temp_dir().join(format!("fcb-rule-edges-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        fs::write(path.join(".gitignore"), rule).unwrap(); fs::write(path.join("secret.tmp"), b"needle").unwrap();
        Self(path)
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn walk(fixture: &Fixture, total: usize) -> BoundedDiscovery {
    let owner = ArenaOwnerId::new(5131).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(128 * 1024 * 1024)).unwrap();
    let grant = RootGrant::new(RootId::new(owner, 1).unwrap(), RawPath::from_path(&fixture.0));
    let mut walker = BoundedDiscovery::open_rule_aware(grant, DiscoveryLimits::modest(),
        RuleLimits { max_total_bytes: total, ..Default::default() }, &budget, ResourceAllocationId::new(1).unwrap()).unwrap();
    for _ in 0..100 {
        match walker.next_batch(&CancelFlag::new()).unwrap() {
            Some(batch) if batch.more() => {},
            _ => return walker,
        }
    }
    panic!("bounded fixture did not terminate");
}

#[test]
fn exact_configuration_byte_exhaustion_does_not_authorize_an_extra_probe_read() {
    let fixture = Fixture::new(b"*.tmp\n");
    let refused = walk(&fixture, 6);
    let rules = refused.repository_rules().unwrap();
    assert_eq!(rules.stats().bytes_read, 6);
    assert_eq!(rules.stats().read_calls, 1);
    assert_eq!(rules.diagnostics()[0].error(), RuleError::ByteLimit);
    assert_eq!(rules.stats().files_loaded, 0);
    assert!(!refused.is_complete());
    let admitted = walk(&fixture, 7);
    let rules = admitted.repository_rules().unwrap();
    assert_eq!(rules.stats().bytes_read, 6);
    assert_eq!(rules.stats().read_calls, 2);
    assert_eq!(rules.stats().files_loaded, 1);
    assert!(admitted.is_complete());
}

#[test]
fn repository_policy_does_not_collapse_empty_segments_into_a_different_pattern() {
    let fixture = Fixture::new(b"deep//secret.tmp\n");
    let walker = walk(&fixture, 100);
    let rules = walker.repository_rules().unwrap();
    assert_eq!(rules.stats().files_loaded, 0);
    assert_eq!(rules.diagnostics()[0].error(), RuleError::UnsupportedPattern);
    assert!(!walker.is_complete());
}
