#![forbid(unsafe_code)]
#![cfg(unix)]

//! Actual extraction, retained source, and ordinary reader-selection workflows.
//! No native rendering or compiler-semantic qualification is asserted here.
use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, ByteRange};
use fcb::search::{SymbolLanguage, SymbolNameMode, SymbolError, MAX_SYMBOL_SOURCE_BYTES};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::reader::{ReaderSession, ReaderSessionError, ReaderOutlineOptions,
    MAX_READER_SYMBOL_QUERY_BYTES, MAX_READER_SYMBOL_PAGE};

fn reader(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9301).unwrap(), Path::new("source.rs"), bytes, || false).unwrap()
}
fn prepare(reader: &mut ReaderSession, generation: u64) {
    reader.prepare_outline(generation, ReaderOutlineOptions::default(), || false).unwrap();
}
fn id(reader: &mut ReaderSession, generation: u64, name: &str) -> u64 {
    let response = reader.symbol_page(generation, name, SymbolNameMode::Exact, 0, 128, || false).unwrap();
    assert!(response.as_str().contains("\"matched_symbols\":\"1\""), "{}", response.as_str());
    response.as_str().split("\"symbol_id\":\"").nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
}
fn bytes(reader: &ReaderSession, range: ByteRange) -> &[u8] {
    &reader.capture().bytes()[range.start().get() as usize..range.end().get() as usize]
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn fixture() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-outline-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}

#[test]
fn outline_filter_jump_and_copy_use_one_retained_capture() {
    let source = b"fn first() {}\nfn second() {}\n";
    let mut r = reader(source); let address = r.capture().bytes().as_ptr();
    let out = r.prepare_outline(1, Default::default(), || false).unwrap();
    assert_eq!(out.exit_code(), EXIT_OK);
    assert!(out.as_str().contains("\"retained_symbols\":\"2\""));
    assert!(out.as_str().contains("\"semantic_complete\":false"));
    let second = id(&mut r, 1, "second");
    let selected = r.symbol_range(1, second, false).unwrap();
    assert_eq!(bytes(&r, selected), b"second");
    let window = r.symbol_window(1, second, 128, || false).unwrap();
    assert!(window.as_str().contains("\"selection_namespace\":\"outline\""));
    assert!(window.as_str().contains("\"window_utf8_range\":{\"start\":\"17\",\"end\":\"23\"}"));
    assert!(window.as_str().contains("\"declaration_line\":\"2\""));
    assert!(!window.as_str().contains("\"query_generation\":"));
    let copied = r.copy_symbol(1, second, false, || false).unwrap();
    assert!(copied.as_str().contains("\"original_hex\":\"7365636f6e64\""));
    let evidence = r.symbol_range(1, second, true).unwrap();
    assert!(evidence.start() <= selected.start() && evidence.end() >= selected.end());
    assert!(r.copy_symbol(1, second, true, || false).unwrap().as_str().contains(&format!("\"original_hex\":\"{}\"", hex(bytes(&r, evidence)))));
    assert_eq!(r.capture().bytes().as_ptr(), address);
    assert!(copied.as_str().contains("\"additional_source_bytes_read\":\"0\""));
}

#[test]
fn structural_navigation_survives_live_edit_rename_and_disappearance() {
    let root = fixture(); let path = root.join("source.rs");
    fs::write(&path, b"fn original() {}\n").unwrap();
    let mut r = ReaderSession::open(ArenaOwnerId::new(9302).unwrap(), &path, 1024, || false).unwrap();
    fs::write(&path, b"fn replacement() {}\n").unwrap();
    fs::rename(&path, root.join("moved.rs")).unwrap();
    prepare(&mut r, 1);
    let original = id(&mut r, 1, "original");
    assert_eq!(bytes(&r, r.symbol_range(1, original, false).unwrap()), b"original");
    assert!(r.symbol_window(1, original, 100, || false).unwrap().as_str().contains("fn original() {}"));
    let absent = r.symbol_page(1, "replacement", SymbolNameMode::Exact, 0, 10, || false).unwrap();
    assert!(absent.as_str().contains("\"matched_symbols\":\"0\""));
}

#[test]
fn utf16_both_endiannesses_keep_name_selection_in_original_and_window_domains() {
    for little in [true, false] {
        let encode = |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect() };
        let source = encode("\u{feff}// 🦀\r\nfn target() {}\r\n");
        let expected = encode("target"); let mut r = reader(&source); prepare(&mut r, 1);
        let target = id(&mut r, 1, "target");
        let selected = r.symbol_range(1, target, false).unwrap();
        assert_eq!(bytes(&r, selected), expected);
        assert_eq!(selected.len().get(), 12);
        let window = r.symbol_window(1, target, 0, || false).unwrap();
        assert!(window.as_str().contains("window_utf8_range"));
        assert!(window.as_str().contains(&format!("\"original_hex\":\"{}\"", hex(&expected))));
        assert!(window.as_str().contains("\"outline_generation\":\"1\""));
    }
}

