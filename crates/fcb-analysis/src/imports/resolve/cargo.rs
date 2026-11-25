#![forbid(unsafe_code)]

//! Bounded Rust path-dependency navigation from captured Cargo manifests.
//!
//! The pinned FrankenMarkdown lexer owns string/comment boundaries. This is a
//! finite metadata policy, not Cargo resolution: no workspace inheritance,
//! registry/git lookup, target/feature selection, macro expansion or compiler
//! binding is inferred. Metadata owns its admitted UTF-8 text, so archive
//! loaders can release temporary captures before loading another manifest.

mod grammar;
#[cfg(test)]
mod tests;

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::CaptureRequest;
use franken_markdown::highlight::{Span, Tok, highlight_into};
use super::{CatalogPath, ImportFact, ImportKind, ImportLanguage, ImportResolution,
    ImportResolutionKind, candidates, valid_source_path};
use super::super::{ImportError, MAX_IMPORT_SPECIFIER_BYTES, Terminal, check, reserve, terminals};

pub const MAX_CARGO_MANIFEST_SOURCE_BYTES: usize = 64 * 1024;
const MAX_DEPENDENCIES: usize = 256;
const MAX_COMPONENTS: usize = 128;
const MAX_ANCESTORS: usize = 64;
const CONTEXT: &str = "rust-dependency-context-unavailable";

pub struct CargoManifestLocation<T> { pub target: T, path: CatalogPath }
impl<T> CargoManifestLocation<T> { pub fn path(&self) -> &[u8] { self.path.bytes() } }

/// The nearest catalog boundary wins even when its bytes are not captured.
/// Never use an outer package after finding an unavailable nested Cargo.toml.
pub fn find_cargo_manifest<T>(source: &[u8], mut lookup: impl FnMut(&[u8]) -> Option<T>)
    -> Option<CargoManifestLocation<T>> {
    let mut path = CatalogPath::parent(source)?;
    for _ in 0..MAX_ANCESTORS {
        let parent = path.len;
        if !path.push(b"Cargo.toml") { return None; }
        if let Some(target) = lookup(path.bytes()) { return Some(CargoManifestLocation { target, path }); }
        path.len = parent;
        if parent == 0 { return None; }
        path.len = path.bytes().iter().rposition(|&byte| byte == b'/').unwrap_or(0);
    }
    None
}

#[derive(Clone, Copy, Default)]
struct TextRange { start: usize, end: usize }
impl TextRange {
    fn text(self, source: &str) -> &str { &source[self.start..self.end] }
    fn original(self) -> Result<ByteRange, ImportError> {
        ByteRange::new(ByteOffset::new(self.start as u64), ByteOffset::new(self.end as u64))
            .map_err(|_| ImportError::InvalidEvidence)
    }
}
#[derive(Clone, Copy)]
struct Dependency {
    name: TextRange, path: Option<TextRange>, package: Option<TextRange>,
    evidence: TextRange, normal: bool, unsupported: bool,
}

/// An explicit dependency path and its exact declaration in the importing
/// manifest's capture. The target manifest must be independently supplied.
pub struct CargoDependencyLocation { path: CatalogPath, declaration: ByteRange }
impl CargoDependencyLocation {
    pub fn path(&self) -> &[u8] { self.path.bytes() }
    pub const fn declaration_range(&self) -> ByteRange { self.declaration }
}

