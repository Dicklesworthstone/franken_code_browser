#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Production captured docs -> source -> docs navigation. Regression source is
//! authored in the code-first campaign; the independent verifier owns execution.
mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, owner, entry};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}};
use fcb::document::links::MAX_DOCUMENT_LINK_SOURCE_BYTES;
use fcb::search::snapshot::{SnapshotData, SnapshotEntry};
use fcb::store::Sha256;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, reader::ReaderSession,
    desk::{DeskSession, DeskLimits, DeskCommand, DeskError, DeskSessionError},
    saved_repository::{SavedRepositorySession, desk::SavedDeskError,
        relationships::{RelationshipDirection as Direction, RelationshipSide as Side, SavedRelationshipError as Error}}};

fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn reads(saved: &mut SavedRepositorySession) -> (u64, u64) {
    let info = json(&saved.info(|| false).unwrap());
    (info.get("member_bytes_read").number(), info.get("member_read_calls").number())
}
fn copy_all(reader: &mut ReaderSession) -> Json {
    json(&reader.copy_range(0, reader.capture().bytes().len() as u64, || false).unwrap())
}
fn prepare(saved: &mut SavedRepositorySession, generation: u64) -> Json {
    json(&saved.prepare_relationships(generation, 64, 1024 * 1024, || false).unwrap())
}
fn whole(range: &Json, bytes: &[u8]) {
    assert_eq!(range.get("start").number(), 0);
    assert_eq!(range.get("end").number(), bytes.len() as u64);
}

#[test]
fn captured_links_and_reverse_backlinks_use_exact_manifest_members_and_occurrence_ids() {
    let readme = b"# Overview\n\n[implementation](src/a%20b.rs#L2) [again](src/a%20b.rs)\n\n[once](src/a%2520b.rs)\n\n[missing](src/missing.rs)\n\n[blocked](src/blocked.rs)\n\n[network](https://example.invalid/never-fetch)\n\n[escape](../outside.rs)\n\n[anchor](#overview)\n";
    let guide = b"[readme](../README.md#overview)\n\n[code](../src/a%20b.rs)\n";
    let f = Fixture::new(&[
        entry(b"README.md", readme), entry(b"docs/guide.md", guide),
        entry(b"src/a b.rs", b"fn captured() {}\n"),
        entry(b"src/a%20b.rs", b"percent name"),
        SnapshotEntry { path: b"src/blocked.rs", observed_bytes: 77, data: SnapshotData::Unavailable("READ_DENIED") },
    ], true);
    let mut saved = f.open(56201);
    let response = saved.prepare_relationships(7, 64, 1024 * 1024, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let result = json(&response);
    assert_eq!(result.get("relationship_generation").number(), 7);
    assert_eq!(result.get("projection").text(), "markdown-document-links");
    assert_eq!(result.get("evidence_precision").text(), "whole-captured-markdown-document");
    assert_eq!(result.get("resolver").text(), "fmd-exact-member-v1");
    assert_eq!(result.get("completeness_scope").text(), "parsed-document-links-in-saved-manifest");
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("scanned_markdown_members").number(), 2);
    assert_eq!(result.get("retained_links").number(), 10);
    assert_eq!(result.get("links_seen").number(), 10);
    assert_eq!(result.get("member_bytes_read").number(), (readme.len() + guide.len()) as u64);
    assert_eq!(result.get("retained_source_payload_bytes").number(), 0);
    let links = result.get("links").array();
    for (index, link) in links.iter().enumerate() {
        assert_eq!(link.get("link_id").number(), index as u64 + 1);
        let source_member = if index < 8 { 0 } else { 1 };
        assert_eq!(link.get("source_member").number(), source_member);
        assert_eq!(link.get("source_file_id").number(), source_member + 1);
        assert_eq!(link.get("source_revision").number(), source_member + 1);
        whole(link.get("evidence_range"), if index < 8 { readme.as_slice() } else { guide.as_slice() });
    }
    assert_eq!(links[0].get("target_member").number(), 2);
    assert_eq!(links[0].get("target_path").get("hex").text(), hex(b"src/a b.rs"));
    assert_eq!(links[0].get("target_suffix").text(), "#L2");
    assert_eq!(links[0].get("fragment_resolution").text(), "unresolved");
    assert_eq!(links[0].get("source_digest").text(), Sha256::digest(readme).to_hex());
    assert_eq!(links[1].get("target_member").number(), 2);
    assert_eq!(links[1].get("source_link_id").number(), 2);
    assert_eq!(links[2].get("target_member").number(), 3);
    assert_eq!(links[2].get("target_path").get("hex").text(), hex(b"src/a%20b.rs"));
    for (index, status) in [(3, "missing"), (4, "unavailable"), (5, "external"), (6, "unresolved")] {
        assert_eq!(links[index].get("resolution").text(), status);
    }
    assert_eq!(links[7].get("target_member").number(), 0);
    assert_eq!(links[7].get("fragment_resolution").text(), "unresolved");

    let before = reads(&mut saved);
    let first = json(&saved.relationship_page(7, Some(2), Direction::Incoming, 0, 2, || false).unwrap());
    assert_eq!(first.get("matching_links").number(), 3);
    assert_eq!(first.get("next_offset").number(), 2);
    assert_eq!(first.get("links").array().iter().map(|r| r.get("link_id").number()).collect::<Vec<_>>(), vec![1, 2]);
    let last = json(&saved.relationship_page(7, Some(2), Direction::Incoming, 2, 2, || false).unwrap());
    assert_eq!(last.get("links").array()[0].get("link_id").number(), 10);
    assert_eq!(last.get("next_offset"), &Json::Null);
    let outgoing = json(&saved.relationship_page(7, Some(1), Direction::Outgoing, 0, 10, || false).unwrap());
    assert_eq!(outgoing.get("matching_links").number(), 2);
    let either = json(&saved.relationship_page(7, Some(0), Direction::All, 0, 20, || false).unwrap());
    assert_eq!(either.get("matching_links").number(), 9);
    assert_eq!(reads(&mut saved), before);
}

