#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

//! Production worker route: real temporary roots, exact retained Markdown,
//! upstream image occurrences, original encoded PNG bytes and deliberate
//! authority/identity/limit/cancellation failures. Pixel decode is qualified by
//! the native ImageIO consumer, never by this signature-only transport.

use std::{fs, path::{Path, PathBuf}, sync::atomic::{AtomicU64, Ordering},
    time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, document::reader::DocumentReadError, store::Sha256};
use fcb_app::{AppError, host::{HostError, reader::{DocumentImageError,
    ReaderSession, ReaderSessionError, ReaderDocumentOptions, MAX_DOCUMENT_IMAGE_BYTES,
    MAX_DOCUMENT_IMAGE_CACHE_BYTES, MAX_DOCUMENT_IMAGE_CHUNK, MAX_DOCUMENT_IMAGES}}};

// A complete one-pixel red RGBA PNG with IHDR, stored-deflate IDAT and IEND.
const PNG: &[u8] = &[0x89,0x50,0x4e,0x47,0x0d,0x0a,0x1a,0x0a,0x00,0x00,0x00,0x0d,0x49,0x48,0x44,0x52,0x00,0x00,0x00,0x01,0x00,0x00,0x00,0x01,0x08,0x06,0x00,0x00,0x00,0x1f,0x15,0xc4,0x89,0x00,0x00,0x00,0x10,0x49,0x44,0x41,0x54,0x78,0x01,0x01,0x05,0x00,0xfa,0xff,0x00,0xff,0x00,0x00,0xff,0x05,0x00,0x01,0xff,0xfa,0x5c,0x88,0xd1,0x00,0x00,0x00,0x00,0x49,0x45,0x4e,0x44,0xae,0x42,0x60,0x82];
static NEXT_ROOT: AtomicU64 = AtomicU64::new(0);
struct Root(PathBuf);
impl Root {
    fn new() -> Self {
        let nonce = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
        let path = std::env::temp_dir().join(format!("fcb-document-images-{}-{nonce}-{}",
            std::process::id(), NEXT_ROOT.fetch_add(1, Ordering::Relaxed)));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn path(&self) -> &Path { &self.0 }
    fn write(&self, relative: &str, bytes: &[u8]) {
        let target = self.0.join(relative);
        if let Some(parent) = target.parent() { fs::create_dir_all(parent).unwrap(); }
        fs::write(target, bytes).unwrap();
    }
}
impl Drop for Root {
    fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); }
}
fn error(value: DocumentImageError) -> ReaderSessionError { ReaderSessionError::DocumentImage(value) }
fn number(json: &str, key: &str) -> u64 { field(json, key).parse().unwrap() }
fn field<'a>(json: &'a str, key: &str) -> &'a str {
    json.split_once(&format!("\"{key}\":\"")).unwrap().1.split('"').next().unwrap()
}
fn bytes(json: &str) -> Vec<u8> {
    field(json, "image_hex").as_bytes().chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect()
}
fn digest(bytes: &[u8]) -> String { let mut hash = Sha256::new(); hash.update(bytes); hash.finalize().to_hex() }
fn targets(json: &str) -> Vec<(usize, u64)> {
    json.split("\"item_index\":\"").skip(1).filter_map(|item| {
        item.contains("\"kind\":\"image\"").then(|| (
            item.split('"').next().unwrap().parse().unwrap(), number(item, "request_id")))
    }).collect()
}
fn prepared(root: &Root, label: &str, source: &[u8]) -> (ReaderSession, Vec<(usize, u64)>) {
    root.write(label, source);
    let mut reader = ReaderSession::from_bytes(ArenaOwnerId::new(8873).unwrap(),
        Path::new(label), source, || false).unwrap();
    reader.prepare_document(1, Default::default(), || false).unwrap();
    let display = reader.prepare_document_display(1, 640, || false).unwrap();
    let images = targets(display.as_str());
    (reader, images)
}

