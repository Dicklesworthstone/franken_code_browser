#![deny(unsafe_op_in_unsafe_fn)]

use super::*;
use std::ffi::{CStr, CString};

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let text = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; text
}

#[test]
fn all_document_entrypoint_types_and_public_header_names_agree() {
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_document;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_window;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_document_display;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_display_page;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_display_from_source;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_display_font;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, *const u8, u64) -> *mut c_char = fcb_reader_document_images_authorize;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_image_prepare;
    let _: extern "C" fn(u64, u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_image_chunk;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_headings;
    let _: extern "C" fn(u64, u64, u64) -> *mut c_char = fcb_reader_document_links_prepare;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_links;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_link;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64) -> *mut c_char = fcb_reader_document_heading;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_from_source;
    let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_reader_document_source;
    let _: extern "C" fn(u64, u64, u64, u64, u8) -> *mut c_char = fcb_reader_document_copy;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_reader_document_clear;
    let header = include_str!("../include/fcb_reader_document.h");
    for name in ["fcb_reader_document(", "fcb_reader_document_window(", "fcb_reader_document_headings(",
        "fcb_reader_document_display(", "fcb_reader_document_display_page(", "fcb_reader_document_display_font(",
        "fcb_reader_document_display_from_source(",
        "fcb_reader_document_images_authorize(", "fcb_reader_document_image_prepare(", "fcb_reader_document_image_chunk(",
        "fcb_reader_document_links_prepare(", "fcb_reader_document_links(", "fcb_reader_document_link(",
        "fcb_reader_document_heading(", "fcb_reader_document_from_source(", "fcb_reader_document_source(",
        "fcb_reader_document_copy(", "fcb_reader_document_clear("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}

#[test]
fn copy_modes_are_explicit_not_truthy_boolean_aliases() {
    assert_eq!(copy_mode(0).unwrap(), DocumentCopyMode::RenderedText);
    assert_eq!(copy_mode(1).unwrap(), DocumentCopyMode::EnclosingMarkdown);
    for mode in 2..=u8::MAX { assert_eq!(copy_mode(mode).err(), Some(AccessError::InvalidArgument)); }
}

#[test]
fn unknown_handles_null_slugs_and_wide_offsets_return_owned_error_strings() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    // No global handle capacity is consumed by these marshaling checks. Positive
    // source/layout workflows exercise the actual registry in document sessions.
    let slug = CString::new("heading").unwrap();
    let pointers = [
        fcb_reader_document(u64::MAX, 1, 100, 65536, 8192, 8192, 4096),
        fcb_reader_document_window(u64::MAX, 1, u64::MAX, 1),
        fcb_reader_document_display(u64::MAX, 1, u64::MAX),
        fcb_reader_document_display_page(u64::MAX, 1, u64::MAX, 1),
        fcb_reader_document_display_from_source(u64::MAX, 1, u64::MAX, 1),
        fcb_reader_document_display_font(u64::MAX, 1, u64::MAX, u64::MAX, 1),
        fcb_reader_document_headings(u64::MAX, 1, 0, 1),
        fcb_reader_document_links_prepare(u64::MAX, 1, u64::MAX),
        fcb_reader_document_links(u64::MAX, 1, u64::MAX, 1),
        fcb_reader_document_link(u64::MAX, 1, u64::MAX, 1),
        unsafe { fcb_reader_document_heading(u64::MAX, 1, slug.as_ptr(), 1) },
        unsafe { fcb_reader_document_heading(u64::MAX, 1, std::ptr::null(), 1) },
        fcb_reader_document_from_source(u64::MAX, 1, u64::MAX, 1),
        fcb_reader_document_source(u64::MAX, 1, 0, u64::MAX, 0),
        fcb_reader_document_copy(u64::MAX, 1, 0, 1, 255),
        fcb_reader_document_clear(u64::MAX, 1),
    ];
    for pointer in pointers {
        let error = take(pointer);
        assert!(error.contains("\"status\":\"error\""), "{error}");
        assert!(!error.contains("\"document_ready\":true"));
    }
}

