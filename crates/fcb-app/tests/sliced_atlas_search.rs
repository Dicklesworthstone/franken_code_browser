#![forbid(unsafe_code)]
#![cfg(unix)]

//! Exercise real catalog/capture/query/reader services across verification
//! yields. The first-step assertions fail for the former whole-file verifier;
//! these are work/publication invariants, not wall-clock or native-frame claims.

use std::{
    fs,
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};

use fcb::ArenaOwnerId;
use fcb_app::host::atlas_search::{
    AtlasSearchError, AtlasSearchOptions, AtlasSearchScope, AtlasSearchStop,
    MAX_ATLAS_VERIFICATION_STEP_BYTES, RetainedAtlasSearch,
};
use fcb_app::host::atlas_session::{AtlasSession, AtlasSessionOptions};

const SLICE: usize = MAX_ATLAS_VERIFICATION_STEP_BYTES;

struct Fixture(PathBuf);
impl Fixture {
    fn new(files: &[(&str, &[u8])]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!(
            "fcb-sliced-atlas-{}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos(),
            NEXT.fetch_add(1, Ordering::Relaxed),
        ));
        fs::create_dir_all(&root).unwrap();
        for (name, bytes) in files {
            fs::write(root.join(name), bytes).unwrap();
        }
        Self(root)
    }

    fn open(&self) -> AtlasSession {
        AtlasSession::open(owner(8701), &self.0, AtlasSessionOptions::default(), || {
            false
        })
        .unwrap()
    }
}
// Keep fixtures available for independent replay and capture inspection.

fn owner(value: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(value).unwrap()
}

fn options(source_bytes: usize, max_matches: usize) -> AtlasSearchOptions {
    AtlasSearchOptions {
        max_matches,
        max_files: 16,
        max_file_bytes: 1024 * 1024,
        max_source_bytes: source_bytes,
    }
}

fn finish(search: &mut RetainedAtlasSearch, atlas: &AtlasSession, generation: u64) {
    for _ in 0..128 {
        if !search.progress(atlas, generation).unwrap().is_running() {
            return;
        }
        search.step(atlas, generation, || false).unwrap();
    }
    panic!("bounded fixture did not finish verification");
}

#[derive(Clone, Copy, Debug)]
enum Encoding {
    Utf8,
    Utf16Le,
    Utf16Be,
}

/// Put an emoji's UTF-8 sequence or UTF-16 surrogate pair across the first
/// verification boundary. Original-byte coordinates are constructed directly,
/// independently of the production decoder/search mapping.
fn boundary_source(encoding: Encoding) -> (Vec<u8>, u64, u64) {
    let marker = "😀NeEdLe";
    let mut bytes = match encoding {
        Encoding::Utf8 => vec![b'x'; SLICE - 2],
        Encoding::Utf16Le | Encoding::Utf16Be => {
            let mut bytes = match encoding {
                Encoding::Utf16Le => vec![0xff, 0xfe],
                _ => vec![0xfe, 0xff],
            };
            for _ in 0..(SLICE - 4) / 2 {
                let unit = match encoding {
                    Encoding::Utf16Le => u16::from(b'x').to_le_bytes(),
                    _ => u16::from(b'x').to_be_bytes(),
                };
                bytes.extend_from_slice(&unit);
            }
            bytes
        }
    };
    let start = bytes.len() as u64;
    match encoding {
        Encoding::Utf8 => bytes.extend_from_slice(marker.as_bytes()),
        _ => {
            for unit in marker.encode_utf16() {
                bytes.extend_from_slice(&match encoding {
                    Encoding::Utf16Le => unit.to_le_bytes(),
                    _ => unit.to_be_bytes(),
                });
            }
        }
    }
    let end = bytes.len() as u64;
    match encoding {
        Encoding::Utf8 => bytes.resize(3 * SLICE + 8, b'z'),
        _ => {
            while bytes.len() < 3 * SLICE + 8 {
                bytes.extend_from_slice(&match encoding {
                    Encoding::Utf16Le => u16::from(b'z').to_le_bytes(),
                    _ => u16::from(b'z').to_be_bytes(),
                });
            }
        }
    }
    assert_eq!(start, (SLICE - 2) as u64);
    (bytes, start, end)
}

fn wire_u64(json: &str, key: &str) -> u64 {
    let prefix = format!("\"{key}\":\"");
    let value = json
        .split_once(&prefix)
        .unwrap()
        .1
        .split('"')
        .next()
        .unwrap();
    let number = value.parse::<u64>().unwrap();
    assert_eq!(number.to_string(), value);
    number
}

