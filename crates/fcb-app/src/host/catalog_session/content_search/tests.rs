#![forbid(unsafe_code)]

use super::*;
use std::{fs, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use super::super::super::atlas_search::AtlasSearchPathKind;

fn fixture(label: &str) -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(1);
    let path = std::env::temp_dir().join(format!("fcb-catalog-stream-{label}-{}-{}-{}", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(),
        NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap();
    path // Fixtures remain available to the independent qualification runner.
}
fn owner(value: u64) -> ArenaOwnerId { ArenaOwnerId::new(value).unwrap() }
fn counted() -> CatalogContentSearchOptions {
    CatalogContentSearchOptions { count_all_matches: true, ..CatalogContentSearchOptions::default() }
}
fn finish(session: &mut CatalogContentSearchSession) {
    for _ in 0..10_000 {
        if !session.more() { return; }
        let before = session.io;
        let reply = session.advance(session.generation.get(), session.revision, MAX_CATALOG_STREAM_STEPS, || false).unwrap();
        assert!(session.io.bytes_read - before.bytes_read <= (MAX_CATALOG_STREAM_STEPS * 16 * 1024) as u64);
        assert!(session.catalog.pages.len() <= super::super::MAX_CATALOG_RETAINED_PAGES);
        assert!(session.catalog.pages.iter().map(|page| page.files.len()).sum::<usize>()
            <= super::super::MAX_CATALOG_RETAINED_FILES);
        assert!(reply.as_str().contains("\"schema\":\"fcb.catalog-stream/1\""));
        assert!(!reply.as_str().contains("node_ordinal"));
        assert!(!reply.as_str().contains("layout_revision"));
    }
    panic!("catalog content scan did not reach a bounded terminal state");
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&if little { unit.to_le_bytes() } else { unit.to_be_bytes() });
    }
    bytes
}

#[test]
fn discovers_a_match_after_twenty_thousand_files_without_retaining_the_visited_catalog() {
    let root = fixture("beyond-atlas-limit");
    for index in 0..20_600 { fs::write(root.join(format!("file-{index:05}.rs")), b"").unwrap(); }
    let mut options = counted(); options.limits.max_matches = 4;
    let mut session = CatalogContentSearchSession::open(owner(78101), &root, 1, "needle", options, || false).unwrap();
    let early_path = session.catalog.pages.back().unwrap().files[0].path.raw().to_path_buf();
    fs::write(root.join(&early_path), b"needle").unwrap();
    // The traversal itself selects the late source, avoiding an assumption
    // that a native directory iterator has lexical/insertion order.
    while session.catalog.catalogued_files <= 20_000 {
        assert!(session.more()); session.step(&mut || false).unwrap();
        assert!(session.catalog.pages.len() <= 4);
    }
    let late = session.catalog.pages.back().unwrap().files.last().unwrap();
    assert!(late.id.get() > 20_000);
    let late_id = late.id;
    let late_path = late.path.raw().to_path_buf();
    fs::write(root.join(&late_path), b"needle").unwrap();
    finish(&mut session);
    assert!(session.complete());
    assert_eq!(session.examined, 20_600);
    assert_eq!(session.matches_seen, 2);
    assert_eq!(session.hits.len(), 2);
    assert_eq!(session.hits[1].file.request.file(), late_id);
    assert!(session.catalog.pages.front().unwrap().id > 1, "the early hit's discovery page was evicted");
    assert_eq!(session.witnesses, b"needleneedle");
    assert_eq!(session.contexts, b"needleneedle");
    let before = session.io;
    fs::write(root.join(early_path), b"replaced early source").unwrap();
    fs::write(root.join(late_path), b"replaced late source").unwrap();
    for position in 0..2 {
        let witness = session.hit(1, session.revision, position, || false).unwrap();
        assert!(witness.as_str().contains("\"original_hex\":\"6e6565646c65\""));
        assert!(witness.as_str().contains("\"capture_kind\":\"literal-witnesses-with-context\""));
        assert!(witness.as_str().contains("\"context\":{\"state\":\"retained\""));
        assert!(witness.as_str().contains("\"text_original_range\":{\"start\":\"0\",\"end\":\"6\"}"));
        assert!(witness.as_str().contains("\"additional_source_bytes_read\":\"0\""));
    }
    assert_eq!(session.io, before);
}

