#![forbid(unsafe_code)]
#![cfg(unix)]

//! Public host tests for explicit atlas file-type scopes: display filtering,
//! cached relayout, reliable All restoration and workspace-wide search lanes.

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, Point2D};
use fcb_app::EXIT_OK;
use fcb_app::host::atlas_session::{AtlasAction, AtlasExtensionScope, AtlasScope, AtlasSession,
    AtlasSessionError, AtlasSessionOptions};
use fcb_app::host::atlas_search::{AtlasSearchError, AtlasSearchOptions, RetainedAtlasSearch};

struct Fixture(PathBuf);
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-scope-{}-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files {
            let path = root.join(name);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, bytes).unwrap();
        }
        Self(root)
    }
    fn open(&self) -> AtlasSession {
        AtlasSession::open(owner(29601), &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn search_options() -> AtlasSearchOptions {
    AtlasSearchOptions { max_matches: 100, max_files: 100, max_file_bytes: 1024,
        max_source_bytes: 64 * 1024 }
}
fn fixture() -> Fixture {
    Fixture::new(&[("src/a.rs", b"needle a\n"), ("src/b.md", b"# needle b\n"),
        ("README.MD", b"plain\n"), ("Cargo.toml", b"[package]\n"),
        ("src/deep/main.rs", b"fn main() {}\n")])
}
fn rust_scope() -> AtlasScope {
    AtlasScope::Extensions(AtlasExtensionScope::from_extensions([b"rs".as_slice()]).unwrap())
}
fn custom_scope(tokens: &[&str]) -> AtlasScope {
    AtlasScope::Extensions(AtlasExtensionScope::from_extensions(
        tokens.iter().map(|token| token.as_bytes())).unwrap())
}
/// Extract a scalar wire field; integer values are canonical quoted strings.
fn field(record: &str, key: &str) -> String {
    let probe = format!("\"{key}\":");
    let position = record.find(&probe).expect(key) + probe.len();
    let rest = &record[position..];
    let end = rest.find(|c: char| c == ',' || c == '}').unwrap();
    rest[..end].trim_matches('"').to_string()
}
fn root_ordinal(atlas: &mut AtlasSession) -> u32 {
    let info = atlas.info(|| false).unwrap();
    let position = info.as_str().find("\"focus\":{\"ordinal\":").unwrap()
        + "\"focus\":{\"ordinal\":".len();
    info.as_str()[position..].split('"').nth(1).unwrap().parse().unwrap()
}
fn children_ordinals(atlas: &mut AtlasSession, parent: u32) -> Vec<(u32, String)> {
    let rows = atlas.children(parent, 0, 1024, || false).unwrap();
    let text = rows.as_str();
    let start = text.find("\"rows\":[").unwrap() + "\"rows\":[".len();
    let body = &text[start..text[start..].find("],\"next_offset\"").unwrap() + start];
    body.split("},{").map(|row| {
        let ordinal: u32 = field(&format!("{{\"x\":{row}}}"), "x").parse().unwrap_or(u32::MAX);
        let ordinal = row.find("\"ordinal\":").map(|p| {
            let rest = &row[p + "\"ordinal\":".len()..];
            rest.split('"').nth(1).unwrap().parse().unwrap()
        }).unwrap_or(ordinal);
        let path = row.find("\"path\":").map(|p| {
            let rest = &row[p + "\"path\":".len()..];
            rest.split("\",").next().unwrap().trim_matches('"').to_string()
        }).unwrap_or_default();
        (ordinal, path)
    }).collect()
}
fn parcel_field(plan: &str) -> String {
    let start = plan.find("\"parcels\":").expect("parcels") + "\"parcels\":".len();
    let end = plan[start..].find("],\"visited_nodes\"").expect("parcel end") + start;
    plan[start..end].to_string()
}

#[test]
fn scope_filters_display_and_restores_all_geometry() {
    let fixture = fixture(); let mut atlas = fixture.open();
    let all_plan = atlas.prepare(1, AtlasAction::View, || false).unwrap();
    assert_eq!(field(all_plan.as_str(), "catalogued_files"), "5");
    assert!(all_plan.as_str().contains("\"scope\":\"all\""));
    assert!(all_plan.as_str().contains("\"workspace_files\":\"5\""));

    let scoped = atlas.prepare(2, AtlasAction::Scope(rust_scope()), || false).unwrap();
    assert_eq!(field(scoped.as_str(), "catalogued_files"), "2");
    assert_eq!(field(scoped.as_str(), "workspace_files"), "5");
    assert!(scoped.as_str().contains("\"scope\":{\"kind\":\"extensions\",\"extensions\":[\"rs\"]}"));
    // Fresh layout identity for the repacked scope.
    assert_ne!(field(scoped.as_str(), "layout_revision"), field(all_plan.as_str(), "layout_revision"));

    // The tree listing follows the scope: the src ancestor stays (structure
    // intact, nested manifests included), non-Rust files disappear.
    let root = root_ordinal(&mut atlas);
    let root_rows = children_ordinals(&mut atlas, root);
    let paths: Vec<&String> = root_rows.iter().map(|(_, path)| path).collect();
    assert!(paths.contains(&&"src".to_string()));
    assert!(!paths.iter().any(|path| path.contains("b.md")));
    assert!(!paths.iter().any(|path| path.contains("Cargo.toml")));
    assert!(!paths.iter().any(|path| path.contains("README.MD")));
    let src_ordinal = root_rows.iter().find(|(_, path)| path == "src").unwrap().0;
    let src_rows = children_ordinals(&mut atlas, src_ordinal);
    let src_paths: Vec<&String> = src_rows.iter().map(|(_, path)| path).collect();
    assert!(src_paths.contains(&&"src/a.rs".to_string()));
    assert!(src_paths.contains(&&"src/deep".to_string()));
    assert!(!src_paths.iter().any(|path| path.contains("b.md")));

    // Restoring All reuses the retained base layout: original revision,
    // original file count, original parcel geometry.
    let restored = atlas.prepare(3, AtlasAction::Scope(AtlasScope::All), || false).unwrap();
    assert_eq!(field(restored.as_str(), "catalogued_files"), "5");
    assert_eq!(field(restored.as_str(), "layout_revision"), field(all_plan.as_str(), "layout_revision"));
    assert_eq!(parcel_field(restored.as_str()), parcel_field(all_plan.as_str()));
}

#[test]
fn alternation_reuses_cached_scope_geometry() {
    let fixture = fixture(); let mut atlas = fixture.open();
    let first_rust = atlas.prepare(2, AtlasAction::Scope(rust_scope()), || false).unwrap();
    let rust_revision = field(first_rust.as_str(), "layout_revision");
    atlas.prepare(3, AtlasAction::Scope(AtlasScope::All), || false).unwrap();
    // The scoped layout is retained while All is displayed: switching back is
    // a cache hit with identical geometry identity.
    let second_rust = atlas.prepare(4, AtlasAction::Scope(rust_scope()), || false).unwrap();
    assert_eq!(field(second_rust.as_str(), "layout_revision"), rust_revision);
    // A different scope mints a fresh revision.
    let markdown = atlas.prepare(5, AtlasAction::Scope(custom_scope(&["md"])), || false).unwrap();
    let md_revision = field(markdown.as_str(), "layout_revision");
    assert_ne!(md_revision, rust_revision);
    assert_eq!(field(markdown.as_str(), "catalogued_files"), "2"); // b.md + README.MD
    assert!(markdown.as_str().contains("\"extensions\":[\"md\"]"));
}

#[test]
fn scope_change_resets_navigation_identity() {
    let fixture = fixture(); let mut atlas = fixture.open();
    let root = root_ordinal(&mut atlas);
    let src_ordinal = children_ordinals(&mut atlas, root)
        .into_iter().find(|(_, path)| path == "src").unwrap().0;
    atlas.prepare(1, AtlasAction::Focus(src_ordinal), || false).unwrap();
    // Focus pushed exactly one history entry; a scope change invalidates the
    // previous layout's node identity, so Back must be refused, not aliased.
    atlas.prepare(2, AtlasAction::Scope(rust_scope()), || false).unwrap();
    assert!(matches!(atlas.prepare(3, AtlasAction::Back, || false),
        Err(AtlasSessionError::EmptyHistory)));
    // Returning to All keeps the reset semantics.
    atlas.prepare(4, AtlasAction::Scope(AtlasScope::All), || false).unwrap();
    assert!(matches!(atlas.prepare(5, AtlasAction::Back, || false),
        Err(AtlasSessionError::EmptyHistory)));
}

#[test]
fn search_rows_follow_scope_while_counts_stay_workspace_wide() {
    let fixture = fixture(); let mut atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let result = search.search(&atlas, 1, "needle", search_options(), || false).unwrap();
    assert_eq!(result.exit_code(), EXIT_OK);

    let unscoped = search.page(&atlas, 1, 0, 100, || false).unwrap();
    assert!(unscoped.as_str().contains("\"displayed_hits\":\"2\""));
    assert!(unscoped.as_str().contains("\"retained_hits\":\"2\""));

    // An explicit scope change must NOT retire the workspace-wide query and
    // must not re-read source: the retained results keep validating.
    atlas.prepare(2, AtlasAction::Scope(rust_scope()), || false).unwrap();
    let scoped = search.page(&atlas, 1, 0, 100, || false).unwrap();
    assert!(scoped.as_str().contains("\"displayed_hits\":\"1\""));
    assert!(scoped.as_str().contains("\"retained_hits\":\"2\""));
    assert!(scoped.as_str().contains("\"count_basis\":\"workspace-wide-retained-hits\""));
    assert!(scoped.as_str().contains("\"row_filter\":\"active-scope\""));
    assert!(scoped.as_str().contains("src/a.rs"));
    assert!(!scoped.as_str().contains("src/b.md"));

    let overlay = search.overlay(&atlas, 1, || false).unwrap();
    assert!(overlay.as_str().contains("\"displayed_files\":\"1\""));
    assert!(overlay.as_str().contains("\"count_basis\":\"workspace-wide-retained-hits\""));
    assert!(overlay.as_str().contains("\"matching_files\":\"2\""));

    // Restoring All shows every row again.
    atlas.prepare(3, AtlasAction::Scope(AtlasScope::All), || false).unwrap();
    let restored = search.page(&atlas, 1, 0, 100, || false).unwrap();
    assert!(restored.as_str().contains("\"displayed_hits\":\"2\""));
    assert!(restored.as_str().contains("src/b.md"));
}

#[test]
fn focus_resolves_hits_into_the_active_display_layout() {
    let fixture = fixture(); let mut atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search.search(&atlas, 1, "needle", search_options(), || false).unwrap();
    let page = search.page(&atlas, 1, 0, 100, || false).unwrap();
    let text = page.as_str();
    // Hits keep workspace-stable IDs in catalog raw-path order: src/a.rs then
    // src/b.md.
    let hit_id = |position: usize| -> u64 {
        let at = text.match_indices("\"hit_id\":").nth(position).unwrap().0
            + "\"hit_id\":".len();
        text[at..].split('"').nth(1).unwrap().parse().unwrap()
    };
    let (rust_id, md_id) = (hit_id(0), hit_id(1));
    assert_ne!(rust_id, md_id);

    atlas.prepare(2, AtlasAction::Scope(rust_scope()), || false).unwrap();
    // The in-scope hit focuses its node in the ACTIVE scoped layout.
    let focused = search.focus_hit(&mut atlas, 1, rust_id, 3, || false).unwrap();
    assert!(focused.as_str().contains("src/a.rs"));
    // The out-of-scope hit has no displayed node and is refused.
    assert!(matches!(search.focus_hit(&mut atlas, 1, md_id, 4, || false),
        Err(AtlasSearchError::OutOfScope)));
}

#[test]
fn uppercase_extensions_and_custom_scopes_filter_display() {
    let fixture = fixture(); let mut atlas = fixture.open();
    // README.MD folds to md: Markdown scope displays both Markdown files.
    let markdown = atlas.prepare(1, AtlasAction::Scope(custom_scope(&["md"])), || false).unwrap();
    assert_eq!(field(markdown.as_str(), "catalogued_files"), "2");
    // A custom multi-extension scope unions the token set.
    let custom = atlas.prepare(2, AtlasAction::Scope(custom_scope(&["toml", "md"])), || false).unwrap();
    assert_eq!(field(custom.as_str(), "catalogued_files"), "3");
    assert!(custom.as_str().contains("\"extensions\":[\"md\",\"toml\"]"));
    // An empty-result scope is a valid, explicitly empty display.
    let empty = atlas.prepare(3, AtlasAction::Scope(custom_scope(&["zzz"])), || false).unwrap();
    assert_eq!(field(empty.as_str(), "catalogued_files"), "0");
    let restored = atlas.prepare(4, AtlasAction::Scope(AtlasScope::All), || false).unwrap();
    assert_eq!(field(restored.as_str(), "catalogued_files"), "5");
}

#[test]
fn same_scope_reapplication_is_a_cache_hit_not_a_repack() {
    let fixture = fixture(); let mut atlas = fixture.open();
    let first = atlas.prepare(1, AtlasAction::Scope(rust_scope()), || false).unwrap();
    let revision = field(first.as_str(), "layout_revision");
    // Re-applying the ACTIVE scope performs no relayout: same revision.
    let again = atlas.prepare(2, AtlasAction::Scope(rust_scope()), || false).unwrap();
    assert_eq!(field(again.as_str(), "layout_revision"), revision);
}


/// Keep both navigation directions and an unpresented candidate alive before
/// attempting another scope. The visible file exists in both starting scopes.
fn ready_scope(fixture: &Fixture, scope: AtlasScope) -> (AtlasSession, Point2D) {
    let mut atlas = fixture.open();
    atlas.prepare(1, AtlasAction::Scope(scope), || false).unwrap();
    let a = atlas.view().index().unwrap().find_path(b"src/a.rs").unwrap().ordinal();
    let b = atlas.view().index().unwrap().find_path(b"src/deep/main.rs").unwrap().ordinal();
    atlas.prepare(2, AtlasAction::Focus(a), || false).unwrap();
    atlas.prepare(3, AtlasAction::Focus(b), || false).unwrap();
    atlas.prepare(4, AtlasAction::Back, || false).unwrap();
    let rect = atlas.pending_plan().unwrap().parcels()[0].logical_rect();
    let at = Point2D::new(rect.min_x() + rect.size().width() / 2.0,
        rect.min_y() + rect.size().height() / 2.0).unwrap();
    atlas.acknowledge(4, 1, 1, || false).unwrap();
    atlas.prepare(5, AtlasAction::View, || false).unwrap();
    (atlas, at)
}

#[test]
fn cancellation_at_every_scope_checkpoint_preserves_navigation_and_presented_source() {
    let fixture = fixture();
    // Fresh construction, the cached All layout, and replacing the single
    // alternative cache all obey the same publication boundary.
    for (start, target) in [
        (AtlasScope::All, rust_scope()),
        (rust_scope(), AtlasScope::All),
        (rust_scope(), custom_scope(&["md"])),
    ] {
        let (mut probe, _) = ready_scope(&fixture, start);
        let mut checkpoints = 0;
        probe.prepare(6, AtlasAction::Scope(target), || {
            checkpoints += 1;
            false
        }).unwrap();
        assert!(checkpoints >= 3);
        drop(probe);
        for cancel_at in 1..=checkpoints {
            let (mut atlas, at) = ready_scope(&fixture, start);
            let old_layout = atlas.view().layout().nodes().as_ptr();
            let old_camera = atlas.pending_plan().unwrap().camera();
            let old_focus = atlas.pending_plan().unwrap().focus();
            let before = atlas.info(|| false).unwrap();
            let old_pick = atlas.pick(1, 1, at, || false).unwrap();
            let mut polls = 0;
            let failed = atlas.prepare(6, AtlasAction::Scope(target), || {
                polls += 1;
                polls >= cancel_at
            });
            assert!(failed.is_err(), "checkpoint {cancel_at} of {checkpoints}");
            assert!(polls >= cancel_at);
            assert_eq!(*atlas.scope(), start);
            assert_eq!(atlas.view().layout().nodes().as_ptr(), old_layout);
            assert_eq!(atlas.pending_plan().unwrap().generation().get(), 5);
            assert_eq!(atlas.pending_plan().unwrap().camera(), old_camera);
            assert_eq!(atlas.pending_plan().unwrap().focus(), old_focus);
            assert_eq!(atlas.presented_plan().unwrap().generation().get(), 4);
            assert_eq!(atlas.pick(1, 1, at, || false).unwrap().as_str(), old_pick.as_str());
            let after = atlas.info(|| false).unwrap();
            for key in ["layout_revision", "history_depth", "forward_history_depth"] {
                assert_eq!(field(after.as_str(), key), field(before.as_str(), key), "{key}");
            }
            assert!(matches!(atlas.prepare(6, AtlasAction::View, || false),
                Err(AtlasSessionError::StaleGeneration)));
            // A failed scope change cannot discard either endpoint stack.
            atlas.prepare(7, AtlasAction::Forward, || false).unwrap();
            atlas.prepare(8, AtlasAction::Back, || false).unwrap();
            assert_eq!(atlas.pending_plan().unwrap().focus(), old_focus);
            assert_eq!(atlas.pending_plan().unwrap().camera().origin(), old_camera.origin());
        }
    }
}

#[test]
fn displayed_scope_keeps_its_file_bindings_when_the_alternative_cache_is_replaced() {
    let fixture = fixture();
    let (mut atlas, at) = ready_scope(&fixture, rust_scope());
    let before = atlas.pick(1, 1, at, || false).unwrap();
    let displayed_revision = field(before.as_str(), "layout_revision");
    assert!(before.as_str().contains("src/a.rs"));

    atlas.prepare(6, AtlasAction::Scope(custom_scope(&["md"])), || false).unwrap();
    atlas.prepare(7, AtlasAction::Scope(custom_scope(&["toml"])), || false).unwrap();
    assert_ne!(atlas.view().layout().revision().get().to_string(), displayed_revision);
    assert_eq!(atlas.pick(1, 1, at, || false).unwrap().as_str(), before.as_str());

    let (reader, receipt) = atlas.open_reader(owner(29602), 1, 1, at, 1024, || false).unwrap();
    assert_eq!(reader.capture().bytes(), b"needle a\n");
    assert_eq!(field(receipt.as_str(), "layout_revision"), displayed_revision);
    assert!(receipt.as_str().contains("\"extensions\":[\"rs\"]"));
    assert!(receipt.as_str().contains("src/a.rs"));
    // Opening a displayed file that is absent from the prepared scope must
    // not assign that old ordinal to an unrelated file in the new layout.
    assert!(atlas.info(|| false).unwrap().as_str().contains("\"selection\":null"));

    let acknowledged = atlas.acknowledge(4, 2, 1, || false).unwrap();
    assert_eq!(field(acknowledged.as_str(), "layout_revision"), displayed_revision);
    assert!(acknowledged.as_str().contains("\"extensions\":[\"rs\"]"));
    assert_eq!(atlas.pending_plan().unwrap().generation().get(), 7);
    assert!(atlas.pick(2, 1, at, || false).unwrap().as_str().contains("src/a.rs"));

    atlas.acknowledge(7, 3, 1, || false).unwrap();
    assert!(matches!(atlas.pick(2, 1, at, || false), Err(AtlasSessionError::StaleFrame)));
    assert_eq!(atlas.presented_plan().unwrap().generation().get(), 7);
}
