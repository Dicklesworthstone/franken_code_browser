#![forbid(unsafe_code)]

//! A finite, explicit path policy over a caller's closed file catalog.
//! No filesystem, package manifest, compiler configuration or URL is consulted.
//! A local target is a navigable lexical candidate, not compiler resolution.
use super::{ImportFact, ImportKind, ImportLanguage};

mod rust;
mod go;
mod java;
mod python;
pub use go::{CapturedGoModule, GoModuleLocation, find_go_module, go_package_source,
    resolve_go_import, MAX_GO_MODULE_SOURCE_BYTES};

pub const MAX_IMPORT_RESOLUTION_PATH_BYTES: usize = 16 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ImportResolutionKind { Local, Missing, Ambiguous, External, Unresolved }
impl ImportResolutionKind {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Local => "local", Self::Missing => "missing", Self::Ambiguous => "ambiguous",
            Self::External => "external", Self::Unresolved => "unresolved",
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportResolution<T> {
    pub target: Option<T>,
    pub kind: ImportResolutionKind,
    pub basis: &'static str,
}
impl<T> ImportResolution<T> {
    fn without(kind: ImportResolutionKind, basis: &'static str) -> Self {
        Self { target: None, kind, basis }
    }
    fn local(target: T, basis: &'static str) -> Self {
        Self { target: Some(target), kind: ImportResolutionKind::Local, basis }
    }
}

/// Resolve only against the exact file identities offered by lookup.
/// The caller supplies the captured source's canonical relative catalog path;
/// native bytes are retained, including non-UTF-8 parent components. The lookup
/// must inspect that immutable catalog, not open paths or consult a live root.
///
/// JavaScript/TypeScript static imports, literal import() expressions and bare
/// require() candidates use the exact relative filename first. The latter does
/// not establish that the require identifier binds to Node's module loader.
/// In TypeScript/TSX sources, absent relative .js/.jsx, .mjs and .cjs targets
/// admit a unique corresponding typed source or declaration file. This is a
/// closed-catalog source candidate, without compiler ordering or configuration.
/// If absent and extensionless, a finite set of source extensions and index files is admitted
/// only when exactly one path exists. Rust uses conventional file modules for
/// a top-level external mod. Eligible qualified use leaves navigate conventional
/// module files through the bounded policy in `rust`; a remaining item suffix
/// is explicitly a module-prefix candidate, never a definition location.
/// Attributes and unmodeled source scopes remain unresolved. Multiple matching
/// paths are ambiguous even if a callback maps them to the same target identity.
/// No extension preference is inferred.
/// Python relative module candidates use leading dots to climb the source
/// directory, then dotted names become components, with .py and /__init__.py as
/// the complete finite set. Absolute names probe those same two suffixes under
/// the catalog root, src/, and at most one enclosing regular-package root
/// established by consecutive catalogued __init__.py parents. Distinct targets
/// remain ambiguous; absent candidates remain external. These are filename
/// candidates, with no sys.path, execution, namespace-root or NFKC inference.
/// C/C++ quoted includes admit only the exact source-relative catalog path.
/// Angle includes remain external; computed includes remain unresolved. No
/// include directories, compilation database, macro expansion or header suffix
/// preference is inferred, and header-name bytes are not C-unescaped.
/// Java imports admit a bounded set of conventional source roots and dotted
/// type/enclosing-type filename candidates. Static imports navigate their type
/// candidate, never the member. Package wildcards, classpaths and build-system
/// source roots are not inferred; see `java` for the complete finite policy.
pub fn resolve_import<T: Copy + Eq>(
    source_path: &[u8], fact: &ImportFact, mut lookup: impl FnMut(&[u8]) -> Option<T>,
) -> ImportResolution<T> {
    use ImportResolutionKind::{External, Missing, Unresolved};
    if !valid_source_path(source_path) {
        return ImportResolution::without(Unresolved, "unsupported-specifier");
    }
    if matches!(fact.kind(), ImportKind::JavaImport | ImportKind::JavaStaticImport) {
        return java::resolve_java(source_path, fact, &mut lookup);
    }
    if fact.kind() == ImportKind::GoImport {
        return resolve_go_import(source_path, fact, None, |_| false, |_| None);
    }
    if fact.kind() == ImportKind::RustUse {
        return rust::resolve_use(source_path, fact, &mut lookup);
    }
    if matches!(fact.kind(), ImportKind::CppQuotedInclude | ImportKind::CppAngleInclude | ImportKind::CppComputedInclude) {
        return cpp_include(source_path, fact, &mut lookup);
    }
    let destination = fact.destination();
    if !fact.resolution_allowed() || destination.is_empty()
        || destination.len() > MAX_IMPORT_RESOLUTION_PATH_BYTES
        || destination.chars().any(char::is_control)
        || destination.as_bytes().iter().copied().any(|byte| matches!(byte, b'\\' | b'?' | b'%')) {
        return ImportResolution::without(Unresolved, "unsupported-specifier");
    }
    if fact.kind() == ImportKind::RustModule {
        return rust_module(source_path, destination, &mut lookup);
    }
    if matches!(fact.kind(), ImportKind::PythonImport | ImportKind::PythonFrom) {
        return python_module(source_path, fact.kind(), destination, &mut lookup);
    }
    if destination.contains('#') || destination.ends_with('/')
        || matches!(destination.rsplit('/').next(), Some("." | "..")) {
        return ImportResolution::without(Unresolved, "unsupported-specifier");
    }
    if !(destination.starts_with("./") || destination.starts_with("../")) {
        return ImportResolution::without(External, "external-specifier");
    }
    let Some(mut path) = CatalogPath::relative(source_path, destination.as_bytes()) else {
        return ImportResolution::without(Unresolved, "unsupported-specifier");
    };
    if let Some(target) = lookup(path.bytes()) {
        return ImportResolution::local(target, "exact-relative-path");
    }
    if matches!(ImportLanguage::from_path(source_path), Some(ImportLanguage::TypeScript | ImportLanguage::Tsx)) {
        if let Some(result) = typescript_source_candidate(&mut path, &mut lookup) {
            return result;
        }
    }
    // Other literal extensions never acquire an unrelated source suffix.
    if path.file_name().contains(&b'.') {
        return ImportResolution::without(Missing, "no-catalog-target");
    }
    const CANDIDATES: [&[u8]; 12] = [
        b".ts", b".tsx", b".js", b".jsx", b".mjs", b".cjs",
        b"/index.ts", b"/index.tsx", b"/index.js", b"/index.jsx", b"/index.mjs", b"/index.cjs",
    ];
    candidates(&mut path, &CANDIDATES, "unique-relative-module-candidate", &mut lookup)
}

fn typescript_source_candidate<T: Copy + Eq>(
    path: &mut CatalogPath, lookup: &mut impl FnMut(&[u8]) -> Option<T>,
) -> Option<ImportResolution<T>> {
    let (runtime_suffix, source_suffixes): (&[u8], &[&[u8]]) =
        if path.file_name().ends_with(b".js") { (b".js", &[b".ts", b".tsx", b".d.ts"]) }
        else if path.file_name().ends_with(b".jsx") { (b".jsx", &[b".tsx"]) }
        else if path.file_name().ends_with(b".mjs") { (b".mjs", &[b".mts", b".d.mts"]) }
        else if path.file_name().ends_with(b".cjs") { (b".cjs", &[b".cts", b".d.cts"]) }
        else { return None; };
    // Strip only the written runtime extension; parent bytes and filename case
    // stay exact. Declaration files are candidates, not runtime implementations.
    path.len -= runtime_suffix.len();
    Some(candidates(path, source_suffixes, "typescript-source-extension-candidate", lookup))
}

fn cpp_include<T: Copy + Eq>(source_path: &[u8], fact: &ImportFact,
    lookup: &mut impl FnMut(&[u8]) -> Option<T>) -> ImportResolution<T> {
    let unsupported = || ImportResolution::without(ImportResolutionKind::Unresolved, "unsupported-specifier");
    if ImportLanguage::from_path(source_path) != Some(ImportLanguage::Cpp) { return unsupported(); }
    if fact.kind() == ImportKind::CppComputedInclude {
        return ImportResolution::without(ImportResolutionKind::Unresolved, "unresolved-cpp-include");
    }
    let destination = fact.destination();
    if !fact.resolution_allowed() || destination.is_empty() || destination.len() > MAX_IMPORT_RESOLUTION_PATH_BYTES
        || destination.chars().any(|c| c.is_control() || matches!(c, '\\' | '"' | '<' | '>')) {
        return unsupported();
    }
    if fact.kind() == ImportKind::CppAngleInclude {
        return ImportResolution::without(ImportResolutionKind::External, "external-specifier");
    }
    if destination.starts_with('/') || destination.contains(':') || destination.ends_with('/')
        || matches!(destination.rsplit('/').next(), Some("." | "..")) { return unsupported(); }
    let Some(path) = CatalogPath::relative(source_path, destination.as_bytes()) else { return unsupported(); };
    match lookup(path.bytes()) {
        Some(target) => ImportResolution::local(target, "cpp-source-relative-include-candidate"),
        None => ImportResolution::without(ImportResolutionKind::Missing, "no-catalog-target"),
    }
}

fn rust_module<T: Copy + Eq>(
    source_path: &[u8], destination: &str, lookup: &mut impl FnMut(&[u8]) -> Option<T>,
) -> ImportResolution<T> {
    let unsupported = || ImportResolution::without(ImportResolutionKind::Unresolved, "unsupported-specifier");
    let name = destination.strip_prefix("r#").unwrap_or(destination);
    // The extractor owns Rust identifier semantics. This independent path gate
    // prevents forged separators and accepts literal Unicode identifier bytes.
    if name.is_empty() || name.chars().any(|c| c.is_whitespace() || c.is_control())
        || name.as_bytes().iter().copied().any(|b| matches!(b, b'/' | b'\\' | b'.' | b':' | b'#' | b'?' | b'%')) {
        return unsupported();
    }
    let file = source_path.rsplit(|b| *b == b'/').next().unwrap_or_default();
    let Some(stem) = file.strip_suffix(b".rs") else { return unsupported(); };
    if stem.is_empty() { return unsupported(); }
    let Some(mut path) = CatalogPath::parent(source_path) else { return unsupported(); };
    if !matches!(file, b"lib.rs" | b"main.rs" | b"mod.rs") && !path.push(stem) {
        return unsupported();
    }
    if !path.push(name.as_bytes()) { return unsupported(); }
    candidates(&mut path, &[b".rs", b"/mod.rs"], "rust-module-file-candidate", lookup)
}

fn python_module<T: Copy + Eq>(
    source_path: &[u8], kind: ImportKind, destination: &str, lookup: &mut impl FnMut(&[u8]) -> Option<T>,
) -> ImportResolution<T> {
    let unsupported = || ImportResolution::without(ImportResolutionKind::Unresolved, "unsupported-specifier");
    if ImportLanguage::from_path(source_path) != Some(ImportLanguage::Python) { return unsupported(); }
    let dots = destination.bytes().take_while(|&byte| byte == b'.').count();
    let names = &destination[dots..];
    // Only the ASCII identifier subset has a byte-exact filename spelling
    // without Python's NFKC normalization. Never guess a Unicode target.
    if names.is_empty() || !names.split('.').all(|part| {
        let mut bytes = part.bytes();
        bytes.next().is_some_and(|byte| byte == b'_' || byte.is_ascii_alphabetic())
            && bytes.all(|byte| byte == b'_' || byte.is_ascii_alphanumeric())
    }) { return unsupported(); }
    if dots == 0 {
        return python::resolve_absolute(source_path, destination, lookup);
    }
    if kind != ImportKind::PythonFrom { return unsupported(); }
    let Some(mut path) = CatalogPath::parent(source_path) else { return unsupported(); };
    for _ in 1..dots {
        if path.len == 0 { return unsupported(); }
        path.len = path.bytes().iter().rposition(|&byte| byte == b'/').unwrap_or(0);
    }
    for part in names.split('.') {
        if !path.push(part.as_bytes()) { return unsupported(); }
    }
    candidates(&mut path, &[b".py", b"/__init__.py"], "python-relative-module-candidate", lookup)
}

fn candidates<T: Copy + Eq>(
    path: &mut CatalogPath, suffixes: &[&[u8]], basis: &'static str,
    lookup: &mut impl FnMut(&[u8]) -> Option<T>,
) -> ImportResolution<T> {
    let base = path.len;
    // Check every candidate's capacity before consulting the catalog. A skipped
    // oversized candidate cannot prove uniqueness or absence.
    if suffixes.iter().any(|suffix| suffix.len() > MAX_IMPORT_RESOLUTION_PATH_BYTES - base) {
        return ImportResolution::without(ImportResolutionKind::Unresolved, "unsupported-specifier");
    }
    let mut found = None;
    for suffix in suffixes {
        path.data[base..base + suffix.len()].copy_from_slice(suffix);
        path.len = base + suffix.len();
        if let Some(target) = lookup(path.bytes()) {
            if found.is_some() {
                path.len = base;
                return ImportResolution::without(ImportResolutionKind::Ambiguous, "ambiguous-catalog-target");
            }
            found = Some(target);
        }
    }
    path.len = base;
    match found {
        Some(target) => ImportResolution::local(target, basis),
        None => ImportResolution::without(ImportResolutionKind::Missing, "no-catalog-target"),
    }
}

fn valid_source_path(path: &[u8]) -> bool {
    !path.is_empty() && path.len() <= MAX_IMPORT_RESOLUTION_PATH_BYTES && !path.contains(&0)
        && !path.starts_with(b"/") && !path.ends_with(b"/")
        && path.split(|b| *b == b'/').all(|part| !part.is_empty() && part != b"." && part != b"..")
}
struct CatalogPath { data: [u8; MAX_IMPORT_RESOLUTION_PATH_BYTES], len: usize }
impl CatalogPath {
    fn parent(source: &[u8]) -> Option<Self> {
        if !valid_source_path(source) { return None; }
        let len = source.iter().rposition(|b| *b == b'/').unwrap_or(0);
        let mut path = Self { data: [0; MAX_IMPORT_RESOLUTION_PATH_BYTES], len };
        path.data[..len].copy_from_slice(&source[..len]);
        Some(path)
    }
    fn relative(source: &[u8], destination: &[u8]) -> Option<Self> {
        let mut path = Self::parent(source)?;
        for part in destination.split(|b| *b == b'/') {
            match part {
                b"" => return None,
                b"." => {},
                b".." => {
                    if path.len == 0 { return None; }
                    path.len = path.bytes().iter().rposition(|b| *b == b'/').unwrap_or(0);
                }
                part => { if !path.push(part) { return None; } },
            }
        }
        (path.len > 0).then_some(path)
    }
    fn push(&mut self, part: &[u8]) -> bool {
        let separator = usize::from(self.len > 0);
        let Some(length) = self.len.checked_add(separator).and_then(|n| n.checked_add(part.len())) else { return false; };
        if part.is_empty() || length > MAX_IMPORT_RESOLUTION_PATH_BYTES { return false; }
        if separator != 0 { self.data[self.len] = b'/'; }
        self.data[self.len + separator..length].copy_from_slice(part);
        self.len = length;
        true
    }
    fn bytes(&self) -> &[u8] { &self.data[..self.len] }
    fn file_name(&self) -> &[u8] { self.bytes().rsplit(|b| *b == b'/').next().unwrap_or_default() }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb_core::{ByteOffset, ByteRange};

