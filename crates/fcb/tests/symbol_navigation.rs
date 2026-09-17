#![forbid(unsafe_code)]
#![cfg(all(feature = "analysis", feature = "search"))]

//! Public facade only: actual outline extraction, byte mapping and source reader.
//! Does not claim compiler name resolution, native rendering, or execution proof.

use std::{io::Cursor, sync::{Arc, atomic::{AtomicUsize, Ordering}}};
use fcb::{ArenaOwnerId, BrowserSession, ByteLength, FcbError, FileId, SourceCapture,
    SourceProvider, SourceRevision};
use fcb::search::{QueryGeneration, ReaderLimits, ReadingSeek, ReadingSeekState,
    ReadingWindowOptions, ResourceAllocationId, ResourceBudget};
use fcb::search::symbols::{SymbolError, SymbolLanguage, SymbolNameMode,
    SymbolNavigationError, SymbolOptions};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(841).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap() }
fn source(revision: u64, bytes: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(), FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), revision).unwrap(),
        "same.rs", bytes.to_vec()).unwrap()
}
fn ready(seek: &mut ReadingSeek<'_>) -> fcb::search::ReadingAnchor {
    for _ in 0..10000 {
        match seek.state() {
            ReadingSeekState::Ready(anchor) => return anchor,
            ReadingSeekState::Pending => { seek.step(4, seek.generation(), || false).unwrap(); }
            other => panic!("unexpected seek state {other:?}"),
        }
    }
    panic!("source seek failed to progress")
}
struct Provider { calls: AtomicUsize }
impl SourceProvider for Provider {
    fn capture(&self, _: &str) -> Result<SourceCapture, FcbError> {
        let call = self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(source(call as u64 + 1, if call == 0 { b"fn original() {}\n" } else { b"fn replacement() {}\n" }))
    }
}

#[test]
fn selected_identifier_opens_its_exact_utf16_bytes_and_keeps_duplicate_candidates_distinct() {
    let text = "// 😀\r\nimpl Thing { fn run(&self) {} }\r\nfn run() {}\n";
    for little in [false, true] {
        let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        for unit in text.encode_utf16() { bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() }); }
        let session = BrowserSession::new(owner());
        let prepared = session.prepare_search_capture(source(1, &bytes)).unwrap();
        let view = session.open_capture(prepared.source().clone()).unwrap();
        let budget = budget();
        let symbols = prepared.symbols(SymbolOptions::new(generation(1), SymbolLanguage::Rust), &budget, allocation(1), || false).unwrap();
        let runs: Vec<_> = symbols.candidates().iter().filter(|item| item.matches_name("run", SymbolNameMode::Exact)).collect();
        assert_eq!(runs.len(), 2);
        assert_ne!(runs[0].id(), runs[1].id());
        assert!(runs[0].parent_id().is_some()); assert!(runs[1].parent_id().is_none());
        let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(2)).unwrap();
        for (index, candidate) in runs.into_iter().enumerate() {
            let mut seek = reader.seek_symbol(&symbols, candidate.id(), generation(1)).unwrap();
            let at = ready(&mut seek);
            assert_eq!(at.line_number(), index as u64 + 2);
            assert_eq!(at.selection(), candidate.name_range());
            let window = reader.window(at, generation(1), ReadingWindowOptions::default(), &budget, allocation(3), || false).unwrap();
            let decoded = window.source_to_text(candidate.name_range().unwrap()).unwrap();
            let selected = window.text_selection(decoded).unwrap();
            assert_eq!(selected.text, "run");
            assert_eq!(selected.original_bytes, symbols.name_bytes(candidate.id()).unwrap().unwrap());
            assert_eq!(window.frame_plan().source(), prepared.source().revision());
        }
    }
}

#[test]
fn refreshing_a_provider_does_not_relabel_or_recapture_old_symbol_targets() {
    let provider = Arc::new(Provider { calls: AtomicUsize::new(0) });
    let session = BrowserSession::with_provider(owner(), provider.clone());
    let old = session.open("same.rs").unwrap();
    let budget = budget();
    let symbols = old.symbols(SymbolOptions::new(generation(1), SymbolLanguage::Rust), &budget, allocation(1), || false).unwrap();
    assert_eq!(symbols.candidates()[0].name(), "original");
    let refreshed = session.open("same.rs").unwrap();
    let next_reader = refreshed.source_reader(ReaderLimits::default(), &budget, allocation(2)).unwrap();
    assert!(matches!(next_reader.seek_symbol(&symbols, 1, generation(1)),
        Err(SymbolNavigationError::Symbol(SymbolError::StaleSource))));
    drop(next_reader);
    let old_reader = old.source_reader(ReaderLimits::default(), &budget, allocation(2)).unwrap();
    let mut seek = old_reader.seek_symbol(&symbols, 1, generation(1)).unwrap();
    assert_eq!(ready(&mut seek).offset().get(), 3);
    assert_eq!(symbols.name_bytes(1).unwrap(), Some(b"original".as_slice()));
    assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    session.close();
    assert_eq!(symbols.source_bytes(1).unwrap(), b"fn original() {}");
}

