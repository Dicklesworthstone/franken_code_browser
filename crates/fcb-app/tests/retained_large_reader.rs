#![forbid(unsafe_code)]
#![cfg(unix)]

//! Production file capture -> retained paging/lines/find/copy, beyond the
//! legacy 4 MiB string boundary. Execution belongs to independent strict RCH.
mod support;

use std::{fs, io::{Seek, SeekFrom, Write}, path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering}, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, analysis::comparison::ComparisonError};
use fcb_app::{AppError, EXIT_OK, EXIT_PARTIAL, host::{self, HostError, HostResponse,
    MAX_HOST_TEXT_BYTES, desk::{DeskError, DeskLimits, DeskSession, DeskSessionError,
        imports::ImportedSourceId},
    reader::{ReaderComparisonLimits, ReaderSession, ReaderSessionError,
        MAX_READER_SOURCE_BYTES, MAX_READER_WINDOW_BYTES}}};
use support::{Json, parse};

fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn file(bytes: &[u8]) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-large-reader-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&root).unwrap();
    let path = root.join("source.rs"); fs::write(&path, bytes).unwrap(); path
}
fn json(response: &HostResponse) -> Json { parse(response.as_str().as_bytes()).unwrap() }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }
fn encode(text: &str, encoding: u8) -> Vec<u8> {
    match encoding {
        0 => text.as_bytes().to_vec(),
        1 => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
        2 => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
        _ => unreachable!(),
    }
}
fn is_canceled(error: ReaderSessionError) -> bool {
    match error {
        ReaderSessionError::Canceled => true,
        ReaderSessionError::Host(HostError::App(error)) | ReaderSessionError::App(error) => error.is_canceled(),
        _ => false,
    }
}

#[test]
fn large_utf8_and_utf16_files_keep_exact_far_lines_search_and_copy_after_replacement() {
    const PREFIX_LINES: u64 = 900_000;
    let prefix = format!("\u{feff}{}", "pad\r\n".repeat(PREFIX_LINES as usize));
    let selected_line = "🦀 e\u{301}\0 needle\r\n";
    for encoding in 0..3 {
        let mut bytes = encode(&prefix, encoding);
        let line_start = bytes.len() as u64;
        bytes.extend_from_slice(&encode(selected_line, encoding));
        bytes.extend_from_slice(&encode("last", encoding));
        assert!(line_start > MAX_HOST_TEXT_BYTES as u64);
        let path = file(&bytes);
        let mut reader = ReaderSession::open(owner(9890 + u64::from(encoding)), &path,
            MAX_READER_SOURCE_BYTES, || false).unwrap();
        assert_eq!(reader.capture().bytes(), bytes);
        let info = json(&reader.info(|| false).unwrap());
        assert_eq!(info.get("captured_bytes").number(), bytes.len() as u64);
        assert_eq!(info.get("initial_source_bytes_read").number(), bytes.len() as u64);
        assert!(info.get("initial_read_calls").number() > 64);
        assert!(info.get("initial_read_calls").number() <= host::MAX_HOST_READ_CALLS);

        // Subsequent work has no authority to reopen the replacement pathname.
        fs::rename(&path, path.with_extension("captured")).unwrap();
        fs::write(&path, b"replacement without the old source").unwrap();
        let mut cold_checks = 0;
        let cold = reader.read_lines(PREFIX_LINES + 1, 1, 128,
            || { cold_checks += 1; false }).unwrap();
        assert_eq!(cold.exit_code(), EXIT_OK);
        let lines = json(&cold);
        assert_eq!(lines.get("text").text().as_bytes(), selected_line.as_bytes());
        assert_eq!(lines.get("first_physical_line").number(), PREFIX_LINES + 1);
        assert_eq!(lines.get("visible_range").get("start").number(), line_start);
        assert_eq!(lines.get("original_hex").text(), hex(&encode(selected_line, encoding)));
        assert_eq!(lines.get("additional_source_bytes_read").number(), 0);
        let mut warm_checks = 0;
        let warm = reader.read_lines(PREFIX_LINES + 1, 1, 128,
            || { warm_checks += 1; false }).unwrap();
        assert_eq!(warm.as_str(), cold.as_str());
        assert!(cold_checks > warm_checks + 32, "checkpoints beyond 4 MiB were lost: {cold_checks}/{warm_checks}");

        let exact = reader.read_window(line_start, encode(selected_line, encoding).len(), || false).unwrap();
        assert_eq!(json(&exact).get("text").text().as_bytes(), selected_line.as_bytes());
        let found = reader.search(1, "needle", 8, bytes.len() as u64, || false).unwrap();
        assert_eq!(found.exit_code(), EXIT_OK);
        let found = json(&found);
        assert!(found.get("search_complete").flag());
        assert_eq!(found.get("scanned_bytes").number(), bytes.len() as u64);
        assert_eq!(found.get("retained_hits").number(), 1);
        let start = line_start + encode("🦀 e\u{301}\0 ", encoding).len() as u64;
        let needle = encode("needle", encoding);
        let target = reader.hit_range(1, 0).unwrap();
        assert_eq!(target.start().get(), start);
        assert_eq!(target.end().get(), start + needle.len() as u64);
        let hit = json(&reader.hit_window(1, 0, 32, || false).unwrap());
        let mapped = hit.get("selection").get("window_utf8_range");
        assert_eq!(&hit.get("text").text().as_bytes()[mapped.get("start").number() as usize
            ..mapped.get("end").number() as usize], b"needle");
        assert_eq!(hit.get("selection").get("original_range").get("start").number(), start);
        assert_eq!(hit.get("additional_source_bytes_read").number(), 0);
        assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), hex(&needle));
        assert_eq!(json(&reader.copy_range(target.start().get(), target.end().get(), || false).unwrap())
            .get("original_hex").text(), hex(&needle));
    }
}

