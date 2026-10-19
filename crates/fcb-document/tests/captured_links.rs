#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, QueryGeneration,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::CaptureRequest;
use fcb_document::links::{CapturedDocumentLinks, DocumentLinkError,
    MAX_DOCUMENT_LINK_DESTINATION_BYTES, MAX_DOCUMENT_LINK_SOURCE_BYTES, resolve_document_destination};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(8041).unwrap() }
fn id(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn request() -> CaptureRequest {
    CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), 7).unwrap()).unwrap()
}
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn whole(bytes: &[u8]) -> ByteRange {
    ByteRange::new(ByteOffset::new(0), ByteOffset::new(bytes.len() as u64)).unwrap()
}

#[test]
fn duplicate_links_keep_distinct_occurrences_and_complete_original_document_evidence() {
    let source = "\u{feff}# Links\n\nRead [first](../src/a%20b.rs#L3) and [second](../src/a%20b.rs#L3).\n\n\x60[example](fake.rs)\x60\n\n\x60\x60\x60md\n[code](fake.rs)\n\x60\x60\x60\n\n![image](fake.png)\n";
    let budget = budget();
    let links = CapturedDocumentLinks::build(source.as_bytes(), request(), generation(1), 20,
        &budget, id(1), || false).unwrap();
    assert_eq!(links.links_seen(), 2); assert_eq!(links.links().len(), 2);
    assert!(!links.truncated());
    assert_eq!(links.links()[0].id(), 1); assert_eq!(links.links()[1].id(), 2);
    for link in links.links() {
        assert_eq!(link.destination(), "../src/a%20b.rs#L3");
        assert_eq!(link.original_document_range(), whole(source.as_bytes()));
        let (a, b) = link.original_document_range().as_usize_bounds().unwrap();
        assert_eq!(&source.as_bytes()[a..b], source.as_bytes());
    }
    assert_eq!(links.file(), request().file()); assert_eq!(links.revision(), request().revision());
}

#[test]
fn reference_links_are_parser_resolved_without_claiming_destination_token_bytes() {
    let source = b"[visible][target]\n\n[target]: ../src/lib.rs \"Title\"\n";
    let budget = budget();
    let links = CapturedDocumentLinks::build(source, request(), generation(1), 20,
        &budget, id(1), || false).unwrap();
    assert_eq!(links.links().len(), 1);
    let link = &links.links()[0];
    assert_eq!(link.destination(), "../src/lib.rs");
    assert_eq!(link.original_document_range(), whole(source));
    assert_ne!(source.as_slice(), link.destination().as_bytes());
}

#[test]
fn nested_quote_list_table_and_emphasis_links_use_upstream_structure() {
    let source = b"> [quote](q.rs)\n\n- **[list](l.rs)**\n\n| Link |\n| --- |\n| [cell](t.rs) |\n";
    let budget = budget();
    let links = CapturedDocumentLinks::build(source, request(), generation(1), 20,
        &budget, id(1), || false).unwrap();
    assert_eq!(links.links().iter().map(|link| link.destination()).collect::<Vec<_>>(),
        vec!["q.rs", "l.rs", "t.rs"]);
    for link in links.links() { assert_eq!(link.original_document_range(), whole(source)); }
}

#[test]
fn upstream_span_collector_defects_cannot_move_link_evidence_to_another_source_region() {
    // These valid AST shapes have historically disagreed with the pinned
    // parser's separate block-span collector. The adapter must never trust
    // those spans, even for a later ordinary paragraph with an equal AST kind.
    let cases = [
        ("First\n: [a](a.rs)\n\nSecond\n: [b](b.rs)\n\n[tail](tail.rs)\n", 3),
        ("> [a](a.rs)\ncontinued [b](b.rs)\n\n[tail](tail.rs)\n", 3),
        ("[a](a.rs)\n    continued [b](b.rs)\n\n[tail](tail.rs)\n", 3),
        ("---\ntitle: Original\n---\n\n[tail](tail.rs)\n", 1),
        ("$$\nx+y\n$$\n\n[tail](tail.rs)\n", 1),
    ];
    let budget = budget();
    for (index, (source, count)) in cases.iter().enumerate() {
        let links = CapturedDocumentLinks::build(source.as_bytes(), request(), generation(1), 20,
            &budget, id(index as u64 + 1), || false).unwrap();
        assert_eq!(links.links().len(), *count);
        assert_eq!(links.links().last().unwrap().destination(), "tail.rs");
        for link in links.links() {
            assert_eq!(link.original_document_range(), whole(source.as_bytes()));
        }
    }
}

#[test]
fn upstream_url_policy_decodes_paths_once_and_never_opens_a_root_or_url() {
    assert_eq!(resolve_document_destination("docs/guide.md", "../src/a%20b.rs?plain=1#L3"),
        Some(("src/a b.rs".into(), "?plain=1#L3".into())));
    assert_eq!(resolve_document_destination("docs/guide.md", "../src/a%2520b.rs"),
        Some(("src/a%20b.rs".into(), "".into())));
    for destination in ["../../outside.rs", "https://example.invalid/x", "//example.invalid/x",
        "file:///tmp/source.rs", "../broken%zz.rs", "../bad%FF.rs", "../bad%5Cpath.rs"] {
        assert_eq!(resolve_document_destination("docs/guide.md", destination), None);
    }
}

