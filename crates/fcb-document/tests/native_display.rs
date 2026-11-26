#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_document::{DisplayItem, NativeVectorShape};
use fcb_document::native_display::{NativeDocumentDisplay, NativeDisplayError,
    MAX_DISPLAY_PAGE_GLYPHS, MAX_DISPLAY_PAGE_ITEMS, MAX_DISPLAY_SOURCE_BYTES};
use fcb_document::reader::{DocumentReadError, DocumentReader, DocumentReadOptions};
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

#[test]
fn utf16_styled_display_retains_exact_mapping_after_the_logical_reader_retires() {
    let text = "# Café\n\nA **bold** paragraph.\n\n![Image](picture.png)\n";
    for little in [true, false] {
        let bytes: Vec<u8> = "\u{feff}".encode_utf16().chain(text.encode_utf16())
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        let budget = budget();
        let display = {
            let source = capture(&bytes); let reader = read(&source, &budget);
            display(&reader, &budget, 640)
        };
        assert!(display.source_ranges_qualified());
        assert!(display.glyph_count() > 0);
        let paragraph = 2 + 2 * text[..text.find("A **bold**").unwrap()].encode_utf16().count();
        let item = display.first_item_at_source(paragraph as u64, || false).unwrap();
        assert_eq!(display.original_range(item).unwrap().unwrap().start().get(), paragraph as u64);
        let mut saw_image = false;
        for (index, entry) in display.display().items().iter().enumerate() {
            if let Some(range) = display.original_range(index).unwrap() {
                let (a, b) = range.as_usize_bounds().unwrap();
                assert!(a >= 2 && a % 2 == 0 && b % 2 == 0 && b <= bytes.len());
                let units: Vec<u16> = bytes[a..b].chunks_exact(2).map(|pair|
                    if little { u16::from_le_bytes([pair[0], pair[1]]) }
                    else { u16::from_be_bytes([pair[0], pair[1]]) }).collect();
                let original = String::from_utf16(&units).unwrap();
                assert!(text.contains(&original));
                if let DisplayItem::Image(image) = entry {
                    saw_image = true;
                    let request = display.image_request(index, image.request_id).unwrap();
                    assert_eq!(request.enclosing_original_range, range);
                    assert_eq!(request.destination, "picture.png");
                    assert!(original.contains("![Image](picture.png)"));
                }
            }
        }
        assert!(saw_image);
        for invalid in [0, 1, paragraph + 1, bytes.len() + 1] {
            assert_eq!(display.first_item_at_source(invalid as u64, || false).err(),
                Some(NativeDisplayError::InvalidRange));
        }
        assert_eq!(display.first_item_at_source(bytes.len() as u64, || false).unwrap(),
            display.display().items().len());
    }
}

#[test]
fn expanded_preview_source_limit_also_applies_to_decoded_utf16_input() {
    // Logical and Styled now share the 512 KiB input ceiling. UTF-16 still
    // cannot hide a wider decoded parser input behind fewer original bytes.
    let bytes: Vec<u8> = "\u{feff}".encode_utf16().chain("界".repeat(175_000).encode_utf16())
        .flat_map(u16::to_le_bytes).collect();
    assert!(bytes.len() < MAX_DISPLAY_SOURCE_BYTES);
    let source = capture(&bytes); let budget = budget();
    let before = budget.accounting();
    assert_eq!(DocumentReader::prepare(&source, DocumentId::new(owner(), 1).unwrap(),
        DocumentGeneration::new(owner(), 1).unwrap(),
        DocumentReadOptions { max_source_bytes: MAX_DISPLAY_SOURCE_BYTES, ..Default::default() },
        &budget, allocation(1), || false).err(), Some(DocumentReadError::SourceLimit));
    assert_eq!(budget.accounting().reserved(), before.reserved());
    assert_eq!(budget.accounting().active_allocations(), before.active_allocations());
}

fn large_styled_source(limit: usize) -> String {
    let mut text = String::from("# Large document\n\n");
    let paragraph = "Ordinary text in a retained document remains readable with exact source navigation.\n\n";
    let tail = "\n\n## Final target\n\nA **bold** final paragraph.\n";
    while text.len() + paragraph.len() + tail.len() <= limit {
        text.push_str(paragraph);
    }
    text.push_str(&"x".repeat(limit - text.len() - tail.len()));
    text.push_str(tail);
    text
}

