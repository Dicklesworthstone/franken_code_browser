#![forbid(unsafe_code)]

//! First-use metadata for the native browser, before source previews exist.
//! Reuse the retained atlas discovery/layout policy; this service performs no
//! source capture, profiling, parsing or indexing. A bounded/partial catalog is
//! useful, not an empty-success or a reason to discard its known members.

use std::path::Path;
use fcb::ByteLength;
use fcb::search::ResourceBudget;
use crate::{AppError, allocation, owner};
use crate::output::{Output, MAX_ENCODED_BYTES, OUTPUT_OVERHEAD_BYTES};
use super::super::{HostResponse, atlas_session::{AtlasSession, AtlasSessionError, AtlasSessionOptions,
        MAX_ATLAS_DIRECTORY_BYTES, MAX_ATLAS_FILE_BYTES, directory_path, file_path},
    catalog_session::CatalogDiscoveryPolicy};

pub const MAX_PROJECT_DIRECTORY_BYTES: usize = MAX_ATLAS_DIRECTORY_BYTES;
pub const MAX_PROJECT_FILE_BYTES: usize = MAX_ATLAS_FILE_BYTES;

/// Response-local IDs do not authorize later source access. Paths are reversible
/// native payloads, including names the current UTF-8-only native opener cannot
/// represent. The host must retain and label that distinction, not lose the row.
/// This one-shot worker call does not claim a progressively streamed directory
/// walk or a presented frame. It admits at most MAX_ATLAS_SESSION_FILES entries.
pub fn prepare(root: &Path, max_files: usize, canceled: impl FnMut() -> bool)
    -> Result<HostResponse, AtlasSessionError> {
    prepare_with_policy(root, max_files, CatalogDiscoveryPolicy::RepositoryRules, canceled)
}

/// Use the host's selected discovery scope instead of silently restoring the
/// default while preparing geometry. The receipt retains the actual policy
/// and separate rule-file evidence; including excluded files reads no rules.
pub fn prepare_with_policy(root: &Path, max_files: usize, policy: CatalogDiscoveryPolicy,
    mut canceled: impl FnMut() -> bool) -> Result<HostResponse, AtlasSessionError> {
    let atlas = AtlasSession::open_with_policy(owner(), root,
        AtlasSessionOptions { max_files, ..Default::default() }, policy, &mut canceled)?;
    encode_catalog(&atlas, canceled)
}

/// Explicit bounded subtree, under the original project root and selected
/// discovery policy. Raw Unix components are validated without normalizing
/// backslashes, drive-looking names or invalid UTF-8 into different identities.
/// Existing root catalogs retain their original schema; this additive route
/// echoes the directory and declares all file paths relative to the project.
pub fn prepare_directory(root: &Path, directory: &[u8], max_files: usize,
    policy: CatalogDiscoveryPolicy, mut canceled: impl FnMut() -> bool)
    -> Result<HostResponse, AtlasSessionError> {
    if canceled() { return Err(AtlasSessionError::Canceled); }
    let path = directory_path(directory)?;
    let atlas = AtlasSession::open_directory_with_policy(owner(), root, path,
        AtlasSessionOptions { max_files, ..Default::default() }, policy, &mut canceled)?;
    encode_catalog(&atlas, canceled)
}

/// Metadata for one exact member under the original project root. Siblings do
/// not consume this catalog's allowance, so a member omitted from a wide parent
/// catalog can still receive its own atlas. Ancestor rules and confinement use
/// the same retained engine as directory discovery; no source payload is read.
/// The distinct receipt closes only the selected file's discovery scope, never
/// its parent folder or project. Deliberate exclusions may produce zero rows.
pub fn prepare_file(root: &Path, file: &[u8], max_files: usize,
    policy: CatalogDiscoveryPolicy, mut canceled: impl FnMut() -> bool)
    -> Result<HostResponse, AtlasSessionError> {
    if canceled() { return Err(AtlasSessionError::Canceled); }
    let path = file_path(file)?;
    let atlas = AtlasSession::open_file_with_policy(owner(), root, path,
        AtlasSessionOptions { max_files, ..Default::default() }, policy, &mut canceled)?;
    encode_catalog(&atlas, canceled)
}

