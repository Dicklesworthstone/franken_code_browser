//! Production registry coverage. Execute only in the independent strict-RCH
//! verification lane. Fixtures intentionally retain their files for inspection.
use super::*;
use std::{fs, path::PathBuf, sync::atomic::AtomicU64};

static FIXTURE: AtomicU64 = AtomicU64::new(1);
fn directory() -> PathBuf {
    let path = std::env::temp_dir().join(format!("fcb-native-desk-{}-{}", std::process::id(), FIXTURE.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).expect("new fixture directory"); path
}
fn field(text: &str, key: &str) -> u64 {
    let marker = format!("\"{key}\":\"");
    text.split_once(&marker).expect("field").1.split('"').next().expect("value").parse().expect("decimal")
}
fn command(desks: &DeskSessions, handle: u64, expected: u64, attempt: u64, kind: u64, subject: u64,
    start: u64, end: u64, label: &str) -> Result<HostResponse, AccessError> {
    desks.execute(handle, Command::Apply { expected, attempt, kind, subject, start, end, label }, || false)
}

#[test]
fn native_desk_keeps_exact_reader_bytes_and_bookmarks_after_live_replacement() {
    let root = directory(); let file = root.join("code.rs");
    fs::write(&file, b"original\r\nretained\0source\n").unwrap();
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    readers.open(reader, &file, 4096, || false).unwrap();
    fs::write(&file, b"changed live file").unwrap();
    let desks = DeskSessions::new(); let desk = desks.create().unwrap(); desks.start(desk, || false).unwrap();
    let imported = desks.import_reader(desk, &readers, reader, 0, 1, 10, 18, || false).unwrap();
    let pane = field(imported.as_str(), "pane");
    assert!(imported.as_str().contains("\"initial_source_bytes_read\":\"0\""));
    assert!(imported.as_str().contains("\"retained_sources\":\"1\""));
    command(&desks, desk, 1, 2, 3, pane, 0, 0, "Keep the exact evidence").unwrap();
    command(&desks, desk, 2, 3, 8, pane, 0, 0, "").unwrap();
    readers.close(reader).unwrap();
    let recalled = command(&desks, desk, 3, 4, 4, 1, 0, 0, "").unwrap();
    let recalled_pane = field(recalled.as_str(), "pane");
    let page = desks.execute(desk, Command::Window { expected: 4, pane: recalled_pane, offset: 0, bytes: 64 }, || false).unwrap();
    assert!(page.as_str().contains("original\\r\\nretained\\u0000source\\n"));
    assert!(!page.as_str().contains("changed live file"));
    assert!(page.as_str().contains("\"selection\":{\"start\":\"10\",\"end\":\"18\"}"));
}

#[test]
fn checkpoint_restore_preserves_cross_file_history_and_user_bookmarks() {
    let root = directory(); let a = root.join("a.rs"); let b = root.join("b.rs"); let checkpoint = root.join("desk.fcbdesk");
    fs::write(&a, b"alpha capture\n").unwrap(); fs::write(&b, b"beta capture\n").unwrap();
    let desks = DeskSessions::new(); let desk = desks.create().unwrap(); desks.start(desk, || false).unwrap();
    let first = desks.execute(desk, Command::Open { expected: 0, attempt: 1, path: &a }, || false).unwrap();
    let pane = field(first.as_str(), "pane");
    command(&desks, desk, 1, 2, 3, pane, 0, 0, "alpha note").unwrap();
    desks.execute(desk, Command::Open { expected: 2, attempt: 3, path: &b }, || false).unwrap();
    let saved = desks.save(desk, 3, &checkpoint, 1024, || false).unwrap();
    assert!(saved.as_str().contains("complete-file-and-parent-sync-requested"));
    assert!(saved.as_str().contains("\"overwritten\":false"));
    let original = fs::read(&checkpoint).unwrap();
    let duplicate = desks.save(desk, 3, &checkpoint, 1024, || false).unwrap();
    assert!(duplicate.as_str().contains("DESK_SAVE_DESTINATION_EXISTS"));
    assert!(duplicate.as_str().contains("\"effect\":\"none\""));
    assert_eq!(fs::read(&checkpoint).unwrap(), original);
    fs::write(&a, b"unrelated replacement a").unwrap(); fs::write(&b, b"unrelated replacement b").unwrap();
    desks.close(desk).unwrap();
    let reopened = desks.create().unwrap(); desks.start(reopened, || false).unwrap();
    let restored = desks.execute(reopened, Command::Restore { expected: 0, attempt: 1, path: &checkpoint }, || false).unwrap();
    assert!(restored.as_str().contains("alpha note"));
    assert!(restored.as_str().contains("\"can_go_back\":true"));
    let back = command(&desks, reopened, 1, 2, 9, 0, 0, 0, "").unwrap();
    let active = field(back.as_str(), "active_pane");
    let window = desks.execute(reopened, Command::Window { expected: 2, pane: active, offset: 0, bytes: 64 }, || false).unwrap();
    assert!(window.as_str().contains("alpha capture"));
    assert!(!window.as_str().contains("unrelated replacement"));
}

#[test]
fn cancellation_invalid_restore_and_stale_retry_preserve_the_accepted_desk() {
    let root = directory(); let source = root.join("main.rs"); let bad = root.join("invalid.fcbdesk");
    fs::write(&source, b"preserved source").unwrap(); fs::write(&bad, b"not a checkpoint").unwrap();
    let desks = DeskSessions::new(); let desk = desks.create().unwrap(); desks.start(desk, || false).unwrap();
    let opened = desks.execute(desk, Command::Open { expected: 0, attempt: 1, path: &source }, || false).unwrap();
    let pane = field(opened.as_str(), "pane");
    command(&desks, desk, 1, 2, 3, pane, 0, 0, "survives failed restore").unwrap();
    assert!(desks.execute(desk, Command::Restore { expected: 2, attempt: 3, path: &bad }, || false).is_err());
    assert!(command(&desks, desk, 2, 2, 3, pane, 0, 0, "duplicate").is_err());
    assert!(desks.execute(desk, Command::Open { expected: 2, attempt: 4, path: &source }, || true).is_err());
    let state = desks.execute(desk, Command::State, || false).unwrap();
    assert_eq!(field(state.as_str(), "model_revision"), 2);
    assert!(state.as_str().contains("survives failed restore"));
    assert!(!state.as_str().contains("duplicate"));
    let refused = root.join("refused.fcbdesk");
    let save = desks.save(desk, 2, &refused, 1, || false).unwrap();
    assert!(save.as_str().contains("DESK_SAVE_SOURCE_EXPORT_LIMIT"));
    assert!(!refused.exists());
}

#[test]
fn repeated_exact_import_reuses_source_and_refuses_other_owner_authority() {
    let root = directory(); let path = root.join("source.rs"); fs::write(&path, b"same exact bytes").unwrap();
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap(); readers.open(reader, &path, 4096, || false).unwrap();
    let desks = DeskSessions::new(); let desk = desks.create().unwrap(); desks.start(desk, || false).unwrap();
    desks.import_reader(desk, &readers, reader, 0, 1, 0, 4, || false).unwrap();
    let second = desks.import_reader(desk, &readers, reader, 1, 2, 5, 10, || false).unwrap();
    assert!(second.as_str().contains("\"retained_sources\":\"1\""));
    assert_eq!(field(second.as_str(), "retained_source_bytes"), 16);
    assert!(desks.import_reader(desk, &readers, desk, 2, 3, 0, 4, || false).is_err());
    assert!(desks.import_reader(desk, &readers, reader, 2, 3, 0, 999, || false).is_err());
    let state = desks.execute(desk, Command::State, || false).unwrap(); assert_eq!(field(state.as_str(), "model_revision"), 2);
}

#[test]
fn close_revokes_immediately_and_retiring_work_still_holds_capacity() {
    let desks = DeskSessions::new(); let first = desks.create().unwrap(); let second = desks.create().unwrap();
    desks.start(first, || false).unwrap(); desks.start(second, || false).unwrap();
    assert!(matches!(desks.create(), Err(AccessError::Capacity)));
    let result = desks.with_session(first, || false, |desk, _| {
        desks.close(first)?;
        assert!(matches!(desks.create(), Err(AccessError::Capacity)));
        Ok(desk.state(|| false)?)
    });
    assert!(matches!(result, Err(AccessError::Closed)));
    let replacement = desks.create().unwrap(); assert_ne!(replacement, first);
    assert!(matches!(desks.execute(first, Command::State, || false), Err(AccessError::Unknown)));
}

#[test]
fn canceled_write_returns_effect_receipt_instead_of_erasing_filesystem_outcome() {
    let root = directory(); let source = root.join("source.rs"); let destination = root.join("canceled.fcbdesk");
    fs::write(&source, b"evidence").unwrap();
    let desks = DeskSessions::new(); let desk = desks.create().unwrap(); desks.start(desk, || false).unwrap();
    desks.execute(desk, Command::Open { expected: 0, attempt: 1, path: &source }, || false).unwrap();
    let receipt = desks.save(desk, 1, &destination, 1024, || destination.exists()).unwrap();
    assert!(destination.exists(), "negative control: cancellation must occur after create");
    assert!(receipt.as_str().contains("\"command\":\"save\""));
    assert!(receipt.as_str().contains("\"status\":\"error\""));
    assert!(receipt.as_str().contains("\"effect\":\"destination-created-incomplete\""));
    assert!(receipt.as_str().contains("DESK_CHECKPOINT_CANCELED"));
}