#[test]
fn link_inventory_and_navigation_use_the_actual_c_reader_registry() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    struct OpenReader(u64);
    impl Drop for OpenReader {
        fn drop(&mut self) { crate::reader_ffi::fcb_reader_close(self.0); }
    }
    let handle = OpenReader(crate::reader_ffi::fcb_reader_create());
    assert_ne!(handle.0, 0);
    crate::reader_ffi::registry().unwrap().supply(handle.0,
        b"# Target\n\n[Jump](#target) and [Local](docs/next.md).\n").unwrap();
    let prepared = take(fcb_reader_document(handle.0, 1, 80, 65536, 8192, 8192, 4096));
    assert!(prepared.contains("\"document_ready\":true"), "{prepared}");
    let links = take(fcb_reader_document_links_prepare(handle.0, 1, 16));
    assert!(links.contains("\"command\":\"document-links-prepare\""), "{links}");
    assert!(links.contains("\"retained_links\":\"2\""), "{links}");
    assert!(links.contains("\"evidence_precision\":\"whole-captured-markdown-document\""));
    let page = take(fcb_reader_document_links(handle.0, 1, 1, 1));
    assert!(page.contains("\"first_link\":\"1\""), "{page}");
    assert!(page.contains("\"link_id\":\"2\""), "{page}");
    let jump = take(fcb_reader_document_link(handle.0, 1, 1, 4));
    assert!(jump.contains("\"activated\":true"), "{jump}");
    assert!(jump.contains("\"heading_slug\":\"target\""), "{jump}");
    assert!(jump.contains("\"first_flow_line\":\"0\""), "{jump}");
    let local = take(fcb_reader_document_link(handle.0, 1, 2, 4));
    assert!(local.contains("\"activated\":false"), "{local}");
    assert!(local.contains("\"kind\":\"local-reference\""), "{local}");
    assert!(!local.contains("\"first_flow_line\":"), "inert link invented a navigation window");
    for invalid in [
        fcb_reader_document_links(handle.0, 1, 0, 0),
        fcb_reader_document_link(handle.0, 1, 0, 4),
        fcb_reader_document_link(handle.0, 1, u64::MAX, 4),
        fcb_reader_document_links_prepare(handle.0, 1, 4097),
    ] {
        let error = take(invalid);
        assert!(error.contains("\"status\":\"error\""), "{error}");
    }
    let after_refusals = take(fcb_reader_document_link(handle.0, 1, 1, 4));
    assert!(after_refusals.contains("\"activated\":true"), "{after_refusals}");
    take(fcb_reader_document_clear(handle.0, 2));
    let stale = take(fcb_reader_document_link(handle.0, 1, 1, 4));
    assert!(stale.contains("\"status\":\"error\""), "{stale}");
    assert!(!stale.contains("\"activated\":true"));
    let source = take(crate::reader_ffi::fcb_reader_window(handle.0, 0, 4096));
    assert!(source.contains("# Target"), "clearing derived links discarded retained source");
}

