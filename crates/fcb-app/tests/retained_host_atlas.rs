#![cfg(unix)]
#![forbid(unsafe_code)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, Point2D};
use fcb::map::{AtlasError, VisibleLimits};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::atlas_session::{AtlasAction, AtlasSession, AtlasSessionError, AtlasSessionOptions};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-host-atlas-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join("src/a.rs"), b"original needle\n").unwrap();
        fs::write(root.join("src/b.rs"), b"b\n").unwrap();
        fs::write(root.join("README.md"), b"# hello\n").unwrap();
        Self(root)
    }
    fn open(&self) -> AtlasSession { self.options(AtlasSessionOptions::default()) }
    fn options(&self, options: AtlasSessionOptions) -> AtlasSession {
        AtlasSession::open(ArenaOwnerId::new(601).unwrap(), &self.0, options, || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn point(x: f64, y: f64) -> Point2D { Point2D::new(x, y).unwrap() }
fn focus(session: &mut AtlasSession, generation: u64, path: &[u8]) -> Point2D {
    let ordinal = session.atlas().index().unwrap().find_path(path).unwrap().ordinal();
    session.prepare(generation, AtlasAction::Focus(ordinal), || false).unwrap();
    let rect = session.pending_plan().unwrap().parcels()[0].logical_rect();
    point(rect.min_x() + rect.size().width() / 2.0, rect.min_y() + rect.size().height() / 2.0)
}
fn near(a: f64, b: f64) { assert!((a - b).abs() < 1e-7, "{a} != {b}"); }

#[test]
fn opened_map_and_tree_survive_live_directory_rename_without_rediscovery() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    let old_nodes = session.atlas().layout().nodes().as_ptr();
    let moved = fixture.0.join("moved");
    fs::rename(fixture.0.join("src"), &moved).unwrap();
    fs::write(fixture.0.join("new.rs"), b"new\n").unwrap();
    for generation in 1..=10 {
        let reply = session.prepare(generation, AtlasAction::View, || false).unwrap();
        assert!(reply.as_str().contains("\"catalogued_files\":\"3\""));
        assert_eq!(session.atlas().layout().nodes().as_ptr(), old_nodes);
    }
    let index = session.atlas().index().unwrap();
    assert!(index.find_path(b"src/a.rs").is_some());
    assert!(index.find_path(b"new.rs").is_none());
    let src = index.find_path(b"src").unwrap().ordinal(); drop(index);
    let rows = session.children(src, 0, 10, || false).unwrap();
    assert!(rows.as_str().contains("src/a.rs"));
}

#[test]
fn unacknowledged_camera_does_not_replace_the_pick_frame() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    let at = focus(&mut session, 1, b"src/a.rs");
    assert!(matches!(session.pick(1, 1, at, || false), Err(AtlasSessionError::NoPresentedPlan)));
    session.acknowledge(1, 1, 1, || false).unwrap();
    let before = session.pick(1, 1, at, || false).unwrap();
    session.prepare(2, AtlasAction::Pan(point(10000.0, 10000.0)), || false).unwrap();
    assert!(session.pending_plan().unwrap().parcels().is_empty());
    assert_eq!(session.pick(1, 1, at, || false).unwrap().as_str(), before.as_str());
    session.acknowledge(2, 2, 1, || false).unwrap();
    assert!(session.pick(2, 1, at, || false).unwrap().as_str().contains("\"hit\":null"));
    assert!(matches!(session.pick(1, 1, at, || false), Err(AtlasSessionError::StaleFrame)));
}

#[test]
fn superseded_candidates_and_wrong_display_cannot_be_acknowledged() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    session.prepare(1, AtlasAction::View, || false).unwrap();
    session.prepare(2, AtlasAction::View, || false).unwrap();
    assert!(matches!(session.acknowledge(1, 1, 1, || false), Err(AtlasSessionError::NoPendingPlan)));
    assert!(matches!(session.acknowledge(2, 1, 9, || false), Err(AtlasSessionError::Atlas(AtlasError::DisplayMismatch))));
    session.acknowledge(2, 1, 1, || false).unwrap();
    session.prepare(3, AtlasAction::View, || false).unwrap();
    // An explicitly re-presented accepted plan is allowed; the newer candidate stays pending.
    session.acknowledge(2, 2, 1, || false).unwrap();
    assert_eq!(session.pending_plan().unwrap().generation().get(), 3);
    assert_eq!(session.presented_plan().unwrap().generation().get(), 2);
    assert!(matches!(session.acknowledge(3, 2, 1, || false), Err(AtlasSessionError::StaleFrame)));
}

