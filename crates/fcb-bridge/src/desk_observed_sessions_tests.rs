//! Production registry transfers preserve both cancellation epochs and the
//! acceptance/delivery distinction. No native payloads are fabricated here.
use super::*;
use std::fs;
use crate::observed_reader_sessions::Command as ObservedCommand;
use fcb_app::host::observed_reader::ObservedReaderEncoding;

fn populated() -> (DeskSessions, u64, ObservedReaderSessions, u64) {
    let desks = DeskSessions::new(); let desk = desks.create().unwrap();
    desks.start(desk, || false).unwrap();
    let readers = ObservedReaderSessions::new(); let reader = readers.create().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-desk-observed-registry-{}-{reader}", std::process::id()));
    fs::create_dir(&root).unwrap(); let path = root.join("source");
    fs::write(&path, b"exact retained evidence").unwrap();
    readers.open_file(reader, &path, || false).unwrap();
    readers.execute(reader, ObservedCommand::Window { offset: 0, bytes: 64,
        encoding: ObservedReaderEncoding::Utf8 }, || false).unwrap();
    (desks, desk, readers, reader)
}
fn field(text: &str, key: &str) -> u64 {
    text.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap().parse().unwrap()
}

#[test]
fn canceling_either_registry_rejects_transfer_before_publication_without_erasing_the_observation() {
    for cancel_reader in [true, false] {
        let (desks, desk, readers, reader) = populated();
        let mut polls = 0;
        let rejected = desks.import_observation(desk, &readers, reader, 0, 1, 1, 0, 5, || {
            polls += 1;
            if polls == 2 {
                if cancel_reader { readers.cancel(reader).unwrap(); } else { desks.cancel(desk).unwrap(); }
            }
            false
        });
        assert!(rejected.is_err_and(AccessError::canceled));
        let unchanged = desks.execute(desk, Command::State, || false).unwrap();
        assert_eq!(field(unchanged.as_str(), "model_revision"), 0);
        assert!(unchanged.as_str().contains("\"observed_passages\":[]"));
        let accepted = desks.import_observation(desk, &readers, reader, 0, 2, 1, 0, 5, || false).unwrap();
        let passage = field(accepted.as_str(), "passage");
        readers.close(reader).unwrap();
        let read = desks.execute(desk, Command::ObservedPassage { expected: 2, passage }, || false).unwrap();
        assert!(read.as_str().contains("\"text\":\"exact\""));
        assert!(read.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    }
}

#[test]
fn lost_transfer_reply_is_reconciled_without_duplicating_the_accepted_note() {
    let (desks, desk, readers, reader) = populated();
    let lost = desks.with_session(desk, || false, |session, desk_stop| {
        readers.with_retained(reader, |source, source_stop| {
            session.import_observation(0, 1, source, 1, range(0, 5)?, &mut *source_stop)?;
            readers.cancel(reader)?;
            Ok(session.state(|| false)?)
        }, &mut *desk_stop)
    });
    assert!(lost.is_err_and(AccessError::canceled));
    let accepted = desks.execute(desk, Command::State, || false).unwrap();
    assert_eq!(field(accepted.as_str(), "model_revision"), 1);
    assert_eq!(field(accepted.as_str(), "last_attempt"), 1);
    assert_eq!(accepted.as_str().matches("\"passage\":").count(), 1);
    assert!(desks.import_observation(desk, &readers, reader, 1, 1, 1, 0, 5, || false).is_err());
    assert!(desks.import_observation(desk, &readers, desk, 1, 2, 1, 0, 5, || false).is_err());
    assert_eq!(desks.execute(desk, Command::State, || false).unwrap().as_str(), accepted.as_str());
}