pub struct CapturedCargoManifest {
    path: CatalogPath, text: String, request: CaptureRequest,
    package: Option<TextRange>, library_name: Option<TextRange>,
    library_path: Option<TextRange>, library_declared: bool, autolib: Option<bool>,
    linkable_library: Option<bool>,
    dependencies: Vec<Dependency>, _lease: ResourceLease,
}
impl CapturedCargoManifest {
    /// Recognize package/lib metadata and direct dependency inline tables or
    /// subtables. Quoted keys, comments, CRLF, arrays, booleans and decimal
    /// integers are accepted. Unrelated dotted assignments are ignored after
    /// bounded validation. Multiline/escaped strings and other TOML scalar
    /// forms explicitly refuse the whole metadata file.
    /// This finite grammar does not certify full Cargo-manifest validity.
    pub fn build(path: &[u8], source: &[u8], request: CaptureRequest,
        budget: &ResourceBudget, allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<Self, ImportError> {
        if source.len() > MAX_CARGO_MANIFEST_SOURCE_BYTES { return Err(ImportError::SourceLimit); }
        if request.range().is_some() || !valid_source_path(path)
            || path.rsplit(|&byte| byte == b'/').next() != Some(b"Cargo.toml".as_slice()) {
            return Err(ImportError::InvalidEvidence);
        }
        check(&mut canceled)?;
        let text = std::str::from_utf8(source).map_err(|_| ImportError::UnsupportedEncoding)?;
        if text.chars().any(|c| c.is_control() && !matches!(c, '\n' | '\r' | '\t')) {
            return Err(ImportError::InvalidEvidence);
        }
        let charge = source.len().max(1).checked_mul(128)
            .and_then(|n| n.checked_add(MAX_DEPENDENCIES * size_of::<Dependency>()))
            .and_then(|n| n.checked_add(size_of::<Self>() + 16 * 1024))
            .ok_or(ImportError::ResourceDenied)?;
        let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| ImportError::ResourceDenied)?;
        let mut spans = reserve::<Span>(text.len().max(1))?;
        highlight_into("toml", text, &mut spans);
        if spans.capacity() > text.len().max(1) { return Err(ImportError::InvalidEvidence); }
        for span in &spans {
            check(&mut canceled)?;
            let word = text.get(span.start..span.end).ok_or(ImportError::InvalidEvidence)?;
            if span.kind == Tok::Str {
                let bytes = word.as_bytes();
                if bytes.len() < 2 || !matches!(bytes[0], b'"' | b'\'')
                    || bytes.last() != Some(&bytes[0]) || word.contains(['\n', '\r', '\\'])
                    || bytes.get(1) == Some(&bytes[0]) && bytes.get(2) == Some(&bytes[0]) {
                    return Err(ImportError::UnsupportedLexicalContext);
                }
            } else if span.kind != Tok::Comment && word.contains(['"', '\'']) {
                return Err(ImportError::UnsupportedLexicalContext);
            }
        }
        let tokens = terminals(text, &spans, &mut canceled)?;
        let mut owned = String::new();
        owned.try_reserve_exact(text.len()).map_err(|_| ImportError::ResourceDenied)?;
        if owned.capacity() > text.len() { return Err(ImportError::ResourceDenied); }
        owned.push_str(text);
        let mut manifest_path = CatalogPath::parent(path).ok_or(ImportError::InvalidEvidence)?;
        if !manifest_path.push(b"Cargo.toml") { return Err(ImportError::InvalidEvidence); }
        let mut manifest = Self { path: manifest_path, text: owned, request, package: None,
            library_name: None, library_path: None, library_declared: false, autolib: None, linkable_library: None,
            dependencies: reserve(MAX_DEPENDENCIES)?, _lease: lease };
        grammar::parse(text, &tokens, &mut manifest, &mut canceled)?;
        let name = manifest.package.ok_or(ImportError::InvalidEvidence)?.text(&manifest.text);
        if !package_name(name) { return Err(ImportError::InvalidEvidence); }
        if manifest.library_name.is_some_and(|name| !crate_name(name.text(&manifest.text))) {
            return Err(ImportError::InvalidEvidence);
        }
        drop(tokens); drop(spans);
        let retained = size_of::<Self>().checked_add(manifest.text.capacity())
            .and_then(|n| n.checked_add(manifest.dependencies.capacity() * size_of::<Dependency>()))
            .ok_or(ImportError::ResourceDenied)?;
        manifest._lease.reconcile(ByteLength::new(retained as u64)).map_err(|_| ImportError::ResourceDenied)?;
        check(&mut canceled)?;
        Ok(manifest)
    }
    pub fn path(&self) -> &[u8] { self.path.bytes() }
    pub const fn request(&self) -> CaptureRequest { self.request }
    pub fn source_bytes(&self) -> &[u8] { self.text.as_bytes() }
    pub fn source_len(&self) -> usize { self.text.len() }