fn encode_catalog(atlas: &AtlasSession, mut canceled: impl FnMut() -> bool)
    -> Result<HostResponse, AtlasSessionError> {
    let catalog = atlas.atlas().catalog();
    let index = atlas.atlas().index()?;
    // Admit both response/native-copy overlap and the encoder's path scratch.
    // Reserving only four payloads leaves no capacity for Output's own overhead.
    let budget = ResourceBudget::new(owner(), ByteLength::new((4 * MAX_ENCODED_BYTES + 4096 + OUTPUT_OVERHEAD_BYTES) as u64))
        .map_err(|_| AppError::Admission)?;
    let lease = budget.try_reserve_managed(owner(), allocation(94),
        ByteLength::new((3 * MAX_ENCODED_BYTES + 4096) as u64)).map_err(|_| AppError::Admission)?;
    let mut out = Output::new(owner(), MAX_ENCODED_BYTES, &budget, allocation(1))?;
    if let Some(file) = catalog.file() {
        if catalog.entries().len() > 1
            || catalog.entries().iter().any(|entry| entry.path().as_bytes() != file.as_bytes()) {
            return Err(AppError::InvalidRange.into());
        }
        out.literal("{\"schema\":\"fcb.project-file-catalog/1\",\"status\":\"ok\",\"command\":\"file-catalog\",\"file\":")?;
        out.path(&file.raw().to_path_buf())?;
        out.literal(",\"path_basis\":\"project-root\"")?;
    } else if let Some(directory) = catalog.directory() {
        out.literal("{\"schema\":\"fcb.project-directory-catalog/1\",\"status\":\"ok\",\"command\":\"directory-catalog\",\"directory\":")?;
        out.path(&directory.raw().to_path_buf())?;
        out.literal(",\"path_basis\":\"project-root\"")?;
    } else {
        out.literal("{\"schema\":\"fcb.project-catalog/1\",\"status\":\"ok\",\"command\":\"catalog\"")?;
    }
    out.literal(",\"identity_scope\":\"response-local\",\"native_presented\":false,\"source_payload_read\":false,\"payload_bytes_read\":\"0\",\"read_calls\":\"0\",\"discovery_complete\":")?;
    out.boolean(catalog.discovery_complete())?;
    out.literal(",\"catalogued_files\":")?; out.integer(catalog.entries().len() as u64)?;
    out.literal(",\"policy\":")?; out.quoted(catalog.policy_name())?;
    crate::workspace::rule_output::fields(&mut out, catalog)?;
    out.literal(",\"world\":{\"w\":4096,\"h\":4096},\"files\":[")?;
    for (ordinal, entry) in catalog.entries().iter().enumerate() {
        if canceled() { return Err(AtlasSessionError::Canceled); }
        atlas.validate_active()?;
        let file = catalog.file_id(ordinal).ok_or(AppError::InvalidRange)?;
        let node = atlas.atlas().node_for_file(file)?;
        let rect = index.bounds_in(node, index.root_node())?;
        if ordinal != 0 { out.literal(",")?; }
        out.literal("{\"file_id\":")?; out.integer(file.get())?;
        out.literal(",\"path\":")?; out.path(&entry.path().raw().to_path_buf())?;
        out.literal(",\"observed_bytes\":")?; out.integer(entry.observed_bytes())?;
        for (key, value) in [("x", rect.min_x()), ("y", rect.min_y()),
            ("w", rect.size().width()), ("h", rect.size().height())] {
            if !value.is_finite() || value < 0.0 { return Err(AppError::InvalidRange.into()); }
            out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.literal(&value.to_string())?;
        }
        out.literal("}")?;
    }
    out.literal("]}\n")?;
    atlas.validate_active()?;
    if canceled() { return Err(AtlasSessionError::Canceled); }
    let mut text = String::new();
    text.try_reserve_exact(out.as_bytes().len()).map_err(|_| AppError::Admission)?;
    if text.capacity() > out.as_bytes().len() { return Err(AppError::Admission.into()); }
    text.push_str(std::str::from_utf8(out.as_bytes()).map_err(|_| AppError::InvalidRange)?);
    Ok(HostResponse { text, exit_code: if catalog.discovery_complete() { crate::EXIT_OK } else { crate::EXIT_PARTIAL }, _lease: lease })
}

