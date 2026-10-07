#![deny(unsafe_op_in_unsafe_fn)]

//! Source transfer from a retained index and interruptible one-file indexing.
use super::*;

/// Copy the named indexed source into an EMPTY existing reader. This is NOT a
/// search-hit selection; subsequent query generations may differ. Use the
/// reader's own checked selection APIs after validating the returned capture.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_index_open_reader(handle: u64, reader: u64,
    index_generation: u64, file: u64, source_revision: u64) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let readers = crate::reader_ffi::registry().ok_or(crate::reader_sessions::AccessError::UnknownHandle)?;
        atlases.open_index_reader(handle, readers, reader, index_generation, file, source_revision, || false)
    }))
}

/// Cooperative polling inside capture/index construction, not just between
/// members. No callback is retained after return; a syscall cannot be aborted.
/// # Safety
/// poll/context remain valid until return. poll must not unwind or reenter this
/// atlas. Free returned strings once with fcb_free_string; inspect JSON status.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_atlas_index_step_cancelable(handle: u64, generation: u64,
    poll: Option<crate::search_ffi::SearchCancellationCallback>, context: *mut std::ffi::c_void) -> *mut c_char {
    reply(|| answer(handle, |atlases| {
        let mut canceled = || poll.is_some_and(|poll| unsafe { poll(context) != 0 });
        if canceled() { return Err(AccessError::Canceled); }
        atlases.execute_index(handle, IndexCommand::PrepareStep { generation }, &mut canceled)
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_and_cancelable_step_signatures_match_public_header() {
        let _: extern "C" fn(u64, u64, u64, u64, u64) -> *mut c_char = fcb_atlas_index_open_reader;
        let _: unsafe extern "C" fn(u64, u64, Option<crate::search_ffi::SearchCancellationCallback>,
            *mut std::ffi::c_void) -> *mut c_char = fcb_atlas_index_step_cancelable;
        let header = include_str!("../include/fcb_atlas_index_source.h");
        for name in ["fcb_atlas_index_open_reader(", "fcb_atlas_index_step_cancelable("] {
            assert_eq!(header.matches(name).count(), 1);
        }
    }
}
