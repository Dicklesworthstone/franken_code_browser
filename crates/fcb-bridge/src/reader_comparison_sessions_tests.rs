#![forbid(unsafe_code)]

use super::*;

fn pair(sessions: &ReaderSessions) -> [u64; 2] {
    let before = sessions.create().unwrap(); let after = sessions.create().unwrap();
    sessions.supply(before, b"before retained").unwrap();
    sessions.supply(after, b"after retained").unwrap();
    [before, after]
}

#[test]
fn pair_comparison_borrows_two_exact_handles_and_keeps_query_authority_separate() {
    let sessions = ReaderSessions::new(); let [before, after] = pair(&sessions);
    sessions.execute(before, Command::Find { generation: 9, needle: "before", limit: 16, scan_bytes: 4096 }, || false).unwrap();
    let result = sessions.compare(before, after, 1, ReaderComparisonLimits::default(), || false).unwrap();
    assert!(result.as_str().contains("\"comparison_complete\":true"));
    assert!(result.as_str().contains(&format!("\"after_source\":{{\"owner\":\"{after}\"")));
    assert!(sessions.execute(before, Command::CopyHit { generation: 9, index: 0 }, || false).is_ok());
    assert_eq!(sessions.execute(after, Command::CopyHit { generation: 9, index: 0 }, || false).err(),
        Some(AccessError::Reader(ReaderSessionError::MissingHit)));
    assert_eq!(sessions.compare(before, before, 2, ReaderComparisonLimits::default(), || false).err(), Some(AccessError::InvalidArgument));
    assert_eq!(sessions.compare(before, 0, 2, ReaderComparisonLimits::default(), || false).err(), Some(AccessError::UnknownHandle));
    sessions.close(before).unwrap(); sessions.close(after).unwrap();
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
    assert!(result.as_str().contains("\"source_reopened\":false"));
}

#[test]
fn either_busy_cell_refuses_without_waiting_or_consuming_the_generation() {
    let sessions = ReaderSessions::new(); let [before, after] = pair(&sessions);
    for handle in [before, after] {
        let cell = sessions.get(handle).unwrap(); let guard = cell.state.lock().unwrap();
        assert_eq!(sessions.compare(before, after, 1, ReaderComparisonLimits::default(), || false).err(), Some(AccessError::Busy));
        drop(guard);
    }
    assert!(sessions.compare(before, after, 1, ReaderComparisonLimits::default(), || false).is_ok());
    sessions.close(before).unwrap(); sessions.close(after).unwrap();
}

#[test]
fn cancellation_of_either_capture_aborts_and_its_next_generation_can_retry() {
    for side in [0, 1] {
        let sessions = ReaderSessions::new(); let handles = pair(&sessions);
        let [before, after] = handles;
        let mut once = false;
        let result = sessions.compare(before, after, 1, ReaderComparisonLimits::default(), || {
            if !once { once = true; sessions.cancel(handles[side]).unwrap(); }
            false
        });
        assert!(result.err().expect("cancellation must suppress output").canceled());
        assert!(sessions.compare(before, after, 2, ReaderComparisonLimits::default(), || false).is_ok());
        sessions.close(before).unwrap(); sessions.close(after).unwrap();
    }
}

#[test]
fn closure_during_comparison_keeps_both_admissions_until_the_worker_drains() {
    for side in [0, 1] {
        let sessions = ReaderSessions::new(); let handles = pair(&sessions);
        let [before, after] = handles;
        let mut once = false;
        let result = sessions.compare(before, after, 1, ReaderComparisonLimits::default(), || {
            if !once {
                once = true; sessions.close(handles[side]).unwrap();
                assert_eq!(sessions.live.load(Ordering::Acquire), 2);
            }
            false
        });
        assert!(result.err().expect("close must suppress output").canceled());
        assert_eq!(sessions.live.load(Ordering::Acquire), 1);
        sessions.close(handles[1 - side]).unwrap();
        assert_eq!(sessions.live.load(Ordering::Acquire), 0);
    }
}

#[test]
fn retained_service_borrow_validates_cancel_and_preserves_exact_capture() {
    let sessions = ReaderSessions::new(); let [before, after] = pair(&sessions);
    let bytes: Vec<u8> = sessions.with_retained(before, |reader, stop| -> Result<_, AccessError> {
        assert!(!stop()); assert_eq!(reader.path(), Path::new("retained.rs"));
        Ok(reader.capture().bytes().to_vec())
    }, || false).unwrap();
    assert_eq!(bytes, b"before retained");
    let result = sessions.with_retained(before, |_, stop| -> Result<(), AccessError> {
        sessions.cancel(before).unwrap(); assert!(stop()); Ok(())
    }, || false);
    assert_eq!(result, Err(AccessError::Canceled));
    sessions.close(before).unwrap(); sessions.close(after).unwrap();
}
