#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Actual archive -> retained Markdown -> upstream image occurrence -> encoded
//! chunk route. ImageIO pixel decoding is a separate native qualification.
mod support;
#[path = "support/saved_desk.rs"] mod fixture;
use fixture::{Fixture, owner, entry};
use support::{Json, parse};
use std::{fs::{self, OpenOptions}, io::{Seek, SeekFrom, Write}, path::Path};
use fcb::{search::snapshot::{SnapshotData, SnapshotEntry}, store::Sha256};
use fcb_app::host::{HostResponse, reader::{ReaderSession, ReaderSessionError,
    DocumentImageError, MAX_DOCUMENT_IMAGE_CHUNK, MAX_DOCUMENT_IMAGES},
    saved_repository::{SavedRepositorySession, SavedRepositoryError}};

const PNG: &[u8] = &[0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,0x00,0x00,0x00,0x0d,0x49,0x48,0x44,0x52,0x00,0x00,0x00,0x01,0x00,0x00,0x00,0x01,0x08,0x06,0x00,0x00,0x00,0x1f,0x15,0xc4,0x89,0x00,0x00,0x00,0x10,0x49,0x44,0x41,0x54,0x78,0x01,0x01,0x05,0x00,0xfa,0xff,0x00,0xff,0x00,0x00,0xff,0x05,0x00,0x01,0xff,0xfa,0x5c,0x88,0xd1,0x00,0x00,0x00,0x00,0x49,0x45,0x4e,0x44,0xae,0x42,0x60,0x82];
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn error(value: DocumentImageError) -> SavedRepositoryError {
    SavedRepositoryError::Reader(ReaderSessionError::DocumentImage(value))
}
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn reads(saved: &mut SavedRepositorySession) -> (u64, u64) {
    let result = json(&saved.info(|| false).unwrap());
    (result.get("member_bytes_read").number(), result.get("member_read_calls").number())
}
fn display(reader: &mut ReaderSession, generation: u64) -> Vec<(usize, u64)> {
    reader.prepare_document(generation, Default::default(), || false).unwrap();
    let mut page = json(&reader.prepare_document_display(generation, 640, || false).unwrap());
    let mut images = Vec::new();
    loop {
        for item in page.get("display_items").array() {
            if item.get("kind").text() == "image" {
                images.push((item.get("item_index").number() as usize, item.get("request_id").number()));
            }
        }
        if page.get("next_display_item") == &Json::Null { break; }
        page = json(&reader.document_display_page(generation,
            page.get("next_display_item").number() as usize, 64, || false).unwrap());
    }
    images
}
fn opened(saved: &mut SavedRepositorySession, member: usize) -> (ReaderSession, Vec<(usize, u64)>) {
    let (mut reader, _) = saved.open_member_reader(owner(saved.owner().get() + 100), member, || false).unwrap();
    let images = display(&mut reader, 1); (reader, images)
}