#[test]
fn bounded_outputs_count_omissions_without_renumbering_source_occurrences() {
    let source = b"[one](a) [two](b) [three](c)";
    let budget = budget();
    let limited = CapturedDocumentLinks::build(source, request(), generation(1), 1, &budget, id(1), || false).unwrap();
    assert_eq!(limited.links_seen(), 3); assert_eq!(limited.links().len(), 1); assert!(limited.truncated());
    let empty = CapturedDocumentLinks::build(source, request(), generation(2), 0, &budget, id(2), || false).unwrap();
    assert_eq!(empty.links_seen(), 3); assert!(empty.links().is_empty()); assert!(empty.truncated());
    let long = format!("[omitted]({}) [kept](ok.rs)", "x".repeat(MAX_DOCUMENT_LINK_DESTINATION_BYTES + 1));
    let links = CapturedDocumentLinks::build(long.as_bytes(), request(), generation(3), 4,
        &budget, id(3), || false).unwrap();
    assert_eq!(links.links_seen(), 2); assert!(links.truncated());
    assert_eq!(links.links()[0].id(), 2); assert_eq!(links.links()[0].destination(), "ok.rs");
}

#[test]
fn source_owner_generation_encoding_and_extent_guards_precede_publication() {
    let source = b"[link](a.rs)"; let budget = budget();
    let links = CapturedDocumentLinks::build(source, request(), generation(1), 10, &budget, id(1), || false).unwrap();
    links.validate_delivery(source, request(), generation(1)).unwrap();
    assert_eq!(links.validate_delivery(b"[link](b.rs)", request(), generation(1)), Err(DocumentLinkError::StaleSource));
    assert_eq!(links.validate_delivery(source, request(), generation(2)), Err(DocumentLinkError::StaleGeneration));
    let foreign = QueryGeneration::new(ArenaOwnerId::new(8042).unwrap(), 1).unwrap();
    assert_eq!(CapturedDocumentLinks::build(source, request(), foreign, 10, &budget, id(2), || false).err(),
        Some(DocumentLinkError::OwnerMismatch));
    let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(1)).unwrap();
    assert_eq!(CapturedDocumentLinks::build(source, request().with_range(range).unwrap(), generation(2), 10,
        &budget, id(3), || false).err(), Some(DocumentLinkError::InvalidEvidence));
    assert_eq!(CapturedDocumentLinks::build(&[0xff], request(), generation(2), 10,
        &budget, id(4), || false).err(), Some(DocumentLinkError::InvalidUtf8));
    let oversized = vec![b' '; MAX_DOCUMENT_LINK_SOURCE_BYTES + 1];
    assert_eq!(CapturedDocumentLinks::build(&oversized, request(), generation(2), 10,
        &budget, id(5), || false).err(), Some(DocumentLinkError::SourceLimit));
}

#[test]
fn canceled_or_denied_replacement_leaves_an_independent_old_result_usable() {
    let source = b"[old](a.rs)"; let budget = budget();
    let old = CapturedDocumentLinks::build(source, request(), generation(1), 10, &budget, id(1), || false).unwrap();
    let mut polls = 0;
    assert_eq!(CapturedDocumentLinks::build(b"[new](b.rs)", request(), generation(2), 10,
        &budget, id(2), || { polls += 1; polls >= 3 }).err(), Some(DocumentLinkError::Canceled));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(CapturedDocumentLinks::build(source, request(), generation(2), 10,
        &tiny, id(3), || false).err(), Some(DocumentLinkError::ResourceDenied));
    old.validate_delivery(source, request(), generation(1)).unwrap();
    assert_eq!(old.links()[0].destination(), "a.rs");
}

#[test]
fn empty_and_bom_only_captures_have_complete_empty_link_sets() {
    let budget = budget();
    for (index, source) in [b"".as_slice(), &[0xef, 0xbb, 0xbf]].into_iter().enumerate() {
        let links = CapturedDocumentLinks::build(source, request(), generation(1), 10,
            &budget, id(index as u64 + 1), || false).unwrap();
        assert!(links.links().is_empty()); assert_eq!(links.links_seen(), 0); assert!(!links.truncated());
    }
}

#[test]
fn repeated_long_reference_destinations_and_titles_are_refused_before_parser_allocation() {
    // Both inputs remain below the ordinary source limit but would make the
    // upstream parser clone gigabytes before post-parse output caps take effect.
    let uses = "[r]\n".repeat(30_000);
    let destination = format!("[r]: {}\n\n{uses}", "x".repeat(128 * 1024));
    let title = format!("[r]: a.rs \"{}\"\n\n{uses}", "x".repeat(128 * 1024));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    for (index, source) in [destination, title].iter().enumerate() {
        assert!(source.len() < MAX_DOCUMENT_LINK_SOURCE_BYTES);
        // A one-byte budget would produce ResourceDenied at reservation; this
        // exact error proves that pre-parser complexity admission rejected first.
        assert_eq!(CapturedDocumentLinks::build(source.as_bytes(), request(), generation(1), 1,
            &tiny, id(index as u64 + 1), || false).err(), Some(DocumentLinkError::ComplexityLimit));
    }
}
