#![forbid(unsafe_code)]

//! Java source navigation over the same closed catalog as other import kinds.
//!
//! This is a finite filename policy, not Java binding. Probe repository-root,
//! src, src/main/java and src/test/java paths, and the same four roots beneath
//! the source's nearest conventional src-owning module. An explicit
//! src/{main,test}/java marker takes precedence over a package component named
//! src. Custom build roots, dependencies, package declarations, visibility and
//! compiler configuration are not consulted or inferred.
//!
//! Every dotted prefix may name an enclosing type, so Outer.Inner can navigate
//! Outer.java. No capitalization heuristic decides the package/type boundary.
//! Only one distinct matching catalog path is admissible, even when multiple
//! paths alias one file identity. Static imports omit the member or wildcard
//! before probing; ordinary package wildcards cannot name one target file.

use super::{CatalogPath, ImportFact, ImportKind, ImportLanguage, ImportResolution,
    ImportResolutionKind, MAX_IMPORT_RESOLUTION_PATH_BYTES};
use super::super::MAX_IMPORT_SPECIFIER_BYTES;

const MAX_COMPONENTS: usize = 64;
const ROOTS: [&[u8]; 4] = [b"", b"src", b"src/main/java", b"src/test/java"];
const EXACT: &str = "java-source-file-candidate";
const ENCLOSING: &str = "java-enclosing-type-candidate";

fn unsupported<T>() -> ImportResolution<T> {
    ImportResolution::without(ImportResolutionKind::Unresolved, "unsupported-specifier")
}

pub(super) fn resolve_java<T: Copy + Eq>(source: &[u8], fact: &ImportFact,
    lookup: &mut impl FnMut(&[u8]) -> Option<T>) -> ImportResolution<T> {
    if ImportLanguage::from_path(source) != Some(ImportLanguage::Java) {
        return unsupported();
    }
    let destination = fact.destination();
    if destination.is_empty() || destination.len() > MAX_IMPORT_SPECIFIER_BYTES {
        return unsupported();
    }
    let mut parts = [""; MAX_COMPONENTS];
    let mut count = 0;
    for part in destination.split('.') {
        if count == parts.len() || part.is_empty() { return unsupported(); }
        parts[count] = part;
        count += 1;
    }
    let wildcard = parts[count - 1] == "*";
    let names = count - usize::from(wildcard);
    if names == 0 || !parts[..names].iter().all(|part| ascii_name(part)) {
        return unsupported();
    }
    if fact.kind() == ImportKind::JavaImport && wildcard {
        return ImportResolution::without(ImportResolutionKind::Unresolved, "unresolved-java-wildcard");
    }
    if !fact.resolution_allowed() || count < 2 { return unsupported(); }
    if fact.kind() == ImportKind::JavaStaticImport {
        // A static member or star belongs to the preceding named type. This
        // policy locates that type's source, never an invented member range.
        count -= 1;
    }

    let module = module_prefix(source);
    let bases = [b"".as_slice(), module];
    let base_count = if module.is_empty() { 1 } else { 2 };
    let type_bytes = parts[..count].iter().map(|part| part.len()).sum::<usize>() + count - 1;
    // All candidate lengths must fit BEFORE any lookup. A discarded oversized
    // candidate could otherwise make a different candidate appear unique.
    for base in &bases[..base_count] {
        for root in ROOTS {
            let root_bytes = base.len() + usize::from(!base.is_empty() && !root.is_empty()) + root.len();
            if root_bytes.checked_add(usize::from(root_bytes > 0))
                .and_then(|n| n.checked_add(type_bytes))
                .and_then(|n| n.checked_add(b".java".len()))
                .is_none_or(|n| n > MAX_IMPORT_RESOLUTION_PATH_BYTES) {
                return unsupported();
            }
        }
    }

    let Some(mut path) = CatalogPath::parent(source) else { return unsupported(); };
    let mut found_path = [0u8; MAX_IMPORT_RESOLUTION_PATH_BYTES];
    let mut found: Option<(T, usize, &'static str)> = None;
    for base in &bases[..base_count] {
        for root in ROOTS {
            path.len = 0;
            if (!base.is_empty() && !path.push(base)) || (!root.is_empty() && !path.push(root)) {
                return unsupported();
            }
            for (ordinal, part) in parts[..count].iter().enumerate() {
                if !path.push(part.as_bytes()) { return unsupported(); }
                let prefix_len = path.len;
                path.data[prefix_len..prefix_len + 5].copy_from_slice(b".java");
                path.len += 5;
                if let Some(target) = lookup(path.bytes()) {
                    let basis = if ordinal + 1 == count { EXACT } else { ENCLOSING };
                    if let Some((previous, len, previous_basis)) = found {
                        if path.bytes() != &found_path[..len] || target != previous {
                            return ImportResolution::without(ImportResolutionKind::Ambiguous,
                                "ambiguous-catalog-target");
                        }
                        // Overlapping root/prefix combinations can name the
                        // SAME path. They are not two different candidates.
                        // Retain the stronger full-type filename basis if any
                        // supported spelling reaches this one catalog entry.
                        if previous_basis != EXACT && basis == EXACT { found = Some((target, len, EXACT)); }
                    } else {
                        found_path[..path.len].copy_from_slice(path.bytes());
                        found = Some((target, path.len, basis));
                    }
                }
                path.len = prefix_len;
            }
        }
    }
    match found {
        Some((target, _, basis)) => ImportResolution::local(target, basis),
        None => ImportResolution::without(ImportResolutionKind::Missing, "no-catalog-target"),
    }
}

fn ascii_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    bytes.next().is_some_and(|byte| byte.is_ascii_alphabetic() || matches!(byte, b'_' | b'$'))
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'$'))
}