#[test]
fn ordinary_128_kib_profile_preserves_qualified_tail_and_bounded_pages_under_256_mib() {
    let text = large_styled_source(128 * 1024);
    assert!(text.len() > 100 * 1024);
    assert_eq!(text.len(), 128 * 1024);
    let source = capture(text.as_bytes()); let budget = budget();
    let reader = DocumentReader::prepare(&source, DocumentId::new(owner(), 1).unwrap(),
        DocumentGeneration::new(owner(), 1).unwrap(),
        DocumentReadOptions { max_source_bytes: MAX_DISPLAY_SOURCE_BYTES, ..Default::default() },
        &budget, allocation(1), || false).unwrap();
    let baseline = budget.accounting();
    let display = display(&reader, &budget, 640);
    assert!(display.source_ranges_qualified());
    assert!(display.glyph_count() > 0 && display.font_count() >= 2);
    assert_eq!(budget.accounting().active_allocations(), baseline.active_allocations() + 1);
    assert!(budget.accounting().peak_reserved().get() <= 256 * 1024 * 1024);

    // Negative control: these full envelopes cannot coexist in the host's
    // real capacity. Successful preparation above requires the audit scratch
    // to retire before the display parser/shaper/output envelope is admitted.
    let accepted_charge = budget.accounting().reserved();
    assert_eq!(reader.qualify_heading_source_ranges(&budget, allocation(4), || false),
        Err(DocumentReadError::ResourceDenied));
    assert_eq!(budget.accounting().reserved(), accepted_charge);

    let tail = text.find("A **bold** final").unwrap() as u64;
    let item = display.first_item_at_source(tail, || false).unwrap();
    assert!(item > MAX_DISPLAY_PAGE_ITEMS);
    let range = display.original_range(item).unwrap().unwrap();
    assert_eq!(range.start().get(), tail);
    assert!(range.end().get() <= source.bytes().len() as u64);
    let (start, end) = range.as_usize_bounds().unwrap();
    assert!(source.bytes()[start..end].starts_with(b"A **bold** final"));
    let mut first = 0;
    let mut paged_glyphs = 0;
    while first < display.display().items().len() {
        let end = display.page_end(first, MAX_DISPLAY_PAGE_ITEMS).unwrap();
        assert!(end > first && end - first <= MAX_DISPLAY_PAGE_ITEMS);
        let glyphs: usize = display.display().items()[first..end].iter().map(|item| match item {
            DisplayItem::Text(text) => text.font_run.as_ref().unwrap().glyphs.len(), _ => 0,
        }).sum();
        assert!(glyphs <= MAX_DISPLAY_PAGE_GLYPHS);
        paged_glyphs += glyphs;
        first = end;
    }
    assert_eq!(paged_glyphs, display.glyph_count());
    assert_eq!(display.first_item_at_source(source.bytes().len() as u64, || false).unwrap(), first);
    assert_eq!(display.page_end(first, 1).unwrap(), first);
    display.validate_delivery(&source, 1).unwrap();
    drop(display);
    assert_eq!(budget.accounting().reserved(), baseline.reserved());
    assert_eq!(budget.accounting().active_allocations(), baseline.active_allocations());
}

#[test]
fn larger_styled_profile_requires_its_output_reservation_and_keeps_the_logical_reader() {
    let text = large_styled_source(160 * 1024);
    let source = capture(text.as_bytes()); let budget = budget();
    let reader = DocumentReader::prepare(&source, DocumentId::new(owner(), 1).unwrap(),
        DocumentGeneration::new(owner(), 1).unwrap(),
        DocumentReadOptions { max_source_bytes: MAX_DISPLAY_SOURCE_BYTES, ..Default::default() },
        &budget, allocation(1), || false).unwrap();
    let before = budget.accounting();
    let mut saw_audit = false;
    let mut audit_retired = false;
    assert_eq!(NativeDocumentDisplay::prepare(&reader, 640, &budget,
        [allocation(2), allocation(3)], || {
            let active = budget.accounting().active_allocations();
            if active == before.active_allocations() + 2 { saw_audit = true; }
            if saw_audit && active == before.active_allocations() + 1 { audit_retired = true; }
            false
        }).err(), Some(NativeDisplayError::ResourceDenied));
    assert!(saw_audit && audit_retired,
        "the larger output profile must be admitted after its provenance audit, before shaping");
    assert_eq!(budget.accounting().reserved(), before.reserved());
    assert_eq!(budget.accounting().active_allocations(), before.active_allocations());
    assert!(reader.rendered_text().contains("final paragraph."));
    assert_eq!(reader.capture().bytes(), text.as_bytes());
}

