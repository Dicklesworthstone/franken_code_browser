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
