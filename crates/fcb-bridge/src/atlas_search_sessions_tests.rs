#![forbid(unsafe_code)]
use super::*;
use std::{fs, path::PathBuf};
use crate::reader_sessions::{AccessError as ReaderError, Command as ReaderCommand};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("fcb-search-registry-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap(); fs::write(path.join("a.rs"), b"old needle\n").unwrap(); Self(path)
    }
    fn open(&self, registry: &AtlasSessions) -> u64 {
        let handle = registry.create().unwrap();
        registry.open(handle, &self.0, Default::default(), || false).unwrap(); handle
    }
}
fn find<'a>(generation: u64, needle: &'a str) -> Command<'a> {
    Command::Search { generation, needle, options: AtlasSearchOptions::default() }
}

#[test]
fn search_to_reader_uses_old_bytes_after_source_and_atlas_close() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap();
    atlases.execute(a, find(1, "needle"), || false).unwrap();
    fs::write(fixture.0.join("a.rs"), b"new live source").unwrap();
    let linked = atlases.open_search_reader(a, &readers, r, 1, 1, || false).unwrap();
    assert!(linked.as_str().contains("retained-search-capture"));
    assert!(linked.as_str().contains(&format!("\"reader_owner\":\"{r}\"")));
    atlases.close(a).unwrap();
    let copied = readers.execute(r, ReaderCommand::CopyRange { start: 0, end: 11 }, || false).unwrap();
    assert!(copied.as_str().contains("6f6c64206e6565646c650a"));
    readers.close(r).unwrap();
}

#[test]
fn canceled_replacement_retains_query_and_can_be_retried_with_fresh_generation() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let a = fixture.open(&atlases);
    atlases.execute(a, find(1, "needle"), || false).unwrap();
    let mut fired = false;
    let result = atlases.execute(a, find(2, "old"), || {
        if !fired { fired = true; atlases.cancel(a).unwrap(); } false
    });
    assert!(matches!(result, Err(AccessError::Canceled)));
    let old = atlases.execute(a, Command::SearchPage { generation: 1, start: 0, limit: 10 }, || false).unwrap();
    assert!(old.as_str().contains("\"needle\":\"needle\""));
    assert!(matches!(atlases.execute(a, find(2, "old"), || false), Err(AccessError::Search(AtlasSearchError::StaleQuery))));
    atlases.execute(a, find(3, "old"), || false).unwrap();
    atlases.close(a).unwrap();
}

#[test]
fn stale_or_missing_hit_keeps_empty_reader_retryable_and_loaded_reader_is_protected() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap();
    atlases.execute(a, find(1, "needle"), || false).unwrap();
    assert!(matches!(atlases.open_search_reader(a, &readers, r, 2, 1, || false), Err(AccessError::Search(AtlasSearchError::StaleQuery))));
    assert!(matches!(atlases.open_search_reader(a, &readers, r, 1, 2, || false), Err(AccessError::Search(AtlasSearchError::MissingHit))));
    assert!(matches!(readers.execute(r, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    atlases.open_search_reader(a, &readers, r, 1, 1, || false).unwrap();
    assert!(matches!(atlases.open_search_reader(a, &readers, r, 1, 1, || false), Err(AccessError::Reader(ReaderError::AlreadyOpen))));
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn reader_cancellation_during_search_activation_never_installs_partial_source() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap();
    atlases.execute(a, find(1, "needle"), || false).unwrap();
    let mut fired = false;
    assert!(atlases.open_search_reader(a, &readers, r, 1, 1, || {
        if !fired { fired = true; readers.cancel(r).unwrap(); } false
    }).is_err());
    assert!(matches!(readers.execute(r, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    atlases.open_search_reader(a, &readers, r, 1, 1, || false).unwrap();
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn search_does_not_hold_table_lock_or_block_other_atlases() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new();
    let a = fixture.open(&atlases); let b = fixture.open(&atlases);
    let mut checked = false;
    atlases.execute(a, find(1, "needle"), || {
        if !checked {
            checked = true;
            assert!(atlases.execute(b, Command::Info, || false).is_ok());
            assert!(matches!(atlases.execute(a, Command::Info, || false), Err(AccessError::Busy)));
        }
        false
    }).unwrap();
    atlases.close(a).unwrap(); atlases.close(b).unwrap();
}

#[test]
fn close_during_search_keeps_retiring_capacity_until_source_work_drains() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new();
    let a = fixture.open(&atlases);
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| atlases.create().unwrap()).collect();
    let mut fired = false;
    let result = atlases.execute(a, find(1, "needle"), || {
        if !fired {
            fired = true; atlases.close(a).unwrap();
            assert_eq!(atlases.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS);
            assert!(matches!(atlases.create(), Err(AccessError::Capacity)));
        }
        false
    });
    assert!(matches!(result, Err(AccessError::Closed)));
    assert_eq!(atlases.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS - 1);
    for handle in others { atlases.close(handle).unwrap(); }
    assert_eq!(atlases.budget.accounting().reserved().get(), 0);
}

#[test]
fn query_focus_and_clear_preserve_independent_reader_and_presentation_ownership() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap();
    atlases.execute(a, Command::Prepare { generation: 1, action: AtlasAction::View }, || false).unwrap();
    atlases.execute(a, Command::Present { generation: 1, frame: 1, display: 1 }, || false).unwrap();
    atlases.execute(a, find(1, "needle"), || false).unwrap();
    let focused = atlases.execute(a, Command::SearchFocus { generation: 1, hit: 1, plan_generation: 2 }, || false).unwrap();
    assert!(focused.as_str().contains("\"native_presented\":false"));
    assert!(atlases.execute(a, Command::Info, || false).unwrap().as_str().contains("\"presented_plan_generation\":\"1\""));
    atlases.open_search_reader(a, &readers, r, 1, 1, || false).unwrap();
    atlases.execute(a, Command::SearchClear { generation: 2 }, || false).unwrap();
    assert!(matches!(atlases.execute(a, Command::SearchOverlay { generation: 1 }, || false), Err(AccessError::Search(AtlasSearchError::MissingQuery))));
    assert!(readers.execute(r, ReaderCommand::Info, || false).is_ok());
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn independent_atlas_handles_never_share_captures_or_query_state() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new();
    let a = fixture.open(&atlases); let b = fixture.open(&atlases);
    atlases.execute(a, find(1, "needle"), || false).unwrap();
    assert!(matches!(atlases.execute(b, Command::SearchPage { generation: 1, start: 0, limit: 10 }, || false), Err(AccessError::Search(AtlasSearchError::MissingQuery))));
    let empty = atlases.execute(b, find(1, "absent"), || false).unwrap();
    assert!(empty.as_str().contains("\"retained_hits\":\"0\""));
    atlases.close(a).unwrap();
    assert!(atlases.execute(b, Command::SearchOverlay { generation: 1 }, || false).is_ok());
    atlases.close(b).unwrap();
}
