use std::{io::Write, path::PathBuf, sync::atomic::{AtomicU64, Ordering}};
use fcb_app::host::{source_document, HostError};
static NEXT: AtomicU64 = AtomicU64::new(1);
fn file(bytes: &[u8], extension: &str) -> PathBuf {
    let path = std::env::temp_dir().join(format!("fcb-source-document-{}-{}.{}", std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed), extension));
    let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(&path).unwrap();
    file.write_all(bytes).unwrap(); path
}
#[test]
fn same_observation_contains_exact_source_and_real_roles() {
    let path = file("// 🦀\nfn main() { \"hi\"; }\n".as_bytes(), "rs");
    let output = source_document::read(&path, || false).unwrap();
    assert_eq!(output.exit_code(), 0);
    assert!(output.as_str().contains("\"schema\":\"fcb.source-document/1\""));
    assert!(output.as_str().contains("\"text\":\"// 🦀\\nfn main() { \\\"hi\\\"; }\\n\""));
    assert!(output.as_str().contains("\"role\":\"comment\""));
    assert!(output.as_str().contains("{\"start\":\"6\",\"length\":\"2\",\"role\":\"keyword\"}"));
    assert!(output.as_str().contains("\"role\":\"string\""));
}
#[test]
fn empty_source_is_success_and_errors_never_become_empty_source() {
    let path = file(b"", "rs");
    let output = source_document::read(&path, || false).unwrap();
    assert!(output.as_str().contains("\"text\":\"\""));
    assert!(output.as_str().contains("\"runs\":[]"));
    assert!(matches!(source_document::read(&file(&[255], "rs"), || false), Err(HostError::InvalidUtf8)));
    assert!(matches!(source_document::read(&file(b"x\0y", "rs"), || false), Err(HostError::EmbeddedNul)));
    assert!(source_document::read(&path.join("missing"), || false).is_err());
    assert!(source_document::read(&path, || true).is_err());
}
