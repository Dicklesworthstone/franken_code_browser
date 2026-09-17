//! C ABI bridge for host application shells (SwiftUI) over the real fcb
//! engine.
//!
//! Thin marshaling only. Search and file reads route through
//! [`fcb_app::run`] — the same command routing the `fcb` CLI uses — and
//! atlas layout routes through `fcb-map`'s partition engine, so a native
//! shell, the terminal, and the planned Metal renderer share one
//! implementation, one policy, and one JSON contract. No parsing, policy,
//! or caching lives here.
//!
//! FFI safety: the `unsafe` surface is exactly four sites, each limited
//! to C-string marshaling at the ABI boundary. Callers own returned
//! strings and release them with [`fcb_free_string`].

use std::ffi::{c_char, CStr, CString, OsString};

use fcb_core::{ArenaOwnerId, RootId};
use fcb_map::NodeKind;
use fcb_map::{commit_layout, HierarchySpec, LayoutOptions, NodeSpec};

/// The longest file body the bridge will hand a shell. The engine's own
/// bounded-read policy stays authoritative; this only caps host memory.
const MAX_READ_BYTES: usize = 4 << 20;

/// Atlas walk bounds: workspaces above these are refused with a note
/// rather than silently truncated (the shell surfaces the refusal).
const ATLAS_MAX_FILES: usize = 20_000;
const ATLAS_MAX_DEPTH: u16 = 14;
const ATLAS_MAX_LINES: usize = 4000;

fn is_ignored_dir(name: &[u8]) -> bool {
    matches!(
        name,
        b".git" | b"target" | b"node_modules" | b".build" | b"dist" | b".venv"
            | b"__pycache__" | b".beads" | b".idea" | b".vscode"
    )
}

struct WalkEntry {
    relative: Vec<u8>,
    is_dir: bool,
    bytes: u64,
}

fn walk(
    absolute: &std::path::Path,
    prefix: &str,
    depth: u16,
    out: &mut Vec<WalkEntry>,
) -> Result<(), String> {
    if depth > ATLAS_MAX_DEPTH {
        return Err(format!(
            "directory tree deeper than {ATLAS_MAX_DEPTH} levels: {prefix}"
        ));
    }
    let mut entries: Vec<_> = std::fs::read_dir(absolute)
        .map_err(|error| format!("{}: {error}", absolute.display()))?
        .filter_map(|entry| entry.ok())
        .collect();
    entries.sort_by_key(|entry| entry.file_name());
    for entry in entries {
        let name = entry.file_name();
        let name_bytes = name.as_encoded_bytes().to_vec();
        let name_str = String::from_utf8_lossy(&name_bytes).into_owned();
        let child_absolute = entry.path();
        let relative = if prefix.is_empty() {
            name_str.clone()
        } else {
            format!("{prefix}/{name_str}")
        };
        let metadata = entry
            .metadata()
            .map_err(|error| format!("{}: {error}", child_absolute.display()))?;
        if metadata.is_dir() {
            if is_ignored_dir(&name_bytes) {
                continue;
            }
            let marker = out.len();
            out.push(WalkEntry {
                relative: relative.clone().into_bytes(),
                is_dir: true,
                bytes: 0,
            });
            walk(&child_absolute, &relative, depth + 1, out)?;
            // Directory weight aggregates its subtree so parent partitions
            // size truthfully.
            let subtree: u64 = out[marker + 1..].iter().map(|entry| entry.bytes).sum();
            out[marker].bytes = subtree;
            if out.iter().filter(|entry| !entry.is_dir).count() > ATLAS_MAX_FILES {
                return Err(format!(
                    "workspace has more than {ATLAS_MAX_FILES} files; scope the root tighter"
                ));
            }
        } else if metadata.is_file() {
            out.push(WalkEntry {
                relative: relative.into_bytes(),
                is_dir: false,
                bytes: metadata.len(),
            });
        }
    }
    Ok(())
}

/// Packed per-line profile: [length_frac, class] × lines.
/// class: 0 code, 1 comment, 2 string, 3 keyword.
fn line_profile(text: &str) -> Vec<u8> {
    let mut profile = Vec::with_capacity(text.lines().count() * 2);
    let max_len = text.lines().map(|line| line.len()).max().unwrap_or(1).max(1);
    for line in text.lines().take(ATLAS_MAX_LINES) {
        let trimmed = line.trim_start();
        let class = if trimmed.starts_with("//") || trimmed.starts_with('#') {
            1_u8
        } else if line.contains('"') || line.contains('\'') {
            2
        } else {
            let keywordish = [
                "fn ", "func ", "def ", "class ", "struct ", "impl ", "pub ", "if ", "for ",
                "while ", "return ",
            ]
            .iter()
            .any(|marker| line.contains(marker));
            if keywordish { 3 } else { 0 }
        };
        let length_frac = ((line.len().min(max_len) * 255) / max_len) as u8;
        profile.push(length_frac);
        profile.push(class);
    }
    profile
}