    fn fact(kind: ImportKind, destination: &str) -> ImportFact {
        let range = ByteRange::new(ByteOffset::new(0), ByteOffset::new(1)).unwrap();
        ImportFact { id: 1, kind, destination: destination.to_owned(), original_range: range,
            specifier_range: range, resolution_allowed: true }
    }
    fn resolve(source: &[u8], kind: ImportKind, destination: &str, paths: &[&[u8]]) -> ImportResolution<usize> {
        resolve_import(source, &fact(kind, destination), |path| paths.iter().position(|candidate| *candidate == path))
    }

    #[test]
    fn exact_relative_target_wins_and_preserves_native_parent_bytes() {
        let paths: &[&[u8]] = &[b"src/\xff/tool", b"src/\xff/tool.ts", b"src/\xff/tool/index.js"];
        let result = resolve(b"src/\xff/main.ts", ImportKind::JavaScriptImport, "./tool", paths);
        assert_eq!(result.target, Some(0));
        assert_eq!(result.basis, "exact-relative-path");
        assert_eq!(resolve(b"src/\xff/deep/main.ts", ImportKind::JavaScriptExport, "../tool", paths), result);
        assert_eq!(resolve(b"src/\xff/main.ts", ImportKind::JavaScriptImport, "./Tool", paths).kind,
            ImportResolutionKind::Missing, "native path case must not be folded");
    }

