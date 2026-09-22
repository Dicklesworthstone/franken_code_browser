#![forbid(unsafe_code)]
#![cfg(unix)]

use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb_app::{host::{self, atlas::{self, HostAtlasError, LegacyAtlasOptions}, HostError}, AppError};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-native-atlas-{}-{now}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn field(object: &str, key: &str) -> f64 {
    object.split_once(&format!("\"{key}\":" )).unwrap().1.split([',', '}']).next().unwrap().parse().unwrap()
}
fn file_record<'a>(json: &'a str, path: &str) -> &'a str {
    let at = json.find(&format!("{{\"path\":\"{path}\"" )).unwrap();
    let tail = &json[at..]; &tail[..=tail.find('}').unwrap()]
}

#[test]
fn native_world_rectangles_are_exactly_the_shared_atlas_not_parent_local_or_rounded() {
    use fcb::{ArenaOwnerId, ByteLength, FileId, Size2D};
    use fcb::map::{LayoutOptions, LayoutRevision, ResourceAllocationId, ResourceBudget, RootId};
    use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasLimits};
    use fcb::search::{RawPath, SearchManifestId};
    use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};
    use fcb::source::CancelFlag;
    let root = root(); fs::create_dir_all(root.join("src/deep")).unwrap();
    for (name, content) in [("src/deep/a.rs", "aaaa"), ("src/deep/b.rs", "bb"), ("top.rs", "c")] {
        fs::write(root.join(name), content).unwrap();
    }
    let actual = atlas::prepare(&root, LegacyAtlasOptions { max_profile_source_bytes: 0, ..Default::default() }, || false).unwrap();
    let owner = ArenaOwnerId::new(1).unwrap();
    let allocation = |n| ResourceAllocationId::new(n).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    let grant = RootGrant::new(RootId::new(owner, 1).unwrap(), RawPath::from_path(&root));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner, 1).unwrap(), FileId::new(owner, 1).unwrap(),
        WorkspaceLimits::default(), false, &budget, allocation(1)).unwrap();
    while catalog.stage() == WorkspaceStage::Discovering { catalog.step(&CancelFlag::new()).unwrap(); }
    let retained = WorkspaceAtlas::build(&catalog, &fcb::map::workspace::AtlasScope::All, LayoutRevision::new(owner, 1).unwrap(), Size2D::new(4096.0, 4096.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), &budget, allocation(2), || false).unwrap();
    let index = retained.index(&budget, allocation(3), || false).unwrap();
    for name in ["src/deep/a.rs", "src/deep/b.rs", "top.rs"] {
        let expected = index.bounds_in(index.find_path(name.as_bytes()).unwrap(), index.root_node()).unwrap();
        let record = file_record(actual.as_str(), name);
        assert_eq!(field(record, "x"), expected.min_x()); assert_eq!(field(record, "y"), expected.min_y());
        assert_eq!(field(record, "w"), expected.size().width()); assert_eq!(field(record, "h"), expected.size().height());
        assert!(field(record, "w") > 0.0 && field(record, "h") > 0.0);
    }
    assert!(actual.as_str().contains("\"payload_bytes_read\":\"0\""));
}
#[test]
fn static_exclusions_are_shared_and_no_repository_rule_read_is_implicit() {
    let root = root(); fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/hidden.rs"), b"not admitted").unwrap();
    fs::write(root.join("visible.rs"), b"one\r\ntwo\rthree\n").unwrap();
    fs::write(root.join(".gitignore"), b"visible.rs\n").unwrap();
    let result = atlas::prepare(&root, LegacyAtlasOptions::default(), || false).unwrap();
    assert!(!result.as_str().contains("hidden.rs"));
    let file = file_record(result.as_str(), "visible.rs");
    assert_eq!(field(file, "n"), 3.0); assert!(file.contains("\"source_lines\":\"3\""));
    assert!(file.contains("\"profile_state\":\"complete\""));
    assert!(result.as_str().contains("neutral-class-not-syntax"));
}
#[test]
fn global_profile_byte_limit_counts_across_files_instead_of_restarting_per_file() {
    let root = root();
    for name in ["a", "b", "c"] { fs::write(root.join(name), b"abcd").unwrap(); }
    let result = atlas::prepare(&root, LegacyAtlasOptions { max_profile_source_bytes: 8, ..Default::default() }, || false).unwrap();
    assert_eq!(result.exit_code(), fcb_app::EXIT_PARTIAL);
    assert!(result.as_str().contains("\"payload_bytes_read\":\"8\""));
    assert!(file_record(result.as_str(), "c").contains("\"profile_state\":\"global-byte-limit\""));
    assert!(file_record(result.as_str(), "c").contains("\"source_lines\":null"));
    assert_eq!(field(file_record(result.as_str(), "c"), "n"), 0.0);
}
#[test]
fn giant_files_keep_parcels_without_unbounded_profile_reads_or_fake_empty_counts() {
    let root = root(); fs::write(root.join("small"), b"abcd").unwrap();
    fs::File::create(root.join("huge")).unwrap().set_len(1 << 34).unwrap();
    let result = atlas::prepare(&root, LegacyAtlasOptions::default(), || false).unwrap();
    assert!(result.as_str().contains("\"payload_bytes_read\":\"4\""));
    let huge = file_record(result.as_str(), "huge");
    assert!(huge.contains("\"profile_state\":\"file-byte-limit\""));
    assert!(huge.contains("\"source_lines\":null"));
    assert!(field(huge, "w") > 0.0 && field(huge, "h") > 0.0);
}
#[test]
fn truncated_profile_rows_remain_distinct_from_the_exact_source_line_total() {
    let root = root(); fs::write(root.join("three"), b"a\nbb\r\nccc").unwrap();
    let result = atlas::prepare(&root, LegacyAtlasOptions { max_profile_rows: 1, ..Default::default() }, || false).unwrap();
    let record = file_record(result.as_str(), "three");
    assert_eq!(field(record, "n"), 1.0);
    assert!(record.contains("\"tex\":\"VQA=\""));
    assert!(record.contains("\"source_lines\":\"3\""));
    assert!(record.contains("\"profile_state\":\"row-limit\""));
}
#[test]
fn complete_empty_roots_are_not_refusals_and_incomplete_trees_are_not_legacy_maps() {
    let root = root();
    let empty = atlas::prepare(&root, LegacyAtlasOptions::default(), || false).unwrap();
    assert_eq!(empty.exit_code(), 0); assert!(empty.as_str().contains("\"files\":[]"));
    fs::write(root.join("a"), b"a").unwrap(); fs::write(root.join("b"), b"b").unwrap();
    assert!(matches!(atlas::prepare(&root, LegacyAtlasOptions { max_files: 1, ..Default::default() }, || false),
        Err(HostAtlasError::IncompleteDiscovery)));
}
#[test]
#[cfg(target_os = "linux")]
fn native_byte_paths_are_refused_only_by_legacy_schema_not_lossily_aliased() {
    use std::os::unix::ffi::OsStringExt;
    let root = root(); fs::write(root.join(std::ffi::OsString::from_vec(b"raw-\xff".to_vec())), b"text").unwrap();
    assert!(matches!(atlas::prepare(&root, LegacyAtlasOptions::default(), || false), Err(HostAtlasError::NonUtf8Path)));
    let plan = host::atlas_plan(&root, || false).unwrap();
    assert!(plan.as_str().contains("7261772dff")); assert!(!plan.as_str().contains('\u{fffd}'));
}
#[test]
fn invalid_or_canceled_atlas_requests_do_not_widen_scope() {
    let root = root(); let linked = root.join("link"); std::os::unix::fs::symlink(&root, &linked).unwrap();
    assert!(matches!(atlas::prepare(&linked, LegacyAtlasOptions::default(), || false),
        Err(HostAtlasError::Host(HostError::App(AppError::Symlink)))));
    assert!(matches!(atlas::prepare(&root.join("absent"), LegacyAtlasOptions::default(), || true),
        Err(HostAtlasError::Host(HostError::App(AppError::Canceled)))));
    assert!(matches!(atlas::prepare(&root, LegacyAtlasOptions { max_profile_rows: 4001, ..Default::default() }, || false),
        Err(HostAtlasError::InvalidLimits)));
}
#[test]
fn native_markdown_services_use_real_flow_and_heading_identity_with_safe_errors() {
    let file = root().join("README.md"); fs::write(&file, "# Start\n\nalpha\n\n## Install\n\n**beta**\n").unwrap();
    let window = host::markdown_window(&file, 1, 40, 100, || false).unwrap();
    assert!(window.as_str().contains("fcb.document/1")); assert!(window.as_str().contains("beta"));
    let heading = host::markdown_heading(&file, "install", 40, 100, || false).unwrap();
    assert!(heading.as_str().contains("beta")); assert_ne!(heading.exit_code(), fcb_app::EXIT_ERROR);
    let missing = host::markdown_heading(&file, "--json", 40, 100, || false).unwrap();
    assert_eq!(missing.exit_code(), fcb_app::EXIT_ERROR);
    assert!(missing.as_str().contains("\"status\":\"error\""));
}
