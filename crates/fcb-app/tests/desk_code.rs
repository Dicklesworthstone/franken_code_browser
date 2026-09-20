#![forbid(unsafe_code)]

mod support;
use support::{parse, Json};
use fcb::{ArenaOwnerId, ByteOffset, ByteRange, FileId, SourceCapture, SourceRevision};
use fcb::search::{ResourceAllocationId, SymbolError, ReferenceError, MAX_SYMBOL_SOURCE_BYTES};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, DeskCommand, DeskPaneId,
    code::{DeskOutline, DeskOutlineOptions, DeskReferences, DeskCodeError, SymbolNameMode, SymbolLanguage},
    imports::ImportedSourceId}};

fn owner(n: u64) -> ArenaOwnerId { ArenaOwnerId::new(n).unwrap() }
fn session() -> DeskSession { DeskSession::new(owner(9801), DeskLimits::default()).unwrap() }
fn adopt(d: &mut DeskSession, id: u64, revision: u64, label: &str, bytes: &[u8]) -> DeskPaneId {
    let source = SourceCapture::from_bytes(d.model().owner(), FileId::new(d.model().owner(), id).unwrap(),
        SourceRevision::new(d.model().owner(), revision).unwrap(), label, bytes.to_vec()).unwrap();
    d.adopt(d.model().revision(), d.model().last_attempt() + 1, source, 0, None, || false).unwrap().active.unwrap()
}
fn apply(d: &mut DeskSession, command: DeskCommand) {
    d.apply(d.model().revision(), d.model().last_attempt() + 1, command, || false).unwrap();
}
fn outline(d: &mut DeskSession, pane: DeskPaneId, generation: u64) -> DeskOutline {
    DeskOutline::prepare(d, d.model().revision(), pane, generation, Default::default(), || false).unwrap()
}
fn symbol(o: &DeskOutline, d: &DeskSession, name: &str) -> u64 {
    o.candidates(d, d.model().revision(), o.generation()).unwrap().iter().find(|s| s.name() == name).unwrap().id()
}
fn json(reply: &HostResponse) -> Json { parse(reply.as_str().as_bytes()).unwrap() }
fn selected(d: &DeskSession, p: DeskPaneId) -> &[u8] { d.model().selected_bytes(p, d.model().revision()).unwrap() }

#[test]
fn retained_outline_filter_and_selection_use_original_ids_and_source() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", b"fn alpha() {}\nfn alphabet() {}\nfn beta() {}\n");
    let address = d.model().source(p, 1).unwrap().bytes().as_ptr(); let o = outline(&mut d, p, 1);
    let alphabet = symbol(&o, &d, "alphabet");
    let page = o.page(&mut d, 1, 1, "alph", SymbolNameMode::Prefix, 1, 1, || false).unwrap();
    let row = json(&page); assert_eq!(page.exit_code(), EXIT_OK);
    assert_eq!(row.get("matched_symbols").number(), 2);
    assert_eq!(row.get("symbols").array()[0].get("symbol_id").number(), alphabet);
    assert!(!row.get("compiler_resolved").flag()); assert!(!row.get("semantic_complete").flag());
    o.select(&mut d, 1, 2, 1, alphabet, false, || false).unwrap();
    assert_eq!(selected(&d, p), b"alphabet");
    assert_eq!(d.model().source(p, 2).unwrap().bytes().as_ptr(), address);
    let page = o.page(&mut d, 2, 1, "ALPHA", SymbolNameMode::Exact, 0, 10, || false).unwrap();
    assert_eq!(json(&page).get("matched_symbols").number(), 0);
    assert_eq!(d.model().history().len(), 2);
}

