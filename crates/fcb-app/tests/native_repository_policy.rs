#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real native-host discovery, filename finding and source search share the
//! existing repository-rule engine. These are source-authored regressions;
//! execution and physical-Mac qualification remain separate evidence.
mod support;

use std::{collections::BTreeSet, fs, os::unix::fs::symlink, path::PathBuf,
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, FileId};
use fcb_app::{EXIT_PARTIAL, host::{HostResponse,
    atlas::{self, LegacyAtlasOptions},
    atlas_session::{AtlasSession, AtlasSessionOptions},
    atlas_search::{AtlasSearchOptions, RetainedAtlasSearch},
    atlas_stream_search::{AtlasStreamSearchOptions, RetainedAtlasStreamSearch},
    catalog_session::CatalogSession,
    file_finder_session::{FileFinderOptions, FileFinderSession}}};
use support::{Json, parse};

const SOURCE: &[u8] = b"policy_source_needle\n";
struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-native-rules-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap(); Self(root)
    }
    fn write(&self, path: &str, bytes: &[u8]) {
        let path = self.0.join(path);
        fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
    }
    fn catalog(&self) -> CatalogSession { CatalogSession::open(owner(), &self.0, || false).unwrap() }
    fn atlas(&self) -> AtlasSession {
        AtlasSession::open(owner(), &self.0, AtlasSessionOptions::default(), || false).unwrap()
    }
    // Keep fixture directories for independent diagnosis; never clean user paths.
}
fn owner() -> ArenaOwnerId {
    static NEXT: AtomicU64 = AtomicU64::new(820_000);
    ArenaOwnerId::new(NEXT.fetch_add(1, Ordering::Relaxed)).unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn path(row: &Json) -> Vec<u8> {
    let hex = row.get("path").get("hex").text();
    (0..hex.len()).step_by(2).map(|at| u8::from_str_radix(&hex[at..at + 2], 16).unwrap()).collect()
}
fn drain(session: &mut CatalogSession) -> (BTreeSet<Vec<u8>>, Json) {
    let mut page = json(&session.page(1, || false).unwrap());
    let mut paths = BTreeSet::new();
    for _ in 0..1024 {
        assert_eq!(page.get("discovery_source_bytes_read").number(), 0);
        assert_eq!(page.get("discovery_source_read_calls").number(), 0);
        assert!(page.get("retained_pages").number() <= 4);
        for row in page.get("files").array() { assert!(paths.insert(path(row))); }
        if !page.get("more").flag() { return (paths, page); }
        page = json(&session.advance(page.get("page_id").number(), || false).unwrap());
    }
    panic!("bounded fixture did not finish");
}
fn find(fixture: &Fixture, needle: &[u8]) -> Json {
    let mut finder = FileFinderSession::open(owner(), &fixture.0, 1, needle, FileFinderOptions::default(), || false).unwrap();
    let mut page = json(&finder.info(|| false).unwrap());
    for _ in 0..1024 {
        if !page.get("more").flag() { return page; }
        page = json(&finder.advance(1, page.get("revision").number(), 16, || false).unwrap());
    }
    panic!("bounded finder fixture did not finish");
}

#[test]
fn native_browse_finder_atlas_and_both_searches_share_nested_rule_precedence() {
    let f = Fixture::new();
    let rules = [
        (".gitignore", b"hidden.rs\n*.tmp\nignored/\n".as_slice()),
        (".fcbignore", b"!hidden.rs\n".as_slice()),
        ("nested/.gitignore", b"*.rs\n".as_slice()),
        ("nested/.fcbignore", b"!keep.rs\n".as_slice()),
    ];
    for (name, bytes) in rules { f.write(name, bytes); }
    for name in ["visible.rs", "hidden.rs", "ignored.tmp", "ignored/visible.rs",
        "nested/keep.rs", "nested/omit.rs", ".git/private.rs", ".fcb-cache/private.rs"] { f.write(name, SOURCE); }
    let expected: BTreeSet<Vec<u8>> = [".gitignore", ".fcbignore", "visible.rs", "hidden.rs",
        "nested/.gitignore", "nested/.fcbignore", "nested/keep.rs"].map(|s| s.as_bytes().to_vec()).into();
    let mut browser = f.catalog(); let (paths, page) = drain(&mut browser);
    assert_eq!(paths, expected);
    assert_eq!(page.get("policy").text(), "repository-rules-v1");
    let evidence = page.get("rule_policy");
    assert!(evidence.get("observed_rules_complete").flag() && evidence.get("scope_complete").flag());
    assert_eq!(evidence.get("files_loaded").number(), 4);
    assert_eq!(evidence.get("configuration_bytes_read").number(), rules.iter().map(|(_, b)| b.len() as u64).sum::<u64>());
    assert!(page.get("discovery").get("excluded_entries").number() >= 5);
    let found = find(&f, b"visible.rs");
    assert!(found.get("search_complete").flag());
    assert_eq!(found.get("hits").array().len(), 1);
    assert_eq!(path(&found.get("hits").array()[0]), b"visible.rs");
    assert_eq!(found.get("discovery_source_bytes_read").number(), 0);
    assert_eq!(found.get("rule_policy").get("files_loaded").number(), 4);

    let atlas = f.atlas();
    let atlas_paths: BTreeSet<Vec<u8>> = atlas.atlas().catalog().entries().iter()
        .map(|entry| entry.path().as_bytes().to_vec()).collect();
    assert_eq!(atlas_paths, expected);
    assert!(atlas.atlas().catalog().policy_name().starts_with("repository-rules-v1:"));
    let mut captured = RetainedAtlasSearch::new(&atlas).unwrap();
    let report = json(&captured.search(&atlas, 1, "policy_source_needle", AtlasSearchOptions::default(), || false).unwrap());
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("matches_seen").number(), 3);
    assert!(report.get("rule_policy").get("scope_complete").flag());
    let mut streamed = RetainedAtlasStreamSearch::new(&atlas).unwrap();
    let report = json(&streamed.search(&atlas, "policy_source_needle", AtlasStreamSearchOptions::default(), || false).unwrap());
    assert!(report.get("search_complete").flag());
    assert_eq!(report.get("retained_hits").number(), 3);
    assert_eq!(report.get("rule_policy").get("files_loaded").number(), 4);

    // Both compatibility native paths also consume the same rule-aware catalog.
    for response in [atlas::prepare_catalog(&f.0, 32, || false).unwrap(),
        atlas::prepare(&f.0, LegacyAtlasOptions { max_profile_source_bytes: 0, ..Default::default() }, || false).unwrap()] {
        assert!(response.as_str().contains("\"rule_files_enabled\":true"));
        assert!(!response.as_str().contains(&hex(b"nested/omit.rs")));
        assert!(!response.as_str().contains(&hex(b"ignored/visible.rs")));
    }
}