    #[test]
    fn extensionless_targets_require_a_unique_finite_candidate() {
        for path in [b"src/tool.ts".as_slice(), b"src/tool.tsx", b"src/tool.js", b"src/tool.jsx",
            b"src/tool.mjs", b"src/tool.cjs", b"src/tool/index.ts", b"src/tool/index.tsx",
            b"src/tool/index.js", b"src/tool/index.jsx", b"src/tool/index.mjs", b"src/tool/index.cjs"] {
            let result = resolve(b"src/main.ts", ImportKind::JavaScriptImport, "./tool", &[path]);
            assert_eq!(result.target, Some(0), "{path:?}");
            assert_eq!(result.basis, "unique-relative-module-candidate");
        }
        for paths in [vec![b"src/tool.ts".as_slice(), b"src/tool.js"],
            vec![b"src/tool.ts".as_slice(), b"src/tool/index.ts"]] {
            let result = resolve(b"src/main.ts", ImportKind::JavaScriptImport, "./tool", &paths);
            assert_eq!(result.kind, ImportResolutionKind::Ambiguous);
            assert_eq!(result.target, None);
        }
        let result = resolve_import(b"src/main.ts", &fact(ImportKind::JavaScriptImport, "./tool"),
            |path| [b"src/tool.ts".as_slice(), b"src/tool.js"].contains(&path).then_some(7));
        assert_eq!(result.kind, ImportResolutionKind::Ambiguous, "two paths remain ambiguous even with one aliased identity");
        let result = resolve(b"src/main.js", ImportKind::JavaScriptImport, "./tool.js", &[b"src/tool.ts"]);
        assert_eq!(result.kind, ImportResolutionKind::Missing, "JavaScript sources do not infer typed source replacements");
    }

