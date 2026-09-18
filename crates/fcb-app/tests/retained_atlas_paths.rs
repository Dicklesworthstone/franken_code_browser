#![cfg(unix)]
#![forbid(unsafe_code)]

//! Real catalog/path-engine/reader composition; no native presentation claim.
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, FileId, Point2D};
use fcb::search::PathMatchKind;
use fcb_app::host::atlas_paths::{RetainedAtlasPaths, AtlasPathOptions, AtlasPathError, PathCase, PathMatchMode};
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions, AtlasAction};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!("fcb-path-host-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(root.join("src")).unwrap(); fs::create_dir_all(root.join("docs")).unwrap();
        for path in ["src/main.rs", "src/main_extra.rs", "src/Other.rs", "docs/guide.md"] {
            fs::write(root.join(path), format!("original {path}\n")).unwrap();
        }
        Self(root)
    }
    fn atlas(&self) -> AtlasSession {
        AtlasSession::open(owner(), &self.0, Default::default(), || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner() -> ArenaOwnerId { ArenaOwnerId::new(2501).unwrap() }
fn reader_owner() -> ArenaOwnerId { ArenaOwnerId::new(2502).unwrap() }
fn file(atlas: &AtlasSession, path: &[u8]) -> FileId {
    let node = atlas.atlas().index().unwrap().find_path(path).unwrap(); atlas.atlas().file(node).unwrap()
}
fn exact() -> AtlasPathOptions { AtlasPathOptions { mode: PathMatchMode::Exact, ..Default::default() } }

#[test]
fn first_find_builds_from_frozen_metadata_even_after_sources_disappear_and_reuses_keys() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    assert!(paths.prepared_index().is_none());
    fs::remove_dir_all(fixture.0.join("src")).unwrap();
    let response = paths.find(&atlas, 1, b"main", Default::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    assert!(response.as_str().contains("\"source_payload_read\":false"));
    assert_eq!(paths.hits(&atlas, 1).unwrap().len(), 2);
    let backing = paths.prepared_index().unwrap().paths().next().unwrap().raw_path().as_bytes().as_ptr();
    paths.clear(&atlas, 2, || false).unwrap();
    paths.find(&atlas, 3, b"main.rs", exact(), || false).unwrap();
    assert_eq!(paths.hits(&atlas, 3).unwrap()[0].file, file(&atlas, b"src/main.rs"));
    assert_eq!(paths.prepared_index().unwrap().paths().next().unwrap().raw_path().as_bytes().as_ptr(), backing);
    assert_eq!(paths.prepared_index().unwrap().len(), 4);
}

#[test]
fn exact_prefix_and_fuzzy_use_production_rank_classes() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"main.rs", Default::default(), || false).unwrap();
    assert_eq!(paths.hits(&atlas, 1).unwrap()[0].rank.kind, PathMatchKind::ExactFilename);
    paths.find(&atlas, 2, b"main", AtlasPathOptions { mode: PathMatchMode::Prefix, ..Default::default() }, || false).unwrap();
    assert_eq!(paths.hits(&atlas, 2).unwrap().len(), 2);
    assert!(paths.hits(&atlas, 2).unwrap().iter().all(|h| h.rank.kind == PathMatchKind::FilenamePrefix));
    paths.find(&atlas, 3, b"mnrs", Default::default(), || false).unwrap();
    assert!(paths.hits(&atlas, 3).unwrap().iter().any(|h| h.file == file(&atlas, b"src/main.rs")));
    paths.find(&atlas, 4, b"src/main.rs", exact(), || false).unwrap();
    assert_eq!(paths.hits(&atlas, 4).unwrap()[0].rank.kind, PathMatchKind::ExactPath);
}

#[test]
fn case_option_does_not_change_native_identity_or_reinterpret_literal_punctuation() {
    let fixture = Fixture::new();
    fs::write(fixture.0.join("src/path:token.rs"), b"literal").unwrap();
    let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"other.rs", AtlasPathOptions { case: PathCase::Sensitive, ..exact() }, || false).unwrap();
    assert!(paths.hits(&atlas, 1).unwrap().is_empty());
    paths.find(&atlas, 2, b"other.rs", exact(), || false).unwrap();
    assert_eq!(paths.hits(&atlas, 2).unwrap()[0].file, file(&atlas, b"src/Other.rs"));
    paths.find(&atlas, 3, b"path:token.rs", exact(), || false).unwrap();
    assert_eq!(paths.hits(&atlas, 3).unwrap().len(), 1);
}

