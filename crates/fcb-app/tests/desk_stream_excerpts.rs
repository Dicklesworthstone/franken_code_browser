#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real streaming search → exact excerpt → durable observed passage. No live
//! pathname may substitute for retained bytes. Code-first; execution is pending.
mod support;

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange};
use fcb::store::Sha256;
use fcb_app::host::{HostResponse, RetainedSearchExcerpt,
    atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_stream_search::{AtlasStreamSearchOptions, AtlasStreamSearchError, RetainedAtlasStreamSearch},
    catalog_session::{CatalogContentSearchSession, CatalogContentSearchOptions,
        CatalogContentSearchMode, CatalogContentSearchError},
    desk::{DeskSession, DeskLimits, export::DeskSourcePackLimits}};
use support::{Json, parse};

fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-desk-stream-excerpts-{}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap(); root // Preserve the fixture for diagnosis.
}
fn owner() -> ArenaOwnerId {
    static NEXT: AtomicU64 = AtomicU64::new(193_000);
    ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn range(value: &Json) -> ByteRange {
    ByteRange::new(ByteOffset::new(value.get("start").number()), ByteOffset::new(value.get("end").number())).unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn encoded(text: &str, encoding: &str) -> Vec<u8> {
    match encoding {
        "utf16le" => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        "utf16be" => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => text.as_bytes().to_vec(),
    }
}
fn keep(desk: &mut DeskSession, excerpt: &RetainedSearchExcerpt<'_>, row: &Json,
    source: &[u8]) -> u64 {
    let context = row.get("context");
    let captured = range(context.get("original_range"));
    let selected = range(context.get("text_original_range"));
    let matched = range(row.get("original_range"));
    assert_eq!(excerpt.extent().range(), captured);
    assert_eq!(excerpt.extent().request().range(), Some(range(context.get("requested_range"))));
    assert_eq!(excerpt.selection(), selected);
    assert!(selected.start() <= matched.start() && selected.end() >= matched.end());
    assert!(selected.len().get() > matched.len().get(), "the saved passage includes surrounding source");
    let original = &source[captured.start().get() as usize..captured.end().get() as usize];
    assert_eq!(excerpt.extent().bytes(), original);
    assert_eq!(context.get("extent_sha256").text(), Sha256::digest(original).to_hex());
    assert_eq!(hex(excerpt.path_bytes()), row.get("path").get("hex").text());
    assert_eq!(excerpt.extent().request().file().get(), row.get("file_id").number());
    assert_eq!(excerpt.extent().request().revision().get(), row.get("source_revision").number());
    let kept = desk.import_search_excerpt(desk.model().revision(), desk.model().last_attempt() + 1,
        excerpt, || false).unwrap();
    let read = json(&desk.observed_passage(desk.model().revision(), kept.passage, || false).unwrap());
    assert_eq!(read.get("text"), context.get("text"));
    assert_eq!(read.get("original_hex").text(),
        hex(&source[selected.start().get() as usize..selected.end().get() as usize]));
    assert_eq!(read.get("observation").get("extent_sha256"), context.get("extent_sha256"));
    assert_eq!(read.get("observation").get("consistency"), row.get("consistency"));
    assert_eq!(read.get("observation").get("final_observed_length"), row.get("final_observed_length"));
    assert_eq!(read.get("additional_source_bytes_read").number(), 0);
    assert!(!read.get("source_reopened").flag()); assert!(!read.get("whole_file_immutable").flag());
    kept.passage
}
fn finish(session: &mut CatalogContentSearchSession, generation: u64) -> Json {
    let mut result = json(&session.info(|| false).unwrap());
    for _ in 0..1000 {
        if !result.get("more").flag() { return result; }
        result = json(&session.advance(generation, result.get("revision").number(), 256, || false).unwrap());
    }
    panic!("fixture did not finish within its bounded scan");
}

#[test]
fn atlas_excerpts_keep_surrounding_utf8_and_utf16_bytes_after_path_replacement_and_query_clear() {
    for encoding in ["utf8", "utf16le", "utf16be"] {
        let root = fixture();
        let text = format!("\u{feff}{}needle😀{}\r\nneedle😀{}", "α😀".repeat(100), "x".repeat(100), "z".repeat(200));
        let bytes = encoded(&text, encoding);
        let path = root.join("evidence.rs"); fs::write(&path, &bytes).unwrap();
        let atlas = AtlasSession::open(owner(), &root, AtlasSessionOptions::default(), || false).unwrap();
        let mut source = RetainedAtlasStreamSearch::new(&atlas).unwrap();
        let report = json(&source.search_counted(&atlas, "needle😀", AtlasStreamSearchOptions::default(), || false).unwrap());
        let generation = report.get("query_generation").number();
        assert_eq!(report.get("hits").array().len(), 2);
        let before = (source.source_bytes_read(), source.read_calls());
        fs::rename(&path, root.join("old-evidence")).unwrap();
        fs::create_dir(&path).unwrap(); // Reopening as a regular source now fails.
        let mut desk = DeskSession::new(owner(), DeskLimits::default()).unwrap();
        for (index, row) in report.get("hits").array().iter().enumerate() {
            let excerpt = source.retained_excerpt(&atlas, generation, index, || false).unwrap();
            let passage = keep(&mut desk, &excerpt, row, &bytes);
            let attempt = desk.model().last_attempt() + 1;
            desk.edit_observed_passage(desk.model().revision(), attempt, passage, 1,
                &format!("Excerpt {index}"), &format!("note {index}"), || false).unwrap();
        }
        assert_eq!(source.source_bytes_read(), before.0); assert_eq!(source.read_calls(), before.1);
        assert_eq!(desk.model().observations().len(), 2);
        source.clear(&atlas).unwrap();
        assert!(matches!(source.retained_excerpt(&atlas, generation, 0, || false),
            Err(AtlasStreamSearchError::MissingQuery)));
        let checkpoint = root.join("captured-excerpts.fcbk");
        assert_eq!(desk.save_checkpoint(desk.model().revision(), &checkpoint, 4096, || false).error(), None);
        drop(source); drop(atlas); drop(desk);
        let mut restored = DeskSession::new(owner(), DeskLimits::default()).unwrap();
        restored.restore_checkpoint_file(0, 1, &checkpoint, || false).unwrap();
        let state = json(&restored.state(|| false).unwrap());
        for (index, note) in state.get("observed_passages").array().iter().enumerate() {
            assert_eq!(note.get("body").text(), format!("note {index}"));
            let read = json(&restored.observed_passage(1, note.get("passage").number(), || false).unwrap());
            assert_eq!(read.get("text"), report.get("hits").array()[index].get("context").get("text"));
        }
        let destination = root.join("captured-excerpts.json");
        assert_eq!(restored.export_source_pack(1, &destination, DeskSourcePackLimits::default(), || false).error(), None);
        let pack = parse(&fs::read(destination).unwrap()).unwrap();
        assert_eq!(pack.get("entries").array().len(), 2);
        assert!(pack.get("complete").flag()); assert!(!pack.get("live_roots_accessed").flag());
    }
}

#[test]
fn catalog_modes_bind_owner_query_revision_and_exact_full_text_range_without_new_reads() {
    for mode in [CatalogContentSearchMode::Text, CatalogContentSearchMode::Occurrences, CatalogContentSearchMode::Expression] {
        let root = fixture();
        let bytes = encoded(&format!("\u{feff}{} needle {} second {}", "Ω😀".repeat(90), "x".repeat(40), "z".repeat(200)), "utf16be");
        let path = root.join("candidate.rs"); fs::write(&path, &bytes).unwrap();
        let query = if mode == CatalogContentSearchMode::Expression { "needle second" } else { "needle" };
        let mut source = CatalogContentSearchSession::open(owner(), &root, 7, query,
            CatalogContentSearchOptions { mode, count_all_matches: true, ..Default::default() }, || false).unwrap();
        let report = finish(&mut source, 7);
        assert!(report.get("search_complete").flag()); assert_eq!(report.get("hits").array().len(), 1);
        let revision = report.get("revision").number();
        fs::write(&path, b"unrelated replacement source").unwrap();
        assert!(matches!(source.retained_excerpt(8, revision, 0, || false), Err(CatalogContentSearchError::StaleQuery)));
        assert!(matches!(source.retained_excerpt(7, revision - 1, 0, || false), Err(CatalogContentSearchError::StaleRevision)));
        assert!(matches!(source.retained_excerpt(7, revision, 1, || false), Err(CatalogContentSearchError::MissingHit)));
        assert!(source.retained_excerpt(7, revision, 0, || true).is_err());
        let mut desk = DeskSession::new(owner(), DeskLimits::default()).unwrap();
        let excerpt = source.retained_excerpt(7, revision, 0, || false).unwrap();
        let passage = keep(&mut desk, &excerpt, &report.get("hits").array()[0], &bytes);
        drop(excerpt);
        let after = json(&source.info(|| false).unwrap());
        assert_eq!(after.get("payload_bytes_read"), report.get("payload_bytes_read"));
        assert_eq!(after.get("read_calls"), report.get("read_calls"));
        drop(source);
        let read = json(&desk.observed_passage(desk.model().revision(), passage, || false).unwrap());
        assert!(read.get("text").text().contains("needle"));
        assert!(!read.get("text").text().contains("unrelated replacement"));
    }
}