#[test]
fn text_occurrences_and_expressions_keep_encoding_scope_and_original_witnesses() {
    let root = fixture("modes");
    fs::create_dir(root.join("src")).unwrap();
    fs::write(root.join(".gitignore"), b"ignored.rs\n").unwrap();
    fs::write(root.join("src/good.rs"), utf16("needle required needle", true)).unwrap();
    fs::write(root.join("src/bad.rs"), b"needle required forbidden").unwrap();
    fs::write(root.join("src/other.txt"), b"needle required").unwrap();
    fs::write(root.join("src/ignored.rs"), b"needle required").unwrap();
    fs::write(root.join("src/malformed.rs"), b"needle required\xff").unwrap();
    let scope = AtlasSearchScope::from_csv(Some("rs"), None).unwrap()
        .with_path(AtlasSearchPathKind::Directory, "src").unwrap();
    let options = CatalogContentSearchOptions { mode: CatalogContentSearchMode::Expression,
        scope, ..counted() };
    let mut expression = CatalogContentSearchSession::open(owner(78102), &root, 1,
        "needle required -forbidden lang:rust", options, || false).unwrap();
    finish(&mut expression);
    assert_eq!(expression.matches_seen, 2);
    assert_eq!(expression.hits.len(), 2);
    assert_eq!(expression.unsupported, 1);
    assert!(!expression.complete());
    assert!(expression.catalog.aggregate.excluded > 0);
    assert!(expression.hits.iter().all(|hit| hit.path.as_str().unwrap() == "src/good.rs"));
    assert_eq!(expression.hits[0].file.encoding, Some(DetectedEncoding::Utf16Le));
    let witness = expression.hit(1, expression.revision, 0, || false).unwrap();
    assert!(witness.as_str().contains("\"original_hex\":\"6e006500650064006c006500\""));

    let scalar_root = fixture("lowercase");
    fs::write(scalar_root.join("text.txt"), utf16("\u{212a} K k", false)).unwrap();
    let mut options = counted(); options.scope = AtlasSearchScope::all().with_case_sensitive(false);
    let mut lowercase = CatalogContentSearchSession::open(owner(78103), &scalar_root, 1, "k", options, || false).unwrap();
    finish(&mut lowercase);
    assert!(lowercase.complete()); assert_eq!(lowercase.matches_seen, 3);
    assert_eq!(lowercase.matched_text, "\u{212a}Kk");
    assert_eq!(lowercase.witnesses, [0x21, 0x2a, 0, b'K', 0, b'k']);

    let token_root = fixture("tokens");
    fs::write(token_root.join("tokens.rs"), b"foo foobar foo_ Foo foo").unwrap();
    let options = CatalogContentSearchOptions { mode: CatalogContentSearchMode::Occurrences, ..counted() };
    let mut tokens = CatalogContentSearchSession::open(owner(78104), &token_root, 1, "foo", options, || false).unwrap();
    finish(&mut tokens);
    assert!(tokens.complete()); assert_eq!(tokens.matches_seen, 2);
    assert_eq!(tokens.hits[0].range.start().get(), 0);
    assert_eq!(tokens.hits[1].range.start().get(), 20);
}

