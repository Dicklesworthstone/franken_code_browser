#![forbid(unsafe_code)]

use super::*;
use std::{fs, path::PathBuf, sync::atomic::AtomicU64};

fn destination() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-reader-export-registry-{}-{}-{}",
        std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir_all(&root).unwrap(); root.join("original.bin")
}

#[test]
fn cancel_epoch_during_write_keeps_the_actual_prefix_receipt_and_reader_query() {
    let sessions = ReaderSessions::new(); let handle = sessions.create().unwrap();
    let bytes = vec![b'x'; 3 * 64 * 1024 + 1];
    sessions.supply(handle, &bytes).unwrap();
    sessions.execute(handle, Command::Find { generation: 3, needle: "absent",
        limit: 1, scan_bytes: bytes.len() as u64 }, || false).unwrap();
    let output = destination(); let mut canceled = false;
    let response = sessions.export_original(handle, 0, bytes.len() as u64, &output, || {
        if !canceled && fs::metadata(&output).is_ok_and(|m| m.len() >= 64 * 1024) {
            canceled = true; sessions.cancel(handle).unwrap();
        }
        false
    }).unwrap();
    assert!(canceled);
    let written = fs::read(&output).unwrap();
    assert!(written.len() >= 64 * 1024 && written.len() < bytes.len());
    assert_eq!(written, bytes[..written.len()]);
    assert!(response.as_str().contains("READER_EXPORT_CANCELED"));
    assert!(response.as_str().contains("\"effect\":\"destination-created-incomplete\""));
    assert!(response.as_str().contains(&format!("\"bytes_written\":\"{}\"", written.len())));
    assert!(response.as_str().contains(&format!("\"owner\":\"{handle}\"")));
    let info = sessions.execute(handle, Command::Info, || false).unwrap();
    assert!(info.as_str().contains("\"accepted_query_generation\":\"3\""));
    sessions.close(handle).unwrap();
}

#[test]
fn close_during_publication_keeps_receipt_alive_and_releases_only_its_slot() {
    let sessions = ReaderSessions::new();
    let handle = sessions.create().unwrap(); let neighbor = sessions.create().unwrap();
    let bytes = vec![b'y'; 3 * 64 * 1024 + 1];
    sessions.supply(handle, &bytes).unwrap(); sessions.supply(neighbor, b"neighbor").unwrap();
    let output = destination(); let mut closed = false;
    let response = sessions.export_original(handle, 0, bytes.len() as u64, &output, || {
        if !closed && fs::metadata(&output).is_ok_and(|m| m.len() >= 64 * 1024) {
            closed = true; sessions.close(handle).unwrap();
            // The retiring in-flight capture still owns its admission permit.
            assert_eq!(sessions.live.load(Ordering::Acquire), 2);
        }
        false
    }).unwrap();
    assert!(closed); assert!(response.as_str().contains("READER_EXPORT_CANCELED"));
    let written = fs::read(&output).unwrap();
    assert!(written.len() < bytes.len());
    assert_eq!(written, bytes[..written.len()]);
    assert!(response.as_str().contains(&format!("\"bytes_written\":\"{}\"", written.len())));
    assert_eq!(sessions.live.load(Ordering::Acquire), 1);
    assert_eq!(sessions.execute(handle, Command::Info, || false).err(), Some(AccessError::UnknownHandle));
    assert!(sessions.execute(neighbor, Command::CopyRange { start: 0, end: 8 }, || false)
        .unwrap().as_str().contains("\"original_hex\":\"6e65696768626f72\""));
    sessions.close(neighbor).unwrap();
}

#[test]
fn late_cancel_after_last_byte_is_a_complete_effect_not_a_lost_result() {
    let sessions = ReaderSessions::new(); let handle = sessions.create().unwrap();
    let bytes = b"accepted immutable capture"; sessions.supply(handle, bytes).unwrap();
    let output = destination(); let mut canceled = false;
    let response = sessions.export_original(handle, 0, bytes.len() as u64, &output, || {
        if !canceled && fs::metadata(&output).is_ok_and(|m| m.len() == bytes.len() as u64) {
            canceled = true; sessions.cancel(handle).unwrap();
            return true;
        }
        false
    }).unwrap();
    assert!(response.as_str().contains("\"complete\":true"), "{}", response.as_str());
    assert!(response.as_str().contains("\"effect\":\"complete-file-sync-unconfirmed\""));
    assert!(response.as_str().contains("READER_EXPORT_CANCELED"));
    assert_eq!(fs::read(output).unwrap(), bytes);
    sessions.close(handle).unwrap();
}

#[test]
fn busy_unopened_and_foreign_handles_cannot_publish_a_destination() {
    let sessions = ReaderSessions::new(); let handle = sessions.create().unwrap();
    let unopened = destination();
    assert_eq!(sessions.export_original(handle, 0, 0, &unopened, || false).err(), Some(AccessError::NotOpen));
    assert!(!unopened.exists());
    sessions.supply(handle, b"retained").unwrap();
    let foreign = ReaderSessions::new(); let denied = destination();
    assert_eq!(foreign.export_original(handle, 0, 8, &denied, || false).err(), Some(AccessError::UnknownHandle));
    let output = destination(); let mut probed = false;
    let response = sessions.export_original(handle, 0, 8, &output, || {
        if !probed {
            probed = true;
            assert_eq!(sessions.export_original(handle, 0, 8, &denied, || false).err(), Some(AccessError::Busy));
        }
        false
    }).unwrap();
    assert!(probed); assert!(response.as_str().contains("\"status\":\"ok\""));
    assert_eq!(fs::read(output).unwrap(), b"retained"); assert!(!denied.exists());
    sessions.close(handle).unwrap();
}
