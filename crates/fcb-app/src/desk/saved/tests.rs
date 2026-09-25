use super::*;
use fcb::{ArenaOwnerId, ByteLength};
use fcb::search::{ResourceAllocationId, ResourceBudget, snapshot::{SnapshotBytes, SnapshotEntry, SnapshotData}};

fn fixture() -> PathBuf {
    use std::{fs, time::{SystemTime, UNIX_EPOCH}, sync::atomic::{AtomicU64, Ordering}};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-expression-publish-{}-{}-{}", std::process::id(),
        SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let owner = ArenaOwnerId::new(67100).unwrap(); let source = b"old new required";
    let budget = ResourceBudget::new(owner, ByteLength::new(64 * 1024 * 1024)).unwrap();
    let bytes = SnapshotBytes::encode(owner, true, "expression-publication",
        &[SnapshotEntry { path: b"a.rs", observed_bytes: source.len() as u64, data: SnapshotData::Captured(source) }],
        Default::default(), &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let path = root.join("repository.fcbs"); fs::write(&path, bytes.bytes()).unwrap(); path
}
fn setup(path: &std::path::Path, owner: u64) -> (SavedCommands, DeskSession) {
    let mut desk = DeskSession::new(ArenaOwnerId::new(owner).unwrap(), Default::default()).unwrap();
    let mut commands = SavedCommands::new();
    commands.execute(&mut desk, 0, 1, "saved-open", &[path.to_str().unwrap()], &mut || false).unwrap();
    commands.execute(&mut desk, 0, 2, "saved-expression", &["1", "1", "10", "1000", "old required"], &mut || false).unwrap();
    (commands, desk)
}

#[test]
fn cancellation_at_every_prepare_and_response_boundary_keeps_previous_expression() {
    let path = fixture(); let (mut probe, mut probe_desk) = setup(&path, 67101);
    let mut polls = 0;
    probe.execute(&mut probe_desk, 0, 3, "saved-expression", &["1", "2", "10", "1000", "new required"],
        &mut || { polls += 1; false }).unwrap();
    let (mut commands, mut desk) = setup(&path, 67102);
    for n in 1..=polls {
        let generation = (n + 1).to_string(); let mut at = 0;
        let result = commands.execute(&mut desk, 0, n as u64 + 2, "saved-expression",
            &["1", &generation, "10", "1000", "new required"], &mut || { at += 1; at == n });
        assert!(matches!(result, Err(e) if e.canceled()), "checkpoint {n}");
        assert_eq!(commands.expression.as_ref().unwrap().generation(), 1);
        assert_eq!(commands.last_expression_attempt, n as u64 + 1);
        assert_eq!(desk.model().revision(), 0);
    }
    let opened = commands.execute(&mut desk, 0, polls as u64 + 10, "saved-expression-hit", &["1", "1", "1"], &mut || false).unwrap();
    assert_eq!(opened.exit_code(), crate::EXIT_OK);
    assert_eq!(desk.model().selected_bytes(desk.model().active().unwrap(), desk.model().revision()).unwrap(), b"old");
}

#[test]
fn canceled_clear_and_generation_exhaustion_do_not_destroy_accepted_rows() {
    let path = fixture(); let (mut commands, mut desk) = setup(&path, 67103);
    assert!(commands.execute(&mut desk, 0, 3, "saved-expression-clear", &["1", "1"], &mut || true).is_err());
    assert_eq!(commands.expression.as_ref().unwrap().generation(), 1);
    commands.last_expression_attempt = u64::MAX;
    for generation in ["0", "1", "18446744073709551615"] {
        assert!(commands.execute(&mut desk, 0, 4, "saved-expression", &["1", generation, "10", "1000", "new"], &mut || false).is_err());
    }
    assert_eq!(commands.expression.as_ref().unwrap().generation(), 1);
    assert!(commands.execute(&mut desk, 0, 5, "saved-expression-page", &["1", "1", "0", "10"], &mut || false).is_ok());
}
