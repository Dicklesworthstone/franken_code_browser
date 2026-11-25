#![forbid(unsafe_code)]

//! Bounded Rust path-dependency navigation from captured Cargo manifests.
//!
//! The pinned FrankenMarkdown lexer owns string/comment boundaries. This is a
//! finite metadata policy, not Cargo resolution: explicitly declared workspace
//! members may inherit local paths, but registry/git lookup, target/feature
//! selection, macro expansion and compiler binding are not inferred.
//! Metadata owns its admitted UTF-8 text, so archive
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
const MAX_WORKSPACE_PATTERNS: usize = 256;
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
#[derive(Clone, Copy, PartialEq, Eq)]
enum DependencyScope { Normal, Other, Workspace }
#[derive(Clone, Copy)]
struct Dependency {
    name: TextRange, path: Option<TextRange>, package: Option<TextRange>,
    evidence: TextRange, scope: DependencyScope, unsupported: bool,
    inherited: bool, sealed: bool, fields: u16,
}
impl Dependency {
    fn can_inherit(self) -> bool {
        self.scope == DependencyScope::Normal && self.inherited && !self.unsupported
            && self.path.is_none() && self.package.is_none() && self.fields & grammar::VERSION == 0
    }
}

/// A dependency path and its exact declaration in the importing capture.
/// Inheritance additionally retains the workspace declaration and its capture.
/// The target manifest must be independently supplied.
pub struct CargoDependencyLocation {
    path: CatalogPath, declaration: ByteRange, declaration_request: CaptureRequest,
    workspace_declaration: Option<(CaptureRequest, ByteRange)>,
}
impl CargoDependencyLocation {
    pub fn path(&self) -> &[u8] { self.path.bytes() }
    pub const fn declaration_range(&self) -> ByteRange { self.declaration }
    pub const fn declaration_request(&self) -> CaptureRequest { self.declaration_request }
    pub const fn workspace_declaration(&self) -> Option<(CaptureRequest, ByteRange)> { self.workspace_declaration }
}