    /// Return a unique ordinary path dependency. Inherited, conditional,
    /// dev/build or competing aliases do not authorize a metadata load.
    pub fn dependency(&self, source: &[u8], fact: &ImportFact) -> Option<CargoDependencyLocation> {
        if ImportLanguage::from_path(source) != Some(ImportLanguage::Rust) || !valid_source_path(source)
            || !is_rust_dependency_import(fact) || !self.contains_source(source) { return None; }
        let dependency = self.selected(fact).ok().flatten()?;
        if !dependency.normal || dependency.unsupported { return None; }
        let mut path = relative_directory(self.path(), dependency.path?.text(&self.text))?;
        if !path.push(b"Cargo.toml") { return None; }
        Some(CargoDependencyLocation { path, declaration: dependency.evidence.original().ok()? })
    }
    fn contains_source(&self, source: &[u8]) -> bool {
        let root = &self.path()[..self.path().len() - b"Cargo.toml".len()];
        source.starts_with(root)
    }
    fn selected(&self, fact: &ImportFact) -> Result<Option<&Dependency>, ()> {
        let name = dependency_parts(fact).ok_or(())?.0;
        let mut found = None;
        for dependency in &self.dependencies {
            if alias_matches(dependency.name.text(&self.text), name) {
                if found.is_some() { return Err(()); }
                found = Some(dependency);
            }
        }
        Ok(found)
    }
    fn library(&self) -> Option<CatalogPath> {
        if self.autolib == Some(false) && !self.library_declared || self.linkable_library == Some(false) { return None; }
        let relative = self.library_path.map_or("src/lib.rs", |path| path.text(&self.text));
        if !safe_relative_file(relative) || !relative.ends_with(".rs") { return None; }
        let path = CatalogPath::relative(self.path(), relative.as_bytes())?;
        self.contains_source(path.bytes()).then_some(path)
    }
}

/// External-leading use paths only. Standard-library and qualified local
/// imports retain their existing behavior.
pub fn is_rust_dependency_import(fact: &ImportFact) -> bool { dependency_parts(fact).is_some() }

fn dependency_parts(fact: &ImportFact) -> Option<(&str, &str)> {
    if fact.kind() != ImportKind::RustUse || !fact.resolution_allowed()
        || fact.rust_module_context().is_none() || fact.destination().len() > MAX_IMPORT_SPECIFIER_BYTES {
        return None;
    }
    let path = fact.destination().strip_prefix("::").unwrap_or(fact.destination());
    if path.ends_with("::") { return None; }
    let (head, tail) = path.split_once("::").unwrap_or((path, ""));
    let head = head.strip_prefix("r#").unwrap_or(head);
    if !crate_name(head) || matches!(head, "crate" | "self" | "super" | "Self" | "std" | "core" | "alloc") {
        return None;
    }
    let count = if tail.is_empty() { 0 } else { tail.split("::").count() };
    if count > MAX_COMPONENTS { return None; }
    for (ordinal, part) in tail.split("::").take(count).enumerate() {
        if part == "*" && ordinal + 1 == count { continue; }
        let part = part.strip_prefix("r#").unwrap_or(part);
        if !crate_name(part) || matches!(part, "crate" | "self" | "super" | "Self") { return None; }
    }
    Some((head, tail))
}

fn unresolved<T>() -> ImportResolution<T> {
    ImportResolution::without(ImportResolutionKind::Unresolved, CONTEXT)
}

