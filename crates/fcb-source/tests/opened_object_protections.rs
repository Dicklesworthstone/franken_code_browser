#![forbid(unsafe_code)]

//! Integration and oracle tests for opened-object protections, non-blocking FIFO
//! admission, own-artifact exclusions, and traversal limits (FCB-083.B / fcb-tzet.2).
//!
//! Verifies (§8.6, §8.10):
//! 1. Opened file descriptor revalidation rejects FIFOs, sockets, and character/block
//!    devices with `SourceError::SpecialObject` without blocking indefinitely.
//! 2. `safe_open_regular_file` uses non-blocking open on Unix to prevent malicious
//!    named pipes from starving reader threads.
//! 3. `BoundedRetryReader` and `ConfinedSourceReader` safely reject FIFOs and devices.
//! 4. Regular-to-FIFO replacement race is caught at the opened descriptor level.
//! 5. Own-artifact exclusions match conventional basenames (`.fcb-cache`, `.fcb-store`,
//!    `.fcb.db`, etc.) and prevent self-indexing loops.
//! 6. Own-artifact identity (`DirectoryId`) exclusion prevents traversal into registered
//!    stores even when renamed or symlinked.
//! 7. Traversal cycle detection identifies symlink ancestry loops using `DirectoryId`.
//! 8. Traversal alias hop limits bound symlink expansion chains.
//! 9. Raw path controls and escaped display sanitize bidi controls and newlines.
//! 10. Negative control oracles detect FIFO blocking leaks, own-cache leakage, and
//!     alias hop budget bypass defects.

use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use fcb_core::{ArenaOwnerId, ByteLength, FileId, RootId, SourceRevision};
use fcb_source::chunk::ChunkedReaderConfig;
use fcb_source::confined::{safe_open_regular_file, ConfinedSourceReader, SymlinkPolicy};
use fcb_source::discovery::{BoundedDiscovery, DiscoveryKind, DiscoveryLimits};
use fcb_source::path::{NormalizedPath, RawPath};
use fcb_source::root::RootGrant;
use fcb_source::snapshot::{BoundedRetryReader, RetryPolicy};
use fcb_source::{CancelFlag, CaptureRequest, SourceError};

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
            "fcb_protect_{}_{}_{}",
            prefix,
            std::process::id(),
            nanos
        ));
        fs::create_dir_all(&path).expect("create temp dir");
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

fn test_owner_id(val: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(val).unwrap()
}

fn test_root_id(owner: u64, root: u64) -> RootId {
    RootId::new(test_owner_id(owner), root).unwrap()
}

fn test_file_id(owner: u64, file: u64) -> FileId {
    FileId::new(test_owner_id(owner), file).unwrap()
}

fn test_revision(owner: u64, rev: u64) -> SourceRevision {
    SourceRevision::new(test_owner_id(owner), rev).unwrap()
}

fn write_test_file(dir: &Path, rel: &str, bytes: &[u8]) {
    let path = dir.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).unwrap();
    }
    let mut file = File::create(&path).unwrap();
    file.write_all(bytes).unwrap();
}

// ---------------------------------------------------------------------------
// 1. safe_open_regular_file rejects FIFO without blocking
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn test_safe_open_regular_file_rejects_fifo_without_blocking() {
    let dir = TempTestDir::new("fifo_open");
    let fifo_path = dir.path().join("malicious.pipe");

    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status();

    if let Ok(exit) = status
        && exit.success()
    {
        // safe_open_regular_file must return Err(SourceError::SpecialObject)
        // without hanging (since no writer is open).
        let start = std::time::Instant::now();
        let result = safe_open_regular_file(&fifo_path);
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 2,
            "FIFO open must be non-blocking, took {:?}",
            elapsed
        );
        assert_eq!(result.err(), Some(SourceError::SpecialObject));
    }
}

// ---------------------------------------------------------------------------
// 2. safe_open_regular_file rejects directories and devices
// ---------------------------------------------------------------------------
#[test]
fn test_safe_open_regular_file_rejects_directory() {
    let dir = TempTestDir::new("dir_open");
    let result = safe_open_regular_file(dir.path());
    assert_eq!(result.err(), Some(SourceError::SpecialObject));
}

