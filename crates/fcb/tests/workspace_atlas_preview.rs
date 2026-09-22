#![forbid(unsafe_code)]
#![cfg(all(feature = "map", feature = "search", unix))]

//! Exact captured search -> bounded logical source context through public APIs.
//! No native font, renderer or physical-presentation qualification is implied.
use std::{fs, path::{Path, PathBuf}, sync::{Arc, atomic::{AtomicU64, Ordering}},
    time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteLength, FileId, Size2D, SourceRevision};
use fcb::map::{LayoutOptions, LayoutRevision, QueryGeneration, ResourceAllocationId, ResourceBudget, RootId};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasLimits};
use fcb::map::workspace::text_search::{AtlasTextLimits, WorkspaceTextSource};
use fcb::map::workspace::text_preview::{AtlasTextPreviewError, AtlasTextPreviewOptions};
use fcb::search::{CompleteCapture, IndexLimits, RawPath, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCaptures, WorkspaceCatalog, WorkspaceLimits, WorkspaceStage};
use fcb::source::CancelFlag;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(712).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn generation(n: u64) -> QueryGeneration { QueryGeneration::new(owner(), n).unwrap() }
fn root() -> PathBuf {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let path = std::env::temp_dir().join(format!("fcb-atlas-preview-{}-{now}-{}",
        std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::create_dir(&path).unwrap(); path
}
fn fixture(bytes: &[u8]) -> (PathBuf, ResourceBudget, WorkspaceCatalog) {
    let root = root(); fs::write(root.join("source.txt"), bytes).unwrap();
    let budget = ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap();
    let grant = RootGrant::new(RootId::new(owner(), 1).unwrap(), RawPath::from_path(&root));
    let mut catalog = WorkspaceCatalog::open(grant, SearchManifestId::new(owner(), 1).unwrap(),
        FileId::new(owner(), 100).unwrap(), WorkspaceLimits::default(), false, &budget, allocation(1)).unwrap();
    for _ in 0..4097 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&CancelFlag::new()).unwrap();
    }
    assert_eq!(catalog.stage(), WorkspaceStage::Ready);
    assert!(catalog.discovery_complete());
    (root, budget, catalog)
}
fn capture<'a>(catalog: &'a WorkspaceCatalog, root: &Path, budget: &ResourceBudget, id: u64) -> WorkspaceCaptures<'a> {
    let mut captures = WorkspaceCaptures::new(catalog, SourceRevision::new(owner(), 500).unwrap(), budget, allocation(id)).unwrap();
    captures.step(&CancelFlag::new(), |request, path, allowance| {
        let bytes = fs::read(root.join(path.raw().to_path_buf())).unwrap();
        assert!(bytes.len() <= allowance);
        CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
    }).unwrap();
    assert!(captures.finished()); captures
}
fn atlas<'a>(catalog: &'a WorkspaceCatalog, budget: &ResourceBudget) -> WorkspaceAtlas<'a> {
    WorkspaceAtlas::build(catalog, &fcb::map::workspace::AtlasScope::All, LayoutRevision::new(owner(), 1).unwrap(), Size2D::new(1024.0, 768.0).unwrap(),
        LayoutOptions::modest(), WorkspaceAtlasLimits::default(), budget, allocation(20), || false).unwrap()
}
fn utf16(text: &str, little: bool) -> Vec<u8> {
    let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
    for unit in text.encode_utf16() {
        let encoded = if little { unit.to_le_bytes() } else { unit.to_be_bytes() };
        bytes.extend_from_slice(&encoded);
    }
    bytes
}