#[test]
fn scalar_and_surrogate_pairs_cross_the_old_capture_boundary_without_byte_changes() {
    for encoding in 0..3 {
        let prefix = if encoding == 0 {
            "x".repeat(MAX_HOST_TEXT_BYTES - 1)
        } else {
            format!("\u{feff}{}", "x".repeat(MAX_HOST_TEXT_BYTES / 2 - 2))
        };
        let mut bytes = encode(&prefix, encoding);
        let start = bytes.len() as u64;
        let needle = "🦀\r\n";
        let encoded_needle = encode(needle, encoding);
        bytes.extend_from_slice(&encoded_needle);
        bytes.extend_from_slice(&encode("tail", encoding));
        assert!(start < MAX_HOST_TEXT_BYTES as u64);
        assert!(start + encode("🦀", encoding).len() as u64 > MAX_HOST_TEXT_BYTES as u64);
        let path = file(&bytes);
        let mut reader = ReaderSession::open(owner(9900 + u64::from(encoding)), &path,
            MAX_READER_SOURCE_BYTES, || false).unwrap();
        assert_eq!(reader.capture().bytes(), bytes);
        reader.search(1, needle, 1, bytes.len() as u64, || false).unwrap();
        let range = reader.hit_range(1, 0).unwrap();
        assert_eq!(range.start().get(), start);
        assert_eq!(range.len().get(), encoded_needle.len() as u64);
        let hit = json(&reader.hit_window(1, 0, 0, || false).unwrap());
        assert!(!hit.get("has_replacements").flag());
        assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), hex(&encoded_needle));
        assert_eq!(json(&reader.read_lines(2, 1, 32, || false).unwrap()).get("text").text(), "tail");
    }
}

