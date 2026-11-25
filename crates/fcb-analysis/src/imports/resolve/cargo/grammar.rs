#![forbid(unsafe_code)]

//! Finite Cargo metadata grammar over the pinned TOML lexical terminals.
//! Unsupported source never authorizes a partial path-dependency binding.

use super::{CapturedCargoManifest, Dependency, ImportError, MAX_DEPENDENCIES,
    Terminal, TextRange, Tok, check, package_name};

const MAX_KEYS: usize = 8;
const MAX_DEPTH: usize = 32;
const MAX_INLINE_FIELDS: usize = 32;

#[derive(Clone, Copy)]
struct Keys { parts: [TextRange; MAX_KEYS], len: usize }
impl Keys {
    fn name<'a>(&self, index: usize, text: &'a str) -> &'a str { self.parts[index].text(text) }
}
#[derive(Clone, Copy)]
enum Table { Other, Package, Library, Dependencies(bool), Dependency(usize) }
struct Grammar<'a> { text: &'a str, tokens: &'a [Terminal] }
impl<'a> Grammar<'a> {
    fn word(&self, at: usize) -> &'a str {
        self.tokens.get(at).map_or("", |token| token.text(self.text))
    }
    fn newline(&self, at: usize) -> bool {
        at > 0 && at < self.tokens.len()
            && self.text[self.tokens[at - 1].end..self.tokens[at].start].contains(['\r', '\n'])
    }
    fn range(&self, first: usize, end: usize) -> Result<TextRange, ImportError> {
        if first >= end || end > self.tokens.len() { return Err(ImportError::InvalidEvidence); }
        Ok(TextRange { start: self.tokens[first].start, end: self.tokens[end - 1].end })
    }
    fn string(&self, first: usize, end: usize) -> Result<TextRange, ImportError> {
        if end != first + 1 || self.tokens.get(first).is_none_or(|t| t.kind != Tok::Str) {
            return Err(ImportError::InvalidEvidence);
        }
        let span = self.range(first, end)?;
        Ok(TextRange { start: span.start + 1, end: span.end - 1 })
    }
    fn keys(&self, mut at: usize) -> Result<(Keys, usize), ImportError> {
        let mut keys = Keys { parts: [TextRange::default(); MAX_KEYS], len: 0 };
        loop {
            if keys.len == MAX_KEYS || at >= self.tokens.len() { return Err(ImportError::InvalidEvidence); }
            let start = at;
            let key = if self.tokens[at].kind == Tok::Str {
                at += 1;
                self.string(start, at)?
            } else {
                while at < self.tokens.len()
                    && self.word(at).bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-'))
                    && !self.word(at).is_empty()
                    && (at == start || self.tokens[at - 1].end == self.tokens[at].start) {
                    at += 1;
                }
                self.range(start, at)?
            };
            keys.parts[keys.len] = key; keys.len += 1;
            if self.word(at) != "." { return Ok((keys, at)); }
            if self.newline(at) || self.newline(at + 1) { return Err(ImportError::InvalidEvidence); }
            at += 1;
        }
    }
    fn value(&self, at: usize, depth: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, ImportError> {
        check(canceled)?;
        if depth > MAX_DEPTH || at >= self.tokens.len() { return Err(ImportError::InvalidEvidence); }
        if self.tokens[at].kind == Tok::Str { return Ok(at + 1); }
        match self.word(at) {
            "[" => {
                let mut next = at + 1;
                while self.word(next) != "]" {
                    next = self.value(next, depth + 1, canceled)?;
                    match self.word(next) {
                        "," => next += 1,
                        "]" => break,
                        _ => return Err(ImportError::InvalidEvidence),
                    }
                }
                Ok(next + 1)
            }
            "{" => {
                let mut next = at + 1;
                if self.word(next) == "}" { return Ok(next + 1); }
                loop {
                    let (_, equal) = self.keys(next)?;
                    if self.word(equal) != "=" { return Err(ImportError::InvalidEvidence); }
                    let end = self.value(equal + 1, depth + 1, canceled)?;
                    if (next..=end.min(self.tokens.len().saturating_sub(1))).any(|i| self.newline(i)) {
                        return Err(ImportError::InvalidEvidence);
                    }
                    match self.word(end) {
                        "}" => return Ok(end + 1),
                        "," => { next = end + 1; if self.word(next) == "}" { return Err(ImportError::InvalidEvidence); } }
                        _ => return Err(ImportError::InvalidEvidence),
                    }
                }
            }
            _ => {
                let mut end = at + 1;
                while end < self.tokens.len() && self.tokens[end - 1].end == self.tokens[end].start
                    && !matches!(self.word(end), "," | "]" | "}" | "=" | "[" | "{") { end += 1; }
                let raw = self.range(at, end)?.text(self.text);
                let number = raw.strip_prefix('+').or_else(|| raw.strip_prefix('-')).unwrap_or(raw);
                let integer = !number.is_empty() && number.as_bytes()[0].is_ascii_digit()
                    && number.as_bytes()[number.len() - 1].is_ascii_digit()
                    && !number.contains("__") && number.bytes().all(|b| b.is_ascii_digit() || b == b'_');
                if matches!(raw, "true" | "false") || integer { Ok(end) }
                else { Err(ImportError::UnsupportedLexicalContext) }
            }
        }
    }
}

