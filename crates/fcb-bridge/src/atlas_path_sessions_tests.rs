#![forbid(unsafe_code)]
use super::*;
use std::{fs, path::PathBuf};
use crate::reader_sessions::{AccessError as ReaderError, Command as ReaderCommand};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("fcb-path-registry-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap(); fs::write(root.join("a.rs"), b"old needle\n").unwrap();
        fs::write(root.join("b.rs"), b"other\n").unwrap(); Self(root)
    }
    fn open(&self, registry: &AtlasSessions) -> u64 {
        let handle = registry.create().unwrap(); registry.open(handle, &self.0, Default::default(), || false).unwrap(); handle
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn find(registry: &AtlasSessions, handle: u64, generation: u64, needle: &[u8]) -> HostResponse {
    registry.execute_paths(handle, PathCommand::Find { generation, needle, options: Default::default() }, || false).unwrap()
}
fn target(registry: &AtlasSessions, handle: u64, path: &[u8]) -> u64 {
    let cell = registry.get(handle).unwrap(); let state = lock(&cell.state).unwrap();
    let atlas = &state.as_ref().unwrap().atlas;
    let node = atlas.atlas().index().unwrap().find_path(path).unwrap(); atlas.atlas().file(node).unwrap().get()
}

#[test]
fn file_find_select_focus_and_reader_use_current_capture_then_keep_it_independent() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap();
    assert!(find(&atlases, a, 1, b"a.rs").as_str().contains("\"source_payload_read\":false"));
    let file = target(&atlases, a, b"a.rs");
    let selected = atlases.execute_paths(a, PathCommand::Select { generation: 1, file }, || false).unwrap();
    assert!(selected.as_str().contains("\"command\":\"select\""));
    let plan = atlases.execute_paths(a, PathCommand::Focus { generation: 1, file, plan_generation: 1 }, || false).unwrap();
    assert!(plan.as_str().contains("\"native_presented\":false"));
    fs::write(fixture.0.join("a.rs"), b"changed before activation\n").unwrap();
    let receipt = atlases.open_path_reader(a, &readers, r, 1, file, 1024, || false).unwrap();
    assert!(receipt.as_str().contains("new-capture-after-path-selection"));
    assert!(receipt.as_str().contains(&format!("\"reader_owner\":\"{r}\"")));
    atlases.execute_paths(a, PathCommand::Clear { generation: 2 }, || false).unwrap();
    atlases.close(a).unwrap(); fs::remove_file(fixture.0.join("a.rs")).unwrap();
    let window = readers.execute(r, ReaderCommand::Window { offset: 0, bytes: 128 }, || false).unwrap();
    assert!(window.as_str().contains("changed before activation")); readers.close(r).unwrap();
}

#[test]
fn path_queries_and_clear_do_not_cancel_or_overwrite_running_content_queries() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let a = fixture.open(&atlases);
    atlases.execute(a, Command::SearchBegin { generation: 1, needle: "needle", options: Default::default() }, || false).unwrap();
    atlases.execute(a, Command::SearchStep { generation: 1 }, || false).unwrap();
    // Independent generation 1 is legal. No content capture/search is dispatched.
    assert!(find(&atlases, a, 1, b"b.rs").as_str().contains("\"retained_hits\":\"1\""));
    atlases.execute_paths(a, PathCommand::Clear { generation: 2 }, || false).unwrap();
    let page = atlases.execute(a, Command::SearchPage { generation: 1, start: 0, limit: 10 }, || false).unwrap();
    assert!(page.as_str().contains("\"needle\":\"needle\""));
    assert!(page.as_str().contains("\"retained_hits\":\"1\""));
    atlases.execute(a, Command::SearchStep { generation: 1 }, || false).unwrap();
    atlases.close(a).unwrap();
}

