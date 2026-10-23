#![forbid(unsafe_code)]

use std::path::Path;
use fcb::ArenaOwnerId;
use super::*;
use super::super::{range, MAX_READER_WINDOW_BYTES};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn source(value: u64, path: &str, bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(owner(value), Path::new(path), bytes, || false).unwrap()
}
fn ready(reader: &ReaderSession) -> &Arc<HighlightedCapture> {
    match reader.highlight.as_ref().unwrap() {
        CaptureHighlight::Ready(ready) => ready,
        CaptureHighlight::Plain { state, .. } => panic!("expected lexical roles, got {state:?}"),
    }
}
fn highlighted(response: &str) {
    assert!(response.contains("\"highlight\":{\"state\":\"highlighted\",\"language\":\"rust\""), "{response}");
    assert!(response.contains("\"coordinate_space\":\"window-utf16\",\"source_context\":\"complete-capture\""));
}

#[test]
fn interior_comment_page_uses_complete_context_not_fresh_page_lexing() {
    let prefix = "/* source context\n";
    let fragment = "fn pretend() { 42 }\n";
    let bytes = format!("{prefix}{fragment}*/\nfn actual() {{}}\n");
    let mut reader = source(9601, "source.rs", bytes.as_bytes());
    let page = reader.read_window(prefix.len() as u64, fragment.len(), || false).unwrap();
    highlighted(page.as_str());
    assert!(page.as_str().contains(&format!("\"runs\":[{{\"start\":\"0\",\"length\":\"{}\",\"role\":\"comment\"}}]", fragment.len())));
    // Negative control: treating this arbitrary page as a whole program yields
    // keyword tokens. The retained page must preserve its enclosing comment.
    let mut standalone = source(9602, "source.rs", fragment.as_bytes());
    standalone.prepare_highlight(&mut || false).unwrap();
    assert!(ready(&standalone).tokens.runs.iter().any(|run| run.role == "keyword"));
    reader.search(1, "pretend", 8, bytes.len() as u64, || false).unwrap();
    highlighted(reader.hit_window(1, 0, 0, || false).unwrap().as_str());
    highlighted(reader.read_lines(2, 1, 1024, || false).unwrap().as_str());
}

#[test]
fn bom_sparse_unicode_crlf_and_embedded_feff_keep_exact_page_coordinates() {
    let prefix = format!("\u{feff}/* {}\r\n", "é🦀".repeat(1200));
    let fragment = "🦀\u{feff}\r\nstill";
    let bytes = format!("{prefix}{fragment}\n*/\nfn after() {{}}\n");
    let mut reader = source(9611, "source.rs", bytes.as_bytes());
    let response = reader.read_window(prefix.len() as u64, fragment.len(), || false).unwrap();
    highlighted(response.as_str());
    assert!(response.as_str().contains("\"text\":\"🦀\u{feff}\\r\\nstill\""));
    assert!(response.as_str().contains("\"runs\":[{\"start\":\"0\",\"length\":\"10\",\"role\":\"comment\"}]"));
    let cache = ready(&reader);
    assert_eq!(cache.checkpoints[0].byte, 3);
    assert!(cache.checkpoints.len() > 1);
    let page = cache.page(bytes.as_bytes(), range(prefix.len() as u64, (prefix.len() + fragment.len()) as u64).unwrap(),
        fragment, false, &mut || false).unwrap().unwrap();
    assert_eq!(page.start, prefix.trim_start_matches('\u{feff}').encode_utf16().count() as u64);
    assert_eq!(page.end - page.start, 10);
    let initial = reader.read_window(0, 4, || false).unwrap();
    highlighted(initial.as_str());
    assert!(initial.as_str().contains("\"visible_range\":{\"start\":\"3\",\"end\":\"4\"}"));
    assert!(initial.as_str().contains("\"text\":\"/\""));
}

#[test]
fn unsupported_capture_encoding_never_colors_a_valid_local_fragment() {
    let mut malformed = source(9621, "source.rs", b"fn valid() {}\n\xff");
    let response = malformed.read_window(0, 12, || false).unwrap();
    assert!(response.as_str().contains("\"has_replacements\":false"));
    assert!(response.as_str().contains("\"highlight\":{\"state\":\"unsupported_encoding\""));
    assert!(response.as_str().contains("\"runs\":[]"));
    for little in [true, false] {
        let bytes: Vec<u8> = "\u{feff}fn 🦀() {}\r\n".encode_utf16()
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        let mut reader = source(9622, "source.rs", &bytes);
        let response = reader.read_window(0, 1024, || false).unwrap();
        assert!(response.as_str().contains("fn 🦀() {}\\r\\n"));
        assert!(response.as_str().contains("\"highlight\":{\"state\":\"unsupported_encoding\""));
        assert!(response.as_str().contains("\"runs\":[]"));
    }
    let mut unknown = source(9623, "source.no-such-language", b"fn normal() {}");
    let response = unknown.read_window(0, 1024, || false).unwrap();
    assert!(response.as_str().contains("\"highlight\":{\"state\":\"plain\",\"language\":\"plain\""));
    // A genuine replacement character is valid source, unlike decoder repair.
    let mut valid = source(9624, "source.rs", "// �\nfn valid() {}".as_bytes());
    highlighted(valid.read_window(0, 1024, || false).unwrap().as_str());
    for bytes in [&b""[..], &b"\xef\xbb\xbf"[..]] {
        let mut empty = source(9625, "empty.rs", bytes);
        let response = empty.read_window(0, 4, || false).unwrap();
        highlighted(response.as_str());
        assert!(response.as_str().contains("\"text\":\"\""));
        assert!(response.as_str().contains("\"runs\":[]"));
    }
}