    #[test]
    fn typescript_runtime_extensions_find_unique_typed_sources() {
        for (destination, target) in [
            ("./tool.js", b"src/\xff/tool.ts".as_slice()),
            ("./tool.js", b"src/\xff/tool.tsx"),
            ("./tool.js", b"src/\xff/tool.d.ts"),
            ("./tool.jsx", b"src/\xff/tool.tsx"),
            ("./tool.mjs", b"src/\xff/tool.mts"),
            ("./tool.mjs", b"src/\xff/tool.d.mts"),
            ("./tool.cjs", b"src/\xff/tool.cts"),
            ("./tool.cjs", b"src/\xff/tool.d.cts"),
        ] {
            for source in [b"src/\xff/main.ts".as_slice(), b"src/\xff/main.tsx", b"src/\xff/main.mts", b"src/\xff/main.cts"] {
                for kind in [ImportKind::JavaScriptImport, ImportKind::JavaScriptExport,
                    ImportKind::JavaScriptRequire, ImportKind::JavaScriptDynamicImport] {
                    let result = resolve(source, kind, destination, &[target]);
                    assert_eq!(result.target, Some(0), "{source:?}: {destination} -> {target:?}");
                    assert_eq!(result.basis, "typescript-source-extension-candidate");
                }
            }
        }
    }

