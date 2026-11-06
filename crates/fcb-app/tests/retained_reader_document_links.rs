#![forbid(unsafe_code)]

//! Public retained-reader link composition: upstream occurrences, bounded
//! inventory, exact capture identity, and qualified same-document navigation.
//! These tests do not qualify native presentation or live-root confinement.

mod support;
use support::{Json, parse};
use std::path::Path;
use fcb::ArenaOwnerId;
use fcb::document::{links::{DocumentLinkError, MAX_DOCUMENT_LINKS,
    MAX_DOCUMENT_LINK_DESTINATION_BYTES}, reader::DocumentReadError};
use fcb_app::{EXIT_OK, EXIT_PARTIAL};
use fcb_app::host::{HostResponse, reader::{ReaderSession, ReaderSessionError, ReaderDocumentOptions}};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn reader(bytes: &[u8]) -> ReaderSession {
    let mut reader = ReaderSession::from_bytes(owner(31901), Path::new("ungranted/README.md"), bytes, || false).unwrap();
    reader.prepare_document(1, Default::default(), || false).unwrap();
    reader
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn absent(value: &Json, field: &str) -> bool {
    matches!(value, Json::Object(fields) if !fields.contains_key(field))
}
fn stale() -> ReaderSessionError { ReaderSessionError::Document(DocumentReadError::StaleGeneration) }

#[test]
fn parser_occurrences_keep_whole_capture_evidence_and_stable_paging() {
    let source = b"\xef\xbb\xbf# Links\n\n[first](#details) [second](#details) [local](../src/lib.rs) [web](https://example.invalid/) [reference][target]\n\n`[not a link](code.rs)`\n\n![image](image.png)\n\n## Details\n\n[target]: #details\n";
    let mut reader = reader(source);
    let digest = reader.capture_sha256();
    assert_eq!(reader.document_links(1, 0, 2, || false).err(), Some(ReaderSessionError::MissingDocumentLinks));
    let response = reader.prepare_document_links(1, 16, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let prepared = json(&response);
    assert_eq!(prepared.get("command").text(), "document-links-prepare");
    assert!(prepared.get("links_ready").flag()); assert!(prepared.get("links_complete").flag());
    assert!(!prepared.get("links_truncated").flag());
    assert_eq!(prepared.get("links_seen").number(), 5);
    assert_eq!(prepared.get("retained_links").number(), 5);
    assert_eq!(prepared.get("parser_diagnostics").number(), 0);
    assert_eq!(prepared.get("evidence_precision").text(), "whole-captured-markdown-document");
    let destinations = ["#details", "#details", "../src/lib.rs", "https://example.invalid/", "#details"];
    for (index, (row, destination)) in prepared.get("links").array().iter().zip(destinations).enumerate() {
        assert_eq!(row.get("link_id").number(), index as u64 + 1);
        assert_eq!(row.get("destination").text(), destination);
        assert_eq!(row.get("evidence_range").get("start").number(), 0);
        assert_eq!(row.get("evidence_range").get("end").number(), source.len() as u64);
        assert_eq!(reader.document_link_destination(1, index as u64 + 1).unwrap(), destination);
    }
    let page = json(&reader.document_links(1, 1, 2, || false).unwrap());
    assert_eq!(page.get("command").text(), "document-links");
    assert_eq!(page.get("first_link").number(), 1); assert_eq!(page.get("next_link").number(), 3);
    assert_eq!(page.get("links").array(), &prepared.get("links").array()[1..3]);
    assert_eq!(page.get("additional_source_bytes_read").number(), 0);
    let end = json(&reader.document_links(1, 5, 2, || false).unwrap());
    assert!(end.get("links").array().is_empty()); assert_eq!(end.get("next_link"), &Json::Null);
    assert_eq!(reader.document_links(1, 6, 2, || false).err(), Some(ReaderSessionError::InvalidRange));
    for count in [0, 129] {
        assert_eq!(reader.document_links(1, 0, count, || false).err(), Some(ReaderSessionError::InvalidLimits));
    }
    assert_eq!(reader.capture().bytes(), source); assert_eq!(reader.capture_sha256(), digest);
    let info = json(&reader.info(|| false).unwrap());
    assert_eq!(info.get("capture_sha256").text(), digest.to_hex());
    assert_eq!(info.get("accepted_document_links_generation").number(), 1);
}

#[test]
fn percent_decoded_heading_and_document_start_use_the_existing_capture() {
    let source = b"\xef\xbb\xbf# Intro\n\n[go](#det%61ils) [top](#)\n\n## Details\n\nOriginal retained text.\n";
    let mut reader = reader(source);
    let inventory = json(&reader.prepare_document_links(1, 16, || false).unwrap());
    assert_eq!(inventory.get("links").array()[0].get("fragment_slug").text(), "details");
    assert_eq!(inventory.get("links").array()[0].get("kind").text(), "same-document-heading");
    let response = reader.document_link(1, 1, 3, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_OK);
    let heading = json(&response);
    assert_eq!(heading.get("command").text(), "document-link"); assert!(heading.get("activated").flag());
    assert_eq!(heading.get("link").get("link_id").number(), 1);
    assert_eq!(heading.get("heading_slug").text(), "details");
    let direct = json(&reader.document_at_heading(1, "details", 3, || false).unwrap());
    assert_eq!(heading.get("heading_original_range"), direct.get("heading_original_range"));
    assert_eq!(heading.get("first_flow_line"), direct.get("first_flow_line"));
    assert_eq!(heading.get("flow_lines"), direct.get("flow_lines"));
    assert_eq!(heading.get("additional_source_bytes_read").number(), 0);
    let start = json(&reader.document_link(1, 2, 2, || false).unwrap());
    assert!(start.get("activated").flag()); assert_eq!(start.get("first_flow_line").number(), 0);
    assert_eq!(start.get("link").get("kind").text(), "same-document-start");
    assert!(absent(&start, "heading_original_range"));
    assert_eq!(reader.capture().bytes(), source);
}

#[test]
fn display_limits_and_oversized_destinations_do_not_renumber_occurrences() {
    let source = format!("# Target\n\n[omitted]({}) [kept](#target)\n",
        "x".repeat(MAX_DOCUMENT_LINK_DESTINATION_BYTES + 1));
    let mut reader = reader(source.as_bytes());
    let response = reader.prepare_document_links(1, 1, || false).unwrap();
    assert_eq!(response.exit_code(), EXIT_PARTIAL);
    let limited = json(&response);
    assert_eq!(limited.get("links_seen").number(), 2); assert_eq!(limited.get("retained_links").number(), 1);
    assert!(limited.get("links_truncated").flag()); assert!(!limited.get("links_complete").flag());
    assert_eq!(limited.get("links").array()[0].get("link_id").number(), 2);
    assert_eq!(reader.document_link_destination(1, 1).err(), Some(ReaderSessionError::MissingDocumentLink));
    assert_eq!(reader.document_link_destination(1, 2).unwrap(), "#target");
    assert!(json(&reader.document_link(1, 2, 2, || false).unwrap()).get("activated").flag());
    let empty = json(&reader.prepare_document_links(1, 0, || false).unwrap());
    assert_eq!(empty.get("links_seen").number(), 2); assert_eq!(empty.get("retained_links").number(), 0);
    assert!(empty.get("links_truncated").flag()); assert!(empty.get("links").array().is_empty());
    assert_eq!(reader.document_link(1, 2, 2, || false).err(), Some(ReaderSessionError::MissingDocumentLink));
}

#[test]
fn failed_preparation_and_reflow_preserve_links_but_successful_reflow_and_clear_revoke_them() {
    let mut reader = reader(b"# Target\n\n[go](#target)\n");
    reader.prepare_document_links(1, 16, || false).unwrap();
    let accepted = reader.document_links(1, 0, 16, || false).unwrap();
    assert_eq!(reader.prepare_document_links(1, MAX_DOCUMENT_LINKS + 1, || false).err(),
        Some(ReaderSessionError::DocumentLinks(DocumentLinkError::InvalidLimits)));
    for cancel_at in 1..=12 {
        let mut polls = 0;
        assert_eq!(reader.prepare_document_links(1, 1,
            || { polls += 1; polls == cancel_at }).err(), Some(ReaderSessionError::Canceled));
        assert_eq!(reader.document_links(1, 0, 16, || false).unwrap().as_str(), accepted.as_str());
    }
    assert_eq!(reader.prepare_document(2, ReaderDocumentOptions { width_columns: 1, ..Default::default() }, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::InvalidLimits)));
    assert_eq!(reader.document_links(1, 0, 16, || false).unwrap().as_str(), accepted.as_str());
    reader.prepare_document(3, Default::default(), || false).unwrap();
    assert_eq!(reader.document_links_generation(), None);
    assert_eq!(reader.document_link_destination(1, 1).err(), Some(stale()));
    assert_eq!(reader.document_links(3, 0, 16, || false).err(), Some(ReaderSessionError::MissingDocumentLinks));
    reader.prepare_document_links(3, 16, || false).unwrap();
    reader.clear_document(4, || false).unwrap();
    assert_eq!(reader.document_generation(), None); assert_eq!(reader.document_links_generation(), None);
    assert_eq!(reader.document_links(3, 0, 16, || false).err(), Some(ReaderSessionError::MissingDocument));
}

#[test]
fn supplied_labels_local_paths_and_external_urls_never_open_a_source() {
    let source = b"# Links\n\n[local](../src/lib.rs) [web](https://example.invalid/) [file](file:///private/source.md) [script](javascript:run) [query](?view=raw) [bad](#bad%ZZ) [nul](#bad%00) [utf8](#bad%FF)\n";
    let mut reader = reader(source);
    let prepared = json(&reader.prepare_document_links(1, 16, || false).unwrap());
    let kinds = ["local-reference", "external", "external", "external",
        "unsupported", "unsupported", "unsupported", "unsupported"];
    assert_eq!(prepared.get("links").array().len(), kinds.len());
    for (index, kind) in kinds.into_iter().enumerate() {
        assert_eq!(prepared.get("links").array()[index].get("kind").text(), kind);
        let response = reader.document_link(1, index as u64 + 1, 2, || false).unwrap();
        assert_eq!(response.exit_code(), EXIT_PARTIAL);
        let inert = json(&response);
        assert!(!inert.get("activated").flag()); assert!(absent(&inert, "flow_lines"));
        assert_eq!(inert.get("additional_source_bytes_read").number(), 0);
        assert_eq!(reader.capture().bytes(), source);
    }
    assert_eq!(reader.document_generation(), Some(1)); assert_eq!(reader.document_links_generation(), Some(1));
}

#[test]
fn destination_labels_escape_bidi_without_changing_parser_destination_bytes() {
    let source = "# Links\n\n[local](dir/ab\u{202e}cd.rs)\n";
    let mut reader = reader(source.as_bytes());
    let prepared = json(&reader.prepare_document_links(1, 16, || false).unwrap());
    let link = &prepared.get("links").array()[0];
    assert_eq!(link.get("destination").text(), "dir/ab\u{202e}cd.rs");
    assert_eq!(link.get("destination_display").text(), "dir/ab\\u{202e}cd.rs");
    assert!(!json(&reader.document_link(1, 1, 2, || false).unwrap()).get("activated").flag());
}

#[test]
fn unqualified_or_ambiguous_headings_are_not_missing_and_leave_inventory_usable() {
    for source in [
        "---\ntitle: Metadata\n---\n\n# Target\n\n[go](#target)\n",
        "# Same\n\n# Same\n\n# Same-2\n\n[go](#same-2)\n",
        "> # Nested\n\n[go](#nested)\n",
    ] {
        let mut reader = reader(source.as_bytes());
        reader.prepare_document_links(1, 16, || false).unwrap();
        assert_eq!(reader.document_link(1, 1, 2, || false).err(),
            Some(ReaderSessionError::Document(DocumentReadError::InvalidOutput)));
        assert!(reader.document_links(1, 0, 16, || false).is_ok());
        assert!(reader.document_window(1, 0, 16, || false).is_ok());
    }
    let mut reader = reader(b"# Present\n\n[go](#absent)\n");
    reader.prepare_document_links(1, 16, || false).unwrap();
    assert_eq!(reader.document_link(1, 1, 2, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::HeadingNotFound)));
}