#[test]
fn reader_navigation_keeps_original_bom_frontmatter_and_malformed_target_bytes_after_archive_close() {
    let source = b"\xef\xbb\xbf---\ntitle: Saved\n---\n\n[implementation](src/lib.rs#L4)\n";
    let code = b"fn archived() {}\r\n\0\xfforiginal\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"src/lib.rs", code)], true);
    let mut saved = f.open(56201);
    prepare(&mut saved, 1);
    let (mut evidence, response) = saved.open_relationship_reader(owner(56202), 1, 1, Side::Evidence, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("member").number(), 0);
    assert_eq!(receipt.get("source_digest").text(), Sha256::digest(source).to_hex());
    assert_eq!(receipt.get("offset").number(), 0);
    whole(receipt.get("selection"), source);
    assert_eq!(receipt.get("evidence_precision").text(), "whole-captured-markdown-document");
    assert!(!receipt.get("live_source_reopened").flag());
    assert_eq!(receipt.get("reader").get("owner").number(), 56202);
    assert_eq!(receipt.get("reader").get("capture_origin").text(), "host-supplied");
    assert_eq!(receipt.get("reader").get("initial_source_bytes_read").number(), 0);
    assert_eq!(receipt.get("member_bytes_read").number(), source.len() as u64);
    let (mut target, response) = saved.open_relationship_reader(owner(56203), 1, 1, Side::Target, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("member").number(), 1);
    assert_eq!(receipt.get("file_id").number(), 2);
    assert_eq!(receipt.get("source_revision").number(), 2);
    assert_eq!(receipt.get("source_digest").text(), Sha256::digest(code).to_hex());
    assert_eq!(receipt.get("offset").number(), 0);
    assert_eq!(receipt.get("selection"), &Json::Null);
    assert_eq!(receipt.get("fragment_resolution").text(), "unresolved");
    assert_eq!(receipt.get("reader").get("capture_sha256").text(), Sha256::digest(code).to_hex());
    assert_eq!(receipt.get("member_bytes_read").number(), code.len() as u64);
    assert_ne!(evidence.capture().request().file().owner(), target.capture().request().file().owner());
    saved.clear_relationships(2, || false).unwrap();
    drop(saved);
    fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
    assert_eq!(copy_all(&mut evidence).get("original_hex").text(), hex(source));
    assert_eq!(copy_all(&mut target).get("original_hex").text(), hex(code));
}

