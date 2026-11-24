//! Exact-member metadata uses the real discovery, retained layout and encoder.
//! Keep fixture inputs on disk for diagnosis by the independent verifier.
use super::*;
use std::{ffi::OsString, fs, os::unix::{ffi::OsStringExt, fs::symlink},
    path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-file-catalog-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn file(root: &Path, name: impl AsRef<Path>, bytes: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
}
fn hex(raw: &[u8]) -> String { raw.iter().map(|byte| format!("{byte:02x}")).collect() }
fn receipt(text: &str, selected: &[u8]) {
    assert!(text.starts_with("{\"schema\":\"fcb.project-file-catalog/1\",\"status\":\"ok\",\"command\":\"file-catalog\",\"file\":"), "{text}");
    assert!(text.contains(&format!("\"file\":{{\"encoding\":\"unix-bytes\",\"hex\":\"{}\"", hex(selected))), "{text}");
    assert!(text.contains("\"path_basis\":\"project-root\""), "{text}");
    assert!(text.contains("\"identity_scope\":\"response-local\",\"native_presented\":false,\"source_payload_read\":false,\"payload_bytes_read\":\"0\",\"read_calls\":\"0\""), "{text}");
    assert!(!text.contains("\"directory\":"), "{text}");
}

#[test]
fn an_exact_file_omitted_from_root_and_parent_catalogs_gets_its_own_atlas() {
    let root = root();
    let candidates: Vec<_> = (0..8).map(|n| format!("crowded/source-{n}.rs")).collect();
    for candidate in &candidates { file(&root, candidate, b"SOURCE_PAYLOAD_MUST_NOT_APPEAR"); }
    let project = prepare(&root, 1, || false).unwrap();
    let parent = prepare_directory(&root, b"crowded", 1,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    for initial in [&project, &parent] {
        assert_eq!(initial.exit_code(), crate::EXIT_PARTIAL);
        assert!(initial.as_str().contains("\"catalogued_files\":\"1\""));
        assert!(initial.as_str().contains("\"discovery_complete\":false"));
    }
    // Select a genuinely omitted member; filesystem enumeration order is not
    // part of the contract, and the two bounded catalogs may differ in order.
    let selected = candidates.iter().find(|path| {
        let marker = format!("\"display\":\"{path}\"");
        !project.as_str().contains(&marker) && !parent.as_str().contains(&marker)
    }).unwrap();
    let focused = prepare_file(&root, selected.as_bytes(), 1,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let text = focused.as_str();
    receipt(text, selected.as_bytes());
    assert_eq!(focused.exit_code(), crate::EXIT_OK);
    assert!(text.contains("\"catalogued_files\":\"1\""));
    assert!(text.contains("\"discovery_complete\":true"));
    assert_eq!(text.matches("\"file_id\":").count(), 1);
    assert_eq!(text.matches(&format!("\"hex\":\"{}\"", hex(selected.as_bytes()))).count(), 2);
    assert!(!text.contains("SOURCE_PAYLOAD_MUST_NOT_APPEAR"));
    for other in candidates.iter().filter(|path| *path != selected) {
        assert!(!text.contains(&format!("\"display\":\"{other}\"")));
    }
    // The additive route cannot relabel a root or directory receipt.
    assert!(project.as_str().starts_with("{\"schema\":\"fcb.project-catalog/1\""));
    assert!(parent.as_str().starts_with("{\"schema\":\"fcb.project-directory-catalog/1\""));
}

#[test]
fn an_excluded_leaf_is_complete_only_within_its_exact_file_scope() {
    let root = root();
    file(&root, ".gitignore", b"*.rs\n");
    file(&root, "selected.rs", b"DO_NOT_READ_SOURCE_PAYLOAD");
    let excluded = prepare_file(&root, b"selected.rs", 1,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    receipt(excluded.as_str(), b"selected.rs");
    assert_eq!(excluded.exit_code(), crate::EXIT_OK);
    assert!(excluded.as_str().contains("\"discovery_complete\":true"));
    assert!(excluded.as_str().contains("\"catalogued_files\":\"0\""));
    assert!(excluded.as_str().contains("\"files\":[]"));
    assert!(excluded.as_str().contains("\"rule_files_enabled\":true"));
    let included = prepare_file(&root, b"selected.rs", 1,
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    receipt(included.as_str(), b"selected.rs");
    assert_eq!(included.exit_code(), crate::EXIT_OK);
    assert!(included.as_str().contains("\"catalogued_files\":\"1\""));
    assert!(included.as_str().contains("\"policy\":\"all-regular-files/no-rule-files-v1\""));
    assert!(included.as_str().contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
    assert!(!included.as_str().contains("DO_NOT_READ_SOURCE_PAYLOAD"));
}

#[test]
fn excluded_or_unavailable_ancestor_policy_stays_partial_until_explicitly_included() {
    for rule in [b"ignored/\n".as_slice(), b"[unsupported\n"] {
        let root = root();
        file(&root, ".gitignore", rule);
        file(&root, "ignored/.gitignore", b"!selected.rs\n");
        file(&root, "ignored/selected.rs", b"\xff\x00BINARY_SOURCE_PAYLOAD");
        let blocked = prepare_file(&root, b"ignored/selected.rs", 1,
            CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
        receipt(blocked.as_str(), b"ignored/selected.rs");
        assert_eq!(blocked.exit_code(), crate::EXIT_PARTIAL);
        assert!(blocked.as_str().contains("\"discovery_complete\":false"));
        assert!(blocked.as_str().contains("\"catalogued_files\":\"0\""));
        assert!(blocked.as_str().contains("\"files\":[]"));
        let included = prepare_file(&root, b"ignored/selected.rs", 1,
            CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
        receipt(included.as_str(), b"ignored/selected.rs");
        assert_eq!(included.exit_code(), crate::EXIT_OK);
        assert!(included.as_str().contains("\"discovery_complete\":true"));
        assert!(included.as_str().contains("\"catalogued_files\":\"1\""));
        assert!(!included.as_str().contains("BINARY_SOURCE_PAYLOAD"));
    }
}

#[test]
fn raw_file_names_are_echoed_without_display_round_tripping() {
    let root = root(); let raw = b"C:\\raw\xff\n.rs";
    file(&root, Path::new(&OsString::from_vec(raw.to_vec())), b"\xff\x00binary");
    let report = prepare_file(&root, raw, 1,
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    receipt(report.as_str(), raw);
    assert_eq!(report.as_str().matches(&format!("\"hex\":\"{}\"", hex(raw))).count(), 2);
    assert!(report.as_str().contains("\"observed_bytes\":\"8\""));
    assert!(report.as_str().contains("\"discovery_complete\":true"));
}

#[test]
fn the_path_admission_boundary_and_final_component_depth_preserve_identity() {
    let root = root();
    let mut components = vec!["a".repeat(224); 8]; components.push("b".repeat(248));
    let longest = components.join("/");
    assert_eq!(longest.len(), MAX_PROJECT_FILE_BYTES);
    // The wire/parser bound is not a filesystem guarantee: adding the root to
    // a 2048-byte relative path can exceed the native platform's path limit.
    assert_eq!(file_path(longest.as_bytes()).unwrap().as_bytes(), longest.as_bytes());
    let deepest = std::iter::repeat_n("d", 32).collect::<Vec<_>>().join("/");
    file(&root, &deepest, b"x");
    let report = prepare_file(&root, deepest.as_bytes(), 1,
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    receipt(report.as_str(), deepest.as_bytes());
    assert!(report.as_str().contains("\"catalogued_files\":\"1\""));
    assert!(report.as_str().contains("\"discovery_complete\":true"));
}

#[test]
fn malformed_missing_symlink_directory_and_over_limit_targets_are_refused() {
    let root = root(); let outside = self::root();
    file(&root, "selected/kept.rs", b"x"); file(&outside, "escape.rs", b"outside");
    for selected in [b"".as_slice(), b"/selected", b"selected/", b".", b"..",
        b"selected/../escape", b"selected//kept.rs", b"selected/\0kept.rs"] {
        assert!(matches!(prepare_file(&root, selected, 1,
            CatalogDiscoveryPolicy::RepositoryRules, || false),
            Err(AtlasSessionError::InvalidLimits)), "{selected:?}");
    }
    for selected in [vec![b'x'; MAX_PROJECT_FILE_BYTES + 1],
        std::iter::repeat_n("a", 33).collect::<Vec<_>>().join("/").into_bytes()] {
        assert!(matches!(prepare_file(&root, &selected, 1,
            CatalogDiscoveryPolicy::RepositoryRules, || false), Err(AtlasSessionError::InvalidLimits)));
    }
    symlink(outside.join("escape.rs"), root.join("alias.rs")).unwrap();
    symlink(&outside, root.join("alias-dir")).unwrap();
    for selected in [b"missing.rs".as_slice(), b"selected", b"alias.rs", b"alias-dir/escape.rs"] {
        assert!(prepare_file(&root, selected, 1,
            CatalogDiscoveryPolicy::IncludeExcluded, || false).is_err(), "{selected:?}");
    }
    for count in [0, 20_001] {
        assert!(matches!(prepare_file(&root, b"selected/kept.rs", count,
            CatalogDiscoveryPolicy::RepositoryRules, || false), Err(AtlasSessionError::InvalidLimits)));
    }
}

#[test]
fn cancellation_before_discovery_or_before_publication_cannot_publish_success() {
    let root = root(); file(&root, "selected/kept.rs", b"x");
    let mut polls = 0;
    prepare_file(&root, b"selected/kept.rs", 1, CatalogDiscoveryPolicy::RepositoryRules,
        || { polls += 1; false }).unwrap();
    assert!(polls > 2);
    for stop_at in [1, 2, polls] {
        let mut count = 0;
        assert!(matches!(prepare_file(&root, b"selected/kept.rs", 1,
            CatalogDiscoveryPolicy::RepositoryRules,
            || { count += 1; count >= stop_at }), Err(AtlasSessionError::Canceled)));
    }
}
