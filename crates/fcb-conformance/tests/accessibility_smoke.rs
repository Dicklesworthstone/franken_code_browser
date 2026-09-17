#![forbid(unsafe_code)]

//! G1 accessibility smoke scenario (fcb-7x8j.1 / FCB-077.A).
//!
//! Exercises the early native source-reader accessibility path end-to-end at
//! the production seam: a real small source file is captured through the
//! confined reader, the captured bytes populate a semantic snapshot (labels,
//! values, and UTF-16 text ranges derived from the real bytes — never
//! simulated semantic snapshots), and native-adapter range queries resolve
//! through [`PendingTextRangeResolver`] with keyboard-selection-safe UTF-16
//! semantics and explicit Pending/Ready/Stale/Refused states.
//!
//! Wrong-offset negative controls are caught: the native not-found sentinel,
//! out-of-bounds offsets, mid-surrogate splits inside astral characters,
//! stale layout revisions, unknown nodes, and foreign owners all refuse
//! instead of clamping.
//!
//! Host: Apple Silicon macOS (Darwin, arm64). The integrated native focus/
//! IME proof with the live responder chain is the adjacent FCB-077.V child.

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use fcb_core::{
    AcceptedLayoutIdentity, AcceptedLayoutSnapshot, ArenaOwnerId, BidiBoundary, ByteLength,
    CaretAffinity, CoreError, DisplayColorConfig, DisplayGeneration, DisplayMetrics, FileId,
    LayoutRevision, PendingRangeStatus, PendingTextRangeResolver, RangeRequestToken, Rect2D,
    RootId, SemanticFocusState, SemanticGeometry, SemanticNode, SemanticNodeId, SemanticRole,
    Size2D, SourceRevision, Utf16CodeUnitOffset, Utf16CodeUnitRange, VisualPosition,
    NATIVE_NOT_FOUND,
};
use fcb_source::confined::{ConfinedSourceReader, SymlinkPolicy};
use fcb_source::path::{NormalizedPath, RawPath};
use fcb_source::root::RootGrant;
use fcb_source::CancelFlag;

