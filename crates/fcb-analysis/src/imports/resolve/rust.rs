#![forbid(unsafe_code)]

//! Conventional Rust module candidates over one immutable catalog.
//!
//! This is navigation policy, not rustc name resolution. No Cargo configuration,
//! module declarations in other files, editions, cfg, macro expansion, reexports,
//! or custom target/#[path] layouts are inferred. The extractor alone decides
//! whether the statement's lexical context permits this policy. Its flattened
//! path is data; this module does not tokenize source again.
//!
//! A unique `lib.rs` or `main.rs` in the nearest ancestor directory supplies a
//! conventional crate-root candidate. Both are ambiguous, even when a catalog
//! aliases their identities. An explicit source named lib.rs/main.rs selects
//! itself. Ancestor search stops at common automatic-target directories rather
//! than attaching another binary/test/example to an outer library. These rules
//! still do not establish the source's compiler crate membership.

use super::{CatalogPath, ImportFact, ImportResolution, ImportResolutionKind,
    MAX_IMPORT_RESOLUTION_PATH_BYTES, candidates};
use super::super::MAX_IMPORT_SPECIFIER_BYTES;

const MAX_ROOT_ANCESTORS: usize = 64;
const MAX_PATH_COMPONENTS: usize = 128;
const EXACT: &str = "rust-use-module-file-candidate";
const PREFIX: &str = "rust-use-module-prefix-candidate";

#[derive(Clone, Copy)]
struct Root<T> { directory_len: usize, target: T }

fn unresolved<T>() -> ImportResolution<T> {
    ImportResolution::without(ImportResolutionKind::Unresolved, "unresolved-rust-use")
}

/// Qualifiers and component counts are validated before consulting the catalog.
/// Each component then admits only the complete .rs / /mod.rs pair. This bounds
/// lookups by two per component plus two per root ancestor, with no allocation.
pub(super) fn resolve_use<T: Copy + Eq>(
    source: &[u8], fact: &ImportFact, lookup: &mut impl FnMut(&[u8]) -> Option<T>,
) -> ImportResolution<T> {
    let destination = fact.destination();
    if !fact.resolution_allowed() || destination.is_empty()
        || destination.len() > MAX_IMPORT_SPECIFIER_BYTES {
        return unresolved();
    }
    let file = source.rsplit(|&byte| byte == b'/').next().unwrap_or_default();
    let Some(stem) = file.strip_suffix(b".rs") else { return unresolved(); };
    if stem.is_empty() { return unresolved(); }

    let mut parts = [""; MAX_PATH_COMPONENTS];
    let mut count = 0usize;
    for part in destination.split("::") {
        if part.is_empty() || count == parts.len() { return unresolved(); }
        parts[count] = part;
        count += 1;
    }
    // A glob authorizes visiting its module candidate, not invented individual
    // imported names. The parser retains the literal '*' evidence separately.
    if parts[count - 1] == "*" { count -= 1; }
    if count == 0 { return unresolved(); }
    let from_crate = parts[0] == "crate";
    let mut first = match parts[0] {
        "crate" | "self" => 1,
        "super" => 0,
        _ => return unresolved(), // bare names and ::extern depend on edition/binding
    };
    let mut parents = 0usize;
    if !from_crate {
        while first < count && parts[first] == "super" {
            parents += 1;
            first += 1;
        }
    }
    for part in &mut parts[first..count] {
        let spelling = *part;
        let name = spelling.strip_prefix("r#").unwrap_or(spelling);
        // Rust filesystem module names have an ASCII-only conventional route.
        // Unicode facts remain exact, but their NFC spelling is not guessed.
        if !ascii_module_name(name) { return unresolved(); }
        *part = name;
    }

    let Some(mut path) = CatalogPath::parent(source) else { return unresolved(); };
    let root = match root(source, file, &mut path, lookup) {
        Ok(root) => root,
        Err(reason) => return reason,
    };
    // root() restores the source directory before returning. A flat module
    // file owns children under its stem; mod.rs owns children beside itself.
    if from_crate {
        path.len = root.directory_len;
    } else if !matches!(file, b"lib.rs" | b"main.rs" | b"mod.rs") && !path.push(stem) {
        return unresolved();
    }
    if !from_crate && file == b"mod.rs" && path.len == root.directory_len {
        // A mod.rs next to the inferred root is not evidence of how that file
        // was installed in a module tree (it may itself be a custom root).
        return unresolved();
    }
    for _ in 0..parents {
        if path.len <= root.directory_len { return unresolved(); }
        path.len = path.bytes().iter().rposition(|&byte| byte == b'/').unwrap_or(0);
        if path.len < root.directory_len { return unresolved(); }
    }

    // self/super name a containing module independently of the trailing item.
    // Its file may be useful even when an imported item has no file of its own.
    // crate::Unknown deliberately does NOT fall back to the crate root: no
    // named module prefix has been established for that unrecognized path.
    let mut containing = if from_crate {
        None
    } else if parents == 0 {
        lookup(source)
    } else if path.len == root.directory_len {
        Some(root.target)
    } else {
        let candidate = candidates(&mut path, &[b".rs", b"/mod.rs"], EXACT, lookup);
        match candidate.kind {
            ImportResolutionKind::Local => candidate.target,
            ImportResolutionKind::Missing => None,
            _ => return candidate,
        }
    };
    if first == count {
        if from_crate { containing = Some(root.target); }
        return match containing {
            Some(target) => ImportResolution::local(target, EXACT),
            None => ImportResolution::without(ImportResolutionKind::Missing, "no-catalog-target"),
        };
    }
    for part in &parts[first..count] {
        if !path.push(part.as_bytes()) { return unresolved(); }
        let candidate = candidates(&mut path, &[b".rs", b"/mod.rs"], EXACT, lookup);
        match candidate.kind {
            ImportResolutionKind::Local => containing = candidate.target,
            ImportResolutionKind::Missing => {
                return containing.map_or(candidate, |target| ImportResolution::local(target, PREFIX));
            }
            // Unprobeable paths and ambiguous intermediate modules cannot be
            // hidden by a previously found prefix or a more specific filename.
            _ => return candidate,
        }
    }
    match containing {
        Some(target) => ImportResolution::local(target, EXACT),
        None => ImportResolution::without(ImportResolutionKind::Missing, "no-catalog-target"),
    }
}