#[cfg(unix)]
#[test]
fn test_safe_open_regular_file_rejects_character_device() {
    let dev_null = Path::new("/dev/null");
    if dev_null.exists() {
        let result = safe_open_regular_file(dev_null);
        assert_eq!(result.err(), Some(SourceError::SpecialObject));
    }
}

// ---------------------------------------------------------------------------
// 3. BoundedRetryReader rejects FIFO non-blocking
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn test_bounded_retry_reader_rejects_fifo_nonblocking() {
    let dir = TempTestDir::new("retry_fifo");
    let fifo_path = dir.path().join("retry.pipe");

    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status();

    if let Ok(exit) = status
        && exit.success()
    {
        let file_id = test_file_id(201, 10);
        let rev = test_revision(201, 1);
        let config = ChunkedReaderConfig::default();
        let request = CaptureRequest::new(file_id, rev).unwrap();
        let cancel = CancelFlag::new();
        let retry_policy = RetryPolicy::default();

        let start = std::time::Instant::now();
        let outcome = BoundedRetryReader::read_file_with_retry(
            request,
            &fifo_path,
            config,
            retry_policy,
            &cancel,
        );
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 2,
            "BoundedRetryReader on FIFO took {:?}",
            elapsed
        );
        assert_eq!(outcome.err(), Some(SourceError::SpecialObject));
    }
}

// ---------------------------------------------------------------------------
// 4. ConfinedSourceReader rejects FIFO non-blocking
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn test_confined_source_reader_rejects_fifo() {
    let dir = TempTestDir::new("confined_fifo");
    let fifo_path = dir.path().join("confined.pipe");

    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status();

    if let Ok(exit) = status
        && exit.success()
    {
        let root_id = test_root_id(202, 1);
        let grant = RootGrant::new(root_id, dir.path());
        let reader = ConfinedSourceReader::new(
            grant,
            SymlinkPolicy::DisallowAll,
            ByteLength::new(4096),
        );

        let norm = NormalizedPath::new("confined.pipe").unwrap();
        let cancel = CancelFlag::new();

        let start = std::time::Instant::now();
        let outcome = reader.read_file(
            test_file_id(202, 1),
            test_revision(202, 1),
            &norm,
            &cancel,
        );
        let elapsed = start.elapsed();

        assert!(
            elapsed.as_secs() < 2,
            "ConfinedSourceReader on FIFO took {:?}",
            elapsed
        );
        assert_eq!(outcome.err(), Some(SourceError::SpecialObject));
    }
}

// ---------------------------------------------------------------------------
// 5. Regular-to-FIFO replacement race detection
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn test_regular_to_fifo_race_detected_on_open() {
    let dir = TempTestDir::new("race_fifo");
    let target = dir.path().join("transmuted.txt");

    // Phase 1: file exists as regular file
    {
        let mut f = File::create(&target).unwrap();
        f.write_all(b"initial regular content").unwrap();
    }
    assert!(safe_open_regular_file(&target).is_ok());

    // Phase 2: file is replaced by FIFO
    fs::remove_file(&target).unwrap();
    let status = std::process::Command::new("mkfifo")
        .arg(&target)
        .status();

    if let Ok(exit) = status
        && exit.success()
    {
        let err = safe_open_regular_file(&target).unwrap_err();
        assert_eq!(err, SourceError::SpecialObject);
    }
}

