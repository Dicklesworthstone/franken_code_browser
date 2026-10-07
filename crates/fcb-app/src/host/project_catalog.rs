#![forbid(unsafe_code)]

//! First-use metadata for the native browser, before source previews exist.
//! Reuse the retained atlas discovery/layout policy; this service performs no
//! source capture, profiling, parsing or indexing. A bounded/partial catalog is
//! useful, not an empty-success or a reason to discard its known members.

use std::path::Path;
use fcb::ByteLength;
use fcb::search::ResourceBudget;
use crate::{AppError, allocation, owner};
use crate::output::{Output, MAX_ENCODED_BYTES};
use super::super::{HostResponse, atlas_session::{AtlasSession, AtlasSessionError, AtlasSessionOptions}};

/// Response-local IDs do not authorize later source access. Paths are reversible
/// native payloads, including names the current UTF-8-only native opener cannot
/// represent. The host must retain and label that distinction, not lose the row.
/// This one-shot worker call does not claim a progressively streamed directory
/// walk or a presented frame. It admits at most MAX_ATLAS_SESSION_FILES entries.
pub fn prepare(root: &Path, max_files: usize, mut canceled: impl FnMut() -> bool)
    -> Result<HostResponse, AtlasSessionError> {
    let atlas = AtlasSession::open(owner(), root,
        AtlasSessionOptions { max_files, ..Default::default() }, &mut canceled)?;
    let catalog = atlas.atlas().catalog();
    let index = atlas.atlas().index()?;
    // Reserve response/native-copy overlap independently from retained geometry.
    let budget = ResourceBudget::new(owner(), ByteLength::new((4 * MAX_ENCODED_BYTES + 4096) as u64))
        .map_err(|_| AppError::Admission)?;
    let lease = budget.try_reserve_managed(owner(), allocation(94),
        ByteLength::new((3 * MAX_ENCODED_BYTES + 4096) as u64)).map_err(|_| AppError::Admission)?;
    let mut out = Output::new(owner(), MAX_ENCODED_BYTES, &budget, allocation(1))?;
    out.literal("{\"schema\":\"fcb.project-catalog/1\",\"status\":\"ok\",\"command\":\"catalog\",\"identity_scope\":\"response-local\",\"native_presented\":false,\"source_payload_read\":false,\"payload_bytes_read\":\"0\",\"read_calls\":\"0\",\"discovery_complete\":")?;
    out.boolean(catalog.discovery_complete())?;
    out.literal(",\"catalogued_files\":")?; out.integer(catalog.entries().len() as u64)?;
    out.literal(",\"policy\":")?; out.quoted(catalog.policy_name())?;
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
}
