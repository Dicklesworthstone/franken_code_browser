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
    // Heading and source-line targets each share one additional qualification pass.
    assert_eq!(result.get("member_bytes_read").number(), (2 * readme.len() + guide.len() + b"fn captured() {}\n".len()) as u64);
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
    assert_eq!(links[0].get("fragment_resolution").text(), "missing-line");
    assert_eq!(links[0].get("source_digest").text(), Sha256::digest(readme).to_hex());
    assert_eq!(links[1].get("target_member").number(), 2);
    assert_eq!(links[1].get("source_link_id").number(), 2);
    assert_eq!(links[2].get("target_member").number(), 3);
    assert_eq!(links[2].get("target_path").get("hex").text(), hex(b"src/a%20b.rs"));
    for (index, status) in [(3, "missing"), (4, "unavailable"), (5, "external"), (6, "unresolved")] {
        assert_eq!(links[index].get("resolution").text(), status);
    }
    assert_eq!(links[7].get("target_member").number(), 0);
    assert_eq!(links[7].get("fragment_resolution").text(), "resolved");
    assert_eq!(links[7].get("fragment_slug").text(), "overview");
    assert_eq!(links[7].get("target_range").get("start").number(), 0);

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
    assert_eq!(receipt.get("fragment_resolution").text(), "missing-line");
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
    assert_eq!(receipt.get("fragment_resolution").text(), "missing-line");
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

#[test]
fn encoded_canonical_headings_open_exact_bom_crlf_ranges_in_readers_and_desk() {
    let source = b"# Local\n\n[guide](docs/guide.md#caf-%64etails) [query](docs/guide.md?view=source#caf-%64etails) [self](#local)\n";
    let guide = format!("\u{feff}{}\r\n\r\n## Café Details\r\n\r\nBody.\r\n", "x".repeat(200));
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"docs/guide.md", guide.as_bytes())], true);
    let mut saved = f.open(56201);
    let result = prepare(&mut saved, 1);
    let links = result.get("links").array();
    let range = links[0].get("target_range");
    let start = range.get("start").number(); let end = range.get("end").number();
    assert_eq!(links[0].get("fragment_resolution").text(), "resolved");
    assert_eq!(links[0].get("fragment_slug").text(), "caf-details");
    assert_eq!(start, 207); // Three BOM bytes, 200 ASCII bytes, two CRLFs.
    assert_eq!(guide[start as usize..end as usize].trim_end(), "## Café Details");
    assert_eq!(links[1].get("target_range"), range);
    assert_eq!(links[1].get("target_suffix").text(), "?view=source#caf-%64etails");
    assert_eq!(links[2].get("fragment_resolution").text(), "resolved");
    assert_eq!(links[2].get("target_range").get("start").number(), 0);
    // Repeated references never reread a target per occurrence; both source
    // members have one extraction pass and one shared heading pass.
    assert_eq!(result.get("member_bytes_read").number(), 2 * (source.len() + guide.len()) as u64);
    let before = reads(&mut saved);
    saved.relationship_page(1, Some(1), Direction::Incoming, 0, 64, || false).unwrap();
    assert_eq!(reads(&mut saved), before);

    let (mut reader, response) = saved.open_relationship_reader(owner(56202), 1, 1, Side::Target, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("offset").number(), start);
    assert_eq!(receipt.get("selection"), range);
    assert_eq!(receipt.get("target_range"), range);
    assert_eq!(receipt.get("fragment_slug").text(), "caf-details");
    assert_eq!(receipt.get("window").get("visible_range").get("start").number(), start - 128);
    assert_eq!(receipt.get("source_digest").text(), Sha256::digest(guide.as_bytes()).to_hex());
    let (mut evidence, response) = saved.open_relationship_reader(owner(56203), 1, 1, Side::Evidence, || false).unwrap();
    whole(json(&response).get("selection"), source);
    assert_eq!(copy_all(&mut evidence).get("original_hex").text(), hex(source));
    let mut desk = DeskSession::new(owner(56204), DeskLimits::default()).unwrap();
    let opened = saved.open_relationship_desk(&mut desk, 0, 1, 1, 1, Side::Target, || false).unwrap();
    let pane = opened.imported.change.active.unwrap();
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), &guide.as_bytes()[start as usize..end as usize]);
    assert_eq!(json(&saved.desk_open_response(&desk, &opened).unwrap()).get("fragment_resolution").text(), "resolved");

    prepare(&mut saved, 2);
    let before = reads(&mut saved);
    assert_eq!(saved.open_relationship_reader(owner(56205), 1, 1, Side::Target, || false).err(), Some(Error::StaleGeneration));
    assert_eq!(reads(&mut saved), before);
    drop(saved);
    fs::rename(&f.path, f.root.join("moved-heading.fcbs")).unwrap();
    assert_eq!(json(&reader.copy_range(start, end, || false).unwrap()).get("original_hex").text(),
        hex(&guide.as_bytes()[start as usize..end as usize]));
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), &guide.as_bytes()[start as usize..end as usize]);
    assert!(!f.root.join("docs/guide.md").exists());
}