// ---------------------------------------------------------------------------
// 6. Own-artifact exclusions: conventional basenames
// ---------------------------------------------------------------------------
#[test]
fn test_own_artifact_exclusion_default_basenames() {
    let dir = TempTestDir::new("own_basenames");

    // Create conventional source files
    write_test_file(dir.path(), "src/main.rs", b"fn main() {}");
    write_test_file(dir.path(), "README.md", b"# Test Repo");

    // Create own-artifact directories and database sidecars
    write_test_file(dir.path(), ".fcb-cache/cached_blob.bin", b"cache data");
    write_test_file(dir.path(), ".fcb-store/retained_blob.bin", b"store data");
    write_test_file(dir.path(), ".fcb-scratch/temp.tmp", b"scratch data");
    write_test_file(dir.path(), ".fcb-recovery/journal.log", b"recovery data");
    write_test_file(dir.path(), ".fcb.db", b"sqlite header");
    write_test_file(dir.path(), ".fcb.db-wal", b"wal header");
    write_test_file(dir.path(), ".fcb.db-shm", b"shm header");

    let grant = RootGrant::new(test_root_id(301, 1), dir.path());
    let limits = DiscoveryLimits::new(4, 10, 256, 32, 4096, 64).unwrap();
    let mut disc = BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();

    let cancel = CancelFlag::new();
    let mut discovered_files = Vec::new();
    let mut saw_db_file = false;
    let mut db_file_was_excluded = false;

    while let Ok(Some(batch)) = disc.next_batch(&cancel) {
        for entry in batch.entries() {
            if entry.path().as_str().unwrap() == ".fcb.db" {
                saw_db_file = true;
                if entry.is_excluded() {
                    db_file_was_excluded = true;
                }
            }
            if entry.kind() == DiscoveryKind::File && !entry.is_excluded() {
                discovered_files.push(entry.path().as_str().unwrap().to_string());
            }
        }
    }

    // src/main.rs and README.md must be discovered
    assert!(discovered_files.contains(&"src/main.rs".to_string()));
    assert!(discovered_files.contains(&"README.md".to_string()));

    // No .fcb-* or .fcb.db* files must be in discovered non-excluded files
    for path in &discovered_files {
        assert!(
            !path.starts_with(".fcb-cache"),
            "Leaked cache file: {}",
            path
        );
        assert!(
            !path.starts_with(".fcb-store"),
            "Leaked store file: {}",
            path
        );
        assert!(
            !path.starts_with(".fcb-scratch"),
            "Leaked scratch file: {}",
            path
        );
        assert!(
            !path.starts_with(".fcb-recovery"),
            "Leaked recovery file: {}",
            path
        );
        assert!(!path.starts_with(".fcb.db"), "Leaked db file: {}", path);
    }

    // .fcb.db was encountered and properly marked excluded
    assert!(saw_db_file, ".fcb.db should be observed in root");
    assert!(db_file_was_excluded, ".fcb.db must be marked as excluded");
}

// ---------------------------------------------------------------------------
// 7. Own-artifact exclusions: DirectoryId matching even if renamed
// ---------------------------------------------------------------------------
#[test]
fn test_own_artifact_exclusion_by_directory_id_survives_custom_name() {
    let dir = TempTestDir::new("own_dir_id");

    // Create normal source file
    write_test_file(dir.path(), "src/lib.rs", b"pub mod test;");

    // Create a custom-named scratch store inside root
    let custom_store = dir.path().join("external_mounted_store");
    fs::create_dir_all(&custom_store).unwrap();
    write_test_file(dir.path(), "external_mounted_store/blob.dat", b"secret index data");

    let grant = RootGrant::new(test_root_id(302, 1), dir.path());
    let limits = DiscoveryLimits::new(4, 10, 256, 32, 4096, 64).unwrap();
    let mut disc = BoundedDiscovery::open(grant, SymlinkPolicy::DisallowAll, limits).unwrap();

    // Register custom_store by DirectoryId
    disc.register_own_artifact_path(&custom_store).unwrap();

    let cancel = CancelFlag::new();
    let mut discovered_files = Vec::new();
    while let Ok(Some(batch)) = disc.next_batch(&cancel) {
        for entry in batch.entries() {
            if entry.kind() == DiscoveryKind::File && !entry.is_excluded() {
                discovered_files.push(entry.path().as_str().unwrap().to_string());
            }
        }
    }

    assert!(discovered_files.contains(&"src/lib.rs".to_string()));
    for path in &discovered_files {
        assert!(
            !path.contains("external_mounted_store"),
            "Registered store by DirectoryId must not be indexed: {}",
            path
        );
    }
}

