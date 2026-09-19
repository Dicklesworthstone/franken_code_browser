use std::{fs, io::Write, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb_app::host::{source_cache::{SourceCache, CacheLimits, CacheError}, source_document};
use fcb::store::Sha256;
static NEXT: AtomicU64 = AtomicU64::new(1);
fn fixture() -> (PathBuf, PathBuf) {
    let parent = fs::canonicalize(std::env::temp_dir()).unwrap();
    let folder = parent.join(format!("fcb-cache-test-{}-{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&folder).unwrap();
    let source = folder.join("source.rs");
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&source).unwrap();
    file.write_all("// 🦀\nfn hello() { let n = 42; }\n".as_bytes()).unwrap();
    (folder.join("cache"), source)
}
#[test]
fn cold_ram_and_reopened_disk_preserve_exact_json_without_relexing() {
    let (root, source) = fixture();
    let reference = source_document::read(&source, || false).unwrap();
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let (cold, key) = cache.source(&source, || false).unwrap();
    assert_eq!(cold.as_str(), reference.as_str());
    let (warm, warm_key) = cache.source(&source, || false).unwrap();
    assert_eq!(warm.as_str(), cold.as_str()); assert_eq!(key, warm_key);
    assert_eq!(cache.stats().lexer_calls, 1); assert_eq!(cache.stats().ram_hits, 1);
    assert_eq!(cache.stats().source_bytes_read, 2 * fs::metadata(&source).unwrap().len());
    drop(cache);
    let mut reopened = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let (disk, disk_key) = reopened.source(&source, || false).unwrap();
    assert_eq!(disk.as_str(), cold.as_str()); assert_eq!(disk_key, key);
    assert_eq!(reopened.stats().lexer_calls, 0); assert_eq!(reopened.stats().disk_hits, 1);
}
#[test]
fn same_size_restored_timestamp_edit_cannot_reuse_stale_roles() {
    let (root, source) = fixture();
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let before_time = fs::metadata(&source).unwrap().modified().unwrap();
    let (before, old_key) = cache.source(&source, || false).unwrap();
    let bytes = fs::read_to_string(&source).unwrap().replace("42", "43");
    fs::write(&source, bytes).unwrap();
    fs::OpenOptions::new().write(true).open(&source).unwrap().set_times(fs::FileTimes::new().set_modified(before_time)).unwrap();
    let (after, new_key) = cache.source(&source, || false).unwrap();
    assert_ne!(old_key, new_key); assert_ne!(before.as_str(), after.as_str());
    assert_eq!(cache.stats().lexer_calls, 2);
}
#[test]
fn corrupt_disk_entry_is_recomputed_and_repaired_atomically() {
    let (root, source) = fixture();
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let (before, key) = cache.source(&source, || false).unwrap();
    drop(cache);
    fs::write(root.join(format!("source-{}.bin", key.to_hex())), b"broken").unwrap();
    let mut reopened = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let (after, _) = reopened.source(&source, || false).unwrap();
    assert_eq!(before.as_str(), after.as_str()); assert_eq!(reopened.stats().lexer_calls, 1);
    assert!(reopened.stats().corrupt >= 1); drop(reopened);
    let mut repaired = SourceCache::open(&root, CacheLimits::default()).unwrap();
    assert_eq!(repaired.source(&source, || false).unwrap().0.as_str(), before.as_str());
    assert_eq!(repaired.stats().lexer_calls, 0);
}
#[test]
fn source_quota_failure_preserves_source_and_cancellation_does_not_publish() {
    let (root, source) = fixture();
    let mut cache = SourceCache::open(&root, CacheLimits { ram_bytes: 0, disk_bytes: 1, entries: 1 }).unwrap();
    assert!(cache.source(&source, || true).is_err()); assert_eq!(cache.stats().lexer_calls, 0);
    assert!(cache.source(&source, || false).unwrap().0.as_str().contains("hello"));
    assert_eq!(cache.disk_bytes(), 0); assert_eq!(cache.stats().write_refusals, 1);
}
#[test]
fn native_artifacts_are_separate_bounded_persistent_and_conflict_checked() {
    let (root, _) = fixture();
    let key = Sha256::digest(b"font/layout/source identity");
    let mut cache = SourceCache::open(&root, CacheLimits { ram_bytes: 4, disk_bytes: 10000, entries: 2 }).unwrap();
    assert!(cache.get_native(key).unwrap().is_none());
    cache.put_native(key, b"native pixels").unwrap();
    assert_eq!(cache.ram_bytes(), 0);
    assert_eq!(cache.get_native(key).unwrap().unwrap().as_ref(), b"native pixels");
    assert_eq!(cache.put_native(key, b"wrong"), Err(CacheError::Conflict));
    drop(cache);
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    assert_eq!(cache.get_native(key).unwrap().unwrap().as_ref(), b"native pixels");
}
#[test]
fn root_ownership_and_exclusive_writer_are_enforced() {
    let (root, _) = fixture();
    let cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    assert!(matches!(SourceCache::open(&root, CacheLimits::default()), Err(CacheError::Busy)));
    drop(cache);
    assert!(SourceCache::open(&root, CacheLimits::default()).is_ok());
    let unrelated = root.parent().unwrap().join("unrelated"); fs::create_dir(&unrelated).unwrap();
    fs::write(unrelated.join("keep"), b"owner data").unwrap();
    assert!(matches!(SourceCache::open(&unrelated, CacheLimits::default()), Err(CacheError::Root)));
    assert_eq!(fs::read(unrelated.join("keep")).unwrap(), b"owner data");
}
#[cfg(unix)]
#[test]
fn static_symlink_cache_root_is_refused() {
    let (root, _) = fixture();
    let link = root.parent().unwrap().join("link");
    fs::create_dir(&root).unwrap(); std::os::unix::fs::symlink(&root, &link).unwrap();
    assert!(matches!(SourceCache::open(&link, CacheLimits::default()), Err(CacheError::Root)));
}

#[test]
fn language_is_part_of_identity_and_empty_native_artifact_is_a_hit() {
    let (root, source) = fixture();
    let plain = source.with_extension("txt");
    let mut file = fs::OpenOptions::new().write(true).create_new(true).open(&plain).unwrap();
    file.write_all(&fs::read(&source).unwrap()).unwrap();
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let (_, rust_key) = cache.source(&source, || false).unwrap();
    let (plain_response, plain_key) = cache.source(&plain, || false).unwrap();
    assert_ne!(rust_key, plain_key);
    assert!(plain_response.as_str().contains("\"syntax_supported\":false"));
    let key = Sha256::digest(b"empty native artifact");
    cache.put_native(key, b"").unwrap();
    assert_eq!(cache.get_native(key).unwrap().unwrap().len(), 0);
}