#[test]
fn metadata_catalog_finder_and_atlas_paths_open_the_same_large_retained_source() {
    use fcb::FileId;
    use fcb_app::host::{catalog_session::CatalogSession,
        file_finder_session::{FileFinderSession, FileFinderOptions, PathMatchMode},
        atlas_paths::{RetainedAtlasPaths, AtlasPathOptions}, atlas_session::AtlasSession};
    let mut bytes = vec![b'x'; MAX_HOST_TEXT_BYTES + 16];
    let tail = bytes.len() as u64;
    bytes.extend_from_slice(b"\nneedle\n");
    let path = file(&bytes);
    let root = path.parent().unwrap();
    let mut catalog = CatalogSession::open(owner(9940), root, || false).unwrap();
    let page = json(&catalog.page(1, || false).unwrap());
    assert_eq!(page.get("discovery_source_bytes_read").number(), 0);
    let id = FileId::new(owner(9940), page.get("files").array()[0].get("file_id").number()).unwrap();
    assert!(catalog.open_reader(owner(9941), 1, id, MAX_READER_SOURCE_BYTES + 1, || false).is_err());
    let (catalog_reader, receipt) = catalog.open_reader(owner(9941), 1, id,
        MAX_READER_SOURCE_BYTES, || false).unwrap();
    let receipt = json(&receipt);
    assert_eq!(receipt.get("source_bytes_read").number(), bytes.len() as u64);
    assert_eq!(receipt.get("reader").get("captured_bytes").number(), bytes.len() as u64);
    assert_eq!(receipt.get("window").get("visible_range").get("end").number(), 16 * 1024);

    let mut finder = FileFinderSession::open(owner(9942), root, 1, b"source.rs",
        FileFinderOptions { mode: PathMatchMode::Exact, ..Default::default() }, || false).unwrap();
    let mut result = json(&finder.info(|| false).unwrap());
    for _ in 0..4 {
        if !result.get("more").flag() { break; }
        result = json(&finder.advance(1, result.get("revision").number(), 1, || false).unwrap());
    }
    assert!(result.get("search_complete").flag());
    assert_eq!(result.get("hits").array().len(), 1);
    let id = FileId::new(owner(9942), result.get("hits").array()[0].get("file_id").number()).unwrap();
    let (finder_reader, receipt) = finder.open_reader(owner(9943), 1,
        result.get("revision").number(), id, MAX_READER_SOURCE_BYTES, || false).unwrap();
    assert_eq!(json(&receipt).get("source_bytes_read").number(), bytes.len() as u64);

    let atlas = AtlasSession::open(owner(9944), root, Default::default(), || false).unwrap();
    let mut paths = RetainedAtlasPaths::new(&atlas).unwrap();
    paths.find(&atlas, 1, b"source.rs", AtlasPathOptions { mode: PathMatchMode::Exact,
        ..Default::default() }, || false).unwrap();
    let id = paths.hits(&atlas, 1).unwrap()[0].file;
    let (path_reader, _) = paths.open_reader(&atlas, owner(9945), 1, id,
        MAX_READER_SOURCE_BYTES, || false).unwrap();

    fs::write(&path, b"replacement").unwrap();
    catalog.grant().revoke(); finder.grant().revoke();
    drop(paths); drop(atlas); drop(catalog); drop(finder);
    for mut reader in [catalog_reader, finder_reader, path_reader] {
        assert_eq!(reader.capture().bytes(), bytes);
        let page = json(&reader.read_window(tail, 64, || false).unwrap());
        assert_eq!(page.get("text").text(), "\nneedle\n");
        assert_eq!(page.get("additional_source_bytes_read").number(), 0);
    }
}

#[test]
fn full_32_mib_capture_and_shared_pins_keep_smaller_page_transfer_and_analysis_limits() {
    assert_eq!(MAX_READER_SOURCE_BYTES, 32 * 1024 * 1024);
    assert_eq!(MAX_HOST_TEXT_BYTES, 4 * 1024 * 1024);
    assert_eq!(MAX_READER_WINDOW_BYTES, 256 * 1024);
    let path = file(b"");
    let suffix = b"\nneedle\n";
    let start = (MAX_READER_SOURCE_BYTES - suffix.len()) as u64;
    let mut disk = fs::OpenOptions::new().write(true).open(&path).unwrap();
    disk.set_len(MAX_READER_SOURCE_BYTES as u64).unwrap();
    disk.seek(SeekFrom::Start(start)).unwrap(); disk.write_all(suffix).unwrap(); drop(disk);
    let mut reader = ReaderSession::open(owner(9910), &path, MAX_READER_SOURCE_BYTES, || false).unwrap();
    assert_eq!(reader.capture().bytes().len(), MAX_READER_SOURCE_BYTES);
    let page = reader.read_lines(1, 1, 4096, || false).unwrap();
    assert_eq!(page.exit_code(), EXIT_PARTIAL);
    assert!(json(&page).get("range_limited").flag());
    assert_eq!(json(&page).get("next_offset").number(), 4096);
    assert!(page.as_str().len() < 40_000);
    assert_eq!(reader.read_lines(3, 1, 32, || false).err(), Some(ReaderSessionError::MissingLine));
    let found = reader.search(1, "needle", 1, MAX_READER_SOURCE_BYTES as u64, || false).unwrap();
    assert_eq!(found.exit_code(), EXIT_OK);
    assert_eq!(reader.hit_range(1, 0).unwrap().start().get(), start + 1);
    assert_eq!(reader.read_window(0, MAX_READER_WINDOW_BYTES + 1, || false).err(), Some(ReaderSessionError::InvalidLimits));
    assert_eq!(reader.copy_range(0, MAX_READER_WINDOW_BYTES as u64 + 1, || false).err(), Some(ReaderSessionError::InvalidRange));
    for limit in [MAX_HOST_TEXT_BYTES, MAX_READER_SOURCE_BYTES] {
        assert_eq!(host::read_text(&path, limit, || false).err(), Some(HostError::App(AppError::InputLimit)));
    }

    let mut pin = reader.fork(owner(9911), || false).unwrap();
    assert!(std::ptr::eq(reader.capture().bytes().as_ptr(), pin.capture().bytes().as_ptr()));
    assert_eq!(reader.compare_retained(&pin, owner(9912), 1,
        ReaderComparisonLimits { max_source_bytes: MAX_HOST_TEXT_BYTES, ..Default::default() }, || false).err(),
        Some(ReaderSessionError::Comparison(ComparisonError::Limits)));
    let mut desk = DeskSession::new(owner(9913), DeskLimits {
        source_bytes: MAX_HOST_TEXT_BYTES as u64, ..Default::default()
    }).unwrap();
    let request = reader.capture().request();
    let revision = desk.model().revision();
    assert_eq!(desk.import_source(revision, 1,
        ImportedSourceId { file: request.file(), revision: request.revision() }, "large.rs",
        reader.capture().bytes(), 0, None, || false).err(), Some(DeskSessionError::Desk(DeskError::SourceLimit)));
    assert_eq!(desk.model().revision(), revision);
    assert_eq!(desk.model().retained_source_count(), 0);
    drop(reader);
    assert_eq!(json(&pin.read_window(start, suffix.len(), || false).unwrap()).get("text").text(), "\nneedle\n");
}

