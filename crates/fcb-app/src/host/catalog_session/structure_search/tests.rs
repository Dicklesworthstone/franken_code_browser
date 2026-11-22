#![forbid(unsafe_code)]

use super::*;
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ByteOffset;
use super::super::super::atlas_search::AtlasSearchPathKind;

fn fixture(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-catalog-structure-{label}-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    root // Retain fixtures for the independent qualification runner.
}
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn symbols() -> CatalogStructureSearchOptions {
    CatalogStructureSearchOptions::new(CatalogStructureSearchMode::Symbols)
}
fn drain(session: &mut CatalogStructureSearchSession) {
    for _ in 0..20_000 {
        if !session.more() { return; }
        let before_bytes = session.io.bytes;
        let before_calls = session.io.calls;
        session.advance_work(session.generation.get(), session.revision, MAX_CATALOG_STRUCTURE_STEPS, &mut || false).unwrap();
        assert!(session.io.bytes - before_bytes <= (MAX_CATALOG_STRUCTURE_STEPS * session.options.mode.source_limit()) as u64);
        assert!(session.io.calls >= before_calls && session.io.calls <= session.options.max_read_calls);
        assert!(session.io.bytes <= session.options.max_scan_bytes);
        assert!(session.catalog.pages.len() <= super::super::MAX_CATALOG_RETAINED_PAGES);
        assert!(session.catalog.pages.iter().map(|page| page.files.len()).sum::<usize>()
            <= super::super::MAX_CATALOG_RETAINED_FILES);
        assert!(session.hits.len() <= session.options.max_matches);
        assert!(session.retained_source_bytes <= MAX_CATALOG_STRUCTURE_RETAINED_SOURCE_BYTES);
    }
    panic!("bounded structural query did not terminate");
}
fn selected_bytes(hit: &Hit) -> &[u8] {
    &hit.source.capture.bytes()[hit.row.selected.start().get() as usize..hit.row.selected.end().get() as usize]
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn late_symbols_survive_twenty_thousand_files_page_eviction_and_live_replacement() {
    let root = fixture("beyond-atlas");
    for index in 0..20_600 { fs::write(root.join(format!("file-{index:05}.rs")), b"").unwrap(); }
    let mut options = symbols(); options.max_matches = 4;
    let mut session = CatalogStructureSearchSession::open(owner(78501), &root, 7, "target", options, || false).unwrap();
    let early_path = session.catalog.pages.back().unwrap().files[0].path.raw().to_path_buf();
    let source = b"fn target() {}\n";
    fs::write(root.join(&early_path), source).unwrap();
    // Select a late entry from actual discovery, not from an invented native
    // directory ordering. The last entry of its batch has not been examined.
    while session.catalog.catalogued_files <= 20_000 {
        assert!(session.more()); session.step(&mut || false).unwrap();
    }
    let late = session.catalog.pages.back().unwrap().files.last().unwrap();
    assert!(late.id.get() > 20_000);
    let late_id = late.id;
    let late_path = late.path.raw().to_path_buf();
    fs::write(root.join(&late_path), source).unwrap();
    drain(&mut session);
    assert!(session.complete());
    assert_eq!(session.examined, 20_600);
    assert_eq!(session.matches_seen, 2);
    assert_eq!(session.hits.len(), 2);
    assert_eq!(session.hits[1].source.capture.request().file(), late_id);
    assert!(session.catalog.pages.front().unwrap().id > 1);
    assert!(session.hits.iter().all(|hit| selected_bytes(hit) == b"target"));
    let report = session.info(|| false).unwrap();
    assert!(report.as_str().contains("\"search_complete\":true"));
    assert!(report.as_str().contains("\"semantic_complete\":false"));
    assert!(!report.as_str().contains("node_ordinal"));
    assert!(!report.as_str().contains("source_manifest"));
    let io = (session.io.bytes, session.io.calls);
    fs::remove_file(root.join(early_path)).unwrap();
    fs::write(root.join(late_path), b"fn replacement() {}\n").unwrap();
    for index in 0..2 {
        let witness = session.hit(7, session.revision, index, || false).unwrap();
        assert!(witness.as_str().contains("\"original_hex\":\"746172676574\""));
        let (reader, receipt) = session.open_hit_reader(owner(78601 + index as u64),
            7, session.revision, index, || false).unwrap();
        assert_eq!(reader.capture().bytes(), source);
        assert_eq!(reader.capture_sha256(), session.hits[index].source.digest);
        assert!(receipt.as_str().contains("\"activation_source\":\"retained-search-capture\""));
        assert!(receipt.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    }
    assert_eq!((session.io.bytes, session.io.calls), io);
}

#[test]
fn directory_file_and_visible_scopes_precede_source_budgets() {
    let root = fixture("scope-before-budgets");
    fs::create_dir(root.join("src")).unwrap();
    let source = b"fn target() {}\n";
    fs::write(root.join("src/target.rs"), source).unwrap();
    fs::write(root.join("src/unsupported.txt"), b"target").unwrap();
    fs::write(root.join("outside.rs"), vec![b'x'; MAX_SYMBOL_SOURCE_BYTES + 1]).unwrap();
    for (offset, scope) in [
        AtlasSearchScope::from_csv(Some("rs"), None).unwrap().with_path(AtlasSearchPathKind::Directory, "src").unwrap(),
        AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, "src/target.rs").unwrap(),
        AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible, "7372632f7461726765742e7273").unwrap(),
    ].into_iter().enumerate() {
        let mut options = symbols(); options.scope = scope; options.max_scan_bytes = source.len() as u64;
        let mut session = CatalogStructureSearchSession::open(owner(78510 + offset as u64),
            &root, 1, "target", options, || false).unwrap();
        assert_eq!(session.io.bytes, 0, "open only discovers metadata");
        drain(&mut session);
        assert!(session.complete());
        assert_eq!(session.eligible, 1);
        assert_eq!(session.excluded, 2);
        assert_eq!(session.io.bytes, source.len() as u64);
        assert_eq!(session.matches_seen, 1);
        assert_eq!(session.unsupported, 0);
        assert_eq!(session.oversized, 0);
    }
    let mut missing = symbols();
    missing.scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::Visible,
        "6d697373696e672e7273,7372632f7461726765742e7273").unwrap();
    let mut session = CatalogStructureSearchSession::open(owner(78513), &root, 1, "target", missing, || false).unwrap();
    drain(&mut session);
    assert_eq!(session.matches_seen, 1);
    assert!(!session.complete(), "a missing frozen region member cannot become a successful miss");
    assert!(session.info(|| false).unwrap().as_str().contains("\"scope_region_missing_files\":\"1\""));
}

