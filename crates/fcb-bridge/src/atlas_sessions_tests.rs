#![forbid(unsafe_code)]

use super::*;
use std::{fs, path::PathBuf};
use super::super::reader_sessions::{AccessError as ReaderError, Command as ReaderCommand};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("fcb-atlas-registry-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&path).unwrap(); fs::write(path.join("a.rs"), b"old needle\n").unwrap(); Self(path)
    }
    fn open(&self, registry: &AtlasSessions) -> u64 {
        let handle = registry.create().unwrap();
        registry.open(handle, &self.0, Default::default(), || false).unwrap(); handle
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn show(registry: &AtlasSessions, handle: u64) -> Point2D {
    registry.execute(handle, Command::Prepare { generation: 1, action: AtlasAction::View }, || false).unwrap();
    let point = {
        let cell = registry.get(handle).unwrap(); let state = lock(&cell.state).unwrap();
        let plan = state.as_ref().unwrap().pending_plan().unwrap();
        let parcel = plan.parcels().iter().find(|p| p.detail() == fcb_map::AtlasDetail::File).unwrap();
        let rect = parcel.logical_rect();
        Point2D::new(rect.min_x() + rect.size().width() / 2.0, rect.min_y() + rect.size().height() / 2.0).unwrap()
    };
    registry.execute(handle, Command::Present { generation: 1, frame: 1, display: 1 }, || false).unwrap();
    point
}

#[test]
fn native_atlas_to_existing_reader_registry_keeps_exact_source() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let handle = fixture.open(&atlases); let point = show(&atlases, handle); let reader = readers.create().unwrap();
    assert_ne!(handle, reader);
    let receipt = atlases.open_reader(handle, &readers, reader, 1, 1, point, 1024, || false).unwrap();
    assert!(receipt.as_str().contains(&format!("\"reader_owner\":\"{reader}\"")));
    assert!(receipt.as_str().contains("\"command\":\"open-reader\""));
    fs::write(fixture.0.join("a.rs"), b"new working tree").unwrap();
    let found = readers.execute(reader, ReaderCommand::Find { generation: 1, needle: "needle", limit: 10, scan_bytes: 1024 }, || false).unwrap();
    assert!(found.as_str().contains("\"retained_hits\":\"1\""));
    atlases.close(handle).unwrap();
    let copied = readers.execute(reader, ReaderCommand::CopyHit { generation: 1, index: 0 }, || false).unwrap();
    assert!(copied.as_str().contains("6e6565646c65"));
    readers.close(reader).unwrap();
}

#[test]
fn wrong_kind_handles_cannot_alias_the_other_registry() {
    let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = atlases.create().unwrap(); let r = readers.create().unwrap();
    assert_ne!(a, r);
    assert!(matches!(atlases.execute(r, Command::Info, || false), Err(AccessError::Unknown)));
    assert!(matches!(readers.execute(a, ReaderCommand::Info, || false), Err(ReaderError::UnknownHandle)));
    atlases.close(a).unwrap(); readers.close(r).unwrap();
    let next = atlases.create().unwrap(); assert_ne!(next, a); assert_ne!(next, r); atlases.close(next).unwrap();
}

