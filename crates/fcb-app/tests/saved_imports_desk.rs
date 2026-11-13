#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Saved lexical evidence must enter the same retained Desk transaction as
//! ordinary members and search hits. Authored for independent RCH execution.
mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, owner, entry};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb_app::host::{HostResponse, desk::{DeskSession, DeskLimits, DeskCommand, DeskError, DeskSessionError},
    saved_repository::{SavedRepositorySession, desk::SavedDeskError,
        imports::{SavedImportSide as Side, SavedImportError as Error}}};

const STATEMENT: &[u8] = b"from .helper import run as main";
const SOURCE: &[u8] = b"# saved\nfrom .helper import run as main\n";
const TARGET: &[u8] = b"def run():\n    return 7\n";
fn desk() -> DeskSession { DeskSession::new(owner(57500), DeskLimits::default()).unwrap() }
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn prepare(saved: &mut SavedRepositorySession) { saved.prepare_imports(1, 64, 4096, || false).unwrap(); }
fn reads(saved: &mut SavedRepositorySession) -> u64 {
    json(&saved.info(|| false).unwrap()).get("member_bytes_read").number()
}
fn utf16(text: &str, big: bool) -> Vec<u8> {
    let mut bytes = if big { vec![0xfe, 0xff] } else { vec![0xff, 0xfe] };
    for word in text.encode_utf16() { bytes.extend_from_slice(&if big { word.to_be_bytes() } else { word.to_le_bytes() }); }
    bytes
}

#[test]
fn lexical_selection_and_target_share_normal_navigation_and_survive_projection_detachment() {
    let f = Fixture::new(&[entry(b"pkg/helper.py", TARGET), entry(b"pkg/main.py", SOURCE)], true);
    let mut saved = f.open(57501); let mut d = desk(); prepare(&mut saved);
    let opened = saved.open_import_desk(&mut d, 0, 1, 1, 1, Side::Evidence, || false).unwrap();
    let pane = opened.imported.change.active.unwrap();
    assert_eq!(opened.member(), 1);
    assert_eq!(opened.selection().unwrap().start().get(), 8);
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), STATEMENT);
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), SOURCE);
    let receipt = json(&saved.desk_open_response(&d, &opened).unwrap());
    assert_eq!(receipt.get("selection_namespace").text(), "saved-import");
    assert_eq!(receipt.get("import_generation").number(), 1);
    assert_eq!(receipt.get("import_link_id").number(), 1);
    assert_eq!(receipt.get("import_side").text(), "evidence");
    assert_eq!(receipt.get("source_observation").text(), "verified-saved-member");
    assert!(!receipt.get("live_source_reopened").flag());
    assert_eq!(receipt.get("member_bytes_read").number(), SOURCE.len() as u64);
    for key in ["query_generation", "expression_generation", "relationship_generation", "fragment_resolution"] {
        assert_eq!(receipt.get(key), &Json::Null);
    }
    let target = saved.open_import_desk(&mut d, 1, 2, 1, 1, Side::Target, || false).unwrap();
    assert_eq!(target.member(), 0); assert_eq!(target.selection(), None);
    assert_eq!(d.model().source(target.imported.change.active.unwrap(), 2).unwrap().bytes(), TARGET);
    let receipt = json(&saved.desk_open_response(&d, &target).unwrap());
    assert_eq!(receipt.get("import_side").text(), "target");
    assert_eq!(receipt.get("selection"), &Json::Null);
    saved.clear_imports(2, || false).unwrap();
    assert_eq!(saved.open_import_desk(&mut d, 2, 3, 1, 1, Side::Evidence, || false).err(),
        Some(SavedDeskError::Import(Error::MissingImports)));
    drop(saved);
    fs::rename(&f.path, f.root.join("retired.fcbs")).unwrap();
    d.apply(2, 3, DeskCommand::Back, || false).unwrap();
    assert_eq!(d.model().selected_bytes(d.model().active().unwrap(), 3).unwrap(), STATEMENT);
    d.apply(3, 4, DeskCommand::Forward, || false).unwrap();
    assert_eq!(d.model().source(d.model().active().unwrap(), 4).unwrap().bytes(), TARGET);
    assert!(!f.root.join("pkg/main.py").exists());
}