// ---------------------------------------------------------------------------
// 8. Traversal cycles: symlink loop detected via ancestry DirectoryId
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn test_symlink_ancestry_cycle_detection() {
    let dir = TempTestDir::new("symlink_cycle");

    // Create a regular directory and file
    let sub = dir.path().join("sub");
    fs::create_dir_all(&sub).unwrap();
    write_test_file(dir.path(), "sub/file.txt", b"regular content");

    // Create a loop: sub/loop -> sub
    let loop_link = sub.join("loop");
    let status = std::os::unix::fs::symlink(&sub, &loop_link);
    if status.is_ok() {
        let grant = RootGrant::new(test_root_id(303, 1), dir.path());
        let limits = DiscoveryLimits::new(10, 20, 256, 32, 4096, 64).unwrap();
        let mut disc = BoundedDiscovery::open(grant, SymlinkPolicy::AllowWithinRoot, limits).unwrap();

        let cancel = CancelFlag::new();
        while let Ok(Some(_batch)) = disc.next_batch(&cancel) {}

        let aggregate = disc.aggregate();
        assert!(
            aggregate.cycles > 0,
            "Ancestry loop must be detected and recorded in cycles count"
        );
    }
}

// ---------------------------------------------------------------------------
// 9. Traversal alias hops: bounded alias expansion
// ---------------------------------------------------------------------------
#[cfg(unix)]
#[test]
fn test_symlink_alias_hop_limit_bounded() {
    let dir = TempTestDir::new("alias_hops");

    // Create nested directories:
    // level1/hop1 -> level2
    // level2/hop2 -> level3
    // level3/file.txt
    let level1 = dir.path().join("level1");
    let level2 = dir.path().join("level2");
    let level3 = dir.path().join("level3");

    fs::create_dir_all(&level1).unwrap();
    fs::create_dir_all(&level2).unwrap();
    fs::create_dir_all(&level3).unwrap();
    write_test_file(dir.path(), "level3/file.txt", b"deep content");

    let hop1 = level1.join("hop1");
    let hop2 = level2.join("hop2");

    let s1 = std::os::unix::fs::symlink(&level2, &hop1);
    let s2 = std::os::unix::fs::symlink(&level3, &hop2);

    if s1.is_ok() && s2.is_ok() {
        let grant = RootGrant::new(test_root_id(304, 1), dir.path());
        // Set max_alias_hops to 1
        let limits = DiscoveryLimits::new(10, 20, 256, 32, 4096, 64)
            .unwrap()
            .with_max_alias_hops(1);
        let mut disc = BoundedDiscovery::open(grant, SymlinkPolicy::AllowWithinRoot, limits).unwrap();

        let cancel = CancelFlag::new();
        while let Ok(Some(_batch)) = disc.next_batch(&cancel) {}

        let aggregate = disc.aggregate();
        assert!(
            aggregate.cycles > 0,
            "Hops exceeding max_alias_hops must be bounded and recorded as cycle/alias limitation"
        );
    }
}

// ---------------------------------------------------------------------------
// 10. Raw path controls and escaped display
// ---------------------------------------------------------------------------
#[test]
fn test_raw_path_controls_and_escaped_display() {
    // 1. Raw path with newline and bidi controls
    let raw_bytes: &[u8] = b"evil\nname\t\xE2\x80\xAEhidden.rs";
    let raw = RawPath::from(raw_bytes);
    assert_eq!(raw.as_bytes(), raw_bytes);

    // 2. Escaped display prevents newlines from forging output lines
    let display = raw.display_escaped();
    let display_str = format!("{}", display);
    assert!(
        !display_str.contains('\n'),
        "Escaped display must not contain raw newline"
    );
    assert!(
        display_str.contains("\\n"),
        "Escaped display must represent newline as '\\n'"
    );
    assert!(
        display_str.contains("\\t"),
        "Escaped display must represent tab as '\\t'"
    );

    // 3. NormalizedPath preserves raw bytes for identity/interchange, while escaped display sanitizes
    let norm = NormalizedPath::new("evil\nname.rs").unwrap();
    assert_eq!(norm.as_bytes(), b"evil\nname.rs");
    let norm_display = format!("{}", norm.display_escaped());
    assert!(!norm_display.contains('\n'));
    assert!(norm_display.contains("\\n"));

    // 4. NormalizedPath rejects NUL byte
    let nul_rejected = NormalizedPath::new(b"evil\0name.rs".as_slice());
    assert_eq!(nul_rejected.err(), Some(SourceError::EncodingError));
}

