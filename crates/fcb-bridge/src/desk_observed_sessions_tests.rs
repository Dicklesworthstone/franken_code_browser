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


fn populated_stream() -> (DeskSessions, u64, catalog_stream_sessions::CatalogStreamSessions, u64, u64, std::path::PathBuf) {
    let desks = DeskSessions::new(); let desk = desks.create().unwrap();
    desks.start(desk, || false).unwrap();
    let streams = catalog_stream_sessions::CatalogStreamSessions::new();
    let stream = streams.create().unwrap();
    let root = std::env::temp_dir().join(format!("fcb-desk-stream-registry-{}-{stream}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let path = root.join("source.rs"); fs::write(&path, b"before needle after").unwrap();
    let options = fcb_app::host::catalog_session::CatalogContentSearchOptions { count_all_matches: true, ..Default::default() };
    let mut report = streams.open(stream, &root, 7, "needle", options, || false).unwrap();
    for _ in 0..1000 {
        if report.as_str().contains("\"more\":false") {
            return (desks, desk, streams, stream, field(report.as_str(), "revision"), path);
        }
        report = streams.execute(stream, catalog_stream_sessions::Command::Advance {
            generation: 7, after_revision: field(report.as_str(), "revision"), max_steps: 256,
        }, || false).unwrap();
    }
    panic!("bounded stream fixture did not finish");
}

#[test]
fn stream_excerpt_transfer_rejects_stale_rows_and_cancel_from_either_registry() {
    for cancel_source in [true, false] {
        let (desks, desk, streams, stream, revision, path) = populated_stream();
        assert!(desks.import_catalog_stream_excerpt(desk, &streams, stream, 0, 1, 8, revision, 0, || false).is_err());
        assert!(desks.import_catalog_stream_excerpt(desk, &streams, stream, 0, 1, 7, revision - 1, 0, || false).is_err());
        assert!(desks.import_catalog_stream_excerpt(desk, &streams, desk, 0, 1, 7, revision, 0, || false).is_err());
        let mut polls = 0;
        let canceled = desks.import_catalog_stream_excerpt(desk, &streams, stream, 0, 1, 7, revision, 0, || {
            polls += 1;
            if polls == 2 {
                if cancel_source { streams.cancel(stream).unwrap(); } else { desks.cancel(desk).unwrap(); }
            }
            false
        });
        assert!(canceled.is_err_and(AccessError::canceled));
        let unchanged = desks.execute(desk, Command::State, || false).unwrap();
        assert_eq!(field(unchanged.as_str(), "model_revision"), 0);
        assert!(unchanged.as_str().contains("\"observed_passages\":[]"));
        fs::write(path, b"unrelated replacement").unwrap();
        let accepted = desks.import_catalog_stream_excerpt(desk, &streams, stream, 0, 2, 7, revision, 0, || false).unwrap();
        let passage = field(accepted.as_str(), "passage");
        streams.close(stream).unwrap();
        let read = desks.execute(desk, Command::ObservedPassage { expected: 2, passage }, || false).unwrap();
        assert!(read.as_str().contains("\"text\":\"before needle after\""));
        assert!(read.as_str().contains("\"additional_source_bytes_read\":\"0\""));
        assert!(read.as_str().contains("\"whole_file_immutable\":false"));
    }
}

#[test]
fn committed_stream_excerpt_with_lost_reply_reconciles_once_and_never_replays_the_note() {
    let (desks, desk, streams, stream, revision, _) = populated_stream();
    let lost = desks.with_session(desk, || false, |session, desk_stop| {
        streams.with_retained(stream, |source, source_stop| {
            let excerpt = source.retained_excerpt(7, revision, 0, &mut *source_stop)
                .map_err(catalog_stream_sessions::AccessError::from)?;
            session.import_search_excerpt(0, 1, &excerpt, &mut *source_stop)?;
            streams.cancel(stream)?;
            Ok(session.state(|| false)?)
        }, &mut *desk_stop)
    });
    assert!(lost.is_err_and(AccessError::canceled));
    let accepted = desks.execute(desk, Command::State, || false).unwrap();
    assert_eq!(field(accepted.as_str(), "model_revision"), 1);
    assert_eq!(field(accepted.as_str(), "last_attempt"), 1);
    assert_eq!(accepted.as_str().matches("\"passage\":").count(), 1);
    assert!(desks.import_catalog_stream_excerpt(desk, &streams, stream, 1, 1, 7, revision, 0, || false).is_err());
    assert_eq!(desks.execute(desk, Command::State, || false).unwrap().as_str(), accepted.as_str());
}

fn populated_retained() -> (DeskSessions, u64, ReaderSessions, u64) {
    let desks = DeskSessions::new(); let desk = desks.create().unwrap();
    desks.start(desk, || false).unwrap();
    let readers = ReaderSessions::new(); let reader = readers.create().unwrap();
    readers.initialize_prepared(reader, |owner, stop| {
        let mut source = fcb_app::host::reader::ReaderSession::from_bytes(owner,
            std::path::Path::new("unopened-retained-source"), b"exact retained evidence", &mut *stop)?;
        let reply = source.info(&mut *stop)?;
        Ok::<_, reader_sessions::AccessError>((source, reply))
    }, || false).unwrap();
    (desks, desk, readers, reader)
}

#[test]
fn retained_passage_transfer_observes_both_cancel_epochs_without_mutating_the_desk() {
    for cancel_reader in [true, false] {
        let (desks, desk, readers, reader) = populated_retained();
        let mut polls = 0;
        let rejected = desks.import_reader_passage(desk, &readers, reader, 0, 1, 0, 5, || {
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
        let accepted = desks.import_reader_passage(desk, &readers, reader, 0, 2, 0, 5, || false).unwrap();
        assert!(accepted.as_str().contains("\"consistency\":\"host-supplied-sequence\""));
        let passage = field(accepted.as_str(), "passage"); readers.close(reader).unwrap();
        let read = desks.execute(desk, Command::ObservedPassage { expected: 2, passage }, || false).unwrap();
        assert!(read.as_str().contains("\"text\":\"exact\""));
        assert!(read.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    }
}

#[test]
fn retained_passage_commit_survives_lost_delivery_and_is_reconciled_without_replay() {
    let (desks, desk, readers, reader) = populated_retained();
    let lost = desks.with_session(desk, || false, |session, desk_stop| {
        readers.with_retained_mut(reader, |source, source_stop| {
            let excerpt = source.retained_passage(range(0, 5)?, &mut *source_stop)
                .map_err(reader_sessions::AccessError::from)?;
            session.import_reader_passage(0, 1, &excerpt, &mut *source_stop)?;
            readers.cancel(reader)?;
            Ok(session.state(|| false)?)
        }, &mut *desk_stop)
    });
    assert!(lost.is_err_and(AccessError::canceled));
    let accepted = desks.execute(desk, Command::State, || false).unwrap();
    assert_eq!(field(accepted.as_str(), "model_revision"), 1);
    assert_eq!(field(accepted.as_str(), "last_attempt"), 1);
    assert_eq!(accepted.as_str().matches("\"passage\":").count(), 1);
    assert!(desks.import_reader_passage(desk, &readers, reader, 1, 1, 0, 5, || false).is_err());
    assert!(desks.import_reader_passage(desk, &readers, desk, 1, 2, 0, 5, || false).is_err());
    assert_eq!(desks.execute(desk, Command::State, || false).unwrap().as_str(), accepted.as_str());
}
