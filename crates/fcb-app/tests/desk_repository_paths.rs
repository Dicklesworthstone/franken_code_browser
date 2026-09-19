#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use support::{Json, parse};
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, FileId};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, atlas_paths::{AtlasPathOptions, AtlasPathError, PathCase, PathMatchMode},
    atlas_session::AtlasSessionOptions, atlas_search::AtlasSearchOptions,
    desk::{DeskSession, DeskCommand, DeskLimits, DeskSessionError, DeskError,
        document::{DeskDocument, DeskDocumentOptions}, repository::{DeskRepository, DeskRepositoryError}}};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-desk-paths-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn repo(root: &Path) -> DeskRepository { DeskRepository::open(owner(501), root, AtlasSessionOptions::default(), || false).unwrap() }
fn desk(n: u64) -> DeskSession { DeskSession::new(owner(n), DeskLimits::default()).unwrap() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn options() -> AtlasPathOptions { AtlasPathOptions { mode: PathMatchMode::Exact, case: PathCase::Sensitive, max_results: 100 } }
fn find(repo: &mut DeskRepository, generation: u64, needle: &[u8]) -> FileId {
    let page = repo.find_paths(generation, needle, options(), || false).unwrap();
    assert_eq!(page.exit_code(), EXIT_OK); let page = json(&page);
    assert!(!page.get("source_payload_read").flag());
    FileId::new(repo.owner(), page.get("hits").array()[0].get("file_id").number()).unwrap()
}
fn apply(desk: &mut DeskSession, command: DeskCommand) {
    desk.apply(desk.model().revision(), desk.model().last_attempt() + 1, command, || false).unwrap();
}

#[test]
fn filename_navigation_and_content_hit_activation_have_distinct_source_semantics() {
    let root = root(); let path = root.join("README.md"); fs::write(&path, b"old needle").unwrap();
    let mut repo = repo(&root); let mut desk = desk(502);
    repo.search(1, "needle", AtlasSearchOptions::default(), || false).unwrap();
    let file = find(&mut repo, 1, b"README.md");
    fs::write(&path, b"new current bytes").unwrap();
    let opened = repo.open_path_hit(&mut desk, 0, 1, 1, file, || false).unwrap();
    let current = opened.change.active.unwrap();
    assert_eq!(desk.model().source(current, 1).unwrap().bytes(), b"new current bytes");
    let receipt = json(&desk.repository_path_open_response(&opened).unwrap());
    assert_eq!(receipt.get("source_observation").text(), "new-capture-after-path-selection");
    assert_eq!(receipt.get("source_bytes_read").number(), b"new current bytes".len() as u64);
    assert_eq!(receipt.get("catalog_file_id").number(), file.get());
    apply(&mut desk, DeskCommand::Pin { pane: current, pinned: true });
    let historical = repo.open_hit(&mut desk, 2, 3, 1, 1, || false).unwrap().change.active.unwrap();
    assert_ne!(historical, current);
    assert_eq!(desk.model().source(historical, 3).unwrap().bytes(), b"old needle");
    assert_eq!(desk.model().source(current, 3).unwrap().bytes(), b"new current bytes");
}

#[test]
fn path_search_does_not_read_missing_or_undecodable_sources_or_cancel_content_work() {
    let root = root(); let missing = root.join("missing.rs");
    fs::write(&missing, b"needle").unwrap(); fs::write(root.join("invalid.bin"), [0xff, 0xfe, 0]).unwrap();
    let mut repo = repo(&root); fs::rename(&missing, root.join("moved.rs")).unwrap();
    repo.begin_query(1, "needle", AtlasSearchOptions::default(), || false).unwrap();
    let before = repo.work_state();
    find(&mut repo, 1, b"missing.rs"); find(&mut repo, 2, b"invalid.bin");
    assert_eq!(repo.work_state(), before);
    let page = json(&repo.path_page(2, 0, 10, || false).unwrap());
    assert!(!page.get("source_payload_read").flag()); assert!(page.get("search_complete").flag());
    assert_eq!(repo.path_generation(), Some(2));
}

#[test]
fn path_open_feeds_document_navigation_and_offline_checkpoint_restoration() {
    let root = root(); let path = root.join("README.md"); fs::write(&path, b"# Intro\n\n## Usage\n\nRead this.\n").unwrap();
    let mut repo = repo(&root); let file = find(&mut repo, 1, b"README.md"); let mut desk = desk(503);
    let pane = repo.open_path_hit(&mut desk, 0, 1, 1, file, || false).unwrap().change.active.unwrap();
    let doc = DeskDocument::prepare(&mut desk, 1, pane, 1, DeskDocumentOptions::default(), || false).unwrap();
    doc.seek_heading(&mut desk, 1, 2, 1, "usage", || false).unwrap();
    apply(&mut desk, DeskCommand::Bookmark { pane, label: "usage instructions".into() });
    let selected = desk.model().selected_bytes(pane, 3).unwrap().to_vec();
    drop(repo); drop(doc); fs::rename(&path, root.join("moved.md")).unwrap();
    let saved = root.join("desk.fcbk"); assert_eq!(desk.save_checkpoint(3, &saved, 1024, || false).error(), None);
    let mut restored = DeskSession::new(owner(504), DeskLimits::default()).unwrap();
    let pane = restored.restore_checkpoint_file(0, 1, &saved, || false).unwrap().active.unwrap();
    assert_eq!(restored.model().selected_bytes(pane, 1).unwrap(), selected);
    assert_eq!(restored.model().bookmarks()[0].label(), "usage instructions");
    let doc = DeskDocument::prepare(&mut restored, 1, pane, 1, DeskDocumentOptions::default(), || false).unwrap();
    assert!(doc.overview(&mut restored, 1, 1, || false).is_ok());
}

#[test]
fn stale_queries_foreign_desks_and_canceled_replacements_preserve_accepted_state() {
    let root = root(); fs::write(root.join("a.rs"), b"source").unwrap();
    let mut repo = repo(&root); let file = find(&mut repo, 1, b"a.rs"); let mut desk = desk(505);
    assert!(matches!(repo.find_paths(2, b"a", options(), || true), Err(e) if e.is_canceled()));
    assert_eq!(repo.path_generation(), Some(1));
    assert_eq!(repo.open_path_hit(&mut desk, 0, 1, 2, file, || false), Err(DeskRepositoryError::Path(AtlasPathError::StaleQuery)));
    assert_eq!(repo.open_path_hit(&mut desk, 1, 1, 1, file, || false), Err(DeskRepositoryError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert!(matches!(repo.open_path_hit(&mut desk, 0, 1, 1, file, || true), Err(e) if e.is_canceled()));
    assert_eq!(desk.model().revision(), 0);
    repo.open_path_hit(&mut desk, 0, 2, 1, file, || false).unwrap();
    let mut foreign = DeskSession::new(owner(506), DeskLimits::default()).unwrap();
    assert_eq!(repo.open_path_hit(&mut foreign, 0, 1, 1, file, || false), Err(DeskRepositoryError::WrongDesk));
    assert_eq!(foreign.model().revision(), 0); assert_eq!(desk.model().revision(), 2);
}

#[test]
fn replacing_a_selected_file_with_symlink_is_refused_without_reading_the_target() {
    use std::os::unix::fs::symlink;
    let root = root(); let path = root.join("a.rs"); fs::write(&path, b"inside").unwrap();
    let mut repo = repo(&root); let file = find(&mut repo, 1, b"a.rs");
    let outside = root.with_extension("outside"); fs::write(&outside, b"not granted").unwrap();
    fs::rename(&path, root.join("old.rs")).unwrap(); symlink(&outside, &path).unwrap();
    let mut desk = desk(507);
    assert!(repo.open_path_hit(&mut desk, 0, 1, 1, file, || false).is_err());
    assert_eq!(desk.model().revision(), 0); assert_eq!(desk.model().retained_source_count(), 0);
    assert_eq!(json(&desk.state(|| false).unwrap()).get("initial_source_bytes_read").number(), 0);
    assert_eq!(fs::read(&outside).unwrap(), b"not granted");
}

#[test]
fn native_byte_filenames_remain_distinct_from_display_labels_and_case_variants() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let root = root(); let raw = b"raw\t\xff.md";
    fs::write(root.join(OsString::from_vec(raw.to_vec())), b"# Raw\n").unwrap();
    fs::write(root.join("README.md"), b"upper").unwrap(); fs::write(root.join("readme.md"), b"lower").unwrap();
    let mut repo = repo(&root); let raw_file = find(&mut repo, 1, raw);
    let upper = find(&mut repo, 2, b"README.md"); let lower = find(&mut repo, 3, b"readme.md");
    assert_ne!(upper, lower); assert_ne!(raw_file, upper);
    let raw_file = find(&mut repo, 4, raw); let mut desk = desk(508);
    let pane = repo.open_path_hit(&mut desk, 0, 1, 4, raw_file, || false).unwrap().change.active.unwrap();
    assert_eq!(desk.model().source(pane, 1).unwrap().bytes(), b"# Raw\n");
}

#[test]
fn source_byte_limit_and_nonmatching_file_ids_do_not_widen_path_authority() {
    let root = root(); fs::write(root.join("small.rs"), b"abcdef").unwrap(); fs::write(root.join("other.rs"), b"other").unwrap();
    let mut repo = repo(&root); let other = find(&mut repo, 1, b"other.rs"); let file = find(&mut repo, 2, b"small.rs");
    let mut desk = DeskSession::new(owner(509), DeskLimits { source_bytes: 3, ..Default::default() }).unwrap();
    assert_eq!(repo.open_path_hit(&mut desk, 0, 1, 2, other, || false), Err(DeskRepositoryError::Path(AtlasPathError::MissingHit)));
    assert!(repo.open_path_hit(&mut desk, 0, 2, 2, file, || false).is_err());
    assert_eq!(desk.model().revision(), 0); assert_eq!(repo.path_generation(), Some(2));
    assert_eq!(json(&desk.state(|| false).unwrap()).get("initial_source_bytes_read").number(), 0);
}

#[test]
fn partial_path_listing_and_selected_refinement_remain_explicit_without_source_work() {
    let root = root();
    for name in ["file-a.rs", "file-b.rs", "file-c.rs"] { fs::write(root.join(name), b"x").unwrap(); }
    let mut repo = repo(&root);
    let selected = find(&mut repo, 1, b"file-c.rs"); repo.select_path(1, selected, || false).unwrap();
    let partial = repo.find_paths(2, b"file", AtlasPathOptions { max_results: 1, mode: PathMatchMode::Prefix, ..options() }, || false).unwrap();
    assert_eq!(partial.exit_code(), EXIT_PARTIAL); let partial = json(&partial);
    assert!(partial.get("truncated").flag()); assert!(!partial.get("search_complete").flag());
    assert!(!partial.get("source_payload_read").flag());
    assert_eq!(partial.get("selection").get("file_id").number(), selected.get());
    let mut desk = desk(510); repo.open_path_hit(&mut desk, 0, 1, 2, selected, || false).unwrap();
    repo.clear_paths(3, || false).unwrap(); assert_eq!(repo.path_generation(), None);
    assert_eq!(desk.model().source(desk.model().active().unwrap(), 1).unwrap().bytes(), b"x");
}