#[test]
fn oversized_files_and_supplied_captures_are_refused_without_a_prefix() {
    let path = file(b"small");
    for limit in [0, MAX_READER_SOURCE_BYTES + 1] {
        assert_eq!(ReaderSession::open(owner(9920), &path, limit, || false).err(),
            Some(ReaderSessionError::Host(HostError::App(AppError::InputLimit))));
    }
    fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(MAX_READER_SOURCE_BYTES as u64 + 1).unwrap();
    assert_eq!(ReaderSession::open(owner(9921), &path, MAX_READER_SOURCE_BYTES, || false).err(),
        Some(ReaderSessionError::Host(HostError::App(AppError::InputLimit))));
    let oversized = vec![b'x'; MAX_READER_SOURCE_BYTES + 1];
    assert_eq!(ReaderSession::from_bytes(owner(9922), Path::new("supplied"), &oversized, || false).err(),
        Some(ReaderSessionError::InvalidLimits));
}

#[test]
fn canceled_large_operations_and_changed_observations_never_publish_partial_source() {
    let mut bytes = vec![b'x'; 5 * 1024 * 1024];
    bytes.extend_from_slice(b"\nneedle\n");
    let path = file(&bytes);
    for stop_at in [1, 64] {
        let mut polls = 0;
        let result = ReaderSession::open(owner(9930), &path, MAX_READER_SOURCE_BYTES,
            || { polls += 1; polls >= stop_at });
        assert!(result.err().is_some_and(is_canceled));
        assert!(polls >= stop_at);
    }
    let mut reader = ReaderSession::open(owner(9931), &path, MAX_READER_SOURCE_BYTES, || false).unwrap();
    assert_eq!(reader.capture().bytes(), bytes);
    reader.search(1, "needle", 1, bytes.len() as u64, || false).unwrap();
    let mut polls = 0;
    let result = reader.search(2, "missing", 1, bytes.len() as u64,
        || { polls += 1; polls >= 32 });
    assert!(result.err().is_some_and(is_canceled));
    assert_eq!(reader.accepted_generation(), Some(1));
    assert_eq!(json(&reader.copy_hit(1, 0, || false).unwrap()).get("original_hex").text(), hex(b"needle"));
    let mut polls = 0;
    assert_eq!(reader.read_lines(2, 1, 32, || { polls += 1; polls >= 4 }).err(), Some(ReaderSessionError::Canceled));
    assert_eq!(json(&reader.read_lines(2, 1, 32, || false).unwrap()).get("text").text(), "needle\n");

    let mut polls = 0;
    let changed = ReaderSession::open(owner(9932), &path, MAX_READER_SOURCE_BYTES, || {
        polls += 1;
        if polls == 2 {
            fs::OpenOptions::new().write(true).open(&path).unwrap().set_len(bytes.len() as u64 + 1).unwrap();
        }
        false
    });
    assert_eq!(changed.err(), Some(ReaderSessionError::Host(HostError::App(AppError::SourceChanged))));
    assert_eq!(reader.capture().bytes(), bytes, "failed observation replaced a previously retained source");
}