#[test]
fn later_windows_skip_occurrences_without_retaining_their_paths_or_witnesses() {
    let root = fixture("later-window");
    fs::write(root.join("many.txt"), b"banana banana banana").unwrap();
    let mut options = counted(); options.start_after = 3; options.limits.max_matches = 2;
    let mut session = CatalogContentSearchSession::open(owner(78105), &root, 7, "ana", options, || false).unwrap();
    finish(&mut session);
    assert!(session.complete()); assert_eq!(session.matches_seen, 6);
    assert_eq!(session.hits.iter().map(|hit| hit.occurrence).collect::<Vec<_>>(), [4, 5]);
    assert_eq!(session.hits.iter().map(|hit| hit.range.start().get()).collect::<Vec<_>>(), [10, 15]);
    assert_eq!(session.witnesses, b"ana");
    let reply = session.info(|| false).unwrap();
    assert!(reply.as_str().contains("\"retained_witness_bytes\":\"3\""));
    assert!(reply.as_str().contains("\"skipped_matches\":\"3\""));
    assert!(reply.as_str().contains("\"page_has_more_matches\":true"));
    assert!(reply.as_str().contains("\"match_count_complete\":true"));
}

#[test]
fn a_full_retained_page_still_detects_an_additional_match_in_the_next_file() {
    let root = fixture("exact-row-cap");
    fs::write(root.join("a.txt"), b"needle").unwrap();
    fs::write(root.join("b.txt"), b"needle").unwrap();
    let mut options = CatalogContentSearchOptions::default(); options.limits.max_matches = 1;
    let mut session = CatalogContentSearchSession::open(owner(78106), &root, 1, "needle", options, || false).unwrap();
    finish(&mut session);
    assert_eq!(session.examined, 2);
    assert_eq!(session.hits.len(), 1); assert_eq!(session.matches_seen, 2);
    assert_eq!(session.stop_reason.unwrap().code(), "match-limit");
    assert!(!session.complete());
}

#[test]
fn stop_preserves_only_finished_files_and_does_not_promote_an_unfinished_expression() {
    for expression in [false, true] {
        let root = fixture("stop");
        fs::write(root.join("large.rs"), b"needle required ".repeat(16_384)).unwrap();
        let mut options = counted();
        if expression { options.mode = CatalogContentSearchMode::Expression; }
        let mut session = CatalogContentSearchSession::open(owner(78107 + u64::from(expression)), &root, 1,
            if expression { "needle required" } else { "needle" }, options, || false).unwrap();
        while session.active.is_none() { session.step(&mut || false).unwrap(); }
        session.step(&mut || false).unwrap();
        assert!(session.io.bytes_read > 0);
        assert!(session.hits.is_empty());
        let reply = session.stop(1, || false).unwrap();
        assert!(session.active.is_none()); assert!(!session.more());
        assert!(reply.as_str().contains("\"stop_reason\":\"user-stopped\""));
        assert!(reply.as_str().contains("\"retained_hits\":\"0\""));
        assert!(reply.as_str().contains("\"files_unexamined\":\"1\""));
        assert!(!session.complete());
    }
    let root = fixture("stop-candidate");
    fs::write(root.join("unopened.rs"), b"needle").unwrap();
    let mut session = CatalogContentSearchSession::open(owner(78109), &root, 1, "needle", counted(), || false).unwrap();
    session.step(&mut || false).unwrap();
    assert!(session.candidate.is_some());
    let reply = session.stop(1, || false).unwrap();
    assert!(session.candidate.is_none()); assert_eq!(session.io.bytes_read, 0);
    assert!(reply.as_str().contains("\"files_unexamined\":\"1\""));
}

#[test]
fn cancellation_before_work_can_retry_but_a_canceled_reader_cannot_publish() {
    let root = fixture("cancel");
    fs::write(root.join("large.txt"), b"needle ".repeat(10_000)).unwrap();
    let mut session = CatalogContentSearchSession::open(owner(78110), &root, 1, "needle", counted(), || false).unwrap();
    assert!(matches!(session.advance(1, 1, 1, || true), Err(CatalogContentSearchError::Canceled)));
    assert_eq!(session.revision, 1); assert_eq!(session.io.bytes_read, 0);
    while session.active.is_none() { session.step(&mut || false).unwrap(); }
    let mut polls = 0;
    assert!(matches!(session.advance(1, 1, 1, || { polls += 1; polls >= 3 }), Err(CatalogContentSearchError::Canceled)));
    assert!(session.canceled);
    assert!(matches!(session.info(|| false), Err(CatalogContentSearchError::Canceled)));
    assert!(session.hits.is_empty());
}