#[test]
fn symbol_to_reference_navigation_bookmark_and_back_share_one_capture() {
    let mut d = session();
    let p = adopt(&mut d, 1, 1, "a.rs", b"fn target() {}\nfn use_it() { target(); }\n// target\n\"target\"\n");
    let o = outline(&mut d, p, 1); let id = symbol(&o, &d, "target");
    let refs = o.references(&mut d, 1, 1, id, 1, 10, || false).unwrap();
    assert_eq!(refs.candidates(&d, 1, 1).unwrap().len(), 4);
    let response = json(&refs.page(&mut d, 1, 1, 0, 10, || false).unwrap());
    assert_eq!(response.get("origin_symbol").get("symbol_id").number(), id);
    assert!(response.get("includes_comments_and_literals").flag());
    assert_eq!(response.get("evidence_level").text(), "whole-token-text-candidate");
    o.select(&mut d, 1, 2, 1, id, false, || false).unwrap();
    let definition_range = d.model().location(p, 2).unwrap().selection;
    refs.select(&mut d, 2, 3, 1, 2, || false).unwrap();
    assert_eq!(selected(&d, p), b"target");
    let use_range = d.model().location(p, 3).unwrap().selection;
    assert_ne!(use_range, definition_range);
    apply(&mut d, DeskCommand::Bookmark { pane: p, label: "use evidence".into() });
    apply(&mut d, DeskCommand::Back);
    assert_eq!(d.model().location(p, d.model().revision()).unwrap().selection, definition_range);
    let bookmark = d.model().bookmarks()[0].id(); apply(&mut d, DeskCommand::RecallBookmark(bookmark));
    assert_eq!(d.model().location(p, d.model().revision()).unwrap().selection, use_range);
    assert_eq!(d.model().retained_source_count(), 1);
}

#[test]
fn capped_outline_filter_never_reports_exhaustive_absence() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", b"fn alpha() {}\nfn beta() {}\n");
    let o = DeskOutline::prepare(&mut d, 1, p, 1, DeskOutlineOptions { max_items: 1, ..Default::default() }, || false).unwrap();
    let page = o.page(&mut d, 1, 1, "beta", SymbolNameMode::Exact, 0, 10, || false).unwrap();
    assert_eq!(page.exit_code(), EXIT_PARTIAL); let j = json(&page);
    assert!(j.get("output_limited").flag()); assert!(!j.get("semantic_complete").flag());
    assert_eq!(j.get("matched_symbols").number(), 0);
    assert_eq!(j.get("count_basis").text(), "retained-candidates");
}

#[test]
fn reference_caps_distinguish_exact_count_lookahead_and_zero_limit() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "text", b"foo foobar foo");
    for (gen, cap, complete, retained, counted) in [(1, 2, true, 2, 2), (2, 1, false, 1, 2), (3, 0, false, 0, 1)] {
        let r = DeskReferences::prepare(&mut d, 1, p, gen, "foo", cap, || false).unwrap();
        let out = r.page(&mut d, 1, gen, 0, 10, || false).unwrap(); let j = json(&out);
        assert_eq!(j.get("search_complete").flag(), complete); assert_eq!(j.get("count_complete").flag(), complete);
        assert_eq!(j.get("retained_references").number(), retained); assert_eq!(j.get("matches_counted").number(), counted);
        assert_eq!(out.exit_code(), if complete { EXIT_OK } else { EXIT_PARTIAL });
    }
    let none = DeskReferences::prepare(&mut d, 1, p, 4, "absent", 0, || false).unwrap();
    assert!(none.is_complete()); assert!(none.candidates(&d, 1, 4).unwrap().is_empty());
}

#[test]
fn both_utf16_endiannesses_keep_exact_name_and_occurrence_bytes() {
    for little in [true, false] {
        let encode = |text: &str| text.encode_utf16().flat_map(|u| if little { u.to_le_bytes() } else { u.to_be_bytes() }).collect::<Vec<_>>();
        let bytes = encode("\u{feff}// 🦀\r\nfn target() {}\r\ntarget();\r\n");
        let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", &bytes); let o = outline(&mut d, p, 1);
        let id = symbol(&o, &d, "target"); o.select(&mut d, 1, 2, 1, id, false, || false).unwrap();
        assert_eq!(selected(&d, p), encode("target"));
        let r = o.references(&mut d, 2, 1, id, 1, 10, || false).unwrap();
        assert_eq!(r.candidates(&d, 2, 1).unwrap().len(), 2);
        r.select(&mut d, 2, 3, 1, 2, || false).unwrap(); assert_eq!(selected(&d, p), encode("target"));
        assert_eq!(d.model().source(p, 3).unwrap().bytes(), bytes);
    }
}

