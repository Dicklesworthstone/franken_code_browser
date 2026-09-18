#![forbid(unsafe_code)]

use super::*;
use std::{fs, path::PathBuf};
use crate::reader_sessions::{AccessError as ReaderError, Command as ReaderCommand};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-progressive-registry-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.rs"), b"needle old\n").unwrap();
        fs::write(root.join("b.rs"), b"needle second\n").unwrap(); Self(root)
    }
    fn open(&self, registry: &AtlasSessions) -> u64 {
        let handle = registry.create().unwrap();
        registry.open(handle, &self.0, Default::default(), || false).unwrap(); handle
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 10, max_files: 10, max_file_bytes: 128, max_source_bytes: 1024 }
}
fn begin(registry: &AtlasSessions, handle: u64, generation: u64) {
    registry.execute(handle, Command::SearchBegin { generation, needle: "needle", options: options() }, || false).unwrap();
}
fn step(registry: &AtlasSessions, handle: u64, generation: u64) -> HostResponse {
    registry.execute(handle, Command::SearchStep { generation }, || false).unwrap()
}
fn progress(registry: &AtlasSessions, handle: u64, generation: u64)
    -> fcb_app::host::atlas_search::AtlasSearchProgress {
    let cell = registry.get(handle).unwrap(); let state = lock(&cell.state).unwrap();
    let session = state.as_ref().unwrap();
    session.search.progress(&session.atlas, generation).unwrap()
}

#[test]
fn partial_results_can_open_readers_and_interleave_camera_before_completion() {
    let f = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = f.open(&atlases); let r = readers.create().unwrap();
    atlases.execute(a, Command::Prepare { generation: 1, action: AtlasAction::View }, || false).unwrap();
    atlases.execute(a, Command::Present { generation: 1, frame: 1, display: 1 }, || false).unwrap();
    begin(&atlases, a, 10); assert_eq!(progress(&atlases, a, 10).source_bytes_read, 0);
    assert!(step(&atlases, a, 10).as_str().contains("\"search_in_progress\":true"));
    fs::write(f.0.join("a.rs"), b"changed now").unwrap();
    let partial = atlases.open_search_reader(a, &readers, r, 10, 1, || false).unwrap();
    assert!(partial.as_str().contains("\"source_reopened\":false"));
    atlases.execute(a, Command::Prepare { generation: 2, action: AtlasAction::Pan(Point2D::new(10000.0, 10000.0).unwrap()) }, || false).unwrap();
    let info = atlases.execute(a, Command::Info, || false).unwrap();
    assert!(info.as_str().contains("\"presented_plan_generation\":\"1\""));
    assert!(atlases.execute(a, Command::SearchOverlay { generation: 10 }, || false).unwrap().as_str().contains("\"search_in_progress\":true"));
    assert!(step(&atlases, a, 10).as_str().contains("\"search_complete\":true"));
    atlases.close(a).unwrap();
    assert!(readers.execute(r, ReaderCommand::Window { offset: 0, bytes: 64 }, || false).unwrap().as_str().contains("needle old"));
    readers.close(r).unwrap();
}

#[test]
fn cancel_between_steps_invalidates_provisional_rows_but_not_finished_query() {
    let f = Fixture::new(); let atlases = AtlasSessions::new(); let a = f.open(&atlases);
    atlases.execute(a, Command::Search { generation: 1, needle: "needle", options: options() }, || false).unwrap();
    begin(&atlases, a, 2); step(&atlases, a, 2);
    atlases.cancel(a).unwrap();
    assert!(atlases.execute(a, Command::SearchPage { generation: 2, start: 0, limit: 10 }, || false).is_err());
    assert!(matches!(atlases.execute(a, Command::SearchStep { generation: 2 }, || false),
        Err(AccessError::Search(AtlasSearchError::StaleQuery))));
    assert!(atlases.execute(a, Command::SearchPage { generation: 1, start: 0, limit: 10 }, || false).unwrap().as_str().contains("\"retained_hits\":\"2\""));
    begin(&atlases, a, 3); assert_eq!(progress(&atlases, a, 3).examined_files, 0);
    step(&atlases, a, 3); assert_eq!(progress(&atlases, a, 3).examined_files, 1);
    atlases.close(a).unwrap();
}