#[test]
fn anchored_zoom_and_back_restore_focus_without_repacking() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    session.prepare(1, AtlasAction::View, || false).unwrap();
    let old = session.pending_plan().unwrap().camera(); let root = session.pending_plan().unwrap().focus();
    let anchor = point(100.25, 231.75); let local = old.logical_to_local(anchor).unwrap();
    session.prepare(2, AtlasAction::Zoom { anchor, factor: 2.0 }, || false).unwrap();
    let zoomed = session.pending_plan().unwrap().camera(); let mapped = zoomed.local_to_logical(local).unwrap();
    near(mapped.x(), anchor.x()); near(mapped.y(), anchor.y());
    focus(&mut session, 3, b"src/a.rs");
    session.prepare(4, AtlasAction::Back, || false).unwrap();
    let back = session.pending_plan().unwrap();
    assert_eq!(back.focus(), root); assert_eq!(back.camera().origin(), zoomed.origin());
    assert_eq!(back.camera().points_per_unit(), zoomed.points_per_unit());
    assert_eq!(back.camera().generation().get(), 4);
    let info = session.info(|| false).unwrap();
    assert!(info.as_str().contains("\"history_depth\":\"0\""));
    assert!(info.as_str().contains("src/a.rs")); // Selection is independent from focus.
    assert!(matches!(session.prepare(5, AtlasAction::Back, || false), Err(AtlasSessionError::EmptyHistory)));
}

#[test]
fn failed_and_canceled_replacements_preserve_candidate_and_consume_attempts() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    focus(&mut session, 1, b"src/a.rs");
    let camera = session.pending_plan().unwrap().camera();
    assert!(session.prepare(2, AtlasAction::Zoom { anchor: point(0.0, 0.0), factor: f64::NAN }, || false).is_err());
    assert!(matches!(session.prepare(3, AtlasAction::View, || true), Err(AtlasSessionError::Canceled)));
    assert_eq!(session.pending_plan().unwrap().camera(), camera);
    assert_eq!(session.pending_plan().unwrap().generation().get(), 1);
    assert!(matches!(session.prepare(3, AtlasAction::View, || false), Err(AtlasSessionError::StaleGeneration)));
    session.prepare(4, AtlasAction::View, || false).unwrap();
}

#[test]
fn display_changes_are_separate_and_failed_display_id_is_not_reused() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    let at = focus(&mut session, 1, b"src/a.rs");
    session.acknowledge(1, 1, 1, || false).unwrap();
    assert!(session.prepare(2, AtlasAction::Resize { width: 0.0, height: 768.0, scale: 1.0 }, || false).is_err());
    session.prepare(3, AtlasAction::Resize { width: 800.0, height: 600.0, scale: 2.0 }, || false).unwrap();
    assert_eq!(session.pending_plan().unwrap().camera().display().generation().get(), 3);
    assert!(session.pick(1, 1, at, || false).is_ok());
    assert!(matches!(session.pick(1, 3, at, || false), Err(AtlasSessionError::Atlas(AtlasError::DisplayMismatch))));
    session.acknowledge(3, 2, 3, || false).unwrap();
    assert!(matches!(session.pick(2, 1, at, || false), Err(AtlasSessionError::Atlas(AtlasError::DisplayMismatch))));
}

#[test]
fn aggregates_cannot_open_unseen_source_and_edges_are_half_open() {
    let fixture = Fixture::new();
    let mut session = fixture.options(AtlasSessionOptions { visible: VisibleLimits { max_items: 1, max_visits: 1 }, ..Default::default() });
    session.prepare(1, AtlasAction::View, || false).unwrap();
    let rect = session.pending_plan().unwrap().parcels()[0].logical_rect();
    session.acknowledge(1, 1, 1, || false).unwrap();
    let inside = point(rect.min_x() + 1.0, rect.min_y() + 1.0);
    assert!(session.pick(1, 1, inside, || false).unwrap().as_str().contains("\"can_open_source\":false"));
    assert!(matches!(session.open_reader(ArenaOwnerId::new(602).unwrap(), 1, 1, inside, 1024, || false), Err(AtlasSessionError::NoFile)));
    assert!(session.pick(1, 1, point(rect.max_x(), rect.min_y()), || false).unwrap().as_str().contains("\"hit\":null"));
}