fn ascii_module_name(name: &str) -> bool {
    if matches!(name, "_" | "crate" | "self" | "super" | "Self") { return false; }
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
        && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
}

fn root<T: Copy + Eq>(source: &[u8], file: &[u8], path: &mut CatalogPath,
    lookup: &mut impl FnMut(&[u8]) -> Option<T>) -> Result<Root<T>, ImportResolution<T>> {
    let original_len = path.len;
    if matches!(file, b"lib.rs" | b"main.rs") {
        return lookup(source).map(|target| Root { directory_len: original_len, target })
            .ok_or_else(unresolved);
    }
    let result = root_ancestors(path, lookup);
    path.len = original_len;
    // Probing overwrites bytes after each ancestor, so restore the entire
    // original source directory, not just its length.
    path.data[..original_len].copy_from_slice(&source[..original_len]);
    result
}

fn root_ancestors<T: Copy + Eq>(path: &mut CatalogPath,
    lookup: &mut impl FnMut(&[u8]) -> Option<T>) -> Result<Root<T>, ImportResolution<T>> {
    for _ in 0..MAX_ROOT_ANCESTORS {
        if matches!(path.file_name(), b"bin" | b"tests" | b"examples" | b"benches") {
            return Err(unresolved());
        }
        let base = path.len;
        // Both probes must fit before either is consulted. Missing is meaningful
        // only after the complete admitted candidate set has been examined.
        let separator = usize::from(base > 0);
        if b"main.rs".len() + separator > MAX_IMPORT_RESOLUTION_PATH_BYTES - base {
            return Err(unresolved());
        }
        let mut found = None;
        for name in [b"lib.rs".as_slice(), b"main.rs".as_slice()] {
            if !path.push(name) { return Err(unresolved()); }
            let candidate = lookup(path.bytes());
            path.len = base;
            if let Some(target) = candidate {
                if found.is_some() {
                    return Err(ImportResolution::without(ImportResolutionKind::Ambiguous,
                        "ambiguous-rust-crate-root"));
                }
                found = Some(target);
            }
        }
        if let Some(target) = found { return Ok(Root { directory_len: base, target }); }
        if base == 0 { return Err(unresolved()); }
        path.len = path.bytes().iter().rposition(|&byte| byte == b'/').unwrap_or(0);
    }
    Err(unresolved())
}