#[test]
fn source_admission_denial_retains_the_unread_candidate_and_output_retry_is_idempotent() {
    let root = fixture("admission");
    fs::write(root.join("only.txt"), b"needle").unwrap();
    let mut session = CatalogContentSearchSession::open(owner(78111), &root, 1, "needle", counted(), || false).unwrap();
    session.step(&mut || false).unwrap();
    assert!(session.candidate.is_some());
    let id = session.catalog.next_id().unwrap();
    let remaining = crate::MANAGED_BYTES - session.catalog.budget.accounting().reserved().get();
    let pressure = session.catalog.budget.try_reserve_managed(session.owner(), id, ByteLength::new(remaining)).unwrap();
    assert!(session.step(&mut || false).is_err());
    assert!(session.candidate.is_some()); assert_eq!(session.next_entry, 1); assert_eq!(session.examined, 0);
    drop(pressure);
    finish(&mut session);
    assert!(session.complete()); assert_eq!(session.matches_seen, 1);

    let second_root = fixture("lost-output");
    for index in 0..600 { fs::write(second_root.join(format!("excluded-{index}.txt")), b"needle").unwrap(); }
    let options = CatalogContentSearchOptions { scope: AtlasSearchScope::from_csv(Some("rs"), None).unwrap(), ..counted() };
    let mut session = CatalogContentSearchSession::open(owner(78112), &second_root, 1, "needle", options, || false).unwrap();
    let id = session.catalog.next_id().unwrap();
    let remaining = crate::MANAGED_BYTES - session.catalog.budget.accounting().reserved().get();
    let pressure = session.catalog.budget.try_reserve_managed(session.owner(), id, ByteLength::new(remaining)).unwrap();
    assert!(session.advance(1, 1, 16, || false).is_err());
    assert_eq!(session.revision, 2); assert_eq!(session.excluded, 16);
    drop(pressure);
    let replay = session.advance(1, 1, 16, || false).unwrap();
    assert_eq!(session.excluded, 16); assert_eq!(session.io.bytes_read, 0);
    assert!(replay.as_str().contains("\"revision\":\"2\""));
    finish(&mut session); assert!(session.complete()); assert_eq!(session.excluded, 600);
}

#[test]
fn a_byte_budget_never_qualifies_an_incomplete_expression_and_exclusions_spend_no_payload() {
    let root = fixture("byte-cap");
    fs::write(root.join("large.rs"), b"needle required more source").unwrap();
    let mut options = counted(); options.mode = CatalogContentSearchMode::Expression; options.limits.max_scan_bytes = 6;
    let mut session = CatalogContentSearchSession::open(owner(78113), &root, 1, "needle required", options, || false).unwrap();
    finish(&mut session);
    assert_eq!(session.io.bytes_read, 6); assert_eq!(session.matches_seen, 0); assert!(session.hits.is_empty());
    assert!(!session.complete()); assert_eq!(session.stop_reason.unwrap().code(), "byte-limit");

    let second_root = fixture("excluded-after-cap");
    for index in 0..600 { fs::write(second_root.join(format!("file-{index}.txt")), b"do not read").unwrap(); }
    let mut session = CatalogContentSearchSession::open(owner(78114), &second_root, 1, "needle", counted(), || false).unwrap();
    // Name the exact first discovered file as the only admitted path. Its source
    // exhausts the budget, but later excluded metadata must finish the walk.
    let first = session.catalog.pages.back().unwrap().files[0].path.as_str().unwrap().to_string();
    fs::write(second_root.join(&first), b"needle").unwrap();
    session.options.scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, &first).unwrap();
    session.options.limits.max_scan_bytes = 6;
    finish(&mut session);
    assert!(session.complete()); assert_eq!(session.matches_seen, 1);
    assert_eq!(session.io.bytes_read, 6); assert_eq!(session.excluded, 599);
}