#[test]
fn filters_preserve_outline_ids_and_are_explicitly_case_sensitive() {
    let mut r = reader(b"fn alpha() {}\nfn alphabet() {}\nfn beta() {}\n"); prepare(&mut r, 1);
    let alphabet = id(&mut r, 1, "alphabet");
    let prefix = r.symbol_page(1, "alph", SymbolNameMode::Prefix, 1, 1, || false).unwrap();
    assert!(prefix.as_str().contains(&format!("\"symbol_id\":\"{alphabet}\"")));
    assert!(prefix.as_str().contains("\"matched_symbols\":\"2\""));
    let contains = r.symbol_page(1, "pha", SymbolNameMode::Contains, 0, 10, || false).unwrap();
    assert!(contains.as_str().contains("\"matched_symbols\":\"2\""));
    let upper = r.symbol_page(1, "ALPHA", SymbolNameMode::Exact, 0, 10, || false).unwrap();
    assert!(upper.as_str().contains("\"matched_symbols\":\"0\""));
    assert_eq!(id(&mut r, 1, "alphabet"), alphabet);
}

#[test]
fn paging_does_not_reextract_or_renumber_candidates() {
    let source: String = (0..80).map(|n| format!("fn item_{n}() {{}}\n")).collect();
    let mut r = reader(source.as_bytes());
    let first = r.prepare_outline(1, Default::default(), || false).unwrap();
    assert!(first.as_str().contains("\"next_offset\":\"64\""));
    let page = r.symbol_page(1, "", SymbolNameMode::Exact, 64, 128, || false).unwrap();
    assert_eq!(page.as_str().matches("\"symbol_id\":").count(), 16);
    assert!(page.as_str().contains("\"symbol_id\":\"65\""));
    assert!(page.as_str().contains("\"next_offset\":null"));
    assert!(r.symbol_page(1, "", SymbolNameMode::Exact, 80, 1, || false).unwrap().as_str().contains("\"symbols\":[]"));
    assert!(r.symbol_page(1, "", SymbolNameMode::Exact, 81, 1, || false).is_err());
}

#[test]
fn limited_inventory_does_not_claim_an_exhaustive_negative() {
    let mut r = reader(b"fn alpha() {}\nfn beta() {}\n");
    let out = r.prepare_outline(1, ReaderOutlineOptions { max_items: 1, ..Default::default() }, || false).unwrap();
    assert_eq!(out.exit_code(), EXIT_PARTIAL);
    assert!(out.as_str().contains("\"output_limited\":true"));
    let result = r.symbol_page(1, "beta", SymbolNameMode::Exact, 0, 10, || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_PARTIAL);
    assert!(result.as_str().contains("\"count_basis\":\"retained-candidates\""));
    assert!(result.as_str().contains("\"semantic_complete\":false"));
}

#[test]
fn failed_replacement_and_canceled_clear_keep_old_outline() {
    let mut r = reader(b"fn retained() {}\n"); prepare(&mut r, 1);
    let symbol = id(&mut r, 1, "retained");
    assert!(r.prepare_outline(2, ReaderOutlineOptions { max_items: 0, ..Default::default() }, || false).is_err());
    assert_eq!(r.outline_generation(), Some(1));
    assert!(r.prepare_outline(2, Default::default(), || false).is_err());
    assert!(r.clear_outline(3, || true).is_err());
    assert!(r.copy_symbol(1, symbol, false, || false).is_ok());
    prepare(&mut r, 4);
    assert_eq!(r.symbol_range(1, symbol, false).err(), Some(ReaderSessionError::Symbol(SymbolError::StaleQuery)));
}

#[test]
fn every_observed_cancellation_boundary_preserves_published_outline() {
    let source = b"fn one() {}\nfn two() {}\n";
    let mut probe = reader(source); prepare(&mut probe, 1);
    let mut checks = 0;
    probe.prepare_outline(2, Default::default(), || { checks += 1; false }).unwrap();
    assert!(checks > 0);
    for cancel_at in 1..=checks {
        let mut r = reader(source); prepare(&mut r, 1); let mut visited = 0;
        let result = r.prepare_outline(2, Default::default(), || { visited += 1; visited == cancel_at });
        assert!(result.is_err(), "cancellation checkpoint {cancel_at}");
        assert_eq!(r.outline_generation(), Some(1));
        assert!(r.copy_symbol(1, 1, false, || false).is_ok());
    }
}