#[test]
fn utf8_bom_and_multibyte_context_preserve_exact_selected_bytes() {
    let logical = "αβ\r\nneedle😀tail\nlast";
    let mut original = vec![0xef, 0xbb, 0xbf]; original.extend_from_slice(logical.as_bytes());
    let (root, budget, catalog) = fixture(&original);
    let captures = capture(&catalog, &root, &budget, 2); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget,
        [allocation(5), allocation(6), allocation(7)], || false).unwrap();
    let preview = overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions::default(),
        &budget, [allocation(8), allocation(9)], || false).unwrap();
    assert_eq!(preview.text().text(), logical);
    assert_eq!(preview.text().first_line_number(), Some(1));
    assert_eq!(preview.hit().original_range().start().get(), 9);
    assert_eq!(preview.selected_text_range().start().get(), 6);
    assert_eq!(preview.selected_text_range().end().get(), 12);
    let selected = preview.text().text_selection(preview.selected_text_range()).unwrap();
    assert_eq!(selected.text, "needle"); assert_eq!(selected.original_bytes, b"needle");
    assert_eq!(selected.original_range, preview.hit().original_range());
    assert!(!preview.text().has_replacements());
    preview.validate_delivery(&overlay, &source, generation(1)).unwrap();
}

#[test]
fn utf16_endianness_surrogates_and_in_content_bom_survive_every_small_context_cut() {
    let logical = "head😀prefix \u{feff}needle 😀suffix\r\nlast";
    for little in [true, false] {
        let original = utf16(logical, little);
        let (root, budget, catalog) = fixture(&original);
        let captures = capture(&catalog, &root, &budget, 2); let atlas = atlas(&catalog, &budget);
        let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
        let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
        let overlay = index.search("\u{feff}needle", generation(1), AtlasTextLimits::default(), &budget,
            [allocation(5), allocation(6), allocation(7)], || false).unwrap();
        assert!(overlay.is_complete()); assert_eq!(overlay.hits().len(), 1);
        for context_bytes in 0..=12 {
            let preview = overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions { context_bytes },
                &budget, [allocation(8), allocation(9)], || false).unwrap();
            let selected = preview.text().text_selection(preview.selected_text_range()).unwrap();
            assert_eq!(selected.text, "\u{feff}needle");
            let range = preview.hit().original_range().as_usize_bounds().unwrap();
            assert_eq!(selected.original_bytes, &original[range.0..range.1]);
            assert!(!selected.contains_replacements);
            assert!(!preview.text().has_replacements(), "little={little} context={context_bytes}");
        }
    }
}

#[test]
fn a_match_ending_between_cr_and_lf_is_not_lost_when_context_is_zero() {
    for original in [b"head\r\ntail".to_vec(), utf16("head\r\ntail", true), utf16("head\r\ntail", false)] {
        let (root, budget, catalog) = fixture(&original);
        let captures = capture(&catalog, &root, &budget, 2); let atlas = atlas(&catalog, &budget);
        let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
        let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
        let overlay = index.search("\r", generation(1), AtlasTextLimits::default(), &budget,
            [allocation(5), allocation(6), allocation(7)], || false).unwrap();
        assert_eq!(overlay.hits().len(), 1);
        let preview = overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions { context_bytes: 0 },
            &budget, [allocation(8), allocation(9)], || false).unwrap();
        let selected = preview.text().text_selection(preview.selected_text_range()).unwrap();
        assert_eq!(selected.text, "\r"); assert_eq!(selected.original_range, overlay.hits()[0].original_range());
        assert!(preview.text().text().contains("\r\n"));
    }
}

#[test]
fn far_hit_preview_is_bounded_and_uses_old_source_after_live_replacement() {
    let mut original = vec![b'x'; 40 * 1024];
    original.extend_from_slice(b"needle and old retained context"); original.extend_from_slice(&[b'y'; 2048]);
    let (root, budget, catalog) = fixture(&original);
    let captures = capture(&catalog, &root, &budget, 2); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget,
        [allocation(5), allocation(6), allocation(7)], || false).unwrap();
    drop(index);
    fs::write(root.join("source.txt"), b"a completely different live file").unwrap();
    let preview = overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions { context_bytes: 32 },
        &budget, [allocation(8), allocation(9)], || false).unwrap();
    assert!(preview.text().text().contains("needle and old retained context"));
    assert!(!preview.text().text().contains("different live file"));
    assert!(preview.text().extent().bytes().len() <= 6 + 2 * 32 + 24);
    assert_eq!(preview.text().first_line_number(), None, "unread prefix is not a line-number oracle");
    assert!(preview.prefix_bytes_omitted()); assert!(preview.suffix_bytes_omitted());
    let (first, last) = preview.text().range().as_usize_bounds().unwrap();
    assert_eq!(preview.text().extent().range_bytes(preview.text().range()).unwrap(), &original[first..last]);
}

