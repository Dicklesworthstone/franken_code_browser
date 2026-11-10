#![deny(unsafe_op_in_unsafe_fn)]

//! C pointer and scalar conversion only. The safe host owns both verified FCBS
//! archives, comparison state and exact source transfer to an admitted reader.
use std::{ffi::{c_char, c_void}, path::Path, sync::OnceLock};
use fcb_app::host::{HostResponse, saved_repository::SnapshotLimits};
use fcb_app::host::snapshot_comparison::{SnapshotChangeFilter, SnapshotComparisonSide};
use crate::{reply, string_out, search_ffi::SearchCancellationCallback};
use crate::snapshot_comparison_sessions::{AccessError, Command, SnapshotComparisonSessions};

const MAX_ARCHIVE_PATH_BYTES: usize = 4096;
static COMPARISONS: OnceLock<SnapshotComparisonSessions> = OnceLock::new();

fn answer(handle: u64,
    work: impl FnOnce(&SnapshotComparisonSessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match COMPARISONS.get() {
        Some(sessions) => work(sessions), None => Err(AccessError::Unknown),
    };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn number(value: u64) -> Result<usize, AccessError> {
    usize::try_from(value).map_err(|_| AccessError::InvalidArgument)
}
fn filter(value: u64) -> Result<SnapshotChangeFilter, AccessError> {
    match value {
        0 => Ok(SnapshotChangeFilter::All), 1 => Ok(SnapshotChangeFilter::Differences),
        2 => Ok(SnapshotChangeFilter::Unchanged), 3 => Ok(SnapshotChangeFilter::Changed),
        4 => Ok(SnapshotChangeFilter::Added), 5 => Ok(SnapshotChangeFilter::Removed),
        6 => Ok(SnapshotChangeFilter::OnlyBefore), 7 => Ok(SnapshotChangeFilter::OnlyAfter),
        8 => Ok(SnapshotChangeFilter::Unavailable), _ => Err(AccessError::InvalidArgument),
    }
}
fn side(value: u64) -> Result<SnapshotComparisonSide, AccessError> {
    match value { 0 => Ok(SnapshotComparisonSide::Before), 1 => Ok(SnapshotComparisonSide::After),
        _ => Err(AccessError::InvalidArgument) }
}
/// The caller supplies readable bytes through NUL or MAX_ARCHIVE_PATH_BYTES+1;
/// no pointer or borrowed view is retained beyond the current foreign call.
unsafe fn path<'a>(pointer: *const c_char) -> Result<&'a Path, AccessError> {
    if pointer.is_null() { return Err(AccessError::InvalidArgument); }
    let mut length = 0;
    while length <= MAX_ARCHIVE_PATH_BYTES {
        if unsafe { *pointer.add(length) } == 0 { break; }
        length += 1;
    }
    if length == 0 || length > MAX_ARCHIVE_PATH_BYTES { return Err(AccessError::InvalidArgument); }
    let bytes = unsafe { std::slice::from_raw_parts(pointer.cast::<u8>(), length) };
    let text = std::str::from_utf8(bytes).map_err(|_| AccessError::InvalidArgument)?;
    Ok(Path::new(text))
}

/// Reserve an empty comparison owner before archive I/O. Zero means refusal.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_create() -> u64 {
    std::panic::catch_unwind(|| COMPARISONS.get_or_init(SnapshotComparisonSessions::new)
        .create().unwrap_or(0)).unwrap_or(0)
}
/// Open both archives under one owner. Failure keeps the empty handle retryable.
/// Opening never starts comparison classification.
/// # Safety
/// Each path is readable NUL-terminated UTF-8, at most 4096 bytes before NUL,
/// stable through return. Every non-null result uses fcb_free_string ownership.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_snapshot_comparison_open(handle: u64,
    before_path: *const c_char, after_path: *const c_char) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let before = unsafe { path(before_path) }?;
        let after = unsafe { path(after_path) }?;
        sessions.open(handle, before, after, SnapshotLimits::default(), || false)
    }))
}
/// Cooperatively cancel both archive opens and verification before acceptance.
/// # Safety
/// Same path contract as open. Poll/context are borrowed until return; poll
/// must not unwind or reenter this comparison. No callback pointer is retained.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_snapshot_comparison_open_cancelable(handle: u64,
    before_path: *const c_char, after_path: *const c_char,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        let before = unsafe { path(before_path) }?;
        let after = unsafe { path(after_path) }?;
        sessions.open(handle, before, after, SnapshotLimits::default(), &mut canceled)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Info, || false)))
}
/// Start an independently monotone comparison attempt. Earlier accepted rows
/// remain authoritative until a complete replacement is accepted.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_begin(handle: u64, generation: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Begin { generation }, || false)))
}
/// # Safety
/// Poll/context follow the borrowed callback contract of open_cancelable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_snapshot_comparison_begin_cancelable(handle: u64, generation: u64,
    poll: Option<SearchCancellationCallback>, context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        sessions.execute(handle, Command::Begin { generation }, &mut canceled)
    }))
}
/// Advance at most max_entries merged paths (1..1024), without source reads.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_step(handle: u64, generation: u64,
    max_entries: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::Step { generation, max_entries: number(max_entries)? }, || false)))
}
/// # Safety
/// Poll/context follow the borrowed callback contract of open_cancelable.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_snapshot_comparison_step_cancelable(handle: u64,
    generation: u64, max_entries: u64, poll: Option<SearchCancellationCallback>,
    context: *mut c_void) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        // An already-canceled step must enter the host so it retires the
        // pending attempt while preserving any prior accepted comparison.
        sessions.execute(handle, Command::Step { generation, max_entries: number(max_entries)? }, &mut canceled)
    }))
}
/// Page accepted classifications; IDs remain global across filters. Filter
/// 0=all, 1=differences, 2=unchanged, 3=changed, 4=added, 5=removed,
/// 6=only-before, 7=only-after, 8=unavailable. Page limit is 1..128.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_page(handle: u64, generation: u64,
    filter_code: u64, start: u64, limit: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Page {
        generation, filter: filter(filter_code)?, start: number(start)?, limit: number(limit)? }, || false)))
}
/// Reverify one selected archived member and install it into an existing EMPTY
/// reader. Side 0 is before, 1 is after. No live source path is reopened, and
/// already opened readers remain independent when this comparison closes.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_open_reader(handle: u64, reader: u64,
    generation: u64, entry_id: u64, side_code: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let side = side(side_code)?;
        let readers = crate::reader_ffi::registry()
            .ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        sessions.open_reader(handle, readers, reader, generation, entry_id, side, || false)
    }))
}
/// Nonblocking epoch cancellation. Pending comparison work retires on drain or
/// the next operation; accepted results and transferred readers are preserved.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_cancel(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(COMPARISONS.get()
        .is_some_and(|sessions| sessions.cancel(handle).is_ok()))).unwrap_or(0)
}
/// Worker-only reclamation. Foreign strings and independently opened readers
/// remain owned by their callers; in-flight calls retain capacity until drain.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_snapshot_comparison_close(handle: u64) -> u8 {
    std::panic::catch_unwind(|| u8::from(COMPARISONS.get()
        .is_some_and(|sessions| sessions.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(test)]
#[path = "snapshot_comparison_ffi_tests.rs"]
mod tests;
