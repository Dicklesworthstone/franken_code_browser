#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer-free saved import commands. Parsing, finite catalog resolution,
//! original byte evidence and independent reader ownership stay in the host.

use super::{AccessError, Command, Selection, answer, c_char, number, reply};
use fcb_app::host::saved_repository::imports::{SavedImportDirection, SavedImportSide};

/// Build a bounded lexical import inventory from the already opened archive.
/// No saved pathname is opened as live source. Host worker only.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_prepare_imports(handle: u64, generation: u64,
    max_links: u64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::PrepareImports { generation, max_links: number(max_links)?, max_source_bytes }, || false)))
}

/// direction 0 = all (member must be zero), 1 = outgoing, 2 = incoming.
/// Member ordinals are zero-based; pages read accepted metadata only.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_imports(handle: u64, generation: u64, direction: u64,
    member: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let (direction, member) = match direction {
            0 if member == 0 => (SavedImportDirection::All, None),
            1 => (SavedImportDirection::Outgoing, Some(number(member)?)),
            2 => (SavedImportDirection::Incoming, Some(number(member)?)),
            _ => return Err(AccessError::InvalidArgument),
        };
        sessions.execute(handle, Command::Imports { generation, member, direction,
            start: number(start)?, limit: number(limit)? }, || false)
    }))
}

/// Retire the accepted import inventory without closing independently retained
/// readers, Markdown relationships, or any of the saved search inventories.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_clear_imports(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::ClearImports { generation }, || false)))
}

/// Open exact statement evidence (side 0) or a unique captured target (side 1)
/// in an EMPTY reader destination. Member digest verification precedes delivery.
/// No import, package initializer, live pathname or external URL is executed.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_open_import_reader(handle: u64, reader: u64,
    generation: u64, link_id: u64, side: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let side = match side {
            0 => SavedImportSide::Evidence, 1 => SavedImportSide::Target,
            _ => return Err(AccessError::InvalidArgument),
        };
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_reader(handle, readers, reader, Selection::Import { generation, id: link_id, side }, || false)
    }))
}

#[cfg(all(test, unix))]
#[path = "saved_imports_ffi_tests.rs"]
mod tests;