fn heading_agrees_with_public_oracle(session: &CatalogStructureSearchSession, hit: &Hit) -> bool {
    let owner = session.owner();
    let document = DocumentReader::prepare(&hit.source.capture,
        DocumentId::new(owner, hit.source.capture.request().file().get()).unwrap(),
        DocumentGeneration::new(owner, session.generation.get()).unwrap(), DocumentReadOptions::default(),
        &session.catalog.budget, fcb::search::ResourceAllocationId::new(900_001).unwrap(), || false).unwrap();
    document.qualify_heading_source_ranges(&session.catalog.budget,
        fcb::search::ResourceAllocationId::new(900_002).unwrap(), || false).unwrap();
    let heading = &document.headings()[hit.row.candidate_id as usize - 1];
    hit.row.selected == document.original_span(heading.source_span).unwrap()
        && hit.row.name == heading.title && hit.row.heading_slug.as_deref() == Some(heading.slug.as_str())
}

#[test]
fn utf16_headings_keep_punctuation_duplicate_ids_and_original_source_ranges() {
    let root = fixture("heading-provenance");
    let text = "ordinary text\n\n# Target, Café!\n\n## Target, Café!\n";
    for (name, little) in [("little.md", true), ("big.MARKDOWN", false)] {
        fs::write(root.join(name), utf16(text, little)).unwrap();
    }
    let options = CatalogStructureSearchOptions::new(CatalogStructureSearchMode::Headings);
    let mut session = CatalogStructureSearchSession::open(owner(78520), &root, 3, "Target", options, || false).unwrap();
    drain(&mut session);
    assert!(session.complete());
    assert_eq!(session.matches_seen, 4);
    assert_eq!(session.hits.len(), 4);
    for hit in &session.hits {
        assert!(heading_agrees_with_public_oracle(&session, hit));
        assert_eq!(hit.row.name, "Target, Café!");
        assert!(hit.row.selected.start().get() >= 2);
        assert!(selected_bytes(hit).windows(2).any(|bytes| bytes == [b'#', 0] || bytes == [0, b'#']));
    }
    assert_ne!(session.hits[0].row.heading_slug, session.hits[1].row.heading_slug);
    // Negative control: an unrelated but valid in-bounds range must fail the
    // independent public heading catalog, even though the title stays correct.
    let correct = session.hits[0].row.selected;
    session.hits[0].row.selected = ByteRange::new(ByteOffset::new(2), ByteOffset::new(6)).unwrap();
    assert!(!heading_agrees_with_public_oracle(&session, &session.hits[0]));
    session.hits[0].row.selected = correct;
}