#[test]
fn captured_png_uses_exact_saved_provenance_and_retained_chunks_survive_archive_close() {
    let source = b"# Captured guide\n\n![Logo](images/logo.png)\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"images/logo.png", PNG)], true);
    fs::write(f.root.join("README.md"), b"different live source").unwrap();
    fs::create_dir(f.root.join("images")).unwrap();
    fs::write(f.root.join("images/logo.png"), b"unrelated live asset").unwrap();
    let mut saved = f.open(58801); let (mut reader, images) = opened(&mut saved, 0);
    let (item, request) = images[0]; let before = reads(&mut saved);
    let authorized = json(&saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap());
    assert_eq!(authorized.get("schema").text(), "fcb.reader-session/1");
    assert_eq!(authorized.get("source_authority").text(), "verified-saved-member");
    assert_eq!(authorized.get("saved_archive_owner").number(), 58801);
    assert_eq!(authorized.get("saved_archive_digest").text(), saved.archive_digest().to_hex());
    assert_eq!(authorized.get("saved_source_member").number(), 0);
    assert_eq!(authorized.get("source_relative").get("hex").text(), hex(b"README.md"));
    assert!(!authorized.get("live_root_authority").flag());
    assert_eq!(authorized.get("capture_sha256").text(), Sha256::digest(source).to_hex());
    assert_eq!(authorized.get("source_verification_bytes_read").number(), 0);
    assert_eq!(reads(&mut saved), before);
    // Rename after archive open; acquisition must use the verified open archive,
    // without consulting either the old archive name or original live tree.
    fs::rename(&f.path, f.root.join("moved.fcbs")).unwrap();
    let captured = json(&saved.prepare_document_image(&mut reader, 0, 1, item, request, || false).unwrap());
    assert_eq!(captured.get("image_hex").text(), hex(PNG));
    assert_eq!(captured.get("image_sha256").text(), Sha256::digest(PNG).to_hex());
    assert_eq!(captured.get("image_member").number(), 1);
    assert_eq!(captured.get("image_archive_digest").text(), saved.archive_digest().to_hex());
    assert_eq!(captured.get("image_observation").text(), "verified-saved-member");
    assert_eq!(captured.get("format_validation").text(), "signature-only");
    assert!(!captured.get("decoded").flag());
    assert_eq!(captured.get("image_bytes_read").number(), PNG.len() as u64);
    assert_eq!(reads(&mut saved).0 - before.0, PNG.len() as u64);
    let after = reads(&mut saved);
    let replay = json(&saved.prepare_document_image(&mut reader, 0, 1, item, request, || false).unwrap());
    assert_eq!(replay.get("image_bytes_read").number(), 0);
    assert_eq!(reads(&mut saved), after);
    // A saved authorization cannot be reused by the current-root route.
    assert_eq!(reader.prepare_document_image(1, item, request, || false).err(),
        Some(ReaderSessionError::DocumentImage(DocumentImageError::DifferentAuthority)));
    drop(saved);
    fs::remove_file(f.root.join("moved.fcbs")).unwrap();
    let chunk = json(&reader.document_image_chunk(1, item, request, 9, 11, || false).unwrap());
    assert_eq!(chunk.get("image_hex").text(), hex(&PNG[9..20]));
    assert_eq!(chunk.get("image_read_calls").number(), 0);
    assert_eq!(reader.capture().bytes(), source);
}

#[test]
fn identical_destinations_still_require_exact_occurrence_and_document_generation() {
    let source = b"![First](logo.png)\n\n![Second](logo.png)\n";
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"logo.png", PNG)], true);
    let mut saved = f.open(58802); let (mut reader, images) = opened(&mut saved, 0);
    assert_eq!(images.len(), 2); assert_ne!(images[0].0, images[1].0);
    saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap();
    let before = reads(&mut saved);
    let wrong_request = images.iter().map(|image| image.1).max().unwrap().checked_add(1).unwrap();
    assert!(saved.prepare_document_image(&mut reader, 0, 1, images[0].0, wrong_request, || false).is_err());
    assert_eq!(reads(&mut saved), before);
    for &(item, request) in &images {
        let image = json(&saved.prepare_document_image(&mut reader, 0, 1, item, request, || false).unwrap());
        assert_eq!(image.get("item_index").number(), item as u64);
        assert_eq!(image.get("request_id").number(), request);
        assert_eq!(image.get("image_hex").text(), hex(PNG));
    }
    let current = display(&mut reader, 2);
    assert!(saved.prepare_document_image(&mut reader, 0, 1, images[0].0, images[0].1, || false).is_err());
    assert_eq!(saved.prepare_document_image(&mut reader, 0, 2, current[0].0, current[0].1, || false).err(),
        Some(error(DocumentImageError::NotAuthorized)));
    saved.authorize_document_images(&mut reader, 0, 2, || false).unwrap();
    assert!(saved.prepare_document_image(&mut reader, 0, 2, current[0].0, current[0].1, || false).is_ok());
}