#[test]
fn changed_bytes_under_a_reused_identity_and_foreign_queries_cannot_select_source() {
    let session = BrowserSession::new(owner());
    let old = session.open_capture(source(1, b"fn original() {}\n")).unwrap();
    let changed = session.open_capture(source(1, b"fn new_name() {}\n")).unwrap();
    let budget = budget();
    let symbols = old.symbols(SymbolOptions::new(generation(1), SymbolLanguage::Rust), &budget, allocation(1), || false).unwrap();
    let reader = changed.source_reader(ReaderLimits::default(), &budget, allocation(2)).unwrap();
    assert!(matches!(reader.seek_symbol(&symbols, 1, generation(1)), Err(SymbolNavigationError::Symbol(SymbolError::StaleSource))));
    drop(reader);
    let reader = old.source_reader(ReaderLimits::default(), &budget, allocation(2)).unwrap();
    assert!(matches!(reader.seek_symbol(&symbols, 1, generation(2)), Err(SymbolNavigationError::Symbol(SymbolError::StaleQuery))));
    let foreign = QueryGeneration::new(ArenaOwnerId::new(842).unwrap(), 1).unwrap();
    assert!(matches!(reader.seek_symbol(&symbols, 1, foreign), Err(SymbolNavigationError::Symbol(SymbolError::OwnerMismatch))));
    assert!(matches!(reader.seek_symbol(&symbols, 99, generation(1)), Err(SymbolNavigationError::Symbol(SymbolError::NotFound))));
}

#[test]
fn line_based_candidates_use_declaration_evidence_without_inventing_identifier_ranges() {
    let session = BrowserSession::new(owner());
    let view = session.open_capture(source(1, b"class Thing:\n    def work(self):\n        return 1\n")).unwrap();
    let budget = budget();
    let symbols = view.symbols(SymbolOptions::new(generation(1), SymbolLanguage::Python), &budget, allocation(1), || false).unwrap();
    let method = symbols.candidates().iter().find(|item| item.name() == "work").unwrap();
    assert_eq!(method.name_range(), None);
    let reader = view.source_reader(ReaderLimits::default(), &budget, allocation(2)).unwrap();
    let mut seek = reader.seek_symbol(&symbols, method.id(), generation(1)).unwrap();
    let at = ready(&mut seek);
    assert_eq!(at.selection(), Some(method.original_range()));
    let window = reader.window(at, generation(1), ReadingWindowOptions::default(), &budget, allocation(3), || false).unwrap();
    assert_eq!(window.line_text(0), Some("    def work(self):"));
}

#[cfg(feature = "snapshot")]
#[test]
fn verified_snapshot_symbols_and_reader_survive_closing_the_archive() {
    use fcb::search::snapshot::{SnapshotBytes, SnapshotData, SnapshotEntry, SnapshotLimits};
    use fcb::search::paged_snapshot::{PagedSnapshot, PagedCapture};
    let budget = budget();
    let data = b"// saved bytes\nfn saved() {}\n";
    let encoded = SnapshotBytes::encode(owner(), true, "symbols-test", &[SnapshotEntry {
        path: b"src/\xff.rs", observed_bytes: data.len() as u64, data: SnapshotData::Captured(data) }],
        SnapshotLimits::default(), &budget, allocation(1), || false).unwrap();
    let mut archive = PagedSnapshot::open(Cursor::new(encoded.bytes()), owner(), SnapshotLimits::default(),
        &budget, allocation(2), || false).unwrap();
    let capture = PagedCapture::load(&mut archive, 0, FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 4).unwrap(),
        &budget, [allocation(3), allocation(4)], || false).unwrap();
    drop(archive); drop(encoded);
    let symbols = capture.symbols(SymbolOptions::new(generation(1), SymbolLanguage::Rust), &budget, allocation(1), || false).unwrap();
    assert_eq!(symbols.candidates()[0].name(), "saved");
    let target = capture.symbol_target(&symbols, 1, generation(1)).unwrap();
    let reader = capture.reader(ReaderLimits::default(), &budget, allocation(2)).unwrap();
    let mut seek = reader.seek(target, generation(1)).unwrap();
    let at = ready(&mut seek);
    let window = reader.window(at, generation(1), ReadingWindowOptions::default(), &budget, allocation(3), || false).unwrap();
    assert_eq!(window.line_text(0), Some("saved() {}"));
    assert_eq!(window.frame_plan().source().get(), 4);
    assert_eq!(capture.bytes(), data);
}