#[test]
fn unsupported_oversized_unavailable_and_analysis_limited_sources_stay_incomplete() {
    let root = fixture("coverage");
    fs::write(root.join("good.rs"), b"fn target() {}\n").unwrap();
    fs::write(root.join("unsupported.bin"), b"target").unwrap();
    fs::write(root.join("invalid.rs"), b"\xff fn target() {}").unwrap();
    fs::write(root.join("large.rs"), vec![b'x'; MAX_SYMBOL_SOURCE_BYTES + 1]).unwrap();
    fs::write(root.join("complex.rs"), "\n".repeat(4100)).unwrap();
    fs::write(root.join("missing.rs"), b"fn target() {}\n").unwrap();
    let mut session = CatalogStructureSearchSession::open(owner(78530), &root, 1, "target", symbols(), || false).unwrap();
    fs::remove_file(root.join("missing.rs")).unwrap();
    drain(&mut session);
    assert_eq!(session.examined, 6);
    assert_eq!(session.matches_seen, 1);
    assert_eq!(session.unsupported, 2);
    assert_eq!(session.oversized, 1);
    assert_eq!(session.unavailable, 1);
    assert_eq!(session.analysis_limited, 1);
    assert_eq!(session.incomplete, 5);
    assert!(!session.complete());
    let info = session.info(|| false).unwrap();
    assert!(info.as_str().contains("\"match_count_complete\":false"));
    assert!(info.as_str().contains("\"STRUCTURE_FILE_BYTE_LIMIT\""));
    assert!(info.as_str().contains("\"SYMBOL_COMPLEXITY_LIMIT\""));
}

#[test]
fn counted_name_windows_keep_matching_semantics_and_bounded_storage_separate() {
    let root = fixture("name-modes");
    fs::write(root.join("names.rs"), b"fn target() {}\nfn target_more() {}\nfn other_target() {}\n").unwrap();
    for (index, mode, expected) in [(0, SymbolNameMode::Exact, 1), (1, SymbolNameMode::Prefix, 2),
        (2, SymbolNameMode::Contains, 3)] {
        let mut options = symbols(); options.name_mode = mode; options.max_matches = 1;
        if mode == SymbolNameMode::Contains { options.start_after = 1; }
        let mut session = CatalogStructureSearchSession::open(owner(78540 + index),
            &root, 1, "target", options, || false).unwrap();
        drain(&mut session);
        assert!(session.complete());
        assert_eq!(session.matches_seen, expected);
        assert_eq!(session.hits.len(), 1);
        assert_eq!(session.hits[0].row.occurrence, if mode == SymbolNameMode::Contains { 2 } else { 1 });
        let info = session.info(|| false).unwrap();
        assert!(info.as_str().contains("\"match_count_complete\":true"));
        if expected > 1 { assert!(info.as_str().contains("\"truncated\":true")); }
    }
}

#[test]
fn stopped_and_canceled_queries_keep_only_previously_accepted_capture_receipts() {
    let root = fixture("cancel");
    for index in 0..3 { fs::write(root.join(format!("source-{index}.rs")), b"fn target() {}\n").unwrap(); }
    let mut session = CatalogStructureSearchSession::open(owner(78550), &root, 1, "target", symbols(), || false).unwrap();
    let initial = session.revision;
    assert!(matches!(session.advance(1, initial, 1, || true), Err(CatalogStructureSearchError::Canceled)));
    assert_eq!(session.revision, initial);
    let first = session.advance(1, initial, 1, || false).unwrap();
    let replay = session.advance(1, initial, MAX_CATALOG_STRUCTURE_STEPS, || false).unwrap();
    assert_eq!(first.as_str(), replay.as_str());
    while session.hits.is_empty() { session.advance_work(1, session.revision, 1, &mut || false).unwrap(); }
    let retained = session.hits[0].source.digest;
    let before = session.hits.len();
    let mut polls = 0;
    let canceled = session.advance(1, session.revision, MAX_CATALOG_STRUCTURE_STEPS, || {
        polls += 1; polls >= 6
    });
    assert!(matches!(canceled, Err(CatalogStructureSearchError::Canceled)));
    assert!(session.canceled);
    assert_eq!(session.hits.len(), before);
    assert_eq!(session.hits[0].source.digest, retained);
    assert!(!session.complete());
    assert!(session.info(|| false).unwrap().as_str().contains("\"state\":\"canceled\""));
    let (reader, _) = session.open_hit_reader(owner(78650), 1, session.revision, 0, || false).unwrap();
    assert_eq!(reader.capture_sha256(), retained);

    let mut stopped = CatalogStructureSearchSession::open(owner(78551), &root, 2, "target", symbols(), || false).unwrap();
    while stopped.hits.is_empty() { stopped.advance_work(2, stopped.revision, 1, &mut || false).unwrap(); }
    let info = stopped.stop(2, || false).unwrap();
    assert!(!stopped.more());
    assert_eq!(stopped.hits.len(), 1);
    assert!(info.as_str().contains("\"stop_reason\":\"user-stopped\""));
    assert!(info.as_str().contains("\"search_complete\":false"));
    assert!(matches!(stopped.page(3, stopped.revision, 0, 1, || false), Err(CatalogStructureSearchError::StaleQuery)));
    assert!(matches!(stopped.open_hit_reader(owner(78651), 2, 1, 0, || false), Err(CatalogStructureSearchError::StaleRevision)));
}