#[test]
fn source_and_archive_authority_cannot_be_replaced_by_equal_labels_or_equal_content() {
    let source = b"![Logo](logo.png)\n";
    let a = Fixture::new(&[entry(b"README.md", source), entry(b"logo.png", PNG)], true);
    let mut other_png = PNG.to_vec(); other_png.extend_from_slice(b"different captured asset");
    let b = Fixture::new(&[entry(b"README.md", source), entry(b"logo.png", &other_png)], true);
    let mut saved = a.open(58803); let mut same_archive = a.open(58804); let mut other_archive = b.open(58805);
    let (mut reader, images) = opened(&mut saved, 0);
    saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap();
    for another in [&mut same_archive, &mut other_archive] {
        let before = reads(another);
        assert_eq!(another.authorize_document_images(&mut reader, 0, 1, || false).err(),
            Some(error(DocumentImageError::DifferentAuthority)));
        assert_eq!(another.prepare_document_image(&mut reader, 0, 1, images[0].0, images[0].1, || false).err(),
            Some(error(DocumentImageError::DifferentAuthority)));
        assert_eq!(reads(another), before);
    }
    for (label, bytes) in [("other.md", source.as_slice()), ("README.md", b"![Changed](logo.png)\n".as_slice())] {
        let mut invalid = ReaderSession::from_bytes(owner(58990), Path::new(label), bytes, || false).unwrap();
        display(&mut invalid, 1);
        assert_eq!(saved.authorize_document_images(&mut invalid, 0, 1, || false).err(),
            Some(error(DocumentImageError::OriginMismatch)));
    }
    assert_eq!(saved.authorize_document_images(&mut reader, 1, 1, || false).err(),
        Some(error(DocumentImageError::OriginMismatch)));
}

#[test]
fn archive_resolution_decodes_once_and_refuses_missing_unavailable_or_nonlocal_images() {
    let source = b"![One](../images/a%20b.png)\n\n![Two](../images/a%2520b.png)\n";
    let mut other = PNG.to_vec(); other.extend_from_slice(b"literal percent filename");
    let f = Fixture::new(&[entry(b"docs/guide.md", source), entry(b"images/a b.png", PNG),
        entry(b"images/a%20b.png", &other)], true);
    let mut saved = f.open(58806); let (mut reader, images) = opened(&mut saved, 0);
    saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap();
    for (index, expected) in [PNG, other.as_slice()].into_iter().enumerate() {
        let image = json(&saved.prepare_document_image(&mut reader, 0, 1,
            images[index].0, images[index].1, || false).unwrap());
        assert_eq!(image.get("image_hex").text(), hex(expected));
        assert_eq!(image.get("image_member").number(), index as u64 + 1);
    }

    for (index, destination) in ["missing.png", "blocked.png", "https://example.invalid/x.png",
        "//example.invalid/x.png", "file:///tmp/x.png", "data:image/png;base64,AAAA",
        "../escape.png", "%2e%2e/escape.png", "/outside.png", "logo.png?x=1", "logo.png#part", "bad%zz.png"].iter().enumerate() {
        let doc = format!("![Image]({destination})\n");
        let f = Fixture::new(&[entry(b"README.md", doc.as_bytes()), entry(b"logo.png", PNG),
            SnapshotEntry { path: b"blocked.png", observed_bytes: 100, data: SnapshotData::Unavailable("READ_DENIED") }], true);
        fs::write(f.root.join("missing.png"), PNG).unwrap();
        fs::write(f.root.join("blocked.png"), PNG).unwrap();
        let mut saved = f.open(58900 + index as u64); let (mut reader, images) = opened(&mut saved, 0);
        saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap();
        let before = reads(&mut saved);
        let expected = match index { 0 => DocumentImageError::MissingSavedMember,
            1 => DocumentImageError::UnavailableSavedMember, _ => DocumentImageError::UnsupportedDestination };
        assert_eq!(saved.prepare_document_image(&mut reader, 0, 1, images[0].0, images[0].1, || false).err(),
            Some(error(expected)), "{destination}");
        assert_eq!(reads(&mut saved), before);
    }
}