pub(super) fn parse(text: &str, tokens: &[Terminal], manifest: &mut CapturedCargoManifest,
    canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
    let grammar = Grammar { text, tokens };
    let mut table = Table::Other;
    let (mut package_seen, mut library_seen, mut dependencies_seen) = (false, false, false);
    let mut at = 0;
    while at < tokens.len() {
        check(canceled)?;
        if at != 0 && !grammar.newline(at) { return Err(ImportError::InvalidEvidence); }
        if grammar.word(at) == "[" {
            let start = at;
            let array = grammar.word(at + 1) == "[";
            let (keys, closing) = grammar.keys(at + 1 + usize::from(array))?;
            if grammar.word(closing) != "]" || array && grammar.word(closing + 1) != "]" {
                return Err(ImportError::InvalidEvidence);
            }
            at = closing + 1 + usize::from(array);
            if (start + 1..at).any(|i| grammar.newline(i))
                || at < tokens.len() && !grammar.newline(at) {
                return Err(ImportError::InvalidEvidence);
            }
            let first = keys.name(0, text);
            let last = keys.name(keys.len - 1, text);
            let group = |name: &str| matches!(name, "dependencies" | "dev-dependencies" | "build-dependencies");
            if array && (matches!(first, "package" | "lib") || group(last)
                || keys.len >= 2 && group(keys.name(keys.len - 2, text))) {
                return Err(ImportError::InvalidEvidence);
            }
            table = if array { Table::Other }
                else if keys.len == 1 && first == "package" {
                    if package_seen { return Err(ImportError::InvalidEvidence); }
                    package_seen = true; Table::Package
                } else if keys.len == 1 && first == "lib" {
                    if library_seen { return Err(ImportError::InvalidEvidence); }
                    library_seen = true; manifest.library_declared = true; Table::Library
                } else if first != "workspace" && group(last) {
                    let normal = keys.len == 1 && first == "dependencies";
                    if normal && dependencies_seen { return Err(ImportError::InvalidEvidence); }
                    dependencies_seen |= normal;
                    Table::Dependencies(normal)
                } else if first != "workspace" && keys.len >= 2 && group(keys.name(keys.len - 2, text)) {
                    let normal = keys.len == 2 && first == "dependencies";
                    let index = add_dependency(manifest, keys.parts[keys.len - 1],
                        grammar.range(start, at)?, normal, text)?;
                    Table::Dependency(index)
                } else { Table::Other };
            continue;
        }
        let start = at;
        let (keys, equal) = grammar.keys(at)?;
        if grammar.word(equal) != "=" || (start + 1..=equal).any(|i| grammar.newline(i))
            || grammar.newline(equal + 1) { return Err(ImportError::InvalidEvidence); }
        let value = equal + 1;
        let end = grammar.value(value, 0, canceled)?;
        if end < tokens.len() && !grammar.newline(end) { return Err(ImportError::InvalidEvidence); }
        let key = keys.name(0, text);
        let evidence = grammar.range(start, end)?;
        match table {
            Table::Package if matches!(key, "name" | "autolib") => {
                if keys.len != 1 { return Err(ImportError::UnsupportedLexicalContext); }
                if key == "name" {
                    set_string(&mut manifest.package, grammar.string(value, end)?)?;
                } else {
                    if manifest.autolib.is_some() || end != value + 1 { return Err(ImportError::InvalidEvidence); }
                    manifest.autolib = Some(match grammar.word(value) {
                        "true" => true, "false" => false, _ => return Err(ImportError::InvalidEvidence),
                    });
                }
            }
            Table::Library if matches!(key, "name" | "path" | "crate-type" | "crate_type") => {
                if keys.len != 1 { return Err(ImportError::UnsupportedLexicalContext); }
                if matches!(key, "crate-type" | "crate_type") {
                    if manifest.linkable_library.is_some() { return Err(ImportError::InvalidEvidence); }
                    manifest.linkable_library = Some(library_types(&grammar, value, end)?);
                } else {
                    let destination = if key == "name" { &mut manifest.library_name } else { &mut manifest.library_path };
                    set_string(destination, grammar.string(value, end)?)?;
                }
            }
            Table::Dependencies(normal) => {
                if keys.len != 1 { return Err(ImportError::UnsupportedLexicalContext); }
                let index = add_dependency(manifest, keys.parts[0], evidence, normal, text)?;
                if grammar.word(value) == "{" {
                    inline_dependency(&grammar, value, end, &mut manifest.dependencies[index], canceled)?;
                } else { manifest.dependencies[index].unsupported = true; }
            }
            Table::Dependency(index) => {
                if keys.len != 1 { return Err(ImportError::UnsupportedLexicalContext); }
                dependency_field(&grammar, key, value, end, &mut manifest.dependencies[index])?;
                manifest.dependencies[index].evidence.end = evidence.end;
            }
            _ => {}
        }
        at = end;
    }
    Ok(())
}

