#![deny(unsafe_op_in_unsafe_fn)]

//! Thin C ownership and pointer conversion for the safe observed-range reader.
//! Path policy, positioned I/O, observation identity and decoding stay in Rust
//! host/source services. No input pointer survives the synchronous call.

use std::{ffi::{c_char, c_void}, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, observed_reader::{ObservedReaderEncoding,
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

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_retained(handle: u64, observation: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle, Command::Retained { observation }, || false)))
}

#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_copy(handle: u64, observation: u64, start: u64, end: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| readers.execute(handle,
        Command::Copy { observation, start, end }, || false)))
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

/// Return only the accepted query's retained matched bytes. No source I/O and
/// no silent substitution from current file contents or another query.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_observed_reader_search_hit(handle: u64, generation: u64, position: u64) -> *mut c_char {
    reply(|| answer(handle, |readers| {
        let position = usize::try_from(position).map_err(|_| AccessError::InvalidArgument)?;
        readers.execute(handle, Command::SearchHit { generation, position }, || false)
    }))

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