#[test]
fn stale_identity_invalid_filters_unavailable_and_unresolved_targets_fail_before_member_io() {
    let source = b"[ok](a.rs) [missing](missing.rs) [blocked](blocked.rs)";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"a.rs", b"source"),
        SnapshotEntry { path: b"blocked.rs", observed_bytes: 12, data: SnapshotData::Unavailable("READ_DENIED") }], true);
    let mut saved = f.open(56201);
    assert_eq!(saved.relationship_page(1, None, Direction::All, 0, 10, || false).err(), Some(Error::MissingRelationships));
    prepare(&mut saved, 1);
    let before = reads(&mut saved);
    for (generation, id, side, expected) in [
        (2, 1, Side::Evidence, Error::StaleGeneration),
        (1, 0, Side::Evidence, Error::MissingLink),
        (1, 4, Side::Target, Error::MissingLink),
        (1, 2, Side::Target, Error::UnresolvedTarget),
        (1, 3, Side::Target, Error::TargetUnavailable),
    ] {
        assert_eq!(saved.open_relationship_reader(owner(56202), generation, id, side, || false).err(), Some(expected));
    }
    assert_eq!(saved.open_relationship_reader(owner(56201), 1, 1, Side::Target, || false).err(), Some(Error::OwnerMismatch));
    assert!(saved.open_relationship_reader(owner(56202), 1, 1, Side::Target, || true).err().unwrap().is_canceled());
    for (member, direction, start, limit) in [
        (None, Direction::Incoming, 0, 10), (Some(99), Direction::Outgoing, 0, 10),
        (None, Direction::All, 4, 10), (None, Direction::All, 0, 0), (None, Direction::All, 0, 129),
    ] {
        assert_eq!(saved.relationship_page(1, member, direction, start, limit, || false).err(), Some(Error::InvalidLimits));
    }
    assert_eq!(reads(&mut saved), before);
}