fn base64(data: &[u8]) -> String {
    const TABLE: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::with_capacity(data.len().div_ceil(3) * 4);
    for chunk in data.chunks(3) {
        let bytes = [
            chunk.first().copied().unwrap_or(0),
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let n = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        out.push(TABLE[(n >> 18) as usize & 63] as char);
        out.push(TABLE[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 {
            TABLE[(n >> 6) as usize & 63] as char
        } else {
            '='
        });
        out.push(if chunk.len() > 2 {
            TABLE[n as usize & 63] as char
        } else {
            '='
        });
    }
    out
}

fn json_escape(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for character in value.chars() {
        match character {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            character if (character as u32) < 0x20 => {
                escaped.push_str(&format!("\\u{:04x}", character as u32))
            }
            character => escaped.push(character),
        }
    }
    escaped
}

fn atlas_json(root_path: &str) -> Option<String> {
    let mut entries = Vec::new();
    walk(std::path::Path::new(root_path), "", 0, &mut entries).ok()?;
    if entries.is_empty() {
        return None;
    }
    let owner = ArenaOwnerId::new(1).ok()?;
    let root_id = RootId::new(owner, 1).ok()?;
    let revision = fcb_map::LayoutRevision::new(owner, 1).ok()?;
    // Overview weight = one file, one tile. Byte-weighting lets a few
    // huge generated files swallow the world and push 19k sources into
    // sub-pixel slivers; the uniform weight makes every file visibly
    // present, which is the atlas contract. Directories aggregate their
    // descendant file counts (walk already sums `bytes` that way).
    let nodes: Vec<NodeSpec> = entries
        .iter()
        .map(|entry| {
            NodeSpec::new(
                entry.relative.clone(),
                if entry.is_dir {
                    NodeKind::Directory
                } else {
                    NodeKind::File
                },
                Some(if entry.is_dir { entry.bytes } else { 1 }),
            )
        })
        .collect();
    let spec = HierarchySpec::new(owner, root_id, nodes).ok()?;
    let layout = commit_layout(
        revision,
        fcb_map::Size2D::new(4096.0, 4096.0).ok()?,
        &spec,
        LayoutOptions::modest(),
    )
    .ok()?;

    let bytes_by_path: std::collections::HashMap<Vec<u8>, u64> = entries
        .iter()
        .map(|entry| (entry.relative.clone(), entry.bytes))
        .collect();

    // LaidOutNode rects are PARENT-LOCAL: each file's rect is relative to
    // its parent directory's rect. Compose world coordinates by walking
    // nodes in path-depth order, carrying each directory's world origin.
    let mut nodes: Vec<&fcb_map::LaidOutNode> = layout.nodes().iter().collect();
    nodes.sort_by_key(|node| node.path().iter().filter(|&&byte| byte == b'/').count());

    let mut dir_world_origin: std::collections::HashMap<Vec<u8>, (f64, f64)> =
        std::collections::HashMap::new();
    let mut out = String::from("{\"world\":{\"w\":4096,\"h\":4096},\"files\":[");
    let mut first = true;
    for node in nodes {
        let depth = node.path().iter().filter(|&&byte| byte == b'/').count();
        let rect = node.parent_local();
        let (origin_x, origin_y) = if depth == 0 {
            (rect.min_x(), rect.min_y())
        } else {
            let split = node
                .path()
                .iter()
                .rposition(|&byte| byte == b'/')
                .expect("depth > 0 implies a separator");
            let parent_path = &node.path()[..split];
            dir_world_origin
                .get(parent_path)
                .map(|(x, y)| (x + rect.min_x(), y + rect.min_y()))
                .unwrap_or((rect.min_x(), rect.min_y()))
        };
        match node.kind() {
            NodeKind::Directory => {
                dir_world_origin.insert(node.path().to_vec(), (origin_x, origin_y));
            }
            NodeKind::File => {
                let path = String::from_utf8_lossy(node.path());
                if !first {
                    out.push(',');
                }
                first = false;
                // Per-line profile (length+class per line, base64-packed)
                // so the shell can draw the line-bar circuit texture
                // without re-reading files.
                let profile = std::fs::read_to_string(root_path.to_owned() + "/" + &path)
                    .map(|text| line_profile(&text))
                    .unwrap_or_default();
                let (line_count, tex) = (profile.len() / 2, base64(&profile));
                out.push_str(&format!(
                    "{{\"path\":\"{}\",\"x\":{:.2},\"y\":{:.2},\"w\":{:.2},\"h\":{:.2},\"bytes\":{},\"n\":{},\"tex\":\"{}\"}}",
                    json_escape(&path),
                    origin_x,
                    origin_y,
                    rect.max_x() - rect.min_x(),
                    rect.max_y() - rect.min_y(),
                    bytes_by_path.get(path.as_bytes()).copied().unwrap_or(0),
                    line_count,
                    tex
                ));
            }
            NodeKind::Placeholder => {}
        }
    }
    out.push_str("]}");
    Some(out)
}

/// Reads a C string at the ABI boundary; null or non-UTF-8 yields None.
unsafe fn cstr<'a>(pointer: *const c_char) -> Option<&'a str> {
    if pointer.is_null() {
        return None;
    }
    unsafe { CStr::from_ptr(pointer) }.to_str().ok()
}

/// Marshals an owned result into a caller-owned C string.
fn string_out(value: Option<String>) -> *mut c_char {
    match value.and_then(|text| CString::new(text.replace('\0', "")).ok()) {
        Some(c_string) => c_string.into_raw(),
        None => std::ptr::null_mut(),
    }
}

fn run_capture(arguments: &[&str]) -> Option<String> {
    let args: Vec<OsString> = arguments.iter().map(OsString::from).collect();
    let mut stdout = Vec::new();
    let mut stderr = Vec::new();
    let mut empty: &[u8] = &[];
    let code = fcb_app::run(&args, &mut empty, &mut stdout, &mut stderr, || false);
    if !stdout.is_empty() {
        return String::from_utf8(stdout).ok();
    }
    if code == 0 || code == 1 {
        // 0 = complete, 1 = complete-without-match: both are valid empty
        // answers for a shell. Anything else carries diagnostics.
        return Some(String::new());
    }
    String::from_utf8(stderr).ok()
}

/// Searches a workspace for exact text matches. Returns the engine's own
/// JSON response (`fcb search ROOT --workspace --text Q --json`).
///
/// # Safety
/// `root` and `query` must be valid NUL-terminated UTF-8 C strings, or
/// null (null yields an empty reply). The returned string is released by
/// the caller through [`fcb_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn fcb_search_workspace(root: *const c_char, query: *const c_char) -> *mut c_char {
    let reply = (|| {
        let root = unsafe { cstr(root) }?;
        let query = unsafe { cstr(query) }?;
        run_capture(&["search", root, "--workspace", "--text", query, "--json"])
    })();
    string_out(reply)
}

/// Reads one named file as lossy UTF-8 text, capped at 4 MiB.
///
/// # Safety
/// `path` must be a valid NUL-terminated UTF-8 C string, or null. The
/// returned string is released by the caller through [`fcb_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn fcb_read_file(path: *const c_char) -> *mut c_char {
    let reply = (|| {
        let path = unsafe { cstr(path) }?;
        let bytes = std::fs::read(path).ok()?;
        let capped = &bytes[..bytes.len().min(MAX_READ_BYTES)];
        Some(String::from_utf8_lossy(capped).into_owned())
    })();
    string_out(reply)
}

/// Lays out every file under `root` with the fcb-map partition engine and
/// returns JSON: {"world":{"w":W,"h":H},"files":[{"path","x","y","w",
/// "h","bytes","n","tex"},...]}, where `n` is the line count and `tex`
/// the base64 per-line profile (2 bytes per line: length fraction and
/// role class). Null on refusal (empty root, depth/file caps, unreadable
/// entries) so the shell can surface the boundary honestly.
///
/// # Safety
/// `root` must be a valid NUL-terminated UTF-8 C string, or null. The
/// returned string is released by the caller through [`fcb_free_string`].
#[unsafe(no_mangle)]
pub extern "C" fn fcb_atlas_layout(root: *const c_char) -> *mut c_char {
    let reply = (|| {
        let root = unsafe { cstr(root) }?;
        atlas_json(root)
    })();
    string_out(reply)
}

/// Releases a string previously returned by this bridge.
///
/// # Safety
/// `pointer` must be null or a string returned by this bridge that has
/// not already been released.
#[unsafe(no_mangle)]
pub extern "C" fn fcb_free_string(pointer: *mut c_char) {
    if !pointer.is_null() {
        drop(unsafe { CString::from_raw(pointer) });
    }
}
