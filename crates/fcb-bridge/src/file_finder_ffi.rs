#![deny(unsafe_op_in_unsafe_fn)]

//! Thin pointer and enum conversion for the progressive path finder. Neither
//! discovery, ranking nor source authorization is implemented at the ABI.
use std::{ffi::c_char, path::Path, sync::OnceLock};
use fcb::search::{PathCase, PathMatchMode};
use fcb_app::host::{HostResponse, file_finder_session::{FileFinderOptions, MAX_FILE_FINDER_QUERY_BYTES}};
use super::{cstr, reply, string_out};
use super::file_finder_sessions::{AccessError, Command, FileFinderSessions};

static FINDERS: OnceLock<FileFinderSessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&FileFinderSessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match FINDERS.get() { Some(finders) => work(finders), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn number(value: u64) -> Result<usize, AccessError> { usize::try_from(value).map_err(|_| AccessError::InvalidArgument) }

/// Reserve an empty cancelable handle without opening the filesystem.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_create() -> u64 {
    std::panic::catch_unwind(|| FINDERS.get_or_init(FileFinderSessions::new).create().unwrap_or(0)).unwrap_or(0)
}
/// Fix one query for this handle and admit the first metadata batch. Native
/// callers advance bounded work next; info can recover a lost initial reply.
/// # Safety
/// root is null or readable NUL-terminated UTF-8. query is readable for exactly
/// query_bytes when its nonzero length is within the public bound. Both remain
/// stable until return. Returned JSON is owned; release with fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_file_finder_open(handle: u64, root: *const c_char,
    generation: u64, query: *const u8, query_bytes: u64, mode: u32, match_case: u8,
    max_results: u64) -> *mut c_char {
    reply(|| answer(handle, |finders| {
        let length = number(query_bytes)?;
        if query.is_null() || length == 0 || length > MAX_FILE_FINDER_QUERY_BYTES {
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
        let root = unsafe { cstr(root) }.ok_or(AccessError::InvalidArgument)?;
        let needle = unsafe { std::slice::from_raw_parts(query, length) };
        let options = FileFinderOptions { max_results: number(max_results)?, case, mode };
        finders.open(handle, Path::new(root), generation, needle, options, || false)
    }))
}
/// Return current retained results and coverage without walking another batch.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |finders| finders.execute(handle, Command::Info, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_page(handle: u64, generation: u64, revision: u64) -> *mut c_char {
    reply(|| answer(handle, |finders| finders.execute(handle, Command::Page { generation, revision }, || false)))
}
/// Process at most 16 discovery batches, or replay the last accepted predecessor
/// without consuming more work. Rows may improve rank; revision qualifies them.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_advance(handle: u64, generation: u64,
    after_revision: u64, max_batches: u64) -> *mut c_char {
    reply(|| answer(handle, |finders| finders.execute(handle, Command::Advance {
        generation, after_revision, max_batches: number(max_batches)?,
    }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_stop(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |finders| finders.execute(handle, Command::Stop { generation }, || false)))
}
/// Capture the selected retained hit into an existing EMPTY reader. There is no
/// caller-supplied path to replace the owner/query/revision/file proof.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_open_reader(handle: u64, reader: u64,
    generation: u64, revision: u64, file: u64, max_source_bytes: u64) -> *mut c_char {
    reply(|| answer(handle, |finders| {
        let readers = super::reader_ffi::registry().ok_or(super::reader_sessions::AccessError::UnknownHandle)?;
        finders.open_reader(handle, readers, reader, generation, revision, file, number(max_source_bytes)?, || false)
    }))
}
/// Nonblocking epoch cancellation. Accepted progress remains recoverable.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(FINDERS.get().is_some_and(|finders| finders.cancel(handle).is_ok()))).unwrap_or(0)
}
/// Worker-only retirement; delivered JSON and independent readers remain owned.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_file_finder_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(FINDERS.get().is_some_and(|finders| finders.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(all(test, unix))]
#[path = "file_finder_ffi_tests.rs"]
mod tests;