#[test]
fn coverage_partitions_unavailable_unsupported_and_source_limited_documents_without_retaining_payloads() {
    let oversized = vec![b' '; MAX_DOCUMENT_LINK_SOURCE_BYTES + 1];
    let f = Fixture::new(&[
        entry(b"a.md", b"[code](b.rs)"), entry(b"b.rs", b"fn code() {}"), entry(b"c.md", &[0xff]),
        entry(b"d.md", &oversized),
        SnapshotEntry { path: b"e.md", observed_bytes: 50, data: SnapshotData::Unavailable("READ_DENIED") },
        entry(b"f.MARKDOWN", b""), entry(b"g\\bad.md", b"[code](b.rs)"),
    ], true);
    let mut saved = f.open(56201);
    let response = saved.prepare_relationships(1, 20, 1024 * 1024, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let result = json(&response);
    assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("markdown_members").number(), 6);
    assert_eq!(result.get("scanned_markdown_members").number(), 2);
    assert_eq!(result.get("unsupported_markdown_members").number(), 2);
    assert_eq!(result.get("source_limited_members").number(), 1);
    assert_eq!(result.get("unavailable_markdown_members").number(), 1);
    assert_eq!(result.get("member_bytes_read").number(), b"[code](b.rs)".len() as u64 + 1);
    assert_eq!(result.get("retained_source_payload_bytes").number(), 0);
    assert_eq!(result.get("retained_links").number(), 1);
    let response = saved.prepare_relationships(2, 20, 0, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let probe = json(&response);
    assert_eq!(probe.get("member_bytes_read").number(), 0);
    assert_eq!(probe.get("retained_links").number(), 0);
    assert_eq!(probe.get("scanned_markdown_members").number(), 1); // Captured empty Markdown is valid.
    assert_eq!(probe.get("source_limited_members").number(), 4);
    assert_eq!(probe.get("unavailable_markdown_members").number(), 1);
}

#[test]
fn display_cap_preserves_total_occurrence_count_and_saved_discovery_partiality() {
    let f = Fixture::new(&[entry(b"a.md", b"[a](code.rs) [b](code.rs) [c](code.rs)"),
        entry(b"code.rs", b"code")], false);
    let mut saved = f.open(56201);
    let response = saved.prepare_relationships(1, 1, 1024, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let result = json(&response);
    assert!(result.get("truncated").flag());
    assert!(!result.get("relationships_complete").flag());
    assert_eq!(result.get("links_seen").number(), 3);
    assert_eq!(result.get("retained_links").number(), 1);
    assert_eq!(result.get("links").array()[0].get("source_link_id").number(), 1);
    assert_eq!(result.get("links").array()[0].get("link_id").number(), 1);
    let page = json(&saved.relationship_page(1, None, Direction::All, 1, 10, || false).unwrap());
    assert!(page.get("links").array().is_empty());
    assert_eq!(page.get("next_offset"), &Json::Null);
    prepare(&mut saved, 2);
    let page = json(&saved.relationship_page(2, None, Direction::All, 0, 10, || false).unwrap());
    assert!(!page.get("truncated").flag());
    assert!(!page.get("relationships_complete").flag()); // The captured manifest itself is partial.
    assert_eq!(page.get("retained_links").number(), 3);
}

#[test]
fn failed_and_final_checkpoint_canceled_replacements_preserve_old_projection_and_literal_query() {
    let f = Fixture::new(&[entry(b"README.md", b"[needle](a.rs)"), entry(b"a.rs", b"needle")], true);
    let mut probe = f.open(56202);
    prepare(&mut probe, 1);
    let mut polls = 0;
    probe.prepare_relationships(2, 64, 1024 * 1024, || { polls += 1; false }).unwrap();
    assert!(polls > 1);
    let mut saved = f.open(56201);
    saved.search(1, "needle", 20, || false).unwrap();
    prepare(&mut saved, 1);
    let old = json(&saved.relationship_page(1, None, Direction::All, 0, 64, || false).unwrap());
    let mut seen = 0;
    let canceled = saved.prepare_relationships(2, 64, 1024 * 1024, || { seen += 1; seen == polls });
    assert!(matches!(canceled, Err(error) if error.is_canceled()));
    assert_eq!(saved.relationship_generation(), Some(1));
    assert_eq!(json(&saved.relationship_page(1, None, Direction::All, 0, 64, || false).unwrap()), old);
    assert_eq!(saved.prepare_relationships(2, 64, 1024, || false).err(), Some(Error::StaleGeneration));
    assert_eq!(saved.prepare_relationships(3, 0, 1024, || false).err(), Some(Error::InvalidLimits));
    assert_eq!(saved.relationship_generation(), Some(1));
    assert_eq!(saved.accepted_generation(), Some(1));
    saved.results(1, 0, 20, || false).unwrap();
    assert!(saved.clear_relationships(4, || true).err().unwrap().is_canceled());
    assert_eq!(saved.relationship_generation(), Some(1));
    saved.clear_relationships(5, || false).unwrap();
    assert_eq!(saved.relationship_generation(), None);
    assert_eq!(saved.accepted_generation(), Some(1));
    assert_eq!(saved.relationship_page(1, None, Direction::All, 0, 20, || false).err(), Some(Error::MissingRelationships));
    assert_eq!(json(&saved.info(|| false).unwrap()).get("accepted_relationship_generation"), &Json::Null);
}

#[test]
fn corruption_cannot_reopen_a_changed_member_or_replace_previously_accepted_relationship_metadata() {
    let source = b"# Unique captured document\n\n[original](src.rs)\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"src.rs", b"original source")], true);
    let mut saved = f.open(56201);
    prepare(&mut saved, 1);
    let (mut retained, _) = saved.open_relationship_reader(owner(56202), 1, 1, Side::Evidence, || false).unwrap();
    let before = json(&saved.relationship_page(1, None, Direction::All, 0, 20, || false).unwrap());
    let at = f.bytes.windows(source.len()).position(|b| b == source).unwrap();
    let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
    writer.seek(SeekFrom::Start(at as u64)).unwrap();
    writer.write_all(b"X").unwrap(); writer.flush().unwrap();
    assert!(saved.open_relationship_reader(owner(56203), 1, 1, Side::Evidence, || false).is_err());
    assert!(saved.prepare_relationships(2, 64, 1024 * 1024, || false).is_err());
    assert_eq!(saved.relationship_generation(), Some(1));
    assert_eq!(json(&saved.relationship_page(1, None, Direction::All, 0, 20, || false).unwrap()), before);
    assert_eq!(copy_all(&mut retained).get("original_hex").text(), hex(source));
}

#[test]
fn relationship_desk_navigation_enters_normal_history_and_refused_actions_keep_original_place() {
    let source = b"# Documentation\n\n[code](src.rs#L2) [missing](absent.rs)\n";
    let code = b"fn original() {}\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"src.rs", code)], true);
    let mut saved = f.open(56201);
    let mut desk = DeskSession::new(owner(56202), DeskLimits::default()).unwrap();
    prepare(&mut saved, 1);
    let evidence = saved.open_relationship_desk(&mut desk, 0, 1, 1, 1, Side::Evidence, || false).unwrap();
    let pane = evidence.imported.change.active.unwrap();
    assert_eq!(desk.model().source(pane, 1).unwrap().bytes(), source);
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), source);
    let receipt = json(&saved.desk_open_response(&desk, &evidence).unwrap());
    assert_eq!(receipt.get("relationship_generation").number(), 1);
    assert_eq!(receipt.get("relationship_link_id").number(), 1);
    assert_eq!(receipt.get("relationship_side").text(), "evidence");
    assert_eq!(receipt.get("selection_namespace").text(), "saved-relationship");
    whole(receipt.get("selection"), source);
    let before = reads(&mut saved);
    assert_eq!(saved.open_relationship_desk(&mut desk, 0, 2, 1, 1, Side::Target, || false).err(),
        Some(SavedDeskError::Desk(DeskSessionError::Desk(DeskError::StaleRevision))));
    assert_eq!(saved.open_relationship_desk(&mut desk, 1, 2, 1, 2, Side::Target, || false).err(),
        Some(SavedDeskError::Relationship(Error::UnresolvedTarget)));
    assert_eq!(reads(&mut saved), before);
    assert_eq!(desk.model().revision(), 1);
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), source);
    let target = saved.open_relationship_desk(&mut desk, 1, 2, 1, 1, Side::Target, || false).unwrap();
    assert_eq!(target.selection(), None);
    let active = target.imported.change.active.unwrap();
    assert_eq!(desk.model().source(active, 2).unwrap().bytes(), code);
    let receipt = json(&saved.desk_open_response(&desk, &target).unwrap());
    assert_eq!(receipt.get("relationship_side").text(), "target");
    assert_eq!(receipt.get("fragment_resolution").text(), "unresolved");
    drop(saved);
    fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
    desk.apply(2, 3, DeskCommand::Back, || false).unwrap();
    let original = desk.model().active().unwrap();
    assert_eq!(desk.model().source(original, 3).unwrap().bytes(), source);
    assert_eq!(desk.model().selected_bytes(original, 3).unwrap(), source);
}

