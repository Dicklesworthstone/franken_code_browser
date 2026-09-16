#![forbid(unsafe_code)]
#![cfg(any(target_os = "macos", all(target_os = "linux", any(target_arch = "x86_64", target_arch = "aarch64"))))]

mod support;
use std::{ffi::{OsStr, OsString}, fs, io, os::unix::ffi::OsStrExt, path::PathBuf};
use support::parse;
use fcb_app::{EXIT_OK, EXIT_PARTIAL};

struct Tree(PathBuf);
impl Tree {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("fcb-names-{}-{}", std::process::id(),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir(&path).unwrap(); Self(path)
    }
}
impl Drop for Tree { fn drop(&mut self) { let _ = fs::remove_dir_all(&self.0); } }
fn hex(bytes: &[u8]) -> String { bytes.iter().map(|byte| format!("{byte:02x}")).collect() }

#[test]
fn unix_filename_syntax_and_malformed_labels_survive_recursive_search_and_omission_reporting() {
    let tree = Tree::new();
    let names: [&[u8]; 4] = [b"odd\\folder/file.rs", b"odd/folder/file.rs", b"C:source.rs", b"line\nbad-\xff.rs"];
    for name in names {
        let path = tree.0.join(OsStr::from_bytes(name));
        fs::create_dir_all(path.parent().unwrap()).unwrap(); fs::write(path, b"needle").unwrap();
    }
    for (cap, expected) in [("6", EXIT_OK), ("2", EXIT_PARTIAL)] {
        let args: Vec<OsString> = vec!["search".into(), tree.0.as_os_str().to_owned(), "--workspace".into(),
            "--text".into(), "needle".into(), "--json".into(), "--max-file-bytes".into(), cap.into()];
        let (mut stdout, mut stderr) = (Vec::new(), Vec::new());
        let exit = fcb_app::run(&args, &mut io::empty(), &mut stdout, &mut stderr, || false);
        assert_eq!(exit, expected, "{}", String::from_utf8_lossy(&stdout)); assert!(stderr.is_empty());
        let json = parse(&stdout).unwrap(); assert!(json.get("discovery_complete").flag());
        assert_eq!(json.get("discovery").get("path_limited").number(), 0);
        assert_eq!(json.get("limits").get("max_file_bytes").text(), cap);
        let records = if cap == "6" { json.get("hits").array() } else { json.get("unavailable_files").array() };
        assert_eq!(records.len(), 4);
        let paths: Vec<_> = records.iter().map(|record| record.get("path").get("hex").text()).collect();
        for name in names { assert!(paths.contains(&hex(name).as_str())); }
        for record in records {
            assert!(!record.get("path").get("display").text().chars().any(char::is_control));
            if cap == "2" { assert_eq!(record.get("reason").text(), "WORKSPACE_FILE_BYTE_LIMIT"); }
        }
        assert_eq!(json.get("workspace_complete").flag(), cap == "6");
        if cap == "2" { assert_eq!(json.get("payload_bytes_read").number(), 0); }
    }
}
