#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use crate::reader_ffi::{fcb_reader_create, fcb_reader_open, fcb_reader_info,
    fcb_reader_find, fcb_reader_copy_hit, fcb_reader_close};
use std::{ffi::{CStr, CString}, fs, sync::{Mutex, atomic::{AtomicU64, Ordering}}};
static TEST_LOCK: Mutex<()> = Mutex::new(());

fn take(pointer: *mut c_char) -> String {
    assert!(!pointer.is_null());
    let result = unsafe { CStr::from_ptr(pointer) }.to_str().unwrap().to_owned();
    unsafe { crate::fcb_free_string(pointer) }; result
}
fn source(bytes: &[u8]) -> (std::path::PathBuf, u64) {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let path = std::env::temp_dir().join(format!("fcb-outline-c-{}-{}-{}.rs", std::process::id(),
        std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos(), NEXT.fetch_add(1, Ordering::Relaxed)));
    fs::write(&path, bytes).unwrap(); let cpath = CString::new(path.to_str().unwrap()).unwrap();
    let handle = fcb_reader_create(); assert_ne!(handle, 0);
    let opened = take(unsafe { fcb_reader_open(handle, cpath.as_ptr(), 65536) });
    assert!(opened.contains("\"status\":\"ok\""), "{opened}"); (path, handle)
}

#[test]
fn c_outline_find_symbol_copy_clear_and_stale_generations_use_real_source() {
    let _guard = TEST_LOCK.lock().unwrap();
    let (path, r) = source(b"fn first() {}\nfn second() {}\n");
    fs::write(&path, b"fn replacement() {}\n").unwrap();
    let out = take(unsafe { fcb_reader_outline(r, 1, std::ptr::null(), 4096) });
    assert!(out.contains("\"retained_symbols\":\"2\""), "{out}");
    let name = CString::new("second").unwrap();
    let filtered = take(unsafe { fcb_reader_symbols(r, 1, name.as_ptr(), 0, 0, 128) });
    assert!(filtered.contains("\"symbol_id\":\"2\""));
    assert!(take(fcb_reader_symbol(r, 1, 2, 128)).contains("window_utf8_range"));
    assert!(take(fcb_reader_copy_symbol(r, 1, 2, 0)).contains("7365636f6e64"));
    assert!(take(fcb_reader_copy_symbol(r, 1, 2, 1)).contains("666e207365636f6e64"));
    assert!(take(fcb_reader_copy_symbol(r, 1, 2, 2)).contains("INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_reader_symbols(r, 1, name.as_ptr(), 3, 0, 10) }).contains("INVALID_ARGUMENT"));
    assert!(take(unsafe { fcb_reader_symbols(r, 1, name.as_ptr(), 0, 0, 0) }).contains("INVALID_LIMITS"));
    let language = CString::new("markdown").unwrap();
    assert!(take(unsafe { fcb_reader_outline(r, 2, language.as_ptr(), 4096) }).contains("INVALID_ARGUMENT"));
    assert!(take(fcb_reader_info(r)).contains("\"accepted_outline_generation\":\"1\""));
    take(unsafe { fcb_reader_find(r, 1, name.as_ptr(), 10, 65536) });
    let saved_pointer = fcb_reader_symbol(r, 1, 2, 128);
    take(unsafe { fcb_reader_outline(r, 2, std::ptr::null(), 4096) });
    assert!(take(fcb_reader_symbol(r, 1, 2, 10)).contains("SYMBOL_STALE_QUERY"));
    assert!(take(fcb_reader_outline_clear(r, 3)).contains("\"retained_symbols\":\"0\""));
    assert!(take(fcb_reader_copy_hit(r, 1, 0)).contains("7365636f6e64"));
    assert!(take(fcb_reader_symbol(r, 2, 2, 10)).contains("NO_OUTLINE"));
    assert_eq!(fcb_reader_close(r), 1);
    assert!(take(saved_pointer).contains("fn second() {}"));
    assert!(take(fcb_reader_symbol(r, 2, 2, 10)).contains("READER_HANDLE_UNKNOWN"));
}

#[test]
fn c_utf16_outline_and_lossless_copy_do_not_relabel_original_offsets() {
    let _guard = TEST_LOCK.lock().unwrap();
    let bytes: Vec<u8> = "\u{feff}// 🦀\r\nfn target() {}\r\n".encode_utf16().flat_map(u16::to_le_bytes).collect();
    let (_, r) = source(&bytes);
    let language = CString::new("rust").unwrap();
    let out = take(unsafe { fcb_reader_outline(r, 1, language.as_ptr(), 4096) });
    assert!(out.contains("\"encoding\":\"utf16le\""), "{out}");
    let page = take(unsafe { fcb_reader_symbols(r, 1, std::ptr::null(), 0, 0, 128) });
    assert!(page.contains("\"name\":\"target\""));
    let window = take(fcb_reader_symbol(r, 1, 1, 0));
    assert!(window.contains("\"selection_namespace\":\"outline\""));
    assert!(!window.contains("\"query_generation\":"));
    assert!(take(fcb_reader_copy_symbol(r, 1, 1, 0)).contains("740061007200670065007400"));
    assert_eq!(fcb_reader_close(r), 1);
}

#[test]
fn c_outline_types_modes_and_public_header_declarations_agree() {
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u64) -> *mut c_char = fcb_reader_outline;
    let _: unsafe extern "C" fn(u64, u64, *const c_char, u8, u64, u64) -> *mut c_char = fcb_reader_symbols;
    let _: extern "C" fn(u64, u64, u64, u64) -> *mut c_char = fcb_reader_symbol;
    let _: extern "C" fn(u64, u64, u64, u8) -> *mut c_char = fcb_reader_copy_symbol;
    let _: extern "C" fn(u64, u64) -> *mut c_char = fcb_reader_outline_clear;
    assert_eq!(mode(0).unwrap(), SymbolNameMode::Exact);
    assert_eq!(mode(1).unwrap(), SymbolNameMode::Prefix);
    assert_eq!(mode(2).unwrap(), SymbolNameMode::Contains);
    assert_eq!(mode(255).err(), Some(AccessError::InvalidArgument));
    let header = include_str!("../include/fcb_reader_outline.h");
    for name in ["fcb_reader_outline(", "fcb_reader_symbols(", "fcb_reader_symbol(",
        "fcb_reader_copy_symbol(", "fcb_reader_outline_clear("] {
        assert_eq!(header.matches(name).count(), 1, "{name}");
    }
}