#[test]
fn byte_and_call_exhaustion_do_not_report_empty_complete_inventories() {
    let root = fixture("io-limits");
    for index in 0..2 { fs::write(root.join(format!("source-{index}.rs")), b"fn target() {}\n").unwrap(); }
    let mut bytes = symbols(); bytes.max_scan_bytes = 1;
    let mut limited = CatalogStructureSearchSession::open(owner(78560), &root, 1, "target", bytes, || false).unwrap();
    drain(&mut limited);
    assert_eq!(limited.io.bytes, 0);
    assert_eq!(limited.matches_seen, 0);
    assert!(!limited.complete());
    assert!(limited.info(|| false).unwrap().as_str().contains("\"stop_reason\":\"source-byte-limit\""));
    let mut calls = symbols(); calls.max_read_calls = 1;
    let mut limited = CatalogStructureSearchSession::open(owner(78561), &root, 1, "target", calls, || false).unwrap();
    drain(&mut limited);
    assert!(limited.io.calls <= 1);
    assert!(!limited.complete());
    assert!(limited.info(|| false).unwrap().as_str().contains("\"stop_reason\":\"read-call-limit\""));
}

#[test]
fn explicit_discovery_policy_is_immutable_and_scope_filtered_before_capture() {
    let root = fixture("discovery-policy");
    fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
    fs::write(root.join("ignored.rs"), b"fn target() {}\n").unwrap();
    for (offset, policy, expected) in [(0, CatalogDiscoveryPolicy::RepositoryRules, 0),
        (1, CatalogDiscoveryPolicy::IncludeExcluded, 1)] {
        let mut options = symbols(); options.scope = AtlasSearchScope::from_csv(Some("rs"), None).unwrap();
        let mut session = CatalogStructureSearchSession::open_with_policy(owner(78570 + offset),
            &root, 1, "target", options, policy, || false).unwrap();
        drain(&mut session);
        assert!(session.complete());
        assert_eq!(session.matches_seen, expected);
        let info = session.info(|| false).unwrap();
        assert!(info.as_str().contains(policy.name()));
        assert_eq!(session.catalog.discovery_policy(), policy);
    }
}

#[test]
fn a_single_post_read_cancel_poll_keeps_its_error_and_actual_io_debits() {
    let root = fixture("one-shot-cancel");
    let source = b"fn target() {}\n";
    fs::write(root.join("source.rs"), source).unwrap();
    let mut session = CatalogStructureSearchSession::open(owner(78580), &root, 1, "target", symbols(), || false).unwrap();
    let entry = &session.catalog.pages.back().unwrap().files[0];
    let file = entry.id;
    let path = entry.path.clone();
    let request = CaptureRequest::new(file, SourceRevision::new(session.owner(), 1).unwrap()).unwrap();
    let id = session.catalog.next_id().unwrap();
    let mut io = workspace::IoCounts::default();
    let mut polls = 0;
    let result = workspace::read_capture_with_call_limit(&root, request, &path, MAX_SYMBOL_SOURCE_BYTES,
        1024, 16, &mut io, &session.catalog.budget, id, &mut || {
            polls += 1;
            // Helper entry, loop entry, shared step entry, pre-read, post-read.
            // Subsequent polls are false; the actual error carries cancellation.
            polls == 5
        });
    assert!(matches!(result, Err(SourceError::Canceled)));
    assert_eq!(io.bytes, source.len() as u64);
    assert_eq!(io.calls, 1);
}
