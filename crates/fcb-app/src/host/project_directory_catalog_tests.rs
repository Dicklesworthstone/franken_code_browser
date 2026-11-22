//! The real metadata -> shared retained layout -> reversible JSON route.
use super::*;
use std::{ffi::OsString, fs, os::unix::{ffi::OsStringExt, fs::symlink},
    path::PathBuf, sync::atomic::{AtomicU64, Ordering}};

fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-directory-catalog-{}-{}-{}",
        std::process::id(), std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn file(root: &Path, name: impl AsRef<Path>, bytes: &[u8]) {
    let path = root.join(name);
    fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, bytes).unwrap();
}

#[test]
fn a_folder_beyond_the_root_catalog_cap_gets_its_own_complete_atlas() {
    let root = root();
    let candidates = ["first-focused", "second-focused"];
    for directory in candidates {
        file(&root, format!("{directory}/kept.rs"), b"SOURCE_PAYLOAD_MUST_NOT_APPEAR");
    }
    let initial = prepare(&root, 1, || false).unwrap();
    assert!(initial.as_str().contains("\"discovery_complete\":false"));
    assert!(initial.as_str().contains("\"catalogued_files\":\"1\""));
    // Discover the omitted candidate from the response instead of assuming a
    // particular read_dir order on this filesystem.
    let selected = candidates.into_iter().find(|directory| {
        let path = format!("\"display\":\"{directory}/kept.rs\"");
        !initial.as_str().contains(path.as_str())
    }).expect("a one-file catalog must omit one of the two candidate folders");
    let focused = prepare_directory(&root, selected.as_bytes(), 1,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    let text = focused.as_str();
    assert!(text.contains("\"schema\":\"fcb.project-directory-catalog/1\""));
    assert!(text.contains("\"command\":\"directory-catalog\""));
    assert!(text.contains("\"directory\":{\"encoding\":\"unix-bytes\""));
    assert!(text.contains(format!("\"display\":\"{selected}\"").as_str()));
    assert!(text.contains("\"path_basis\":\"project-root\""));
    assert!(text.contains(format!("\"display\":\"{selected}/kept.rs\"").as_str()));
    assert!(text.contains("\"catalogued_files\":\"1\""));
    assert!(text.contains("\"discovery_complete\":true"));
    assert!(text.contains("\"payload_bytes_read\":\"0\",\"read_calls\":\"0\""));
    assert!(!text.contains("SOURCE_PAYLOAD_MUST_NOT_APPEAR"));
    for directory in candidates {
        if directory != selected {
            assert!(!text.contains(format!("\"display\":\"{directory}/kept.rs\"").as_str()));
        }
    }
}

#[test]
fn a_focused_catalog_retains_partial_membership_and_distinguishes_empty_from_missing() {
    let root = root();
    file(&root, "selected/a.rs", b"x"); file(&root, "selected/b.rs", b"x");
    fs::create_dir(root.join("empty")).unwrap();
    let partial = prepare_directory(&root, b"selected", 1,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    assert!(partial.as_str().contains("\"discovery_complete\":false"));
    assert!(partial.as_str().contains("\"catalogued_files\":\"1\""));
    let empty = prepare_directory(&root, b"empty", 1,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    assert!(empty.as_str().contains("\"discovery_complete\":true"));
    assert!(empty.as_str().contains("\"files\":[]"));
    assert!(prepare_directory(&root, b"missing", 1,
        CatalogDiscoveryPolicy::RepositoryRules, || false).is_err());
}

#[test]
fn focused_policy_preserves_ancestor_exclusions_and_explicit_include_excluded_choice() {
    let root = root();
    file(&root, ".gitignore", b"ignored/\n");
    file(&root, "ignored/selected/.gitignore", b"!kept.rs\n");
    file(&root, "ignored/selected/kept.rs", b"\xff\x00binary");
    let excluded = prepare_directory(&root, b"ignored/selected", 32,
        CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
    assert!(excluded.as_str().contains("\"discovery_complete\":false"));
    assert!(excluded.as_str().contains("\"files\":[]"));
    assert!(!excluded.as_str().contains("\"display\":\"ignored/selected/kept.rs\""));
    let included = prepare_directory(&root, b"ignored/selected", 32,
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    assert!(included.as_str().contains("\"discovery_complete\":true"));
    assert!(included.as_str().contains("\"display\":\"ignored/selected/kept.rs\""));
    assert!(included.as_str().contains("\"policy\":\"all-regular-files/no-rule-files-v1\""));
    assert!(included.as_str().contains("\"rule_files_enabled\":false"));
}

#[test]
fn raw_directory_bytes_and_source_paths_keep_the_original_project_basis() {
    let root = root(); let name = OsString::from_vec(b"C:\\raw\xff".to_vec());
    file(&root, Path::new(&name).join("source.rs"), b"\xff\x00binary");
    let report = prepare_directory(&root, b"C:\\raw\xff", 32,
        CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
    assert!(report.as_str().contains("\"hex\":\"433a5c726177ff\""));
    assert!(report.as_str().contains("\"hex\":\"433a5c726177ff2f736f757263652e7273\""));
    assert!(report.as_str().contains("\"discovery_complete\":true"));
    assert!(report.as_str().contains("\"source_payload_read\":false"));
}

#[test]
fn malformed_raw_paths_symlinks_limits_and_cancellation_never_become_empty_success() {
    let root = root(); file(&root, "selected/kept.rs", b"x");
    for directory in [b"".as_slice(), b"/selected", b"selected/", b".", b"..",
        b"selected/../escape", b"selected//nested", b"selected/\0nested"] {
        assert!(matches!(prepare_directory(&root, directory, 32,
            CatalogDiscoveryPolicy::RepositoryRules, || false),
            Err(AtlasSessionError::InvalidLimits)), "{directory:?}");
    }
    for directory in [vec![b'x'; MAX_PROJECT_DIRECTORY_BYTES + 1],
        std::iter::repeat_n("a", 32).collect::<Vec<_>>().join("/").into_bytes()] {
        assert!(matches!(prepare_directory(&root, &directory, 32,
            CatalogDiscoveryPolicy::RepositoryRules, || false), Err(AtlasSessionError::InvalidLimits)));
    }
    symlink(root.join("selected"), root.join("alias")).unwrap();
    assert!(prepare_directory(&root, b"alias", 32,
        CatalogDiscoveryPolicy::IncludeExcluded, || false).is_err());
    assert!(prepare_directory(&root, b"selected/kept.rs", 32,
        CatalogDiscoveryPolicy::RepositoryRules, || false).is_err());
    for count in [0, 20_001] {
        assert!(prepare_directory(&root, b"selected", count,
            CatalogDiscoveryPolicy::RepositoryRules, || false).is_err());
    }
    let mut polls = 0;
    prepare_directory(&root, b"selected", 32, CatalogDiscoveryPolicy::RepositoryRules,
        || { polls += 1; false }).unwrap();
    for stop_at in [1, 2, polls] {
        let mut count = 0;
        assert!(matches!(prepare_directory(&root, b"selected", 32,
            CatalogDiscoveryPolicy::RepositoryRules,
            || { count += 1; count >= stop_at }), Err(AtlasSessionError::Canceled)));
    }
}
