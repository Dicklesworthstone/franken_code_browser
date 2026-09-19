#![forbid(unsafe_code)]

mod support;
use support::{parse, Json};
use fcb::{ArenaOwnerId, FileId, SourceRevision, SourceCapture};
use fcb::analysis::comparison::{ComparisonError, ComparisonQuality, ComparisonRelation, CorrespondenceKind};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, DeskCommand, DeskPaneId,
    DeskError, DeskSessionError, comparison::{DeskComparison, DeskComparisonLimits, DeskComparisonError, ComparisonSide}}};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(5600).unwrap() }
fn source(id: u64, bytes: &[u8]) -> SourceCapture {
    SourceCapture::from_bytes(owner(), FileId::new(owner(), id).unwrap(), SourceRevision::new(owner(), id).unwrap(),
        format!("capture-{id}.rs"), bytes.to_vec()).unwrap()
}
fn setup(a: &[u8], b: &[u8]) -> (DeskSession, DeskPaneId, DeskPaneId) {
    let mut desk = DeskSession::new(owner(), DeskLimits::default()).unwrap();
    let before = desk.adopt(0, 1, source(1, a), 0, None, || false).unwrap().active.unwrap();
    desk.apply(1, 2, DeskCommand::Pin { pane: before, pinned: true }, || false).unwrap();
    let after = desk.adopt(2, 3, source(2, b), 0, None, || false).unwrap().active.unwrap();
    (desk, before, after)
}
fn prepare(d: &mut DeskSession, a: DeskPaneId, b: DeskPaneId) -> DeskComparison {
    let expected = d.model().revision();
    DeskComparison::prepare(d, expected, a, b, 1, DeskComparisonLimits::default(), || false).unwrap()
}
fn json(response: &HostResponse) -> Json {
    let value = parse(response.as_str().as_bytes()).unwrap();
    assert_eq!(value.get("schema").text(), "fcb.reading-desk/1");
    assert!(!value.get("native_presented").flag()); value
}

#[test]
fn exact_spans_partition_both_captures_and_equal_bytes_are_verified() {
    let a = b"prefix old tail\r\n"; let b = b"prefix NEW tail\r\n";
    let (mut d, before, after) = setup(a, b); let comparison = prepare(&mut d, before, after);
    assert_eq!(comparison.quality(), ComparisonQuality::Exact);
    assert_eq!(comparison.relation(), ComparisonRelation::Different);
    let (mut old_end, mut new_end) = (0, 0);
    for s in comparison.spans(&d, 3, 1).unwrap() {
        assert_eq!(s.before().start().get(), old_end); assert_eq!(s.after().start().get(), new_end);
        old_end = s.before().end().get(); new_end = s.after().end().get();
        if s.kind() == CorrespondenceKind::Equal {
            let (i, j) = s.before().as_usize_bounds().unwrap(); let (k, l) = s.after().as_usize_bounds().unwrap();
            assert_eq!(&a[i..j], &b[k..l]);
        }
    }
    assert_eq!((old_end, new_end), (a.len() as u64, b.len() as u64));
    let page = comparison.page(&mut d, 3, 1, 0, 128, || false).unwrap();
    assert_eq!(page.exit_code(), EXIT_OK); assert!(json(&page).get("comparison_complete").flag());
    assert_eq!(json(&page).get("additional_source_bytes_read").number(), 0);
    assert_eq!(d.model().revision(), 3); assert_eq!(d.model().history().len(), 2);
}

#[test]
fn exhausted_work_is_undetermined_not_an_identical_result() {
    let (mut d, a, b) = setup(b"same bytes", b"same bytes");
    let comparison = DeskComparison::prepare(&mut d, 3, a, b, 1,
        DeskComparisonLimits { max_work: 0, ..Default::default() }, || false).unwrap();
    assert_eq!(comparison.quality(), ComparisonQuality::WorkLimit);
    assert_eq!(comparison.relation(), ComparisonRelation::Undetermined);
    let page = comparison.page(&mut d, 3, 1, 0, 128, || false).unwrap();
    assert_eq!(page.exit_code(), EXIT_PARTIAL);
    let page = json(&page); assert!(!page.get("comparison_complete").flag());
    assert_eq!(page.get("unresolved_spans").number(), 1); assert_eq!(page.get("changed_spans").number(), 0);
    assert_eq!(page.get("edit_distance"), &Json::Null);
}