#[test]
fn cancellation_after_provenance_audit_retires_the_unpublished_display_envelope() {
    let source = capture(b"# Stable\n\nA retained paragraph.\n");
    let budget = budget(); let reader = read(&source, &budget);
    let before = budget.accounting();
    let mut saw_audit = false;
    let mut stopped_after_audit = false;
    let result = NativeDocumentDisplay::prepare(&reader, 640, &budget,
        [allocation(2), allocation(3)], || {
            let active = budget.accounting().active_allocations();
            if active == before.active_allocations() + 2 { saw_audit = true; }
            if saw_audit && active == before.active_allocations() + 1 {
                stopped_after_audit = true;
                return true;
            }
            false
        });
    assert!(saw_audit && stopped_after_audit, "cancel between audit retirement and display admission");
    assert_eq!(result.err(), Some(NativeDisplayError::Canceled));
    assert_eq!(budget.accounting().reserved(), before.reserved());
    assert_eq!(budget.accounting().active_allocations(), before.active_allocations());
    assert!(reader.rendered_text().contains("retained paragraph"));
    assert!(display(&reader, &budget, 640).source_ranges_qualified(),
        "the canceled attempt must release both allocation identities");
}

#[test]
fn denied_display_growth_preserves_prior_capture_geometry_fonts_and_resource_charges() {
    let source = capture(b"# Stable\n\nA retained paragraph.\n");
    let budget = budget(); let reader = read(&source, &budget);
    let accepted = display(&reader, &budget, 640);
    let accepted_items = accepted.display().items().len();
    let accepted_glyphs = accepted.glyph_count();
    let accepted_font = accepted.font_chunk(0, 0, 32).unwrap().to_vec();
    let source_offset = source.bytes().windows(b"A retained".len())
        .position(|bytes| bytes == b"A retained").unwrap() as u64;
    let accepted_item = accepted.first_item_at_source(source_offset, || false).unwrap();
    let accepted_range = accepted.original_range(accepted_item).unwrap();

    // Forty MiB admits the retained envelope and this small provenance audit,
    // but cannot admit the full display parser/glyph/output envelope.
    let available = budget.accounting().available().get();
    assert!(available > 40 * 1024 * 1024);
    let pressure = budget.try_reserve_managed(owner(), allocation(8),
        ByteLength::new(available - 40 * 1024 * 1024)).unwrap();
    let before = budget.accounting();
    let mut saw_audit = false;
    let mut audit_retired = false;
    let result = NativeDocumentDisplay::prepare(&reader, 320, &budget,
        [allocation(4), allocation(5)], || {
            let active = budget.accounting().active_allocations();
            if active == before.active_allocations() + 2 { saw_audit = true; }
            if saw_audit && active == before.active_allocations() + 1 { audit_retired = true; }
            false
        });
    assert!(saw_audit && audit_retired, "refusal must occur on growth after a successful audit");
    assert_eq!(result.err(), Some(NativeDisplayError::ResourceDenied));
    assert_eq!(budget.accounting().reserved(), before.reserved());
    assert_eq!(budget.accounting().active_allocations(), before.active_allocations());
    accepted.validate_delivery(&source, 1).unwrap();
    assert_eq!(accepted.width_points(), 640);
    assert_eq!(accepted.display().items().len(), accepted_items);
    assert_eq!(accepted.glyph_count(), accepted_glyphs);
    assert_eq!(accepted.font_chunk(0, 0, 32).unwrap(), accepted_font.as_slice());
    assert_eq!(accepted.original_range(accepted_item).unwrap(), accepted_range);
    assert_eq!(accepted.first_item_at_source(source_offset, || false).unwrap(), accepted_item);
    assert!(reader.rendered_text().contains("retained paragraph"));
    drop(pressure);
    assert!(NativeDocumentDisplay::prepare(&reader, 320, &budget,
        [allocation(4), allocation(5)], || false).is_ok(),
        "a denied growth must release both candidate allocation identities");
}