/// Resolve from two caller-supplied captured manifests and the closed catalog.
/// The nearest metadata boundaries are rechecked. A missing type/item name may
/// open the last known file-module prefix; this is not declaration resolution.
pub fn resolve_rust_dependency_import<T: Copy + Eq>(source: &[u8], fact: &ImportFact,
    importing: Option<&CapturedCargoManifest>, target: Option<&CapturedCargoManifest>,
    mut lookup: impl FnMut(&[u8]) -> Option<T>) -> ImportResolution<T> {
    if !valid_source_path(source) || ImportLanguage::from_path(source) != Some(ImportLanguage::Rust) {
        return unresolved();
    }
    let Some((_, tail)) = dependency_parts(fact) else { return unresolved(); };
    let Some(importing) = importing else { return unresolved(); };
    let Some(boundary) = find_cargo_manifest(source, &mut lookup) else { return unresolved(); };
    if boundary.path() != importing.path() { return unresolved(); }
    let dependency = match importing.selected(fact) {
        Ok(Some(value)) => value,
        Err(()) => return ImportResolution::without(ImportResolutionKind::Ambiguous, "ambiguous-catalog-target"),
        Ok(None) => return unresolved(),
    };
    let Some(location) = importing.dependency(source, fact) else { return unresolved(); };
    let Some(target) = target else { return unresolved(); };
    if location.path() != target.path()
        || target.request.file().owner() != importing.request.file().owner() { return unresolved(); }
    let expected = dependency.package.unwrap_or(dependency.name).text(&importing.text);
    if target.package.is_none_or(|name| name.text(&target.text) != expected) { return unresolved(); }
    // Without an explicit package rename, do not infer an overriding lib-name
    // binding from a dependency key with a different spelling.
    if dependency.package.is_none() && target.library_name
        .is_some_and(|name| !alias_matches(name.text(&target.text), expected)) { return unresolved(); }
    let Some(library) = target.library() else { return unresolved(); };
    let Some(boundary) = find_cargo_manifest(library.bytes(), &mut lookup) else { return unresolved(); };
    if boundary.path() != target.path() { return unresolved(); }
    let Some(mut containing) = lookup(library.bytes()) else {
        return ImportResolution::without(ImportResolutionKind::Missing, "no-catalog-target");
    };
    let Some(mut path) = CatalogPath::parent(library.bytes()) else { return unresolved(); };
    for part in tail.split("::") {
        if part.is_empty() || part == "*" { break; }
        let part = part.strip_prefix("r#").unwrap_or(part);
        if !path.push(part.as_bytes()) { return unresolved(); }
        let base = path.len;
        if !path.push(b"Cargo.toml") { return unresolved(); }
        let nested = lookup(path.bytes()).is_some();
        path.len = base;
        if nested { return unresolved(); }
        let candidate = candidates(&mut path, &[b".rs", b"/mod.rs"], "rust-path-dependency-file-candidate", &mut lookup);
        match candidate.kind {
            ImportResolutionKind::Local => {
                let Some(value) = candidate.target else { return unresolved(); };
                containing = value;
            }
            ImportResolutionKind::Missing => return ImportResolution::local(containing, "rust-path-dependency-prefix-candidate"),
            _ => return candidate,
        }
    }
    ImportResolution::local(containing, "rust-path-dependency-file-candidate")
}

fn package_name(name: &str) -> bool {
    name.len() <= MAX_IMPORT_SPECIFIER_BYTES
        && name.as_bytes().first().is_some_and(|b| *b == b'_' || b.is_ascii_alphabetic())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
}
fn crate_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    name != "_" && bytes.next().is_some_and(|b| b == b'_' || b.is_ascii_alphabetic())
        && bytes.all(|b| b == b'_' || b.is_ascii_alphanumeric())
}
fn alias_matches(alias: &str, name: &str) -> bool {
    alias.bytes().map(|b| if b == b'-' { b'_' } else { b })
        .eq(name.bytes().map(|b| if b == b'-' { b'_' } else { b }))
}
fn safe_relative_file(path: &str) -> bool {
    !path.is_empty() && path.len() <= MAX_IMPORT_SPECIFIER_BYTES && !path.starts_with('/')
        && !path.ends_with('/') && !path.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '"' | '\''))
        && path.split('/').count() <= MAX_COMPONENTS
        && !matches!(path.rsplit('/').next(), Some("" | "." | ".."))
}
fn relative_directory(source: &[u8], destination: &str) -> Option<CatalogPath> {
    if destination.is_empty() || destination.len() > MAX_IMPORT_SPECIFIER_BYTES || destination.starts_with('/')
        || destination.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '"' | '\''))
        || destination.split('/').count() > MAX_COMPONENTS { return None; }
    let mut path = CatalogPath::parent(source)?;
    for part in destination.trim_end_matches('/').split('/') {
        match part {
            "" => return None,
            "." => {}
            ".." => {
                if path.len == 0 { return None; }
                path.len = path.bytes().iter().rposition(|&byte| byte == b'/').unwrap_or(0);
            }
            part => if !path.push(part.as_bytes()) { return None; },
        }
    }
    Some(path)
}
