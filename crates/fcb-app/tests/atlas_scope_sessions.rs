#![forbid(unsafe_code)]
#![cfg(unix)]

//! Public host tests for explicit atlas file-type scopes: display filtering,
//! cached relayout, reliable All restoration and workspace-wide search lanes.

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb::ArenaOwnerId;
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
        for (name, bytes) in files { fs::write(root.join(name), bytes).unwrap(); }
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
