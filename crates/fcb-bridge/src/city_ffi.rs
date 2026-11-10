#![deny(unsafe_op_in_unsafe_fn)]

//! Only borrowed numeric-array and scalar marshaling. Authoritative geometry,
//! transform, admission and picking stay in the safe host/render engines.

use std::{ffi::c_char, sync::OnceLock};
use fcb_app::host::{HostResponse, city::{CityInput, CityViewAction, MAX_CITY_FILES}};
use super::{reply, string_out, city_sessions::{AccessError, CitySessions, Command}};

static CITIES: OnceLock<CitySessions> = OnceLock::new();
fn answer(handle: u64, work: impl FnOnce(&CitySessions) -> Result<HostResponse, AccessError>) -> Option<*mut c_char> {
    let result = match CITIES.get() { Some(sessions) => work(sessions), None => Err(AccessError::Unknown) };
    match result { Ok(response) => string_out(response.as_str()), Err(error) => string_out(&error.json(handle)) }
}
fn prepare(handle: u64, generation: u64, action: CityViewAction) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Prepare { generation, action }, || false)))
}
/// Caller guarantees a valid immutable allocation of the admitted length.
unsafe fn array<'a, T>(pointer: *const T, length: usize) -> Result<&'a [T], AccessError> {
    if length == 0 { return Ok(&[]); }
    if pointer.is_null() || !pointer.is_aligned() { return Err(AccessError::InvalidArgument); }
    // The callers below bound every length to MAX_CITY_FILES * 4 before this
    // helper; lengths and element sizes are far below isize::MAX.
    Ok(unsafe { std::slice::from_raw_parts(pointer, length) })
}

/// Empty reservation, no geometry or I/O. Zero means refusal.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_create() -> u64 {
    std::panic::catch_unwind(|| CITIES.get_or_init(CitySessions::new).create().unwrap_or(0)).unwrap_or(0)
}
/// Retain exactly the current atlas rectangles and their metadata observations.
/// # Safety
/// For nonzero count <= 20,000: ids, observed_bytes and known point to count
/// initialized, aligned elements, rects to count*4 f64s. All allocations remain
/// immutable and valid until return. known values and discovery_complete are
/// 0 or 1. Null arrays are permitted only for count zero. No pointer is retained.
/// Returned non-null strings must be freed with fcb_free_string exactly once.
#[unsafe(no_mangle)]
pub unsafe extern "C" fn fcb_city_open(handle: u64, layout: u64, ids: *const u64,
    rects: *const f64, observed_bytes: *const u64, known: *const u8,
    count: u64, discovery_complete: u32) -> *mut c_char {
    reply(|| answer(handle, |sessions| {
        let count = usize::try_from(count).map_err(|_| AccessError::InvalidArgument)?;
        if count > MAX_CITY_FILES || discovery_complete > 1 { return Err(AccessError::InvalidArgument); }
        let input = CityInput {
            ids: unsafe { array(ids, count) }?, rects: unsafe { array(rects, count * 4) }?,
            observed_bytes: unsafe { array(observed_bytes, count) }?, known: unsafe { array(known, count) }?,
        };
        sessions.open(handle, layout, input, discovery_complete == 1, || false)
    }))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_info(handle: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Info, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_view(handle: u64, generation: u64, width: f64, height: f64,
    center_x: f64, center_y: f64, scale: f64, yaw: f64, tilt: f64) -> *mut c_char {
    prepare(handle, generation, CityViewAction::View { width, height, center_x, center_y, scale, yaw, tilt })
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_fit(handle: u64, generation: u64, width: f64, height: f64) -> *mut c_char {
    prepare(handle, generation, CityViewAction::Fit { width, height })
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_pan(handle: u64, generation: u64, base: u64, dx: f64, dy: f64) -> *mut c_char {
    prepare(handle, generation, CityViewAction::Pan { base, dx, dy })
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_zoom(handle: u64, generation: u64, base: u64, x: f64, y: f64, factor: f64) -> *mut c_char {
    prepare(handle, generation, CityViewAction::Zoom { base, x, y, factor })
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_orbit(handle: u64, generation: u64, base: u64, yaw: f64, tilt: f64) -> *mut c_char {
    prepare(handle, generation, CityViewAction::Orbit { base, yaw, tilt })
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_resize(handle: u64, generation: u64, base: u64, width: f64, height: f64) -> *mut c_char {
    prepare(handle, generation, CityViewAction::Resize { base, width, height })
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_present(handle: u64, generation: u64, frame: u64, display: u64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle,
        Command::Present { generation, frame, display }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_pick(handle: u64, frame: u64, display: u64, x: f64, y: f64) -> *mut c_char {
    reply(|| answer(handle, |sessions| sessions.execute(handle, Command::Pick { frame, display, x, y }, || false)))
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_cancel(handle: u64) -> u32 {
    std::panic::catch_unwind(|| u32::from(CITIES.get().is_some_and(|s| s.cancel(handle).is_ok()))).unwrap_or(0)
}
#[unsafe(no_mangle)]
pub extern "C" fn fcb_city_close(handle: u64) -> u32 {
    std::panic::catch_unwind(|| u32::from(CITIES.get().is_some_and(|s| s.close(handle).is_ok()))).unwrap_or(0)
}

#[cfg(test)]
#[path = "city_ffi_tests.rs"]
mod tests;