#[test]
fn selected_file_survives_reranking_even_outside_top_k_and_nonmatches_clear_it() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"main", Default::default(), || false).unwrap();
    let selected = file(&atlas, b"src/main_extra.rs");
    paths.select(&atlas, 1, selected, || false).unwrap();
    let response = paths.find(&atlas, 2, b"main.rs", AtlasPathOptions { max_results: 1, ..Default::default() }, || false).unwrap();
    assert_eq!(paths.hits(&atlas, 2).unwrap()[0].file, file(&atlas, b"src/main.rs"));
    assert_eq!(paths.selected(&atlas, 2).unwrap().unwrap().file, selected);
    assert!(paths.hit(&atlas, 2, selected).is_ok());
    assert!(response.as_str().contains("\"truncated\":true"));
    paths.find(&atlas, 3, b"guide.md", exact(), || false).unwrap();
    assert!(paths.selected(&atlas, 3).unwrap().is_none());
    assert!(matches!(paths.hit(&atlas, 3, selected), Err(AtlasPathError::MissingHit)));
}

#[test]
fn failed_and_canceled_replacements_keep_old_rows_and_consume_attempt_ids() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"main", Default::default(), || false).unwrap();
    let old = paths.hits(&atlas, 1).unwrap().to_vec();
    assert!(paths.find(&atlas, 2, b"", Default::default(), || false).is_err());
    assert!(paths.find(&atlas, 3, b"guide", Default::default(), || true).is_err());
    assert!(paths.find(&atlas, 4, &[b'x'; 257], Default::default(), || false).is_err());
    assert!(paths.find(&atlas, 5, b"x", AtlasPathOptions { max_results: 4097, ..Default::default() }, || false).is_err());
    assert_eq!(paths.accepted_generation(), Some(1)); assert_eq!(paths.hits(&atlas, 1).unwrap(), old);
    assert!(matches!(paths.find(&atlas, 5, b"main", Default::default(), || false), Err(AtlasPathError::StaleQuery)));
    paths.find(&atlas, 6, b"guide", Default::default(), || false).unwrap();
    assert!(matches!(paths.page(&atlas, 1, 0, 10, || false), Err(AtlasPathError::StaleQuery)));
}

#[test]
fn mid_query_cancellation_preserves_old_selection() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"main", Default::default(), || false).unwrap();
    let selected = file(&atlas, b"src/main.rs"); paths.select(&atlas, 1, selected, || false).unwrap();
    let mut calls = 0;
    assert!(paths.find(&atlas, 2, b"rs", Default::default(), || { calls += 1; calls > 3 }).is_err());
    assert!(calls > 3); assert_eq!(paths.selected(&atlas, 1).unwrap().unwrap().file, selected);
    assert_eq!(paths.accepted_generation(), Some(1));
}

#[test]
fn paging_and_count_completeness_are_independent_of_top_k_truncation() {
    let fixture = Fixture::new();
    for i in 0..80 { fs::write(fixture.0.join(format!("item_{i:03}.rs")), b"x").unwrap(); }
    let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    let response = paths.find(&atlas, 1, b"item", Default::default(), || false).unwrap();
    assert!(response.as_str().contains("\"matches_seen\":\"80\""));
    assert!(response.as_str().contains("\"next_offset\":\"64\""));
    let page = paths.page(&atlas, 1, 64, 128, || false).unwrap();
    assert!(page.as_str().contains("\"next_offset\":null"));
    assert_eq!(page.as_str().matches("\"file_id\":").count(), 16);
    assert!(paths.page(&atlas, 1, 81, 10, || false).is_err());
    let limited = paths.find(&atlas, 2, b"item", AtlasPathOptions { max_results: 1, ..Default::default() }, || false).unwrap();
    assert!(limited.as_str().contains("\"search_complete\":true"));
    assert!(limited.as_str().contains("\"truncated\":true"));
    assert!(limited.as_str().contains("\"matches_seen\":\"80\""));
}

#[test]
fn path_activation_is_explicit_new_capture_then_reader_is_independent() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"main.rs", exact(), || false).unwrap();
    let target = paths.hits(&atlas, 1).unwrap()[0].file;
    fs::write(fixture.0.join("src/main.rs"), b"new capture\n").unwrap();
    let (reader, receipt) = paths.open_reader(&atlas, reader_owner(), 1, target, 1024, || false).unwrap();
    assert!(receipt.as_str().contains("new-capture-after-path-selection"));
    assert!(receipt.as_str().contains("\"source_payload_read\":true"));
    assert_eq!(reader.capture().bytes(), b"new capture\n");
    paths.clear(&atlas, 2, || false).unwrap(); drop(paths); drop(atlas);
    fs::write(fixture.0.join("src/main.rs"), b"later\n").unwrap();
    assert_eq!(reader.capture().bytes(), b"new capture\n");
}