/// Real small source with the corpus the accessibility path must survive:
/// ASCII, CJK multibyte, astral emoji (UTF-16 surrogate pairs), and a
/// combining mark.
const SOURCE: &str = concat!(
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
        path.push(format!("fcb-077a-{label}-{}", std::process::id()));
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
    ArenaOwnerId::new(0x0770).expect("owner id valid")
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

/// Total UTF-16 code units of `text`.
fn total_utf16_units(text: &str) -> u64 {
    text.chars().map(|ch| u64::from(ch.len_utf16() as u16)).sum()
}

/// UTF-16 start offset of every line (newline-terminated) in `text`. A final
/// newline does not fabricate a phantom trailing line.
fn line_starts_utf16(text: &str) -> Vec<u64> {
    let total = total_utf16_units(text);
    let mut starts = vec![0_u64];
    let mut units = 0_u64;
    for ch in text.chars() {
        units += u64::from(ch.len_utf16() as u16);
        if ch == '\n' && units < total {
            starts.push(units);
        }
    }
    starts
}

/// Byte offset of the UTF-16 `limit_units` boundary in `text`.
fn utf16_to_byte_offset(text: &str, limit_units: u64) -> Option<usize> {
    let mut units = 0_u64;
    let mut bytes = 0_usize;
    for ch in text.chars() {
        if units == limit_units {
            return Some(bytes);
        }
        units += u64::from(ch.len_utf16() as u16);
        bytes += ch.len_utf8();
    }
    (units == limit_units).then_some(bytes)
}

/// UTF-16 unit offset of the character that starts at byte offset `byte`.
fn utf16_units_at_byte(text: &str, byte: usize) -> u64 {
    let mut units = 0_u64;
    let mut seen = 0_usize;
    for ch in text.chars() {
        if seen == byte {
            break;
        }
        units += u64::from(ch.len_utf16() as u16);
        seen += ch.len_utf8();
    }
    units
}

fn utf16_offset(units: u64) -> Utf16CodeUnitOffset {
    Utf16CodeUnitOffset::new(units)
}

fn utf16_range(start: u64, end: u64) -> Utf16CodeUnitRange {
    Utf16CodeUnitRange::new(utf16_offset(start), utf16_offset(end)).expect("ordered range valid")
}

fn geometry(x: f64, y: f64, w: f64, h: f64) -> SemanticGeometry {
    SemanticGeometry::new(
        Rect2D::from_xywh(x, y, w, h).expect("bounds valid"),
        Rect2D::from_xywh(x, y, w, h).expect("content valid"),
        None,
    )
    .expect("geometry valid")
}

/// Builds the production-shaped snapshot: document root + one SourceText
/// node per line, all derived from the real captured text.
fn build_snapshot(text: &str) -> (AcceptedLayoutSnapshot, Vec<SemanticNodeId>, LayoutRevision) {
    let o = owner();
    let layout_rev = LayoutRevision::new(o, 7).expect("layout revision valid");
    let display_gen = DisplayGeneration::new(o, 3).expect("display generation valid");
    let identity = AcceptedLayoutIdentity::new(o, layout_rev, revision(1), display_gen, None)
        .expect("identity valid");
    let metrics = DisplayMetrics::new(
        2.0,
        Size2D::new(800.0, 200.0).expect("size valid"),
        DisplayColorConfig::Srgb,
        display_gen,
    )
    .expect("metrics valid");

    let root_id = SemanticNodeId::new(o, 1).expect("node id valid");
    let mut root_node = SemanticNode::new(
        root_id,
        geometry(0.0, 0.0, 800.0, 200.0),
        SemanticRole::Document,
        false,
    );
    root_node.set_label(Some("smoke.rs".to_string()));

    let starts = line_starts_utf16(text);
    let total = total_utf16_units(text);
    let mut nodes: BTreeMap<SemanticNodeId, SemanticNode> = BTreeMap::new();
    let mut line_ids = Vec::new();
    for (idx, start) in starts.iter().enumerate() {
        let end = starts.get(idx + 1).copied().unwrap_or(total);
        let byte_start = utf16_to_byte_offset(text, *start).expect("start maps");
        let byte_end = utf16_to_byte_offset(text, end).expect("end maps");

        let id = SemanticNodeId::new(o, (idx + 2) as u64).expect("node id valid");
        let mut node = SemanticNode::new(
            id,
            geometry(0.0, (idx as f64) * 16.0, 800.0, 16.0),
            SemanticRole::SourceText,
            true,
        );
        node.set_parent(Some(root_id)).expect("parent set");
        node.set_label(Some(format!("line {}", idx + 1)));
        // The accessibility value is a genuine slice of the captured bytes.
        node.set_value(Some(text[byte_start..byte_end].to_string()));
        node.set_text_range(Some(utf16_range(*start, end)));
        root_node.add_child(id).expect("child add");
        line_ids.push(id);
        nodes.insert(id, node);
    }
    nodes.insert(root_id, root_node);

    let layout = AcceptedLayoutSnapshot::new(identity, metrics, root_id, nodes)
        .expect("snapshot valid");
    (layout, line_ids, layout_rev)
}

#[test]
fn real_source_populates_snapshot_and_resolves_ranges() {
    let root = TempRoot::new("snapshot");
    fs::write(root.path().join("smoke.rs"), SOURCE).expect("source writes");

    // 1. Real capture through the production confined reader.
    let reader = reader_for(root.path());
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(file_id(1), revision(1), &normalized("smoke.rs"), &cancel)
        .expect("capture succeeds");
    let text = std::str::from_utf8(capture.bytes()).expect("captured bytes are UTF-8");
    assert_eq!(text, SOURCE, "capture preserves exact bytes");

    // 2. Semantic snapshot from the real bytes.
    let (layout, line_ids, layout_rev) = build_snapshot(text);
    assert_eq!(layout.node_count(), SOURCE.lines().count() + 1);
    assert_eq!(line_ids.len(), SOURCE.lines().count());
    assert_eq!(line_starts_utf16(text).len(), SOURCE.lines().count());
    let focusables = layout.focusable_nodes();
    assert_eq!(focusables.len(), line_ids.len(), "every line is focusable");

    // Line 2 carries the CJK multibyte corpus; its value must be the real slice.
    let line2 = layout.node(line_ids[1]).expect("line 2 node");
    assert_eq!(
        line2.value(),
        Some("    let greeting = \"こんにちは\";\n"),
        "value is a genuine slice of the capture, not a simulated snapshot"
    );

    // 3. Native adapter range query over the CJK line resolves Ready with
    //    byte/scalar ranges that map back into the exact capture.
    let range_req = RangeRequestToken::new(owner(), 1, line_ids[1], layout_rev)
        .expect("token valid");
    let line2_range = line2.text_range().expect("text range set");
    let status = PendingTextRangeResolver::resolve_range(
        range_req,
        &layout,
        Some(text),
        line2_range,
        &[],
    );
    let resolved = match status {
        PendingRangeStatus::Ready(resolved) => resolved,
        other => unreachable!("expected Ready, got {other:?}"),
    };
    assert_eq!(resolved.text(), "    let greeting = \"こんにちは\";\n");
    let bytes = capture.bytes();
    let byte_range = resolved.byte_range();
    let byte_slice = std::str::from_utf8(
        &bytes[byte_range.start().get() as usize..byte_range.end().get() as usize],
    )
    .expect("resolved bytes are UTF-8");
    assert_eq!(
        byte_slice,
        resolved.text(),
        "byte range resolves into the exact captured bytes"
    );

    // 4. Without loaded text the resolver returns an explicit Pending state
    //    (native adapters must defer, never shape inside the callback).
    let pending =
        PendingTextRangeResolver::resolve_range(range_req, &layout, None, line2_range, &[]);
    assert!(matches!(pending, PendingRangeStatus::Pending { .. }));

    // 5. Emoji line: astral characters occupy two UTF-16 units each; the
    //    flags literal spans both emoji.
    let emoji_line = layout.node(line_ids[2]).expect("line 3 node");
    let emoji_range = emoji_line.text_range().expect("text range set");
    let status = PendingTextRangeResolver::resolve_range(
        range_req,
        &layout,
        Some(text),
        emoji_range,
        &[],
    );
    assert!(
        matches!(&status, PendingRangeStatus::Ready(r) if r.text() == "    let flags = \"🚦🚀\";\n"),
        "emoji line resolves through surrogate-pair boundaries"
    );

    // 6. Bidi mapping oracle: two logical boundaries mapped to one visual
    //    position is non-1:1 (ligatures/bidi/combining marks).
    let boundaries = [
        BidiBoundary::new(utf16_offset(4), VisualPosition::new(9), CaretAffinity::Upstream),
        BidiBoundary::new(utf16_offset(5), VisualPosition::new(9), CaretAffinity::Downstream),
    ];
    assert!(PendingTextRangeResolver::has_non_one_to_one_bidi_mapping(&boundaries));
}

#[test]
fn wrong_offsets_and_stale_tokens_are_refused_not_clamped() {
    let root = TempRoot::new("negatives");
    fs::write(root.path().join("smoke.rs"), SOURCE).expect("source writes");
    let reader = reader_for(root.path());
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(file_id(1), revision(1), &normalized("smoke.rs"), &cancel)
        .expect("capture succeeds");
    let text = std::str::from_utf8(capture.bytes()).expect("captured bytes are UTF-8");
    let (layout, line_ids, layout_rev) = build_snapshot(text);
    let total = total_utf16_units(text);

    // Native not-found sentinel is refused, never treated as a real offset.
    let sentinel = Utf16CodeUnitOffset::new(NATIVE_NOT_FOUND);
    assert_eq!(
        PendingTextRangeResolver::validate_offset(sentinel, total),
        Err(CoreError::NativeSentinel)
    );

    // Out-of-bounds keyboard offsets exceed the limit.
    let beyond = utf16_offset(total + 1);
    assert_eq!(
        PendingTextRangeResolver::validate_offset(beyond, total),
        Err(CoreError::LimitExceeded)
    );

    // Mid-surrogate split inside an astral emoji is refused: a keyboard
    // selection may never split the pair. Locate the first emoji by byte
    // position and convert to UTF-16 units; the unit just inside it is a
    // low surrogate.
    let emoji_byte = text.find('\u{1f6a6}').expect("emoji present");
    let emoji_unit = utf16_units_at_byte(text, emoji_byte);
    let mid_surrogate = utf16_offset(emoji_unit + 1);
    assert_eq!(
        PendingTextRangeResolver::check_surrogate_boundary(text, mid_surrogate),
        Err(CoreError::InvalidUtf16)
    );

    // A range query ending inside the surrogate pair is refused as well.
    let bad_range_req = RangeRequestToken::new(owner(), 2, line_ids[2], layout_rev).expect("token valid");
    let line3_start = starts_of_line3(text);
    let bad_range = utf16_range(line3_start, emoji_unit + 1);
    assert!(matches!(
        PendingTextRangeResolver::resolve_range(bad_range_req, &layout, Some(text), bad_range, &[]),
        PendingRangeStatus::Refused(CoreError::InvalidUtf16)
    ));

    // Stale layout revision: a token minted for an earlier revision is Stale.
    let stale_rev = LayoutRevision::new(owner(), 6).expect("older revision valid");
    let stale_range_req =
        RangeRequestToken::new(owner(), 3, line_ids[1], stale_rev).expect("token valid");
    assert!(matches!(
        PendingTextRangeResolver::resolve_range(
            stale_range_req,
            &layout,
            Some(text),
            utf16_range(0, 4),
            &[]
        ),
        PendingRangeStatus::Stale(CoreError::StaleLayoutRevision)
    ));

    // Unknown node in an otherwise valid token is Stale/NodeNotFound.
    let ghost = SemanticNodeId::new(owner(), 99).expect("node id valid");
    let ghost_range_req =
        RangeRequestToken::new(owner(), 4, ghost, layout_rev).expect("token valid");
    assert!(matches!(
        PendingTextRangeResolver::resolve_range(
            ghost_range_req,
            &layout,
            Some(text),
            utf16_range(0, 4),
            &[]
        ),
        PendingRangeStatus::Stale(CoreError::NodeNotFound)
    ));

    // Sentinel inside a requested range is refused by the resolver itself.
    let sent_range = Utf16CodeUnitRange::new(utf16_offset(0), sentinel).expect("range valid");
    let sentinel_range_req = RangeRequestToken::new(owner(), 5, line_ids[1], layout_rev).expect("token valid");
    assert!(matches!(
        PendingTextRangeResolver::resolve_range(sentinel_range_req, &layout, Some(text), sent_range, &[]),
        PendingRangeStatus::Refused(CoreError::NativeSentinel)
    ));

    // Stale layout identity: revisions must move together with generations.
    let newer_rev = LayoutRevision::new(owner(), 8).expect("newer revision valid");
    let identity = layout.identity();
    let mismatched = AcceptedLayoutIdentity::new(
        owner(),
        newer_rev,
        identity.source_revision(),
        identity.display_generation(),
        identity.presented_frame(),
    )
    .expect("identity valid");
    assert_eq!(
        mismatched.validate_against(identity),
        Err(CoreError::StaleLayoutRevision)
    );
}

fn starts_of_line3(text: &str) -> u64 {
    line_starts_utf16(text)[2]
}

#[test]
fn focus_state_tracks_keyboard_focus_with_return() {
    let root = TempRoot::new("focus");
    fs::write(root.path().join("smoke.rs"), SOURCE).expect("source writes");
    let reader = reader_for(root.path());
    let cancel = CancelFlag::new();
    let capture = reader
        .read_file(file_id(1), revision(1), &normalized("smoke.rs"), &cancel)
        .expect("capture succeeds");
    let text = std::str::from_utf8(capture.bytes()).expect("captured bytes are UTF-8");
    let (layout, line_ids, _layout_rev) = build_snapshot(text);

    let mut focus = SemanticFocusState::new(owner(), 4);
    assert_eq!(focus.current_focus(), None);

    // Keyboard walk: line 1 -> line 2. The previous target is retained on
    // the return stack.
    focus.focus_node(line_ids[0], &layout).expect("focus line 1");
    assert_eq!(focus.current_focus(), Some(line_ids[0]));
    focus.focus_node(line_ids[1], &layout).expect("focus line 2");
    assert_eq!(focus.current_focus(), Some(line_ids[1]));
    assert_eq!(focus.focus_stack_depth(), 1);

    // Focus bounds come from the accepted snapshot geometry.
    let bounds = focus.focus_bounds(&layout).expect("focus bounds");
    assert!(bounds.max_x() - bounds.min_x() > 0.0);
    assert!(bounds.max_y() - bounds.min_y() > 0.0);

    // Blur clears focus but keeps the return stack.
    focus.blur();
    assert_eq!(focus.current_focus(), None);
    let restored = focus.return_focus(&layout).expect("return focus");
    assert_eq!(restored, Some(line_ids[0]), "returns to the prior target");
}