#[test]
fn fragment_outcomes_never_confuse_captured_files_with_qualified_headings() {
    let source = b"[ok](guide.md#repeat) [collision](guide.md#repeat-2) [missing](guide.md#absent) \
        [once](guide.md#%2572epeat) [case](guide.md#Repeat) [plus](guide.md#repeat+two) \
        [invalid](guide.md#%ff) [zero](guide.md#%00) [top](guide.md#) \
        [code](code.rs#L2) [missing-file](absent.md#repeat) [blocked](blocked.md#repeat)\n";
    // Upstream counts repeated bases independently. The repeated "Repeat"
    // suffix and the explicit "Repeat-2" heading collide; selecting the first
    // would falsely claim a unique destination.
    let guide = b"# Repeat\n\n# Repeat\n\n# Repeat-2\n\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"guide.md", guide), entry(b"code.rs", b"code\n"),
        SnapshotEntry { path: b"blocked.md", observed_bytes: 12, data: SnapshotData::Unavailable("READ_DENIED") }], true);
    let mut saved = f.open(56201);
    let result = prepare(&mut saved, 1);
    let links = result.get("links").array();
    assert_eq!(links.len(), 12);
    for (index, expected) in ["resolved", "ambiguous", "missing-heading", "missing-heading", "missing-heading",
        "missing-heading", "invalid", "invalid", "document-start", "missing-line", "unresolved", "unresolved"].iter().enumerate() {
        assert_eq!(links[index].get("fragment_resolution").text(), *expected, "link {}", index + 1);
        if index > 0 { assert_eq!(links[index].get("target_range"), &Json::Null); }
    }
    assert_eq!(links[3].get("fragment_slug").text(), "%72epeat");
    assert_eq!(links[5].get("fragment_slug").text(), "repeat+two");
    assert_eq!(links[10].get("resolution").text(), "missing");
    assert_eq!(links[11].get("resolution").text(), "unavailable");
    for id in 2..=10 {
        let (_, response) = saved.open_relationship_reader(owner(56300 + id), 1, id, Side::Target, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("offset").number(), 0);
        assert_eq!(receipt.get("selection"), &Json::Null);
        assert_eq!(receipt.get("fragment_resolution"), links[(id - 1) as usize].get("fragment_resolution"));
    }
}

