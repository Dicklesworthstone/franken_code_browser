#![forbid(unsafe_code)]
#![cfg(unix)]

//! Explicit links may reach root-granted files that discovery has not observed.
//! Their authority still comes from an accepted origin capture, and a rejected
//! candidate must not mutate the catalog's bounded receipt set.

mod support;
use support::{Json, parse};
use std::{fs, os::unix::fs::symlink, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}};
use fcb::{ArenaOwnerId, FileId};
use fcb::source::SourceError;
use fcb_app::host::{HostResponse, catalog_session::{CatalogDocumentLinkSelection,
    CatalogSession, CatalogSessionError, FIRST_CATALOG_LINK_FILE_ID,
    MAX_CATALOG_READER_RECEIPTS, MAX_CATALOG_RESPONSE_BYTES},
    reader::{ReaderDocumentOptions, ReaderSession}};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn json(response: &HostResponse) -> Json {
    assert!(response.as_str().len() <= MAX_CATALOG_RESPONSE_BYTES);
    parse(response.as_str().as_bytes()).unwrap()
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let stamp = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)
        .unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-direct-link-adversarial-{}-{stamp}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap();
    path // Keep private fixtures for diagnosis, as in the catalog integration tests.
}
fn prepare(reader: &mut ReaderSession) {
    reader.prepare_document(1, ReaderDocumentOptions::default(), || false).unwrap();
    reader.prepare_document_links(1, 16, || false).unwrap();
}
fn fixture(catalog_owner: u64, source: &[u8])
    -> (PathBuf, CatalogSession, ReaderSession, CatalogDocumentLinkSelection) {
    let root = root();
    fs::write(root.join("README.md"), source).unwrap();
    let mut catalog = CatalogSession::open(owner(catalog_owner), &root, || false).unwrap();
    let page = json(&catalog.page(1, || false).unwrap());
    assert_eq!(page.get("files").array().len(), 1);
    assert!(!page.get("more").flag());
    let row = &page.get("files").array()[0];
    assert_eq!(row.get("path").get("hex").text(), hex(b"README.md"));
    let selection = CatalogDocumentLinkSelection {
        source_page: page.get("page_id").number(),
        source_file: FileId::new(catalog.owner(), row.get("file_id").number()).unwrap(),
        document_generation: 1, link_id: 1,
    };
    let (mut reader, _) = catalog.open_reader(owner(catalog_owner + 1),
        selection.source_page, selection.source_file, 4096, || false).unwrap();
    prepare(&mut reader);
    (root, catalog, reader, selection)
}
fn direct_selection(response: &HostResponse, catalog_owner: ArenaOwnerId)
    -> CatalogDocumentLinkSelection {
    let receipt = json(response);
    CatalogDocumentLinkSelection {
        source_page: receipt.get("target_page_id").number(),
        source_file: FileId::new(catalog_owner,
            receipt.get("target_file").get("file_id").number()).unwrap(),
        document_generation: 1, link_id: 1,
    }
}
fn assert_direct(response: &HostResponse, path: &[u8], file: u64) {
    let receipt = json(response);
    assert_eq!(receipt.get("source_authority").text(), "retained-catalog-capture");
    assert_eq!(receipt.get("target_resolution").text(), "root-grant-path");
    assert_eq!(receipt.get("target_page_id").number(), 0);
    assert_eq!(receipt.get("target_file").get("file_id").number(), file);
    assert_eq!(receipt.get("target_file").get("path").get("hex").text(), hex(path));
    assert_eq!(receipt.get("catalogued_files").number(), 1);
}

#[test]
fn unobserved_leaf_and_ancestor_symlinks_do_not_become_root_granted_captures() {
    let source = b"# Origin\n\n[leaf](late.md)\n\n[ancestor](nested/target.md)\n";
    let (root, mut catalog, origin, selection) = fixture(88401, source);
    let outside = self::root();
    fs::write(outside.join("target.md"), b"outside the selected root").unwrap();
    symlink(outside.join("target.md"), root.join("late.md")).unwrap();
    symlink(&outside, root.join("nested")).unwrap();

    for link_id in 1..=2 {
        let error = catalog.open_document_link(&origin, owner(88410 + link_id),
            CatalogDocumentLinkSelection { link_id, ..selection }, 4096, || false)
            .err().unwrap();
        assert_eq!(error, CatalogSessionError::Source(SourceError::SymlinkForbidden));
    }

    // Preserve the refused paths as separate fixtures. Ordinary files at the
    // same explicit destinations are permitted without advancing discovery.
    fs::rename(root.join("late.md"), root.join("retained-leaf-symlink")).unwrap();
    fs::write(root.join("late.md"), b"permitted leaf").unwrap();
    let (leaf, reply) = catalog.open_document_link(&origin, owner(88420), selection,
        4096, || false).unwrap();
    assert_eq!(leaf.capture().bytes(), b"permitted leaf");
    assert_direct(&reply, b"late.md", FIRST_CATALOG_LINK_FILE_ID);

    fs::rename(root.join("nested"), root.join("retained-ancestor-symlink")).unwrap();
    fs::create_dir(root.join("nested")).unwrap();
    fs::write(root.join("nested/target.md"), b"permitted descendant").unwrap();
    let (descendant, reply) = catalog.open_document_link(&origin, owner(88421),
        CatalogDocumentLinkSelection { link_id: 2, ..selection }, 4096, || false).unwrap();
    assert_eq!(descendant.capture().bytes(), b"permitted descendant");
    assert_direct(&reply, b"nested/target.md", FIRST_CATALOG_LINK_FILE_ID + 1);
    assert_eq!(json(&catalog.info(|| false).unwrap()).get("catalogued_files").number(), 1);
    assert_eq!(origin.capture().bytes(), source);
}