#[test]
fn disappeared_or_symlinked_targets_fail_without_damaging_results() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"main.rs", exact(), || false).unwrap();
    let target = paths.hits(&atlas, 1).unwrap()[0].file;
    fs::remove_file(fixture.0.join("src/main.rs")).unwrap();
    assert!(paths.open_reader(&atlas, reader_owner(), 1, target, 1024, || false).is_err());
    std::os::unix::fs::symlink(fixture.0.join("docs/guide.md"), fixture.0.join("src/main.rs")).unwrap();
    assert!(paths.open_reader(&atlas, reader_owner(), 1, target, 1024, || false).is_err());
    assert_eq!(paths.hits(&atlas, 1).unwrap().len(), 1);
}

#[test]
fn focus_keeps_old_presented_frame_and_geometry_allocation() {
    let fixture = Fixture::new(); let mut atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    let nodes = atlas.atlas().layout().nodes().as_ptr();
    atlas.prepare(1, AtlasAction::View, || false).unwrap(); atlas.acknowledge(1, 1, 1, || false).unwrap();
    let before = atlas.pick(1, 1, Point2D::new(512.0, 384.0).unwrap(), || false).unwrap();
    paths.find(&atlas, 1, b"guide.md", exact(), || false).unwrap();
    let target = paths.hits(&atlas, 1).unwrap()[0].file;
    paths.select(&atlas, 1, target, || false).unwrap();
    assert!(atlas.pending_plan().is_none());
    let focus = paths.focus_hit(&mut atlas, 1, target, 2, || false).unwrap();
    assert!(focus.as_str().contains("\"native_presented\":false"));
    assert_eq!(atlas.presented_plan().unwrap().generation().get(), 1);
    assert_eq!(atlas.pick(1, 1, Point2D::new(512.0, 384.0).unwrap(), || false).unwrap().as_str(), before.as_str());
    assert_eq!(atlas.atlas().layout().nodes().as_ptr(), nodes);
}

#[test]
fn raw_native_names_keep_reversible_identity_and_can_be_opened() {
    use std::{ffi::OsString, os::unix::ffi::OsStringExt};
    let fixture = Fixture::new();
    let name = OsString::from_vec(b"raw\xff.rs".to_vec()); fs::write(fixture.0.join(&name), b"raw source").unwrap();
    let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    let response = paths.find(&atlas, 1, b"raw\xff.rs", exact(), || false).unwrap();
    assert!(response.as_str().contains("726177ff2e7273"));
    let target = paths.hits(&atlas, 1).unwrap()[0].file;
    let (reader, _) = paths.open_reader(&atlas, reader_owner(), 1, target, 1024, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"raw source");
}

#[test]
fn partial_catalog_and_giant_sources_are_not_hidden_by_path_results() {
    let fixture = Fixture::new();
    fs::OpenOptions::new().write(true).open(fixture.0.join("src/main.rs")).unwrap().set_len(5 * 1024 * 1024 * 1024).unwrap();
    let atlas = fixture.atlas(); let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    assert_eq!(paths.find(&atlas, 1, b"main.rs", exact(), || false).unwrap().exit_code(), EXIT_OK);
    let target = paths.hits(&atlas, 1).unwrap()[0].file;
    assert!(paths.open_reader(&atlas, reader_owner(), 1, target, 1024, || false).is_err());
    let partial = AtlasSession::open(reader_owner(), &fixture.0, AtlasSessionOptions { max_files: 1, ..Default::default() }, || false).unwrap();
    let mut limited = RetainedAtlasPaths::new(&partial).unwrap();
    let response = limited.find(&partial, 1, b"rs", Default::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL); assert!(response.as_str().contains("\"search_complete\":false"));
}

#[test]
fn foreign_catalog_revocation_and_generation_exhaustion_reject_without_aliasing() {
    let fixture = Fixture::new(); let atlas = fixture.atlas(); let other = fixture.atlas();
    let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    assert!(matches!(paths.find(&other, 1, b"main", Default::default(), || false), Err(AtlasPathError::WrongAtlas)));
    paths.find(&atlas, u64::MAX, b"main", Default::default(), || false).unwrap();
    let target = paths.hits(&atlas, u64::MAX).unwrap()[0].file;
    assert!(matches!(paths.find(&atlas, 1, b"main", Default::default(), || false), Err(AtlasPathError::StaleQuery)));
    atlas.atlas().catalog().grant().revoke();
    assert!(paths.page(&atlas, u64::MAX, 0, 10, || false).is_err());
    assert!(paths.open_reader(&atlas, reader_owner(), u64::MAX, target, 1024, || false).is_err());
}
