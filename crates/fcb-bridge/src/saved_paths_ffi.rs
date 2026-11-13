#![deny(unsafe_op_in_unsafe_fn)]

//! Borrowed byte/enum conversion for the saved archive's shared path finder.
//! Ranking and source authorization stay in the safe saved-repository host.

use super::{AccessError, Command, Selection, answer, c_char, number, reply};
use fcb_app::host::saved_repository::paths::{SavedPathOptions, PathCase, PathMatchMode,
    MAX_SAVED_PATH_QUERY_BYTES};

/// Find filenames using archive metadata only. mode: 0 exact, 1 prefix, 2 fuzzy;
/// match_case: 0 Unicode lowercase, 1 sensitive. Bounds remain enforced by the
/// host. No source payload or index file is read while finding a filename.
/// # Safety
/// query is readable for query_bytes when the length is nonzero and at most
/// MAX_SAVED_PATH_QUERY_BYTES. The pointer is borrowed only until return.
/// Release every non-null response exactly once with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_saved_path_search(handle: u64, generation: u64,
    query: *const u8, query_bytes: u64, mode: u64, match_case: u64,
    max_results: u64, max_work_units: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let length = number(query_bytes)?;
        if query.is_null() || length == 0 || length > MAX_SAVED_PATH_QUERY_BYTES {
            return Err(AccessError::InvalidArgument);
        }
        let mode = match mode {
            0 => PathMatchMode::Exact, 1 => PathMatchMode::Prefix, 2 => PathMatchMode::Fuzzy,
            _ => return Err(AccessError::InvalidArgument),
        };
        let case = match match_case {
            0 => PathCase::UnicodeLowercase, 1 => PathCase::Sensitive,
            _ => return Err(AccessError::InvalidArgument),
        };
        let needle = unsafe { std::slice::from_raw_parts(query, length) };
        let options = SavedPathOptions { case, mode, max_results: number(max_results)?, max_work_units };
        sessions.execute(handle, Command::PathSearch { generation, needle, options }, || false)
    }))
}

/// Replay a page from the accepted query without reranking or source I/O.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_path_results(handle: u64, generation: u64,
    start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::PathResults { generation, start: number(start)?, limit: number(limit)? }, || false)))
}

/// Retire only path results, with an independently increasing path generation.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_clear_paths(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::ClearPaths { generation }, || false)))
}

/// Verify the captured member selected by this query and install it in an EMPTY
/// reader. Uncaptured rows cannot open. The ordinary saved open-reader receipt
/// carries a null text selection and the selected raw member identity.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_saved_open_path_hit_reader(handle: u64, reader: u64,
    generation: u64, hit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_reader(handle, readers, reader, Selection::PathHit { generation, id: hit }, || false)
    }))
}

#[cfg(all(test, unix))]
#[path = "saved_paths_ffi_tests.rs"]
mod tests;