#[test]
fn large_decoded_files_yield_keep_boundary_hits_and_finish_at_exact_io_cap() {
    for encoding in [Encoding::Utf8, Encoding::Utf16Le, Encoding::Utf16Be] {
        for case_sensitive in [true, false] {
            let (raw, start, end) = boundary_source(encoding);
            let fixture = Fixture::new(&[("source.rs", &raw)]);
            let atlas = fixture.open();
            let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
            let needle = if case_sensitive {
                "😀NeEdLe"
            } else {
                "😀needle"
            };
            search
                .begin_scoped(
                    &atlas,
                    1,
                    needle,
                    AtlasSearchScope::all().with_case_sensitive(case_sensitive),
                    options(raw.len(), 4),
                    || false,
                )
                .unwrap();
            assert_eq!(search.progress(&atlas, 1).unwrap().source_bytes_read, 0);

            search.step(&atlas, 1, || false).unwrap();
            let first = search.progress(&atlas, 1).unwrap();
            assert!(
                first.is_running(),
                "{encoding:?}, case_sensitive={case_sensitive}"
            );
            assert_eq!(
                (
                    first.examined_files,
                    first.scanned_files,
                    first.last_step_files
                ),
                (0, 0, 0)
            );
            assert_eq!(
                (
                    first.matches_seen,
                    first.retained_hits,
                    first.matching_files
                ),
                (0, 0, 0)
            );
            assert_eq!(first.source_bytes_read, raw.len() as u64);
            assert_eq!(first.last_step_source_bytes, raw.len() as u64);
            assert!(first.last_step_read_calls > 0);
            assert_eq!(first.pending_files, 1);
            assert_eq!(search.accepted_generation(), None);
            assert!(matches!(
                search.hit(&atlas, 1, 1),
                Err(AtlasSearchError::MissingHit)
            ));

            // Subsequent slices must use the captured bytes even when the live
            // file changes, and may finish with no remaining I/O allowance.
            fs::write(fixture.0.join("source.rs"), b"replacement without a match").unwrap();
            for _ in 0..16 {
                if !search.progress(&atlas, 1).unwrap().is_running() {
                    break;
                }
                search.step(&atlas, 1, || false).unwrap();
                let progress = search.progress(&atlas, 1).unwrap();
                assert_eq!(progress.source_bytes_read, raw.len() as u64);
                assert_eq!(progress.read_calls, first.read_calls);
                assert_eq!(progress.last_step_source_bytes, 0);
                assert_eq!(progress.last_step_read_calls, 0);
                if progress.is_running() {
                    assert_eq!((progress.examined_files, progress.scanned_files), (0, 0));
                    assert_eq!(
                        (
                            progress.matches_seen,
                            progress.retained_hits,
                            progress.matching_files
                        ),
                        (0, 0, 0)
                    );
                }
            }
            let done = search.progress(&atlas, 1).unwrap();
            assert_eq!(done.stop_reason, Some(AtlasSearchStop::AllFilesExamined));
            assert!(done.complete);
            assert!(!done.truncated);
            assert!(done.step_count > 1);
            assert_eq!(
                (done.examined_files, done.scanned_files, done.pending_files),
                (1, 1, 0)
            );
            assert_eq!(
                (done.matches_seen, done.retained_hits, done.matching_files),
                (1, 1, 1)
            );
            assert_eq!(done.last_step_files, 1);
            assert_eq!(done.retained_source_bytes, raw.len());
            let hit = search.hit(&atlas, 1, 1).unwrap();
            assert_eq!(
                (
                    hit.original_range.start().get(),
                    hit.original_range.end().get()
                ),
                (start, end)
            );
            let (reader, _) = search
                .open_reader(&atlas, owner(8702), 1, 1, || false)
                .unwrap();
            assert_eq!(reader.capture().bytes(), raw);
            search.step(&atlas, 1, || false).unwrap();
            assert_eq!(search.progress(&atlas, 1).unwrap(), done);
        }
    }
}