#[test]
fn hostile_reference_amplification_is_explicit_partial_coverage_while_other_documents_remain_usable() {
    let amplified = format!("[r]: a.rs \"{}\"\n\n{}", "x".repeat(128 * 1024), "[r]\n".repeat(30_000));
    assert!(amplified.len() < MAX_DOCUMENT_LINK_SOURCE_BYTES);
    let f = Fixture::new(&[entry(b"a.md", b"[code](code.rs)"), entry(b"b.md", amplified.as_bytes()),
        entry(b"code.rs", b"captured code")], true);
    let mut saved = f.open(56201);
    let response = saved.prepare_relationships(1, 10, 1024 * 1024, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let result = json(&response);
    assert_eq!(result.get("markdown_members").number(), 2);
    assert_eq!(result.get("scanned_markdown_members").number(), 1);
    assert_eq!(result.get("unsupported_markdown_members").number(), 1);
    assert_eq!(result.get("retained_links").number(), 1);
    assert_eq!(result.get("retained_source_payload_bytes").number(), 0);
    assert!(!result.get("relationships_complete").flag());
    let (mut reader, _) = saved.open_relationship_reader(owner(56202), 1, 1, Side::Target, || false).unwrap();
    assert_eq!(copy_all(&mut reader).get("original_hex").text(), hex(b"captured code"));
}