#[test]
fn stale_query_or_unpublished_file_never_installs_a_reader() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap();
    let file_a = target(&atlases, a, b"a.rs"); let file_b = target(&atlases, a, b"b.rs");
    find(&atlases, a, 1, b"a.rs"); find(&atlases, a, 2, b"b.rs");
    assert!(matches!(atlases.open_path_reader(a, &readers, r, 1, file_a, 1024, || false), Err(AccessError::Paths(AtlasPathError::StaleQuery))));
    assert!(matches!(atlases.open_path_reader(a, &readers, r, 2, file_a, 1024, || false), Err(AccessError::Paths(AtlasPathError::MissingHit))));
    assert!(matches!(readers.execute(r, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    atlases.open_path_reader(a, &readers, r, 2, file_b, 1024, || false).unwrap();
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn existing_reader_refusal_precedes_any_new_source_lookup() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap(); find(&atlases, a, 1, b"a.rs");
    let file = target(&atlases, a, b"a.rs"); atlases.open_path_reader(a, &readers, r, 1, file, 1024, || false).unwrap();
    fs::remove_file(fixture.0.join("a.rs")).unwrap();
    assert!(matches!(atlases.open_path_reader(a, &readers, r, 1, file, 1024, || false), Err(AccessError::Reader(ReaderError::AlreadyOpen))));
    assert!(readers.execute(r, ReaderCommand::Window { offset: 0, bytes: 64 }, || false).unwrap().as_str().contains("old needle"));
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn reader_only_cancellation_preserves_file_results_and_empty_destination_is_retryable() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let r = readers.create().unwrap(); find(&atlases, a, 1, b"a.rs");
    let file = target(&atlases, a, b"a.rs"); let mut fired = false;
    assert!(atlases.open_path_reader(a, &readers, r, 1, file, 1024, || {
        if !fired { fired = true; readers.cancel(r).unwrap(); } false
    }).is_err());
    assert!(matches!(readers.execute(r, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    assert!(atlases.execute_paths(a, PathCommand::Page { generation: 1, start: 0, limit: 10 }, || false).is_ok());
    atlases.open_path_reader(a, &readers, r, 1, file, 1024, || false).unwrap();
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn canceled_replacement_and_one_shot_cancel_pulse_cannot_publish_partial_file_results() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let a = fixture.open(&atlases);
    find(&atlases, a, 1, b"a.rs");
    let file = target(&atlases, a, b"a.rs");
    atlases.execute_paths(a, PathCommand::Select { generation: 1, file }, || false).unwrap();
    // First callback is host admission; second is the path engine's step. The
    // engine latches cancellation even when every subsequent poll returns false.
    let mut polls = 0;
    let result = atlases.execute_paths(a, PathCommand::Find { generation: 2, needle: b"rs", options: Default::default() }, || {
        polls += 1; polls == 2
    });
    assert!(result.is_err()); assert!(polls >= 2);
    let page = atlases.execute_paths(a, PathCommand::Page { generation: 1, start: 0, limit: 10 }, || false).unwrap();
    assert!(page.as_str().contains(&format!("\"selection\":{{\"file_id\":\"{file}\"")));
    let mut fired = false;
    assert!(atlases.execute_paths(a, PathCommand::Find { generation: 3, needle: b"b.rs", options: Default::default() }, || {
        if !fired { fired = true; atlases.cancel(a).unwrap(); } false
    }).is_err());
    assert!(atlases.execute_paths(a, PathCommand::Page { generation: 1, start: 0, limit: 10 }, || false).is_ok());
    atlases.close(a).unwrap();
}

#[test]
fn busy_and_wrong_kind_handles_do_not_interfere_with_another_atlas() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let b = fixture.open(&atlases); let r = readers.create().unwrap();
    let cell = atlases.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert!(matches!(atlases.execute_paths(a, PathCommand::Find { generation: 1, needle: b"a", options: Default::default() }, || false), Err(AccessError::Busy)));
    assert!(find(&atlases, b, 1, b"b").as_str().contains("\"status\":\"ok\""));
    assert!(matches!(atlases.execute_paths(r, PathCommand::Clear { generation: 1 }, || false), Err(AccessError::Unknown)));
    atlases.cancel(a).unwrap(); drop(guard); drop(cell);
    find(&atlases, a, 1, b"a");
    readers.close(r).unwrap(); atlases.close(a).unwrap(); atlases.close(b).unwrap();
}

#[test]
fn active_close_retains_capacity_until_path_work_drains() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let a = fixture.open(&atlases);
    let other: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| atlases.create().unwrap()).collect();
    let mut fired = false;
    let result = atlases.execute_paths(a, PathCommand::Find { generation: 1, needle: b"a", options: Default::default() }, || {
        if !fired {
            fired = true; atlases.close(a).unwrap();
            assert_eq!(atlases.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS);
            assert!(matches!(atlases.create(), Err(AccessError::Capacity)));
        }
        false
    });
    assert!(matches!(result, Err(AccessError::Closed)));
    assert_eq!(atlases.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS - 1);
    let next = atlases.create().unwrap(); assert_ne!(next, a); atlases.close(next).unwrap();
    for id in other { atlases.close(id).unwrap(); }
    assert_eq!(atlases.budget.accounting().reserved().get(), 0);
}

#[test]
fn deleted_sources_do_not_prevent_metadata_find_or_reuse_of_prepared_keys() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let a = fixture.open(&atlases);
    fs::remove_file(fixture.0.join("a.rs")).unwrap();
    assert!(find(&atlases, a, 1, b"a.rs").as_str().contains("\"retained_hits\":\"1\""));
    let backing = {
        let cell = atlases.get(a).unwrap(); let state = lock(&cell.state).unwrap();
        state.as_ref().unwrap().paths.prepared_index().unwrap().paths().next().unwrap().raw_path().as_bytes().as_ptr()
    };
    find(&atlases, a, 2, b"b.rs");
    let cell = atlases.get(a).unwrap(); let state = lock(&cell.state).unwrap();
    assert_eq!(state.as_ref().unwrap().paths.prepared_index().unwrap().paths().next().unwrap().raw_path().as_bytes().as_ptr(), backing);
    drop(state); drop(cell); atlases.close(a).unwrap();
}