#[test]
fn explicit_authorization_binds_current_source_and_replays_immutable_png_without_more_io() {
    let root = Root::new(); root.write("images/logo.png", PNG);
    let source = b"# Readme\n\n![Logo](images/logo.png)\n";
    let (mut reader, images) = prepared(&root, "README.md", source);
    let (item, request) = images[0];
    assert_eq!(reader.prepare_document_image(1, item, request, || false).err(),
        Some(error(DocumentImageError::NotAuthorized)));
    let authorized = reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
    assert_eq!(number(authorized.as_str(), "additional_source_bytes_read"), source.len() as u64);
    assert_eq!(number(authorized.as_str(), "source_verification_bytes_read"), source.len() as u64);
    assert!(number(authorized.as_str(), "source_verification_read_calls") > 0);
    assert!(authorized.as_str().contains("\"authorization_reused\":false"));
    assert_eq!(field(authorized.as_str(), "capture_sha256"), digest(source));
    let image = reader.prepare_document_image(1, item, request, || false).unwrap();
    assert_eq!(bytes(image.as_str()), PNG);
    assert_eq!(field(image.as_str(), "image_sha256"), digest(PNG));
    assert_eq!(field(image.as_str(), "image_format"), "png");
    assert_eq!(number(image.as_str(), "total_image_bytes"), PNG.len() as u64);
    assert_eq!(number(image.as_str(), "image_bytes_read"), PNG.len() as u64);
    assert_eq!(number(image.as_str(), "additional_source_bytes_read"), 0);
    assert!(image.as_str().contains("\"decoded\":false"));
    assert!(image.as_str().contains("\"format_validation\":\"signature-only\""));
    assert!(image.as_str().contains("\"image_observation\":\"current-root-capture\""));
    assert!(image.as_str().contains("\"next_image_offset\":null"));
    // Removing both live files is an oracle for accidental rereads. Previously
    // captured image bytes and same-generation authorization still replay.
    fs::remove_file(root.path().join("README.md")).unwrap();
    fs::remove_file(root.path().join("images/logo.png")).unwrap();
    let repeated = reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
    assert!(repeated.as_str().contains("\"authorization_reused\":true"));
    assert_eq!(number(repeated.as_str(), "additional_source_bytes_read"), 0);
    assert_eq!(number(repeated.as_str(), "source_verification_read_calls"), 0);
    let again = reader.prepare_document_image(1, item, request, || false).unwrap();
    assert_eq!(bytes(again.as_str()), PNG);
    assert_eq!(number(again.as_str(), "image_bytes_read"), 0);
    let tail = reader.document_image_chunk(1, item, request, 9, 11, || false).unwrap();
    assert_eq!(bytes(tail.as_str()), &PNG[9..20]);
    assert_eq!(number(tail.as_str(), "image_offset"), 9);
    assert_eq!(number(tail.as_str(), "next_image_offset"), 20);
    assert_eq!(number(tail.as_str(), "image_read_calls"), 0);
    let eof = reader.document_image_chunk(1, item, request, PNG.len(), 1, || false).unwrap();
    assert!(bytes(eof.as_str()).is_empty());
    assert!(eof.as_str().contains("\"next_image_offset\":null"));
    assert_eq!(reader.document_image_chunk(1, item, request, PNG.len() + 1, 1, || false).err(),
        Some(error(DocumentImageError::InvalidRange)));
    assert_eq!(reader.document_image_chunk(1, item, request, 0, MAX_DOCUMENT_IMAGE_CHUNK + 1, || false).err(),
        Some(error(DocumentImageError::InvalidLimits)));
    assert_eq!(reader.capture().bytes(), source);
}