#[test]
fn fragments_decode_once_and_forks_require_their_own_document_inventory() {
    let mut reader = reader(b"# Target\n\n[once](#%2574arget) [literal](#target)\n");
    let prepared = json(&reader.prepare_document_links(1, 16, || false).unwrap());
    assert_eq!(prepared.get("links").array()[0].get("fragment_slug").text(), "%74arget");
    assert_eq!(reader.document_link(1, 1, 2, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::HeadingNotFound)));
    assert!(json(&reader.document_link(1, 2, 2, || false).unwrap()).get("activated").flag());
    let mut fork = reader.fork(owner(31902), || false).unwrap();
    assert_eq!(fork.capture_sha256(), reader.capture_sha256());
    assert_ne!(fork.capture().request(), reader.capture().request());
    assert_eq!(fork.document_links_generation(), None);
    assert_eq!(fork.document_link_destination(1, 2).err(), Some(ReaderSessionError::MissingDocument));
    fork.prepare_document(1, Default::default(), || false).unwrap();
    assert_eq!(fork.document_link_destination(1, 2).err(), Some(ReaderSessionError::MissingDocumentLinks));
    fork.prepare_document_links(1, 16, || false).unwrap();
    assert_eq!(fork.document_link_destination(1, 2).unwrap(), "#target");
}