#[test]
fn member_literal_and_import_selection_reuse_one_capture_even_at_residency_capacity() {
    let f = Fixture::new(&[entry(b"pkg/helper.py", TARGET), entry(b"pkg/main.py", SOURCE)], true);
    let mut saved = f.open(57502);
    let mut d = DeskSession::new(owner(57500), DeskLimits { sources: 1, ..Default::default() }).unwrap();
    let member = saved.open_member_desk(&mut d, 0, 1, 1, || false).unwrap();
    saved.search(1, "main", 10, || false).unwrap(); prepare(&mut saved);
    let hit = saved.open_hit_desk(&mut d, 1, 2, 1, 1, || false).unwrap();
    let import = saved.open_import_desk(&mut d, 2, 3, 1, 1, Side::Evidence, || false).unwrap();
    assert!(hit.imported.reused_capture && import.imported.reused_capture);
    assert_eq!(member.imported.file, import.imported.file);
    assert_eq!(member.imported.revision, import.imported.revision);
    assert_eq!(member.imported.origin, import.imported.origin);
    assert_eq!(d.model().retained_source_count(), 1);
    assert_eq!(d.model().retained_source_bytes(), SOURCE.len() as u64);
    assert_eq!(d.model().selected_bytes(import.imported.change.active.unwrap(), 3).unwrap(), STATEMENT);
    assert_eq!(saved.accepted_generation(), Some(1)); assert_eq!(saved.import_generation(), Some(1));
}

#[test]
fn both_utf16_endiannesses_keep_original_statement_bytes_and_raw_member_identity() {
    for big in [false, true] {
        let raw = utf16("# \u{1f600}\r\nfrom . import helper as chosen\r\n", big);
        let f = Fixture::new(&[entry(b"pkg/\xff/helper.py", TARGET), entry(b"pkg/\xff/main.pyi", &raw)], true);
        let mut saved = f.open(57503 + u64::from(big)); let mut d = desk(); prepare(&mut saved);
        let opened = saved.open_import_desk(&mut d, 0, 1, 1, 1, Side::Evidence, || false).unwrap();
        let pane = opened.imported.change.active.unwrap();
        assert_eq!(opened.selection().unwrap().start().get(), 14);
        assert_eq!(opened.selection().unwrap().end().get(), 74);
        let expected = utf16("from . import helper as chosen", big);
        assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), &expected[2..]);
        assert_eq!(d.model().source(pane, 1).unwrap().bytes(), raw);
        assert_eq!(opened.imported.origin.file.get(), 2);
        drop(saved);
        fs::rename(&f.path, f.root.join("retired.fcbs")).unwrap();
        assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), &raw[14..74]);
    }
}

