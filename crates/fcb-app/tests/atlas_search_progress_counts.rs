#![forbid(unsafe_code)]
#![cfg(unix)]

//! Typed host progress must expose observed counts, not silently substitute the
//! number of retained rows. Exercise all three real search execution routes.
use std::fs;
use fcb::ArenaOwnerId;
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasIndexOptions, AtlasSearchOptions, RetainedAtlasSearch};

#[test]
fn typed_and_wire_counts_agree_for_direct_progressive_and_indexed_lookahead() {
    let root = std::env::temp_dir().join(format!("fcb-progress-counts-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("source.rs"), b"needle needle needle").unwrap();
    let atlas = AtlasSession::open(ArenaOwnerId::new(27999).unwrap(), &root,
        AtlasSessionOptions::default(), || false).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let options = AtlasSearchOptions { max_matches: 2, max_files: 10,
        max_file_bytes: 1024, max_source_bytes: 4096 };
    search.search(&atlas, 1, "needle", options, || false).unwrap();
    search.prepare_index(&atlas, 2, AtlasIndexOptions { max_files: 10, max_file_bytes: 1024,
        max_source_bytes: 4096, max_index_grams: 1024 }, || false).unwrap();
    // Preparing the index does not replace accepted direct-query progress.
    let direct = search.progress(&atlas, 1).unwrap();
    assert_eq!((direct.matches_seen, direct.retained_hits), (3, 2));
    assert!(direct.truncated && !direct.complete);
    search.search_indexed(&atlas, 3, 2, "needle", 2, 4096, || false).unwrap();
    let indexed = search.progress(&atlas, 3).unwrap();
    assert_eq!((indexed.matches_seen, indexed.retained_hits), (3, 2));
    assert!(indexed.truncated && !indexed.complete);
    assert_eq!(indexed.source_bytes_read, 0);
    search.begin(&atlas, 4, "needle", options, || false).unwrap();
    let pending = search.progress(&atlas, 4).unwrap();
    assert_eq!((pending.matches_seen, pending.retained_hits), (0, 0));
    search.step(&atlas, 4, || false).unwrap();
    let stepped = search.progress(&atlas, 4).unwrap();
    assert_eq!((stepped.matches_seen, stepped.retained_hits), (3, 2));
    assert!(stepped.truncated && !stepped.complete);
    let page = search.page(&atlas, 4, 0, 10, || false).unwrap();
    assert!(page.as_str().contains("\"matches_seen\":\"3\""));
    assert!(page.as_str().contains("\"retained_hits\":\"2\""));
}


#[test]
fn counted_exact_search_finishes_scope_after_retained_rows_fill() {
    let root = std::env::temp_dir().join(format!("fcb-progress-count-all-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir_all(&root).unwrap();
    fs::write(root.join("a.rs"), b"needle needle").unwrap();
    fs::write(root.join("b.rs"), b"needle").unwrap();
    fs::write(root.join("c.rs"), b"absent").unwrap();
    let atlas = AtlasSession::open(ArenaOwnerId::new(28001).unwrap(), &root,
        AtlasSessionOptions::default(), || false).unwrap();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let options = AtlasSearchOptions { max_matches: 1, max_files: 10,
        max_file_bytes: 1024, max_source_bytes: 4096 };
    let legacy = search.search(&atlas, 1, "needle", options, || false).unwrap();
    assert!(legacy.as_str().contains("\"stop_reason\":\"match-limit\""), "{}", legacy.as_str());
    let counted = search.search_counted_scoped(&atlas, 2, "needle",
        fcb_app::host::atlas_search::AtlasSearchScope::all(), options, || false).unwrap();
    assert_eq!(counted.exit_code(), fcb_app::EXIT_PARTIAL);
    let wire = counted.as_str();
    for expected in ["\"count_all_matches\":true", "\"match_count_complete\":true",
        "\"search_complete\":true", "\"truncated\":true",
        "\"stop_reason\":\"all-files-examined\"", "\"matches_seen\":\"3\"",
        "\"retained_hits\":\"1\"", "\"examined_files\":\"3\""] {
        assert!(wire.contains(expected), "missing {expected}: {wire}");
    }
    let progress = search.progress(&atlas, 2).unwrap();
    assert!(progress.complete && progress.truncated);
    assert_eq!((progress.matches_seen, progress.retained_hits), (3, 1));
    let page = search.page(&atlas, 2, 0, 10, || false).unwrap();
    assert_eq!(page.exit_code(), fcb_app::EXIT_PARTIAL);
    assert!(page.as_str().contains("\"match_count_complete\":true"));
    let _ = fs::remove_dir_all(root);
}