#[test]
fn malformed_sources_are_readable_even_when_analysis_is_refused() {
    let mut d = session(); let bytes = b"fn target() {}\n\xff"; let p = adopt(&mut d, 1, 1, "a.rs", bytes);
    assert!(matches!(DeskOutline::prepare(&mut d, 1, p, 1, Default::default(), || false),
        Err(DeskCodeError::Symbol(SymbolError::UnsupportedEncoding))));
    assert!(matches!(DeskReferences::prepare(&mut d, 1, p, 1, "target", 10, || false),
        Err(DeskCodeError::Reference(ReferenceError::UnsupportedEncoding))));
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(bytes.len() as u64)).unwrap();
    apply(&mut d, DeskCommand::Navigate { pane: p, offset: 0, selection: Some(range) });
    assert_eq!(selected(&d, p), bytes); assert!(d.copy_selection(2, p, || false).is_ok());
}

#[test]
fn reference_lookup_does_not_depend_on_outline_size_or_language_support() {
    let mut bytes = b"target ".to_vec(); bytes.resize(MAX_SYMBOL_SOURCE_BYTES + 1, b' ');
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", &bytes);
    assert!(matches!(DeskOutline::prepare(&mut d, 1, p, 1, Default::default(), || false),
        Err(DeskCodeError::Symbol(SymbolError::SourceLimit))));
    let refs = DeskReferences::prepare(&mut d, 1, p, 1, "target", 10, || false).unwrap();
    assert_eq!(refs.candidates(&d, 1, 1).unwrap().len(), 1);
    refs.select(&mut d, 1, 2, 1, 1, || false).unwrap(); assert_eq!(selected(&d, p), b"target");
}

#[test]
fn all_observed_outline_cancel_boundaries_leave_previous_source_and_result_usable() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", b"fn alpha() {}\nfn beta() {}\n");
    let old = outline(&mut d, p, 1); let mut checkpoints = 0;
    let probe = DeskOutline::prepare(&mut d, 1, p, 2, Default::default(), || { checkpoints += 1; false }).unwrap();
    drop(probe);
    for stop in 1..=checkpoints {
        let mut n = 0;
        assert!(matches!(DeskOutline::prepare(&mut d, 1, p, 2 + stop as u64, Default::default(), || {
            n += 1; n == stop
        }), Err(e) if e.is_canceled()));
        assert_eq!(d.model().revision(), 1); assert_eq!(old.candidates(&d, 1, 1).unwrap().len(), 2);
    }
}

#[test]
fn stale_retargeted_sources_are_rejected_but_explicit_history_restores_exact_old_bytes() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", b"fn original() {}\n");
    let o = outline(&mut d, p, 1); let r = o.references(&mut d, 1, 1, 1, 1, 10, || false).unwrap();
    adopt(&mut d, 1, 2, "a.rs", b"fn changed() {}\n");
    assert_eq!(o.validate_source(&d, 2), Err(DeskCodeError::StaleSource));
    assert_eq!(r.validate_source(&d, 2), Err(DeskCodeError::StaleSource));
    assert!(o.select(&mut d, 2, 3, 1, 1, false, || false).is_err());
    apply(&mut d, DeskCommand::Back); let rev = d.model().revision();
    o.select(&mut d, rev, rev + 1, 1, 1, false, || false).unwrap(); assert_eq!(selected(&d, p), b"original");
}

#[test]
fn invalid_ids_generations_and_pages_do_not_change_the_desk() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", b"fn target() {}\n"); let o = outline(&mut d, p, 1);
    let r = o.references(&mut d, 1, 1, 1, 1, 10, || false).unwrap();
    for id in [0, 2, u64::MAX] {
        assert!(o.select(&mut d, 1, 2, 1, id, false, || false).is_err());
        assert!(r.select(&mut d, 1, 2, 1, id, || false).is_err());
    }
    assert!(o.select(&mut d, 1, 2, 2, 1, false, || false).is_err());
    assert!(r.select(&mut d, 0, 2, 1, 1, || false).is_err());
    for (start, count) in [(0, 0), (0, 129), (2, 1), (usize::MAX, 1)] {
        assert!(o.page(&mut d, 1, 1, "", SymbolNameMode::Exact, start, count, || false).is_err());
        assert!(r.page(&mut d, 1, 1, start, count, || false).is_err());
    }
    assert_eq!(d.model().revision(), 1); assert_eq!(d.model().history().len(), 1);
}