#[test]
fn labels_equal_digests_and_stale_sources_do_not_grant_another_origin() {
    let root = Root::new();
    let source = b"# Original\n\n![Logo](logo.png)\n";
    let (mut reader, images) = prepared(&root, "README.md", source);
    root.write("other.md", source);
    assert_eq!(reader.authorize_document_images(1, root.path(), b"other.md", || false).err(),
        Some(error(DocumentImageError::OriginMismatch)));
    root.write("README.md", b"# Different\n\n![Logo](logo.png)\n");
    assert_eq!(reader.authorize_document_images(1, root.path(), b"README.md", || false).err(),
        Some(error(DocumentImageError::OriginChanged)));
    assert_eq!(reader.prepare_document_image(1, images[0].0, images[0].1, || false).err(),
        Some(error(DocumentImageError::NotAuthorized)));
    assert!(reader.authorize_document_images(1, root.path(), b"../README.md", || false).is_err());
    assert!(reader.authorize_document_images(2, root.path(), b"README.md", || false).is_err());
    root.write("README.md", source);
    reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
    assert_eq!(reader.authorize_document_images(1, root.path(), b"other.md", || false).err(),
        Some(error(DocumentImageError::DifferentAuthority)));
    let other_root = Root::new(); other_root.write("README.md", source);
    assert_eq!(reader.authorize_document_images(1, other_root.path(), b"README.md", || false).err(),
        Some(error(DocumentImageError::DifferentAuthority)));
}

#[test]
fn image_paths_use_upstream_single_decode_and_root_relative_resolution() {
    let root = Root::new();
    root.write("images/a b.png", PNG);
    let mut other = PNG.to_vec(); other.extend_from_slice(b"distinct retained observation");
    root.write("images/a%20b.png", &other);
    let source = b"# Images\n\n![First](../images/a%20b.png)\n\n![Second](../images/a%2520b.png)\n";
    let (mut reader, images) = prepared(&root, "docs/guide.md", source);
    assert_eq!(images.len(), 2);
    reader.authorize_document_images(1, root.path(), b"docs/guide.md", || false).unwrap();
    let first = reader.prepare_document_image(1, images[0].0, images[0].1, || false).unwrap();
    assert_eq!(bytes(first.as_str()), PNG);
    assert_eq!(field(first.as_str(), "destination"), "../images/a%20b.png");
    let second = reader.prepare_document_image(1, images[1].0, images[1].1, || false).unwrap();
    assert_eq!(bytes(second.as_str()), other);
    assert_eq!(field(second.as_str(), "destination"), "../images/a%2520b.png");
    assert!(reader.prepare_document_image(1, images[0].0, images[0].1.wrapping_add(1), || false).is_err());
    assert!(reader.document_image_chunk(1, usize::MAX, images[0].1, 0, 1, || false).is_err());
}

#[test]
fn external_escaping_query_and_unsupported_image_requests_never_become_local_files() {
    for destination in ["https://invalid.example/a.png", "//invalid.example/a.png",
        "file:///tmp/a.png", "data:image/png;base64,AAAA", "/tmp/a.png",
        "../escape.png", "%2e%2e/escape.png", "bad%zz.png", "logo.png?cache=1", "logo.png#fragment"] {
        let root = Root::new(); root.write("logo.png", PNG);
        let source = format!("# Images\n\n![Logo]({destination})\n");
        let (mut reader, images) = prepared(&root, "README.md", source.as_bytes());
        assert_eq!(images.len(), 1, "{destination}");
        reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
        assert_eq!(reader.prepare_document_image(1, images[0].0, images[0].1, || false).err(),
            Some(error(DocumentImageError::UnsupportedDestination)), "{destination}");
    }
    let root = Root::new(); root.write("image.gif", b"GIF89a");
    let (mut reader, images) = prepared(&root, "README.md", b"![Image](image.gif)\n");
    reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
    assert_eq!(reader.prepare_document_image(1, images[0].0, images[0].1, || false).err(),
        Some(error(DocumentImageError::UnsupportedFormat)));
    assert_eq!(reader.document_image_chunk(1, images[0].0, images[0].1, 0, 1, || false).err(),
        Some(error(DocumentImageError::MissingImage)));
}