#[test]
fn stale_generations_missing_hits_and_revocation_cannot_activate_context() {
    let (root, budget, catalog) = fixture(b"needle needle");
    let captures = capture(&catalog, &root, &budget, 2); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget,
        [allocation(5), allocation(6), allocation(7)], || false).unwrap();
    for (position, active) in [(0, generation(2)), (2, generation(1))] {
        assert!(overlay.preview_hit(&source, position, active, AtlasTextPreviewOptions::default(),
            &budget, [allocation(8), allocation(9)], || false).is_err());
    }
    let preview = overlay.preview_hit(&source, 1, generation(1), AtlasTextPreviewOptions::default(),
        &budget, [allocation(8), allocation(9)], || false).unwrap();
    assert_eq!(preview.hit_index(), 1); assert_eq!(preview.hit().original_range().start().get(), 7);
    assert!(preview.validate_delivery(&overlay, &source, generation(2)).is_err());
    catalog.grant().revoke();
    assert!(preview.validate_delivery(&overlay, &source, generation(1)).is_err());
    assert!(overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions::default(),
        &budget, [allocation(10), allocation(11)], || false).is_err());
}

#[test]
fn canceled_or_denied_preview_releases_new_admission_without_harming_old_results() {
    let (root, budget, catalog) = fixture(b"before needle after");
    let captures = capture(&catalog, &root, &budget, 2); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget,
        [allocation(5), allocation(6), allocation(7)], || false).unwrap();
    let mut checks = 0;
    assert!(overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions::default(),
        &budget, [allocation(8), allocation(9)], || { checks += 1; checks >= 3 }).is_err());
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert!(overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions::default(),
        &tiny, [allocation(8), allocation(9)], || false).is_err());
    assert!(matches!(overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions { context_bytes: 16_385 },
        &budget, [allocation(8), allocation(9)], || false), Err(AtlasTextPreviewError::InvalidLimits)));
    assert!(matches!(overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions::default(),
        &budget, [allocation(8), allocation(8)], || false), Err(AtlasTextPreviewError::InvalidLimits)));
    let preview = overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions::default(),
        &budget, [allocation(8), allocation(9)], || false).unwrap();
    assert_eq!(preview.text().text(), "before needle after");
    assert_eq!(overlay.select_hit(&source, 0, generation(1)).unwrap().original_bytes(), b"needle");
}

#[test]
fn equal_numeric_capture_ids_cannot_rebind_a_prepared_preview() {
    let (root, budget, catalog) = fixture(b"before needle after");
    let captures = capture(&catalog, &root, &budget, 2); let atlas = atlas(&catalog, &budget);
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(3)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(4), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget,
        [allocation(5), allocation(6), allocation(7)], || false).unwrap();
    let preview = overlay.preview_hit(&source, 0, generation(1), AtlasTextPreviewOptions::default(),
        &budget, [allocation(8), allocation(9)], || false).unwrap();
    let other_captures = capture(&catalog, &root, &budget, 10);
    let other_source = WorkspaceTextSource::new(&atlas, &other_captures, &budget, allocation(11)).unwrap();
    let other_index = other_source.index(IndexLimits::default(), &budget, allocation(12), || false).unwrap();
    let other_overlay = other_index.search("needle", generation(1), AtlasTextLimits::default(), &budget,
        [allocation(13), allocation(14), allocation(15)], || false).unwrap();
    assert_eq!(overlay.hits(), other_overlay.hits());
    assert!(matches!(preview.validate_delivery(&other_overlay, &other_source, generation(1)),
        Err(AtlasTextPreviewError::WrongSelection)));
    preview.validate_delivery(&overlay, &source, generation(1)).unwrap();
}
