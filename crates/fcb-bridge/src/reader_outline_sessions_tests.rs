#![forbid(unsafe_code)]
use super::*;
use crate::atlas_sessions::{AtlasSessions, Command as AtlasCommand};
use fcb_app::host::atlas_search::AtlasSearchOptions;
use std::fs;

fn opened(readers: &ReaderSessions, source: &[u8]) -> u64 {
    let id = readers.create().unwrap(); readers.supply(id, source).unwrap(); id
}
fn outline(readers: &ReaderSessions, handle: u64, generation: u64) -> HostResponse {
    readers.execute(handle, Command::Outline { generation, options: Default::default() }, || false).unwrap()
}
fn names(readers: &ReaderSessions, handle: u64, generation: u64, needle: &str) -> HostResponse {
    readers.execute(handle, Command::Symbols { generation, needle, mode: SymbolNameMode::Exact, start: 0, limit: 128 }, || false).unwrap()
}

#[test]
fn repository_search_reader_outline_and_symbol_remain_on_original_capture() {
    let root = std::env::temp_dir().join(format!("fcb-outline-journey-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    fs::create_dir(&root).unwrap(); fs::write(root.join("source.rs"), b"fn original() {}\n").unwrap();
    let atlases = AtlasSessions::new(); let readers = ReaderSessions::new();
    let atlas = atlases.create().unwrap(); atlases.open(atlas, &root, Default::default(), || false).unwrap();
    atlases.execute(atlas, AtlasCommand::Search { generation: 1, needle: "original", options: AtlasSearchOptions::default() }, || false).unwrap();
    fs::write(root.join("source.rs"), b"fn replacement() {}\n").unwrap();
    let reader = readers.create().unwrap();
    let link = atlases.open_search_reader(atlas, &readers, reader, 1, 1, || false).unwrap();
    assert!(link.as_str().contains("\"source_reopened\":false"));
    atlases.close(atlas).unwrap();
    let out = outline(&readers, reader, 1);
    assert!(out.as_str().contains("\"name\":\"original\""));
    assert!(names(&readers, reader, 1, "replacement").as_str().contains("\"matched_symbols\":\"0\""));
    let window = readers.execute(reader, Command::Symbol { generation: 1, id: 1, context: 100 }, || false).unwrap();
    assert!(window.as_str().contains("fn original() {}"));
    assert!(window.as_str().contains("\"window_utf8_range\":{\"start\":\"3\",\"end\":\"11\"}"));
    let copy = readers.execute(reader, Command::CopySymbol { generation: 1, id: 1, whole_declaration: false }, || false).unwrap();
    readers.close(reader).unwrap();
    assert!(copy.as_str().contains("\"original_hex\":\"6f726967696e616c\""));
}

#[test]
fn cancel_inside_outline_preserves_previous_generation_and_reading() {
    let readers = ReaderSessions::new(); let r = opened(&readers, b"fn keep() {}\n");
    outline(&readers, r, 1); let mut fired = false;
    let result = readers.execute(r, Command::Outline { generation: 2, options: Default::default() }, || {
        if !fired { fired = true; readers.cancel(r).unwrap(); } false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    assert!(names(&readers, r, 1, "keep").as_str().contains("keep"));
    assert!(readers.execute(r, Command::Window { offset: 0, bytes: 100 }, || false).is_ok());
    outline(&readers, r, 3); readers.close(r).unwrap();
}

#[test]
fn one_shot_extractor_cancellation_is_an_explicit_canceled_wire_outcome() {
    let readers = ReaderSessions::new(); let r = opened(&readers, b"fn keep() {}\n");
    outline(&readers, r, 1); let mut calls = 0;
    let error = readers.execute(r, Command::Outline { generation: 2, options: Default::default() }, || {
        calls += 1; calls == 2
    }).err().unwrap();
    assert_eq!(error, AccessError::Reader(ReaderSessionError::Symbol(SymbolError::Canceled)));
    assert!(error.canceled()); assert!(error.json(r).contains("\"exit_code\":\"130\""));
    assert!(names(&readers, r, 1, "keep").as_str().contains("keep"));
    readers.close(r).unwrap();
}

#[test]
fn busy_reader_does_not_block_other_readers_or_cancellation() {
    let readers = ReaderSessions::new();
    let a = opened(&readers, b"fn first() {}\n"); let b = opened(&readers, b"fn second() {}\n");
    outline(&readers, a, 1); outline(&readers, b, 1);
    let cell = readers.get(a).unwrap(); let guard = lock(&cell.state).unwrap();
    assert_eq!(readers.execute(a, Command::Symbol { generation: 1, id: 1, context: 10 }, || false).err(), Some(AccessError::Busy));
    assert!(names(&readers, b, 1, "second").as_str().contains("second"));
    readers.cancel(a).unwrap(); drop(guard); drop(cell);
    assert!(names(&readers, a, 1, "first").as_str().contains("first"));
    readers.close(a).unwrap(); readers.close(b).unwrap();
}

#[test]
fn closing_during_extraction_retains_capacity_until_operation_drains() {
    let readers = ReaderSessions::new();
    let ids: Vec<_> = (0..MAX_READER_SESSIONS).map(|_| readers.create().unwrap()).collect();
    readers.supply(ids[0], b"fn old() {}\n").unwrap();
    let retained = outline(&readers, ids[0], 1); let mut fired = false;
    let result = readers.execute(ids[0], Command::Outline { generation: 2, options: Default::default() }, || {
        if !fired {
            fired = true; readers.close(ids[0]).unwrap();
            assert_eq!(readers.live.load(Ordering::Acquire), MAX_READER_SESSIONS);
            assert_eq!(readers.create().err(), Some(AccessError::Capacity));
        }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    assert_eq!(readers.live.load(Ordering::Acquire), MAX_READER_SESSIONS - 1);
    let next = readers.create().unwrap(); assert_ne!(next, ids[0]); readers.close(next).unwrap();
    for &id in &ids[1..] { readers.close(id).unwrap(); }
    assert_eq!(readers.live.load(Ordering::Acquire), 0);
    assert_eq!(readers.budget.accounting().reserved().get(), 0);
    assert!(retained.as_str().contains("\"name\":\"old\""));
}

#[test]
fn outline_clear_never_clears_content_search_or_another_reader() {
    let readers = ReaderSessions::new();
    let a = opened(&readers, b"fn alpha() {}\n"); let b = opened(&readers, b"fn beta() {}\n");
    readers.execute(a, Command::Find { generation: 1, needle: "alpha", limit: 10, scan_bytes: 1024 }, || false).unwrap();
    outline(&readers, a, 1); outline(&readers, b, 1);
    readers.execute(a, Command::ClearOutline { generation: 2 }, || false).unwrap();
    assert_eq!(readers.execute(a, Command::Symbol { generation: 1, id: 1, context: 10 }, || false).err(), Some(AccessError::Reader(ReaderSessionError::MissingOutline)));
    assert!(readers.execute(a, Command::CopyHit { generation: 1, index: 0 }, || false).unwrap().as_str().contains("616c706861"));
    assert!(names(&readers, b, 1, "beta").as_str().contains("beta"));
    readers.close(a).unwrap(); readers.close(b).unwrap();
}

#[test]
fn stale_ids_and_unopened_handles_do_not_create_or_replace_sources() {
    let readers = ReaderSessions::new(); let r = readers.create().unwrap();
    assert_eq!(readers.execute(r, Command::Outline { generation: 1, options: Default::default() }, || false).err(), Some(AccessError::NotOpen));
    readers.supply(r, b"fn real() {}\n").unwrap(); outline(&readers, r, 1); outline(&readers, r, 2);
    assert_eq!(readers.execute(r, Command::CopySymbol { generation: 1, id: 1, whole_declaration: false }, || false).err(), Some(AccessError::Reader(ReaderSessionError::Symbol(SymbolError::StaleQuery))));
    assert_eq!(readers.execute(r, Command::Symbol { generation: 2, id: 0, context: 10 }, || false).err(), Some(AccessError::Reader(ReaderSessionError::Symbol(SymbolError::NotFound))));
    readers.close(r).unwrap();
    assert_eq!(readers.execute(r, Command::Symbol { generation: 2, id: 1, context: 10 }, || false).err(), Some(AccessError::UnknownHandle));
}

#[test]
fn canceled_filtered_delivery_does_not_invalidate_the_built_inventory() {
    let readers = ReaderSessions::new(); let r = opened(&readers, b"fn alpha() {}\nfn beta() {}\n");
    outline(&readers, r, 1);
    let result = readers.execute(r, Command::Symbols { generation: 1, needle: "a", mode: SymbolNameMode::Contains, start: 0, limit: 128 }, || true);
    assert!(result.err().unwrap().canceled());
    assert!(names(&readers, r, 1, "beta").as_str().contains("\"symbol_id\":\"2\""));
    readers.close(r).unwrap();
}