fn set_string(target: &mut Option<TextRange>, value: TextRange) -> Result<(), ImportError> {
    if target.is_some() { return Err(ImportError::InvalidEvidence); }
    *target = Some(value); Ok(())
}
fn add_dependency(manifest: &mut CapturedCargoManifest, name: TextRange, evidence: TextRange,
    normal: bool, text: &str) -> Result<usize, ImportError> {
    if manifest.dependencies.len() == MAX_DEPENDENCIES { return Err(ImportError::SourceLimit); }
    if !package_name(name.text(text)) { return Err(ImportError::InvalidEvidence); }
    if manifest.dependencies.iter().any(|d| d.normal == normal && d.name.text(text) == name.text(text)) {
        return Err(ImportError::InvalidEvidence);
    }
    let index = manifest.dependencies.len();
    manifest.dependencies.push(Dependency { name, path: None, package: None, evidence, normal, unsupported: false });
    Ok(index)
}
fn dependency_field(grammar: &Grammar<'_>, key: &str, first: usize, end: usize, dependency: &mut Dependency)
    -> Result<(), ImportError> {
    match key {
        "path" => set_string(&mut dependency.path, grammar.string(first, end)?)?,
        "package" => {
            let name = grammar.string(first, end)?;
            if !package_name(name.text(grammar.text)) { return Err(ImportError::InvalidEvidence); }
            set_string(&mut dependency.package, name)?;
        }
        "workspace" | "git" | "registry" => dependency.unsupported = true,
        _ => {}
    }
    Ok(())
}
fn inline_dependency(grammar: &Grammar<'_>, first: usize, end: usize, dependency: &mut Dependency,
    canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
    let mut keys = [TextRange::default(); MAX_INLINE_FIELDS];
    let mut count = 0;
    let mut at = first + 1;
    while at + 1 < end {
        let (key, equal) = grammar.keys(at)?;
        if key.len != 1 || count == MAX_INLINE_FIELDS { return Err(ImportError::UnsupportedLexicalContext); }
        let name = key.parts[0];
        if keys[..count].iter().any(|old| old.text(grammar.text) == name.text(grammar.text)) {
            return Err(ImportError::InvalidEvidence);
        }
        keys[count] = name; count += 1;
        let value = equal + 1;
        let next = grammar.value(value, 1, canceled)?;
        dependency_field(grammar, name.text(grammar.text), value, next, dependency)?;
        if grammar.word(next) == "}" { break; }
        at = next + 1;
    }
    Ok(())
}

fn library_types(grammar: &Grammar<'_>, first: usize, end: usize) -> Result<bool, ImportError> {
    if grammar.word(first) != "[" || end < first + 2 || grammar.word(end - 1) != "]" {
        return Err(ImportError::InvalidEvidence);
    }
    let mut usable = false;
    let mut at = first + 1;
    while at < end - 1 {
        let name = grammar.string(at, at + 1)?.text(grammar.text);
        match name {
            "lib" | "rlib" | "dylib" | "proc-macro" => usable = true,
            "bin" | "cdylib" | "staticlib" => {}
            _ => return Err(ImportError::UnsupportedLexicalContext),
        }
        at += 1;
        if at < end - 1 {
            if grammar.word(at) != "," { return Err(ImportError::InvalidEvidence); }
            at += 1;
        }
    }
    Ok(usable)
}