#[test]
fn corrupt_captured_assets_are_not_replaced_by_live_bytes_and_admitted_images_are_immutable() {
    for load_first in [false, true] {
        let source = b"![Logo](logo.png)\n";
        let f = Fixture::new(&[entry(b"README.md", source), entry(b"logo.png", PNG)], true);
        fs::write(f.root.join("logo.png"), PNG).unwrap();
        let mut saved = f.open(58807); let (mut reader, images) = opened(&mut saved, 0);
        let (item, request) = images[0];
        saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap();
        if load_first { saved.prepare_document_image(&mut reader, 0, 1, item, request, || false).unwrap(); }
        let offset = f.bytes.windows(PNG.len()).position(|part| part == PNG).unwrap();
        let mut writer = OpenOptions::new().write(true).open(&f.path).unwrap();
        writer.seek(SeekFrom::Start(offset as u64)).unwrap(); writer.write_all(b"X").unwrap(); writer.flush().unwrap();
        let result = saved.prepare_document_image(&mut reader, 0, 1, item, request, || false);
        if load_first {
            assert_eq!(json(&result.unwrap()).get("image_hex").text(), hex(PNG));
        } else {
            assert!(result.is_err());
            assert_eq!(reader.document_image_chunk(1, item, request, 0, 1, || false).err(),
                Some(ReaderSessionError::DocumentImage(DocumentImageError::MissingImage)));
        }
        assert_eq!(reader.capture().bytes(), source);
    }
}

#[test]
fn cancellation_during_member_read_drops_candidate_and_retry_keeps_reader_source() {
    let source = b"![Logo](logo.png)\n"; let mut asset = PNG.to_vec();
    asset.resize(3 * MAX_DOCUMENT_IMAGE_CHUNK, 0); // Signature transport; no decode claim.
    let f = Fixture::new(&[entry(b"README.md", source), entry(b"logo.png", &asset)], true);
    let mut saved = f.open(58808); let (mut reader, images) = opened(&mut saved, 0);
    let (item, request) = images[0];
    assert!(saved.authorize_document_images(&mut reader, 0, 1, || true).is_err());
    saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap();
    let mut polls = 0;
    let result = saved.prepare_document_image(&mut reader, 0, 1, item, request, || {
        polls += 1; polls >= 4
    });
    assert!(result.err().unwrap().is_canceled());
    assert_eq!(reader.document_image_chunk(1, item, request, 0, 1, || false).err(),
        Some(ReaderSessionError::DocumentImage(DocumentImageError::MissingImage)));
    assert_eq!(reader.capture().bytes(), source);
    let accepted = json(&saved.prepare_document_image(&mut reader, 0, 1, item, request, || false).unwrap());
    assert_eq!(accepted.get("image_hex").text(), hex(&asset[..MAX_DOCUMENT_IMAGE_CHUNK]));
    assert_eq!(accepted.get("next_image_offset").number(), MAX_DOCUMENT_IMAGE_CHUNK as u64);
    let tail = json(&reader.document_image_chunk(1, item, request, 2 * MAX_DOCUMENT_IMAGE_CHUNK,
        MAX_DOCUMENT_IMAGE_CHUNK, || false).unwrap());
    assert_eq!(tail.get("image_hex").text(), hex(&asset[2 * MAX_DOCUMENT_IMAGE_CHUNK..]));
    assert_eq!(tail.get("next_image_offset"), &Json::Null);
}

#[test]
fn bounded_image_count_rejects_new_occurrences_before_member_io_and_preserves_cached_images() {
    let source = (0..=MAX_DOCUMENT_IMAGES).map(|index| format!("![Image {index}](logo.png)\n\n")).collect::<String>();
    let f = Fixture::new(&[entry(b"README.md", source.as_bytes()), entry(b"logo.png", PNG)], true);
    let mut saved = f.open(58809); let (mut reader, images) = opened(&mut saved, 0);
    assert_eq!(images.len(), MAX_DOCUMENT_IMAGES + 1);
    saved.authorize_document_images(&mut reader, 0, 1, || false).unwrap();
    for &(item, request) in &images[..MAX_DOCUMENT_IMAGES] {
        saved.prepare_document_image(&mut reader, 0, 1, item, request, || false).unwrap();
    }
    let before = reads(&mut saved); let (item, request) = images[MAX_DOCUMENT_IMAGES];
    assert_eq!(saved.prepare_document_image(&mut reader, 0, 1, item, request, || false).err(),
        Some(error(DocumentImageError::CacheLimit)));
    assert_eq!(reads(&mut saved), before);
    assert_eq!(json(&saved.prepare_document_image(&mut reader, 0, 1,
        images[0].0, images[0].1, || false).unwrap()).get("image_hex").text(), hex(PNG));
}
