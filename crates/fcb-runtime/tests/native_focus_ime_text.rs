#![forbid(unsafe_code)]

//! Unit and integration test suite for G1 native focus, IME, VoiceOver/AX route,
//! and source clipboard round-trip (FCB-077.B).

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fcb_core::{
    geometry::{DisplayColorConfig, DisplayMetrics, Point2D, Rect2D, SemanticGeometry, Size2D},
    AcceptedLayoutIdentity, AcceptedLayoutSnapshot, ArenaOwnerId, ByteLength, CoreError,
    DisplayGeneration, FileId, LayoutRevision, PendingRangeStatus, RangeRequestToken, RootId,
    SemanticFocusState, SemanticNode, SemanticNodeId, SemanticRole, SourceRevision,
    Utf16CodeUnitOffset, Utf16CodeUnitRange, NATIVE_NOT_FOUND,
};
use fcb_runtime::{
    accessibility::{AxLineIndex, AxNotification, AxRole, NativeAxRoute},
    clipboard::{
        ClipboardError, ClipboardLimits, ClipboardPayload, ClipboardRoundTrip,
        NativeClipboard,
    },
    ime::{ImeClient, ImeEvent, ImeOutcome},
    responder::{HostResponderChain, ResponderAction, ResponderId},
};
use fcb_source::confined::{ConfinedSourceReader, SymlinkPolicy};
use fcb_source::path::{NormalizedPath, RawPath};
use fcb_source::root::RootGrant;
use fcb_source::CancelFlag;

/// Real small source containing the required multi-script corpus:
/// ASCII, CJK multibyte ("こんにちは"), astral emoji surrogate pairs ("🚦🚀"),
/// and combining mark ("cafe\u{301}").
const SOURCE_BYTES: &str = concat!(
    "fn main() {\n",
    "    let greeting = \"こんにちは\";\n",
    "    let flags = \"🚦🚀\";\n",
    "    let cafe = \"cafe\u{301}\";\n",
    "}\n",
);

struct TempRoot {
    path: PathBuf,
}

