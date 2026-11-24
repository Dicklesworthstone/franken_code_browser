#![forbid(unsafe_code)]

//! Finite absolute Python filename candidates in the caller's closed catalog.
//!
//! The catalog root and its conventional src/ directory are always considered.
//! One additional root may be established by consecutive __init__.py entries
//! enclosing the importing source. A missing marker ends that chain; this does
//! not infer a namespace-package root or an execution/search-path configuration.
//! Every root has equal standing. Existing .py and /__init__.py candidates are
//! compared by exact path, never by interpreter priority or aliased file ID.
//! Target package declarations and import execution remain unmodeled: a found
//! filename is navigation evidence, not proof that Python would load that file.

use super::{CatalogPath, ImportResolution, ImportResolutionKind,
    MAX_IMPORT_RESOLUTION_PATH_BYTES};
use super::super::MAX_IMPORT_SPECIFIER_BYTES;

const MAX_COMPONENTS: usize = 64;
const SUFFIXES: [&[u8]; 2] = [b".py", b"/__init__.py"];
const BASIS: &str = "python-absolute-module-candidate";

fn unsupported<T>() -> ImportResolution<T> {
    ImportResolution::without(ImportResolutionKind::Unresolved, "unsupported-specifier")
}

pub(super) fn resolve_absolute<T: Copy + Eq>(source: &[u8], destination: &str,
    lookup: &mut impl FnMut(&[u8]) -> Option<T>) -> ImportResolution<T> {
    // resolve_import has already validated the canonical source path, Python
    // language, allowed evidence and ASCII dotted-name grammar. Keep independent
    // finite work bounds here before any catalog callback.
    let directory_len = source.iter().rposition(|&byte| byte == b'/').unwrap_or(0);
    let parent = &source[..directory_len];
    if destination.len() > MAX_IMPORT_SPECIFIER_BYTES
        || destination.split('.').count() > MAX_COMPONENTS
        || (!parent.is_empty() && parent.split(|&byte| byte == b'/').count() > MAX_COMPONENTS) {
        return unsupported();
    }
    // Preflight the longest possible inferred root and the conventional src
    // root BEFORE checking markers or candidates. Replacing dots by slashes
    // preserves destination length. The longest suffix also bounds every
    // enclosing __init__.py probe. A skipped oversized root could otherwise
    // turn competing candidates into a false unique target.
    if directory_len.max(b"src".len()).checked_add(1)
        .and_then(|n| n.checked_add(destination.len()))
        .and_then(|n| n.checked_add(b"/__init__.py".len()))
        .is_none_or(|n| n > MAX_IMPORT_RESOLUTION_PATH_BYTES) {
        return unsupported();
    }
    let Some(mut path) = CatalogPath::parent(source) else { return unsupported(); };
    let mut enclosing = parent;
    let mut package_root = None;
    while !enclosing.is_empty() {
        path.len = enclosing.len();
        path.data[..path.len].copy_from_slice(enclosing);
        if !path.push(b"__init__.py") { return unsupported(); }
        if lookup(path.bytes()).is_none() { break; }
        let next = enclosing.iter().rposition(|&byte| byte == b'/').unwrap_or(0);
        enclosing = &enclosing[..next];
        package_root = Some(enclosing);
    }

    // At most 64 marker lookups and six candidate lookups. Deduplicate roots
    // before looking up targets, including an inferred catalog/src root.
    let roots = [b"".as_slice(), b"src".as_slice(), package_root.unwrap_or(b"")];
    let mut found = None;
    for (ordinal, root) in roots.iter().enumerate() {
        if roots[..ordinal].contains(root) { continue; }
        path.len = 0;
        if !root.is_empty() && !path.push(root) { return unsupported(); }
        for component in destination.split('.') {
            if !path.push(component.as_bytes()) { return unsupported(); }
        }
        let base = path.len;
        for suffix in SUFFIXES {
            path.data[base..base + suffix.len()].copy_from_slice(suffix);
            path.len = base + suffix.len();
            if let Some(target) = lookup(path.bytes()) {
                // Different roots/suffixes name distinct paths even when the
                // catalog maps both paths to the same underlying file identity.
                if found.is_some() {
                    return ImportResolution::without(ImportResolutionKind::Ambiguous,
                        "ambiguous-catalog-target");
                }
                found = Some(target);
            }
        }
    }
    match found {
        Some(target) => ImportResolution::local(target, BASIS),
        None => ImportResolution::without(ImportResolutionKind::External, "external-specifier"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::{ImportFact, ImportKind, resolve_import};
    use fcb_core::{ByteOffset, ByteRange};

    fn fact(kind: ImportKind, destination: &str) -> ImportFact {
        let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(1)).unwrap();
        ImportFact { id: 1, kind, destination: destination.to_owned(), original_range: range,
            specifier_range: range, resolution_allowed: true, rust_module_context: None }
    }
    fn resolve(source: &[u8], destination: &str, paths: &[&[u8]]) -> ImportResolution<usize> {
        resolve_import(source, &fact(ImportKind::PythonImport, destination),
            |path| paths.iter().position(|candidate| *candidate == path))
    }

    #[test]
    fn conventional_roots_admit_exact_module_and_package_filenames() {
        for target in [b"tools.py".as_slice(), b"tools/__init__.py", b"src/tools.py", b"src/tools/__init__.py"] {
            let result = resolve(b"cli.py", "tools", &[target]);
            assert_eq!(result.target, Some(0), "{target:?}");
            assert_eq!(result.basis, BASIS);
        }
        for target in [b"acme/tools.py".as_slice(), b"src/acme/tools/__init__.py"] {
            let result = resolve(b"cli.pyi", "acme.tools", &[target]);
            assert_eq!(result.target, Some(0));
            // This is a filename candidate, not a claim about regular versus
            // namespace-package binding of its intermediate components.
            assert_eq!(result.basis, BASIS);
        }
        for target in [b"src/Tools.py".as_slice(), b"src/tools.pyi", b"custom/tools.py",
            b"tools/Member.py"] {
            assert_eq!(resolve(b"cli.py", "tools", &[target]).kind, ImportResolutionKind::External);
        }
    }

    #[test]
    fn enclosing_regular_package_roots_use_only_consecutive_catalogued_markers() {
        let source = b"projects/\xff/acme/feature/main.py";
        let paths: &[&[u8]] = &[b"projects/\xff/acme/feature/__init__.py", b"projects/\xff/acme/__init__.py",
            b"projects/\xff/acme/util.py"];
        let result = resolve(source, "acme.util", paths);
        assert_eq!(result.target, Some(2)); assert_eq!(result.basis, BASIS);
        assert_eq!(resolve(source, "acme.util", &paths[1..]).kind, ImportResolutionKind::External,
            "a missing nearest marker cannot be skipped to guess an outer root");
        assert_eq!(resolve(source, "acme.util", &[paths[0], paths[2]]).kind, ImportResolutionKind::External,
            "the first missing parent ends package ancestry");
        assert_eq!(resolve(b"projects/\xff/acme/__init__.py", "acme.util", &paths[1..]).target,
            Some(1), "package initializer sources use the same ancestry policy");
        assert_eq!(resolve(b"projects/\xff/acme/main.py", "acme.util",
            &[b"projects/\xff/acme/__init__.pyi", paths[2]]).kind, ImportResolutionKind::External,
            "stub markers do not establish a regular package root");
    }

    #[test]
    fn roots_and_suffixes_never_gain_a_priority_or_alias_identity_preference() {
        for targets in [vec![b"tools.py".as_slice(), b"src/tools.py"],
            vec![b"tools.py".as_slice(), b"tools/__init__.py"],
            vec![b"projects/app/acme/__init__.py".as_slice(), b"tools.py", b"projects/app/tools.py"]] {
            let result = resolve_import(b"projects/app/acme/main.py", &fact(ImportKind::PythonFrom, "tools"),
                |path| targets.contains(&path).then_some(7));
            assert_eq!(result.kind, ImportResolutionKind::Ambiguous);
            assert_eq!(result.target, None);
        }
        let mut calls = Vec::new();
        let result = resolve_import(b"src/acme/main.py", &fact(ImportKind::PythonImport, "tools"), |path| {
            calls.push(path.to_vec());
            [b"src/acme/__init__.py".as_slice(), b"src/tools.py"].contains(&path).then_some(4)
        });
        assert_eq!(result.target, Some(4));
        assert_eq!(calls.iter().filter(|path| path.as_slice() == b"src/tools.py").count(), 1);
        assert_eq!(calls.len(), 6, "two marker checks plus four unique-root targets");
    }

    #[test]
    fn malformed_or_oversized_absolute_names_never_consult_the_catalog() {
        for destination in ["", "name..part", "name/part", "name\\part", "caf\u{e9}", "2name", "a\0b"] {
            let result = resolve_import::<u64>(b"main.py", &fact(ImportKind::PythonImport, destination),
                |_| panic!("invalid name authorized lookup: {destination}"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        }
        for (source, destination) in [
            (b"main.py".to_vec(), "x".repeat(MAX_IMPORT_SPECIFIER_BYTES + 1)),
            (b"main.py".to_vec(), vec!["a"; MAX_COMPONENTS + 1].join(".")),
            (format!("{}/main.py", vec!["a"; MAX_COMPONENTS + 1].join("/")).into_bytes(), "tools".to_owned()),
            ([vec![b'x'; MAX_IMPORT_RESOLUTION_PATH_BYTES - 8], b"/main.py".to_vec()].concat(), "tools".to_owned()),
        ] {
            let result = resolve_import::<u64>(&source, &fact(ImportKind::PythonImport, &destination),
                |_| panic!("all possible paths and work must fit before lookup"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        }
        let mut denied = fact(ImportKind::PythonImport, "tools"); denied.resolution_allowed = false;
        assert_eq!(resolve_import::<u64>(b"main.py", &denied, |_| panic!("denied evidence" )).kind,
            ImportResolutionKind::Unresolved);
    }

    #[test]
    fn marker_walk_and_full_candidate_set_have_a_fixed_lookup_bound() {
        let directory = format!("project/{}", vec!["p"; MAX_COMPONENTS - 1].join("/"));
        let source = format!("{directory}/main.py");
        let mut calls = 0;
        let result = resolve_import(source.as_bytes(), &fact(ImportKind::PythonImport, "tools"), |path| {
            calls += 1;
            path.strip_suffix(b"/__init__.py").filter(|prefix| *prefix != b"project"
                && directory.as_bytes().starts_with(prefix)).map(|_| 1)
        });
        assert_eq!(result.kind, ImportResolutionKind::External);
        assert_eq!(calls, MAX_COMPONENTS + 6);
    }
}
