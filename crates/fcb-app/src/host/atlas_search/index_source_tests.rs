#![forbid(unsafe_code)]

use super::*;
use std::sync::atomic::{AtomicU64, Ordering};
use crate::host::atlas_session::AtlasSessionOptions;

static NEXT: AtomicU64 = AtomicU64::new(1);
fn owner(id: u64) -> ArenaOwnerId { ArenaOwnerId::new(id).unwrap() }
fn fixture(bytes: &[u8], grams: usize) -> (std::path::PathBuf, AtlasSession, RetainedAtlasSearch, FileId, SourceRevision) {
    let n = NEXT.fetch_add(1, Ordering::Relaxed);
    let root = std::env::temp_dir().join(format!("fcb-index-source-{}-{n}", std::process::id()));
    std::fs::create_dir(&root).unwrap();
    std::fs::write(root.join("source.txt"), bytes).unwrap();
    let atlas = AtlasSession::open(owner(1000 + n), &root, AtlasSessionOptions::default(), || false).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.prepare_index(&atlas, 1, AtlasIndexOptions { max_index_grams: grams, ..Default::default() }, || false).unwrap();
    let file = atlas.atlas().catalog().file_id(0).unwrap();
    let revision = search.index.as_ref().unwrap().engine.capture(file).unwrap().request().revision();
    (root, atlas, search, file, revision)
}

#[test]
fn indexed_source_survives_query_replacement_clear_and_live_edit() {
    let (root, atlas, mut search, file, revision) = fixture(b"alpha beta alpha", 1024);
    search.search_indexed(&atlas, 2, 1, "alpha", 100, 1024, || false).unwrap();
    search.search_indexed(&atlas, 3, 1, "beta", 100, 1024, || false).unwrap();
    search.clear(&atlas, 4, || false).unwrap();
    std::fs::write(root.join("source.txt"), b"changed contents").unwrap();
    let (mut reader, reply) = search.open_index_reader(&atlas, owner(99), 1, file, revision, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"alpha beta alpha");
    assert!(reply.as_str().contains("\"source_reopened\":false"));
    assert!(reply.as_str().contains("\"selection_namespace\":\"index-source\""));
    assert!(!reply.as_str().contains("\"hit_id\""));
    assert!(!reply.as_str().contains("\"query_generation\""));
    let info = reader.info(|| false).unwrap();
    assert!(info.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    search.clear_index(&atlas, 5, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"alpha beta alpha");
    assert!(matches!(search.open_index_reader(&atlas, owner(98), 1, file, revision, || false), Err(AtlasSearchError::MissingIndex)));
}

#[test]
fn fallback_utf16_and_nul_bytes_transfer_without_a_decoding_or_path_roundtrip() {
    for bytes in [&b"\xff\xfea\0l\0p\0h\0a\0"[..], &b"a\0b\xff"[..], &b""[..]] {
        let (_root, atlas, mut search, file, revision) = fixture(bytes, 0);
        let (reader, reply) = search.open_index_reader(&atlas, owner(97), 1, file, revision, || false).unwrap();
        assert_eq!(reader.capture().bytes(), bytes);
        assert!(reply.as_str().contains("\"capture_sha256\":\""));
        assert!(reply.as_str().contains("\"capture_origin\":\"host-supplied\""));
    }
}

#[test]
fn stale_foreign_and_canceled_transfers_do_not_change_queries_or_index() {
    let (_root, atlas, mut search, file, revision) = fixture(b"alpha", 1024);
    search.search_indexed(&atlas, 2, 1, "alpha", 100, 1024, || false).unwrap();
    assert!(matches!(search.open_index_reader(&atlas, owner(96), 99, file, revision, || false), Err(AtlasSearchError::StaleIndex)));
    let stale = SourceRevision::new(file.owner(), revision.get() + 1).unwrap();
    assert!(matches!(search.open_index_reader(&atlas, owner(96), 1, file, stale, || false), Err(AtlasSearchError::StaleIndex)));
    let foreign = FileId::new(owner(95), file.get()).unwrap();
    assert!(matches!(search.open_index_reader(&atlas, owner(96), 1, foreign, revision, || false), Err(AtlasSearchError::WrongAtlas)));
    assert!(matches!(search.open_index_reader(&atlas, file.owner(), 1, file, revision, || false), Err(AtlasSearchError::WrongAtlas)));
    assert!(matches!(search.open_index_reader(&atlas, owner(96), 1, file, revision, || true), Err(AtlasSearchError::Canceled)));
    assert_eq!(search.index_generation(), Some(1));
    assert_eq!(search.accepted_generation(), Some(2));
    assert!(search.open_index_reader(&atlas, owner(96), 1, file, revision, || false).is_ok());
}

#[test]
fn replacing_index_invalidates_old_revision_even_for_the_same_catalog_file() {
    let (root, atlas, mut search, file, revision) = fixture(b"alpha", 1024);
    std::fs::write(root.join("source.txt"), b"newer").unwrap();
    search.prepare_index(&atlas, 2, AtlasIndexOptions::default(), || false).unwrap();
    assert!(matches!(search.open_index_reader(&atlas, owner(94), 1, file, revision, || false), Err(AtlasSearchError::StaleIndex)));
    assert!(matches!(search.open_index_reader(&atlas, owner(94), 2, file, revision, || false), Err(AtlasSearchError::StaleIndex)));
    let current = search.index.as_ref().unwrap().engine.capture(file).unwrap().request().revision();
    let (reader, _) = search.open_index_reader(&atlas, owner(94), 2, file, current, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"newer");
}
