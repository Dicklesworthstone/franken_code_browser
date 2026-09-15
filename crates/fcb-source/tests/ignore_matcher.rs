#![forbid(unsafe_code)]

//! First-party ignore matching (FCB-010.B / fcb-hh2.2).
//!
//! Verifies:
//! 1. Product defaults exclude source-control DBs, caches, and binary payloads.
//! 2. Anchored, directory-only, doublestar, negation, escaping, nested layers.
//! 3. Unsupported patterns are reported, never guessed.
//! 4. Browse overrides re-include default-policy paths without deleting them.
//! 5. Planted negative: treating exclusion as a missing/tombstoned path fails.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, RootId};
use fcb_source::confined::SymlinkPolicy;
use fcb_source::path::{NormalizedPath, RawPath};
use fcb_source::root::RootGrant;
use fcb_source::{
    BoundedDiscovery, CancelFlag, DiscoveryKind, DiscoveryLimits, ExclusionCause, IgnoreDecision,
    IgnoreMatcher, SourceError, UnsupportedReason,
};

fn np(rel: &str) -> NormalizedPath {
    NormalizedPath::new(RawPath::from_str(rel)).expect("test path")
}

#[test]
fn product_defaults_exclude_git_target_and_object_files() {
    let mut matcher = IgnoreMatcher::product_defaults();
    assert_eq!(
        matcher.decide(&np(".git"), true),
        IgnoreDecision::Exclude {
            cause: ExclusionCause::DefaultPolicy
        }
    );
    assert_eq!(
        matcher.decide(&np("target"), true),
        IgnoreDecision::Exclude {
            cause: ExclusionCause::DefaultPolicy
        }
    );
    assert_eq!(
        matcher.decide(&np("src/lib.rs"), false),
        IgnoreDecision::Include
    );
    assert!(matcher.decide(&np("src/foo.o"), false).is_excluded());
    assert!(!matcher.decide(&np("src/foo.rs"), false).is_excluded());
}

#[test]
fn directory_only_pattern_does_not_exclude_a_file_of_the_same_name() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_scope_rule("build/");
    assert!(matcher.decide(&np("build"), true).is_excluded());
    assert!(!matcher.decide(&np("build"), false).is_excluded());
}

#[test]
fn last_matching_pattern_wins_including_negation() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_rule_file(None, "*.log\n!keep.log\n");
    assert!(matcher.decide(&np("debug.log"), false).is_excluded());
    assert_eq!(
        matcher.decide(&np("keep.log"), false),
        IgnoreDecision::Include
    );
}

#[test]
fn leading_slash_is_anchored_to_the_rule_file_directory() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_rule_file(Some(&np("src")), "/generated.rs\n");
    assert!(matcher.decide(&np("src/generated.rs"), false).is_excluded());
    assert!(!matcher
        .decide(&np("src/nested/generated.rs"), false)
        .is_excluded());
    assert!(!matcher.decide(&np("generated.rs"), false).is_excluded());
}

#[test]
fn doublestar_matches_across_directories() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_scope_rule("**/tmp/*.txt");
    assert!(matcher.decide(&np("tmp/a.txt"), false).is_excluded());
    assert!(matcher.decide(&np("src/tmp/a.txt"), false).is_excluded());
    assert!(!matcher.decide(&np("src/tmp/a.rs"), false).is_excluded());
}

#[test]
fn escaped_asterisk_is_literal_not_a_glob() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_scope_rule(r"\*.rs");
    assert!(matcher.decide(&np("*.rs"), false).is_excluded());
    assert!(!matcher.decide(&np("lib.rs"), false).is_excluded());
}

#[test]
fn nested_rule_file_is_relative_to_its_directory() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_rule_file(Some(&np("vendor")), "*.c\n");
    assert!(matcher.decide(&np("vendor/foo.c"), false).is_excluded());
    assert!(!matcher.decide(&np("src/foo.c"), false).is_excluded());
}

#[test]
fn unsupported_patterns_are_reported_not_guessed() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_scope_rule("[unclosed");
    matcher.add_scope_rule("trailing\\");
    matcher.add_scope_rule("!");
    assert_eq!(matcher.unsupported().len(), 3);
    assert_eq!(
        matcher.unsupported()[0].reason(),
        UnsupportedReason::UnclosedClass
    );
    assert_eq!(
        matcher.unsupported()[1].reason(),
        UnsupportedReason::DanglingEscape
    );
    assert_eq!(
        matcher.unsupported()[2].reason(),
        UnsupportedReason::EmptyPattern
    );
    assert!(!matcher.decide(&np("unclosed"), false).is_excluded());
}

#[test]
fn comments_and_blank_lines_are_ignored() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_rule_file(None, "# ignore this\n\n  \n*.tmp\n");
    assert!(matcher.decide(&np("foo.tmp"), false).is_excluded());
    assert!(matcher.unsupported().is_empty());
}

#[test]
fn character_class_and_negated_class_match() {
    let mut matcher = IgnoreMatcher::include_all();
    matcher.add_scope_rule("foo.[ch]");
    matcher.add_scope_rule("bar.[!a]");
    assert!(matcher.decide(&np("foo.c"), false).is_excluded());
    assert!(matcher.decide(&np("foo.h"), false).is_excluded());
    assert!(!matcher.decide(&np("foo.rs"), false).is_excluded());
    assert!(matcher.decide(&np("bar.b"), false).is_excluded());
    assert!(!matcher.decide(&np("bar.a"), false).is_excluded());
}

