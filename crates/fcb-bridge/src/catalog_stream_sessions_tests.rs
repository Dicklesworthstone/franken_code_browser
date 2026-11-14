#![forbid(unsafe_code)]

use super::*;
use fcb_app::host::{atlas_search::AtlasSearchScope, atlas_stream_search::AtlasStreamSearchOptions,
    catalog_session::CatalogContentSearchMode};
use std::{ffi::OsString, fs, os::unix::ffi::OsStringExt,
    sync::atomic::{AtomicU64, Ordering}};

fn root() -> std::path::PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-catalog-stream-registry-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root // Preserve fixtures for diagnosis.
}
fn number(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}
fn options() -> CatalogContentSearchOptions {
    CatalogContentSearchOptions { mode: CatalogContentSearchMode::Text, count_all_matches: true,
        start_after: 0, scope: AtlasSearchScope::all(),
        limits: AtlasStreamSearchOptions { max_matches: 4, max_scan_bytes: 64 * 1024 * 1024,
            max_read_calls: 1_048_576 } }
}
fn info(streams: &CatalogStreamSessions, handle: u64) -> String {
    streams.execute(handle, Command::Info, || false).unwrap().as_str().to_owned()
}
fn open(streams: &CatalogStreamSessions, root: &Path) -> u64 {
    let handle = streams.create().unwrap();
    let initial = streams.open(handle, root, 7, "needle", options(), || false).unwrap();
    assert_eq!(number(initial.as_str(), "revision"), 1);
    assert_eq!(number(initial.as_str(), "retained_hits"), 0);
    assert_eq!(number(initial.as_str(), "payload_bytes_read"), 0);
    handle
}
fn advance(streams: &CatalogStreamSessions, handle: u64, revision: u64, max_steps: usize) -> String {
    streams.execute(handle, Command::Advance { generation: 7, after_revision: revision, max_steps }, || false)
        .unwrap().as_str().to_owned()
}
fn drain(streams: &CatalogStreamSessions, handle: u64) -> String {
    let mut report = info(streams, handle);
    for _ in 0..10_000 {
        if report.contains("\"more\":false") { return report; }
        report = advance(streams, handle, number(&report, "revision"), 256);
    }
    panic!("bounded query failed to finish: {report}");
}
fn hit_path(hit: &str) -> OsString {
    let encoded = hit.split_once("\"path\":{\"encoding\":\"unix-bytes\",\"hex\":\"")
        .unwrap().1.split('"').next().unwrap();
    let bytes = encoded.as_bytes().chunks_exact(2).map(|pair| {
        let hex = std::str::from_utf8(pair).unwrap(); u8::from_str_radix(hex, 16).unwrap()
    }).collect();
    OsString::from_vec(bytes)
}