#[test]
fn saved_source_line_targets_share_qualification_and_preserve_exact_reader_and_desk_selection() {
    let source = b"[last](src.rs#L4) [prefix](src.rs#L1-L3) [range](src.rs#L2-L3) [same](src.rs#L2) \
        [missing-end](src.rs#L2-L5) [zero](src.rs#L01) [reverse](src.rs#L3-L2) \
        [named](src.rs#License) [decoded](src.rs#%4C2) [once](src.rs#%254C2)";
    let encode = |text: &str| -> Vec<u8> { text.encode_utf16().flat_map(u16::to_le_bytes).collect() };
    let prefix = "\u{feff}head\r";
    let second = "\u{010a}🦀\r\n";
    let third = "third\r";
    let bytes = encode(&format!("{prefix}{second}{third}last"));
    let start = encode(prefix).len() as u64;
    let second_end = start + encode(second).len() as u64;
    let end = second_end + encode(third).len() as u64;
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"src.rs", &bytes)], true);
    let mut saved = f.open(56501);
    let result = prepare(&mut saved, 1);
    assert!(result.get("relationships_complete").flag());
    assert_eq!(result.get("member_bytes_read").number(), (source.len() + bytes.len()) as u64);
    assert_eq!(result.get("retained_source_payload_bytes").number(), 0);
    let links = result.get("links").array();
    for (index, expected) in ["resolved-lines", "resolved-lines", "resolved-lines", "resolved-lines",
        "missing-line", "invalid", "invalid", "unsupported", "resolved-lines", "unsupported"].iter().enumerate() {
        assert_eq!(links[index].get("fragment_resolution").text(), *expected, "link {}", index + 1);
        if *expected == "invalid" { assert_eq!(links[index].get("fragment_slug"), &Json::Null); }
        if *expected != "resolved-lines" { assert_eq!(links[index].get("target_range"), &Json::Null); }
    }
    for (index, first, last) in [(0, end, bytes.len() as u64), (1, 0, end),
        (2, start, end), (3, start, second_end), (8, start, second_end)] {
        let range = links[index].get("target_range");
        assert_eq!(range.get("start").number(), first);
        assert_eq!(range.get("end").number(), last);
    }
    assert_eq!(links[8].get("fragment_slug").text(), "L2");
    assert_eq!(links[9].get("fragment_slug").text(), "%4C2");
    let (mut reader, response) = saved.open_relationship_reader(owner(56502), 1, 3, Side::Target, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("fragment_resolution").text(), "resolved-lines");
    assert_eq!(receipt.get("selection"), links[2].get("target_range"));
    assert_eq!(receipt.get("offset").number(), start);
    assert_eq!(receipt.get("window").get("requested_original_range").get("start").number(), start.saturating_sub(128));
    assert!(!receipt.get("live_source_reopened").flag());
    let mut desk = DeskSession::new(owner(56503), DeskLimits::default()).unwrap();
    let opened = saved.open_relationship_desk(&mut desk, 0, 1, 1, 3, Side::Target, || false).unwrap();
    let pane = opened.imported.change.active.unwrap();
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), &bytes[start as usize..end as usize]);
    assert_eq!(json(&saved.desk_open_response(&desk, &opened).unwrap()).get("fragment_resolution").text(), "resolved-lines");
    drop(saved);
    fs::rename(&f.path, f.root.join("moved-lines.fcbs")).unwrap();
    assert_eq!(json(&reader.copy_range(start, end, || false).unwrap()).get("original_hex").text(), hex(&bytes[start as usize..end as usize]));
    assert_eq!(desk.model().selected_bytes(pane, 1).unwrap(), &bytes[start as usize..end as usize]);
}

#[test]
fn saved_source_line_budget_refusal_is_not_missing_and_retry_keeps_capture_authority() {
    let source = b"[first](large.rs#L2) [overlap](large.rs#L1-L2) [absent](large.rs#L3)";
    let mut bytes = vec![b'x'; MAX_DOCUMENT_LINK_SOURCE_BYTES + 11];
    bytes.extend_from_slice(b"\nlast\n");
    let start = bytes.len() - b"last\n".len();
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"large.rs", &bytes)], true);
    let mut saved = f.open(56510);
    let limited = json(&saved.prepare_relationships(1, 64, source.len() as u64, || false).unwrap());
    assert!(limited.get("relationships_complete").flag());
    assert_eq!(limited.get("member_bytes_read").number(), source.len() as u64);
    for link in limited.get("links").array() {
        assert_eq!(link.get("fragment_resolution").text(), "limited");
        assert_eq!(link.get("target_range"), &Json::Null);
    }
    let accepted = prepare(&mut saved, 2);
    let links = accepted.get("links").array();
    assert_eq!(accepted.get("member_bytes_read").number(), (source.len() + bytes.len()) as u64);
    assert_eq!(links[0].get("fragment_resolution").text(), "resolved-lines");
    assert_eq!(links[0].get("target_range").get("start").number(), start as u64);
    assert_eq!(links[0].get("target_range").get("end").number(), bytes.len() as u64);
    assert_eq!(links[1].get("fragment_resolution").text(), "resolved-lines");
    whole(links[1].get("target_range"), &bytes);
    assert_eq!(links[2].get("fragment_resolution").text(), "missing-line");
    assert_eq!(links[2].get("target_range"), &Json::Null);
}