#[test]
fn picked_file_opens_once_then_reader_keeps_exact_old_source() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    let at = focus(&mut session, 1, b"src/a.rs");
    session.acknowledge(1, 1, 1, || false).unwrap();
    let (mut reader, receipt) = session.open_reader(ArenaOwnerId::new(602).unwrap(), 1, 1, at, 1024, || false).unwrap();
    assert!(receipt.as_str().contains("\"reader_owner\":\"602\""));
    assert!(receipt.as_str().contains("new-capture-after-metadata-selection"));
    assert_eq!(reader.capture().bytes(), b"original needle\n");
    fs::write(fixture.0.join("src/a.rs"), b"changed\n").unwrap();
    reader.search(1, "needle", 10, 1024, || false).unwrap();
    assert!(reader.copy_hit(1, 0, || false).unwrap().as_str().contains("6e6565646c65"));
    // Closing the map cannot mutate a separately owned capture already delivered.
    drop(session);
    assert_eq!(reader.capture().bytes(), b"original needle\n");
}

#[test]
fn source_open_is_new_observation_not_a_metadata_capture() {
    let fixture = Fixture::new(); let mut session = fixture.open();
    let at = focus(&mut session, 1, b"src/a.rs");
    fs::write(fixture.0.join("src/a.rs"), b"changed before source open\n").unwrap();
    session.acknowledge(1, 1, 1, || false).unwrap();
    let (reader, _) = session.open_reader(ArenaOwnerId::new(602).unwrap(), 1, 1, at, 1024, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"changed before source open\n");
    assert!(matches!(session.open_reader(ArenaOwnerId::new(601).unwrap(), 1, 1, at, 1024, || false), Err(AtlasSessionError::Atlas(AtlasError::OwnerMismatch))));
}

#[test]
fn tree_pages_preserve_raw_path_identity_and_explicit_continuation() {
    use std::os::unix::ffi::OsStringExt;
    let fixture = Fixture::new();
    fs::write(fixture.0.join("src").join(std::ffi::OsString::from_vec(vec![0xff])), b"raw").unwrap();
    let mut session = fixture.open();
    let src = session.atlas().index().unwrap().find_path(b"src").unwrap().ordinal();
    let first = session.children(src, 0, 1, || false).unwrap();
    assert_eq!(first.exit_code(), EXIT_PARTIAL); assert!(first.as_str().contains("\"next_offset\":\"1\""));
    let rest = session.children(src, 1, 10, || false).unwrap();
    assert_eq!(rest.exit_code(), EXIT_OK); assert!(rest.as_str().contains("\"next_offset\":null"));
    assert!(rest.as_str().contains("7372632fff"));
}

#[test]
fn partial_catalog_and_giant_payload_are_not_hidden_or_read_as_whole_source() {
    let fixture = Fixture::new();
    fs::OpenOptions::new().write(true).open(fixture.0.join("src/a.rs")).unwrap().set_len(5 * 1024 * 1024 * 1024).unwrap();
    let mut complete = fixture.open();
    assert_eq!(complete.atlas().file_count(), 3);
    assert!(complete.prepare(1, AtlasAction::View, || false).is_ok());
    let mut partial = fixture.options(AtlasSessionOptions { max_files: 1, ..Default::default() });
    let reply = partial.prepare(1, AtlasAction::View, || false).unwrap();
    assert_eq!(reply.exit_code(), EXIT_PARTIAL);
    assert!(reply.as_str().contains("\"discovery_complete\":false"));
}

#[test]
fn cancellation_and_root_refusal_do_not_construct_usable_sessions() {
    let fixture = Fixture::new(); let owner = ArenaOwnerId::new(601).unwrap();
    assert!(matches!(AtlasSession::open(owner, &fixture.0, Default::default(), || true), Err(AtlasSessionError::Canceled)));
    assert!(AtlasSession::open(owner, std::path::Path::new(""), Default::default(), || false).is_err());
    let link = fixture.0.join("link"); std::os::unix::fs::symlink(fixture.0.join("src"), &link).unwrap();
    assert!(matches!(AtlasSession::open(owner, &link, Default::default(), || false), Err(AtlasSessionError::App(fcb_app::AppError::Symlink))));
}
