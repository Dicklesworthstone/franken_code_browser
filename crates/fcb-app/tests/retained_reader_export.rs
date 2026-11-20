#![forbid(unsafe_code)]
#![cfg(unix)]

//! Real create-only files and independently compared original bytes. These
//! tests exercise the retained host and shared publication engine, not a mock
//! encoder. Execution belongs to the independent strict-RCH verifier.
mod support;

use std::{fs, path::{Path, PathBuf}, os::unix::{ffi::OsStrExt, fs::PermissionsExt},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::ArenaOwnerId;
use fcb_app::{EXIT_OK, EXIT_ERROR, EXIT_CANCELED, host::{HostResponse,
    reader::{ReaderSession, ReaderSessionError, MAX_READER_SOURCE_BYTES}}};
use support::{Json, parse};

fn directory() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-reader-export-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&path).unwrap(); path
}
fn source(bytes: &[u8]) -> ReaderSession {
    ReaderSession::from_bytes(ArenaOwnerId::new(9951).unwrap(),
        Path::new("label-only/not-a-live-source"), bytes, || false).unwrap()
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn receipt(response: &HostResponse, reader: &ReaderSession, path: &Path,
    start: u64, end: u64) -> Json {
    let value = json(response);
    assert_eq!(value.get("schema").text(), "fcb.reader-export/1");
    assert_eq!(value.get("command").text(), "export-original");
    assert_eq!(value.get("owner").number(), reader.capture().request().file().owner().get());
    assert_eq!(value.get("file_id").number(), reader.capture().request().file().get());
    assert_eq!(value.get("source_revision").number(), reader.capture().request().revision().get());
    assert_eq!(value.get("capture_sha256").text(), reader.capture_sha256().to_hex());
    assert_eq!(value.get("captured_bytes").number(), reader.capture().bytes().len() as u64);
    assert_eq!(value.get("original_range").get("start").number(), start);
    assert_eq!(value.get("original_range").get("end").number(), end);
    assert_eq!(value.get("export_bytes").number(), end - start);
    assert!(value.get("bytes_written").number() <= end - start);
    assert_eq!(value.get("export_domain").text(), "original-bytes");
    assert_eq!(value.get("destination").get("encoding").text(), "unix-bytes");
    assert_eq!(value.get("destination").get("hex").text(), hex(path.as_os_str().as_bytes()));
    assert_eq!(value.get("additional_source_bytes_read").number(), 0);
    assert!(!value.get("overwritten").flag());
    assert!(!value.get("power_loss_qualified").flag());
    value
}

#[test]
fn full_maximum_capture_exports_beyond_clipboard_limits_without_truncation() {
    let mut bytes = vec![0u8; MAX_READER_SOURCE_BYTES];
    for (position, byte) in bytes.iter_mut().enumerate() { *byte = (position % 251) as u8; }
    let mut reader = source(&bytes);
    let output = directory().join("entire-original.bin");
    let response = reader.export_original(0, bytes.len() as u64, &output, || false).unwrap();
    let value = receipt(&response, &reader, &output, 0, bytes.len() as u64);
    assert_eq!(response.exit_code(), EXIT_OK);
    assert_eq!(value.get("status").text(), "ok");
    assert_eq!(value.get("effect").text(), "complete-file-and-parent-sync-requested");
    assert_eq!(value.get("bytes_written").number(), bytes.len() as u64);
    assert!(value.get("complete").flag()); assert_eq!(value.get("error"), &Json::Null);
    assert_eq!(fs::read(&output).unwrap(), bytes);
    assert_eq!(fs::metadata(&output).unwrap().permissions().mode() & 0o077, 0);
}

#[test]
fn arbitrary_original_ranges_preserve_bom_partial_units_invalid_bytes_and_crlf() {
    for bytes in [
        b"\xef\xbb\xbfA\r\n\xf0\x9f\xa6\x80\xff\0Z".to_vec(),
        vec![0xff, 0xfe, b'A', 0, 13, 0, 10, 0, 0x3e, 0xd8, 0x80, 0xdd, 0xff],
        vec![0xfe, 0xff, 0, b'A', 0, 13, 0, 10, 0xd8, 0x3e, 0xdd, 0x80, 0xff],
    ] {
        let mut reader = source(&bytes);
        for (start, end) in [(0, bytes.len()), (1, bytes.len() - 1), (3, 9), (0, 0)] {
            let output = directory().join("raw.bin");
            let response = reader.export_original(start as u64, end as u64, &output, || false).unwrap();
            let value = receipt(&response, &reader, &output, start as u64, end as u64);
            assert_eq!(response.exit_code(), EXIT_OK); assert!(value.get("complete").flag());
            assert_eq!(fs::read(output).unwrap(), bytes[start..end]);
        }
    }
}

#[test]
fn replacing_the_live_path_cannot_retarget_an_export_or_mutate_accepted_search() {
    let root = directory(); let live = root.join("live.rs");
    let bytes = b"old captured needle\r\n";
    fs::write(&live, bytes).unwrap();
    let mut reader = ReaderSession::open(ArenaOwnerId::new(9952).unwrap(), &live, 4096, || false).unwrap();
    reader.search(7, "needle", 4, 4096, || false).unwrap();
    fs::write(&live, b"entirely different current live bytes").unwrap();
    let output = root.join("selected.bin");
    let response = reader.export_original(4, 19, &output, || false).unwrap();
    receipt(&response, &reader, &output, 4, 19);
    assert_eq!(fs::read(output).unwrap(), &bytes[4..19]);
    assert_eq!(reader.accepted_generation(), Some(7));
    let hit = json(&reader.copy_hit(7, 0, || false).unwrap());
    assert_eq!(hit.get("original_hex").text(), hex(b"needle"));
}

#[test]
fn empty_capture_and_nonzero_empty_range_create_exact_empty_files() {
    for bytes in [b"".as_slice(), b"abc".as_slice()] {
        let mut reader = source(bytes); let at = bytes.len() as u64;
        let output = directory().join("empty.bin");
        let response = reader.export_original(at, at, &output, || false).unwrap();
        let value = receipt(&response, &reader, &output, at, at);
        assert_eq!(response.exit_code(), EXIT_OK); assert!(value.get("complete").flag());
        assert_eq!(value.get("bytes_written").number(), 0);
        assert_eq!(fs::metadata(output).unwrap().len(), 0);
    }
}

#[test]
fn invalid_ranges_and_nonabsolute_destinations_fail_before_creation() {
    let mut reader = source(b"abc"); let output = directory().join("must-not-exist");
    for (start, end) in [(2, 1), (0, 4), (u64::MAX, u64::MAX), (0, u64::MAX)] {
        assert_eq!(reader.export_original(start, end, &output, || false).err(),
            Some(ReaderSessionError::InvalidRange));
        assert!(!output.exists());
    }
    for path in [Path::new("relative-export.bin"), Path::new(""), Path::new("/")] {
        assert_eq!(reader.export_original(0, 3, path, || false).err(),
            Some(ReaderSessionError::InvalidLimits));
    }
}

#[test]
fn existing_regular_files_and_symlinks_are_never_overwritten() {
    let root = directory(); let destination = root.join("existing.bin");
    fs::write(&destination, b"keep this").unwrap();
    let link = root.join("link.bin"); std::os::unix::fs::symlink(&destination, &link).unwrap();
    let mut reader = source(b"new source");
    for output in [&destination, &link] {
        let response = reader.export_original(0, 10, output, || false).unwrap();
        let value = receipt(&response, &reader, output, 0, 10);
        assert_eq!(response.exit_code(), EXIT_ERROR);
        assert_eq!(value.get("effect").text(), "none");
        assert_eq!(value.get("bytes_written").number(), 0);
        assert!(!value.get("complete").flag());
        assert_eq!(value.get("error").get("code").text(), "READER_EXPORT_DESTINATION_EXISTS");
        assert_eq!(fs::read(&destination).unwrap(), b"keep this");
    }
}

#[test]
fn cancellation_receipts_distinguish_no_file_empty_created_prefix_and_complete_bytes() {
    let bytes: Vec<u8> = (0..(2 * 64 * 1024 + 17)).map(|i| (i % 251) as u8).collect();
    for threshold in [None, Some(0), Some(64 * 1024), Some(bytes.len() as u64)] {
        let mut reader = source(&bytes); let output = directory().join("canceled.bin");
        let response = reader.export_original(0, bytes.len() as u64, &output, || {
            match threshold {
                None => true,
                Some(size) => fs::metadata(&output).is_ok_and(|m| m.len() >= size),
            }
        }).unwrap();
        let value = receipt(&response, &reader, &output, 0, bytes.len() as u64);
        assert_eq!(response.exit_code(), EXIT_CANCELED);
        assert_eq!(value.get("status").text(), "error");
        assert_eq!(value.get("error").get("code").text(), "READER_EXPORT_CANCELED");
        let written = threshold.unwrap_or(0);
        assert_eq!(value.get("bytes_written").number(), written);
        match threshold {
            None => { assert!(!output.exists()); assert_eq!(value.get("effect").text(), "none"); }
            Some(size) => {
                assert_eq!(fs::read(&output).unwrap(), &bytes[..size as usize]);
                assert_eq!(value.get("effect").text(), if size == bytes.len() as u64 {
                    "complete-file-sync-unconfirmed"
                } else { "destination-created-incomplete" });
            }
        }
        assert_eq!(value.get("complete").flag(), threshold == Some(bytes.len() as u64));
    }
}

#[test]
fn cancellation_after_file_sync_retains_completed_bytes_and_sync_effect() {
    let bytes = b"source bytes"; let mut reader = source(bytes);
    let output = directory().join("synced.bin"); let mut complete_polls = 0;
    let response = reader.export_original(0, bytes.len() as u64, &output, || {
        if fs::metadata(&output).is_ok_and(|m| m.len() == bytes.len() as u64) {
            complete_polls += 1;
            return complete_polls == 2;
        }
        false
    }).unwrap();
    let value = receipt(&response, &reader, &output, 0, bytes.len() as u64);
    assert_eq!(response.exit_code(), EXIT_CANCELED);
    assert_eq!(value.get("effect").text(), "complete-file-sync-requested");
    assert!(value.get("complete").flag());
    assert_eq!(fs::read(output).unwrap(), bytes);
}

#[test]
fn missing_parent_returns_a_known_none_effect_and_reversible_nonunicode_destination() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let mut reader = source(b"data"); let root = directory();
    let output = root.join("absent").join("file.bin");
    let response = reader.export_original(0, 4, &output, || false).unwrap();
    let value = receipt(&response, &reader, &output, 0, 4);
    assert_eq!(response.exit_code(), EXIT_ERROR);
    assert_eq!(value.get("effect").text(), "none");
    assert_eq!(value.get("error").get("code").text(), "READER_EXPORT_IO");
    assert!(!output.exists());
    let output = root.join(OsString::from_vec(b"raw-\xff-\n.bin".to_vec()));
    let response = reader.export_original(0, 4, &output, || false).unwrap();
    receipt(&response, &reader, &output, 0, 4);
    assert_eq!(response.exit_code(), EXIT_OK);
    assert_eq!(fs::read(output).unwrap(), b"data");
}