#[cfg(unix)]
#[test]
fn observed_leaf_parent_and_source_symlinks_cannot_supply_image_authority() {
    use std::os::unix::fs::symlink;
    let outside = Root::new(); outside.write("logo.png", PNG);
    for parent in [false, true] {
        let root = Root::new();
        if parent { symlink(outside.path(), root.path().join("images")).unwrap(); }
        else {
            fs::create_dir(root.path().join("images")).unwrap();
            symlink(outside.path().join("logo.png"), root.path().join("images/logo.png")).unwrap();
        }
        let (mut reader, images) = prepared(&root, "README.md", b"![Logo](images/logo.png)\n");
        reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
        assert!(reader.prepare_document_image(1, images[0].0, images[0].1, || false).is_err());
        assert_eq!(reader.document_image_chunk(1, images[0].0, images[0].1, 0, 1, || false).err(),
            Some(error(DocumentImageError::MissingImage)));
    }
    let root = Root::new(); let source = b"![Logo](logo.png)\n";
    outside.write("README.md", source);
    symlink(outside.path().join("README.md"), root.path().join("README.md")).unwrap();
    let mut reader = ReaderSession::from_bytes(ArenaOwnerId::new(8874).unwrap(), Path::new("README.md"), source, || false).unwrap();
    reader.prepare_document(1, Default::default(), || false).unwrap();
    reader.prepare_document_display(1, 640, || false).unwrap();
    assert!(reader.authorize_document_images(1, root.path(), b"README.md", || false).is_err());
}

#[test]
fn canceled_candidates_failed_reflow_forks_and_successful_clear_keep_authorities_separate() {
    let root = Root::new(); root.write("logo.png", PNG);
    let (mut reader, images) = prepared(&root, "README.md", b"# Images\n\n![Logo](logo.png)\n\nTail.\n");
    let (item, request) = images[0];
    assert_eq!(reader.authorize_document_images(1, root.path(), b"README.md", || true).err(),
        Some(ReaderSessionError::Canceled));
    assert_eq!(reader.prepare_document_image(1, item, request, || false).err(),
        Some(error(DocumentImageError::NotAuthorized)));
    reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
    assert_eq!(reader.prepare_document_image(1, item, request, || true).err(),
        Some(ReaderSessionError::Canceled));
    assert_eq!(reader.document_image_chunk(1, item, request, 0, 1, || false).err(),
        Some(error(DocumentImageError::MissingImage)));
    reader.prepare_document_image(1, item, request, || false).unwrap();
    assert!(reader.prepare_document(2, ReaderDocumentOptions { max_flow_lines: 1,
        ..Default::default() }, || false).is_err());
    assert_eq!(bytes(reader.document_image_chunk(1, item, request, 0, 7, || false).unwrap().as_str()), &PNG[..7]);
    let mut fork = reader.fork(ArenaOwnerId::new(8875).unwrap(), || false).unwrap();
    fork.prepare_document(1, Default::default(), || false).unwrap();
    let fork_page = fork.prepare_document_display(1, 640, || false).unwrap();
    let fork_target = targets(fork_page.as_str())[0];
    assert_eq!(fork.prepare_document_image(1, fork_target.0, fork_target.1, || false).err(),
        Some(error(DocumentImageError::NotAuthorized)));
    reader.prepare_document(3, Default::default(), || false).unwrap();
    reader.prepare_document_display(3, 320, || false).unwrap();
    assert_eq!(reader.document_image_chunk(1, item, request, 0, 1, || false).err(),
        Some(ReaderSessionError::Document(DocumentReadError::StaleGeneration)));
    assert_eq!(reader.prepare_document_image(3, item, request, || false).err(),
        Some(error(DocumentImageError::NotAuthorized)));
    reader.clear_document(4, || false).unwrap();
    assert_eq!(reader.document_image_chunk(3, item, request, 0, 1, || false).err(),
        Some(ReaderSessionError::MissingDocument));
}