#[cfg(all(test, unix))]
#[path = "project_directory_catalog_tests.rs"]
mod directory_tests;

#[cfg(all(test, unix))]
#[path = "project_file_catalog_tests.rs"]
mod file_tests;

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::{fs, ffi::OsString, os::unix::ffi::OsStringExt, sync::atomic::{AtomicU64, Ordering}};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    fn root() -> std::path::PathBuf {
        let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let root = std::env::temp_dir().join(format!("fcb-project-catalog-{}-{stamp}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&root).unwrap();
        root // Retain test inputs for diagnosis; no destructive cleanup.
    }
    #[test]
    fn metadata_catalog_keeps_raw_names_without_opening_source() {
        let root = root();
        fs::write(root.join("readable.rs"), "SOURCE_PAYLOAD_MUST_NOT_APPEAR").unwrap();
        fs::write(root.join(OsString::from_vec(b"raw-\xff.rs".to_vec())), [0xff, 0xfe, 0, 0]).unwrap();
        let response = prepare(&root, 32, || false).unwrap();
        let text = response.as_str();
        assert!(text.contains("\"catalogued_files\":\"2\""), "{text}");
        assert!(text.contains("7261772dff2e7273"), "{text}");
        assert!(text.contains("\"discovery_complete\":true"), "{text}");
        assert!(text.contains("\"payload_bytes_read\":\"0\",\"read_calls\":\"0\""));
        assert!(!text.contains("SOURCE_PAYLOAD_MUST_NOT_APPEAR"));
        // The existing compatibility endpoint still refuses names it cannot
        // represent, rather than silently changing its wire contract.
        assert!(super::super::prepare(&root, super::super::LegacyAtlasOptions {
            max_profile_source_bytes: 0, ..Default::default()
        }, || false).is_err());
    }
    #[test]
    fn partial_membership_is_explicit_and_retains_known_files() {
        let root = root();
        for name in ["a.rs", "b.rs", "c.rs"] { fs::write(root.join(name), "x").unwrap(); }
        let response = prepare(&root, 1, || false).unwrap();
        let text = response.as_str();
        assert!(text.contains("\"discovery_complete\":false"), "{text}");
        assert!(text.contains("\"catalogued_files\":\"1\""), "{text}");
        assert!(text.contains("\"file_id\":\"1\""), "{text}");
    }
    #[test]
    fn empty_catalog_is_distinct_from_refusal_and_cancellation() {
        let root = root();
        let response = prepare(&root, 32, || false).unwrap();
        assert!(response.as_str().contains("\"discovery_complete\":true"));
        assert!(response.as_str().contains("\"files\":[]"));
        assert!(prepare(&root, 0, || false).is_err());
        assert!(prepare(&root, 20_001, || false).is_err());
        assert!(matches!(prepare(&root, 32, || true), Err(AtlasSessionError::Canceled)));
        assert!(prepare(&root.join("missing"), 32, || false).is_err());
    }
    #[test]
    fn selected_policy_controls_atlas_membership_and_default_is_compatible() {
        let root = root();
        fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
        fs::write(root.join("ignored.rs"), b"DO_NOT_READ_SOURCE_PAYLOAD").unwrap();
        fs::write(root.join("kept.rs"), b"DO_NOT_READ_SOURCE_PAYLOAD").unwrap();
        fs::create_dir(root.join("target")).unwrap();
        fs::write(root.join("target/generated.rs"), b"DO_NOT_READ_SOURCE_PAYLOAD").unwrap();
        std::os::unix::fs::symlink(root.join("kept.rs"), root.join("not-followed.rs")).unwrap();

        let legacy = prepare(&root, 32, || false).unwrap();
        let rules = prepare_with_policy(&root, 32, CatalogDiscoveryPolicy::RepositoryRules, || false).unwrap();
        assert_eq!(legacy.as_str(), rules.as_str());
        assert!(rules.as_str().contains("\"policy\":\"repository-rules-v1"), "{}", rules.as_str());
        assert!(rules.as_str().contains("\"rule_files_enabled\":true"), "{}", rules.as_str());
        assert!(rules.as_str().contains("\"configuration_bytes_read\":\"11\""), "{}", rules.as_str());
        assert!(!rules.as_str().contains("\"display\":\"ignored.rs\""));
        assert!(!rules.as_str().contains("\"display\":\"target/generated.rs\""));

        let included = prepare_with_policy(&root, 32, CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
        let text = included.as_str();
        assert!(text.contains("\"policy\":\"all-regular-files/no-rule-files-v1\""), "{text}");
        assert!(text.contains("\"rule_files_enabled\":false,\"rule_policy\":null"), "{text}");
        for path in ["ignored.rs", "kept.rs", "target/generated.rs"] {
            assert!(text.contains(&format!("\"display\":\"{path}\"")), "{text}");
        }
        assert!(!text.contains("\"display\":\"not-followed.rs\""));
        assert!(text.contains("\"source_payload_read\":false,\"payload_bytes_read\":\"0\",\"read_calls\":\"0\""));
        assert!(!text.contains("DO_NOT_READ_SOURCE_PAYLOAD"));
    }

    #[test]
    fn explicit_inclusion_bypasses_bad_rules_without_turning_limits_into_complete_results() {
        let root = root();
        fs::write(root.join("safe.rs"), b"source").unwrap();
        fs::create_dir(root.join("bad")).unwrap();
        fs::write(root.join("bad/.gitignore"), b"[broken").unwrap();
        fs::write(root.join("bad/visible.rs"), b"source").unwrap();
        let rules = prepare(&root, 32, || false).unwrap();
        assert!(rules.as_str().contains("\"discovery_complete\":false"), "{}", rules.as_str());
        assert!(rules.as_str().contains("RULE_UNSUPPORTED_PATTERN"), "{}", rules.as_str());
        assert!(!rules.as_str().contains("\"display\":\"bad/visible.rs\""));

        let included = prepare_with_policy(&root, 32, CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
        assert!(included.as_str().contains("\"discovery_complete\":true"), "{}", included.as_str());
        assert!(included.as_str().contains("\"display\":\"bad/visible.rs\""));
        assert!(included.as_str().contains("\"rule_files_enabled\":false,\"rule_policy\":null"));
        let limited = prepare_with_policy(&root, 1, CatalogDiscoveryPolicy::IncludeExcluded, || false).unwrap();
        assert!(limited.as_str().contains("\"discovery_complete\":false"), "{}", limited.as_str());
        assert!(limited.as_str().contains("\"catalogued_files\":\"1\""));
        assert_eq!(limited.exit_code(), crate::EXIT_PARTIAL);
    }

    #[test]
    fn both_policies_keep_cancellation_root_and_file_admission() {
        let root = root();
        fs::write(root.join("kept.rs"), b"source").unwrap();
        let alias = root.with_extension("symlink");
        std::os::unix::fs::symlink(&root, &alias).unwrap();
        for policy in [CatalogDiscoveryPolicy::RepositoryRules, CatalogDiscoveryPolicy::IncludeExcluded] {
            assert!(matches!(prepare_with_policy(&root, 32, policy, || true),
                Err(AtlasSessionError::Canceled)));
            assert!(prepare_with_policy(&root, 0, policy, || false).is_err());
            assert!(prepare_with_policy(&root, 20_001, policy, || false).is_err());
            assert!(prepare_with_policy(&alias, 32, policy, || false).is_err());
            let mut polls = 0;
            prepare_with_policy(&root, 32, policy, || { polls += 1; false }).unwrap();
            let mut reached = 0;
            assert!(matches!(prepare_with_policy(&root, 32, policy, || {
                reached += 1; reached == polls
            }), Err(AtlasSessionError::Canceled)));
        }
    }

}
