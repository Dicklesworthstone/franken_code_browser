#![forbid(unsafe_code)]

//! Module-local Go package navigation from captured metadata. This never
//! consults the working tree, module cache, network, go.work, replace directives
//! or build constraints. The chosen file represents a package directory; it
//! does not establish compiler binding or a package's complete source set.

use fcb_core::{ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::CaptureRequest;
use franken_markdown::highlight::{Span, Tok, highlight_into};
use super::{CatalogPath, ImportFact, ImportKind, ImportLanguage, ImportResolution,
    ImportResolutionKind, valid_source_path};
use super::super::{ImportError, MAX_IMPORT_SPECIFIER_BYTES, check, reserve, terminals};

pub const MAX_GO_MODULE_SOURCE_BYTES: usize = 64 * 1024;

pub struct GoModuleLocation<T> { pub target: T, path: CatalogPath }
impl<T> GoModuleLocation<T> { pub fn path(&self) -> &[u8] { self.path.bytes() } }

/// Nearest catalog go.mod wins even when its captured bytes are unavailable.
/// A caller must not fall back to a parent module after finding such a boundary.
pub fn find_go_module<T>(source_path: &[u8], mut lookup: impl FnMut(&[u8]) -> Option<T>)
    -> Option<GoModuleLocation<T>> {
    let mut path = CatalogPath::parent(source_path)?;
    loop {
        let parent = path.len;
        if !path.push(b"go.mod") { return None; }
        if let Some(target) = lookup(path.bytes()) { return Some(GoModuleLocation { target, path }); }
        path.len = parent;
        if parent == 0 { return None; }
        path.len = path.bytes().iter().rposition(|&b| b == b'/').unwrap_or(0);
    }
}

pub struct CapturedGoModule<'source> {
    root: CatalogPath,
    name: &'source str,
    request: CaptureRequest,
    _lease: ResourceLease,
}
impl<'source> CapturedGoModule<'source> {
    /// Bounded recognition of the single module directive using the pinned Go
    /// lexer for comments/quoted tokens. Unescaped quoted and raw path names
    /// are supported, including the parenthesized module directive form.
    pub fn build(path: &[u8], source: &'source [u8], request: CaptureRequest,
        budget: &ResourceBudget, allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<Self, ImportError> {
        if source.len() > MAX_GO_MODULE_SOURCE_BYTES { return Err(ImportError::SourceLimit); }
        if request.range().is_some() || !valid_source_path(path)
            || path.rsplit(|&b| b == b'/').next() != Some(b"go.mod".as_slice()) {
            return Err(ImportError::InvalidEvidence);
        }
        check(&mut canceled)?;
        let charge = (source.len() + 1).checked_mul(128)
            .and_then(|n| n.checked_add(std::mem::size_of::<Self>() + 4096))
            .ok_or(ImportError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ImportError::ResourceDenied)?;
        let text = std::str::from_utf8(source).map_err(|_| ImportError::UnsupportedEncoding)?;
        let mut spans = reserve::<Span>(text.len().max(1))?;
        highlight_into("go", text, &mut spans);
        if spans.capacity() > text.len().max(1) { return Err(ImportError::InvalidEvidence); }
        super::super::go::validate_context(text, &spans, &mut canceled)?;
        if spans.iter().any(|span| span.kind == Tok::Comment && text[span.start..span.end].starts_with("/*")) {
            return Err(ImportError::UnsupportedLexicalContext);
        }
        let tokens = terminals(text, &spans, &mut canceled)?;
        let word = |i: usize| tokens.get(i).map_or("", |t| t.text(text));
        let newline = |i: usize| i > 0 && i < tokens.len()
            && text[tokens[i - 1].end..tokens[i].start].contains('\n');
        let mut name = None;
        let mut at = 0;
        let mut depth = 0usize;
        while at < tokens.len() {
            check(&mut canceled)?;
            if depth == 0 && (at == 0 || newline(at)) && word(at) == "module" {
                if name.is_some() || at + 1 == tokens.len() || newline(at + 1) {
                    return Err(ImportError::InvalidEvidence);
                }
                at += 1;
                let grouped = word(at) == "(";
                if grouped {
                    at += 1;
                    if !newline(at) { return Err(ImportError::InvalidEvidence); }
                }
                let first = at;
                while at < tokens.len() && word(at) != ")" && (at == first || !newline(at)) { at += 1; }
                if first == at { return Err(ImportError::InvalidEvidence); }
                let raw = &text[tokens[first].start..tokens[at - 1].end];
                let parsed = if tokens[first].kind == Tok::Str {
                    if at != first + 1 || !matches!(raw.as_bytes()[0], b'"' | b'`') {
                        return Err(ImportError::InvalidEvidence);
                    }
                    &raw[1..raw.len() - 1]
                } else { raw };
                if !safe_path(parsed) { return Err(ImportError::InvalidEvidence); }
                name = Some(parsed);
                if grouped {
                    if word(at) != ")" || !newline(at) { return Err(ImportError::InvalidEvidence); }
                    at += 1;
                    if at < tokens.len() && !newline(at) { return Err(ImportError::InvalidEvidence); }
                }
                continue;
            }
            match word(at) {
                "(" => { depth += 1; if depth > 32 { return Err(ImportError::InvalidEvidence); } }
                ")" => { depth = depth.checked_sub(1).ok_or(ImportError::InvalidEvidence)?; }
                _ => {}
            }
            at += 1;
        }
        if depth != 0 { return Err(ImportError::InvalidEvidence); }
        let root = CatalogPath::parent(path).ok_or(ImportError::InvalidEvidence)?;
        Ok(Self { root, name: name.ok_or(ImportError::InvalidEvidence)?, request, _lease: lease })
    }
    pub fn module_path(&self) -> &str { self.name }
    pub fn root(&self) -> &[u8] { self.root.bytes() }
    pub fn request(&self) -> CaptureRequest { self.request }
}

fn safe_path(path: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_IMPORT_SPECIFIER_BYTES
        && path.split('/').all(|part| !part.is_empty() && !matches!(part, "." | "..")
            && part.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+')))
}

/// `package` receives a root-relative directory prefix (empty at the root,
/// otherwise ending in '/'). It returns the lexicographically first catalog
/// file accepted by go_package_source. This is a deterministic representative,
/// not a compiler-selected implementation or a name/definition target.
pub fn resolve_go_import<T: Copy + Eq>(source_path: &[u8], fact: &ImportFact,
    module: Option<&CapturedGoModule<'_>>, mut module_exists: impl FnMut(&[u8]) -> bool,
    mut package: impl FnMut(&[u8]) -> Option<T>) -> ImportResolution<T> {
    use ImportResolutionKind::{External, Missing, Unresolved};
    let unsupported = || ImportResolution::without(Unresolved, "unsupported-specifier");
    if !valid_source_path(source_path) || ImportLanguage::from_path(source_path) != Some(ImportLanguage::Go)
        || fact.kind() != ImportKind::GoImport || !fact.resolution_allowed() || !safe_path(fact.destination()) {
        return unsupported();
    }
    if fact.destination() == "C" { return ImportResolution::without(External, "external-specifier"); }
    let Some(module) = module else { return ImportResolution::without(Unresolved, "go-module-context-unavailable"); };
    let root = module.root();
    if !root.is_empty() && (source_path.len() <= root.len() || !source_path.starts_with(root) || source_path[root.len()] != b'/') {
        return unsupported();
    }
    let suffix = if fact.destination() == module.name { "" }
        else if let Some(suffix) = fact.destination().strip_prefix(module.name).and_then(|s| s.strip_prefix('/')) { suffix }
        else { return ImportResolution::without(External, "external-specifier"); };
    let mut directory = CatalogPath { data: module.root.data, len: module.root.len };
    if !suffix.is_empty() {
        for part in suffix.split('/') {
            if !directory.push(part.as_bytes()) { return unsupported(); }
            let len = directory.len;
            if !directory.push(b"go.mod") { return unsupported(); }
            if module_exists(directory.bytes()) {
                return ImportResolution::without(Unresolved, "go-nested-module-boundary");
            }
            directory.len = len;
        }
    }
    if directory.len > 0 {
        if directory.len == directory.data.len() { return unsupported(); }
        directory.data[directory.len] = b'/'; directory.len += 1;
    }
    match package(directory.bytes()) {
        Some(target) => ImportResolution::local(target, "go-module-package-file-candidate"),
        None => ImportResolution::without(Missing, "no-catalog-target"),
    }
}

/// Candidate files are direct .go members, excluding dot/underscore names and
/// _test.go. OS/architecture suffixes and build tags are not evaluated.
pub fn go_package_source(prefix: &[u8], path: &[u8]) -> bool {
    let Some(name) = path.strip_prefix(prefix) else { return false; };
    !name.is_empty() && !name.contains(&b'/') && !matches!(name[0], b'.' | b'_')
        && name.ends_with(b".go") && !name.ends_with(b"_test.go")
}
