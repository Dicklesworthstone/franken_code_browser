#![forbid(unsafe_code)]

use super::*;

fn selected() -> Command<'static> { Command::MatchingDelimiter { start: 0, end: 1 } }

#[test]
fn matching_is_owner_local_and_does_not_replace_accepted_search() {
    let sessions = ReaderSessions::new();
    let first = sessions.create().unwrap();
    let other = sessions.create().unwrap();
    sessions.supply(first, b"[needle]").unwrap();
    sessions.supply(other, b"[second ]").unwrap();
    sessions.execute(first, Command::Find { generation: 2, needle: "needle",
        limit: 1, scan_bytes: 8 }, || false).unwrap();
    let one = sessions.execute(first, selected(), || false).unwrap();
    let two = sessions.execute(other, selected(), || false).unwrap();
    assert!(one.as_str().contains("\"matching_range\":{\"start\":\"7\",\"end\":\"8\"}"));
    assert!(two.as_str().contains("\"matching_range\":{\"start\":\"8\",\"end\":\"9\"}"));
    assert!(one.as_str().contains(&format!("\"owner\":\"{first}\"")));
    assert!(two.as_str().contains(&format!("\"owner\":\"{other}\"")));
    assert!(sessions.execute(first, Command::CopyHit { generation: 2, index: 0 }, || false)
        .unwrap().as_str().contains("\"original_hex\":\"6e6565646c65\""));
    assert_eq!(ReaderSessions::new().execute(first, selected(), || false).err(), Some(AccessError::UnknownHandle));
    sessions.close(first).unwrap();
    assert!(sessions.execute(other, selected(), || false).is_ok());
    sessions.close(other).unwrap();
}

#[test]
fn cancel_during_matching_refuses_delivery_without_poisoning_capture() {
    let sessions = ReaderSessions::new();
    let handle = sessions.create().unwrap();
    sessions.supply(handle, b"[needle]").unwrap();
    let mut canceled = false;
    let result = sessions.execute(handle, selected(), || {
        if !canceled { canceled = true; sessions.cancel(handle).unwrap(); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    assert!(sessions.execute(handle, selected(), || false).unwrap().as_str()
        .contains("\"delimiter_state\":\"matched\""));
    sessions.close(handle).unwrap();
}

#[test]
fn closing_during_matching_revokes_only_that_reader() {
    let sessions = ReaderSessions::new();
    let handle = sessions.create().unwrap();
    let neighbor = sessions.create().unwrap();
    sessions.supply(handle, b"[needle]").unwrap();
    sessions.supply(neighbor, b"[second]").unwrap();
    let mut closed = false;
    let result = sessions.execute(handle, selected(), || {
        if !closed { closed = true; sessions.close(handle).unwrap(); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    assert_eq!(sessions.live.load(Ordering::Acquire), 1);
    assert!(sessions.execute(neighbor, selected(), || false).is_ok());
    sessions.close(neighbor).unwrap();
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
}

#[test]
fn matching_reentry_is_busy_instead_of_waiting_on_its_own_session() {
    let sessions = ReaderSessions::new();
    let handle = sessions.create().unwrap();
    sessions.supply(handle, b"[needle]").unwrap();
    let mut probed = false;
    let reply = sessions.execute(handle, selected(), || {
        if !probed {
            probed = true;
            assert_eq!(sessions.execute(handle, selected(), || false).err(), Some(AccessError::Busy));
        }
        false
    }).unwrap();
    assert!(probed);
    assert!(reply.as_str().contains("\"delimiter_state\":\"matched\""));
    sessions.close(handle).unwrap();
}