impl TempRoot {
    fn new(label: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("fcb-077b-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp root creates");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempRoot {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0772).expect("owner id valid")
}

fn file_id(n: u64) -> FileId {
    FileId::new(owner(), n).expect("file id valid")
}

fn revision(n: u64) -> SourceRevision {
    SourceRevision::new(owner(), n).expect("revision valid")
}

fn reader_for(root: &Path) -> ConfinedSourceReader {
    let root_id = RootId::new(owner(), 1).expect("root id valid");
    let grant = RootGrant::new(root_id, RawPath::from_str(&root.to_string_lossy()));
    ConfinedSourceReader::new(grant, SymlinkPolicy::AllowWithinRoot, ByteLength::new(1 << 20))
}

fn normalized(rel: &str) -> NormalizedPath {
    NormalizedPath::new(RawPath::from_str(rel)).expect("test relative path normalizes")
}

fn total_utf16_units(text: &str) -> u64 {
    text.chars().map(|ch| u64::from(ch.len_utf16() as u16)).sum()
}

fn line_starts_utf16(text: &str) -> Vec<u64> {
    let mut starts = vec![0u64];
    let mut running = 0u64;
    for ch in text.chars() {
        running += u64::from(ch.len_utf16() as u16);
        if ch == '\n' {
            starts.push(running);
        }
    }
    starts
}

fn build_layout_snapshot(text: &str) -> (AcceptedLayoutSnapshot, Vec<SemanticNodeId>, LayoutRevision) {
    let o = owner();
    let layout_rev = LayoutRevision::new(o, 1).expect("layout rev");
    let source_rev = SourceRevision::new(o, 1).expect("source rev");
    let disp_gen = DisplayGeneration::new(o, 1).expect("display gen");
    let identity = AcceptedLayoutIdentity::new(o, layout_rev, source_rev, disp_gen, None)
        .expect("identity");

    let metrics = DisplayMetrics::new(
        2.0,
        Size2D::new(800.0, 600.0).expect("logical size"),
        DisplayColorConfig::Srgb,
        disp_gen,
    )
    .expect("metrics");

    let root_id = SemanticNodeId::new(o, 1).expect("root id");
    let mut nodes = BTreeMap::new();

    let root_rect = Rect2D::from_xywh(0.0, 0.0, 800.0, 600.0).expect("rect");
    let root_geom = SemanticGeometry::new(root_rect, root_rect, None).expect("geom");
    let mut root_node = SemanticNode::new(root_id, root_geom, SemanticRole::Document, false);
    root_node.set_label(Some("Main Source Document".to_string()));

    let starts = line_starts_utf16(text);
    let total_units = total_utf16_units(text);
    let mut line_ids = Vec::new();

    let lines: Vec<&str> = text.split_inclusive('\n').collect();
    for (i, line) in lines.iter().enumerate() {
        let node_id = SemanticNodeId::new(o, (i + 2) as u64).expect("node id");
        let start_unit = *starts.get(i).unwrap_or(&0);
        let end_unit = if i + 1 < starts.len() {
            *starts.get(i + 1).unwrap_or(&total_units)
        } else {
            total_units
        };

        let u_start = Utf16CodeUnitOffset::new(start_unit);
        let u_end = Utf16CodeUnitOffset::new(end_unit);
        let text_range = Utf16CodeUnitRange::new(u_start, u_end).ok();

        let line_rect = Rect2D::from_xywh(
            10.0,
            10.0 + (i as f64 * 20.0),
            500.0,
            18.0,
        )
        .expect("rect");
        let geom = SemanticGeometry::new(line_rect, line_rect, None).expect("geom");

        let mut node = SemanticNode::new(node_id, geom, SemanticRole::SourceText, true);
        node.set_label(Some(format!("Line {}", i + 1)));
        node.set_value(Some((*line).to_string()));
        node.set_text_range(text_range);
        node.set_parent(Some(root_id)).expect("set parent");

        root_node.add_child(node_id).expect("add child");
        nodes.insert(node_id, node);
        line_ids.push(node_id);
    }

    nodes.insert(root_id, root_node);
    let snapshot = AcceptedLayoutSnapshot::new(identity, metrics, root_id, nodes)
        .expect("snapshot valid");
    (snapshot, line_ids, layout_rev)
}

#[test]
fn test_01_real_source_capture_and_semantic_ax_tree() {
    let root = TempRoot::new("ax-tree");
    fs::write(root.path().join("source.rs"), SOURCE_BYTES).expect("write source");

    let reader = reader_for(root.path());
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(file_id(1), revision(1), &normalized("source.rs"), &cancel)
        .expect("read file");

    let text = std::str::from_utf8(capture.bytes()).expect("utf8 text");
    assert_eq!(text, SOURCE_BYTES);

    let (layout, line_ids, _layout_rev) = build_layout_snapshot(text);
    let ax = NativeAxRoute::new(owner());

    let root_node = layout.node(layout.root_node()).expect("root node");
    assert_eq!(ax.role(root_node), AxRole::Group);
    assert_eq!(ax.role(root_node).ax_identifier(), "AXGroup");
    assert_eq!(ax.label(root_node), Some("Main Source Document"));

    // Line 2: "let greeting = \"こんにちは\";\n"
    let line2_id = *line_ids.get(1).expect("line 2 id");
    let line2_node = layout.node(line2_id).expect("line 2 node");
    assert_eq!(ax.role(line2_node), AxRole::SourceText);
    assert_eq!(ax.role(line2_node).ax_identifier(), "AXStaticText");
    assert!(ax.number_of_characters(line2_node) > 0);
    assert_eq!(ax.label(line2_node), Some("Line 2"));
}

#[test]
fn test_02_virtualized_pending_text_range_resolution() {
    let root = TempRoot::new("pending-range");
    fs::write(root.path().join("source.rs"), SOURCE_BYTES).expect("write source");
    let reader = reader_for(root.path());
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(file_id(1), revision(1), &normalized("source.rs"), &cancel)
        .expect("read file");
    let text = std::str::from_utf8(capture.bytes()).expect("utf8");
    let (layout, line_ids, layout_rev) = build_layout_snapshot(text);
    let ax = NativeAxRoute::new(owner());

    let line2_id = *line_ids.get(1).expect("line 2 id");
    let line2_node = layout.node(line2_id).expect("line 2");
    let range = line2_node.text_range().expect("text range");
    let range_req = RangeRequestToken::new(owner(), 1, line2_id, layout_rev).expect("range request valid");

    // Virtualized query without prepared text returns Pending (no AppKit UI stall)
    let pending_status = ax.string_for_range(range_req, &layout, None, range, &[]);
    assert!(matches!(
        pending_status,
        PendingRangeStatus::Pending {
            requested_range,
            ..
        } if requested_range == range
    ));

    // When background context completes, returns Ready with verified text
    let ready_status = ax.string_for_range(range_req, &layout, Some(text), range, &[]);
    if let PendingRangeStatus::Ready(resolved) = ready_status {
        assert_eq!(resolved.range(), range);
        assert!(resolved.text().contains("こんにちは"));
    } else {
        assert!(false, "expected Ready status");
    }
}

#[test]
fn test_03_wrong_offset_and_sentinel_negative_controls() {
    let root = TempRoot::new("neg-controls");
    fs::write(root.path().join("source.rs"), SOURCE_BYTES).expect("write source");
    let reader = reader_for(root.path());
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(file_id(1), revision(1), &normalized("source.rs"), &cancel)
        .expect("read file");
    let text = std::str::from_utf8(capture.bytes()).expect("utf8");
    let (layout, line_ids, layout_rev) = build_layout_snapshot(text);
    let ax = NativeAxRoute::new(owner());

    // Negative control 1: Native sentinel NATIVE_NOT_FOUND is refused, not clamped
    let sentinel = Utf16CodeUnitOffset::new(NATIVE_NOT_FOUND);
    let zero = Utf16CodeUnitOffset::new(0);
    let sent_range = Utf16CodeUnitRange::new(zero, sentinel).expect("range");
    let line1_id = *line_ids.first().expect("line 1 id");
    let token1 = RangeRequestToken::new(owner(), 2, line1_id, layout_rev).expect("request descriptor 1");
    let res1 = ax.string_for_range(token1, &layout, Some(text), sent_range, &[]);
    assert!(matches!(res1, PendingRangeStatus::Refused(CoreError::NativeSentinel)));

    // Negative control 2: Mid-surrogate split in astral emoji "🚦" (line 3) is refused
    let starts = line_starts_utf16(text);
    let line3_start = *starts.get(2).expect("line 3 start");
    // Find offset of emoji: "    let flags = \"" has 17 chars before 🚦
    let emoji_offset = line3_start + 17;
    let mid_surrogate = Utf16CodeUnitOffset::new(emoji_offset + 1);
    let end_offset = Utf16CodeUnitOffset::new(emoji_offset + 2);
    let bad_range = Utf16CodeUnitRange::new(mid_surrogate, end_offset).expect("range");
    let line3_id = *line_ids.get(2).expect("line 3 id");
    let token2 = RangeRequestToken::new(owner(), 3, line3_id, layout_rev).expect("request descriptor 2");
    let res2 = ax.string_for_range(token2, &layout, Some(text), bad_range, &[]);
    assert!(matches!(res2, PendingRangeStatus::Refused(CoreError::InvalidUtf16)));

    // Negative control 3: Out-of-bounds offset exceeds total units
    let total_units = total_utf16_units(text);
    let oob_start = Utf16CodeUnitOffset::new(total_units + 100);
    let oob_end = Utf16CodeUnitOffset::new(total_units + 200);
    let oob_range = Utf16CodeUnitRange::new(oob_start, oob_end).expect("range");
    let token3 = RangeRequestToken::new(owner(), 4, line1_id, layout_rev).expect("request descriptor 3");
    let res3 = ax.string_for_range(token3, &layout, Some(text), oob_range, &[]);
    assert!(matches!(res3, PendingRangeStatus::Refused(CoreError::LimitExceeded)));
}

#[test]
fn test_04_ax_line_index_and_bounds_for_range() {
    let text = SOURCE_BYTES;
    let starts = line_starts_utf16(text);
    let total_units = total_utf16_units(text);
    let index = AxLineIndex::new(starts.clone(), total_units).expect("line index");

    assert_eq!(index.line_count(), starts.len());
    assert_eq!(index.total_units(), total_units);

    // Line lookup: offset 0 is line 1
    assert_eq!(index.line_for_offset(Utf16CodeUnitOffset::new(0)), Ok(1));
    // Line 2 start
    assert_eq!(index.line_for_offset(Utf16CodeUnitOffset::new(starts[1])), Ok(2));

    // Range for line 1
    let l1_range = index.range_for_line(1).expect("line 1 range");
    assert_eq!(l1_range.start().get(), starts[0]);
    assert_eq!(l1_range.end().get(), starts[1]);

    // Bounds calculation
    let (layout, line_ids, _) = build_layout_snapshot(text);
    let ax = NativeAxRoute::new(owner());
    let line1_id = *line_ids.first().expect("line 1");
    let line1_node = layout.node(line1_id).expect("line 1 node");
    let bounds = ax.bounds_for_range(line1_node, l1_range, 18.0, 8.0).expect("bounds");
    assert!(bounds.size().width() > 0.0);
    assert_eq!(bounds.size().height(), 18.0);
}

#[test]
fn test_05_point_hit_testing_in_accepted_layout() {
    let (layout, line_ids, _) = build_layout_snapshot(SOURCE_BYTES);
    let ax = NativeAxRoute::new(owner());

    // Point in line 1: (15.0, 15.0) falls within (10..510, 10..28)
    let hit1 = ax.hit_test(Point2D::new(15.0, 15.0).expect("pt"), &layout);
    assert_eq!(hit1, Some(line_ids[0]));

    // Point in line 2: (15.0, 35.0) falls within (10..510, 30..48)
    let hit2 = ax.hit_test(Point2D::new(15.0, 35.0).expect("pt"), &layout);
    assert_eq!(hit2, Some(line_ids[1]));

    // Point completely outside layout bounds
    let miss = ax.hit_test(Point2D::new(900.0, 900.0).expect("pt"), &layout);
    assert_eq!(miss, None);
}

#[test]
fn test_06_keyboard_focus_walk_and_bounded_return_stack() {
    let (layout, line_ids, _) = build_layout_snapshot(SOURCE_BYTES);
    let ax = NativeAxRoute::new(owner());
    let mut focus_state = SemanticFocusState::new(owner(), 4);

    assert_eq!(ax.focused_element(&focus_state, &layout), None);

    // Focus next: moves to line 1
    let next1 = ax.focus_next(&mut focus_state, &layout, &line_ids).expect("focus next");
    assert_eq!(next1, Some(line_ids[0]));
    let (f_id, f_bounds) = ax.focused_element(&focus_state, &layout).expect("focused elem");
    assert_eq!(f_id, line_ids[0]);
    assert!(f_bounds.size().width() > 0.0);

    // Focus next: moves to line 2
    let next2 = ax.focus_next(&mut focus_state, &layout, &line_ids).expect("focus next");
    assert_eq!(next2, Some(line_ids[1]));

    // Focus prev: moves back to line 1
    let prev1 = ax.focus_prev(&mut focus_state, &layout, &line_ids).expect("focus prev");
    assert_eq!(prev1, Some(line_ids[0]));

    // Check emitted accessibility notifications
    let notifs = ax.drain_notifications();
    assert_eq!(notifs.len(), 3);
    assert!(notifs.iter().all(|(n, _)| *n == AxNotification::FocusedUIElementChanged));

    // Blur clears focus but preserves return stack
    focus_state.blur();
    assert_eq!(focus_state.current_focus(), None);
    let restored = focus_state.return_focus(&layout).expect("return focus");
    assert!(restored.is_some());
}

#[test]
fn test_07_host_responder_chain_dispatch_without_global_taps() {
    let o = owner();
    let mut chain = HostResponderChain::new(o);

    let view_id = ResponderId::new(o, 10);
    let pane_id = ResponderId::new(o, 20);
    let window_id = ResponderId::new(o, 30);

    // First responder (focused text view) handles Copy and SelectAll
    chain.push_first_responder(view_id, "FocusedTextView", |act| {
        matches!(act, ResponderAction::Copy | ResponderAction::SelectAll)
    });

    // Parent reading pane handles PageUp, PageDown
    chain.push_fallback_responder(pane_id, "ReadingPane", |act| {
        matches!(act, ResponderAction::PageUp | ResponderAction::PageDown)
    });

    // Window handles Find, DismissOverlay
    chain.push_fallback_responder(window_id, "HostWindow", |act| {
        matches!(act, ResponderAction::Find | ResponderAction::DismissOverlay)
    });

    // Copy dispatched to first responder
    assert_eq!(chain.dispatch(ResponderAction::Copy), Some(view_id));
    // PageDown bubbles up to reading pane
    assert_eq!(chain.dispatch(ResponderAction::PageDown), Some(pane_id));
    // Find bubbles up to window
    assert_eq!(chain.dispatch(ResponderAction::Find), Some(window_id));
    // MoveLeft unhandled in this chain
    assert_eq!(chain.dispatch(ResponderAction::MoveLeft), None);
}

#[test]
fn test_08_ime_multi_stage_composition_suppresses_queries() {
    let mut ime = ImeClient::new(owner());
    assert!(!ime.has_marked_text());

    // Step 1: User types "konn" (marked text set to "こん")
    let outcome1 = ime.handle_event(ImeEvent::SetMarkedText {
        text: "こん".to_string(),
        selection_in_marked: (0, 2),
        replacement_range: None,
    });
    if let ImeOutcome::CompositionUpdated {
        marked_text,
        query_suppressed,
    } = outcome1 {
        assert_eq!(marked_text, "こん");
        assert!(query_suppressed, "intermediate IME composition MUST suppress search queries");
    } else {
        assert!(false, "unexpected outcome");
    }
    assert!(ime.has_marked_text());
    assert_eq!(ime.marked_text(), Some("こん"));

    // Candidate window rect calculation
    let candidate_rect = ime
        .candidate_window_rect(Point2D::new(50.0, 100.0).expect("pt"), 20.0, 8.0)
        .expect("candidate rect");
    assert!(candidate_rect.size().width() > 0.0);
    assert_eq!(candidate_rect.min_y(), 100.0);

    // Step 2: User completes Kana conversion to "こんにちは"
    let outcome2 = ime.handle_event(ImeEvent::SetMarkedText {
        text: "こんにちは".to_string(),
        selection_in_marked: (0, 5),
        replacement_range: None,
    });
    assert!(matches!(
        outcome2,
        ImeOutcome::CompositionUpdated {
            query_suppressed: true,
            ..
        }
    ));

    // Step 3: User hits Enter to confirm (InsertText)
    let outcome3 = ime.handle_event(ImeEvent::InsertText {
        text: "こんにちは".to_string(),
        replacement_range: None,
    });
    if let ImeOutcome::FinalizedTextInserted { text, .. } = outcome3 {
        assert_eq!(text, "こんにちは");
    } else {
        assert!(false, "expected FinalizedTextInserted");
    }
    assert!(!ime.has_marked_text());
}

#[test]
fn test_09_actual_source_clipboard_round_trip() {
    let root = TempRoot::new("clipboard-rt");
    fs::write(root.path().join("source.rs"), SOURCE_BYTES).expect("write source");
    let reader = reader_for(root.path());
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(file_id(1), revision(1), &normalized("source.rs"), &cancel)
        .expect("read file");
    let bytes = capture.bytes();

    // Select line 3: `    let flags = "🚦🚀";\n`
    let text = std::str::from_utf8(bytes).expect("utf8");
    let line3_start = text.find("    let flags").expect("line 3 find");
    let newline_offset = text.get(line3_start..).and_then(|s| s.find('\n')).expect("newline");
    let line3_end = line3_start + newline_offset + 1;
    let range = line3_start..line3_end;

    let payload = ClipboardPayload::stage(
        bytes,
        range.clone(),
        "source.rs",
        1,
        3,
        3,
        ClipboardLimits::default(),
    )
    .expect("stage clipboard payload");

    assert!(payload.plain_text.contains("🚦🚀"));
    assert!(!payload.has_replacement_characters);
    assert_eq!(&payload.exact_bytes, &bytes[range.clone()]);
    assert!(payload.provenance.contains("source.rs"));

    let mut clipboard = NativeClipboard::new();
    let seq = clipboard.generation_seq();
    clipboard.publish(payload, seq).expect("publish clipboard");

    // Verify round-trip byte-for-byte fidelity
    let matched = ClipboardRoundTrip::verify_round_trip(bytes, range, &clipboard);
    assert!(matched, "clipboard exact bytes must match original captured source bytes");
}

#[test]
fn test_10_clipboard_budget_refusal_negative_control() {
    let data = vec![b'A'; 2000];
    let limits = ClipboardLimits {
        max_clipboard_bytes: 500,
    };

    let result = ClipboardPayload::stage(&data, 0..1500, "large.rs", 1, 1, 50, limits);
    if let Err(ClipboardError::BudgetExceeded {
        requested_bytes,
        max_budget_bytes,
    }) = result {
        assert_eq!(requested_bytes, 1500);
        assert_eq!(max_budget_bytes, 500);
    } else {
        assert!(false, "expected BudgetExceeded");
    }
}

#[test]
fn test_11_clipboard_concurrent_external_change_negative_control() {
    let data = b"local content to copy";
    let payload = ClipboardPayload::stage(
        data,
        0..data.len(),
        "test.rs",
        1,
        1,
        1,
        ClipboardLimits::default(),
    )
    .expect("stage");

    let mut clipboard = NativeClipboard::new();
    let initial_seq = clipboard.generation_seq();

    // External process updates clipboard in the background
    clipboard.simulate_external_change("another application copied this");
    assert_eq!(clipboard.generation_seq(), initial_seq + 1);

    // Attempt to publish using stale generation token fails safely
    let err = clipboard.publish(payload, initial_seq).unwrap_err();
    assert!(matches!(
        err,
        ClipboardError::ConcurrentExternalChange {
            expected_seq,
            actual_seq,
        } if expected_seq == initial_seq && actual_seq == initial_seq + 1
    ));

    // Foreign clipboard contents remain intact
    assert_eq!(clipboard.plain_text(), Some("another application copied this"));
}