    #[test]
    fn typed_source_candidates_preserve_exact_precedence_and_ambiguity() {
        let result = resolve(b"src/main.ts", ImportKind::JavaScriptImport, "./tool.js",
            &[b"src/tool.ts", b"src/tool.d.ts", b"src/tool.js"]);
        assert_eq!(result.target, Some(2));
        assert_eq!(result.basis, "exact-relative-path");
        for paths in [vec![b"src/tool.ts".as_slice(), b"src/tool.tsx"],
            vec![b"src/tool.ts".as_slice(), b"src/tool.d.ts"]] {
            let result = resolve(b"src/main.ts", ImportKind::JavaScriptImport, "./tool.js", &paths);
            assert_eq!(result.kind, ImportResolutionKind::Ambiguous);
            assert_eq!(result.target, None);
        }
        let result = resolve_import(b"src/main.ts", &fact(ImportKind::JavaScriptExport, "./tool.mjs"),
            |path| [b"src/tool.mts".as_slice(), b"src/tool.d.mts"].contains(&path).then_some(7));
        assert_eq!(result.kind, ImportResolutionKind::Ambiguous, "distinct paths cannot become a unique candidate through aliased IDs");
        for (source, destination, target) in [
            (b"src/main.js".as_slice(), "./tool.js", b"src/tool.ts".as_slice()),
            (b"src/main.jsx", "./tool.jsx", b"src/tool.tsx"),
            (b"src/main.ts", "./tool.mjs", b"src/tool.ts"),
            (b"src/main.ts", "./tool.cjs", b"src/tool.mts"),
            (b"src/main.ts", "./tool.JS", b"src/tool.ts"),
            (b"src/main.ts", "./tool.css", b"src/tool.css.ts"),
            (b"src/main.ts", "./tool", b"src/tool.mts"),
        ] {
            assert_eq!(resolve(source, ImportKind::JavaScriptImport, destination, &[target]).kind,
                ImportResolutionKind::Missing, "{source:?}: {destination}");
        }
    }