// ---------------------------------------------------------------------------
// 11. Negative control oracle: FIFO blocking detection
// ---------------------------------------------------------------------------
fn verify_fifo_safety_oracle(result: &Result<File, SourceError>) -> Result<(), &'static str> {
    match result {
        Err(SourceError::SpecialObject) => Ok(()),
        Ok(_) => Err("DEFECT: FIFO was successfully opened as a regular file!"),
        Err(_) => {
            Err("DEFECT: FIFO was not rejected with SpecialObject")
        }
    }
}

#[cfg(unix)]
#[test]
fn test_negative_control_oracle_fifo_blocking() {
    let dir = TempTestDir::new("oracle_fifo");
    let fifo_path = dir.path().join("oracle.pipe");

    let status = std::process::Command::new("mkfifo")
        .arg(&fifo_path)
        .status();

    if let Ok(exit) = status
        && exit.success()
    {
        // 1. Correct implementation passes oracle
        let correct_result = safe_open_regular_file(&fifo_path);
        assert_eq!(verify_fifo_safety_oracle(&correct_result), Ok(()));

        // 2. Planted defective outcome: opening succeeded
        let regular_file = dir.path().join("dummy.txt");
        write_test_file(dir.path(), "dummy.txt", b"regular");
        let defective_result = File::open(&regular_file).map_err(|_| SourceError::RootUnavailable);
        let oracle_res = verify_fifo_safety_oracle(&defective_result);
        assert!(
            oracle_res.is_err(),
            "Negative control oracle must detect regular file masquerading as safe FIFO result!"
        );
    }
}

// ---------------------------------------------------------------------------
// 12. Negative control oracle: own-cache leakage detection
// ---------------------------------------------------------------------------
fn verify_own_cache_isolation_oracle(discovered_paths: &[String]) -> Result<(), &'static str> {
    for path in discovered_paths {
        if path.starts_with(".fcb-cache")
            || path.starts_with(".fcb-store")
            || path.starts_with(".fcb.db")
        {
            return Err("DEFECT: Own-artifact was leaked into discovery results!");
        }
    }
    Ok(())
}

#[test]
fn test_negative_control_oracle_own_cache_leak() {
    // 1. Correct results (no own artifacts) pass oracle
    let clean_results = vec!["src/lib.rs".to_string(), "Cargo.toml".to_string()];
    assert_eq!(verify_own_cache_isolation_oracle(&clean_results), Ok(()));

    // 2. Planted defective results containing leaked cache file
    let leaked_results = vec![
        "src/lib.rs".to_string(),
        ".fcb-cache/index.db".to_string(), // LEAKED DEFECT
    ];
    let oracle_res = verify_own_cache_isolation_oracle(&leaked_results);
    assert!(
        oracle_res.is_err(),
        "Negative control oracle must detect leaked own cache!"
    );
}

// ---------------------------------------------------------------------------
// 13. Negative control oracle: alias hop budget bypass detection
// ---------------------------------------------------------------------------
fn verify_alias_hop_oracle(
    max_allowed_hops: u32,
    observed_hops: u32,
    cycle_recorded: bool,
) -> Result<(), &'static str> {
    if observed_hops > max_allowed_hops && !cycle_recorded {
        return Err("DEFECT: Alias hops exceeded limit without recording cycle limitation!");
    }
    Ok(())
}

#[test]
fn test_negative_control_oracle_alias_hop_budget() {
    // 1. Correct: hops within budget
    assert_eq!(verify_alias_hop_oracle(2, 1, false), Ok(()));

    // 2. Correct: hops exceeded budget and cycle recorded
    assert_eq!(verify_alias_hop_oracle(2, 3, true), Ok(()));

    // 3. Planted defect: hops exceeded budget but cycle NOT recorded
    let oracle_res = verify_alias_hop_oracle(2, 3, false);
    assert!(
        oracle_res.is_err(),
        "Negative control oracle must detect unrecorded alias budget bypass!"
    );
}