#[test]
fn cancel_during_step_never_leaves_a_resumable_old_epoch_job() {
    let f = Fixture::new(); let atlases = AtlasSessions::new(); let a = f.open(&atlases);
    begin(&atlases, a, 1);
    let mut fired = false;
    let result = atlases.execute(a, Command::SearchStep { generation: 1 }, || {
        if !fired { fired = true; atlases.cancel(a).unwrap(); } false
    });
    assert!(fired); assert!(matches!(result, Err(AccessError::Canceled)));
    assert!(atlases.execute(a, Command::SearchStep { generation: 1 }, || false).is_err());
    let cell = atlases.get(a).unwrap();
    assert_eq!(lock(&cell.state).unwrap().as_ref().unwrap().search.pending_generation(), None);
    drop(cell); begin(&atlases, a, 2); step(&atlases, a, 2);
    atlases.close(a).unwrap();
}

#[test]
fn reader_only_cancel_does_not_discard_shared_partial_search() {
    let f = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = f.open(&atlases); let r = readers.create().unwrap(); begin(&atlases, a, 1); step(&atlases, a, 1);
    let before = progress(&atlases, a, 1); let mut fired = false;
    assert!(atlases.open_search_reader(a, &readers, r, 1, 1, || {
        if !fired { fired = true; readers.cancel(r).unwrap(); } false
    }).is_err());
    assert!(matches!(readers.execute(r, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    assert_eq!(progress(&atlases, a, 1), before);
    atlases.open_search_reader(a, &readers, r, 1, 1, || false).unwrap();
    step(&atlases, a, 1); readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn stale_steps_do_not_advance_newer_pending_queries() {
    let f = Fixture::new(); let atlases = AtlasSessions::new(); let a = f.open(&atlases);
    begin(&atlases, a, 1); step(&atlases, a, 1); begin(&atlases, a, 2);
    let before = progress(&atlases, a, 2);
    assert!(atlases.execute(a, Command::SearchStep { generation: 1 }, || false).is_err());
    assert_eq!(progress(&atlases, a, 2), before);
    assert!(atlases.execute(a, Command::SearchFocus { generation: 1, hit: 1, plan_generation: 1 }, || false).is_err());
    step(&atlases, a, 2); step(&atlases, a, 2);
    let end = progress(&atlases, a, 2); atlases.cancel(a).unwrap();
    // Completed results survive cancellation; terminal replay does not reread.
    assert!(step(&atlases, a, 2).as_str().contains("\"search_in_progress\":false"));
    assert_eq!(progress(&atlases, a, 2), end); atlases.close(a).unwrap();
}

#[test]
fn busy_step_does_not_block_another_atlas_or_cancel_and_resume_old_work() {
    let f = Fixture::new(); let atlases = AtlasSessions::new();
    let a = f.open(&atlases); let b = f.open(&atlases); begin(&atlases, a, 1);
    let cell = atlases.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert!(matches!(atlases.execute(a, Command::SearchStep { generation: 1 }, || false), Err(AccessError::Busy)));
    assert!(atlases.execute(b, Command::Info, || false).is_ok());
    atlases.cancel(a).unwrap(); drop(guard); drop(cell);
    assert!(atlases.execute(a, Command::SearchStep { generation: 1 }, || false).is_err());
    assert!(atlases.execute(a, Command::Info, || false).is_ok());
    atlases.close(a).unwrap(); atlases.close(b).unwrap();
}

#[test]
fn closing_pending_capture_keeps_retiring_capacity_until_the_last_owner_drops() {
    let f = Fixture::new(); let atlases = AtlasSessions::new(); let a = f.open(&atlases);
    begin(&atlases, a, 1); step(&atlases, a, 1);
    let held = atlases.get(a).unwrap();
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| atlases.create().unwrap()).collect();
    atlases.close(a).unwrap();
    assert!(matches!(atlases.create(), Err(AccessError::Capacity)));
    assert_eq!(atlases.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS);
    drop(held);
    let next = atlases.create().unwrap(); assert_ne!(next, a); atlases.close(next).unwrap();
    for id in others { atlases.close(id).unwrap(); }
    assert_eq!(atlases.live.load(Ordering::Acquire), 0);
    assert_eq!(atlases.budget.accounting().reserved().get(), 0);
}

#[test]
fn independently_progressing_atlases_do_not_share_cancel_or_query_state() {
    let f = Fixture::new(); let atlases = AtlasSessions::new();
    let a = f.open(&atlases); let b = f.open(&atlases); begin(&atlases, a, 1); begin(&atlases, b, 1);
    step(&atlases, a, 1); step(&atlases, b, 1);
    let before = progress(&atlases, b, 1); atlases.cancel(a).unwrap();
    assert!(atlases.execute(a, Command::SearchPage { generation: 1, start: 0, limit: 10 }, || false).is_err());
    assert_eq!(progress(&atlases, b, 1), before);
    step(&atlases, b, 1); assert!(progress(&atlases, b, 1).complete);
    atlases.close(a).unwrap(); atlases.close(b).unwrap();
}