fn module_prefix(source: &[u8]) -> &[u8] {
    let directory_len = source.iter().rposition(|&byte| byte == b'/').unwrap_or(0);
    let mut offset = 0usize;
    let mut conventional = None;
    let mut plain = None;
    for component in source[..directory_len].split(|&byte| byte == b'/') {
        if component == b"src" {
            let prefix_len = offset.saturating_sub(1);
            plain = Some(prefix_len);
            let following = &source[offset + component.len()..];
            if following.starts_with(b"/main/java/") || following.starts_with(b"/test/java/") {
                conventional = Some(prefix_len);
            }
        }
        offset += component.len() + 1;
    }
    &source[..conventional.or(plain).unwrap_or(0)]
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ByteOffset, ByteRange};

    fn fact(kind: ImportKind, destination: &str) -> ImportFact {
        let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(1)).unwrap();
        ImportFact { id: 1, kind, destination: destination.to_owned(), original_range: range,
            specifier_range: range, resolution_allowed: true, rust_module_context: None }
    }
    fn resolve(source: &[u8], kind: ImportKind, destination: &str, paths: &[&[u8]]) -> ImportResolution<usize> {
        super::super::resolve_import(source, &fact(kind, destination),
            |path| paths.iter().position(|candidate| *candidate == path))
    }

    #[test]
    fn conventional_roots_and_module_native_bytes_remain_exact() {
        for target in [b"lib/Helper.java".as_slice(), b"src/lib/Helper.java",
            b"src/main/java/lib/Helper.java", b"src/test/java/lib/Helper.java"] {
            let result = resolve(b"src/main/java/app/Main.java", ImportKind::JavaImport, "lib.Helper", &[target]);
            assert_eq!(result.target, Some(0)); assert_eq!(result.basis, EXACT);
        }
        for target in [b"modules/\xff/lib/Helper.java".as_slice(), b"modules/\xff/src/lib/Helper.java",
            b"modules/\xff/src/main/java/lib/Helper.java", b"modules/\xff/src/test/java/lib/Helper.java"] {
            let result = resolve(b"modules/\xff/src/test/java/app/Main.JAVA", ImportKind::JavaImport,
                "lib.Helper", &[target]);
            assert_eq!(result.target, Some(0)); assert_eq!(result.basis, EXACT);
        }
        assert_eq!(module_prefix(b"modules/one/src/main/java/pkg/src/Main.java"), b"modules/one");
        assert_eq!(module_prefix(b"modules/one/src/pkg/Main.java"), b"modules/one");
        assert_eq!(resolve(b"src/Main.java", ImportKind::JavaImport, "lib.Helper",
            &[b"src/lib/helper.java"]).kind, ImportResolutionKind::Missing);
        assert_eq!(resolve(b"src/Main.java", ImportKind::JavaImport, "lib.Helper",
            &[b"custom/java/lib/Helper.java"]).kind, ImportResolutionKind::Missing);
    }

    #[test]
    fn nested_types_and_static_members_navigate_only_the_source_file() {
        for (kind, destination, expected) in [
            (ImportKind::JavaImport, "lib.Outer.Inner", ENCLOSING),
            (ImportKind::JavaStaticImport, "lib.Outer.member", EXACT),
            (ImportKind::JavaStaticImport, "lib.Outer.*", EXACT),
            (ImportKind::JavaStaticImport, "lib.Outer.Inner.member", ENCLOSING),
        ] {
            let result = resolve(b"src/Main.java", kind, destination, &[b"src/lib/Outer.java"]);
            assert_eq!(result.target, Some(0)); assert_eq!(result.basis, expected);
        }
        let result = resolve(b"src/Main.java", ImportKind::JavaImport, "lib.Outer.Inner",
            &[b"src/lib/Outer/Inner.java"]);
        assert_eq!(result.target, Some(0)); assert_eq!(result.basis, EXACT);
    }

    #[test]
    fn distinct_paths_never_gain_a_preference_even_when_their_file_id_matches() {
        for paths in [vec![b"lib/Helper.java".as_slice(), b"src/lib/Helper.java"],
            vec![b"src/lib/Outer.java".as_slice(), b"src/lib/Outer/Inner.java"]] {
            let destination = if paths[0] == b"lib/Helper.java" { "lib.Helper" } else { "lib.Outer.Inner" };
            let result = super::super::resolve_import(b"src/Main.java", &fact(ImportKind::JavaImport, destination),
                |path| paths.contains(&path).then_some(9));
            assert_eq!(result.kind, ImportResolutionKind::Ambiguous); assert_eq!(result.target, None);
        }
        // The full type under the repository root and an enclosing type under
        // src happen to produce the same path. It remains one exact candidate.
        let repeated = resolve(b"src/Main.java", ImportKind::JavaImport, "src.src", &[b"src/src.java"]);
        assert_eq!(repeated.target, Some(0)); assert_eq!(repeated.basis, EXACT);
    }

    #[test]
    fn wildcards_unmodeled_identifiers_and_invalid_sources_never_consult_targets() {
        let result: ImportResolution<u64> = super::super::resolve_import(b"Main.java",
            &fact(ImportKind::JavaImport, "lib.*"), |_| panic!("package wildcard is not one target"));
        assert_eq!(result.basis, "unresolved-java-wildcard"); assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        for destination in ["", "Single", "lib..Type", "lib.Type/Other", "lib.\\u0054ype",
            "lib.Typé", "lib.Type%2fOther", "lib.Type\0", "lib.2Type", "lib.*.Other", ".Type"] {
            let result: ImportResolution<u64> = super::super::resolve_import(b"src/Main.java",
                &fact(ImportKind::JavaImport, destination), |_| panic!("invalid path: {destination}"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved, "{destination}");
        }
        for source in [b"Main.rs".as_slice(), b"/Main.java", b"../Main.java", b"src//Main.java"] {
            let result: ImportResolution<u64> = super::super::resolve_import(source,
                &fact(ImportKind::JavaImport, "lib.Type"), |_| panic!("invalid source"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        }
        let mut denied = fact(ImportKind::JavaImport, "lib.Type"); denied.resolution_allowed = false;
        let result: ImportResolution<u64> = super::super::resolve_import(b"Main.java", &denied, |_| panic!("denied fact"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    }

    #[test]
    fn every_probe_fits_and_work_is_bounded_before_any_candidate_can_succeed() {
        let mut calls = 0;
        let result: ImportResolution<u64> = super::super::resolve_import(b"modules/core/src/main/java/Main.java",
            &fact(ImportKind::JavaImport, "lib.Type"), |_| { calls += 1; None });
        assert_eq!(calls, 16); assert_eq!(result.kind, ImportResolutionKind::Missing);
        let destination = vec!["a"; MAX_COMPONENTS + 1].join(".");
        let result: ImportResolution<u64> = super::super::resolve_import(b"Main.java",
            &fact(ImportKind::JavaImport, &destination), |_| panic!("component limit before lookup"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        let source = [vec![b'x'; MAX_IMPORT_RESOLUTION_PATH_BYTES - b"/src/Main.java".len()], b"/src/Main.java".to_vec()].concat();
        let result: ImportResolution<u64> = super::super::resolve_import(&source,
            &fact(ImportKind::JavaImport, "lib.Type"), |_| panic!("all roots must fit before lookup"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    }
}