pub struct CapturedCargoManifest {
    path: CatalogPath, text: String, request: CaptureRequest,
    package: Option<TextRange>, library_name: Option<TextRange>,
    library_path: Option<TextRange>, library_declared: bool, autolib: Option<bool>,
    linkable_library: Option<bool>, package_workspace: Option<TextRange>,
    workspace: bool, members_declared: bool, excludes_declared: bool,
    workspace_members: Vec<TextRange>, workspace_excludes: Vec<TextRange>,
    dependencies: Vec<Dependency>, _lease: ResourceLease,
}
impl CapturedCargoManifest {
    /// Recognize package/lib metadata, explicit workspace members and dependency
    /// inline tables, subtables or dotted fields. Virtual workspace roots are
    /// accepted without a package. Quoted keys, comments, CRLF, arrays, booleans and decimal
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
            .and_then(|n| n.checked_add(2 * MAX_WORKSPACE_PATTERNS * size_of::<TextRange>()))
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
            package_workspace: None, workspace: false, members_declared: false, excludes_declared: false,
            workspace_members: reserve(MAX_WORKSPACE_PATTERNS)?, workspace_excludes: reserve(MAX_WORKSPACE_PATTERNS)?,
            dependencies: reserve(MAX_DEPENDENCIES)?, _lease: lease };
        grammar::parse(text, &tokens, &mut manifest, &mut canceled)?;
        if manifest.package.is_none() && !manifest.workspace
            || manifest.package.is_some_and(|name| !package_name(name.text(&manifest.text)))
            || manifest.workspace && manifest.package_workspace.is_some() {
            return Err(ImportError::InvalidEvidence);
        }
        if manifest.library_name.is_some_and(|name| !crate_name(name.text(&manifest.text))) {
            return Err(ImportError::InvalidEvidence);
        }
        drop(tokens); drop(spans);
        let retained = size_of::<Self>().checked_add(manifest.text.capacity())
            .and_then(|n| n.checked_add(manifest.dependencies.capacity() * size_of::<Dependency>()))
            .and_then(|n| n.checked_add(manifest.workspace_members.capacity() * size_of::<TextRange>()))
            .and_then(|n| n.checked_add(manifest.workspace_excludes.capacity() * size_of::<TextRange>()))
            .ok_or(ImportError::ResourceDenied)?;
        manifest._lease.reconcile(ByteLength::new(retained as u64)).map_err(|_| ImportError::ResourceDenied)?;
        check(&mut canceled)?;
        Ok(manifest)
    }
    pub fn path(&self) -> &[u8] { self.path.bytes() }
    pub const fn request(&self) -> CaptureRequest { self.request }
    pub fn source_bytes(&self) -> &[u8] { self.text.as_bytes() }
    pub fn source_len(&self) -> usize { self.text.len() }

    pub const fn is_workspace(&self) -> bool { self.workspace }

    /// Whether this ordinary dependency requires an ancestor workspace capture.
    /// A package that is itself the workspace root uses its own metadata.
    pub fn needs_workspace(&self, fact: &ImportFact) -> bool {
        !self.workspace && self.selected(fact).ok().flatten().is_some_and(|d| d.can_inherit())
    }

    /// Return a unique ordinary path dependency, using this capture for a root
    /// package's own workspace declarations. Ancestor inheritance requires
    /// `dependency_with_workspace`; dev/build, conditional and competing aliases
    /// do not authorize a metadata load.
    pub fn dependency(&self, source: &[u8], fact: &ImportFact) -> Option<CargoDependencyLocation> {
        self.dependency_with_workspace(source, fact, None)
    }

    /// Plan a bounded metadata load from caller-supplied captures. Inherited
    /// paths are relative to the workspace root. Only explicitly listed members
    /// are admitted: exact relative directories and single-component `*`
    /// patterns, with exclusions checked first. Other glob syntax and implicit
    /// path-dependency membership remain unavailable.
    /// The resolver separately rechecks nearest catalog boundaries before use.
    pub fn dependency_with_workspace(&self, source: &[u8], fact: &ImportFact,
        workspace: Option<&Self>) -> Option<CargoDependencyLocation> {
        if ImportLanguage::from_path(source) != Some(ImportLanguage::Rust) || !valid_source_path(source)
            || !is_rust_dependency_import(fact) || !self.contains_source(source) { return None; }
        let declaration = self.selected(fact).ok().flatten()?;
        let (origin, dependency) = self.definition(fact, workspace)?;
        let mut path = relative_directory(origin.path(), dependency.path?.text(&origin.text))?;
        if !path.push(b"Cargo.toml") { return None; }
        Some(CargoDependencyLocation { path, declaration: declaration.evidence.original().ok()?,
            declaration_request: self.request,
            workspace_declaration: if declaration.inherited {
                Some((origin.request, dependency.evidence.original().ok()?))
            } else { None } })
    }

    fn definition<'a>(&'a self, fact: &ImportFact, workspace: Option<&'a Self>)
        -> Option<(&'a Self, &'a Dependency)> {
        let dependency = self.selected(fact).ok().flatten()?;
        if dependency.scope != DependencyScope::Normal || dependency.unsupported { return None; }
        if !dependency.inherited { return Some((self, dependency)); }
        if !dependency.can_inherit() { return None; }
        let workspace = if self.workspace { self } else { workspace? };
        if workspace.request.file().owner() != self.request.file().owner()
            || !workspace.admits_member(self) { return None; }
        let name = dependency.name.text(&self.text);
        // Cargo inheritance looks up the exact manifest key; Rust's hyphen to
        // underscore normalization only applies when selecting the source use.
        let mut matches = workspace.dependencies.iter().filter(|candidate|
            candidate.scope == DependencyScope::Workspace && candidate.name.text(&workspace.text) == name);
        let inherited = matches.next()?;
        if matches.next().is_some() || inherited.unsupported || inherited.inherited
            || inherited.fields & grammar::OPTIONAL != 0 { return None; }
        Some((workspace, inherited))
    }

    fn admits_member(&self, member: &Self) -> bool {
        if !self.workspace || member.package.is_none() { return false; }
        if self.path() == member.path() { return member.package_workspace.is_none(); }
        if member.workspace || !self.members_declared { return false; }
        if let Some(explicit) = member.package_workspace {
            let Some(mut root) = relative_directory(member.path(), explicit.text(&member.text)) else { return false; };
            if !root.push(b"Cargo.toml") || root.bytes() != self.path() { return false; }
        }
        let prefix = &self.path()[..self.path().len() - b"Cargo.toml".len()];
        let Some(relative) = member.path().strip_prefix(prefix)
            .and_then(|path| path.strip_suffix(b"/Cargo.toml")) else { return false; };
        if relative.is_empty()
            || self.workspace_members.iter().chain(&self.workspace_excludes)
                .any(|pattern| !workspace_pattern(pattern.text(&self.text))) { return false; }
        if self.workspace_excludes.iter().any(|pattern| member_matches(pattern.text(&self.text), relative)) {
            return false;
        }
        self.workspace_members.iter().any(|pattern| member_matches(pattern.text(&self.text), relative))
    }
    fn contains_source(&self, source: &[u8]) -> bool {
        let root = &self.path()[..self.path().len() - b"Cargo.toml".len()];
        source.starts_with(root)
    }
    fn selected(&self, fact: &ImportFact) -> Result<Option<&Dependency>, ()> {
        let name = dependency_parts(fact).ok_or(())?.0;
        let mut found = None;
        for dependency in &self.dependencies {
            if dependency.scope != DependencyScope::Workspace && alias_matches(dependency.name.text(&self.text), name) {
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

/// Locate the one nearest ancestor metadata boundary for inherited dependencies.
/// A missing/unsupported capture or a non-workspace manifest at that boundary
/// never authorizes continuing into an outer workspace. Root packages need no
/// second capture. The ancestor walk shares the 64-directory Cargo search cap.
pub fn find_cargo_workspace_manifest<T>(importing: &CapturedCargoManifest,
    lookup: impl FnMut(&[u8]) -> Option<T>) -> Option<CargoManifestLocation<T>> {
    if importing.workspace || !importing.dependencies.iter().any(|d| d.can_inherit()) { return None; }
    let directory = CatalogPath::parent(importing.path())?;
    if directory.len == 0 { return None; }
    find_cargo_manifest(directory.bytes(), lookup)
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
    lookup: impl FnMut(&[u8]) -> Option<T>) -> ImportResolution<T> {
    resolve_rust_dependency_import_with_workspace(source, fact, importing, None, target, lookup)
}

/// Resolve from an importing package, an optional nearest ancestor workspace
/// and a target package in the same captured catalog. The workspace argument is
/// unnecessary for a root package. Every path remains a lexical candidate,
/// without Cargo execution, registry access or feature/target selection.
pub fn resolve_rust_dependency_import_with_workspace<T: Copy + Eq>(source: &[u8], fact: &ImportFact,
    importing: Option<&CapturedCargoManifest>, workspace: Option<&CapturedCargoManifest>,
    target: Option<&CapturedCargoManifest>, mut lookup: impl FnMut(&[u8]) -> Option<T>) -> ImportResolution<T> {
    if !valid_source_path(source) || ImportLanguage::from_path(source) != Some(ImportLanguage::Rust) {
        return unresolved();
    }
    let Some((_, tail)) = dependency_parts(fact) else { return unresolved(); };
    let Some(importing) = importing else { return unresolved(); };
    let Some(boundary) = find_cargo_manifest(source, &mut lookup) else { return unresolved(); };
    if boundary.path() != importing.path() { return unresolved(); }
    match importing.selected(fact) {
        Ok(Some(_)) => {},
        Err(()) => return ImportResolution::without(ImportResolutionKind::Ambiguous, "ambiguous-catalog-target"),
        Ok(None) => return unresolved(),
    }
    if importing.needs_workspace(fact) {
        let Some(workspace) = workspace else { return unresolved(); };
        let Some(boundary) = find_cargo_workspace_manifest(importing, &mut lookup) else { return unresolved(); };
        if boundary.path() != workspace.path() { return unresolved(); }
    }
    let Some((origin, dependency)) = importing.definition(fact, workspace) else { return unresolved(); };
    let Some(location) = importing.dependency_with_workspace(source, fact, workspace) else { return unresolved(); };
    let Some(target) = target else { return unresolved(); };
    if location.path() != target.path()
        || target.request.file().owner() != importing.request.file().owner() { return unresolved(); }
    let expected = dependency.package.unwrap_or(dependency.name).text(&origin.text);
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


// Deliberately smaller than Cargo's glob grammar: a wildcard is one complete
// directory component. Reject unsupported exclusions as well as members, so
// uncertainty can never silently turn an excluded package into a member.
fn workspace_pattern(pattern: &str) -> bool {
    !pattern.is_empty() && pattern.len() <= MAX_IMPORT_SPECIFIER_BYTES
        && !pattern.starts_with('/') && !pattern.ends_with('/')
        && !pattern.chars().any(|c| c.is_control() || matches!(c, '\\' | ':' | '"' | '\'' | '?' | '[' | ']'))
        && pattern.split('/').count() <= MAX_COMPONENTS
        && pattern.split('/').all(|part| !matches!(part, "" | "..")
            && (part == "*" || !part.contains('*')))
}
fn member_matches(pattern: &str, member: &[u8]) -> bool {
    let mut patterns = pattern.as_bytes().split(|&byte| byte == b'/').filter(|part| *part != b".");
    let mut components = member.split(|&byte| byte == b'/');
    loop {
        match (patterns.next(), components.next()) {
            (None, None) => return true,
            (Some(pattern), Some(component)) if pattern == b"*" || pattern == component => {}
            _ => return false,
        }
    }
}