    #[test]
    fn typed_source_lookup_is_bounded_and_confined_to_the_capture() {
        let mut calls = Vec::new();
        let result: ImportResolution<usize> = resolve_import(b"src/deep/main.ts",
            &fact(ImportKind::JavaScriptImport, "../tool.js"), |path| { calls.push(path.to_vec()); None });
        assert_eq!(result.kind, ImportResolutionKind::Missing);
        assert_eq!(calls, [b"src/tool.js".to_vec(), b"src/tool.ts".to_vec(), b"src/tool.tsx".to_vec(), b"src/tool.d.ts".to_vec()]);
        let result: ImportResolution<usize> = resolve_import(b"main.ts",
            &fact(ImportKind::JavaScriptImport, "../tool.js"), |_| panic!("cannot leave catalog root"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        let source = [vec![b'x'; MAX_IMPORT_RESOLUTION_PATH_BYTES - 9], b"/main.ts".to_vec()].concat();
        let mut calls = 0;
        let result: ImportResolution<usize> = resolve_import(&source,
            &fact(ImportKind::JavaScriptImport, "./tool.js"), |_| { calls += 1; None });
        assert_eq!(result.kind, ImportResolutionKind::Unresolved, "oversized declaration candidate cannot prove uniqueness");
        assert_eq!(calls, 1, "validate all substituted path lengths before probing any of them");
    }

    #[test]
    fn rust_modules_use_only_the_declared_file_module_policy() {
        for (source, target) in [
            (b"src/lib.rs".as_slice(), b"src/engine.rs".as_slice()),
            (b"src/main.rs", b"src/engine/mod.rs"),
            (b"src/net/mod.rs", b"src/net/engine.rs"),
            (b"src/net.rs", b"src/net/engine.rs"),
            (b"lib.rs", b"engine/mod.rs"),
        ] {
            let result = resolve(source, ImportKind::RustModule, "engine", &[target]);
            assert_eq!(result.target, Some(0), "{source:?}");
            assert_eq!(result.basis, "rust-module-file-candidate");
        }
        let result = resolve(b"src/lib.rs", ImportKind::RustModule, "engine", &[b"src/engine.rs", b"src/engine/mod.rs"]);
        assert_eq!(result.kind, ImportResolutionKind::Ambiguous);
        assert_eq!(resolve(b"src/lib.rs", ImportKind::RustModule, "r#type", &[b"src/type.rs"]).target, Some(0));
        assert_eq!(resolve(b"src/main.rs", ImportKind::RustUse, "crate::engine", &[b"src/engine.rs"]).kind,
            ImportResolutionKind::Unresolved);
        let mut nested = fact(ImportKind::RustModule, "engine"); nested.resolution_allowed = false;
        let result: ImportResolution<usize> = resolve_import(b"src/lib.rs", &nested,
            |_| panic!("unmodeled source context cannot consult targets"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    }

    #[test]
    fn unsupported_or_external_specifiers_never_widen_the_catalog() {
        for destination in ["../../outside", "./x.js?raw", "./x.js#entry", "./x%2fsecret.js",
            "./x\\u002ejs", "./a\0b", "./x\n.js", "./", "./a//b", "../.", "./a/.."] {
            let result: ImportResolution<usize> = resolve_import(b"src/main.js",
                &fact(ImportKind::JavaScriptImport, destination), |_| panic!("unsupported target was looked up: {destination}"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved, "{destination}");
        }
        for destination in ["react", "@scope/package", "node:fs", "https://example.invalid/a.js", "/absolute.js"] {
            let result: ImportResolution<usize> = resolve_import(b"src/main.js",
                &fact(ImportKind::JavaScriptImport, destination), |_| panic!("external target was looked up"));
            assert_eq!(result.kind, ImportResolutionKind::External, "{destination}");
        }
        for source in [b"".as_slice(), b"/absolute.js", b"src/../main.js", b"src//main.js", b"src/main.js/"] {
            let result: ImportResolution<usize> = resolve_import(source, &fact(ImportKind::JavaScriptImport, "./a"),
                |_| panic!("invalid source path authorized lookup"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        }
    }

    #[test]
    fn relative_components_stay_beneath_root_and_lookups_are_bounded() {
        assert_eq!(resolve(b"a/b/main.ts", ImportKind::JavaScriptImport, ".././tool",
            &[b"a/tool.ts"]).target, Some(0));
        assert_eq!(resolve(b"main.js", ImportKind::JavaScriptImport, "../tool", &[b"tool.js"]).kind,
            ImportResolutionKind::Unresolved);
        let mut calls = 0;
        let result: ImportResolution<usize> = resolve_import(b"src/main.js", &fact(ImportKind::JavaScriptImport, "./tool"),
            |_| { calls += 1; None });
        assert_eq!(result.kind, ImportResolutionKind::Missing);
        assert_eq!(calls, 13);
        let source = [vec![b'x'; MAX_IMPORT_RESOLUTION_PATH_BYTES - 8], b"/main.js".to_vec()].concat();
        let result: ImportResolution<usize> = resolve_import(&source, &fact(ImportKind::JavaScriptImport, "./tool"),
            |_| None);
        assert_eq!(result.kind, ImportResolutionKind::Unresolved, "unprobeable candidates cannot establish absence");
    }
    #[test]
    fn python_forged_specifiers_and_unprobeable_candidates_never_authorize_lookup() {
        for destination in [".", "..", ".foo..bar", ".foo/bar", ".foo\\bar",
            ".foo bar", ".foo%2fbar", ".foo?raw", ".foo#anchor", ".caf\u{e9}", ".2name"] {
            let result: ImportResolution<usize> = resolve_import(b"pkg/main.py",
                &fact(ImportKind::PythonFrom, destination), |_| panic!("unsupported Python target: {destination}"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved, "{destination}");
        }
        for (source, kind, destination) in [
            (b"pkg/main.rs".as_slice(), ImportKind::PythonFrom, ".helper"),
            (b"pkg/main.py", ImportKind::PythonImport, ".helper"),
            (b"main.py", ImportKind::PythonFrom, "..helper"),
        ] {
            let result: ImportResolution<usize> = resolve_import(source, &fact(kind, destination),
                |_| panic!("invalid Python source/scope looked up"));
            assert_eq!(result.kind, ImportResolutionKind::Unresolved);
        }
        let source = [vec![b'x'; MAX_IMPORT_RESOLUTION_PATH_BYTES - 8], b"/main.py".to_vec()].concat();
        let result: ImportResolution<usize> = resolve_import(&source, &fact(ImportKind::PythonFrom, ".helper"),
            |_| panic!("not all Python candidates fit; uniqueness cannot be established"));
        assert_eq!(result.kind, ImportResolutionKind::Unresolved);
    }

    #[test]
    fn python_local_candidates_are_exactly_module_and_package_without_extension_preference() {
        for source in [b"pkg/main.py".as_slice(), b"pkg/main.pyi", b"pkg/__init__.py", b"pkg/API.PY"] {
            for target in [b"pkg/helper.py".as_slice(), b"pkg/helper/__init__.py"] {
                let result = resolve(source, ImportKind::PythonFrom, ".helper", &[target]);
                assert_eq!(result.target, Some(0));
                assert_eq!(result.basis, "python-relative-module-candidate");
            }
        }
        assert_eq!(resolve(b"main.py", ImportKind::PythonFrom, ".helper", &[b"helper.py"]).target, Some(0));
        assert_eq!(resolve(b"pkg/main.py", ImportKind::PythonFrom, ".Helper", &[b"pkg/helper.py"]).kind,
            ImportResolutionKind::Missing);
        assert_eq!(resolve(b"a/b/main.py", ImportKind::PythonFrom, "..shared.tool", &[b"a/shared/tool.py"]).target,
            Some(0));
        let mut calls = 0;
        let missing: ImportResolution<usize> = resolve_import(b"pkg/main.py", &fact(ImportKind::PythonFrom, ".helper"),
            |_| { calls += 1; None });
        assert_eq!(calls, 2);
        assert_eq!(missing.kind, ImportResolutionKind::Missing);
    }

}
