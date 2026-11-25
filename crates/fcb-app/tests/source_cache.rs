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

#[test]
fn explicit_native_repair_preserves_old_arc_and_publishes_new_disk_bytes() {
    let (root, source) = fixture();
    let key = Sha256::digest(b"native semantic identity");
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let (source_before, source_key) = cache.source(&source, || false).unwrap();
    cache.put_native(key, b"old font table").unwrap();
    let old = cache.get_native(key).unwrap().unwrap();
    assert_eq!(cache.put_native(key, b"new font table"), Err(CacheError::Conflict));
    cache.repair_native(key, b"new font table").unwrap();
    assert_eq!(old.as_ref(), b"old font table");
    assert_eq!(cache.get_native(key).unwrap().unwrap().as_ref(), b"new font table");
    // Repairing a native artifact with the same digest cannot alter source data.
    cache.repair_native(source_key, b"native under equal key").unwrap();
    assert_eq!(cache.source(&source, || false).unwrap().0.as_str(), source_before.as_str());
    drop(cache);
    let mut disk = SourceCache::open(&root, CacheLimits::default()).unwrap();
    assert_eq!(disk.get_native(key).unwrap().unwrap().as_ref(), b"new font table");
}

#[test]
fn failed_repair_admission_leaves_incumbent_intact() {
    let (root, _) = fixture();
    let key = Sha256::digest(b"one entry");
    let mut cache = SourceCache::open(&root, CacheLimits { ram_bytes: 1024, disk_bytes: 10000, entries: 1 }).unwrap();
    cache.put_native(key, b"old").unwrap();
    assert_eq!(cache.repair_native(key, b"new"), Err(CacheError::Limit));
    assert_eq!(cache.get_native(key).unwrap().unwrap().as_ref(), b"old");
    drop(cache);
    let mut disk = SourceCache::open(&root, CacheLimits::default()).unwrap();
    assert_eq!(disk.get_native(key).unwrap().unwrap().as_ref(), b"old");
}

#[cfg(unix)]
#[test]
fn cache_objects_are_private_and_insecure_existing_root_is_not_chmodded() {
    use std::os::unix::fs::PermissionsExt;
    let (root, source) = fixture();
    let parent = root.parent().unwrap();
    let parent_mode = fs::metadata(parent).unwrap().permissions().mode();
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    cache.source(&source, || false).unwrap();
    assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o700);
    for entry in fs::read_dir(&root).unwrap() {
        assert_eq!(entry.unwrap().metadata().unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(fs::metadata(parent).unwrap().permissions().mode(), parent_mode);
    drop(cache);
    fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(SourceCache::open(&root, CacheLimits::default()), Err(CacheError::Root)));
    assert_eq!(fs::metadata(&root).unwrap().permissions().mode() & 0o777, 0o755);
}

#[test]
fn concurrent_readers_observe_complete_envelopes_during_native_republication() {
    use std::sync::{Arc, atomic::AtomicBool};
    use fcb::store::{EnvelopeReader, EnvelopeSchema, EnvelopeLimits, UnknownPolicy};
    let (root, _) = fixture();
    let key = Sha256::digest(b"publication identity");
    let mut cache = SourceCache::open(&root, CacheLimits::default()).unwrap();
    let first = vec![17u8; 16 * 1024];
    let second = vec![93u8; 16 * 1024];
    cache.put_native(key, &first).unwrap();
    let path = root.join(format!("native-{}.bin", key.to_hex()));
    let done = Arc::new(AtomicBool::new(false));
    let finished = Arc::clone(&done);
    let barrier = Arc::new(std::sync::Barrier::new(2));
    let reading = Arc::clone(&barrier);
    let reader = std::thread::spawn(move || {
        let mut reads = 0;
        loop {
            let raw = fs::read(&path).unwrap();
            let mut envelope = EnvelopeReader::open(&raw, EnvelopeSchema::new(0x53434348, 1, 0),
                EnvelopeLimits::default(), UnknownPolicy::Strict).unwrap();
            assert_eq!(envelope.get_str().unwrap(), "native");
            assert_eq!(envelope.get_bytes().unwrap(), key.as_bytes());
            let payload = envelope.get_bytes().unwrap();
            assert_eq!(payload.len(), 16 * 1024);
            assert!(payload.iter().all(|b| *b == 17) || payload.iter().all(|b| *b == 93));
            envelope.finish().unwrap();
            reads += 1;
            if reads == 1 { reading.wait(); continue; }
            if finished.load(Ordering::Acquire) { break; }
        }
        reads
    });
    barrier.wait();
    for i in 0..32 { cache.repair_native(key, if i % 2 == 0 { &second } else { &first }).unwrap(); }
    done.store(true, Ordering::Release);
    assert!(reader.join().unwrap() >= 2);
}