#[test]
fn counted_queries_publish_one_matching_file_once_across_many_slices() {
    let mut large = b"needle ".to_vec();
    large.resize(SLICE + 8, b'x');
    large.extend_from_slice(b" needle ");
    large.resize(2 * SLICE + 8, b'x');
    large.extend_from_slice(b" needle");
    for retained_limit in [0, 1] {
        let fixture = Fixture::new(&[("a.rs", b"needle"), ("b.rs", &large)]);
        let atlas = fixture.open();
        let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search
            .begin_counted_scoped(
                &atlas,
                1,
                "needle",
                AtlasSearchScope::all(),
                options(6 + large.len(), retained_limit),
                || false,
            )
            .unwrap();
        search.step(&atlas, 1, || false).unwrap();
        let first = search.progress(&atlas, 1).unwrap();
        assert_eq!(
            (
                first.matches_seen,
                first.matching_files,
                first.examined_files
            ),
            (1, 1, 1)
        );
        assert_eq!(first.retained_hits, retained_limit);
        search.step(&atlas, 1, || false).unwrap();
        let running = search.progress(&atlas, 1).unwrap();
        assert!(running.is_running());
        assert_eq!(
            (
                running.matches_seen,
                running.matching_files,
                running.examined_files
            ),
            (1, 1, 1)
        );
        assert_eq!(running.last_step_files, 0);
        assert_eq!(running.last_step_source_bytes, large.len() as u64);

        finish(&mut search, &atlas, 1);
        let done = search.progress(&atlas, 1).unwrap();
        assert!(done.complete && done.match_count_complete && done.count_all_matches);
        assert!(done.truncated);
        assert_eq!(
            (done.matches_seen, done.matching_files, done.examined_files),
            (4, 2, 2)
        );
        assert_eq!(done.retained_hits, retained_limit);
        assert_eq!(done.retained_matching_files, retained_limit);
        assert_eq!(done.retained_source_bytes, 6 * retained_limit);
        assert_eq!(done.source_bytes_read, (6 + large.len()) as u64);
        assert_eq!(done.last_step_source_bytes, 0);
        assert_eq!(done.last_step_read_calls, 0);
        assert_eq!(done.last_step_files, 1);
        if retained_limit == 1 {
            let (reader, _) = search
                .open_reader(&atlas, owner(8702), 1, 1, || false)
                .unwrap();
            assert_eq!(reader.capture().bytes(), b"needle");
        }
        search.step(&atlas, 1, || false).unwrap();
        assert_eq!(search.progress(&atlas, 1).unwrap(), done);
    }
}

