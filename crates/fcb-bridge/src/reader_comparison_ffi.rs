#![deny(unsafe_op_in_unsafe_fn)]

//! Pointer-free pair-handle marshaling. The registry owns source lifetime and
//! cancellation; the supported reader host and existing analysis engine own
//! all comparison policy. No implicit filesystem or Git operation is present.

use std::ffi::c_char;
use fcb_app::host::{MAX_HOST_TEXT_BYTES, reader::ReaderComparisonLimits};
use super::{answer, number, reply};

/// Compare two different retained reader handles, in caller-selected before
/// and after roles. Generation increases per before handle across attempts.
/// The host invokes this synchronous bounded operation on a worker. Returned
/// JSON is released exactly once using this library's fcb_free_string.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_reader_compare(before: u64, after: u64, generation: u64,
    max_edit_distance: u64, max_work: u64) -> *mut c_char {
    reply(|| answer(before, |readers| readers.compare(before, after, generation,
        ReaderComparisonLimits { max_source_bytes: MAX_HOST_TEXT_BYTES,
            max_edit_distance: number(max_edit_distance)?, max_work }, || false)))
}

#[cfg(all(test, unix))]
#[path = "reader_comparison_ffi_tests.rs"]
mod tests;
