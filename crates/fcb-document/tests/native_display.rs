#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_document::{DisplayItem, NativeVectorShape};
use fcb_document::native_display::{NativeDocumentDisplay, NativeDisplayError,
    MAX_DISPLAY_PAGE_GLYPHS, MAX_DISPLAY_PAGE_ITEMS};
use fcb_document::reader::{DocumentReader, DocumentReadOptions};
use franken_markdown::text::FontId;

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(813).unwrap() }
fn allocation(n: u64) -> ResourceAllocationId { ResourceAllocationId::new(n).unwrap() }
fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(256 * 1024 * 1024)).unwrap() }
fn capture(bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), 1).unwrap(),
        SourceRevision::new(owner(), 7).unwrap()).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn read<'a>(source: &'a CompleteCapture, budget: &ResourceBudget) -> DocumentReader<'a> {
    DocumentReader::prepare(source, DocumentId::new(owner(), 1).unwrap(),
        DocumentGeneration::new(owner(), 1).unwrap(), DocumentReadOptions::default(),
        budget, allocation(1), || false).unwrap()
}
fn display(reader: &DocumentReader<'_>, budget: &ResourceBudget, width: u32) -> NativeDocumentDisplay {
    NativeDocumentDisplay::prepare(reader, width, budget, [allocation(2), allocation(3)], || false).unwrap()
}

#[test]
fn native_packets_preserve_upstream_faces_styles_vectors_and_inert_images() {
    let source = capture(b"# Heading\n\nPlain **bold** and *italic* with `code`.\n\n> Quote.\n\n- [x] Done\n\n| Key | Value |\n| --- | --- |\n| a | b |\n\n---\n\n[Jump](#heading)\n\n![Diagram](https://invalid.example/never-fetch.png)\n");
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 640);
    assert!(display.glyph_count() > 0);
    assert!((4..=6).contains(&display.font_count()), "regular, bold, italic and code use actual distinct faces");
    let mut heading = false; let mut link = false; let mut table = false; let mut checked = false;
    for item in display.display().items() {
        match item {
            DisplayItem::Text(text) => {
                let run = text.font_run.as_ref().unwrap();
                assert_eq!(run.logical_text, text.text);
                for glyph in &run.glyphs {
                    assert_ne!(glyph.glyph_id, 0, "missing glyphs are not successful native output");
                    let index = display.font_index(glyph.font_id).unwrap();
                    let bytes = display.font_bytes(index).unwrap();
                    assert_eq!(FontId::from_font_data(bytes), glyph.font_id);
                }
            }
            DisplayItem::Anchor(anchor) => {
                heading |= anchor.is_heading && anchor.anchor_id == "heading";
                link |= !anchor.is_heading && anchor.anchor_id == "#heading";
            }
            DisplayItem::Vector(vector) => {
                table |= vector.shape == NativeVectorShape::TableBorder;
                checked |= vector.shape == NativeVectorShape::CheckboxCheck;
            }
            DisplayItem::Image(image) => {
                assert!(!image.is_resolved);
                assert_eq!(image.destination, "https://invalid.example/never-fetch.png");
            }
            DisplayItem::Clip(_) => panic!("clip state has no native wire contract"),
        }
    }
    assert!(heading && link && table && checked);
    assert_eq!(display.unresolved_images(), 1);
    assert_eq!(reader.capture().bytes(), source.bytes());
    for index in 0..display.font_count() {
        let bytes = display.font_bytes(index).unwrap();
        assert_eq!(display.font_chunk(index, 0, 17).unwrap(), &bytes[..17]);
        assert!(display.font_chunk(index, bytes.len(), 17).unwrap().is_empty());
        assert_eq!(display.font_chunk(index, bytes.len() + 1, 17).err(), Some(NativeDisplayError::InvalidRange));
    }
}