#[test]
fn loaded_reader_cannot_be_replaced_by_another_atlas_activation() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let point = show(&atlases, a); let r = readers.create().unwrap();
    atlases.open_reader(a, &readers, r, 1, 1, point, 1024, || false).unwrap();
    fs::write(fixture.0.join("a.rs"), b"changed").unwrap();
    assert!(matches!(atlases.open_reader(a, &readers, r, 1, 1, point, 1024, || false), Err(AccessError::Reader(ReaderError::AlreadyOpen))));
    assert!(readers.execute(r, ReaderCommand::Window { offset: 0, bytes: 100 }, || false).unwrap().as_str().contains("old needle"));
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn stale_frame_failure_leaves_empty_reader_destination_retryable() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let point = show(&atlases, a); let r = readers.create().unwrap();
    assert!(matches!(atlases.open_reader(a, &readers, r, 9, 1, point, 1024, || false), Err(AccessError::Atlas(AtlasSessionError::StaleFrame))));
    assert!(matches!(readers.execute(r, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    atlases.open_reader(a, &readers, r, 1, 1, point, 1024, || false).unwrap();
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}

#[test]
fn cancel_initialization_then_retry_does_not_publish_a_partial_catalog() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let a = atlases.create().unwrap();
    let mut fired = false;
    assert!(atlases.open(a, &fixture.0, Default::default(), || {
        if !fired { fired = true; atlases.cancel(a).unwrap(); } false
    }).is_err());
    assert!(matches!(atlases.execute(a, Command::Info, || false), Err(AccessError::NotOpen)));
    atlases.open(a, &fixture.0, Default::default(), || false).unwrap();
    assert!(matches!(atlases.open(a, &fixture.0, Default::default(), || false), Err(AccessError::AlreadyOpen)));
    atlases.close(a).unwrap();
}

#[test]
fn busy_atlas_never_blocks_an_independent_session_or_cancellation() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new();
    let a = fixture.open(&atlases); let b = fixture.open(&atlases);
    let cell = atlases.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert!(matches!(atlases.execute(a, Command::Info, || false), Err(AccessError::Busy)));
    assert!(atlases.execute(b, Command::Info, || false).is_ok());
    atlases.cancel(a).unwrap();
    drop(guard); drop(cell);
    assert!(atlases.execute(a, Command::Info, || false).is_ok());
    atlases.close(a).unwrap(); atlases.close(b).unwrap();
}

#[test]
fn active_close_keeps_admission_until_the_old_operation_drains() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new();
    let ids: Vec<_> = (0..MAX_ATLAS_SESSIONS).map(|_| atlases.create().unwrap()).collect();
    let mut fired = false;
    let result = atlases.open(ids[0], &fixture.0, Default::default(), || {
        if !fired {
            fired = true; atlases.close(ids[0]).unwrap();
            assert_eq!(atlases.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS);
            assert!(matches!(atlases.create(), Err(AccessError::Capacity)));
        }
        false
    });
    assert!(matches!(result, Err(AccessError::Closed)));
    assert_eq!(atlases.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS - 1);
    let next = atlases.create().unwrap(); assert_ne!(next, ids[0]); atlases.close(next).unwrap();
    for id in &ids[1..] { atlases.close(*id).unwrap(); }
    assert_eq!(atlases.live.load(Ordering::Acquire), 0);
    assert_eq!(atlases.budget.accounting().reserved().get(), 0);
}

#[test]
fn poisoned_operation_can_be_canceled_and_closed_without_other_session_damage() {
    let atlases = AtlasSessions::new(); let a = atlases.create().unwrap(); let b = atlases.create().unwrap();
    let cell = atlases.get(a).unwrap();
    assert!(std::panic::catch_unwind(|| { let _guard = cell.state.lock().unwrap(); panic!("injected worker failure"); }).is_err());
    assert!(matches!(atlases.execute(a, Command::Info, || false), Err(AccessError::Poisoned)));
    atlases.cancel(a).unwrap(); atlases.close(a).unwrap(); drop(cell);
    assert!(matches!(atlases.execute(b, Command::Info, || false), Err(AccessError::NotOpen)));
    atlases.close(b).unwrap(); assert_eq!(atlases.live.load(Ordering::Acquire), 0);
}

#[test]
fn canceled_plan_preserves_previously_acknowledged_geometry() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let a = fixture.open(&atlases); let point = show(&atlases, a);
    let result = atlases.execute(a, Command::Prepare { generation: 2, action: AtlasAction::View }, || true);
    assert!(result.is_err());
    assert!(atlases.execute(a, Command::Pick { frame: 1, display: 1, point }, || false).unwrap().as_str().contains("\"can_open_source\":true"));
    atlases.close(a).unwrap();
}

#[test]
fn cancel_reader_during_joint_initialization_does_not_install_capture() {
    let fixture = Fixture::new(); let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let a = fixture.open(&atlases); let point = show(&atlases, a); let r = readers.create().unwrap();
    let mut fired = false;
    let result = atlases.open_reader(a, &readers, r, 1, 1, point, 1024, || {
        if !fired { fired = true; readers.cancel(r).unwrap(); } false
    });
    assert!(result.is_err());
    assert!(matches!(readers.execute(r, ReaderCommand::Info, || false), Err(ReaderError::NotOpen)));
    atlases.open_reader(a, &readers, r, 1, 1, point, 1024, || false).unwrap();
    readers.close(r).unwrap(); atlases.close(a).unwrap();
}
