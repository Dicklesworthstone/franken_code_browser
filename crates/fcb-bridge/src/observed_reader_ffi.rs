#![deny(unsafe_op_in_unsafe_fn)]

//! Thin C ownership and pointer conversion for the safe observed-range reader.
//! Path policy, positioned I/O, observation identity and decoding stay in Rust
//! host/source services. No input pointer survives the synchronous call.

use std::{ffi::{c_char, c_void}, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, observed_reader::{ObservedReaderEncoding, ObservedReaderSearchKind,
    MAX_OBSERVED_READER_NEEDLE_BYTES}};
use super::{reply, string_out, SearchCancellationCallback};
use super::observed_reader_sessions::{AccessError, Command, ObservedReaderSessions};

static READERS: OnceLock<ObservedReaderSessions> = OnceLock::new();
const MAX_PATH_BYTES: usize = 16_384;

fn answer(handle: u64, work: impl FnOnce(&ObservedReaderSessions) -> Result<HostResponse, AccessError>)
    -> Option<*mut c_char> {
    let result = match READERS.get() { Some(readers) => work(readers), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}

/// The caller supplies readable NUL-terminated storage. Inspect at most the
/// documented bound plus its terminator; never allocate from a foreign length.
unsafe fn bounded_text<'a>(pointer: *const c_char, limit: usize) -> Result<&'a str, AccessError> {
    if pointer.is_null() { return Err(AccessError::InvalidArgument); }
    for length in 0..=limit {
        if unsafe { *pointer.add(length) } == 0 {
            let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
            return std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument);
        }
    }
    Err(AccessError::InvalidArgument)
}
fn decode_path(hex: &str, bytes: &mut [u8; MAX_PATH_BYTES]) -> Result<usize, AccessError> {
    if hex.is_empty() || hex.len() > MAX_PATH_BYTES * 2 || !hex.len().is_multiple_of(2) {
        return Err(AccessError::InvalidArgument);
    }
    fn digit(byte: u8) -> Result<u8, AccessError> {
        match byte { b'0'..=b'9' => Ok(byte - b'0'), b'a'..=b'f' => Ok(byte - b'a' + 10),
            _ => Err(AccessError::InvalidArgument) }
    }
    for (index, pair) in hex.as_bytes().chunks_exact(2).enumerate() {
        bytes[index] = digit(pair[0])? * 16 + digit(pair[1])?;
    }
    Ok(hex.len() / 2)
}
fn native_path(bytes: &[u8]) -> Result<&Path, AccessError> {
    #[cfg(unix)] {
        use std::{ffi::OsStr, os::unix::ffi::OsStrExt};
        Ok(Path::new(OsStr::from_bytes(bytes)))
    }
    #[cfg(not(unix))] {
        let _ = bytes;
        Err(AccessError::Reader(fcb_app::host::observed_reader::ObservedReaderError::App(
            fcb_app::AppError::UnsupportedPlatform)))
    }
}
fn decode_encoding(encoding: u64) -> Result<ObservedReaderEncoding, AccessError> {
    match encoding {
        1 => Ok(ObservedReaderEncoding::Utf8), 2 => Ok(ObservedReaderEncoding::Utf16Le),
        3 => Ok(ObservedReaderEncoding::Utf16Be), _ => Err(AccessError::InvalidArgument),
    }
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_create() -> u64 {
    std::panic::catch_unwind(|| READERS.get_or_init(ObservedReaderSessions::new).create().unwrap_or(0)).unwrap_or(0)
}

/// Metadata-only open under the host's current root access. See the public
/// header for the explicit current-file and opened-descriptor identity contract.
/// # Safety
/// root/path_hex are null or readable NUL-terminated strings through return.
/// If present, poll/context are callable on this worker through return and poll
/// cannot unwind, block or mutate inputs. Free returned strings once using
/// fcb_free_string. The host retains the native root grant until close/drain.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_open(handle: u64, root: *const c_char,
    relative_path_hex: *const c_char, poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let root = unsafe { bounded_text(root, MAX_PATH_BYTES) }?;
        let hex = unsafe { bounded_text(relative_path_hex, MAX_PATH_BYTES * 2) }?;
        let mut bytes = [0u8; MAX_PATH_BYTES];
        let length = decode_path(hex, &mut bytes)?;
        readers.open(handle, Path::new(root), &bytes[..length],
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Metadata-only open of the one explicitly selected native file. The exact
/// absolute path is hex encoded so non-UTF8 filenames need no lossy conversion.
/// No parent project is opened or discovered and no payload bytes are read.
/// # Safety
/// absolute_path_hex is null or readable NUL-terminated storage through return.
/// poll/context obey the same worker lifetime contract as project-relative open.
/// The host retains the selected file's native grant until close/drain and frees
/// returned strings once using fcb_free_string.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_open_file(handle: u64, absolute_path_hex: *const c_char,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let hex = unsafe { bounded_text(absolute_path_hex, MAX_PATH_BYTES * 2) }?;
        let mut bytes = [0u8; MAX_PATH_BYTES];
        let length = decode_path(hex, &mut bytes)?;
        readers.open_file(handle, native_path(&bytes[..length])?,
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle, Command::Info, || false)))
}