#[test]
fn canceled_large_replacement_preserves_accepted_hits_and_an_independent_reader() {
    let mut raw = b"needle ".to_vec();
    raw.resize(3 * SLICE, b'x');
    raw.extend_from_slice(b" replacement");
    let fixture = Fixture::new(&[("source.rs", &raw)]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    search
        .search(&atlas, 1, "needle", options(raw.len(), 4), || false)
        .unwrap();
    let previous = search.page(&atlas, 1, 0, 4, || false).unwrap();
    let hit = search.hit(&atlas, 1, 1).unwrap();
    let (reader, _) = search
        .open_reader(&atlas, owner(8702), 1, 1, || false)
        .unwrap();

    search
        .begin(&atlas, 2, "replacement", options(raw.len(), 4), || false)
        .unwrap();
    search.step(&atlas, 2, || false).unwrap();
    assert!(search.progress(&atlas, 2).unwrap().is_running());
    assert_eq!(search.progress(&atlas, 2).unwrap().examined_files, 0);
    fs::write(fixture.0.join("source.rs"), b"new source").unwrap();
    assert!(matches!(
        search.step(&atlas, 2, || true),
        Err(AtlasSearchError::Canceled)
    ));
    assert_eq!(search.pending_generation(), None);
    assert_eq!(search.accepted_generation(), Some(1));
    assert_eq!(search.hit(&atlas, 1, 1).unwrap(), hit);
    assert_eq!(
        search.page(&atlas, 1, 0, 4, || false).unwrap().as_str(),
        previous.as_str()
    );
    assert!(search.progress(&atlas, 2).is_err());
    drop(search);
    drop(atlas);
    assert_eq!(reader.capture().bytes(), raw);
}

#[test]
fn synchronous_and_sliced_lowercase_search_have_identical_final_results() {
    let (utf8, _, _) = boundary_source(Encoding::Utf8);
    let (utf16, _, _) = boundary_source(Encoding::Utf16Be);
    let fixture = Fixture::new(&[("a.rs", &utf8), ("b.rs", &utf16)]);
    let atlas = fixture.open();
    let limits = options(utf8.len() + utf16.len(), 4);
    let scope = AtlasSearchScope::all().with_case_sensitive(false);
    let mut synchronous = RetainedAtlasSearch::new(&atlas).unwrap();
    let mut sliced = RetainedAtlasSearch::new(&atlas).unwrap();
    synchronous
        .search_scoped(&atlas, 1, "😀needle", scope.clone(), limits, || false)
        .unwrap();
    sliced
        .begin_scoped(&atlas, 1, "😀needle", scope, limits, || false)
        .unwrap();
    finish(&mut sliced, &atlas, 1);
    let done = sliced.progress(&atlas, 1).unwrap();
    assert!(done.step_count > 2 && done.complete);
    assert_eq!((done.matches_seen, done.matching_files), (2, 2));
    assert_eq!(synchronous.progress(&atlas, 1).unwrap(), done);
    for id in 1..=2 {
        assert_eq!(
            synchronous.hit(&atlas, 1, id).unwrap(),
            sliced.hit(&atlas, 1, id).unwrap()
        );
    }
    assert_eq!(
        synchronous
            .page(&atlas, 1, 0, 4, || false)
            .unwrap()
            .as_str(),
        sliced.page(&atlas, 1, 0, 4, || false).unwrap().as_str(),
    );
}

#[test]
fn expression_waits_for_late_required_and_excluded_terms_before_publishing() {
    for rejected in [false, true] {
        let mut raw = b"needle needle ".to_vec();
        raw.resize(2 * SLICE + 16, b'x');
        raw.extend_from_slice(b" required ");
        if rejected {
            raw.extend_from_slice(b"forbidden");
        }
        let fixture = Fixture::new(&[("source.rs", &raw)]);
        let atlas = fixture.open();
        let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search
            .begin_expression_counted_scoped(
                &atlas,
                1,
                "needle required -forbidden",
                AtlasSearchScope::all(),
                options(raw.len(), 1),
                raw.len() as u64,
                || false,
            )
            .unwrap();
        let first = search.step(&atlas, 1, || false).unwrap();
        let initial = search.progress(&atlas, 1).unwrap();
        assert!(initial.is_running());
        assert_eq!(
            (
                initial.matches_seen,
                initial.retained_hits,
                initial.examined_files
            ),
            (0, 0, 0)
        );
        assert_eq!(
            wire_u64(first.as_str(), "scanned_input_bytes"),
            SLICE as u64
        );
        // All predicates must continue on the retained capture, not this edit.
        fs::write(fixture.0.join("source.rs"), b"needle required forbidden").unwrap();
        let mut scanned = SLICE as u64;
        for _ in 0..16 {
            if !search.progress(&atlas, 1).unwrap().is_running() {
                break;
            }
            let reply = search.step(&atlas, 1, || false).unwrap();
            let next = wire_u64(reply.as_str(), "scanned_input_bytes");
            assert!(next >= scanned && next - scanned <= SLICE as u64);
            scanned = next;
            let progress = search.progress(&atlas, 1).unwrap();
            assert_eq!(progress.last_step_source_bytes, 0);
            assert_eq!(progress.last_step_read_calls, 0);
            if progress.is_running() {
                assert_eq!(
                    (
                        progress.matches_seen,
                        progress.retained_hits,
                        progress.examined_files
                    ),
                    (0, 0, 0)
                );
            }
        }
        let done = search.progress(&atlas, 1).unwrap();
        assert!(done.complete && done.match_count_complete);
        assert_eq!(done.stop_reason, Some(AtlasSearchStop::AllFilesExamined));
        assert_eq!(done.examined_files, 1);
        assert_eq!(done.scanned_files, 1);
        assert_eq!(done.matches_seen, if rejected { 0 } else { 2 });
        assert_eq!(done.matching_files, if rejected { 0 } else { 1 });
        assert_eq!(done.retained_hits, if rejected { 0 } else { 1 });
        assert_eq!(done.truncated, !rejected);
        assert_eq!(scanned, raw.len() as u64);
        if !rejected {
            let (reader, _) = search
                .open_reader(&atlas, owner(8702), 1, 1, || false)
                .unwrap();
            assert_eq!(reader.capture().bytes(), raw);
        }
    }
}

#[test]
fn expression_work_limit_never_exposes_primary_hits_with_unresolved_predicates() {
    let mut raw = b"needle ".to_vec();
    raw.resize(3 * SLICE, b'x');
    raw.extend_from_slice(b" required");
    let fixture = Fixture::new(&[("source.rs", &raw)]);
    let atlas = fixture.open();
    let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
    let work_limit = (SLICE + 11) as u64;
    search
        .begin_expression_counted_scoped(
            &atlas,
            1,
            "needle required",
            AtlasSearchScope::all(),
            options(raw.len(), 4),
            work_limit,
            || false,
        )
        .unwrap();
    search.step(&atlas, 1, || false).unwrap();
    assert!(search.progress(&atlas, 1).unwrap().is_running());
    assert_eq!(search.progress(&atlas, 1).unwrap().matches_seen, 0);
    let terminal = search.step(&atlas, 1, || false).unwrap();
    let done = search.progress(&atlas, 1).unwrap();
    assert_eq!(
        done.stop_reason,
        Some(AtlasSearchStop::VerificationByteLimit)
    );
    assert!(!done.complete && !done.match_count_complete);
    assert_eq!(
        (done.matches_seen, done.retained_hits, done.matching_files),
        (0, 0, 0)
    );
    assert_eq!(
        (
            done.examined_files,
            done.scanned_files,
            done.unavailable_files
        ),
        (1, 1, 1)
    );
    assert_eq!(done.source_bytes_read, raw.len() as u64);
    assert_eq!(done.last_step_source_bytes, 0);
    assert_eq!(done.last_step_read_calls, 0);
    assert_eq!(
        wire_u64(terminal.as_str(), "scanned_input_bytes"),
        work_limit
    );
    assert!(matches!(
        search.hit(&atlas, 1, 1),
        Err(AtlasSearchError::MissingHit)
    ));
}
