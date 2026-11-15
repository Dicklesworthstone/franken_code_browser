#![forbid(unsafe_code)]

use super::*;

fn encoded(text: &str, little: bool) -> Vec<u8> {
    std::iter::once(0xfeff).chain(text.encode_utf16())
        .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn raw_range(start: usize, end: usize) -> ByteRange {
    range(start as u64, end as u64).unwrap()
}

#[test]
fn both_byte_orders_share_upstream_roles_and_keep_original_capture_bytes() {
    let text = "/* é🦀 e\u{301}\r\n*/\r\nfn actual() { let s = \"界\u{feff}�\"; }\0\r\n";
    let mut utf8 = source(9671, "source.rs", text.as_bytes());
    utf8.prepare_highlight(&mut || false).unwrap();
    for little in [true, false] {
        let bytes = encoded(text, little);
        let mut reader = source(9672, "source.rs", &bytes);
        let response = reader.read_window(0, bytes.len(), || false).unwrap();
        highlighted(response.as_str());
        assert!(response.as_str().contains("\"has_replacements\":false"));
        assert!(response.as_str().contains(if little { "\"encoding\":\"utf16le\"" } else { "\"encoding\":\"utf16be\"" }));
        assert_eq!(ready(&reader).tokens.runs, ready(&utf8).tokens.runs);
        assert!(ready(&reader).checkpoints.is_empty(), "fixed-width source units need no sparse byte index");
        let page = ready(&reader).page(&bytes, raw_range(2, bytes.len()), text, false, &mut || false)
            .unwrap().unwrap();
        assert_eq!((page.start, page.end), (0, text.encode_utf16().count() as u64));
        assert!(response.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&bytes[2..]))));
        let copied = reader.copy_range(0, bytes.len() as u64, || false).unwrap();
        assert!(copied.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&bytes))));
        assert!(copied.as_str().contains("\"copy_domain\":\"original-bytes\""));
        assert_eq!(reader.capture.bytes(), bytes);
    }
}

#[test]
fn interior_comment_windows_lines_and_hits_retain_complete_utf16_context() {
    let prefix = "/* enclosing 🦀 comment\r\n";
    let fragment = "fn pretend() { 42 }\r\n";
    let text = format!("{prefix}{fragment}*/\r\nfn actual() {{}}\r\n");
    let start = 2 + prefix.encode_utf16().count() * 2;
    let length = fragment.encode_utf16().count() * 2;
    for little in [true, false] {
        let bytes = encoded(&text, little);
        let mut reader = source(9673, "source.rs", &bytes);
        let response = reader.read_window(start as u64, length, || false).unwrap();
        highlighted(response.as_str());
        assert!(response.as_str().contains(&format!("\"runs\":[{{\"start\":\"0\",\"length\":\"{}\",\"role\":\"comment\"}}]",
            fragment.encode_utf16().count())));
        assert!(response.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&bytes[start..start + length]))));
        let page = ready(&reader).page(&bytes, raw_range(start, start + length), fragment, false, &mut || false)
            .unwrap().unwrap();
        assert_eq!(page.start, prefix.encode_utf16().count() as u64);
        assert_eq!(page.end - page.start, fragment.encode_utf16().count() as u64);
        highlighted(reader.read_lines(2, 1, 1024, || false).unwrap().as_str());
        reader.search(1, "pretend", 8, bytes.len() as u64, || false).unwrap();
        highlighted(reader.hit_window(1, 0, 0, || false).unwrap().as_str());
        let copied = reader.copy_hit(1, 0, || false).unwrap();
        assert!(copied.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&encoded("pretend", little)[2..]))));
        let mut fragment_only = source(9674, "source.rs", &encoded(fragment, little));
        fragment_only.prepare_highlight(&mut || false).unwrap();
        assert!(ready(&fragment_only).tokens.runs.iter().any(|run| run.role == "keyword"),
            "negative control must expose fresh-page lexing");
    }
}