#[test]
fn explicit_language_override_and_nested_parent_ids_survive_filtering() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "README.md", b"mod inner {\n    fn nested() {}\n}\n");
    assert!(matches!(DeskOutline::prepare(&mut d, 1, p, 1, Default::default(), || false), Err(DeskCodeError::UnsupportedLanguage)));
    let o = DeskOutline::prepare(&mut d, 1, p, 2, DeskOutlineOptions { language: Some(SymbolLanguage::Rust), ..Default::default() }, || false).unwrap();
    let items = o.candidates(&d, 1, 2).unwrap(); let nested = items.iter().find(|s| s.name() == "nested").unwrap();
    let parent = nested.parent_id().unwrap(); assert_eq!(items.iter().find(|s| s.id() == parent).unwrap().name(), "inner");
    let page = json(&o.page(&mut d, 1, 2, "nested", SymbolNameMode::Exact, 0, 10, || false).unwrap());
    assert_eq!(page.get("symbols").array()[0].get("parent_id").number(), parent);
}

#[test]
fn duplicate_readers_have_independent_navigation_over_shared_source() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", b"fn alpha() {}\nfn beta() {}\n");
    let other = d.apply(1, 2, DeskCommand::Duplicate(p), || false).unwrap().active.unwrap();
    let a = outline(&mut d, p, 1); let b = outline(&mut d, other, 2);
    a.select(&mut d, 2, 3, 1, 1, false, || false).unwrap();
    b.select(&mut d, 3, 4, 2, 2, false, || false).unwrap();
    assert_eq!(selected(&d, p), b"alpha"); assert_eq!(selected(&d, other), b"beta");
    assert_eq!(d.model().retained_source_count(), 1);
    assert_eq!(d.model().source(p, 4).unwrap().bytes().as_ptr(), d.model().source(other, 4).unwrap().bytes().as_ptr());
}

#[test]
fn code_selections_persist_but_analysis_identities_do_not_survive_restore() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "a.rs", b"fn target() {}\ntarget();\n");
    let o = outline(&mut d, p, 1); let r = o.references(&mut d, 1, 1, 1, 1, 10, || false).unwrap();
    r.select(&mut d, 1, 2, 1, 2, || false).unwrap();
    apply(&mut d, DeskCommand::Bookmark { pane: p, label: "reference candidate".into() });
    let saved = d.model().checkpoint(3, ResourceAllocationId::new(9000001).unwrap(), || false).unwrap();
    let restored = d.restore_checkpoint_bytes(3, 4, saved.bytes(), || false).unwrap().active.unwrap();
    assert_ne!(p, restored); assert!(o.validate_source(&d, 4).is_err()); assert!(r.validate_source(&d, 4).is_err());
    assert_eq!(selected(&d, restored), b"target"); assert_eq!(d.model().bookmarks()[0].label(), "reference candidate");
    let next = outline(&mut d, restored, 2); assert_eq!(symbol(&next, &d, "target"), 1);
}

#[test]
fn imported_provider_source_and_outline_share_the_exact_receiving_capture() {
    let mut d = session(); let origin = ImportedSourceId { file: FileId::new(owner(9900), 1).unwrap(), revision: SourceRevision::new(owner(9900), 1).unwrap() };
    let bytes = b"fn retained() {}\n";
    let imported = d.import_source(0, 1, origin, "raw-\\xff.RS", bytes, 0, None, || false).unwrap();
    let p = imported.change.active.unwrap(); let o = outline(&mut d, p, 1);
    o.select(&mut d, 1, 2, 1, 1, false, || false).unwrap(); assert_eq!(selected(&d, p), b"retained");
    let mut foreign = DeskSession::new(owner(9802), DeskLimits::default()).unwrap();
    adopt(&mut foreign, 1, 1, "same.rs", bytes);
    assert!(o.validate_source(&foreign, 1).is_err());
    assert_eq!(d.model().source(p, 2).unwrap().bytes(), bytes);
}

#[test]
fn canceled_reference_preparation_and_selection_leave_prior_state_unchanged() {
    let mut d = session(); let p = adopt(&mut d, 1, 1, "text", b"target target");
    let old = DeskReferences::prepare(&mut d, 1, p, 1, "target", 10, || false).unwrap();
    assert!(matches!(DeskReferences::prepare(&mut d, 1, p, 2, "target", 10, || true), Err(e) if e.is_canceled()));
    assert!(matches!(old.select(&mut d, 1, 2, 1, 1, || true), Err(e) if e.is_canceled()));
    assert_eq!(d.model().revision(), 1); assert_eq!(d.model().history().len(), 1);
    assert_eq!(old.candidates(&d, 1, 1).unwrap().len(), 2);
}
