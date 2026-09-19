#![deny(unsafe_op_in_unsafe_fn)]

//! C marshaling only; extraction and exact selection use the shared reader.
use std::ffi::c_char;
use fcb::search::{SymbolLanguage, SymbolNameMode};
use fcb_app::host::reader::ReaderOutlineOptions;
use super::{answer, cstr, number, reply, AccessError, Command};

fn mode(value: u8) -> Result<SymbolNameMode, AccessError> {
    match value { 0 => Ok(SymbolNameMode::Exact), 1 => Ok(SymbolNameMode::Prefix),
        2 => Ok(SymbolNameMode::Contains), _ => Err(AccessError::InvalidArgument) }
}

/// Extract from the retained whole capture, not a live path. Null language means
/// infer the label suffix; a supplied language is explicit and must be supported.
/// # Safety
/// language is null or readable NUL-terminated UTF-8 stable until return.
/// Free returned non-null JSON exactly once with fcb_free_string. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_outline(handle: u64, generation: u64,
    language: *const c_char, max_items: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let language = if language.is_null() { None } else {
            let name = unsafe { cstr(language) }.ok_or(AccessError::InvalidArgument)?;
            if name.len() > 32 { return Err(AccessError::InvalidArgument); }
            Some(SymbolLanguage::from_name(name).ok_or(AccessError::InvalidArgument)?)
        };
        readers.execute(handle, Command::Outline { generation,
            options: ReaderOutlineOptions { language, max_items: number(max_items)? } }, || false)
    }))
}

/// Filter already-extracted names, without source I/O or parsing. Null/empty
/// needle lists the inventory. start indexes the filtered view, not symbol IDs.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 stable until return.
/// Returned strings have the same fcb_free_string ownership as other reader calls.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_symbols(handle: u64, generation: u64,
    needle: *const c_char, name_mode: u8, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let needle = if needle.is_null() { "" } else { unsafe { cstr(needle) }.ok_or(AccessError::InvalidArgument)? };
        readers.execute(handle, Command::Symbols { generation, needle, mode: mode(name_mode)?,
            start: number(start)?, limit: number(limit)? }, || false)
    }))
}

/// Exact candidate selection plus bounded captured context. Symbol IDs are
/// one-based within the stated outline, not rows or content-search occurrences.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_symbol(handle: u64, generation: u64, symbol_id: u64,
    context_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::Symbol { generation, id: symbol_id, context: number(context_bytes)? }, || false)))
}

/// Original identifier bytes (0), or original declaration-evidence bytes (1).
/// A missing exact name span falls back to the disclosed evidence span. This is
/// returned data, not a clipboard write. Other flag values are invalid arguments.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_copy_symbol(handle: u64, generation: u64, symbol_id: u64,
    whole_declaration: u8) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let whole_declaration = match whole_declaration { 0 => false, 1 => true, _ => return Err(AccessError::InvalidArgument) };
        readers.execute(handle, Command::CopySymbol { generation, id: symbol_id, whole_declaration }, || false)
    }))
}

/// Clear derived symbols with a new outline generation. Retained source,
/// literal-search results and already-returned strings remain owned separately.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_outline_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle, Command::ClearOutline { generation }, || false)))
}

#[cfg(all(test, unix))]
#[path = "reader_outline_ffi_tests.rs"]
mod tests;
