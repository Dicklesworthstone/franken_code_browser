#![forbid(unsafe_code)]

use super::*;
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Fixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let path = std::env::temp_dir().join(format!("fcb-stream-registry-{}-{}-{}",
            std::process::id(), std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        fs::write(path.join("source.rs"), b"old needle\n").unwrap();
        Self(path)
    }
    fn open(&self, registry: &AtlasSessions) -> u64 {
        let handle = registry.create().unwrap();
        registry.open(handle, &self.0, Default::default(), || false).unwrap();
        handle
    }
}
impl Drop for Fixture { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }

fn search<'a>(needle: &'a str) -> StreamCommand<'a> {
    StreamCommand::Search { needle, options: AtlasStreamSearchOptions::default() }
}

#[test]
fn canceling_entered_stream_is_nonblocking_and_preserves_accepted_literal() {
    let fixture = Fixture::new();
    let registry = AtlasSessions::new();
    let handle = fixture.open(&registry);
    let independent = fixture.open(&registry);
    let accepted = registry.execute_stream(handle, search("needle"), || false).unwrap();
    assert!(accepted.as_str().contains("\"query_generation\":\"1\""));
    let mut entered = false;
    let canceled = registry.execute_stream(handle, search("replacement"), || {
        if !entered {
            entered = true;
            assert!(matches!(registry.execute_stream(handle,
                StreamCommand::Page { generation: 1, start: 0, count: 1 }, || false),
                Err(AccessError::Busy)));
            assert!(registry.execute(independent, Command::Info, || false).is_ok());
            registry.cancel(handle).unwrap();
        }
        false
    });
    assert!(entered);
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    // No source read is needed to activate the previous terminal witness.
    fs::rename(fixture.0.join("source.rs"), fixture.0.join("retired.rs")).unwrap();
    let retained = registry.execute_stream(handle,
        StreamCommand::Hit { generation: 1, position: 0 }, || false).unwrap();
    assert!(retained.as_str().contains("\"original_hex\":\"6e6565646c65\""));
    assert!(retained.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    assert!(registry.execute_stream(independent,
        StreamCommand::Hit { generation: 1, position: 0 }, || false).is_err());
    registry.close(handle).unwrap();
    registry.close(independent).unwrap();
}

#[test]
fn close_during_stream_holds_capacity_until_source_worker_drains() {
    let fixture = Fixture::new();
    let registry = AtlasSessions::new();
    let handle = fixture.open(&registry);
    let other: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| registry.create().unwrap()).collect();
    let mut entered = false;
    let closed = registry.execute_stream(handle, search("needle"), || {
        if !entered {
            entered = true;
            registry.close(handle).unwrap();
            assert_eq!(registry.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS);
            assert!(matches!(registry.create(), Err(AccessError::Capacity)));
        }
        false
    });
    assert!(entered);
    assert!(matches!(closed, Err(AccessError::Closed)));
    assert_eq!(registry.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS - 1);
    let next = registry.create().unwrap();
    assert_ne!(next, handle);
    registry.close(next).unwrap();
    for id in other { registry.close(id).unwrap(); }
    assert_eq!(registry.live.load(Ordering::Acquire), 0);
    assert_eq!(registry.budget.accounting().reserved().get(), 0);
}

#[test]
fn streaming_and_complete_capture_queries_keep_distinct_retained_state() {
    let fixture = Fixture::new();
    let registry = AtlasSessions::new();
    let handle = fixture.open(&registry);
    registry.execute(handle, Command::Search { generation: 1, needle: "needle",
        options: AtlasSearchOptions::default() }, || false).unwrap();
    registry.execute_stream(handle, search("old"), || false).unwrap();
    registry.execute_stream(handle, StreamCommand::Clear, || false).unwrap();
    assert!(registry.execute_stream(handle,
        StreamCommand::Hit { generation: 1, position: 0 }, || false).is_err());
    let complete = registry.execute(handle,
        Command::SearchPage { generation: 1, start: 0, limit: 1 }, || false).unwrap();
    assert!(complete.as_str().contains("\"retained_hits\":\"1\""));
    assert!(complete.as_str().contains("needle"));
    registry.close(handle).unwrap();
}


#[test]
fn progress_holds_own_permit_without_blocking_other_sessions_and_cancel_revokes_delivery() {
    let fixture = Fixture::new();
    let registry = AtlasSessions::new();
    let handle = fixture.open(&registry);
    let independent = fixture.open(&registry);
    registry.execute_stream(handle, search("old"), || false).unwrap();
    let mut callbacks = 0;
    let canceled = registry.execute_stream_with_progress(handle, "needle",
        AtlasStreamSearchOptions::default(), || false, || false, |response| {
            callbacks += 1;
            assert!(response.as_str().contains("\"selection_available\":false"));
            assert!(matches!(registry.execute_stream(handle,
                StreamCommand::Page { generation: 1, start: 0, count: 1 }, || false),
                Err(AccessError::Busy)));
            assert!(registry.execute(independent, Command::Info, || false).is_ok());
            registry.cancel(handle).unwrap();
        });
    assert_eq!(callbacks, 1);
    assert!(matches!(canceled, Err(AccessError::Canceled)));
    let retained = registry.execute_stream(handle,
        StreamCommand::Hit { generation: 1, position: 0 }, || false).unwrap();
    assert!(retained.as_str().contains("\"original_hex\":\"6f6c64\""));
    registry.close(handle).unwrap();
    registry.close(independent).unwrap();
}

#[test]
fn close_from_progress_retains_capacity_until_entered_call_drains() {
    let fixture = Fixture::new();
    let registry = AtlasSessions::new();
    let handle = fixture.open(&registry);
    let others: Vec<_> = (1..MAX_ATLAS_SESSIONS).map(|_| registry.create().unwrap()).collect();
    let mut callbacks = 0;
    let closed = registry.execute_stream_with_progress(handle, "needle",
        AtlasStreamSearchOptions::default(), || false, || false, |_| {
            callbacks += 1;
            registry.close(handle).unwrap();
            assert_eq!(registry.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS);
            assert!(matches!(registry.create(), Err(AccessError::Capacity)));
        });
    assert_eq!(callbacks, 1);
    assert!(matches!(closed, Err(AccessError::Closed)));
    assert_eq!(registry.live.load(Ordering::Acquire), MAX_ATLAS_SESSIONS - 1);
    for id in others { registry.close(id).unwrap(); }
    assert_eq!(registry.live.load(Ordering::Acquire), 0);
    assert_eq!(registry.budget.accounting().reserved().get(), 0);
}