#[test]
fn browse_override_reincludes_default_policy_without_deleting_the_path() {
    let mut matcher = IgnoreMatcher::product_defaults();
    let node_modules = np("node_modules");
    assert!(matcher.peek(&node_modules, true).is_excluded());
    matcher.override_browse(&node_modules);
    assert_eq!(
        matcher.decide(&node_modules, true),
        IgnoreDecision::Include,
        "override must re-include the excluded directory rather than tombstone it"
    );
    assert_eq!(
        matcher.decide(&np("node_modules/left-pad/index.js"), false),
        IgnoreDecision::Include
    );
    assert!(
        matcher.decide(&np("target"), true).is_excluded(),
        "override is prefix-scoped, not a global disable"
    );
}

#[test]
fn exclusion_is_classification_not_deletion() {
    let mut matcher = IgnoreMatcher::product_defaults();
    let git = np(".git");
    let decision = matcher.decide(&git, true);
    assert!(decision.is_excluded());
    assert_eq!(git.as_str().unwrap(), ".git");
    assert!(
        matcher.excluded_count() >= 1,
        "excluded count is a classification tally, not a removed-path count"
    );
}

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(prefix: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "fcb_ign_{}_{}_{}",
            prefix,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&path).expect("temp dir");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn test_root_id(owner: u64, root: u64) -> RootId {
    RootId::new(ArenaOwnerId::new(owner).unwrap(), root).unwrap()
}

fn write_file(dir: &Path, rel: &str, bytes: &[u8]) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = File::create(&path).unwrap();
    file.write_all(bytes).unwrap();
}

fn modest_limits() -> DiscoveryLimits {
    DiscoveryLimits::new(4, 16, 256, 64, 4096, 64).unwrap()
}

fn drain(
    discovery: &mut BoundedDiscovery,
) -> Result<Vec<(String, DiscoveryKind, bool)>, SourceError> {
    let cancel = CancelFlag::new();
    let mut out = Vec::new();
    while let Some(batch) = discovery.next_batch(&cancel)? {
        for entry in batch.entries() {
            out.push((
                entry.path().as_str().unwrap().to_string(),
                entry.kind(),
                entry.is_excluded(),
            ));
        }
        if !batch.more() {
            break;
        }
    }
    Ok(out)
}

#[test]
fn discovery_still_publishes_excluded_entries_and_does_not_descend() {
    let dir = TempTestDir::new("no_descend");
    write_file(dir.path(), "src/main.rs", b"fn main() {}");
    write_file(dir.path(), ".git/HEAD", b"ref: refs/heads/main");
    write_file(dir.path(), ".git/objects/pack/x", b"blob");
    write_file(dir.path(), "target/debug/app", b"elf");

    let grant = RootGrant::new(test_root_id(11, 1), dir.path());
    let mut discovery =
        BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, modest_limits()).unwrap();
    let entries = drain(&mut discovery).unwrap();

    assert!(
        entries
            .iter()
            .any(|(path, kind, excluded)| path == ".git"
                && *kind == DiscoveryKind::Directory
                && *excluded),
        "excluded .git must still be published"
    );
    assert!(
        entries.iter().all(|(path, _, _)| !path.starts_with(".git/")),
        "excluded directories must not be descended"
    );
    assert!(
        entries
            .iter()
            .any(|(path, _, excluded)| path == "src/main.rs" && !*excluded)
    );
    assert!(discovery.aggregate().excluded >= 1);
    assert!(
        !entries.iter().any(|(path, _, _)| path.contains("objects")),
        "planted negative: a matcher that deleted .git would also hide this, but descent is the contract"
    );
}

#[test]
fn nested_gitignore_and_browse_override_coordinate_at_the_discovery_seam() {
    let dir = TempTestDir::new("nested_override");
    write_file(dir.path(), "src/lib.rs", b"lib");
    write_file(dir.path(), "src/.gitignore", b"generated.rs\n");
    write_file(dir.path(), "src/generated.rs", b"gen");
    write_file(dir.path(), "node_modules/pkg/index.js", b"js");

    let grant = RootGrant::new(test_root_id(12, 1), dir.path());
    let mut matcher = IgnoreMatcher::product_defaults();
    matcher.override_browse(&np("node_modules"));
    let mut discovery = BoundedDiscovery::open_with_ignore(
        grant,
        SymlinkPolicy::DisallowAll,
        modest_limits(),
        matcher,
    )
    .unwrap();
    let entries = drain(&mut discovery).unwrap();

    let generated = entries
        .iter()
        .find(|(path, _, _)| path == "src/generated.rs")
        .expect("generated.rs is observed, not deleted");
    assert!(generated.2, "nested gitignore excludes generated.rs");

    assert!(
        entries
            .iter()
            .any(|(path, _, excluded)| path == "node_modules" && !*excluded),
        "browse override re-includes node_modules"
    );
    assert!(
        entries
            .iter()
            .any(|(path, _, excluded)| path == "node_modules/pkg/index.js" && !*excluded)
    );
}

#[test]
fn include_all_does_not_apply_product_defaults() {
    let dir = TempTestDir::new("include_all");
    write_file(dir.path(), "target/foo.rs", b"x");
    let grant = RootGrant::new(test_root_id(13, 1), dir.path());
    let mut discovery = BoundedDiscovery::open_with_ignore(
        grant,
        SymlinkPolicy::DisallowAll,
        modest_limits(),
        IgnoreMatcher::include_all(),
    )
    .unwrap();
    let entries = drain(&mut discovery).unwrap();
    assert!(
        entries
            .iter()
            .any(|(path, _, excluded)| path == "target/foo.rs" && !*excluded)
    );
}