#[test]
fn edit_limit_preserves_verified_edges_and_labels_only_the_interior_unresolved() {
    let (mut d, a, b) = setup(b"head old tail", b"head NEW tail");
    let comparison = DeskComparison::prepare(&mut d, 3, a, b, 1,
        DeskComparisonLimits { max_edit_distance: 0, ..Default::default() }, || false).unwrap();
    assert_eq!(comparison.quality(), ComparisonQuality::EditLimit);
    assert_eq!(comparison.relation(), ComparisonRelation::Different);
    let spans = comparison.spans(&d, 3, 1).unwrap();
    assert_eq!(spans.len(), 3); assert_eq!(spans[0].kind(), CorrespondenceKind::Equal);
    assert_eq!(spans[1].kind(), CorrespondenceKind::Unresolved); assert_eq!(spans[2].kind(), CorrespondenceKind::Equal);
    assert_eq!(comparison.stats().edit_distance, None);
}

#[test]
fn independently_paged_windows_never_silently_truncate_a_large_unresolved_region() {
    let (mut d, a, b) = setup(b"abcdefghij", b"ABCDEFGHIJ");
    let comparison = DeskComparison::prepare(&mut d, 3, a, b, 1,
        DeskComparisonLimits { max_work: 0, ..Default::default() }, || false).unwrap();
    let first = json(&comparison.window(&mut d, 3, 1, 0, [0, 3], 4, || false).unwrap());
    assert_eq!(first.get("before").get("exact_utf8_text").text(), "abcd");
    assert_eq!(first.get("after").get("exact_utf8_text").text(), "DEFG");
    assert_eq!(first.get("before").get("next_skip").number(), 4);
    assert_eq!(first.get("after").get("next_skip").number(), 7);
    assert!(!first.get("before").get("whole_span_visible").flag());
    let last = json(&comparison.window(&mut d, 3, 1, 0, [8, 10], 4, || false).unwrap());
    assert_eq!(last.get("before").get("exact_utf8_text").text(), "ij");
    assert_eq!(last.get("after").get("original_hex").text(), "");
    assert_eq!(last.get("after").get("next_skip"), &Json::Null);
    assert!(!last.get("before").get("whole_span_visible").flag());
    assert!(comparison.window(&mut d, 3, 1, 0, [11, 0], 4, || false).is_err());
}

#[test]
fn insertion_side_selects_an_empty_caret_and_other_side_preserves_exact_bytes() {
    let (mut d, a, b) = setup(b"head tail", b"head INSERT tail"); let comparison = prepare(&mut d, a, b);
    let span = comparison.spans(&d, 3, 1).unwrap().iter().position(|s| s.kind() == CorrespondenceKind::Changed).unwrap();
    comparison.select(&mut d, 3, 4, 1, span, ComparisonSide::Before, || false).unwrap();
    assert!(d.model().selected_bytes(a, 4).unwrap().is_empty());
    assert_eq!(d.model().location(b, 4).unwrap().selection, None);
    comparison.select(&mut d, 4, 5, 1, span, ComparisonSide::After, || false).unwrap();
    assert_eq!(d.model().selected_bytes(b, 5).unwrap(), b"INSERT ");
    let original = d.model().location(b, 5).unwrap();
    d.apply(5, 6, DeskCommand::Bookmark { pane: b, label: "inserted content".into() }, || false).unwrap();
    assert_eq!(d.model().bookmarks()[0].location().selection, original.selection);
    d.apply(6, 7, DeskCommand::Back, || false).unwrap();
    d.apply(7, 8, DeskCommand::Forward, || false).unwrap();
    assert_eq!(d.model().selected_bytes(b, 8).unwrap(), b"INSERT ");
    assert!(comparison.validate_sources(&d, 8).is_ok());
}

#[test]
fn malformed_and_utf16_source_bytes_are_not_reconstructed_from_display_text() {
    let raw = [0xff, 0xfe, b'a', 0, b'\r', 0, b'\n', 0];
    let (mut d, a, b) = setup(&raw, &raw); let comparison = prepare(&mut d, a, b);
    let page = json(&comparison.window(&mut d, 3, 1, 0, [0, 0], 64, || false).unwrap());
    assert_eq!(page.get("before").get("original_hex").text(), "fffe61000d000a00");
    assert_eq!(page.get("before").get("exact_utf8_text"), &Json::Null);
    comparison.select(&mut d, 3, 4, 1, 0, ComparisonSide::Before, || false).unwrap();
    assert_eq!(d.model().selected_bytes(a, 4).unwrap(), raw);
}

#[test]
fn unicode_byte_windows_report_interior_scalars_without_lossy_replacement() {
    let (mut d, a, b) = setup("😀".as_bytes(), "😀".as_bytes()); let comparison = prepare(&mut d, a, b);
    let partial = json(&comparison.window(&mut d, 3, 1, 0, [1, 0], 2, || false).unwrap());
    assert_eq!(partial.get("before").get("original_hex").text(), "9f98");
    assert_eq!(partial.get("before").get("exact_utf8_text"), &Json::Null);
    assert_eq!(partial.get("after").get("exact_utf8_text"), &Json::Null);
    let whole = json(&comparison.window(&mut d, 3, 1, 0, [0, 0], 4, || false).unwrap());
    assert_eq!(whole.get("before").get("exact_utf8_text").text(), "😀");
}