#[test]
fn source_regions_are_verified_enclosing_blocks_in_the_original_bom_capture() {
    let source = capture("\u{feff}# Café\n\nA **bold** word.\n".as_bytes());
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 320);
    assert!(display.source_ranges_qualified());
    for (index, item) in display.display().items().iter().enumerate() {
        let range = display.original_range(index).unwrap().unwrap();
        let (start, end) = range.as_usize_bounds().unwrap();
        assert!(start >= 3);
        let original = std::str::from_utf8(&source.bytes()[start..end]).unwrap();
        match item {
            DisplayItem::Anchor(anchor) if anchor.is_heading => assert!(original.starts_with("# Café")),
            DisplayItem::Text(text) if text.color_role == "heading" => assert!(original.starts_with("# Café")),
            _ => assert!(original.starts_with("A **bold** word.")),
        }
    }
    assert_eq!(display.original_range(display.display().items().len()).err(), Some(NativeDisplayError::InvalidRange));
}

#[test]
fn unqualified_frontmatter_disables_source_actions_without_discarding_rich_reading() {
    let source = capture(b"---\ntitle: metadata\n---\n\n# Target\n\nVisible body.\n");
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 640);
    assert!(!display.source_ranges_qualified());
    assert!(display.glyph_count() > 0);
    for index in 0..display.display().items().len() { assert_eq!(display.original_range(index).unwrap(), None); }
    assert_eq!(display.first_item_at_source(0, || false).err(), Some(NativeDisplayError::InvalidOutput));
}

#[test]
fn original_source_anchors_restore_rich_blocks_without_guessing_unmapped_positions() {
    let text = "\u{feff}\n\n# Café\n\nFirst paragraph.\n\n## Later\n\nSecond paragraph.\n";
    let source = capture(text.as_bytes());
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 320);
    assert!(display.source_ranges_qualified());
    let offset = text.find("Second").unwrap() as u64;
    let index = display.first_item_at_source(offset, || false).unwrap();
    assert!(index > 0);
    let range = display.original_range(index).unwrap().unwrap();
    assert!(range.start().get() <= offset && offset < range.end().get());
    assert!(matches!(&display.display().items()[index], DisplayItem::Text(run) if run.text.contains("Second")));
    for previous in 0..index {
        if let Some(range) = display.original_range(previous).unwrap() {
            assert!(offset < range.start().get() || offset >= range.end().get());
        }
    }
    assert_eq!(display.first_item_at_source(source.bytes().len() as u64, || false).unwrap(),
        display.display().items().len());
    for offset in [0, 3, (text.find('é').unwrap() + 1) as u64, text.len() as u64 + 1, u64::MAX] {
        assert_eq!(display.first_item_at_source(offset, || false).err(), Some(NativeDisplayError::InvalidRange));
    }
    assert_eq!(display.first_item_at_source(offset, || true).err(), Some(NativeDisplayError::Canceled));
}

#[test]
fn page_glyph_limits_preserve_whole_runs_and_progress_to_exact_eof() {
    let source = capture((format!("{}\n\n", "a".repeat(300)).repeat(80)).as_bytes());
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 4096);
    let total = display.display().items().len();
    let first_end = display.page_end(0, MAX_DISPLAY_PAGE_ITEMS).unwrap();
    assert!(first_end < total.min(MAX_DISPLAY_PAGE_ITEMS), "glyph budget must cut a large page before item budget");
    let mut first = 0; let mut glyphs = 0;
    while first < total {
        let end = display.page_end(first, MAX_DISPLAY_PAGE_ITEMS).unwrap();
        assert!(end > first && end - first <= MAX_DISPLAY_PAGE_ITEMS);
        let page_glyphs: usize = display.display().items()[first..end].iter().map(|item| match item {
            DisplayItem::Text(text) => text.font_run.as_ref().unwrap().glyphs.len(), _ => 0,
        }).sum();
        assert!(page_glyphs <= MAX_DISPLAY_PAGE_GLYPHS);
        glyphs += page_glyphs; first = end;
    }
    assert_eq!(glyphs, display.glyph_count());
    assert_eq!(display.page_end(total, 1).unwrap(), total);
    assert_eq!(display.page_end(total + 1, 1).err(), Some(NativeDisplayError::InvalidRange));
    assert_eq!(display.page_end(0, 0).err(), Some(NativeDisplayError::InvalidLimits));
}

