#![deny(unsafe_op_in_unsafe_fn)]

//! C marshaling for the shared retained-reader whole-token occurrence service.
//! No parser, source lookup, runtime or name-resolution policy lives here.
use std::ffi::c_char;
use super::{answer, cstr, number, reply, AccessError, Command};

/// Search one retained capture; comments and literals participate. This is not
/// compiler reference resolution. Generations are independent of Find/Outline.
/// # Safety
/// name is null or readable NUL-terminated UTF-8 stable until return. The name
/// is copied by the host service and no foreign borrow survives this call.
/// Free returned non-null JSON exactly once with fcb_free_string. Worker only.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_reader_references(handle: u64, generation: u64,
    name: *const c_char, max_items: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let name = unsafe { cstr(name) }.ok_or(AccessError::InvalidArgument)?;
        readers.execute(handle, Command::References { generation, name,
            max_items: number(max_items)? }, || false)
    }))
}

/// Page already-retained occurrences without rescanning or source I/O. start is
/// a zero-based page offset; activate the returned one-based reference_id.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_reference_page(handle: u64, generation: u64,
    start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::ReferencePage { generation, start: number(start)?, limit: number(limit)? }, || false)))
}

/// Return exact original bytes and decoded selection in a bounded context
/// window. No pathname is reopened, including after working-tree replacement.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_reference(handle: u64, generation: u64,
    reference_id: u64, context_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::Reference { generation, id: reference_id, context: number(context_bytes)? }, || false)))
}

/// Return the exact occurrence bytes as hex; this does not write a clipboard.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_copy_reference(handle: u64, generation: u64,
    reference_id: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::CopyReference { generation, id: reference_id }, || false)))
}

/// Release only the accepted occurrence inventory with a fresh generation.
/// Content search, outline, document and previously returned strings survive.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_references_clear(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::ClearReferences { generation }, || false)))
}

#[cfg(all(test, unix))]
#[path = "reader_references_ffi_tests.rs"]
mod tests;
