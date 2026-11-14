#![forbid(unsafe_code)]
#![cfg(feature = "search")]

//! Owning workers must survive the request-building stack without leaking the
//! compiled query, changing match semantics, or coupling independent workers.

use std::{io::{Cursor, Read}, sync::Arc};
use fcb::{ArenaOwnerId, ByteLength, FileId, SourceRevision};
use fcb::search::{CaptureRequest, ParsedQuery, QueryGeneration, ReaderSearch,
    ResourceAllocationId, ResourceBudget, StreamReadOptions, StreamReadState,
    StreamReadStep, StreamingNeedle};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(1791).unwrap() }
fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
fn generation(id: u64) -> QueryGeneration { QueryGeneration::new(owner(), id).unwrap() }
fn request(id: u64) -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), id).unwrap(), SourceRevision::new(owner(), id).unwrap()).unwrap()
}
fn budget() -> ResourceBudget {
    ResourceBudget::new(owner(), ByteLength::new(32 * 1024 * 1024)).unwrap()
}
fn finish<R: Read>(query: &mut ReaderSearch<'_, R>) {
    let step = StreamReadStep { max_bytes: 3, max_calls: 1, max_hits: 1 };
    for _ in 0..100_000 {
        if query.state() != StreamReadState::Pending { return; }
        query.step(step, query.generation(), || false).unwrap();
        assert!(query.stats().last_step_read <= step.max_bytes);
        assert!(query.stats().last_step_scanned <= step.max_bytes);
        assert!(query.stats().last_step_calls <= step.max_calls);
    }
    panic!("owned search failed to make bounded progress");
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut result = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        result.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    result
}

