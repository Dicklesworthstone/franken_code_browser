use super::*;
use fcb::{ArenaOwnerId, FileId, SourceCapture, SourceRevision};
use crate::host::desk::DeskLimits;

fn setup() -> (DeskSession, CodeCommands) {
    let owner = ArenaOwnerId::new(9810).unwrap();
    let mut desk = DeskSession::new(owner, DeskLimits::default()).unwrap();
    let source = SourceCapture::from_bytes(owner, FileId::new(owner, 1).unwrap(),
        SourceRevision::new(owner, 1).unwrap(), "source.rs",
        b"fn alpha() {}\nfn beta() { alpha(); }\n".to_vec()).unwrap();
    desk.adopt(0, 1, source, 0, None, || false).unwrap();
    (desk, CodeCommands::new())
}
fn execute(code: &mut CodeCommands, desk: &mut DeskSession, command: &str, args: &[&str],
    mut canceled: impl FnMut() -> bool) -> Result<HostResponse, Failure> {
    let expected = desk.model().revision();
    code.execute(desk, expected, 99, command, args, &mut canceled)
}

#[test]
fn every_outline_publication_checkpoint_preserves_the_accepted_inventory() {
    let (mut desk, mut code) = setup();
    execute(&mut code, &mut desk, "code-outline", &["1", "1", "auto"], || false).unwrap();
    let mut checkpoints = 0;
    execute(&mut code, &mut desk, "code-outline", &["1", "2", "auto"], || { checkpoints += 1; false }).unwrap();
    assert!(checkpoints > 0);
    for cancel_at in 1..=checkpoints {
        let (mut desk, mut code) = setup();
        execute(&mut code, &mut desk, "code-outline", &["1", "1", "auto"], || false).unwrap();
        let mut visited = 0;
        let result = execute(&mut code, &mut desk, "code-outline", &["1", "2", "auto"], || {
            visited += 1; visited == cancel_at
        });
        assert!(matches!(result, Err(e) if e.canceled()), "checkpoint {cancel_at}");
        assert_eq!(code.last_outline_attempt, 2);
        let p = desk.model().pane_id(1).unwrap();
        assert_eq!(code.get_outline(p, 1).unwrap().candidates(&desk, 1, 1).unwrap().len(), 2);
        assert_eq!(desk.model().revision(), 1);
    }
}

#[test]
fn every_reference_publication_checkpoint_preserves_the_previous_query() {
    let (mut desk, mut code) = setup();
    execute(&mut code, &mut desk, "code-references", &["1", "1", "10", "alpha"], || false).unwrap();
    let mut checkpoints = 0;
    execute(&mut code, &mut desk, "code-references", &["1", "2", "10", "beta"], || { checkpoints += 1; false }).unwrap();
    assert!(checkpoints > 0);
    for cancel_at in 1..=checkpoints {
        let (mut desk, mut code) = setup();
        execute(&mut code, &mut desk, "code-references", &["1", "1", "10", "alpha"], || false).unwrap();
        let mut visited = 0;
        let result = execute(&mut code, &mut desk, "code-references", &["1", "2", "10", "beta"], || {
            visited += 1; visited == cancel_at
        });
        assert!(matches!(result, Err(e) if e.canceled()), "checkpoint {cancel_at}");
        assert_eq!(code.last_reference_attempt, 2);
        let p = desk.model().pane_id(1).unwrap(); let accepted = code.get_references(p, 1).unwrap();
        assert_eq!(accepted.name(), "alpha"); assert_eq!(accepted.candidates(&desk, 1, 1).unwrap().len(), 2);
        assert_eq!(desk.model().revision(), 1);
    }
}

#[test]
fn canceled_clear_preserves_both_independent_indexes() {
    let (mut desk, mut code) = setup();
    execute(&mut code, &mut desk, "code-outline", &["1", "1", "auto"], || false).unwrap();
    execute(&mut code, &mut desk, "code-symbol-references", &["1", "1", "1", "1", "10"], || false).unwrap();
    for command in ["code-clear", "code-ref-clear"] {
        assert!(matches!(execute(&mut code, &mut desk, command, &["1", "1"], || true), Err(e) if e.canceled()));
        let p = desk.model().pane_id(1).unwrap();
        assert!(code.get_outline(p, 1).is_ok()); assert!(code.get_references(p, 1).is_ok());
    }
}

#[test]
fn exhausted_global_generation_never_recycles_after_clear_or_on_another_pane() {
    let (mut desk, mut code) = setup();
    let max = u64::MAX.to_string();
    execute(&mut code, &mut desk, "code-outline", &["1", &max, "auto"], || false).unwrap();
    execute(&mut code, &mut desk, "code-clear", &["1", &max], || false).unwrap();
    assert!(matches!(execute(&mut code, &mut desk, "code-outline", &["1", "1", "auto"], || false), Err(_)));
    // Reference identity is independent and still usable after outline exhaustion.
    execute(&mut code, &mut desk, "code-references", &["1", "1", "10", "alpha"], || false).unwrap();
    let p = desk.model().pane_id(1).unwrap();
    desk.apply(1, 2, crate::host::desk::DeskCommand::Duplicate(p), || false).unwrap();
    assert!(matches!(execute(&mut code, &mut desk, "code-outline", &["2", &max, "auto"], || false), Err(_)));
    assert_eq!(code.last_outline_attempt, u64::MAX); assert_eq!(code.last_reference_attempt, 1);
}
