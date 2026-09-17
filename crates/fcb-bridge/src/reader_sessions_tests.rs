#![forbid(unsafe_code)]

use super::*;
use std::{fs, panic::{AssertUnwindSafe, catch_unwind}, sync::Barrier, time::{SystemTime, UNIX_EPOCH}};

#[test]
fn creation_is_inert_and_closed_handles_never_alias_reused_slots() {
    let sessions = ReaderSessions::new();
    let a = sessions.create().unwrap();
    assert_eq!(sessions.execute(a, Command::Info, || false).err(), Some(AccessError::NotOpen));
    sessions.close(a).unwrap();
    let b = sessions.create().unwrap(); assert_ne!(a, b);
    assert_eq!(sessions.execute(a, Command::Info, || false).err(), Some(AccessError::UnknownHandle));
    assert_eq!(sessions.close(a), Err(AccessError::UnknownHandle));
    assert_eq!(sessions.cancel(0), Err(AccessError::UnknownHandle));
    sessions.close(b).unwrap();
}

#[test]
fn independent_registries_cannot_resolve_each_others_handle() {
    let a = ReaderSessions::new(); let b = ReaderSessions::new();
    let ah = a.create().unwrap(); let bh = b.create().unwrap(); assert_ne!(ah, bh);
    a.supply(ah, b"alpha").unwrap(); b.supply(bh, b"beta").unwrap();
    assert_eq!(b.execute(ah, Command::Info, || false).err(), Some(AccessError::UnknownHandle));
    a.close(ah).unwrap();
    assert!(b.execute(bh, Command::Window { offset: 0, bytes: 10 }, || false).unwrap().as_str().contains("beta"));
}

#[test]
fn a_loaded_handle_is_immutable_and_does_not_reopen_a_new_path() {
    let sessions = ReaderSessions::new(); let h = sessions.create().unwrap();
    sessions.supply(h, b"old needle").unwrap();
    assert_eq!(sessions.open(h, Path::new("/not-granted/missing"), 100, || false).err(), Some(AccessError::AlreadyOpen));
    sessions.execute(h, Command::Find { generation: 1, needle: "needle", limit: 1, scan_bytes: 100 }, || false).unwrap();
    let copy = sessions.execute(h, Command::CopyHit { generation: 1, index: 0 }, || false).unwrap();
    assert!(copy.as_str().contains("6e6565646c65"));
}

#[test]
fn empty_and_live_handles_share_the_same_capacity_bound() {
    let sessions = ReaderSessions::new();
    let handles: Vec<_> = (0..MAX_READER_SESSIONS).map(|_| sessions.create().unwrap()).collect();
    sessions.supply(handles[0], b"one source").unwrap();
    assert_eq!(sessions.create(), Err(AccessError::Capacity));
    sessions.close(handles[1]).unwrap();
    let new = sessions.create().unwrap(); assert!(!handles.contains(&new));
    assert_eq!(sessions.live.load(Ordering::Acquire), MAX_READER_SESSIONS);
    sessions.close(new).unwrap();
    for (i, h) in handles.into_iter().enumerate() { if i != 1 { sessions.close(h).unwrap(); } }
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
}

#[test]
fn busy_reader_does_not_block_cancel_or_an_independent_reader() {
    let sessions = ReaderSessions::new();
    let a = sessions.create().unwrap(); let b = sessions.create().unwrap();
    sessions.supply(a, b"needle").unwrap(); sessions.supply(b, b"other").unwrap();
    sessions.execute(a, Command::Find { generation: 1, needle: "needle", limit: 10, scan_bytes: 100 }, || false).unwrap();
    let entered = Barrier::new(2); let release = Barrier::new(2);
    std::thread::scope(|scope| {
        let job = scope.spawn(|| {
            let mut first = true;
            sessions.execute(a, Command::Window { offset: 0, bytes: 100 }, || {
                if first { first = false; entered.wait(); release.wait(); }
                false
            })
        });
        entered.wait();
        let busy = sessions.execute(a, Command::Info, || false).err();
        let other = sessions.execute(b, Command::Info, || false);
        let canceled = sessions.cancel(a);
        release.wait();
        let stopped = job.join().unwrap().err();
        assert_eq!(busy, Some(AccessError::Busy)); assert!(other.is_ok()); assert!(canceled.is_ok());
        assert_eq!(stopped, Some(AccessError::Canceled));
    });
    assert!(sessions.execute(a, Command::CopyHit { generation: 1, index: 0 }, || false).is_ok());
    assert!(sessions.execute(a, Command::Window { offset: 0, bytes: 100 }, || false).is_ok());
}

