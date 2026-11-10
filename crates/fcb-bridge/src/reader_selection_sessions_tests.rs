#![forbid(unsafe_code)]

use super::*;

fn selected() -> Command<'static> {
    Command::ResolveSelection { window_start: 0, window_end: 12, utf8_start: 5, utf8_end: 11 }
}

#[test]
fn exact_selection_uses_only_its_immutable_owner_capture() {
    let sessions = ReaderSessions::new();
    let first = sessions.create().unwrap();
    let other = sessions.create().unwrap();
    sessions.supply(first, b"keep needle!").unwrap();
    sessions.supply(other, b"keep second!").unwrap();
    let one = sessions.execute(first, selected(), || false).unwrap();
    let two = sessions.execute(other, selected(), || false).unwrap();
    assert!(one.as_str().contains(&format!("\"owner\":\"{first}\"")));
    assert!(two.as_str().contains(&format!("\"owner\":\"{other}\"")));
    assert!(one.as_str().contains("\"original_hex\":\"6e6565646c65\""));
    assert!(two.as_str().contains("\"original_hex\":\"7365636f6e64\""));
    assert!(one.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    let foreign = ReaderSessions::new();
    assert_eq!(foreign.execute(first, selected(), || false).err(), Some(AccessError::UnknownHandle));
    sessions.close(first).unwrap();
    assert_eq!(sessions.execute(first, selected(), || false).err(), Some(AccessError::UnknownHandle));
    assert!(sessions.execute(other, selected(), || false).is_ok());
    sessions.close(other).unwrap();
}

#[test]
fn canceled_selection_never_invalidates_query_rows_or_next_resolver_request() {
    let sessions = ReaderSessions::new();
    let handle = sessions.create().unwrap();
    sessions.supply(handle, b"keep needle!").unwrap();
    sessions.execute(handle, Command::Find { generation: 2, needle: "needle",
        limit: 1, scan_bytes: 12 }, || false).unwrap();
    let mut canceled = false;
    let result = sessions.execute(handle, selected(), || {
        if !canceled { canceled = true; sessions.cancel(handle).unwrap(); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Canceled));
    let info = sessions.execute(handle, Command::Info, || false).unwrap();
    assert!(info.as_str().contains("\"accepted_query_generation\":\"2\""));
    assert!(sessions.execute(handle, Command::CopyHit { generation: 2, index: 0 }, || false)
        .unwrap().as_str().contains("\"original_hex\":\"6e6565646c65\""));
    assert!(sessions.execute(handle, selected(), || false).is_ok());
    sessions.close(handle).unwrap();
}

#[test]
fn close_during_resolution_revokes_delivery_and_releases_only_its_owned_slot() {
    let sessions = ReaderSessions::new();
    let handle = sessions.create().unwrap();
    let neighbor = sessions.create().unwrap();
    sessions.supply(handle, b"keep needle!").unwrap();
    sessions.supply(neighbor, b"keep second!").unwrap();
    let mut closed = false;
    let result = sessions.execute(handle, selected(), || {
        if !closed { closed = true; sessions.close(handle).unwrap(); }
        false
    });
    assert_eq!(result.err(), Some(AccessError::Closed));
    assert_eq!(sessions.live.load(Ordering::Acquire), 1);
    let reply = sessions.execute(neighbor, selected(), || false).unwrap();
    assert!(reply.as_str().contains("\"text\":\"second\""));
    sessions.close(neighbor).unwrap();
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
}

#[test]
fn busy_resolver_refuses_reentry_without_queuing_or_replacing_active_state() {
    let sessions = ReaderSessions::new();
    let handle = sessions.create().unwrap();
    sessions.supply(handle, b"keep needle!").unwrap();
    let mut probed = false;
    let reply = sessions.execute(handle, selected(), || {
        if !probed {
            probed = true;
            assert_eq!(sessions.execute(handle, selected(), || false).err(), Some(AccessError::Busy));
        }
        false
    }).unwrap();
    assert!(probed);
    assert!(reply.as_str().contains("\"text\":\"needle\""));
    sessions.close(handle).unwrap();
}