#[test]
fn excessive_page_runs_fall_back_without_poisoning_smaller_pages() {
    let bytes = "fn a() { let b = 1; }\n".repeat(2000);
    let mut reader = source(9631, "dense.rs", bytes.as_bytes());
    let response = reader.read_window(0, MAX_READER_WINDOW_BYTES, || false).unwrap();
    assert!(ready(&reader).tokens.runs.len() > MAX_PAGE_RUNS, "fixture must exceed the wire run bound");
    assert!(response.as_str().contains("\"highlight\":{\"state\":\"page_limit\""));
    assert!(response.as_str().contains("\"runs\":[]"));
    assert!(response.as_str().contains("\"range_limited\":false"));
    highlighted(reader.read_window(0, 64, || false).unwrap().as_str());
}

#[test]
fn optional_cache_admission_preserves_the_largest_readable_window() {
    let bytes = vec![b'a'; 1024 * 1024];
    let mut reader = source(9641, "large.rs", &bytes);
    let [pressure_id] = reader.allocations().unwrap();
    let pressure = reader.budget.try_reserve_managed(reader.owner(), pressure_id,
        ByteLength::new(WINDOW_HEADROOM)).unwrap();
    let response = reader.read_window(0, MAX_READER_WINDOW_BYTES, || false).unwrap();
    assert!(response.as_str().contains("\"highlight\":{\"state\":\"admission_denied\""));
    assert!(response.as_str().contains("\"runs\":[]"));
    assert!(response.as_str().contains(&format!("\"next_offset\":\"{MAX_READER_WINDOW_BYTES}\"")));
    drop(response);
    drop(pressure);
    assert_eq!(reader.budget.accounting().active_allocations(), 2, "fallback retains no optional allocations");
}

#[test]
fn cached_roles_are_shared_only_by_explicit_forks_and_release_with_last_owner() {
    let mut parent = source(9651, "retained.rs", b"fn first() {}\nfn second() {}\n");
    drop(parent.read_window(0, 8, || false).unwrap());
    let original_budget = parent.budget.clone();
    let reserved = original_budget.accounting().reserved();
    let pointer = Arc::as_ptr(ready(&parent));
    for offset in [4, 12, 0] {
        drop(parent.read_window(offset, 8, || false).unwrap());
        assert_eq!(Arc::as_ptr(ready(&parent)), pointer);
        assert_eq!(original_budget.accounting().reserved(), reserved);
    }
    let mut child = parent.fork(owner(9652), || false).unwrap();
    assert_eq!(Arc::as_ptr(ready(&child)), pointer);
    assert_eq!(child.budget.accounting().active_allocations(), 1);
    let independent = source(9653, "retained.rs", parent.capture.bytes());
    assert!(independent.highlight.is_none());
    drop(parent);
    highlighted(child.read_window(0, 64, || false).unwrap().as_str());
    assert!(original_budget.accounting().reserved().get() > 0);
    drop(child);
    assert_eq!(original_budget.accounting().reserved().get(), 0);
}

#[test]
fn cancellation_at_every_preparation_boundary_drops_candidate_and_allows_retry() {
    let bytes = "// unicode é🦀\n".repeat(800);
    let mut reference = source(9661, "source.rs", bytes.as_bytes());
    let mut polls = 0;
    reference.prepare_highlight(&mut || { polls += 1; false }).unwrap();
    assert!(polls > 6, "fixture must reach sparse checkpoint preparation");
    for stop in 1..=polls {
        let mut reader = source(9662, "source.rs", bytes.as_bytes());
        let before = reader.budget.accounting().reserved();
        let mut calls = 0;
        let result = reader.prepare_highlight(&mut || { calls += 1; calls == stop });
        assert_eq!(result.err(), Some(ReaderSessionError::Canceled), "boundary {stop}");
        assert!(reader.highlight.is_none());
        assert_eq!(reader.budget.accounting().reserved(), before);
        highlighted(reader.read_window(0, 64, || false).unwrap().as_str());
    }
}