#[test]
fn abandoned_direct_candidates_neither_grant_receipts_nor_refresh_the_origin() {
    let source = b"# Origin\n\n[next](late.md)\n";
    let target = b"# Target\n\n[back](README.md)\n";
    let (root, mut catalog, origin, selection) = fixture(88601, source);
    fs::write(root.join("late.md"), target).unwrap();
    let catalog_owner = catalog.owner();

    // Fill the bounded receipt set, leaving the first accepted origin oldest.
    for index in 1..MAX_CATALOG_READER_RECEIPTS {
        catalog.open_reader(owner(88610 + index as u64), selection.source_page,
            selection.source_file, 4096, || false).unwrap();
    }
    let (mut candidate, reply, acceptance) = catalog.prepare_document_link_capture(
        &origin, owner(88630), selection, 4096, || false).unwrap();
    assert_direct(&reply, b"late.md", FIRST_CATALOG_LINK_FILE_ID);
    let candidate_selection = direct_selection(&reply, catalog_owner);
    drop(acceptance); // Simulate an empty destination rejecting installation.
    prepare(&mut candidate);
    assert!(matches!(catalog.open_document_link(&candidate, owner(88631),
        candidate_selection, 4096, || false), Err(CatalogSessionError::StaleReaderReceipt)));

    // One accepted unrelated capture must still evict the oldest receipt. The
    // abandoned activation must not have refreshed that origin behind the UI.
    let (mut newest, _) = catalog.open_reader(owner(88632), selection.source_page,
        selection.source_file, 4096, || false).unwrap();
    prepare(&mut newest);
    assert!(matches!(catalog.open_document_link(&origin, owner(88633), selection,
        4096, || false), Err(CatalogSessionError::StaleReaderReceipt)));

    let (accepted, reply) = catalog.open_document_link(&newest, owner(88634),
        selection, 4096, || false).unwrap();
    assert_direct(&reply, b"late.md", FIRST_CATALOG_LINK_FILE_ID);
    assert_eq!(accepted.capture().bytes(), target);
    assert_eq!(candidate.capture().bytes(), target);
    assert_eq!(origin.capture().bytes(), source);
}

#[test]
fn final_direct_capture_cancellation_cannot_mint_a_receipt_or_consume_an_identity() {
    let source = b"# Origin\n\n[next](late.md)\n";
    let target = b"# Target\n\n[back](README.md)\n";
    let (root, mut catalog, mut origin, selection) = fixture(88801, source);
    fs::write(root.join("late.md"), target).unwrap();
    let catalog_owner = catalog.owner();
    let mut polls = 0;
    let (measured, reply, acceptance) = catalog.prepare_document_link_capture(
        &origin, owner(88810), selection, 4096, || { polls += 1; false }).unwrap();
    let candidate_selection = direct_selection(&reply, catalog_owner);
    assert_direct(&reply, b"late.md", FIRST_CATALOG_LINK_FILE_ID);
    drop(acceptance);
    drop(measured);
    drop(reply);
    assert!(polls > 2);

    let mut canceled_polls = 0;
    let error = catalog.prepare_document_link_capture(&origin, owner(88803),
        selection, 4096, || {
            canceled_polls += 1;
            canceled_polls == polls
        }).err().unwrap();
    assert!(error.is_canceled(), "{error}");
    assert_eq!(canceled_polls, polls);
    let mut supplied = ReaderSession::from_bytes(owner(88803), Path::new("late.md"),
        target, || false).unwrap();
    prepare(&mut supplied);
    assert!(matches!(catalog.open_document_link(&supplied, owner(88804),
        candidate_selection, 4096, || false), Err(CatalogSessionError::StaleReaderReceipt)));

    let (accepted, reply) = catalog.open_document_link(&origin, owner(88805),
        selection, 4096, || false).unwrap();
    assert_direct(&reply, b"late.md", FIRST_CATALOG_LINK_FILE_ID);
    assert_eq!(accepted.capture().bytes(), target);
    catalog.grant().revoke();
    assert_eq!(catalog.open_document_link(&origin, owner(88806), selection,
        4096, || false).err().unwrap(), CatalogSessionError::Source(SourceError::GrantRevoked));
    // Revoking the live project cannot replace or disable already captured data.
    assert_eq!(json(&origin.copy_range(0, source.len() as u64, || false).unwrap())
        .get("original_hex").text(), hex(source));
}