/// A fresh bounded observation of the already opened file, never a refill of
/// an old exact capture. Each successful request has a new source revision.
/// # Safety
/// If supplied, poll/context obey the same worker lifetime contract as open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_window(handle: u64, offset: u64, bytes: u64,
    encoding: u64, poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let encoding = decode_encoding(encoding)?;
        let bytes = usize::try_from(bytes).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::Window { offset, bytes, encoding },
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Locate and retain a line window during one bounded forward observation of
/// the already opened file. It never scans an old prefix and rereads the target
/// from different live bytes. Missing and budget-limited lines remain distinct.
/// # Safety
/// If supplied, poll/context obey the same worker lifetime contract as open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_lines(handle: u64, first_line: u64,
    line_count: u64, max_bytes: u64, max_scan_bytes: u64, encoding: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let encoding = decode_encoding(encoding)?;
        let max_bytes = usize::try_from(max_bytes).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::Lines { first_line, line_count, max_bytes, max_scan_bytes, encoding },
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_retained(handle: u64, observation: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle, Command::Retained { observation }, || false)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_copy(handle: u64, observation: u64, start: u64, end: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::Copy { observation, start, end }, || false)))
}

/// Resolve decoded UTF-8 boundaries in one exact retained observation. This
/// performs no source I/O and has no clipboard effect. Cancellation through the
/// handle's epoch withdraws in-flight delivery; the observation stays readable.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_resolve_selection(handle: u64, observation: u64,
    utf8_start: u64, utf8_end: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::ResolveSelection { observation, utf8_start, utf8_end }, || false)))
}

/// Search one continuous observation of the already opened file. Only exact
/// literal witnesses are retained, not surrounding source or a whole capture.
/// # Safety
/// needle is null or readable NUL-terminated UTF-8 through return. The bounded
/// input is borrowed only for this call. If supplied, poll/context obey the
/// same worker lifetime contract as open, including no blocking or unwinding.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_find(handle: u64, generation: u64,
    needle: *const c_char, encoding: u64, max_matches: u64, max_scan_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let needle = unsafe { bounded_text(needle, MAX_OBSERVED_READER_NEEDLE_BYTES) }?;
        let encoding = decode_encoding(encoding)?;
        let max_matches = usize::try_from(max_matches).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::Find { generation, needle, encoding, max_matches, max_scan_bytes },
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Count all exact occurrences within the same scan budgets while retaining at
/// most max_matches rows. Legacy fcb_observed_reader_find keeps early stopping.
/// # Safety
/// Pointer and callback requirements match fcb_observed_reader_find.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_find_counted(handle: u64, generation: u64,
    needle: *const c_char, encoding: u64, max_matches: u64, max_scan_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let needle = unsafe { bounded_text(needle, MAX_OBSERVED_READER_NEEDLE_BYTES) }?;
        let encoding = decode_encoding(encoding)?;
        let max_matches = usize::try_from(max_matches).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::FindCounted { generation, needle, encoding, max_matches, max_scan_bytes },
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Find with explicit matching and retention policies. Flags must be 0 or 1;
/// lowercase uses Unicode scalar lowering, not canonical normalization or full
/// case folding. Each retained row owns its original spelling/byte witness.
/// # Safety
/// Pointer and callback requirements match fcb_observed_reader_find.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_find_matching(handle: u64, generation: u64,
    needle: *const c_char, encoding: u64, case_sensitive: u64, count_all_matches: u64,
    max_matches: u64, max_scan_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        if case_sensitive > 1 || count_all_matches > 1 { return Err(AccessError::InvalidArgument); }
        let needle = unsafe { bounded_text(needle, MAX_OBSERVED_READER_NEEDLE_BYTES) }?;
        let encoding = decode_encoding(encoding)?;
        let max_matches = usize::try_from(max_matches).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::FindMatching { generation, needle, encoding,
            case_sensitive: case_sensitive == 1, count_all_matches: count_all_matches == 1,
            max_matches, max_scan_bytes }, || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Select literal text (mode 0) or whole-token occurrences (mode 1). Occurrence