#[test]
fn exact_512_kib_styled_profile_keeps_qualified_tail_and_bounded_complete_pages() {
    let text = large_styled_source(MAX_DISPLAY_SOURCE_BYTES);
    let source = capture(text.as_bytes());
    let budget = ResourceBudget::new(owner(), ByteLength::new(768 * 1024 * 1024)).unwrap();
    let reader = DocumentReader::prepare(&source, DocumentId::new(owner(), 1).unwrap(),
        DocumentGeneration::new(owner(), 1).unwrap(),
        DocumentReadOptions { max_source_bytes: MAX_DISPLAY_SOURCE_BYTES,
            max_flow_lines: 16384, max_flow_items: 16384, max_blocks: 8192, ..Default::default() },
        &budget, allocation(1), || false).unwrap();
    let baseline = budget.accounting();
    let display = display(&reader, &budget, 640);
    assert!(display.source_ranges_qualified());
    assert!(display.glyph_count() > 131_072, "the entire large document must survive the old glyph ceiling");
    assert!(display.glyph_count() <= fcb_document::native_display::MAX_DISPLAY_GLYPHS);
    assert!(budget.accounting().peak_reserved().get() <= 768 * 1024 * 1024);
    let tail = text.find("A **bold** final").unwrap() as u64;
    let tail_item = display.first_item_at_source(tail, || false).unwrap();
    assert_eq!(display.original_range(tail_item).unwrap().unwrap().start().get(), tail);
    let mut first = 0;
    let mut glyphs = 0;
    while first < display.display().items().len() {
        let end = display.page_end(first, MAX_DISPLAY_PAGE_ITEMS).unwrap();
        assert!(end > first && end - first <= MAX_DISPLAY_PAGE_ITEMS);
        let page: usize = display.display().items()[first..end].iter().map(|item| match item {
            DisplayItem::Text(text) => text.font_run.as_ref().unwrap().glyphs.len(),
            _ => 0,
        }).sum();
        assert!(page <= MAX_DISPLAY_PAGE_GLYPHS);
        glyphs += page;
        first = end;
    }
    assert_eq!(glyphs, display.glyph_count());
    assert_eq!(display.first_item_at_source(text.len() as u64, || false).unwrap(), first);
    drop(display);
    assert_eq!(budget.accounting().reserved(), baseline.reserved());
    assert_eq!(budget.accounting().active_allocations(), baseline.active_allocations());
}

#[test]
fn large_utf16_styled_source_preserves_original_bom_and_tail_offsets() {
    let text = large_styled_source(160 * 1024);
    for little in [true, false] {
        let bytes: Vec<u8> = "\u{feff}".encode_utf16().chain(text.encode_utf16())
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect();
        assert!(bytes.len() > 256 * 1024 && bytes.len() < MAX_DISPLAY_SOURCE_BYTES);
        let source = capture(&bytes);
        let budget = ResourceBudget::new(owner(), ByteLength::new(768 * 1024 * 1024)).unwrap();
        let reader = DocumentReader::prepare(&source, DocumentId::new(owner(), 1).unwrap(),
            DocumentGeneration::new(owner(), 1).unwrap(),
            DocumentReadOptions { max_source_bytes: MAX_DISPLAY_SOURCE_BYTES, ..Default::default() },
            &budget, allocation(1), || false).unwrap();
        let display = display(&reader, &budget, 640);
        assert!(display.source_ranges_qualified());
        let tail = 2 + 2 * text[..text.find("A **bold** final").unwrap()].encode_utf16().count();
        let item = display.first_item_at_source(tail as u64, || false).unwrap();
        let range = display.original_range(item).unwrap().unwrap();
        assert_eq!(range.start().get(), tail as u64);
        assert_eq!(range.end().get() % 2, 0);
        assert!(range.end().get() <= bytes.len() as u64);
        assert_eq!(display.first_item_at_source(tail as u64 + 1, || false).err(),
            Some(NativeDisplayError::InvalidRange));
        assert_eq!(reader.capture().bytes(), bytes);
        assert!(budget.accounting().peak_reserved().get() <= 768 * 1024 * 1024);
    }
}