#[test]
fn owned_worker_and_finished_report_keep_the_compiled_query_until_the_last_consumer_drops() {
    let budget = budget();
    let (mut query, weak): (ReaderSearch<'static, Cursor<Vec<u8>>>, _) = {
        let pattern = Arc::new(StreamingNeedle::text(owner(), "ana", &budget, allocation(1)).unwrap());
        let weak = Arc::downgrade(&pattern);
        let query = ReaderSearch::new_owned_page(Cursor::new(b"banana".to_vec()), request(1),
            ByteLength::new(6), pattern, StreamReadOptions::new(generation(1)), 0, true,
            &budget, allocation(2)).unwrap();
        (query, weak)
    };
    assert!(weak.upgrade().is_some());
    let charged = budget.accounting().reserved().get();
    assert!(charged > 0);
    finish(&mut query);
    assert_eq!(budget.accounting().reserved().get(), charged);
    let (_, report) = query.finish().unwrap();
    assert!(report.input_complete());
    assert_eq!(report.matches_seen(), 2);
    assert_eq!(report.hits().iter().map(|hit| hit.original_range().start().get()).collect::<Vec<_>>(), [1, 3]);
    assert_eq!(report.witness_bytes(0).unwrap(), b"ana");
    assert_eq!(report.matched_text(1).unwrap(), "ana");
    assert!(weak.upgrade().is_some(), "the report still owns exact-match witness backing");
    drop(report);
    assert!(weak.upgrade().is_none());
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn owned_result_windows_preserve_all_existing_match_modes_and_original_encoding() {
    let text = "banana ana ANA ana! \u{212a} K k required";
    for bytes in [text.as_bytes().to_vec(), utf16(text, true), utf16(text, false)] {
        for mode in 0..5 {
            let budget = budget();
            let pattern = Arc::new(match mode {
                0 => StreamingNeedle::text(owner(), "ana", &budget, allocation(1)),
                1 => StreamingNeedle::text_matching(owner(), "k", false, &budget, allocation(1)),
                2 => StreamingNeedle::identifier(owner(), "ana", &budget, allocation(1)),
                3 => StreamingNeedle::expression(owner(), &ParsedQuery::parse("ana required -forbidden").unwrap(),
                    true, &budget, allocation(1)),
                _ => StreamingNeedle::raw(owner(), b"a", &budget, allocation(1)),
            }.unwrap());
            let mut options = StreamReadOptions::new(generation(1));
            options.max_matches = 2;
            let mut borrowed = ReaderSearch::new_page(Cursor::new(bytes.clone()), request(1),
                ByteLength::new(bytes.len() as u64), &pattern, options, 1, true,
                &budget, allocation(2)).unwrap();
            finish(&mut borrowed);
            let (_, reference) = borrowed.finish().unwrap();
            let expected = reference.hits().iter().enumerate().map(|(i, hit)|
                (hit.occurrence_id(), hit.original_range(), reference.witness_bytes(i).unwrap().to_vec(),
                    reference.matched_text(i).ok().map(str::to_owned))).collect::<Vec<_>>();
            let count = reference.matches_seen();
            let qualification = reference.expression_qualified();
            let more = reference.has_more_matches();
            assert!(reference.input_complete());
            drop(reference);

            let mut owned: ReaderSearch<'static, _> = ReaderSearch::new_owned_page(Cursor::new(bytes.clone()),
                request(1), ByteLength::new(bytes.len() as u64), pattern, options, 1, true,
                &budget, allocation(3)).unwrap();
            finish(&mut owned);
            let (_, actual) = owned.finish().unwrap();
            assert!(actual.input_complete());
            assert_eq!(actual.matches_seen(), count);
            assert_eq!(actual.expression_qualified(), qualification);
            assert_eq!(actual.page_start_after(), 1);
            assert_eq!(actual.skipped_matches(), count.min(1));
            assert_eq!(actual.has_more_matches(), more);
            assert_eq!(actual.stats().bytes_read, bytes.len() as u64);
            let hits = actual.hits().iter().enumerate().map(|(i, hit)|
                (hit.occurrence_id(), hit.original_range(), actual.witness_bytes(i).unwrap().to_vec(),
                    actual.matched_text(i).ok().map(str::to_owned))).collect::<Vec<_>>();
            assert_eq!(hits, expected, "mode={mode}, encoding={:?}", actual.encoding());
            for (i, hit) in actual.hits().iter().enumerate() {
                let (start, end) = hit.original_range().as_usize_bounds().unwrap();
                assert_eq!(actual.witness_bytes(i).unwrap(), &bytes[start..end]);
            }
            drop(actual);
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }
}

#[test]
fn canceling_one_owned_worker_does_not_release_or_cancel_a_shared_query_in_another_worker() {
    let budget = budget();
    let pattern = Arc::new(StreamingNeedle::text(owner(), "needle", &budget, allocation(1)).unwrap());
    let weak = Arc::downgrade(&pattern);
    let mut first = ReaderSearch::new_owned_page(Cursor::new(b"needle".to_vec()), request(1),
        ByteLength::new(6), pattern.clone(), StreamReadOptions::new(generation(1)), 0, true,
        &budget, allocation(2)).unwrap();
    let mut second = ReaderSearch::new_owned_page(Cursor::new(b"needle needle".to_vec()), request(2),
        ByteLength::new(13), pattern, StreamReadOptions::new(generation(2)), 0, true,
        &budget, allocation(3)).unwrap();
    first.cancel();
    assert_eq!(first.state(), StreamReadState::Canceled);
    drop(first);
    assert!(weak.upgrade().is_some());
    assert_eq!(second.state(), StreamReadState::Pending);
    finish(&mut second);
    let (_, report) = second.finish().unwrap();
    assert!(report.input_complete());
    assert_eq!(report.matches_seen(), 2);
    assert_eq!(report.generation(), generation(2));
    drop(report);
    assert!(weak.upgrade().is_none());
    assert_eq!(budget.accounting().reserved().get(), 0);
}

#[test]
fn partial_or_invalid_owned_expression_never_publishes_candidate_hits() {
    for invalid in [false, true] {
        let budget = budget();
        let pattern = Arc::new(StreamingNeedle::expression(owner(),
            &ParsedQuery::parse("ana required -forbidden").unwrap(), true, &budget, allocation(1)).unwrap());
        let mut source = b"banana required tail".to_vec();
        if invalid { source.push(0xff); }
        let mut options = StreamReadOptions::new(generation(1));
        if !invalid { options.max_bytes = 12; }
        let mut query = ReaderSearch::new_owned_page(Cursor::new(source.clone()), request(1),
            ByteLength::new(source.len() as u64), pattern, options, 0, true,
            &budget, allocation(2)).unwrap();
        // Invalid decoding and the byte-limit terminal can return an error;
        // the retained terminal report must still withhold every candidate.
        for _ in 0..1000 {
            if query.state() != StreamReadState::Pending { break; }
            let _ = query.step(StreamReadStep::default(), generation(1), || false);
        }
        assert_ne!(query.state(), StreamReadState::Pending);
        let (_, report) = query.finish().unwrap();
        assert!(!report.input_complete());
        assert_eq!(report.expression_qualified(), None);
        assert!(report.hits().is_empty());
        assert_eq!(report.matches_seen(), 0);
    }
}

#[cfg(unix)]
#[test]
fn owned_file_worker_retains_old_match_bytes_without_moving_the_hosts_file_cursor() {
    use std::{fs::{self, File}, io::{Seek, SeekFrom}, sync::atomic::{AtomicU64, Ordering}};
    use fcb::{BrowserSession};
    use fcb::search::FileSearch;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-owned-stream-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let path = root.join("source.txt");
    fs::write(&path, b"before needle after").unwrap();
    let mut host = File::open(&path).unwrap();
    host.seek(SeekFrom::Start(2)).unwrap();
    let budget = budget();
    let mut worker: FileSearch<'static> = {
        let pattern = Arc::new(StreamingNeedle::text(owner(), "needle", &budget, allocation(1)).unwrap());
        FileSearch::new_owned_page(host.try_clone().unwrap(), request(1), pattern,
            StreamReadOptions::new(generation(1)), 0, true, &budget, allocation(2)).unwrap()
    };
    for _ in 0..1000 {
        if worker.state() != StreamReadState::Pending { break; }
        worker.step(StreamReadStep { max_bytes: 2, max_calls: 1, max_hits: 1 }, generation(1), || false).unwrap();
    }
    let report = worker.finish().unwrap();
    assert!(report.is_complete());
    assert_eq!(host.stream_position().unwrap(), 2);
    fs::write(&path, b"replacement source").unwrap();
    let witness = report.retain_hit(0, &budget, allocation(3)).unwrap();
    assert_eq!(witness.range().start().get(), 7);
    assert_eq!(witness.range().end().get(), 13);
    drop(report);
    let view = BrowserSession::new(owner()).open_extent(witness.clone()).unwrap();
    assert_eq!(view.raw_selection(witness.range()).unwrap(), b"needle");
    // Keep the exact input/replacement fixture for independent qualification.
}