#[test]
fn full_entry_cache_reclaims_oldest_artifact_without_invalidating_borrowed_bytes() {
    use std::time::{Duration, UNIX_EPOCH};
    let (root, _) = fixture();
    let oldest = Sha256::digest(b"oldest");
    let recent = Sha256::digest(b"recent");
    let incoming = Sha256::digest(b"incoming");
    let limits = CacheLimits { ram_bytes: 1024, disk_bytes: 10000, entries: 2 };
    let mut cache = SourceCache::open(&root, limits).unwrap();
    cache.put_native(oldest, b"old exact pixels").unwrap();
    cache.put_native(recent, b"recent exact pixels").unwrap();
    let retained = cache.get_native(oldest).unwrap().unwrap();
    for (key, seconds) in [(oldest, 1), (recent, 2)] {
        fs::OpenOptions::new().write(true).open(root.join(format!("native-{}.bin", key.to_hex())))
            .unwrap().set_times(fs::FileTimes::new().set_modified(UNIX_EPOCH + Duration::from_secs(seconds))).unwrap();
    }
    cache.put_native(incoming, b"new exact pixels").unwrap();
    assert_eq!(cache.stats().evicted_entries, 1);
    assert!(cache.stats().evicted_bytes > 0);
    assert_eq!(retained.as_ref(), b"old exact pixels");
    assert!(cache.get_native(oldest).unwrap().is_none());
    assert_eq!(cache.get_native(recent).unwrap().unwrap().as_ref(), b"recent exact pixels");
    assert_eq!(cache.get_native(incoming).unwrap().unwrap().as_ref(), b"new exact pixels");
    drop(cache);
    let mut reopened = SourceCache::open(&root, limits).unwrap();
    assert!(reopened.get_native(oldest).unwrap().is_none());
    assert_eq!(reopened.get_native(incoming).unwrap().unwrap().as_ref(), b"new exact pixels");
}

#[test]
fn full_disk_cache_reclaims_enough_entries_and_remains_within_quota() {
    let (root, _) = fixture();
    let first = Sha256::digest(b"first disk entry");
    let second = Sha256::digest(b"second disk entry");
    let incoming = Sha256::digest(b"larger disk entry");
    let limits = CacheLimits { ram_bytes: 0, disk_bytes: 1050, entries: 8 };
    let mut cache = SourceCache::open(&root, limits).unwrap();
    cache.put_native(first, &[1; 100]).unwrap();
    cache.put_native(second, &[2; 200]).unwrap();
    let before = cache.disk_bytes();
    cache.put_native(incoming, &[3; 800]).unwrap();
    assert_eq!(cache.stats().evicted_entries, 2);
    assert_eq!(cache.stats().evicted_bytes, before);
    assert!(cache.disk_bytes() <= limits.disk_bytes);
    assert!(cache.get_native(first).unwrap().is_none());
    assert!(cache.get_native(second).unwrap().is_none());
    assert_eq!(cache.get_native(incoming).unwrap().unwrap().as_ref(), &[3; 800]);
}

#[test]
fn impossible_new_artifact_does_not_evict_existing_cache_entries() {
    let (root, _) = fixture();
    let first = Sha256::digest(b"keep first");
    let second = Sha256::digest(b"keep second");
    let incoming = Sha256::digest(b"too large");
    let mut cache = SourceCache::open(&root,
        CacheLimits { ram_bytes: 0, disk_bytes: 1000, entries: 2 }).unwrap();
    cache.put_native(first, b"first").unwrap();
    cache.put_native(second, b"second").unwrap();
    let before = cache.disk_bytes();
    assert_eq!(cache.put_native(incoming, &[0; 1000]), Err(CacheError::Limit));
    assert_eq!(cache.stats().evicted_entries, 0);
    assert_eq!(cache.disk_bytes(), before);
    assert_eq!(cache.get_native(first).unwrap().unwrap().as_ref(), b"first");
    assert_eq!(cache.get_native(second).unwrap().unwrap().as_ref(), b"second");
}