#[test]
fn retained_capture_and_generation_are_required_for_all_display_delivery() {
    let source = capture(b"# Original\n"); let same_ids = capture(source.bytes());
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 640);
    display.validate_delivery(&source, 1).unwrap();
    assert_eq!(display.validate_delivery(&same_ids, 1), Err(NativeDisplayError::StaleCapture));
    assert_eq!(display.validate_delivery(&source, 2), Err(NativeDisplayError::StaleGeneration));
    assert_eq!(display.font_chunk(0, 0, 65537).err(), Some(NativeDisplayError::InvalidLimits));
    assert_eq!(display.font_bytes(display.font_count()).err(), Some(NativeDisplayError::InvalidRange));
}

#[test]
fn unsupported_shaping_cancellation_and_admission_preserve_the_logical_reader() {
    let source = capture("# Read 😀\n\nLogical preview survives.\n".as_bytes());
    let budget = budget(); let reader = read(&source, &budget);
    assert_eq!(NativeDocumentDisplay::prepare(&reader, 640, &budget, [allocation(2), allocation(3)], || false).err(),
        Some(NativeDisplayError::ShapingUnsupported));
    assert_eq!(NativeDocumentDisplay::prepare(&reader, 640, &budget, [allocation(4), allocation(5)], || true).err(),
        Some(NativeDisplayError::Canceled));
    assert_eq!(NativeDocumentDisplay::prepare(&reader, 159, &budget, [allocation(6), allocation(7)], || false).err(),
        Some(NativeDisplayError::InvalidLimits));
    let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
    assert_eq!(NativeDocumentDisplay::prepare(&reader, 640, &tiny, [allocation(2), allocation(3)], || false).err(),
        Some(NativeDisplayError::ResourceDenied));
    assert!(reader.rendered_text().contains("Logical preview survives."));
    assert!(reader.window(0, 64).unwrap().whole_document_visible());
}

#[test]
fn image_occurrences_keep_upstream_identity_destination_and_original_bom_region() {
    let source = capture("\u{feff}# Images\n\n![First](assets/a%20b.png)\n\n![Second](../outside.jpg)\n".as_bytes());
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 640);
    let images: Vec<_> = display.display().items().iter().enumerate().filter_map(|(index, item)| match item {
        DisplayItem::Image(image) => Some((index, image.request_id, image.destination.as_str())),
        _ => None,
    }).collect();
    assert_eq!(images.len(), 2);
    for (position, (index, id, destination)) in images.iter().copied().enumerate() {
        let image = display.image_request(index, id).unwrap();
        assert_eq!(image.item_index, index);
        assert_eq!(image.request_id, id);
        assert_eq!(image.destination, destination, "the adapter must not reinterpret URL bytes");
        assert_eq!(image.alt_text, if position == 0 { "First" } else { "Second" });
        let (start, end) = image.enclosing_original_range.as_usize_bounds().unwrap();
        assert!(start >= 3 && start < end);
        assert!(std::str::from_utf8(&source.bytes()[start..end]).unwrap().contains(destination));
        assert_eq!(display.image_request(index, id.wrapping_add(1)).err(), Some(NativeDisplayError::InvalidRange));
    }
    let text = display.display().items().iter().position(|item| matches!(item, DisplayItem::Text(_))).unwrap();
    assert_eq!(display.image_request(text, images[0].1).err(), Some(NativeDisplayError::InvalidRange));
    assert_eq!(display.image_request(display.display().items().len(), 0).err(), Some(NativeDisplayError::InvalidRange));
}

#[test]
fn unqualified_display_images_keep_fallbacks_but_cannot_authorize_asset_acquisition() {
    let source = capture(b"---\ntitle: metadata\n---\n\n![Image](images/local.png)\n");
    let budget = budget(); let reader = read(&source, &budget); let display = display(&reader, &budget, 640);
    assert!(!display.source_ranges_qualified());
    let (index, id) = display.display().items().iter().enumerate().find_map(|(index, item)| match item {
        DisplayItem::Image(image) => Some((index, image.request_id)), _ => None,
    }).unwrap();
    assert_eq!(display.unresolved_images(), 1);
    assert_eq!(display.image_request(index, id).err(), Some(NativeDisplayError::InvalidOutput));
}