#[test]
fn search_and_outline_generations_and_selections_are_independent() {
    let mut r = reader(b"fn target() {}\n");
    r.search(1, "target", 10, 1024, || false).unwrap(); prepare(&mut r, 1);
    let symbol = id(&mut r, 1, "target");
    assert_eq!(r.hit_range(1, 0).unwrap(), r.symbol_range(1, symbol, false).unwrap());
    let symbol_window = r.symbol_window(1, symbol, 0, || false).unwrap();
    let hit_window = r.hit_window(1, 0, 0, || false).unwrap();
    assert!(!symbol_window.as_str().contains("\"query_generation\":"));
    assert!(!hit_window.as_str().contains("\"outline_generation\":"));
    r.clear_outline(2, || false).unwrap();
    assert_eq!(r.outline_generation(), None); assert_eq!(r.accepted_generation(), Some(1));
    assert!(r.copy_hit(1, 0, || false).is_ok());
    assert_eq!(r.symbol_range(1, symbol, false).err(), Some(ReaderSessionError::MissingOutline));
    prepare(&mut r, 3);
    r.search(2, "fn", 10, 1024, || false).unwrap();
    assert!(r.copy_symbol(3, symbol, false, || false).is_ok());
}

#[test]
fn extractor_refusals_leave_large_and_malformed_sources_readable() {
    let mut source = b"fn real() {}\n".to_vec(); source.resize(MAX_SYMBOL_SOURCE_BYTES + 1, b' ');
    let mut r = reader(&source);
    assert_eq!(r.prepare_outline(1, Default::default(), || false).err(), Some(ReaderSessionError::Symbol(SymbolError::SourceLimit)));
    assert!(r.read_window(0, 128, || false).is_ok());
    assert!(r.search(1, "real", 10, source.len() as u64, || false).is_ok());
    let mut bad = reader(b"fn bad() {}\n\xff");
    assert!(bad.prepare_outline(1, Default::default(), || false).is_err());
    assert!(bad.copy_range(0, bad.capture().bytes().len() as u64, || false).is_ok());
}

#[test]
fn unsupported_language_never_becomes_an_empty_successful_outline() {
    let mut r = ReaderSession::from_bytes(ArenaOwnerId::new(9303).unwrap(), Path::new("README.md"), b"fn named() {}\n", || false).unwrap();
    assert_eq!(r.prepare_outline(1, Default::default(), || false).err(), Some(ReaderSessionError::UnsupportedOutlineLanguage));
    r.prepare_outline(2, ReaderOutlineOptions { language: Some(SymbolLanguage::Rust), ..Default::default() }, || false).unwrap();
    assert!(r.copy_symbol(2, 1, false, || false).is_ok());
}

#[test]
fn native_non_utf8_label_can_infer_an_ascii_language_suffix() {
    use std::os::unix::ffi::OsStringExt;
    let path = PathBuf::from(std::ffi::OsString::from_vec(b"raw-\xff\n.RS".to_vec()));
    let mut r = ReaderSession::from_bytes(ArenaOwnerId::new(9304).unwrap(), &path, b"fn raw() {}\n", || false).unwrap();
    let out = r.prepare_outline(1, Default::default(), || false).unwrap();
    assert!(out.as_str().contains("7261772dff0a2e5253"));
    assert!(out.as_str().contains("\"language\":\"rust\""));
    assert_eq!(bytes(&r, r.symbol_range(1, 1, false).unwrap()), b"raw");
}

#[test]
fn invalid_ids_pages_and_exhausted_generations_never_alias() {
    let mut r = reader(b"fn last() {}\n"); prepare(&mut r, u64::MAX);
    for invalid in [0, 2, u64::MAX] { assert!(r.symbol_range(u64::MAX, invalid, false).is_err()); }
    assert!(r.prepare_outline(0, Default::default(), || false).is_err());
    assert!(r.clear_outline(u64::MAX, || false).is_err());
    assert!(r.symbol_page(u64::MAX, "", SymbolNameMode::Exact, 0, MAX_READER_SYMBOL_PAGE + 1, || false).is_err());
    assert!(r.symbol_page(u64::MAX, &"x".repeat(MAX_READER_SYMBOL_QUERY_BYTES + 1), SymbolNameMode::Exact, 0, 10, || false).is_err());
    assert!(r.copy_symbol(u64::MAX, 1, false, || false).is_ok());
}

#[test]
fn response_ownership_and_absent_candidates_do_not_claim_semantic_completeness() {
    let mut r = reader(b"fn kept() {}\n");
    let outline = r.prepare_outline(1, Default::default(), || false).unwrap();
    let copy = r.copy_symbol(1, 1, false, || false).unwrap();
    r.clear_outline(2, || false).unwrap(); drop(r);
    assert!(outline.as_str().contains("kept")); assert!(copy.as_str().contains("6b657074"));
    let mut empty = reader(b"");
    let out = empty.prepare_outline(1, Default::default(), || false).unwrap();
    assert!(out.as_str().contains("\"retained_symbols\":\"0\""));
    assert!(out.as_str().contains("\"semantic_complete\":false"));
}