#[test]
fn surrogate_crossing_decode_chunk_retains_one_scalar_and_exact_later_coordinates() {
    // BOM and prefix occupy 4094 raw bytes: the high surrogate is the last
    // complete unit of chunk one and the low surrogate begins chunk two.
    let prefix = format!("/*{}", "x".repeat(CHECKPOINT_BYTES / 2 - 4));
    assert_eq!(2 + prefix.encode_utf16().count() * 2, CHECKPOINT_BYTES - 2);
    let fragment = "🦀\u{feff}\r\nstill";
    let text = format!("{prefix}{fragment}*/\nfn next() {{}}\n");
    let mut utf8 = source(9675, "source.rs", text.as_bytes());
    utf8.prepare_highlight(&mut || false).unwrap();
    for little in [true, false] {
        let bytes = encoded(&text, little);
        let mut reader = source(9676, "source.rs", &bytes);
        let start = CHECKPOINT_BYTES - 2;
        let end = start + fragment.encode_utf16().count() * 2;
        let response = reader.read_window(start as u64, end - start, || false).unwrap();
        highlighted(response.as_str());
        assert!(response.as_str().contains("\"text\":\"🦀\u{feff}\\r\\nstill\""));
        assert!(response.as_str().contains("\"runs\":[{\"start\":\"0\",\"length\":\"10\",\"role\":\"comment\"}]"));
        assert_eq!(ready(&reader).tokens.runs, ready(&utf8).tokens.runs);
        let page = ready(&reader).page(&bytes, raw_range(start, end), fragment, false, &mut || false)
            .unwrap().unwrap();
        assert_eq!(page.start, prefix.encode_utf16().count() as u64);
        assert_eq!(page.end - page.start, 10);
    }
}

#[test]
fn malformed_utf16_elsewhere_never_colors_a_valid_local_page() {
    for little in [true, false] {
        let prefix = encoded("fn valid() {}\r\n", little);
        let malformed: Vec<Vec<u8>> = vec![
            vec![0x00],
            (if little { 0xd800u16.to_le_bytes() } else { 0xd800u16.to_be_bytes() }).to_vec(),
            (if little { 0xdc00u16.to_le_bytes() } else { 0xdc00u16.to_be_bytes() }).to_vec(),
            [0xd800u16, 0x0061].into_iter().flat_map(|unit|
                if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect(),
        ];
        for tail in malformed {
            let mut bytes = prefix.clone();
            bytes.extend_from_slice(&tail);
            let mut reader = source(9677, "source.rs", &bytes);
            let before = reader.budget.accounting().reserved();
            let response = reader.read_window(2, 4, || false).unwrap();
            assert!(response.as_str().contains("\"has_replacements\":false"));
            assert!(response.as_str().contains("\"highlight\":{\"state\":\"unsupported_encoding\""));
            assert!(response.as_str().contains("\"runs\":[]"));
            drop(response);
            assert_eq!(reader.budget.accounting().reserved(), before, "failed preparation retained scratch");
            let copy = reader.copy_range(0, bytes.len() as u64, || false).unwrap();
            assert!(copy.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&bytes))));
        }
        let mut valid = source(9678, "source.rs", &encoded("// �\nfn valid() {}", little));
        highlighted(valid.read_window(0, 1024, || false).unwrap().as_str());
        let mut empty = source(9679, "empty.rs", &encoded("", little));
        let response = empty.read_window(0, 4, || false).unwrap();
        highlighted(response.as_str());
        assert!(response.as_str().contains("\"text\":\"\""));
        assert!(response.as_str().contains("\"runs\":[]"));
    }
}

