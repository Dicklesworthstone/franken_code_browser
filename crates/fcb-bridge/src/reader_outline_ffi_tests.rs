#![deny(unsafe_op_in_unsafe_fn)]
use super::*;
use crate::reader_ffi::{fcb_reader_create, fcb_reader_open, fcb_reader_info,
    fcb_reader_find, fcb_reader_copy_hit, fcb_reader_close, FFI_TEST_LOCK};
use std::{ffi::{CStr, CString}, fs, sync::atomic::{AtomicU64, Ordering}};

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
    let opened = take(unsafe { fcb_reader_open(handle, cpath.as_ptr(), fcb::search::MAX_SYMBOL_SOURCE_BYTES as u64) });
    assert!(opened.contains("\"status\":\"ok\""), "{opened}"); (path, handle)
}

#[test]
fn c_outline_find_symbol_copy_clear_and_stale_generations_use_real_source() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
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
    let _guard = FFI_TEST_LOCK.lock().unwrap();
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


#[test]
fn c_java_and_csharp_outline_aliases_copy_original_utf16_names_after_source_replacement() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for (alias, language) in [("java", "java"), ("cs", "csharp"), ("c#", "csharp")] {
        let bytes: Vec<_> = "\u{feff}// 😀\nclass Browser { public void open() {} }\n"
            .encode_utf16().flat_map(u16::to_le_bytes).collect();
        let (path, r) = source(&bytes);
        fs::write(&path, b"class Replacement {}").unwrap();
        let alias = CString::new(alias).unwrap();
        let outline = take(unsafe { fcb_reader_outline(r, 1, alias.as_ptr(), 4096) });
        assert!(outline.contains(&format!("\"language\":\"{language}\"")), "{outline}");
        assert!(outline.contains("\"retained_symbols\":\"2\""));
        let needle = CString::new("open").unwrap();
        let page = take(unsafe { fcb_reader_symbols(r, 1, needle.as_ptr(), 0, 0, 128) });
        assert!(page.contains("\"name\":\"open\"") && page.contains("\"symbol_id\":\"2\""), "{page}");
        assert!(take(fcb_reader_copy_symbol(r, 1, 2, 0)).contains("6f00700065006e00"));
        let window = take(fcb_reader_symbol(r, 1, 2, 128));
        assert!(window.contains("class Browser") && !window.contains("Replacement"), "{window}");
        assert_eq!(fcb_reader_close(r), 1);
    }
}

#[test]
fn c_python_outline_ignores_docstrings_and_copies_exact_names_from_retained_source() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    for little in [false, true] {
        let text = "# 😀\r\nclass Browser:\r\n    '''\r\n    def fake(self): pass\r\n    '''\r\n    async def ouvrir(self):\r\n        return 1\r\n";
        let encode = |text: &str| text.encode_utf16().flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect::<Vec<_>>();
        let mut bytes = if little { vec![0xff, 0xfe] } else { vec![0xfe, 0xff] };
        bytes.extend(encode(text));
        let (path, reader) = source(&bytes);
        fs::write(&path, b"class Replacement: pass\n").unwrap();
        let language = CString::new("python").unwrap();
        let outline = take(unsafe { fcb_reader_outline(reader, 1, language.as_ptr(), 4096) });
        assert!(outline.contains("\"retained_symbols\":\"2\""), "{outline}");
        let name = CString::new("ouvrir").unwrap();
        let page = take(unsafe { fcb_reader_symbols(reader, 1, name.as_ptr(), 0, 0, 128) });
        assert!(page.contains("\"symbol_id\":\"2\"") && page.contains("\"name\":\"ouvrir\""), "{page}");
        let expected = encode("ouvrir").iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        let copied = take(fcb_reader_copy_symbol(reader, 1, 2, 0));
        assert!(copied.contains(&expected), "{copied}");
        let body = encode("async def ouvrir(self):\r\n        return 1").iter()
            .map(|byte| format!("{byte:02x}")).collect::<String>();
        let copied_body = take(fcb_reader_copy_symbol(reader, 1, 2, 1));
        assert!(copied_body.contains(&body), "{copied_body}");
        let window = take(fcb_reader_symbol(reader, 1, 2, 128));
        assert!(window.contains("ouvrir") && !window.contains("Replacement"), "{window}");
        assert!(window.contains("\"selection_namespace\":\"outline\""));
        assert_eq!(fcb_reader_close(reader), 1);
    }
}

#[test]
fn c_large_outline_keeps_late_utf16_names_and_owned_copy_after_reader_close() {
    let _guard = FFI_TEST_LOCK.lock().unwrap();
    let prefix = "// 😀 retained context\r\n".repeat(5_000);
    let text = format!("\u{feff}{prefix}fn late_target() {{}}\r\n");
    for little in [true, false] {
        let encode = |value: &str| -> Vec<u8> {
            value.encode_utf16().flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
        };
        let bytes = encode(&text);
        assert!(bytes.len() > 65_536);
        let (path, reader) = source(&bytes);
        fs::write(&path, b"fn replacement() {}\n").unwrap();
        let outline = take(unsafe { fcb_reader_outline(reader, 1, std::ptr::null(), 4096) });
        assert!(outline.contains("\"retained_symbols\":\"1\""), "{outline}");
        assert!(outline.contains("\"declaration_line\":\"5001\""), "{outline}");
        let name = CString::new("late_target").unwrap();
        let page = take(unsafe { fcb_reader_symbols(reader, 1, name.as_ptr(), 0, 0, 128) });
        assert!(page.contains("\"name\":\"late_target\"") && page.contains("\"symbol_id\":\"1\""), "{page}");
        let expected_start = encode(&format!("\u{feff}{prefix}fn ")).len();
        assert!(page.contains(&format!("\"name_range\":{{\"start\":\"{expected_start}\"")), "{page}");
        let copy = fcb_reader_copy_symbol(reader, 1, 1, 0);
        assert_eq!(fcb_reader_close(reader), 1);
        let copied = take(copy);
        let expected = encode("late_target").iter().map(|byte| format!("{byte:02x}")).collect::<String>();
        assert!(copied.contains(&format!("\"original_hex\":\"{expected}\"")), "{copied}");
        assert!(copied.contains("\"additional_source_bytes_read\":\"0\""));
    }
}
