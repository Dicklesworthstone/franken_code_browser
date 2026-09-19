#![deny(unsafe_op_in_unsafe_fn)]

//! Thin marshaling for the same retained-reader document service used by hosts.
//! No native renderer, Markdown engine, source lookup or independent registry.
use std::ffi::c_char;
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
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_document_headings(handle: u64, generation: u64, first: u64, count: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::DocumentHeadings { generation, first: number(first)?, count: number(count)? }, || false)))
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