#[test]
fn repair_reclaims_other_artifacts_and_keeps_incumbent_until_replacement() {
    let (root, _) = fixture();
    let key = Sha256::digest(b"repair target");
    let other = Sha256::digest(b"disposable neighbor");
    let mut cache = SourceCache::open(&root,
        CacheLimits { ram_bytes: 1024, disk_bytes: 10000, entries: 2 }).unwrap();
    cache.put_native(key, b"old retained pixels").unwrap();
    cache.put_native(other, b"rebuildable neighbor").unwrap();
    let old = cache.get_native(key).unwrap().unwrap();
    cache.repair_native(key, b"new pixels").unwrap();
    assert_eq!(old.as_ref(), b"old retained pixels");
    assert_eq!(cache.get_native(key).unwrap().unwrap().as_ref(), b"new pixels");
    assert!(cache.get_native(other).unwrap().is_none());
    assert_eq!(cache.stats().evicted_entries, 1);
}

fn private_test_file(path: &std::path::Path, bytes: &[u8]) {
    let mut options = fs::OpenOptions::new(); options.write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    options.open(path).unwrap().write_all(bytes).unwrap();
}

#[test]
fn quota_reclamation_preserves_unrelated_files_even_with_cache_shaped_names() {
    let (root, _) = fixture();
    let old = Sha256::digest(b"actual artifact");
    let incoming = Sha256::digest(b"replacement artifact");
    let impostor = root.join(format!("native-{}.bin", Sha256::digest(b"not cache data").to_hex()));
    let notes = root.join("notes.txt");
    let mut cache = SourceCache::open(&root,
        CacheLimits { ram_bytes: 0, disk_bytes: 10000, entries: 3 }).unwrap();
    cache.put_native(old, b"disposable").unwrap();
    private_test_file(&notes, b"personal note");
    private_test_file(&impostor, b"unrelated payload with a cache-shaped name");
    cache.put_native(incoming, b"new artifact").unwrap();
    assert_eq!(cache.stats().evicted_entries, 1);
    assert_eq!(fs::read(notes).unwrap(), b"personal note");
    assert_eq!(fs::read(impostor).unwrap(), b"unrelated payload with a cache-shaped name");
    assert_eq!(cache.get_native(incoming).unwrap().unwrap().as_ref(), b"new artifact");
}

#[test]
fn insufficient_reclaimable_space_preserves_every_existing_file() {
    let (root, _) = fixture();
    let old = Sha256::digest(b"small actual artifact");
    let incoming = Sha256::digest(b"large new artifact");
    let mut cache = SourceCache::open(&root,
        CacheLimits { ram_bytes: 0, disk_bytes: 1000, entries: 3 }).unwrap();
    cache.put_native(old, b"old").unwrap();
    let notes = root.join("notes.txt");
    private_test_file(&notes, &[7; 500]);
    // The new artifact alone fits, but retaining the unrelated file leaves too
    // little capacity. Preflight must refuse before deleting the one candidate.
    assert_eq!(cache.put_native(incoming, &[9; 600]), Err(CacheError::Limit));
    assert_eq!(cache.stats().evicted_entries, 0);
    assert_eq!(cache.get_native(old).unwrap().unwrap().as_ref(), b"old");
    assert_eq!(fs::read(notes).unwrap(), [7; 500]);
}

#[test]
fn repeated_source_edits_keep_the_newest_cache_usable_after_reopen() {
    let (root, source) = fixture();
    let limits = CacheLimits { ram_bytes: 0, disk_bytes: 10000, entries: 1 };
    let mut cache = SourceCache::open(&root, limits).unwrap();
    for version in 0..5 {
        fs::write(&source, format!("fn version_{version}() {{}}\n")).unwrap();
        let response = cache.source(&source, || false).unwrap().0;
        assert_eq!(response.as_str(), source_document::read(&source, || false).unwrap().as_str());
    }
    assert_eq!(cache.stats().evicted_entries, 4);
    assert_eq!(cache.stats().write_refusals, 0);
    drop(cache);
    let mut reopened = SourceCache::open(&root, limits).unwrap();
    assert_eq!(reopened.source(&source, || false).unwrap().0.as_str(),
        source_document::read(&source, || false).unwrap().as_str());
    assert_eq!(reopened.stats().lexer_calls, 0);
    assert_eq!(reopened.stats().disk_hits, 1);
}