#[test]
fn lowercase_identifier_discovery_retains_case_variants_in_later_windows() {
    for little in [true, false] {
        let root = fixture("lowercase-occurrences");
        fs::write(root.join("tokens.rs"), utf16("K k K Ko _k k_ 1K // K", little)).unwrap();
        let mut options = counted();
        options.mode = CatalogContentSearchMode::Occurrences;
        options.scope = AtlasSearchScope::all().with_case_sensitive(false);
        options.start_after = 1;
        options.limits.max_matches = 2;
        let mut session = CatalogContentSearchSession::open(owner(if little { 78130 } else { 78131 }),
            &root, 9, "k", options, || false).unwrap();
        finish(&mut session);
        assert!(session.complete());
        assert_eq!(session.matches_seen, 4);
        assert_eq!(session.hits.len(), 2);
        assert_eq!(session.matched_text, "kK");
        assert_eq!(session.hits.iter().map(|hit| hit.occurrence).collect::<Vec<_>>(), [2, 3]);
        let mut expected = utf16("kK", little);
        expected.drain(..2);
        assert_eq!(session.witnesses, expected);
        let before = session.io;
        fs::write(root.join("tokens.rs"), b"replaced").unwrap();
        for position in 0..2 {
            let reply = session.hit(9, session.revision, position, || false).unwrap();
            assert!(reply.as_str().contains("\"mode\":\"whole-token-occurrences\""));
            assert!(reply.as_str().contains("\"case_sensitive\":false"));
            assert!(reply.as_str().contains("\"additional_source_bytes_read\":\"0\""));
        }
        assert_eq!(session.io, before);
    }
}

#[test]
fn focused_discovery_ignores_unrelated_siblings_and_applies_ancestor_rules() {
    let root = fixture("direct-search");
    fs::create_dir_all(root.join("src/nested")).unwrap();
    fs::create_dir(root.join("src-other")).unwrap();
    fs::write(root.join(".gitignore"), b"src/nested/secret.rs\n").unwrap();
    fs::write(root.join("src-other/.gitignore"), vec![0xff; 128]).unwrap();
    fs::write(root.join("src-other/also.rs"), b"needle").unwrap();
    fs::write(root.join("src/nested/file.rs"), b"needle").unwrap();
    fs::write(root.join("src/nested/secret.rs"), b"needle").unwrap();

    for (i, (kind, path, policy, expected)) in [
        (AtlasSearchPathKind::Directory, "src/nested", CatalogDiscoveryPolicy::RepositoryRules, 1),
        (AtlasSearchPathKind::File, "src/nested/file.rs", CatalogDiscoveryPolicy::RepositoryRules, 1),
        (AtlasSearchPathKind::File, "src/nested/secret.rs", CatalogDiscoveryPolicy::RepositoryRules, 0),
        (AtlasSearchPathKind::Directory, "src/nested", CatalogDiscoveryPolicy::IncludeExcluded, 2),
        (AtlasSearchPathKind::File, "src/nested/secret.rs", CatalogDiscoveryPolicy::IncludeExcluded, 1),
    ].into_iter().enumerate() {
        let scope = AtlasSearchScope::all().with_path(kind, path).unwrap();
        let mut session = CatalogContentSearchSession::open_with_policy(owner(78200 + i as u64), &root,
            1, "needle", CatalogContentSearchOptions { scope, ..counted() }, policy, || false).unwrap();
        finish(&mut session);
        assert!(session.complete());
        assert_eq!(session.catalog.catalogued_files, expected);
        assert_eq!(session.matches_seen, expected);
        assert_eq!(session.io.bytes_read, 6 * expected);
        let receipt = session.info(|| false).unwrap();
        assert!(receipt.as_str().contains(&format!("\"discovery_scope\":\"{}\"", kind.name())));
    }
    let scope = AtlasSearchScope::all().with_path(AtlasSearchPathKind::File, "missing.rs").unwrap();
    assert!(CatalogContentSearchSession::open(owner(78206), &root, 1, "needle",
        CatalogContentSearchOptions { scope, ..counted() }, || false).is_err());
}