#[test]
fn rich_display_and_exact_font_chunks_use_retained_c_handle_authority() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    struct OpenReader(u64);
    impl Drop for OpenReader {
        fn drop(&mut self) { crate::reader_ffi::fcb_reader_close(self.0); }
    }
    let handle = OpenReader(crate::reader_ffi::fcb_reader_create());
    assert_ne!(handle.0, 0);
    let bytes = b"# Native\n\nA **bold** paragraph.\n";
    crate::reader_ffi::registry().unwrap().supply(handle.0, bytes).unwrap();
    let missing = take(fcb_reader_document_display(handle.0, 1, 640));
    assert!(missing.contains("\"status\":\"error\""));
    take(fcb_reader_document(handle.0, 1, 80, 65536, 8192, 8192, 4096));
    let prepared = take(fcb_reader_document_display(handle.0, 1, 640));
    assert!(prepared.contains("\"rendering\":\"bundled-frankenmarkdown-display\""), "{prepared}");
    assert!(prepared.contains("\"glyph_shaped\":true"));
    let page = take(fcb_reader_document_display_page(handle.0, 1, 0, 1));
    assert!(page.contains("\"first_display_item\":\"0\""), "{page}");
    assert_eq!(page.matches("\"item_index\":").count(), 1);
    let offset = bytes.windows(4).position(|part| part == b"bold").unwrap() as u64;
    let restored = take(fcb_reader_document_display_from_source(handle.0, 1, offset, 4));
    assert!(restored.contains("\"command\":\"document-display-from-source\""), "{restored}");
    assert!(restored.contains(&format!("\"original_anchor\":\"{offset}\"")));
    assert!(!restored.contains("\"first_display_item\":\"0\""));
    let font = take(fcb_reader_document_display_font(handle.0, 1, 0, 0, 16));
    assert!(font.contains("\"font_bytes\":\"16\""), "{font}");
    assert!(font.contains("\"next_font_offset\":\"16\""));
    for invalid in [
        fcb_reader_document_display(handle.0, 1, 320),
        fcb_reader_document_display_page(handle.0, 1, 0, 129),
        fcb_reader_document_display_font(handle.0, 1, 0, 0, 65537),
        fcb_reader_document_display_font(handle.0, 2, 0, 0, 16),
    ] { assert!(take(invalid).contains("\"status\":\"error\"")); }
    take(fcb_reader_document_clear(handle.0, 2));
    let old_font = take(fcb_reader_document_display_font(handle.0, 1, 0, 16, 16));
    assert!(old_font.contains("\"status\":\"error\""));
    let source = take(crate::reader_ffi::fcb_reader_window(handle.0, 0, 4096));
    assert!(source.contains("# Native"));
    assert!(font.contains("\"font_hex\":"), "owned buffers survive display retirement");
}


#[test]
fn image_authority_marshaling_bounds_paths_before_reading_foreign_buffers() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    let root = CString::new("/private/image-root").unwrap();
    let relative = b"README.md";
    let invalid = [
        unsafe { fcb_reader_document_images_authorize(u64::MAX, 1, std::ptr::null(), relative.as_ptr(), relative.len() as u64) },
        unsafe { fcb_reader_document_images_authorize(u64::MAX, 1, root.as_ptr(), std::ptr::null(), 1) },
        unsafe { fcb_reader_document_images_authorize(u64::MAX, 1, root.as_ptr(), relative.as_ptr(), 0) },
        unsafe { fcb_reader_document_images_authorize(u64::MAX, 1, root.as_ptr(), relative.as_ptr(), u64::MAX) },
        fcb_reader_document_image_prepare(u64::MAX, 1, u64::MAX, u64::MAX),
        fcb_reader_document_image_chunk(u64::MAX, 1, 0, 0, u64::MAX, u64::MAX),
    ];
    for pointer in invalid {
        let error = take(pointer);
        assert!(error.contains("\"status\":\"error\"") && error.len() < 4096);
        assert!(!error.contains("/private/image-root") && !error.contains("README.md"));
    }
    assert!(unsafe { image_root(std::ptr::null()) }.is_err());
    let empty = CString::new("").unwrap();
    assert!(unsafe { image_root(empty.as_ptr()) }.is_err());
    let exact = CString::new("a".repeat(16_384)).unwrap();
    assert_eq!(unsafe { image_root(exact.as_ptr()) }.unwrap().len(), 16_384);
    let over = [b'a'; 16_385];
    assert!(unsafe { image_root(over.as_ptr().cast()) }.is_err());
    let invalid_utf8 = [0xffu8, 0];
    assert!(unsafe { image_root(invalid_utf8.as_ptr().cast()) }.is_err());
}