#[test]
fn invalid_oversized_and_symlink_rules_withhold_only_affected_subtrees() {
    let f = Fixture::new(); f.write("visible.rs", SOURCE);
    f.write("invalid/.gitignore", b"deep//secret.rs\n"); f.write("invalid/private.rs", SOURCE);
    f.write("large/.gitignore", &vec![b'x'; 16_385]); f.write("large/private.rs", SOURCE);
    f.write("outside-rules", b"*.rs\n"); f.write("linked/private.rs", SOURCE);
    symlink(f.0.join("outside-rules"), f.0.join("linked/.gitignore")).unwrap();
    let mut browser = f.catalog(); let (paths, page) = drain(&mut browser);
    assert_eq!(paths, [b"visible.rs".to_vec(), b"outside-rules".to_vec()].into());
    assert_eq!(page.get("state").text(), "partial"); assert!(!page.get("more").flag());
    assert!(!page.get("discovery_complete").flag());
    let rules = page.get("rule_policy");
    assert!(!rules.get("observed_rules_complete").flag() && !rules.get("scope_complete").flag());
    assert_eq!(rules.get("failed_files").number(), 3);
    assert_eq!(rules.get("files_loaded").number(), 0);
    assert_eq!(rules.get("configuration_bytes_read").number(), 16);
    let codes: BTreeSet<&str> = rules.get("diagnostics").array().iter().map(|row| row.get("code").text()).collect();
    assert_eq!(codes, ["RULE_UNSUPPORTED_PATTERN", "RULE_BYTE_LIMIT", "RULE_SPECIAL_OBJECT"].into());
    // Evidence survives retiring the exhausted walker and replaying its page.
    assert_eq!(rules, json(&browser.info(|| false).unwrap()).get("rule_policy"));
    let found = find(&f, b"private.rs");
    assert!(found.get("hits").array().is_empty()); assert!(!found.get("search_complete").flag());
    assert!(!found.get("rule_policy").get("observed_rules_complete").flag());
    let atlas = f.atlas(); let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let response = search.search(&atlas, 1, "policy_source_needle", AtlasSearchOptions::default(), || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let report = json(&response); assert!(!report.get("search_complete").flag());
    assert_eq!(report.get("matches_seen").number(), 1);
    assert!(!report.get("rule_policy").get("scope_complete").flag());
}

#[test]
fn diagnostic_output_is_bounded_without_losing_failure_counts() {
    let f = Fixture::new(); f.write("visible.rs", SOURCE);
    for n in 0..40 { f.write(&format!("d{n:02}/.gitignore"), b"[bad"); f.write(&format!("d{n:02}/private.rs"), SOURCE); }
    let mut browser = f.catalog(); let (paths, page) = drain(&mut browser);
    assert_eq!(paths, [b"visible.rs".to_vec()].into());
    let rules = page.get("rule_policy");
    assert_eq!(rules.get("failed_files").number(), 40);
    assert_eq!(rules.get("diagnostics").array().len(), 8);
    assert_eq!(rules.get("diagnostics_omitted").number(), 32);
    assert_eq!(rules.get("configuration_bytes_read").number(), 160);
    assert!(!page.get("discovery_complete").flag());
}

#[test]
fn rule_edits_change_new_catalogs_without_rebinding_retained_source() {
    let f = Fixture::new(); f.write("visible.rs", SOURCE);
    let mut before = f.catalog(); let (_, page) = drain(&mut before);
    let row = &page.get("files").array()[0];
    let file = FileId::new(before.owner(), row.get("file_id").number()).unwrap();
    let (mut retained, _) = before.open_reader(owner(), page.get("page_id").number(), file, 4096, || false).unwrap();
    let first_atlas = f.atlas(); let first_policy = first_atlas.atlas().catalog().policy_name().to_owned();
    f.write(".gitignore", b"visible.rs\n"); f.write("visible.rs", b"new live bytes\n");
    let mut after = f.catalog(); let (paths, _) = drain(&mut after);
    assert!(!paths.contains(b"visible.rs".as_slice()));
    let second_atlas = f.atlas(); assert_ne!(second_atlas.atlas().catalog().policy_name(), first_policy);
    assert_eq!(json(&retained.copy_range(0, SOURCE.len() as u64, || false).unwrap()).get("original_hex").text(), hex(SOURCE));
    let old_page = json(&before.page(page.get("page_id").number(), || false).unwrap());
    assert_eq!(path(&old_page.get("files").array()[0]), b"visible.rs");
    assert_eq!(old_page.get("rule_policy").get("files_loaded").number(), 0);
    let same_membership_policy = second_atlas.atlas().catalog().policy_name().to_owned();
    f.write(".gitignore", b"# changed policy evidence\nvisible.rs\n");
    assert_ne!(f.atlas().atlas().catalog().policy_name(), same_membership_policy);
}