#[test]
fn closing_active_work_does_not_recycle_its_source_admission_early() {
    let sessions = ReaderSessions::new();
    let handles: Vec<_> = (0..MAX_READER_SESSIONS).map(|_| sessions.create().unwrap()).collect();
    let h = handles[0]; sessions.supply(h, b"retained source").unwrap();
    let entered = Barrier::new(2); let release = Barrier::new(2);
    std::thread::scope(|scope| {
        let job = scope.spawn(|| {
            let mut first = true;
            sessions.execute(h, Command::Info, || {
                if first { first = false; entered.wait(); release.wait(); }
                false
            })
        });
        entered.wait();
        let closed = sessions.close(h);
        let still_charged = sessions.live.load(Ordering::Acquire);
        let refused = sessions.create();
        let invalid = sessions.execute(h, Command::Info, || false).err();
        release.wait();
        let result = job.join().unwrap().err();
        assert!(closed.is_ok()); assert_eq!(still_charged, MAX_READER_SESSIONS);
        assert_eq!(refused, Err(AccessError::Capacity));
        assert_eq!(invalid, Some(AccessError::UnknownHandle)); assert_eq!(result, Some(AccessError::Closed));
    });
    assert_eq!(sessions.live.load(Ordering::Acquire), MAX_READER_SESSIONS - 1);
    assert_ne!(sessions.create().unwrap(), h);
}

#[test]
fn cancellation_of_initial_capture_keeps_an_empty_handle_without_source_io() {
    let sessions = ReaderSessions::new(); let h = sessions.create().unwrap();
    let error = sessions.open(h, Path::new("/not-granted/never-opened"), 100, || true).err().unwrap();
    assert!(error.canceled());
    assert_eq!(sessions.execute(h, Command::Info, || false).err(), Some(AccessError::NotOpen));
    sessions.supply(h, b"explicit retry").unwrap();
    assert!(sessions.execute(h, Command::Info, || false).is_ok());
}

#[test]
fn failed_capture_can_be_retried_without_replacing_an_accepted_source() {
    let sessions = ReaderSessions::new(); let h = sessions.create().unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-reader-registry-{}-{now}-{h}", std::process::id()));
    fs::write(&path, b"12345").unwrap();
    assert!(sessions.open(h, &path, 4, || false).is_err());
    assert_eq!(sessions.execute(h, Command::Info, || false).err(), Some(AccessError::NotOpen));
    sessions.open(h, &path, 5, || false).unwrap();
    fs::write(&path, b"changed").unwrap();
    let copy = sessions.execute(h, Command::CopyRange { start: 0, end: 5 }, || false).unwrap();
    assert!(copy.as_str().contains("3132333435"));
}

#[test]
fn panicking_worker_poisons_only_its_session_and_close_still_reclaims() {
    let sessions = ReaderSessions::new(); let a = sessions.create().unwrap(); let b = sessions.create().unwrap();
    sessions.supply(a, b"a").unwrap(); sessions.supply(b, b"b").unwrap();
    let panic = catch_unwind(AssertUnwindSafe(|| sessions.execute(a, Command::Info, || panic!("injected worker failure"))));
    assert!(panic.is_err());
    assert_eq!(sessions.execute(a, Command::Info, || false).err(), Some(AccessError::Poisoned));
    assert!(sessions.execute(b, Command::Info, || false).is_ok());
    sessions.cancel(a).unwrap(); sessions.close(a).unwrap(); sessions.close(b).unwrap();
    assert_eq!(sessions.live.load(Ordering::Acquire), 0);
}

#[test]
fn exhausted_handle_and_cancellation_identities_do_not_wrap() {
    let next = AtomicU64::new(u64::MAX - 1);
    assert_eq!(allocate_handle(&next).unwrap(), u64::MAX - 1);
    assert_eq!(allocate_handle(&next), Err(AccessError::IdentityExhausted));
    assert_eq!(next.load(Ordering::Acquire), u64::MAX);
    let sessions = ReaderSessions::new(); let h = sessions.create().unwrap();
    sessions.get(h).unwrap().epoch.store(u64::MAX, Ordering::Release);
    assert_eq!(sessions.cancel(h), Err(AccessError::IdentityExhausted));
    assert_eq!(sessions.execute(h, Command::Info, || false).err(), Some(AccessError::Closed));
    sessions.close(h).unwrap();
}

#[test]
fn ffi_handle_storage_can_cross_host_threads_without_unsafe_send_impls() {
    fn send_sync<T: Send + Sync>() {}
    fn send<T: Send>() {}
    send_sync::<ReaderSessions>(); send::<ReaderSession>(); send::<HostResponse>();
}

#[test]
fn error_handoffs_are_bounded_and_never_success_shaped() {
    for error in [AccessError::UnknownHandle, AccessError::Busy, AccessError::Capacity, AccessError::Canceled, AccessError::Closed] {
        let json = error.json(u64::MAX);
        assert!(json.contains("\"status\":\"error\"")); assert!(json.contains("18446744073709551615"));
        assert!(!json.contains("\"status\":\"ok\"")); assert!(json.len() < 4096);
        assert!(!json.as_bytes().contains(&0)); assert!(json.ends_with("}\n"));
    }
}