#[cfg(unix)]
#[test]
fn local_image_c_route_requires_explicit_root_and_retains_exact_encoded_capture() {
    let _guard = crate::reader_ffi::FFI_TEST_LOCK.lock().unwrap();
    struct OpenReader(u64);
    impl Drop for OpenReader { fn drop(&mut self) { crate::reader_ffi::fcb_reader_close(self.0); } }
    fn field(text: &str, name: &str) -> u64 {
        text.split(&format!("\"{name}\":\"")).nth(1).unwrap().split('"').next().unwrap().parse().unwrap()
    }
    let root = std::env::temp_dir().join(format!("fcb-document-images-c-{}-{}",
        std::process::id(), std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
    std::fs::create_dir_all(&root).unwrap();
    let markdown = b"![Red pixel](pixel.png)\n";
    let png_hex = "89504e470d0a1a0a0000000d49484452000000010000000108060000001f15c48900000010494441547801010500faff00ff0000ff050001fffa5c88d10000000049454e44ae426082";
    let png: Vec<u8> = png_hex.as_bytes().chunks_exact(2).map(|pair|
        u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap()).collect();
    std::fs::write(root.join("retained.rs"), markdown).unwrap();
    std::fs::write(root.join("pixel.png"), &png).unwrap();
    let root = CString::new(root.to_str().unwrap()).unwrap();
    let relative = b"retained.rs";
    let handle = OpenReader(crate::reader_ffi::fcb_reader_create());
    assert_ne!(handle.0, 0);
    crate::reader_ffi::registry().unwrap().supply(handle.0, markdown).unwrap();
    take(fcb_reader_document(handle.0, 1, 80, 65536, 8192, 8192, 4096));
    let display = take(fcb_reader_document_display(handle.0, 1, 640));
    let row = display.split("{\"item_index\":").skip(1)
        .find(|row| row.contains("\"kind\":\"image\"")).unwrap();
    let item: u64 = row.trim_start_matches('"').split('"').next().unwrap().parse().unwrap();
    let request = field(row, "request_id");
    let ungranted = take(fcb_reader_document_image_prepare(handle.0, 1, item, request));
    assert!(ungranted.contains("\"status\":\"error\""));
    let authorized = take(unsafe { fcb_reader_document_images_authorize(handle.0, 1,
        root.as_ptr(), relative.as_ptr(), relative.len() as u64) });
    assert!(authorized.contains("\"status\":\"ok\""), "{authorized}");
    assert_eq!(field(&authorized, "additional_source_bytes_read"), markdown.len() as u64);
    let loaded = take(fcb_reader_document_image_prepare(handle.0, 1, item, request));
    assert!(loaded.contains("\"status\":\"ok\""), "{loaded}");
    assert!(loaded.contains("\"decoded\":false") && loaded.contains("\"image_format\":\"png\""));
    assert!(loaded.contains(&format!("\"image_hex\":\"{png_hex}\"")));
    assert_eq!(field(&loaded, "total_image_bytes"), png.len() as u64);
    assert_eq!(field(&loaded, "image_bytes_read"), png.len() as u64);
    assert_eq!(field(&loaded, "additional_source_bytes_read"), 0);
    let disk_root = std::path::Path::new(root.to_str().unwrap());
    std::fs::write(disk_root.join("pixel.png"), b"changed after capture").unwrap();
    let chunk = take(fcb_reader_document_image_chunk(handle.0, 1, item, request, 8, 16));
    assert!(chunk.contains(&format!("\"image_hex\":\"{}\"", &png_hex[16..48])), "{chunk}");
    assert_eq!(field(&chunk, "image_bytes_read"), 0);
    let repeated = take(fcb_reader_document_image_prepare(handle.0, 1, item, request));
    assert!(repeated.contains(&format!("\"image_hex\":\"{png_hex}\"")));
    assert_eq!(field(&repeated, "image_bytes_read"), 0);
    for invalid in [
        fcb_reader_document_image_prepare(handle.0, 1, item, request.saturating_add(1)),
        fcb_reader_document_image_chunk(handle.0, 1, item, request, 0, 65_537),
        fcb_reader_document_image_chunk(handle.0, 2, item, request, 0, 16),
    ] { assert!(take(invalid).contains("\"status\":\"error\"")); }
    take(fcb_reader_document_clear(handle.0, 2));
    assert!(take(fcb_reader_document_image_chunk(handle.0, 1, item, request, 0, 16))
        .contains("\"status\":\"error\""));
    let source = take(crate::reader_ffi::fcb_reader_window(handle.0, 0, 4096));
    assert!(source.contains("Red pixel"), "image retirement discarded retained source");
}
