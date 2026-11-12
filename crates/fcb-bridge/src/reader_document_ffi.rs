#![deny(unsafe_op_in_unsafe_fn)]

//! Thin marshaling for the same retained-reader document service used by hosts.
//! No native renderer, Markdown engine, source lookup or independent registry.
use std::{ffi::c_char, path::Path};
use fcb_app::host::reader::{ReaderDocumentOptions, DocumentCopyMode};
use super::{answer, cstr, number, reply, AccessError, Command};

#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document(handle: u64, generation: u64, width_columns: u64,
    max_source_bytes: u64, max_flow_lines: u64, max_flow_items: u64, max_blocks: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let options = ReaderDocumentOptions {
            width_columns: u32::try_from(width_columns).map_err(|_| AccessError::InvalidArgument)?,
            max_source_bytes: number(max_source_bytes)?, max_flow_lines: number(max_flow_lines)?,
            max_flow_items: number(max_flow_items)?, max_blocks: number(max_blocks)?,
        };
        readers.execute(handle, Command::Document { generation, options }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_window(handle: u64, generation: u64, first: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentWindow { generation, first: number(first)?, count: number(count)? }, || false)))
}
/// Prepare exact upstream glyph geometry for an already accepted logical
/// document generation. This is host-worker work, never a drawing callback.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_display(handle: u64, generation: u64, width_points: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentDisplay { generation,
            width_points: u32::try_from(width_points).map_err(|_| AccessError::InvalidArgument)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_display_page(handle: u64, generation: u64, first: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentDisplayPage { generation, first: number(first)?, count: number(count)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_display_from_source(handle: u64, generation: u64,
    original_offset: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentDisplayFromSource { generation, offset: original_offset, count: number(count)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_display_font(handle: u64, generation: u64,
    font_index: u64, offset: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentDisplayFont { generation, index: number(font_index)?,
            offset: number(offset)?, count: number(count)? }, || false)))
}
/// Explicit current-root authorization for optional local images. Source path
/// labels never become authority: the safe host verifies the selected source
/// under this root against the retained capture before admitting image reads.
/// # Safety
/// root is null or readable through its NUL terminator (at most 16384 bytes).
/// source_relative is null or points to source_relative_length readable bytes;
/// both buffers remain immutable through return. No pointer is retained. The
/// host retains its native root grant until all image work and reader close drain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_document_images_authorize(handle: u64, generation: u64,
    root: *const c_char, source_relative: *const u8, source_relative_length: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let length = number(source_relative_length)?;
        if source_relative.is_null() || !(1..=16_384).contains(&length) {
            return Err(AccessError::InvalidArgument);
        }
        let root = unsafe { image_root(root) }?;
        let relative = unsafe { std::slice::from_raw_parts(source_relative, length) };
        if relative.contains(&0) { return Err(AccessError::InvalidArgument); }
        readers.execute(handle, Command::DocumentImagesAuthorize {
            generation, root: Path::new(root), source_relative: relative }, || false)
    }))
}

/// Bound the scan before constructing a borrowed string; allocation never
/// depends on an untrusted foreign path length.
unsafe fn image_root<'a>(pointer: *const c_char) -> Result<&'a str, AccessError> {
    if pointer.is_null() { return Err(AccessError::InvalidArgument); }
    for length in 0..=16_384 {
        if unsafe { *pointer.add(length) } == 0 {
            if length == 0 { return Err(AccessError::InvalidArgument); }
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            return std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument);
        }
    }
    Err(AccessError::InvalidArgument)
}

/// Read a bounded local asset on the host worker, or reuse the immutable
/// encoded capture for the same display item/request. This performs no decode.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_image_prepare(handle: u64, generation: u64,
    item_index: u64, request_id: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentImagePrepare { generation, item_index: number(item_index)?, request_id }, || false)))
}

/// Copy a bounded chunk of the retained encoded asset, without filesystem I/O.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_image_chunk(handle: u64, generation: u64,
    item_index: u64, request_id: u64, offset: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentImageChunk { generation, item_index: number(item_index)?, request_id,
            offset: number(offset)?, count: number(count)? }, || false)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_headings(handle: u64, generation: u64, first: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentHeadings { generation, first: number(first)?, count: number(count)? }, || false)))
}
/// Parse links from the same retained source on a host worker. The accepted
/// document generation, not a path label, identifies the source of every row.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_links_prepare(handle: u64, generation: u64, max_links: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentLinksPrepare { generation, max_links: number(max_links)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_links(handle: u64, generation: u64, first: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentLinks { generation, first: number(first)?, count: number(count)? }, || false)))
}
/// Resolve a retained occurrence, not caller-supplied destination text. Current
/// document anchors navigate retained bytes; other destinations remain inert.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_link(handle: u64, generation: u64, link_id: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentLink { generation, id: link_id, count: number(count)? }, || false)))
}
/// # Safety
/// slug is null or readable NUL-terminated UTF-8 stable until return. No foreign
/// reference survives the call. Every returned string uses fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_document_heading(handle: u64, generation: u64,
    slug: *const c_char, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let slug = unsafe { cstr(slug) }.ok_or(AccessError::InvalidArgument)?;
        if slug.is_empty() || slug.len() > 4096 { return Err(AccessError::InvalidArgument); }
        readers.execute(handle, Command::DocumentHeading { generation, slug, count: number(count)? }, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_from_source(handle: u64, generation: u64, original_offset: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentFromSource { generation, offset: original_offset, count: number(count)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_source(handle: u64, generation: u64,
    rendered_start: u64, rendered_end: u64, context_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentSource { generation, start: number(rendered_start)?, end: number(rendered_end)?,
            context: number(context_bytes)? }, || false)))
}
fn copy_mode(mode: u8) -> Result<DocumentCopyMode, AccessError> {
    match mode { 0 => Ok(DocumentCopyMode::RenderedText), 1 => Ok(DocumentCopyMode::EnclosingMarkdown),
        _ => Err(AccessError::InvalidArgument) }
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_copy(handle: u64, generation: u64,
    rendered_start: u64, rendered_end: u64, mode: u8) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentCopy { generation, start: number(rendered_start)?, end: number(rendered_end)?,
            mode: copy_mode(mode)? }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle, Command::ClearDocument { generation }, || false)))
}

#[cfg(test)]
#[path = "reader_document_ffi_tests.rs"]
mod tests;