#[test]
fn byte_and_count_limits_are_enforced_before_retained_cache_growth() {
    let root = Root::new();
    let mut large = vec![0; MAX_DOCUMENT_IMAGE_BYTES]; large[..PNG.len()].copy_from_slice(PNG);
    for name in ["a.png", "b.png", "c.png"] { root.write(name, &large); }
    let (mut reader, images) = prepared(&root, "README.md", b"![A](a.png)\n\n![B](b.png)\n\n![C](c.png)\n");
    reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
    for (index, &(item, request)) in images[..2].iter().enumerate() {
        let image = reader.prepare_document_image(1, item, request, || false).unwrap();
        assert_eq!(bytes(image.as_str()), large[..MAX_DOCUMENT_IMAGE_CHUNK]);
        assert_eq!(number(image.as_str(), "next_image_offset"), MAX_DOCUMENT_IMAGE_CHUNK as u64);
        assert_eq!(number(image.as_str(), "retained_image_bytes"), ((index + 1) * large.len()) as u64);
    }
    assert_eq!(2 * large.len(), MAX_DOCUMENT_IMAGE_CACHE_BYTES);
    assert_eq!(reader.prepare_document_image(1, images[2].0, images[2].1, || false).err(),
        Some(error(DocumentImageError::CacheLimit)));
    let tail = reader.document_image_chunk(1, images[0].0, images[0].1,
        large.len() - 8, MAX_DOCUMENT_IMAGE_CHUNK, || false).unwrap();
    assert_eq!(bytes(tail.as_str()), vec![0; 8]);
    assert_eq!(number(tail.as_str(), "image_bytes_read"), 0);
    large.push(0); root.write("too-large.png", &large);
    let (mut oversized, target) = prepared(&root, "OVERSIZED.md", b"![Large](too-large.png)\n");
    oversized.authorize_document_images(1, root.path(), b"OVERSIZED.md", || false).unwrap();
    assert_eq!(oversized.prepare_document_image(1, target[0].0, target[0].1, || false).err(),
        Some(ReaderSessionError::Host(HostError::App(AppError::InputLimit))));
    let mut source = String::from("# Images\n\n");
    for index in 0..=MAX_DOCUMENT_IMAGES {
        let name = format!("{index}.png"); root.write(&name, PNG);
        source.push_str(&format!("![Image]({name})\n\n"));
    }
    let (mut counted, targets) = prepared(&root, "COUNT.md", source.as_bytes());
    counted.authorize_document_images(1, root.path(), b"COUNT.md", || false).unwrap();
    assert_eq!(targets.len(), MAX_DOCUMENT_IMAGES + 1);
    for &(item, request) in &targets[..MAX_DOCUMENT_IMAGES] {
        counted.prepare_document_image(1, item, request, || false).unwrap();
    }
    assert_eq!(counted.prepare_document_image(1, targets[MAX_DOCUMENT_IMAGES].0,
        targets[MAX_DOCUMENT_IMAGES].1, || false).err(), Some(error(DocumentImageError::CacheLimit)));
}

#[test]
fn jpeg_signature_admission_explicitly_does_not_claim_a_successful_decode() {
    let root = Root::new(); root.write("truncated.jpg", &[0xff, 0xd8, 0xff]);
    let (mut reader, images) = prepared(&root, "README.md", b"![Invalid JPEG](truncated.jpg)\n");
    reader.authorize_document_images(1, root.path(), b"README.md", || false).unwrap();
    let image = reader.prepare_document_image(1, images[0].0, images[0].1, || false).unwrap();
    assert_eq!(bytes(image.as_str()), [0xff, 0xd8, 0xff]);
    assert_eq!(field(image.as_str(), "image_format"), "jpeg");
    assert!(image.as_str().contains("\"format_validation\":\"signature-only\",\"decoded\":false"));
    assert!(!image.as_str().contains("width_pixels"));
}
