#![forbid(unsafe_code)]
#![cfg(unix)]

mod support;
use support::{Json, parse};
use std::{fs, os::unix::fs::symlink, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, FileId};
use fcb::source::SourceError;
use fcb_app::host::{HostResponse, catalog_session::{CatalogDocumentLinkSelection,
    CatalogSession, CatalogSessionError, FIRST_CATALOG_LINK_FILE_ID, MAX_CATALOG_PAGE_ENTRIES, MAX_CATALOG_READER_RECEIPTS,
    MAX_CATALOG_RETAINED_PAGES, MAX_CATALOG_RESPONSE_BYTES},
    reader::{ReaderDocumentOptions, ReaderSession}};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn json(response: &HostResponse) -> Json {
    assert!(response.as_str().len() <= MAX_CATALOG_RESPONSE_BYTES);
    parse(response.as_str().as_bytes()).unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn path(row: &Json) -> String {
    let text = row.get("path").get("hex").text();
    String::from_utf8((0..text.len()).step_by(2)
        .map(|start| u8::from_str_radix(&text[start..start + 2], 16).unwrap()).collect()).unwrap()
}
fn new_root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let root = std::env::temp_dir().join(format!("fcb-catalog-links-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    root // Retain private fixtures for diagnosis, as in progressive_catalog.rs.
}

#[derive(Clone)]
struct Row { page: u64, file: FileId, path: String, observed_bytes: u64 }
fn rows(page: &Json, catalog_owner: ArenaOwnerId) -> Vec<Row> {
    page.get("files").array().iter().map(|row| Row {
        page: page.get("page_id").number(),
        file: FileId::new(catalog_owner, row.get("file_id").number()).unwrap(),
        path: path(row), observed_bytes: row.get("observed_bytes").number(),
    }).collect()
}
fn catalog(root: &Path, catalog_owner: u64) -> (CatalogSession, Vec<Row>) {
    let mut catalog = CatalogSession::open(owner(catalog_owner), root, || false).unwrap();
    let mut page = json(&catalog.page(1, || false).unwrap());
    let mut found = rows(&page, catalog.owner());
    while page.get("more").flag() {
        page = json(&catalog.advance(page.get("page_id").number(), || false).unwrap());
        found.extend(rows(&page, catalog.owner()));
    }
    // These small fixtures stay inside the production retention bound. The
    // separate eviction regression deliberately advances beyond it.
    assert_eq!(page.get("first_retained_page").number(), 1);
    (catalog, found)
}
fn row(rows: &[Row], name: &str) -> Row { rows.iter().find(|row| row.path == name).unwrap().clone() }
fn selection(row: &Row, generation: u64, id: u64) -> CatalogDocumentLinkSelection {
    CatalogDocumentLinkSelection { source_page: row.page, source_file: row.file,
        document_generation: generation, link_id: id }
}
fn prepare(reader: &mut ReaderSession) {
    reader.prepare_document(1, ReaderDocumentOptions::default(), || false).unwrap();
    reader.prepare_document_links(1, 128, || false).unwrap();
}
fn open(catalog: &mut CatalogSession, row: &Row, reader_owner: u64) -> ReaderSession {
    let (mut reader, _) = catalog.open_reader(owner(reader_owner), row.page, row.file, 4096, || false).unwrap();
    prepare(&mut reader);
    reader
}

#[test]
fn relative_links_keep_original_document_and_open_independent_current_targets() {
    let root = new_root();
    fs::create_dir(root.join("docs")).unwrap(); fs::create_dir(root.join("src")).unwrap();
    let original = b"# Original\n\n[target](../src/a%20b.md?plain=1#details)\n";
    fs::write(root.join("docs/README.md"), original).unwrap();
    fs::write(root.join("src/a b.md"), b"old").unwrap();
    let (mut catalog, found) = catalog(&root, 73101);
    let source = row(&found, "docs/README.md"); let target_row = row(&found, "src/a b.md");
    let mut origin = open(&mut catalog, &source, 73102);
    let changed = b"# Changed source after capture\n";
    let current_target = b"# Current target\n\n[back](../docs/README.md)\n";
    fs::write(root.join("docs/README.md"), changed).unwrap();
    fs::write(root.join("src/a b.md"), current_target).unwrap();
    let (mut target, receipt) = catalog.open_document_link(&origin, owner(73103), selection(&source, 1, 1), 4096, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("command").text(), "open-document-link");
    assert_eq!(receipt.get("source_authority").text(), "retained-catalog-capture");
    assert_eq!(receipt.get("target_resolution").text(), "retained-catalog-entry");
    assert_eq!(receipt.get("owner").number(), 73101);
    assert_eq!(receipt.get("source_reader_owner").number(), 73102);
    assert_eq!(receipt.get("source_page_id").number(), source.page);
    assert_eq!(receipt.get("source_file_id").number(), source.file.get());
    assert_eq!(receipt.get("source_capture_sha256").text(), origin.capture_sha256().to_hex());
    assert_eq!(receipt.get("target_page_id").number(), target_row.page);
    assert_eq!(receipt.get("target_file").get("file_id").number(), target_row.file.get());
    assert_eq!(path(receipt.get("target_file")), "src/a b.md");
    assert_eq!(receipt.get("target_file").get("observed_bytes").number(), 3);
    assert_eq!(target_row.observed_bytes, 3);
    assert_eq!(receipt.get("source_observation").text(), "new-capture-after-document-link");
    assert_eq!(receipt.get("source_bytes_read").number(), current_target.len() as u64);
    assert!(receipt.get("source_read_calls").number() > 0);
    assert_eq!(receipt.get("destination").text(), "../src/a%20b.md?plain=1#details");
    assert_eq!(receipt.get("destination_suffix").text(), "?plain=1#details");
    assert_eq!(receipt.get("fragment_status").text(), "missing-heading");
    assert_eq!(receipt.get("fragment_slug").text(), "details");
    assert_eq!(receipt.get("target_range"), &Json::Null);
    assert_eq!(receipt.get("fragment_suffix").text(), "#details");
    assert_eq!(receipt.get("initial_original_offset").number(), 0);
    assert_eq!(target.capture().bytes(), current_target);
    assert_eq!(origin.capture().bytes(), original);
    assert_eq!(json(&origin.copy_range(0, original.len() as u64, || false).unwrap())
        .get("original_hex").text(), hex(original));
    // The target's successful catalog capture has its own receipt, permitting
    // a second explicit activation without reassigning the origin's identity.
    prepare(&mut target);
    let (back, _) = catalog.open_document_link(&target, owner(73104), selection(&target_row, 1, 1), 4096, || false).unwrap();
    assert_eq!(back.capture().bytes(), changed);
    assert_eq!(origin.capture().bytes(), original);
    assert_ne!(back.capture().request().file().owner(), origin.capture().request().file().owner());
}

#[test]
fn supplied_labels_foreign_owners_and_changed_digests_cannot_forge_catalog_receipts() {
    let root = new_root();
    let bytes = b"# Guide\n\n[target](target.md)\n";
    fs::write(root.join("README.md"), bytes).unwrap();
    fs::write(root.join("target.md"), b"# Target\n").unwrap();
    let (mut catalog, found) = catalog(&root, 73201);
    let source = row(&found, "README.md"); let target = row(&found, "target.md");
    let origin = open(&mut catalog, &source, 73202);
    let mut supplied = ReaderSession::from_bytes(owner(73203), Path::new("README.md"), bytes, || false).unwrap();
    prepare(&mut supplied);
    assert!(matches!(catalog.open_document_link(&supplied, owner(73204), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::StaleReaderReceipt)));
    // The safe library requires fresh owner domains; these deliberate caller
    // violations additionally demonstrate that request/label equality cannot
    // substitute different bytes under the registered source receipt.
    let mut changed = ReaderSession::from_bytes(owner(73202), Path::new("README.md"),
        b"# Guide\n\n[target](elsewhere.md)\n", || false).unwrap();
    prepare(&mut changed);
    assert!(matches!(catalog.open_document_link(&changed, owner(73204), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::ReaderReceiptMismatch)));
    let mut relabeled = ReaderSession::from_bytes(owner(73202), Path::new("elsewhere.md"), bytes, || false).unwrap();
    prepare(&mut relabeled);
    assert!(matches!(catalog.open_document_link(&relabeled, owner(73204), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::ReaderReceiptMismatch)));
    let mut normalized_label = ReaderSession::from_bytes(owner(73202), Path::new("./README.md"), bytes, || false).unwrap();
    prepare(&mut normalized_label);
    assert!(matches!(catalog.open_document_link(&normalized_label, owner(73204), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::ReaderReceiptMismatch)));
    assert!(matches!(catalog.open_document_link(&origin, owner(73204), selection(&target, 1, 1), 4096, || false),
        Err(CatalogSessionError::ReaderReceiptMismatch)));
    assert!(matches!(catalog.open_document_link(&origin, owner(73202), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::ForeignOwner)));
    assert!(catalog.open_reader(owner(73205), source.page, source.file, 1, || false).is_err());
    let mut denied = ReaderSession::from_bytes(owner(73205), Path::new("README.md"), bytes, || false).unwrap();
    prepare(&mut denied);
    assert!(matches!(catalog.open_document_link(&denied, owner(73204), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::StaleReaderReceipt)));
}

#[test]
fn receipt_labels_compare_raw_bytes_without_path_component_normalization() {
    let root = new_root(); fs::create_dir(root.join("docs")).unwrap();
    let bytes = b"# Guide\n\n[target](target.md)\n";
    fs::write(root.join("docs/README.md"), bytes).unwrap();
    fs::write(root.join("docs/target.md"), b"# Target\n").unwrap();
    let (mut catalog, found) = catalog(&root, 74201);
    let source = row(&found, "docs/README.md");
    let origin = open(&mut catalog, &source, 74202);
    for label in ["docs//README.md", "docs/./README.md"] {
        // Path equality can normalize these labels; receipt identity must not.
        assert_eq!(Path::new(label), Path::new("docs/README.md"));
        let mut relabeled = ReaderSession::from_bytes(owner(74202), Path::new(label), bytes, || false).unwrap();
        prepare(&mut relabeled);
        assert!(matches!(catalog.open_document_link(&relabeled, owner(74203),
            selection(&source, 1, 1), 4096, || false),
            Err(CatalogSessionError::ReaderReceiptMismatch)));
    }
    catalog.open_document_link(&origin, owner(74203), selection(&source, 1, 1), 4096, || false).unwrap();
}

#[test]
fn explicit_unobserved_targets_use_the_existing_grant_and_keep_uri_policy() {
    let root = new_root();
    let bytes = b"# Guide\n\n[late](late.rs)\n\n[escape](../outside.rs)\n\n[encoded](%2e%2e/outside.rs)\n\n[web](https://example.invalid/x)\n\n[file](file:///tmp/outside.rs)\n\n[authority](//example.invalid/x)\n\n[bad](bad%zz.rs)\n\n[backslash](bad%5Cname.rs)\n";
    fs::write(root.join("README.md"), bytes).unwrap();
    let (mut catalog, found) = catalog(&root, 73301);
    let source = row(&found, "README.md"); let origin = open(&mut catalog, &source, 73302);
    let late_bytes = b"fn not_in_the_observed_catalog() {}\n";
    fs::write(root.join("late.rs"), late_bytes).unwrap();
    let before = json(&catalog.page(source.page, || false).unwrap());
    let (target, response) = catalog.open_document_link(&origin, owner(73303), selection(&source, 1, 1), 4096, || false).unwrap();
    let response = json(&response);
    assert_eq!(target.capture().bytes(), late_bytes);
    assert_eq!(response.get("target_resolution").text(), "root-grant-path");
    assert_eq!(response.get("target_page_id").number(), 0);
    assert_eq!(response.get("target_file").get("file_id").number(), FIRST_CATALOG_LINK_FILE_ID);
    assert_eq!(response.get("target_file").get("observed_bytes").number(), late_bytes.len() as u64);
    assert_eq!(response.get("source_bytes_read").number(), late_bytes.len() as u64);
    for field in ["catalogued_files", "latest_page", "retained_files", "retained_path_bytes", "discovery"] {
        assert_eq!(response.get(field), before.get(field), "link activation advanced discovery: {field}");
    }
    for id in 2..=8 {
        assert!(matches!(catalog.open_document_link(&origin, owner(73303), selection(&source, 1, id), 4096, || false),
            Err(CatalogSessionError::UnsupportedDocumentLink)), "destination occurrence {id}");
    }
    assert_eq!(origin.capture().bytes(), bytes);
}

#[test]
fn uri_path_components_are_decoded_once_and_unfragmented_targets_start_at_zero() {
    let root = new_root();
    fs::write(root.join("README.md"), b"# Guide\n\n[space](a%20b.rs)\n\n[percent](a%2520b.rs)\n").unwrap();
    fs::write(root.join("a b.rs"), b"space destination").unwrap();
    fs::write(root.join("a%20b.rs"), b"literal percent destination").unwrap();
    let (mut catalog, found) = catalog(&root, 73401);
    let source = row(&found, "README.md"); let origin = open(&mut catalog, &source, 73402);
    for (id, expected, bytes) in [(1, "a b.rs", b"space destination".as_slice()),
        (2, "a%20b.rs", b"literal percent destination".as_slice())] {
        let (reader, receipt) = catalog.open_document_link(&origin, owner(73402 + id), selection(&source, 1, id), 4096, || false).unwrap();
        let receipt = json(&receipt);
        assert_eq!(path(receipt.get("target_file")), expected);
        assert_eq!(receipt.get("fragment_status").text(), "none");
        assert_eq!(receipt.get("fragment_slug"), &Json::Null);
        assert_eq!(receipt.get("target_range"), &Json::Null);
        assert_eq!(receipt.get("fragment_suffix").text(), "");
        assert_eq!(receipt.get("initial_original_offset").number(), 0);
        assert_eq!(reader.capture().bytes(), bytes);
    }
}

#[test]
fn canonical_heading_links_select_original_bom_crlf_offsets_without_consuming_preview_generation() {
    let root = new_root(); fs::create_dir(root.join("docs")).unwrap();
    let source = b"# Local\n\n[guide](docs/guide.MARKDOWN#caf-%64etails) [query](docs/guide.MARKDOWN?view=source#caf-%64etails)\n";
    let guide = format!("\u{feff}{}\r\n\r\n## Café Details\r\n\r\nBody.\r\n", "x".repeat(200));
    fs::write(root.join("README.md"), source).unwrap(); fs::write(root.join("docs/guide.MARKDOWN"), &guide).unwrap();
    let (mut catalog, found) = catalog(&root, 75101);
    let source_row = row(&found, "README.md"); let origin = open(&mut catalog, &source_row, 75102);
    let mut first_range: Option<Json> = None;
    for id in 1..=2 {
        let (mut reader, response) = catalog.open_document_link(&origin, owner(75102 + id),
            selection(&source_row, 1, id), 4096, || false).unwrap();
        let receipt = json(&response);
        let range = receipt.get("target_range");
        let start = range.get("start").number(); let end = range.get("end").number();
        assert_eq!(receipt.get("fragment_status").text(), "resolved");
        assert_eq!(receipt.get("fragment_slug").text(), "caf-details");
        assert_eq!(start, 207); // UTF-8 BOM, 200 ASCII bytes, then two CRLFs.
        assert_eq!(guide[start as usize..end as usize].trim_end(), "## Café Details");
        assert_eq!(receipt.get("initial_original_offset").number(), start);
        assert_eq!(receipt.get("window").get("requested_original_range").get("start").number(), start);
        assert_eq!(receipt.get("window").get("visible_range").get("start").number(), start);
        assert_eq!(receipt.get("source_bytes_read").number(), guide.len() as u64);
        assert_eq!(receipt.get("reader").get("accepted_document_generation"), &Json::Null);
        assert_eq!(reader.document_generation(), None);
        assert_eq!(reader.document_links_generation(), None);
        if let Some(first) = &first_range { assert_eq!(range, first); }
        else { first_range = Some(range.clone()); }
        if id == 2 {
            assert_eq!(receipt.get("destination_suffix").text(), "?view=source#caf-%64etails");
            fs::write(root.join("docs/guide.MARKDOWN"), b"# Replacement\n").unwrap();
        }
        // Qualification is temporary derived work over this new capture. It
        // consumes neither the reader's first preview attempt nor live bytes.
        let prepared = reader.prepare_document(1, ReaderDocumentOptions::default(), || false).unwrap();
        assert_eq!(json(&prepared).get("document_generation").number(), 1);
        assert_eq!(json(&reader.copy_range(start, end, || false).unwrap()).get("original_hex").text(),
            hex(&guide.as_bytes()[start as usize..end as usize]));
        assert_eq!(reader.capture().bytes(), guide.as_bytes());
        assert_eq!(origin.capture().bytes(), source);
    }
}

#[test]
fn canonical_collisions_missing_and_invalid_fragments_remain_distinct_from_file_capture() {
    let root = new_root();
    let source = b"[ok](guide.md#repeat) [collision](guide.md#repeat-2) [missing](guide.md#absent) \
        [once](guide.md#%2572epeat) [case](guide.md#Repeat) [plus](guide.md#repeat+two) \
        [invalid](guide.md#%ff) [zero](guide.md#%00) [top](guide.md#) \
        [code](code.rs#L2) [code-top](code.rs#) [query](guide.md?plain=1) [plain](guide.md)\n";
    let guide = b"# Repeat\n\n# Repeat\n\n# Repeat-2\n\n";
    fs::write(root.join("README.md"), source).unwrap(); fs::write(root.join("guide.md"), guide).unwrap();
    fs::write(root.join("code.rs"), b"first\nsecond\n").unwrap();
    let (mut catalog, found) = catalog(&root, 75201);
    let source_row = row(&found, "README.md"); let origin = open(&mut catalog, &source_row, 75202);
    let expected = [
        ("resolved", Some("repeat")), ("ambiguous", Some("repeat-2")),
        ("missing-heading", Some("absent")), ("missing-heading", Some("%72epeat")),
        ("missing-heading", Some("Repeat")), ("missing-heading", Some("repeat+two")),
        ("invalid", None), ("invalid", None), ("document-start", None),
        ("resolved-lines", Some("L2")), ("document-start", None), ("none", None), ("none", None),
    ];
    for (index, (status, slug)) in expected.iter().enumerate() {
        let id = index as u64 + 1;
        let (reader, response) = catalog.open_document_link(&origin, owner(75300 + id),
            selection(&source_row, 1, id), 4096, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("fragment_status").text(), *status, "occurrence {id}");
        match slug {
            Some(slug) => assert_eq!(receipt.get("fragment_slug").text(), *slug),
            None => assert_eq!(receipt.get("fragment_slug"), &Json::Null),
        }
        let offset = if *status == "resolved-lines" { 6 } else { 0 };
        assert_eq!(receipt.get("initial_original_offset").number(), offset);
        assert_eq!(receipt.get("window").get("visible_range").get("start").number(), offset);
        if *status == "resolved" {
            let range = receipt.get("target_range");
            assert_eq!(range.get("start").number(), 0);
            assert_eq!(std::str::from_utf8(&guide[..range.get("end").number() as usize]).unwrap().trim_end(), "# Repeat");
        } else if *status == "resolved-lines" {
            assert_eq!(receipt.get("target_range").get("start").number(), 6);
            assert_eq!(receipt.get("target_range").get("end").number(), 13);
        } else { assert_eq!(receipt.get("target_range"), &Json::Null); }
        assert_eq!(reader.document_generation(), None);
        assert_eq!(receipt.get("reader").get("accepted_document_generation"), &Json::Null);
        if id == 10 || id == 11 { assert_eq!(reader.capture().bytes(), b"first\nsecond\n"); }
        else { assert_eq!(reader.capture().bytes(), guide); }
    }
    assert_eq!(origin.capture().bytes(), source);
}

#[test]
fn source_line_links_select_original_utf8_utf16_and_large_capture_ranges() {
    for encoding in 0..3 {
        let root = new_root();
        let encode = |text: &str| -> Vec<u8> { match encoding {
            0 => text.as_bytes().to_vec(),
            1 => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
            _ => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        } };
        let prefix = "\u{feff}preamble\r\n";
        let selected = "\u{010a}🦀\rthird\n";
        let bytes = encode(&format!("{prefix}{selected}last"));
        let start = encode(prefix).len() as u64;
        let end = start + encode(selected).len() as u64;
        fs::write(root.join("README.md"), b"[lines](code.rs?source=1#%4C2-L3) [same](code.rs#L2-L3)").unwrap();
        fs::write(root.join("code.rs"), &bytes).unwrap();
        let (mut catalog, found) = catalog(&root, 78100 + encoding * 10);
        let source = row(&found, "README.md");
        let origin = open(&mut catalog, &source, 78101 + encoding * 10);
        for id in 1..=2 {
            let (mut reader, response) = catalog.open_document_link(&origin, owner(78101 + encoding * 10 + id),
                selection(&source, 1, id), 4096, || false).unwrap();
            let receipt = json(&response);
            assert_eq!(receipt.get("fragment_status").text(), "resolved-lines");
            assert_eq!(receipt.get("fragment_slug").text(), "L2-L3");
            assert_eq!(receipt.get("target_range").get("start").number(), start);
            assert_eq!(receipt.get("target_range").get("end").number(), end);
            assert_eq!(receipt.get("initial_original_offset").number(), start);
            assert_eq!(receipt.get("window").get("requested_original_range").get("start").number(), start);
            assert_eq!(receipt.get("source_bytes_read").number(), bytes.len() as u64);
            assert_eq!(reader.document_generation(), None);
            assert_eq!(reader.document_links_generation(), None);
            assert_eq!(json(&reader.copy_range(start, end, || false).unwrap()).get("original_hex").text(), hex(&encode(selected)));
        }
    }

    let root = new_root();
    let mut bytes = vec![b'x'; fcb::document::links::MAX_DOCUMENT_LINK_SOURCE_BYTES + 31];
    bytes.extend_from_slice(b"\r\nexact final line");
    let start = bytes.len() - b"exact final line".len();
    fs::write(root.join("README.md"), b"[deep](large.rs#L2)").unwrap();
    fs::write(root.join("large.rs"), &bytes).unwrap();
    let (mut catalog, found) = catalog(&root, 78140);
    let source = row(&found, "README.md"); let origin = open(&mut catalog, &source, 78141);
    let (reader, response) = catalog.open_document_link(&origin, owner(78142), selection(&source, 1, 1),
        1024 * 1024, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("fragment_status").text(), "resolved-lines");
    assert_eq!(receipt.get("target_range").get("start").number(), start as u64);
    assert_eq!(receipt.get("target_range").get("end").number(), bytes.len() as u64);
    assert_eq!(reader.capture().bytes(), bytes);
}

#[test]
fn source_line_syntax_missing_endpoints_and_markdown_heading_identity_stay_distinct() {
    let root = new_root();
    let source = b"[missing](code.rs#L3) [end](code.rs#L1-L3) [zero](code.rs#L0) \
        [leading](code.rs#L01) [reverse](code.rs#L2-L1) [overflow](code.rs#L18446744073709551616) \
        [named](code.rs#License) [twice](code.rs#%254C2) [heading](guide.md#L2) [empty](empty.rs#L1)";
    fs::write(root.join("README.md"), source).unwrap();
    fs::write(root.join("code.rs"), b"one\r\ntwo\n").unwrap();
    fs::write(root.join("guide.md"), b"# L2\n").unwrap();
    fs::write(root.join("empty.rs"), b"").unwrap();
    let (mut catalog, found) = catalog(&root, 78150);
    let source_row = row(&found, "README.md"); let origin = open(&mut catalog, &source_row, 78151);
    for (index, (status, slug)) in [
        ("missing-line", Some("L3")), ("missing-line", Some("L1-L3")), ("invalid", None),
        ("invalid", None), ("invalid", None), ("invalid", None), ("unsupported", Some("License")),
        ("unsupported", Some("%4C2")), ("missing-heading", Some("L2")), ("missing-line", Some("L1")),
    ].iter().enumerate() {
        let (_, response) = catalog.open_document_link(&origin, owner(78160 + index as u64),
            selection(&source_row, 1, index as u64 + 1), 4096, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("fragment_status").text(), *status, "link {}", index + 1);
        match slug {
            Some(slug) => assert_eq!(receipt.get("fragment_slug").text(), *slug),
            None => assert_eq!(receipt.get("fragment_slug"), &Json::Null),
        }
        assert_eq!(receipt.get("target_range"), &Json::Null);
        assert_eq!(receipt.get("initial_original_offset").number(), 0);
    }
}

#[test]
fn complete_heading_provenance_is_required_before_claiming_a_missing_heading() {
    let cases = [
        "\u{feff}---\ntitle: Guide\n---\n\n# Target\n",
        "> target\ntarget\n\n# target\n",
        "p\n    target\n\n# target\n",
        "$$\n# target\n$$\n\n# target\n",
        "Term\n: first\n\nTarget\n: second\n\n# Target\n",
        "> # target\n",
    ];
    for (index, guide) in cases.iter().enumerate() {
        let root = new_root();
        fs::write(root.join("README.md"), b"[target](guide.md#absent)\n").unwrap();
        fs::write(root.join("guide.md"), guide).unwrap();
        let (mut catalog, found) = catalog(&root, 75400 + index as u64 * 3);
        let source = row(&found, "README.md");
        let origin = open(&mut catalog, &source, 75401 + index as u64 * 3);
        let (mut reader, response) = catalog.open_document_link(&origin, owner(75402 + index as u64 * 3),
            selection(&source, 1, 1), 4096, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("fragment_status").text(), "unsupported", "case {index}");
        assert_eq!(receipt.get("fragment_slug").text(), "absent");
        assert_eq!(receipt.get("target_range"), &Json::Null);
        assert_eq!(receipt.get("initial_original_offset").number(), 0);
        assert_eq!(json(&reader.copy_range(0, guide.len() as u64, || false).unwrap()).get("original_hex").text(),
            hex(guide.as_bytes()));
    }
    // A qualified fence/list document remains a positive control: source that
    // merely looks like a heading inside code must not steal the actual anchor.
    let root = new_root();
    let guide = b"```text\n# target\n```\n\n- ordinary list\n\n# Target\n";
    fs::write(root.join("README.md"), b"[target](guide.md#target)\n").unwrap();
    fs::write(root.join("guide.md"), guide).unwrap();
    let (mut catalog, found) = catalog(&root, 75430);
    let source = row(&found, "README.md"); let origin = open(&mut catalog, &source, 75431);
    let (_, response) = catalog.open_document_link(&origin, owner(75432), selection(&source, 1, 1), 4096, || false).unwrap();
    let receipt = json(&response);
    assert_eq!(receipt.get("fragment_status").text(), "resolved");
    assert_eq!(&guide[receipt.get("initial_original_offset").number() as usize..], b"# Target\n");
}

#[test]
fn target_source_flow_and_parser_limits_keep_captured_reading_available() {
    let oversized = vec![b'x'; 256 * 1024 + 1];
    let flow_limited = format!("{}# Last\n", "paragraph\n\n".repeat(4100)).into_bytes();
    let amplified = format!("[r]: x \"{}\"\n\n{}", "x".repeat(128 * 1024), "[r]\n".repeat(30_000)).into_bytes();
    for (index, guide) in [oversized, flow_limited, amplified].into_iter().enumerate() {
        let root = new_root();
        fs::write(root.join("README.md"), b"[target](guide.md#last)\n").unwrap();
        fs::write(root.join("guide.md"), &guide).unwrap();
        let (mut catalog, found) = catalog(&root, 75500 + index as u64 * 3);
        let source = row(&found, "README.md"); let origin = open(&mut catalog, &source, 75501 + index as u64 * 3);
        let (reader, response) = catalog.open_document_link(&origin, owner(75502 + index as u64 * 3),
            selection(&source, 1, 1), 1024 * 1024, || false).unwrap();
        let receipt = json(&response);
        assert_eq!(receipt.get("fragment_status").text(), "limited", "case {index}");
        assert_eq!(receipt.get("fragment_slug").text(), "last");
        assert_eq!(receipt.get("target_range"), &Json::Null);
        assert_eq!(receipt.get("initial_original_offset").number(), 0);
        assert_eq!(receipt.get("source_bytes_read").number(), guide.len() as u64);
        assert_eq!(receipt.get("window").get("visible_range").get("start").number(), 0);
        assert_eq!(reader.capture().bytes(), guide.as_slice());
        assert_eq!(reader.document_generation(), None);
    }
}

#[test]
fn cancellation_inside_heading_qualification_remains_canceled_and_does_not_accept_a_receipt() {
    let root = new_root();
    let source = b"[plain](guide.md) [heading](guide.md#target)\n";
    let mut guide = String::new();
    for index in 0..256 { guide.push_str(&format!("# Heading {index}\n\n")); }
    guide.push_str("# Target\n\n[back](README.md)\n");
    fs::write(root.join("README.md"), source).unwrap(); fs::write(root.join("guide.md"), &guide).unwrap();
    let (mut catalog, found) = catalog(&root, 75601);
    let source_row = row(&found, "README.md"); let target_row = row(&found, "guide.md");
    let origin = open(&mut catalog, &source_row, 75602);
    let mut plain_polls = 0;
    catalog.open_document_link(&origin, owner(75603), selection(&source_row, 1, 1), 64 * 1024,
        || { plain_polls += 1; false }).unwrap();
    let mut heading_polls = 0;
    let (_, response) = catalog.open_document_link(&origin, owner(75604), selection(&source_row, 1, 2), 64 * 1024,
        || { heading_polls += 1; false }).unwrap();
    assert_eq!(json(&response).get("fragment_status").text(), "resolved");
    assert!(heading_polls > plain_polls + 100);
    // Derive the cancellation point from this input's successful work. The
    // large isolated-heading qualification dominates the extra polls; no fixed
    // implementation-specific poll number or parser substitute is required.
    let cancel_at = plain_polls + (heading_polls - plain_polls) / 2;
    let mut polls = 0;
    let error = catalog.open_document_link(&origin, owner(75605), selection(&source_row, 1, 2), 64 * 1024,
        || { polls += 1; polls == cancel_at }).err().unwrap();
    assert!(error.is_canceled(), "nested document cancellation lost its classification: {error}");
    assert!(polls >= cancel_at);
    let mut unaccepted = ReaderSession::from_bytes(owner(75605), Path::new("guide.md"), guide.as_bytes(), || false).unwrap();
    prepare(&mut unaccepted);
    assert!(matches!(catalog.open_document_link(&unaccepted, owner(75606), selection(&target_row, 1, 1), 4096, || false),
        Err(CatalogSessionError::StaleReaderReceipt)));
    assert_eq!(origin.capture().bytes(), source);
}

#[test]
fn receipt_retention_refreshes_active_origin_and_refuses_evicted_sources() {
    let root = new_root();
    let bytes = b"# Guide\n\n[target](target.rs)\n";
    fs::write(root.join("README.md"), bytes).unwrap(); fs::write(root.join("target.rs"), b"target").unwrap();
    let (mut catalog, found) = catalog(&root, 73501);
    let source = row(&found, "README.md"); let target = row(&found, "target.rs");
    let origin = open(&mut catalog, &source, 73502);
    for index in 0..(2 * MAX_CATALOG_READER_RECEIPTS) {
        let (reader, _) = catalog.open_document_link(&origin, owner(73600 + index as u64),
            selection(&source, 1, 1), 4096, || false).unwrap();
        assert_eq!(reader.capture().bytes(), b"target");
    }
    // Unrelated successful captures can evict an unused receipt, while the
    // already retained source remains independently readable.
    for index in 0..MAX_CATALOG_READER_RECEIPTS {
        catalog.open_reader(owner(73700 + index as u64), target.page, target.file, 4096, || false).unwrap();
    }
    assert!(matches!(catalog.open_document_link(&origin, owner(73800), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::StaleReaderReceipt)));
    assert_eq!(origin.capture().bytes(), bytes);
}

#[test]
fn metadata_eviction_keeps_exact_origin_receipts_and_chained_direct_targets() {
    let root = new_root(); let bytes = b"# Guide\n\n[target](target.md)\n";
    let seed = root.join("file-00000.md"); fs::write(&seed, bytes).unwrap();
    let count = MAX_CATALOG_PAGE_ENTRIES * (MAX_CATALOG_RETAINED_PAGES + 1);
    for index in 1..count { fs::hard_link(&seed, root.join(format!("file-{index:05}.md"))).unwrap(); }
    let mut catalog = CatalogSession::open(owner(73901), &root, || false).unwrap();
    let first = json(&catalog.page(1, || false).unwrap());
    let source = rows(&first, catalog.owner())[0].clone();
    let origin = open(&mut catalog, &source, 73902);
    let mut page = first;
    while page.get("more").flag() {
        page = json(&catalog.advance(page.get("page_id").number(), || false).unwrap());
    }
    assert!(page.get("first_retained_page").number() > source.page);
    let target_bytes = format!("# Target\n\n[back]({})\n", source.path);
    fs::write(root.join("target.md"), &target_bytes).unwrap();
    let (mut target, response) = catalog.open_document_link(&origin, owner(73903), selection(&source, 1, 1), 4096, || false).unwrap();
    let response = json(&response);
    assert_eq!(response.get("source_page_id").number(), source.page);
    assert_eq!(response.get("target_resolution").text(), "root-grant-path");
    assert_eq!(response.get("target_page_id").number(), 0);
    assert_eq!(response.get("catalogued_files"), page.get("catalogued_files"));
    assert_eq!(response.get("retained_pages"), page.get("retained_pages"));
    assert_eq!(response.get("retained_files"), page.get("retained_files"));
    let target_row = Row { page: 0,
        file: FileId::new(catalog.owner(), response.get("target_file").get("file_id").number()).unwrap(),
        path: "target.md".into(), observed_bytes: target_bytes.len() as u64 };
    assert_eq!(target_row.file.get(), FIRST_CATALOG_LINK_FILE_ID);
    assert_eq!(target.capture().bytes(), target_bytes.as_bytes());
    prepare(&mut target);
    let (back, back_response) = catalog.open_document_link(&target, owner(73904), selection(&target_row, 1, 1), 4096, || false).unwrap();
    let back_response = json(&back_response);
    assert_eq!(back_response.get("source_page_id").number(), 0);
    assert_eq!(back_response.get("source_file_id").number(), target_row.file.get());
    assert_eq!(back_response.get("target_page_id").number(), 0);
    assert_eq!(back_response.get("target_file").get("file_id").number(), FIRST_CATALOG_LINK_FILE_ID + 1);
    assert_eq!(back.capture().bytes(), bytes);
    assert_eq!(origin.capture().bytes(), bytes);
    // Page eviction cannot make a caller-selected page or raw label authoritative.
    let mut forged_row = source.clone(); forged_row.page = 0;
    assert!(matches!(catalog.open_document_link(&origin, owner(73905), selection(&forged_row, 1, 1), 4096, || false),
        Err(CatalogSessionError::ReaderReceiptMismatch)));
    let mut relabeled = ReaderSession::from_bytes(owner(73902), Path::new("unrelated.md"), bytes, || false).unwrap();
    prepare(&mut relabeled);
    assert!(matches!(catalog.open_document_link(&relabeled, owner(73905), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::ReaderReceiptMismatch)));
}

#[test]
fn late_cancellation_and_revocation_preserve_origin_and_refuse_new_delivery() {
    let root = new_root();
    let bytes = b"# Guide\n\n[target](target.md)\n";
    let target_bytes = b"# Target\n\n[back](README.md)\n";
    fs::write(root.join("README.md"), bytes).unwrap(); fs::write(root.join("target.md"), target_bytes).unwrap();
    let (mut catalog, found) = catalog(&root, 74001);
    let source = row(&found, "README.md"); let target_row = row(&found, "target.md");
    let origin = open(&mut catalog, &source, 74002);
    let mut polls = 0;
    catalog.open_document_link(&origin, owner(74003), selection(&source, 1, 1), 4096,
        || { polls += 1; false }).unwrap();
    assert!(polls > 2);
    let mut canceled_polls = 0;
    let error = catalog.open_document_link(&origin, owner(74004), selection(&source, 1, 1), 4096,
        || { canceled_polls += 1; canceled_polls == polls }).err().unwrap();
    assert!(error.is_canceled(), "{error}");
    // A rejected delivery cannot manufacture a registered target receipt.
    let mut unaccepted = ReaderSession::from_bytes(owner(74004), Path::new("target.md"), target_bytes, || false).unwrap();
    prepare(&mut unaccepted);
    assert!(matches!(catalog.open_document_link(&unaccepted, owner(74005), selection(&target_row, 1, 1), 4096, || false),
        Err(CatalogSessionError::StaleReaderReceipt)));
    let grant = catalog.grant().clone(); let mut revoked_polls = 0;
    let error = catalog.open_document_link(&origin, owner(74006), selection(&source, 1, 1), 4096, || {
        revoked_polls += 1;
        if revoked_polls == polls { grant.revoke(); }
        false
    }).err().unwrap();
    assert_eq!(error, CatalogSessionError::Source(SourceError::GrantRevoked));
    assert_eq!(origin.capture().bytes(), bytes);
}

#[test]
fn stale_document_generation_and_replaced_symlink_target_are_refused() {
    let root = new_root(); let bytes = b"# Guide\n\n[target](target.rs)\n";
    fs::write(root.join("README.md"), bytes).unwrap(); fs::write(root.join("target.rs"), b"observed").unwrap();
    let (mut catalog, found) = catalog(&root, 74101);
    let source = row(&found, "README.md"); let mut origin = open(&mut catalog, &source, 74102);
    origin.prepare_document(2, ReaderDocumentOptions::default(), || false).unwrap();
    let expected = origin.document_link_destination(1, 1).err().unwrap();
    assert!(matches!(catalog.open_document_link(&origin, owner(74103), selection(&source, 1, 1), 4096, || false),
        Err(CatalogSessionError::Reader(error)) if error == expected));
    origin.prepare_document_links(2, 128, || false).unwrap();
    let outside = root.with_extension("outside"); fs::write(&outside, b"not granted").unwrap();
    fs::rename(root.join("target.rs"), root.join("moved.rs")).unwrap();
    symlink(outside, root.join("target.rs")).unwrap();
    assert!(matches!(catalog.open_document_link(&origin, owner(74103), selection(&source, 2, 1), 4096, || false),
        Err(CatalogSessionError::Source(SourceError::SymlinkForbidden))));
    assert_eq!(origin.capture().bytes(), bytes);
}

#[test]
fn rejected_direct_target_does_not_publish_receipts_or_consume_identity() {
    let root = new_root();
    fs::write(root.join("README.md"), b"# Guide\n\n[target](late.md)\n").unwrap();
    let (mut catalog, found) = catalog(&root, 75701);
    let source = row(&found, "README.md"); let origin = open(&mut catalog, &source, 75702);
    fs::write(root.join("late.md"), b"# Late\n\n[back](README.md)\n").unwrap();
    let (mut rejected, response, acceptance) = catalog.prepare_document_link_capture(&origin, owner(75703),
        selection(&source, 1, 1), 4096, || false).unwrap();
    let response = json(&response);
    let target_row = Row { page: response.get("target_page_id").number(),
        file: FileId::new(owner(75701), response.get("target_file").get("file_id").number()).unwrap(),
        path: "late.md".into(), observed_bytes: response.get("target_file").get("observed_bytes").number() };
    assert_eq!(target_row.page, 0);
    assert_eq!(target_row.file.get(), FIRST_CATALOG_LINK_FILE_ID);
    // The caller can still reject the prepared response at the installation
    // boundary. Neither its receipt nor its identity is committed in that case.
    drop(acceptance);
    prepare(&mut rejected);
    assert!(matches!(catalog.open_document_link(&rejected, owner(75704), selection(&target_row, 1, 1), 4096, || false),
        Err(CatalogSessionError::StaleReaderReceipt)));
    let (_, accepted) = catalog.open_document_link(&origin, owner(75705), selection(&source, 1, 1), 4096, || false).unwrap();
    assert_eq!(json(&accepted).get("target_file").get("file_id").number(), FIRST_CATALOG_LINK_FILE_ID);
}

#[test]
fn direct_targets_still_refuse_symlinks_and_missing_paths_without_source_receipts() {
    let root = new_root(); let outside = new_root();
    fs::write(root.join("README.md"), b"[target](late.md) [nested](linked/target.md) [missing](missing.md)\n").unwrap();
    let (mut catalog, found) = catalog(&root, 75801);
    let source = row(&found, "README.md"); let origin = open(&mut catalog, &source, 75802);
    fs::write(outside.join("target.md"), b"# Outside\n").unwrap();
    symlink(outside.join("target.md"), root.join("late.md")).unwrap();
    symlink(&outside, root.join("linked")).unwrap();
    for link_id in [1, 2] {
        assert!(matches!(catalog.open_document_link(&origin, owner(75803), selection(&source, 1, link_id), 4096, || false),
            Err(CatalogSessionError::Source(SourceError::SymlinkForbidden))));
    }
    assert!(catalog.open_document_link(&origin, owner(75803), selection(&source, 1, 3), 4096, || false).is_err());
    assert_eq!(json(&catalog.page(source.page, || false).unwrap()).get("catalogued_files").number(), 1);
}


#[test]
fn verified_relative_and_absolute_origins_bind_exact_labels_and_distinct_direct_receipts() {
    for (index, absolute) in [false, true].into_iter().enumerate() {
        let base = 75900 + index as u64 * 10;
        let root = new_root();
        let bytes = b"# Original\n\n[target](late.md#details)\n";
        let target_bytes = b"# Late\n\n## Details\n\n[back](README.md)\n";
        fs::write(root.join("README.md"), bytes).unwrap();
        let (mut catalog, found) = catalog(&root, base + 1);
        let source_row = row(&found, "README.md");
        let canonical_root = fs::canonicalize(&root).unwrap();
        let label = if absolute { canonical_root.join("README.md") } else { PathBuf::from("README.md") };
        let mut origin = ReaderSession::from_bytes(owner(base + 2), &label, bytes, || false).unwrap();
        prepare(&mut origin);
        fs::write(root.join("late.md"), target_bytes).unwrap();
        let (mut target, response, acceptance) = catalog.prepare_verified_document_link_capture(&origin,
            owner(base + 3), 1, 1, 4096, || false).unwrap();
        let response = json(&response);
        assert_eq!(response.get("command").text(), "open-document-link");
        assert_eq!(response.get("source_authority").text(), "verified-current-source");
        assert_eq!(response.get("source_page_id").number(), 0);
        assert_eq!(response.get("source_file_id").number(), FIRST_CATALOG_LINK_FILE_ID);
        assert_eq!(response.get("source_capture_sha256").text(), origin.capture_sha256().to_hex());
        assert_eq!(response.get("source_verification_bytes_read").number(), bytes.len() as u64);
        assert!(response.get("source_verification_read_calls").number() > 0);
        assert_eq!(response.get("target_resolution").text(), "root-grant-path");
        assert_eq!(response.get("target_page_id").number(), 0);
        assert_eq!(response.get("target_file").get("file_id").number(), FIRST_CATALOG_LINK_FILE_ID + 1);
        assert_eq!(response.get("source_bytes_read").number(), target_bytes.len() as u64);
        assert_eq!(response.get("fragment_status").text(), "resolved");
        assert!(response.get("initial_original_offset").number() > 0);
        assert_eq!(response.get("catalogued_files").number(), 1);
        assert_eq!(target.capture().bytes(), target_bytes);
        assert_eq!(origin.capture().bytes(), bytes);
        assert_eq!(origin.document_generation(), Some(1));
        acceptance.accept();
        // The verified origin receives authority only for its exact raw label,
        // including absolute labels. An equivalent label cannot impersonate it.
        let source = Row { page: 0, file: FileId::new(owner(base + 1), FIRST_CATALOG_LINK_FILE_ID).unwrap(),
            path: "README.md".into(), observed_bytes: bytes.len() as u64 };
        let different_label = if absolute { PathBuf::from("README.md") } else { canonical_root.join("README.md") };
        let mut relabeled = ReaderSession::from_bytes(owner(base + 2), &different_label, bytes, || false).unwrap();
        prepare(&mut relabeled);
        assert!(matches!(catalog.open_document_link(&relabeled, owner(base + 4), selection(&source, 1, 1), 4096, || false),
            Err(CatalogSessionError::ReaderReceiptMismatch)));
        catalog.open_document_link(&origin, owner(base + 4), selection(&source, 1, 1), 4096, || false).unwrap();
        prepare(&mut target);
        let target_row = Row { page: 0, file: FileId::new(owner(base + 1), FIRST_CATALOG_LINK_FILE_ID + 1).unwrap(),
            path: "late.md".into(), observed_bytes: target_bytes.len() as u64 };
        let (back, back_response) = catalog.open_document_link(&target, owner(base + 5), selection(&target_row, 1, 1), 4096, || false).unwrap();
        assert_eq!(json(&back_response).get("target_file").get("file_id").number(), source_row.file.get());
        assert_eq!(back.capture().bytes(), bytes);
    }
}

#[test]
fn verified_source_changes_and_caller_limit_refuse_without_receipts_or_identity_consumption() {
    let root = new_root();
    let bytes = b"# Original\n\n[target](target.md)\n";
    fs::write(root.join("README.md"), bytes).unwrap();
    fs::write(root.join("target.md"), b"# Target\n").unwrap();
    let (mut catalog, _) = catalog(&root, 76001);
    let mut origin = ReaderSession::from_bytes(owner(76002), Path::new("README.md"), bytes, || false).unwrap();
    prepare(&mut origin);
    assert!(matches!(catalog.prepare_verified_document_link_capture(&origin, owner(76003), 1, 1,
        bytes.len() - 1, || false), Err(CatalogSessionError::InvalidLimits)));
    let mut changed_same_length = bytes.to_vec(); changed_same_length[2] = b'X';
    for changed in [changed_same_length, b"changed".to_vec(), vec![b'x'; bytes.len() + 1]] {
        fs::write(root.join("README.md"), changed).unwrap();
        let error = catalog.prepare_verified_document_link_capture(&origin, owner(76003), 1, 1, 4096, || false).err().unwrap();
        assert_eq!(error, CatalogSessionError::OriginCaptureChanged);
        assert_eq!(error.to_string(), "CATALOG_SESSION_ORIGIN_CAPTURE_CHANGED");
        assert_eq!(origin.capture().bytes(), bytes);
    }
    fs::write(root.join("README.md"), bytes).unwrap();
    let (_, response, acceptance) = catalog.prepare_verified_document_link_capture(&origin, owner(76003), 1, 1, 4096, || false).unwrap();
    assert_eq!(json(&response).get("source_file_id").number(), FIRST_CATALOG_LINK_FILE_ID);
    acceptance.accept();
}

#[test]
fn verified_origins_refuse_path_aliases_outside_roots_and_symlinks() {
    let root = new_root(); let outside = new_root();
    let bytes = b"# Guide\n\n[target](target.md)\n";
    fs::write(root.join("README.md"), bytes).unwrap(); fs::write(root.join("target.md"), b"target").unwrap();
    fs::write(outside.join("README.md"), bytes).unwrap();
    let (mut catalog, _) = catalog(&root, 76101);
    let canonical_root = fs::canonicalize(&root).unwrap();
    let labels = [outside.join("README.md"), canonical_root.join("./README.md"),
        PathBuf::from("./README.md"), PathBuf::from("../README.md"), PathBuf::from("docs//README.md"),
        PathBuf::from("docs\\README.md")];
    for (index, label) in labels.into_iter().enumerate() {
        let mut origin = ReaderSession::from_bytes(owner(76110 + index as u64), &label, bytes, || false).unwrap();
        prepare(&mut origin);
        assert!(matches!(catalog.prepare_verified_document_link_capture(&origin, owner(76103), 1, 1, 4096, || false),
            Err(CatalogSessionError::Source(SourceError::PathEscape))));
    }
    symlink(outside.join("README.md"), root.join("linked.md")).unwrap();
    symlink(&outside, root.join("linked")).unwrap();
    for (index, label) in ["linked.md", "linked/README.md"].into_iter().enumerate() {
        let mut origin = ReaderSession::from_bytes(owner(76120 + index as u64), Path::new(label), bytes, || false).unwrap();
        prepare(&mut origin);
        assert!(matches!(catalog.prepare_verified_document_link_capture(&origin, owner(76103), 1, 1, 4096, || false),
            Err(CatalogSessionError::Source(SourceError::SymlinkForbidden))));
    }
}

#[test]
fn verified_document_occurrences_are_checked_before_root_io() {
    let root = new_root(); let bytes = b"[target](target.md)\n";
    fs::write(root.join("README.md"), bytes).unwrap();
    let (mut catalog, _) = catalog(&root, 76201);
    let mut origin = ReaderSession::from_bytes(owner(76202), Path::new("README.md"), bytes, || false).unwrap();
    prepare(&mut origin);
    let stale_document = origin.document_link_destination(2, 1).err().unwrap();
    let missing_link = origin.document_link_destination(1, 99).err().unwrap();
    catalog.grant().revoke();
    assert!(matches!(catalog.prepare_verified_document_link_capture(&origin, owner(76203), 2, 1, 4096, || false),
        Err(CatalogSessionError::Reader(error)) if error == stale_document));
    assert!(matches!(catalog.prepare_verified_document_link_capture(&origin, owner(76203), 1, 99, 4096, || false),
        Err(CatalogSessionError::Reader(error)) if error == missing_link));
    assert!(matches!(catalog.prepare_verified_document_link_capture(&origin, owner(76203), 1, 1, 4096, || false),
        Err(CatalogSessionError::Source(SourceError::GrantRevoked))));
    assert_eq!(origin.capture().bytes(), bytes);
}

#[test]
fn discarded_verified_delivery_cannot_publish_origin_or_target_authority() {
    let root = new_root(); let bytes = b"[target](late.md)\n";
    fs::write(root.join("README.md"), bytes).unwrap();
    let (mut catalog, _) = catalog(&root, 76301);
    let mut origin = ReaderSession::from_bytes(owner(76302), Path::new("README.md"), bytes, || false).unwrap();
    prepare(&mut origin);
    fs::write(root.join("late.md"), b"[back](README.md)\n").unwrap();
    let (mut rejected, response, acceptance) = catalog.prepare_verified_document_link_capture(&origin,
        owner(76303), 1, 1, 4096, || false).unwrap();
    let response = json(&response);
    let source = Row { page: 0, file: FileId::new(owner(76301), response.get("source_file_id").number()).unwrap(),
        path: "README.md".into(), observed_bytes: bytes.len() as u64 };
    let target = Row { page: 0, file: FileId::new(owner(76301), response.get("target_file").get("file_id").number()).unwrap(),
        path: "late.md".into(), observed_bytes: 0 };
    drop(acceptance);
    prepare(&mut rejected);
    for (reader, row) in [(&origin, &source), (&rejected, &target)] {
        assert!(matches!(catalog.open_document_link(reader, owner(76304), selection(row, 1, 1), 4096, || false),
            Err(CatalogSessionError::StaleReaderReceipt)));
    }
    let (_, accepted, acceptance) = catalog.prepare_verified_document_link_capture(&origin,
        owner(76305), 1, 1, 4096, || false).unwrap();
    let accepted = json(&accepted);
    assert_eq!(accepted.get("source_file_id").number(), source.file.get());
    assert_eq!(accepted.get("target_file").get("file_id").number(), target.file.get());
    acceptance.accept();
}


#[test]
fn verified_origins_accept_only_canonical_and_originally_admitted_root_spellings() {
    let container = new_root();
    let physical_parent = container.join("physical"); fs::create_dir(&physical_parent).unwrap();
    let root = physical_parent.join("project"); fs::create_dir(&root).unwrap();
    let admitted_parent = container.join("selected-parent"); symlink(&physical_parent, &admitted_parent).unwrap();
    let admitted_root = admitted_parent.join("project");
    let unrelated_parent = container.join("unadmitted-parent"); symlink(&physical_parent, &unrelated_parent).unwrap();
    let bytes = b"# Guide\n\n[target](target.md)\n";
    fs::write(root.join("README.md"), bytes).unwrap(); fs::write(root.join("target.md"), b"granted target").unwrap();
    // The selected root itself is a directory; only an ancestor is a symlink.
    // This is the same distinction as ordinary macOS /var and /private/var.
    assert!(!fs::symlink_metadata(&admitted_root).unwrap().file_type().is_symlink());
    let (mut catalog, _) = catalog(&admitted_root, 76401);
    let canonical_root = fs::canonicalize(&root).unwrap();
    for (index, label) in [canonical_root.join("README.md"), admitted_root.join("README.md")].into_iter().enumerate() {
        let mut origin = ReaderSession::from_bytes(owner(76410 + index as u64), &label, bytes, || false).unwrap();
        prepare(&mut origin);
        let (target, response, acceptance) = catalog.prepare_verified_document_link_capture(&origin,
            owner(76420 + index as u64), 1, 1, 4096, || false).unwrap();
        assert_eq!(target.capture().bytes(), b"granted target");
        assert_eq!(json(&response).get("source_verification_bytes_read").number(), bytes.len() as u64);
        assert_eq!(origin.path().as_os_str(), label.as_os_str());
        acceptance.accept();
    }
    // An admitted spelling can itself start with the canonical root. Choosing
    // the shorter canonical prefix would incorrectly treat its symlinked root
    // alias as a source-path component and refuse this legitimate selection.
    symlink(canonical_root.parent().unwrap(), canonical_root.join("up")).unwrap();
    let nested_root = canonical_root.join("up/project");
    let mut nested_catalog = CatalogSession::open(owner(76451), &nested_root, || false).unwrap();
    let mut nested_origin = ReaderSession::from_bytes(owner(76452), &nested_root.join("README.md"), bytes, || false).unwrap();
    prepare(&mut nested_origin);
    let (target, _, acceptance) = nested_catalog.prepare_verified_document_link_capture(&nested_origin,
        owner(76453), 1, 1, 4096, || false).unwrap();
    assert_eq!(target.capture().bytes(), b"granted target");
    acceptance.accept();
    // A different alias to the SAME object was never admitted by project open.
    // Caller labels do not trigger canonicalization, and admitted prefixes do
    // not permit newly inserted dot components or repeated separators either.
    let labels = [unrelated_parent.join("project/README.md"), admitted_root.join("./README.md"),
        admitted_root.join("sub/../README.md"), admitted_root.join("sub//README.md")];
    for (index, label) in labels.into_iter().enumerate() {
        let mut origin = ReaderSession::from_bytes(owner(76430 + index as u64), &label, bytes, || false).unwrap();
        prepare(&mut origin);
        assert!(matches!(catalog.prepare_verified_document_link_capture(&origin, owner(76440), 1, 1, 4096, || false),
            Err(CatalogSessionError::Source(SourceError::PathEscape))));
    }
}