#[test]
fn empty_captures_are_identical_and_a_duplicate_can_share_backing() {
    let (mut d, a, b) = setup(b"", b""); let comparison = prepare(&mut d, a, b);
    assert_eq!(comparison.relation(), ComparisonRelation::Identical);
    assert!(comparison.spans(&d, 3, 1).unwrap().is_empty());
    assert!(json(&comparison.page(&mut d, 3, 1, 0, 1, || false).unwrap()).get("spans").array().is_empty());
    assert_eq!(comparison.select(&mut d, 3, 4, 1, 0, ComparisonSide::Before, || false), Err(DeskComparisonError::MissingSpan));
    let clone = d.apply(3, 4, DeskCommand::Duplicate(a), || false).unwrap().active.unwrap();
    let same = prepare(&mut d, a, clone); assert_eq!(same.relation(), ComparisonRelation::Identical);
    assert!(std::ptr::eq(d.model().source(a, 4).unwrap().bytes(), d.model().source(clone, 4).unwrap().bytes()));
}

#[test]
fn replacing_either_pane_invalidates_old_comparison_navigation() {
    let (mut d, a, b) = setup(b"before", b"after"); let comparison = prepare(&mut d, a, b);
    d.adopt(3, 4, source(3, b"replacement"), 0, None, || false).unwrap();
    assert_eq!(comparison.select(&mut d, 4, 5, 1, 0, ComparisonSide::Before, || false), Err(DeskComparisonError::Analysis(ComparisonError::Stale)));
    assert_eq!(d.model().revision(), 4); assert_eq!(d.model().source(b, 4).unwrap().bytes(), b"replacement");
    d.apply(4, 5, DeskCommand::Back, || false).unwrap();
    assert!(comparison.validate_sources(&d, 5).is_ok());
    d.apply(5, 6, DeskCommand::Close(a), || false).unwrap();
    assert_eq!(comparison.validate_sources(&d, 6), Err(DeskComparisonError::Desk(DeskSessionError::Desk(DeskError::MissingPane))));
}

#[test]
fn stale_generations_limits_and_cancellation_preserve_selection_and_queries() {
    let (mut d, a, b) = setup(b"old needle", b"new needle"); let comparison = prepare(&mut d, a, b);
    d.search(3, b, 1, "needle", 10, 100, || false).unwrap();
    assert!(matches!(DeskComparison::prepare(&mut d, 3, a, b, 2, DeskComparisonLimits::default(), || true), Err(e) if e.is_canceled()));
    assert!(matches!(DeskComparison::prepare(&mut d, 3, a, b, 2,
        DeskComparisonLimits { max_source_bytes: 1, ..Default::default() }, || false), Err(DeskComparisonError::Analysis(ComparisonError::Limits))));
    assert_eq!(comparison.select(&mut d, 3, 4, 2, 0, ComparisonSide::After, || false), Err(DeskComparisonError::Analysis(ComparisonError::Stale)));
    assert_eq!(comparison.select(&mut d, 2, 4, 1, 0, ComparisonSide::After, || false), Err(DeskComparisonError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert_eq!(d.model().revision(), 3); assert_eq!(d.accepted_query(), Some(1));
    assert!(comparison.page(&mut d, 3, 1, 0, 128, || false).is_ok());
}

#[test]
fn cancellation_at_final_prepare_checkpoint_does_not_move_either_pane() {
    let (mut probe, a, b) = setup(b"old", b"new"); let mut calls = 0;
    DeskComparison::prepare(&mut probe, 3, a, b, 1, DeskComparisonLimits::default(), || { calls += 1; false }).unwrap();
    let (mut d, a, b) = setup(b"old", b"new"); let mut seen = 0;
    assert!(matches!(DeskComparison::prepare(&mut d, 3, a, b, 1, DeskComparisonLimits::default(), || { seen += 1; seen == calls }), Err(e) if e.is_canceled()));
    assert_eq!(d.model().revision(), 3); assert_eq!(d.model().location(a, 3).unwrap().selection, None);
    assert_eq!(d.model().location(b, 3).unwrap().selection, None);
}

#[test]
fn same_pane_is_rejected_but_different_panes_need_not_share_a_logical_file_id() {
    let (mut d, a, b) = setup(b"alpha", b"beta");
    assert!(matches!(DeskComparison::prepare(&mut d, 3, a, a, 1, DeskComparisonLimits::default(), || false), Err(DeskComparisonError::SamePane)));
    let comparison = prepare(&mut d, a, b); let page = json(&comparison.page(&mut d, 3, 1, 0, 128, || false).unwrap());
    assert_ne!(page.get("before_source").get("file_id"), page.get("after_source").get("file_id"));
    assert_eq!(page.get("alignment_semantics").text(), "deterministic-not-identity-proof");
}