#[test]
fn heading_source_and_flow_limits_are_not_reported_as_missing_destinations() {
    let source = b"[heading](guide.md#last)";
    let guide = b"# Last\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"guide.md", guide)], true);
    let mut saved = f.open(56201);
    let cap = (source.len() + guide.len()) as u64;
    let result = json(&saved.prepare_relationships(1, 64, cap, || false).unwrap());
    assert!(result.get("relationships_complete").flag()); // Link extraction completed; heading qualification did not.
    assert_eq!(result.get("member_bytes_read").number(), cap);
    assert_eq!(result.get("links").array()[0].get("resolution").text(), "captured");
    assert_eq!(result.get("links").array()[0].get("fragment_resolution").text(), "limited");
    let result = prepare(&mut saved, 2);
    assert_eq!(result.get("links").array()[0].get("fragment_resolution").text(), "resolved");
    let full = format!("{}# Last\n", "paragraph\n\n".repeat(4100));
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"guide.md", full.as_bytes())], true);
    let mut saved = f.open(56202);
    let result = prepare(&mut saved, 1);
    assert_eq!(result.get("links").array()[0].get("fragment_resolution").text(), "limited");
    assert_eq!(result.get("links").array()[0].get("target_range"), &Json::Null);
    let amplified = format!("[r]: x \"{}\"\n\n{}", "x".repeat(128 * 1024), "[r]\n".repeat(30_000));
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"guide.md", amplified.as_bytes())], true);
    let mut saved = f.open(56203);
    let result = prepare(&mut saved, 1);
    assert_eq!(result.get("unsupported_markdown_members").number(), 1);
    assert_eq!(result.get("links").array()[0].get("fragment_resolution").text(), "limited");
    assert_eq!(result.get("links").array()[0].get("target_range"), &Json::Null);
}

#[test]
fn upstream_span_mismatches_and_missing_nested_heading_catalogs_keep_explicit_file_fallback() {
    let source = b"[target](guide.md#target)";
    let unsupported = [
        "\u{feff}---\ntitle: Guide\n---\n\n# Target\n",
        "> target\ntarget\n\n# target\n",
        "p\n    target\n\n# target\n",
        "$$\n# target\n$$\n\n# target\n",
        "Term\n: first\n\nTarget\n: second\n\n# Target\n",
        "> # target\n",
    ];
    for (case, guide) in unsupported.iter().enumerate() {
        let f = Fixture::new(&[entry(b"README.md", source), entry(b"guide.md", guide.as_bytes())], true);
        let mut saved = f.open(56201 + case as u64 * 2);
        let result = prepare(&mut saved, 1);
        let link = &result.get("links").array()[0];
        assert_eq!(link.get("resolution").text(), "captured", "case {case}");
        assert_eq!(link.get("fragment_resolution").text(), "unsupported", "case {case}");
        assert_eq!(link.get("target_range"), &Json::Null);
        let (mut reader, response) = saved.open_relationship_reader(owner(56202 + case as u64 * 2),
            1, 1, Side::Target, || false).unwrap();
        assert_eq!(json(&response).get("offset").number(), 0);
        assert_eq!(json(&response).get("selection"), &Json::Null);
        assert_eq!(copy_all(&mut reader).get("original_hex").text(), hex(guide.as_bytes()));
    }
    // A real fenced block and ordinary list remain supported. The fake heading
    // inside the fence must not displace the actual following heading's range.
    let guide = b"```text\n# target\n```\n\n- ordinary list\n\n# Target\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"guide.md", guide)], true);
    let mut saved = f.open(56301);
    let result = prepare(&mut saved, 1);
    let link = &result.get("links").array()[0];
    assert_eq!(link.get("fragment_resolution").text(), "resolved");
    let start = link.get("target_range").get("start").number() as usize;
    assert_eq!(&guide[start..], b"# Target\n");
}