#[test]
fn catalog_stream_searches_beyond_atlas_prefix_and_retains_evicted_page_witnesses() {
    let root = root();
    // Larger than the native atlas's ordinary 4096-file prefix and the catalog's
    // four retained pages. Every file counts, while only four witnesses survive.
    for i in 0..4200 { fs::write(root.join(format!("file-{i:04}.rs")), b"needle\n").unwrap(); }
    let streams = CatalogStreamSessions::new(); let handle = open(&streams, &root);
    let first = advance(&streams, handle, 1, 1);
    let replay = advance(&streams, handle, 1, 256);
    assert_eq!(number(&first, "revision"), number(&replay, "revision"));
    assert_eq!(number(&first, "payload_bytes_read"), number(&replay, "payload_bytes_read"));
    assert_eq!(number(&first, "catalogued_files"), number(&replay, "catalogued_files"));
    let mut progress = first;
    for _ in 0..1000 {
        if number(&progress, "retained_hits") > 0 { break; }
        progress = advance(&streams, handle, number(&progress, "revision"), 16);
    }
    assert!(number(&progress, "retained_hits") > 0, "{progress}");
    let early_revision = number(&progress, "revision");
    let early = streams.execute(handle, Command::Hit { generation: 7, revision: early_revision, position: 0 }, || false).unwrap();
    let file = number(early.as_str(), "file_id");
    let path = hit_path(early.as_str());
    assert!(early.as_str().contains("\"original_hex\":\"6e6565646c65\""));
    let complete = drain(&streams, handle); let revision = number(&complete, "revision");
    assert_eq!(number(&complete, "catalogued_files"), 4200, "{complete}");
    assert_eq!(number(&complete, "files_examined"), 4200, "{complete}");
    assert_eq!(number(&complete, "matches_seen"), 4200, "{complete}");
    assert_eq!(number(&complete, "retained_hits"), 4);
    assert!(complete.contains("\"discovery_complete\":true"));
    assert!(complete.contains("\"search_complete\":true"));
    assert!(complete.contains("\"truncated\":true"));
    assert!(!complete.contains("node_ordinal"));
    assert!(!complete.contains("source_manifest"));
    assert!(!complete.contains("layout_revision"));
    assert!(streams.execute(handle, Command::Page { generation: 7, revision: early_revision, start: 0, count: 4 }, || false).is_err());
    assert!(streams.execute(handle, Command::Hit { generation: 8, revision, position: 0 }, || false).is_err());
    fs::write(root.join(path), b"changed after search").unwrap();
    let retained = streams.execute(handle, Command::Hit { generation: 7, revision, position: 0 }, || false).unwrap();
    assert_eq!(number(retained.as_str(), "file_id"), file);
    assert!(retained.as_str().contains("\"original_hex\":\"6e6565646c65\""));
    assert!(retained.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    assert!(retained.as_str().contains("\"additional_read_calls\":\"0\""));
    streams.close(handle).unwrap();
}

#[test]
fn catalog_stream_cancel_and_lost_open_delivery_preserve_accepted_query() {
    let root = root(); fs::write(root.join("source.rs"), b"needle\n").unwrap();
    let streams = CatalogStreamSessions::new(); let first = streams.create().unwrap();
    let mut polls = 0;
    streams.open(first, &root, 7, "needle", options(), || { polls += 1; false }).unwrap();
    streams.close(first).unwrap(); assert!(polls > 0);
    let handle = streams.create().unwrap(); let mut at = 0;
    let lost = streams.open(handle, &root, 7, "needle", options(), || {
        at += 1;
        if at == polls { streams.cancel(handle).unwrap(); }
        false
    });
    assert!(matches!(lost, Err(AccessError::Canceled)));
    let recovered = info(&streams, handle);
    assert_eq!(number(&recovered, "revision"), 1);
    assert_eq!(number(&recovered, "payload_bytes_read"), 0);
    let mut visits = 0;
    let canceled = streams.execute(handle, Command::Advance { generation: 7, after_revision: 1, max_steps: 256 }, || {
        visits += 1;
        if visits == 1 { streams.cancel(handle).unwrap(); }
        false
    });
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    let complete = drain(&streams, handle);
    assert_eq!(number(&complete, "matches_seen"), 1, "{complete}");
    assert!(complete.contains("\"search_complete\":true"));
    streams.close(handle).unwrap();
}

#[test]
fn catalog_stream_cancellation_inside_shared_matcher_revokes_the_query() {
    let root = root(); fs::write(root.join("large.rs"), b"needle\n".repeat(100_000)).unwrap();
    let streams = CatalogStreamSessions::new(); let handle = open(&streams, &root);
    // Two steps retain a candidate and admit its file; the next advance enters
    // the shared matcher without any source payload having been read yet.
    let admitted = advance(&streams, handle, 1, 2);
    assert_eq!(number(&admitted, "payload_bytes_read"), 0);
    let mut polls = 0;
    let canceled = streams.execute(handle, Command::Advance { generation: 7,
        after_revision: number(&admitted, "revision"), max_steps: 16 }, || {
        polls += 1;
        // The first two checks guard the command and work loop. The third is
        // inside FileSearch, whose canceled decoder cannot be silently resumed.
        if polls == 3 { streams.cancel(handle).unwrap(); }
        false
    });
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    assert!(matches!(streams.execute(handle, Command::Info, || false),
        Err(AccessError::Search(CatalogContentSearchError::Canceled))));
    streams.close(handle).unwrap();
}

#[test]
fn catalog_stream_stop_discards_unfinished_file_and_root_replacement_revokes_access() {
    let root = root(); fs::write(root.join("large.rs"), b"needle\n".repeat(100_000)).unwrap();
    let streams = CatalogStreamSessions::new(); let handle = open(&streams, &root);
    let progress = advance(&streams, handle, 1, 1);
    assert!(progress.contains("\"more\":true"));
    let stopped = streams.execute(handle, Command::Stop { generation: 7 }, || false).unwrap();
    assert!(stopped.as_str().contains("\"more\":false"), "{}", stopped.as_str());
    assert!(stopped.as_str().contains("\"search_complete\":false"));
    assert_eq!(number(stopped.as_str(), "retained_hits"), 0);
    let retained = root.with_extension("retained-after-move");
    fs::rename(&root, retained).unwrap();
    fs::create_dir(&root).unwrap();
    fs::write(root.join("large.rs"), b"replacement root").unwrap();
    assert!(streams.execute(handle, Command::Info, || false).is_err());
    streams.close(handle).unwrap();
}

#[test]
fn catalog_stream_retiring_capacity_busy_lock_and_cross_family_handles_are_safe() {
    let streams = CatalogStreamSessions::new();
    let readers = reader_sessions::ReaderSessions::new(); let reader = readers.create().unwrap();
    assert!(matches!(streams.execute(reader, Command::Info, || false), Err(AccessError::Unknown)));
    let handles: Vec<_> = (0..MAX_CATALOG_STREAM_SESSIONS).map(|_| streams.create().unwrap()).collect();
    assert!(!handles.contains(&reader));
    assert!(matches!(streams.create(), Err(AccessError::Capacity)));
    let held = streams.get(handles[0]).unwrap(); let locked = held.state.lock().unwrap();
    assert!(matches!(streams.execute(handles[0], Command::Info, || false), Err(AccessError::Busy)));
    streams.cancel(handles[0]).unwrap(); streams.close(handles[0]).unwrap();
    assert!(matches!(streams.create(), Err(AccessError::Capacity)));
    assert!(matches!(streams.execute(handles[0], Command::Info, || false), Err(AccessError::Unknown)));
    drop(locked); drop(held);
    let replacement = streams.create().unwrap(); assert!(!handles.contains(&replacement));
    streams.close(replacement).unwrap();
    for &handle in &handles[1..] { streams.close(handle).unwrap(); }
    readers.close(reader).unwrap();
}