#[test]
fn utf16_page_claims_must_match_endianness_original_units_and_scalar_boundaries() {
    for little in [true, false] {
        let bytes = encoded("/*🦀*/\nfn a() {}", little);
        let mut reader = source(9680, "source.rs", &bytes);
        reader.prepare_highlight(&mut || false).unwrap();
        let cache = ready(&reader);
        for (start, end, text, replacements) in [
            (0, 2, "", false), (3, 7, "/*", false),
            (6, 8, "�", false), (8, 10, "�", false),
            (2, 6, "//", false), (6, 10, "🦀", true),
            (6, 10, "", false), (8, 8, "", false), (bytes.len(), bytes.len() + 2, "", false),
        ] {
            assert!(cache.page(&bytes, raw_range(start, end), text, replacements, &mut || false)
                .unwrap().is_err(), "unverified page {start}..{end} was colored");
        }
        let opposite = encoded("/*🦀*/\nfn a() {}", !little);
        assert!(cache.page(&opposite, raw_range(6, 10), "🦀", false, &mut || false).unwrap().is_err());
        let scalar = cache.page(&bytes, raw_range(6, 10), "🦀", false, &mut || false).unwrap().unwrap();
        assert_eq!((scalar.start, scalar.end), (2, 4));
    }
}

#[test]
fn decoded_utf8_limit_preserves_readable_utf16_page_without_retaining_optional_cache() {
    let text = "界".repeat(MAX_SOURCE_HIGHLIGHT_BYTES / 3 + 1);
    let bytes = encoded(&text, true);
    assert!(bytes.len() < MAX_SOURCE_HIGHLIGHT_BYTES && text.len() > MAX_SOURCE_HIGHLIGHT_BYTES);
    let mut reader = source(9681, "large.rs", &bytes);
    let before = reader.budget.accounting().reserved();
    let response = reader.read_window(2, 4, || false).unwrap();
    assert!(response.as_str().contains("\"text\":\"界界\""));
    assert!(response.as_str().contains("\"highlight\":{\"state\":\"source_limit\""));
    assert!(response.as_str().contains("\"runs\":[]"));
    drop(response);
    assert_eq!(reader.budget.accounting().reserved(), before);
}

#[test]
fn utf16_cancellation_releases_decode_scratch_and_allows_retry_at_every_boundary() {
    for little in [true, false] {
        let bytes = encoded(&"// unicode é🦀\n".repeat(800), little);
        let mut reference = source(9682, "source.rs", &bytes);
        let mut polls = 0;
        reference.prepare_highlight(&mut || { polls += 1; false }).unwrap();
        assert!(polls > 8, "fixture must span decoder chunks and lexer admission");
        for stop in 1..=polls {
            let mut reader = source(9683, "source.rs", &bytes);
            let before = reader.budget.accounting().reserved();
            let mut calls = 0;
            let result = reader.prepare_highlight(&mut || { calls += 1; calls == stop });
            assert_eq!(result.err(), Some(ReaderSessionError::Canceled), "boundary {stop}");
            assert!(reader.highlight.is_none());
            assert_eq!(reader.budget.accounting().reserved(), before);
            highlighted(reader.read_window(0, 64, || false).unwrap().as_str());
        }
    }
}

#[test]
fn utf16_roles_are_shared_by_capture_forks_and_released_with_last_owner() {
    let bytes = encoded("/* 🦀 */\r\nfn first() {}\n", false);
    let mut parent = source(9684, "retained.rs", &bytes);
    parent.prepare_highlight(&mut || false).unwrap();
    let original_budget = parent.budget.clone();
    let pointer = Arc::as_ptr(ready(&parent));
    let mut child = parent.fork(owner(9685), || false).unwrap();
    assert_eq!(Arc::as_ptr(ready(&child)), pointer);
    drop(parent);
    highlighted(child.read_window(0, 64, || false).unwrap().as_str());
    let copied = child.copy_range(0, bytes.len() as u64, || false).unwrap();
    assert!(copied.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&bytes))));
    drop(copied);
    assert!(original_budget.accounting().reserved().get() > 0);
    drop(child);
    assert_eq!(original_budget.accounting().reserved().get(), 0);
}