/// names use the shared bounded token policy and require case_sensitive=1.
/// Existing find entry points retain their literal-text behavior.
/// # Safety
/// Pointer and callback requirements match fcb_observed_reader_find.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_find_mode(handle: u64, generation: u64,
    needle: *const c_char, encoding: u64, mode: u64, case_sensitive: u64,
    count_all_matches: u64, max_matches: u64, max_scan_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        if case_sensitive > 1 || count_all_matches > 1 { return Err(AccessError::InvalidArgument); }
        let kind = match mode {
            0 => ObservedReaderSearchKind::Text, 1 => ObservedReaderSearchKind::Occurrences,
            _ => return Err(AccessError::InvalidArgument),
        };
        let needle = unsafe { bounded_text(needle, MAX_OBSERVED_READER_NEEDLE_BYTES) }?;
        let encoding = decode_encoding(encoding)?;
        let max_matches = usize::try_from(max_matches).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::FindMode { generation, needle, encoding, kind,
            case_sensitive: case_sensitive == 1, count_all_matches: count_all_matches == 1,
            max_matches, max_scan_bytes }, || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Rescan from byte zero and retain a page after start_after occurrences, with
/// the same mode/count/byte limits as find_mode. Each page is a NEW observation
/// of the live opened file, never continuation of an immutable old capture.
/// # Safety
/// Pointer and callback requirements match fcb_observed_reader_find.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_find_page(handle: u64, generation: u64,
    needle: *const c_char, encoding: u64, mode: u64, case_sensitive: u64,
    count_all_matches: u64, start_after: u64, max_matches: u64, max_scan_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        if case_sensitive > 1 || count_all_matches > 1 { return Err(AccessError::InvalidArgument); }
        let kind = match mode {
            0 => ObservedReaderSearchKind::Text, 1 => ObservedReaderSearchKind::Occurrences,
            _ => return Err(AccessError::InvalidArgument),
        };
        let needle = unsafe { bounded_text(needle, MAX_OBSERVED_READER_NEEDLE_BYTES) }?;
        let encoding = decode_encoding(encoding)?;
        let max_matches = usize::try_from(max_matches).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::FindPage { generation, needle, encoding, kind,
            case_sensitive: case_sensitive == 1, count_all_matches: count_all_matches == 1,
            start_after, max_matches, max_scan_bytes }, || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

/// Return only the accepted query's retained matched bytes. No source I/O and
/// no silent substitution from current file contents or another query.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_search_hit(handle: u64, generation: u64, position: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let position = usize::try_from(position).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::SearchHit { generation, position }, || false)
    }))
}

/// Read fresh context for a retained search hit, verify its literal witness,
/// and publish a NEW observation only after verification. Never an old capture.
/// # Safety
/// poll/context obey the worker lifetime and non-reentrancy contract of open.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_observed_reader_observe_hit(handle: u64, generation: u64,
    hit_index: u64, context_bytes: u64, window_bytes: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let hit_index = usize::try_from(hit_index).map_err(|_| AccessError::InvalidArgument)?;
        let context_bytes = usize::try_from(context_bytes).map_err(|_| AccessError::InvalidArgument)?;
        let window_bytes = usize::try_from(window_bytes).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::ObserveHit { generation, hit_index, context_bytes, window_bytes },
            || poll.is_some_and(|poll| unsafe { poll(context) != 0 }))
    }))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(READERS.get().is_some_and(|readers| readers.cancel(handle).is_ok()))).unwrap_or(0)
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(READERS.get().is_some_and(|readers| readers.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(all(test, unix))]
#[path = "observed_reader_ffi_tests.rs"]
mod tests;

#[cfg(all(test, unix))]
#[path = "observed_reader_search_ffi_tests.rs"]
mod search_tests;

#[cfg(all(test, unix))]
#[path = "observed_reader_lines_ffi_tests.rs"]
mod lines_tests;

#[cfg(all(test, unix))]
#[path = "observed_reader_selection_ffi_tests.rs"]
mod selection_tests;