#[test]
fn stale_unresolved_and_unavailable_endpoints_are_refused_before_source_io_or_navigation() {
    let source = b"from .ambiguous import X\nfrom .blocked import X\nfrom .missing import X\nimport os\n";
    let f = Fixture::new(&[entry(b"pkg/ambiguous.py", b""), entry(b"pkg/ambiguous/__init__.py", b""),
        SnapshotEntry { path: b"pkg/blocked.py", observed_bytes: 42, data: SnapshotData::Unavailable("READ_DENIED") },
        entry(b"pkg/main.py", source)], true);
    let mut saved = f.open(57505); let mut d = desk(); prepare(&mut saved);
    let before = reads(&mut saved);
    for (generation, id, error) in [(2, 1, Error::StaleGeneration), (1, 0, Error::MissingImport),
        (1, 5, Error::MissingImport), (1, 1, Error::UnresolvedTarget), (1, 2, Error::TargetUnavailable),
        (1, 3, Error::UnresolvedTarget), (1, 4, Error::UnresolvedTarget)] {
        assert_eq!(saved.open_import_desk(&mut d, 0, 1, generation, id, Side::Target, || false).err(),
            Some(SavedDeskError::Import(error)));
    }
    assert_eq!(saved.open_import_desk(&mut d, 1, 1, 1, 1, Side::Evidence, || false).err(),
        Some(SavedDeskError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    let mut small = DeskSession::new(owner(57506), DeskLimits { source_bytes: 4, ..Default::default() }).unwrap();
    assert!(saved.open_import_desk(&mut small, 0, 1, 1, 1, Side::Evidence, || false).is_err());
    let mut same_owner = DeskSession::new(saved.owner(), DeskLimits::default()).unwrap();
    assert_eq!(saved.open_import_desk(&mut same_owner, 0, 1, 1, 1, Side::Evidence, || false).err(),
        Some(SavedDeskError::Desk(DeskSessionError::Desk(DeskError::OwnerMismatch))));
    assert_eq!(reads(&mut saved), before);
    assert_eq!(d.model().revision(), 0); assert_eq!(d.model().retained_source_count(), 0);
    let opened = saved.open_import_desk(&mut d, 0, 1, 1, 2, Side::Evidence, || false).unwrap();
    assert_eq!(d.model().selected_bytes(opened.imported.change.active.unwrap(), 1).unwrap(),
        b"from .blocked import X");
}

#[test]
fn cancellation_at_the_last_import_checkpoint_preserves_previous_capture_and_accepted_projection() {
    let f = Fixture::new(&[entry(b"before.txt", b"old retained source"), entry(b"pkg/helper.py", TARGET),
        entry(b"pkg/main.py", SOURCE)], true);
    let mut probe = f.open(57507); let mut pd = desk(); prepare(&mut probe);
    probe.open_member_desk(&mut pd, 0, 1, 0, || false).unwrap();
    let mut polls = 0;
    probe.open_import_desk(&mut pd, 1, 2, 1, 1, Side::Evidence, || { polls += 1; false }).unwrap();
    assert!(polls > 0);
    let mut saved = f.open(57508); let mut d = desk(); prepare(&mut saved);
    let old = saved.open_member_desk(&mut d, 0, 1, 0, || false).unwrap().imported.change.active.unwrap();
    let mut seen = 0;
    let canceled = saved.open_import_desk(&mut d, 1, 2, 1, 1, Side::Evidence, || { seen += 1; seen == polls });
    assert!(matches!(canceled, Err(error) if error.is_canceled()));
    assert_eq!(d.model().revision(), 1); assert_eq!(d.model().retained_source_count(), 1);
    assert_eq!(d.model().source(old, 1).unwrap().bytes(), b"old retained source");
    assert_eq!(saved.import_generation(), Some(1));
    let accepted = saved.open_import_desk(&mut d, 1, 3, 1, 1, Side::Evidence, || false).unwrap();
    assert_eq!(d.model().selected_bytes(accepted.imported.change.active.unwrap(), 3).unwrap(), STATEMENT);
}

#[test]
fn corrupt_target_cannot_replace_the_exact_retained_import_evidence() {
    let f = Fixture::new(&[entry(b"pkg/helper.py", TARGET), entry(b"pkg/main.py", SOURCE)], true);
    let mut saved = f.open(57509); let mut d = desk(); prepare(&mut saved);
    let opened = saved.open_import_desk(&mut d, 0, 1, 1, 1, Side::Evidence, || false).unwrap();
    let pane = opened.imported.change.active.unwrap();
    let at = f.bytes.windows(TARGET.len()).position(|bytes| bytes == TARGET).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at as u64)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(saved.open_import_desk(&mut d, 1, 2, 1, 1, Side::Target, || false).is_err());
    assert_eq!(d.model().revision(), 1); assert_eq!(d.model().retained_source_count(), 1);
    assert_eq!(d.model().source(pane, 1).unwrap().bytes(), SOURCE);
    assert_eq!(d.model().selected_bytes(pane, 1).unwrap(), STATEMENT);
    assert_eq!(saved.import_generation(), Some(1));
}

