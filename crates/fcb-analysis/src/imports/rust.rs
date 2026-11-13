#![forbid(unsafe_code)]

//! Source-specific use-tree grammar over FrankenMarkdown's validated terminals.
//! A whole statement is validated before any normalized leaf is published.
//! There is no local string/comment lexer, compiler scope or Cargo discovery.

use super::{ImportError, ImportKind, MAX_IMPORT_SPECIFIER_BYTES, MAX_STATEMENT_BYTES,
    MAX_STATEMENT_TERMINALS, Statements, Terminal, Tok, check, identifier};

const MAX_USE_TREE_DEPTH: usize = 32;

impl Statements<'_> {
    pub(super) fn rust(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        // A path override can change the module interpretation of other uses.
        // Keep their evidence, but admit no conventional-path targets in that
        // file. Nested inline modules and macro bodies likewise lack a modeled
        // module context, even when a leaf begins with an explicit qualifier.
        let mut overrides = false;
        for i in 0..self.tokens.len() {
            if i & 255 == 0 { check(canceled)?; }
            overrides |= self.word(i) == "#" && self.word(i + 1) == "["
                && matches!(self.word(i + 2), "path" | "cfg_attr");
        }
        let mut depth = 0usize;
        let mut i = 0;
        while i < self.tokens.len() {
            check(canceled)?;
            if self.keyword(i, "mod") && !self.rust_raw_tail(i) {
                let raw = self.word(i + 1) == "r" && self.word(i + 2) == "#" && identifier(self.word(i + 3))
                    && self.tokens[i + 1].end == self.tokens[i + 2].start
                    && self.tokens[i + 2].end == self.tokens[i + 3].start;
                let name_end = i + if raw { 3 } else { 1 };
                let semicolon = name_end + 1;
                if self.word(semicolon) == ";" && (raw || identifier(self.word(i + 1))) {
                    let start = self.tokens[i + 1].start; let end = self.tokens[name_end].end;
                    let text = self.text;
                    self.add(ImportKind::RustModule, self.tokens[i].start, self.tokens[semicolon].end,
                        start, end, &text[start..end], depth == 0 && !overrides)?;
                    i = semicolon + 1; continue;
                }
            }
            if self.keyword(i, "use") && !self.rust_raw_tail(i) {
                let mut j = i + 1; let mut braces = 0usize;
                while j < self.tokens.len() && j - i <= MAX_STATEMENT_TERMINALS
                    && self.tokens[j].end - self.tokens[i].start <= MAX_STATEMENT_BYTES {
                    check(canceled)?;
                    let word = self.word(j);
                    if word == ";" && braces == 0 { break; }
                    if word == "}" && braces == 0 { break; }
                    if j > i + 1 && !self.rust_raw_tail(j)
                        && (self.keyword(j, "use") || self.keyword(j, "mod")) { break; }
                    if word == "{" { braces += 1; } else if word == "}" { braces -= 1; }
                    j += 1;
                }
                if self.word(j) == ";" && j > i + 1 && j - i <= MAX_STATEMENT_TERMINALS
                    && self.tokens[j].end - self.tokens[i].start <= MAX_STATEMENT_BYTES {
                    let text = self.text; let tokens = self.tokens;
                    let statement_start = tokens[i].start; let statement_end = tokens[j].end;
                    // Two bounded passes avoid a temporary expanded leaf tree
                    // (and avoid half-valid output if a later branch is bad).
                    if walk_use(text, tokens, i + 1, j, canceled, |_, _, _| Ok(()))? {
                        let allowed = depth == 0 && !overrides;
                        if !walk_use(text, tokens, i + 1, j, canceled, |path, start, end| {
                            self.add(ImportKind::RustUse, statement_start, statement_end,
                                start, end, path, allowed)
                        })? { return Err(ImportError::InvalidEvidence); }
                    } else {
                        self.diagnostics += 1;
                        let start = tokens[i + 1].start; let end = tokens[j - 1].end;
                        self.add(ImportKind::RustUse, statement_start, statement_end,
                            start, end, &text[start..end], false)?;
                    }
                    i = j + 1; continue;
                }
                self.diagnostics += 1;
            }
            match self.word(i) {
                "{" | "(" | "[" => depth = depth.saturating_add(1),
                "}" | ")" | "]" => depth = depth.saturating_sub(1), _ => {}
            }
            i += 1;
        }
        Ok(())
    }

    fn rust_raw_tail(&self, at: usize) -> bool {
        at >= 2 && self.word(at - 2) == "r" && self.word(at - 1) == "#"
            && self.tokens[at - 2].end == self.tokens[at - 1].start
            && self.tokens[at - 1].end == self.tokens[at].start
    }
}

fn walk_use(
    text: &str, tokens: &[Terminal], start: usize, end: usize,
    canceled: &mut impl FnMut() -> bool,
    mut emit: impl FnMut(&str, usize, usize) -> Result<(), ImportError>,
) -> Result<bool, ImportError> {
    let mut parser = UseTree { text, tokens, at: start, end, path: [0; MAX_IMPORT_SPECIFIER_BYTES],
        length: 0, canceled };
    Ok(parser.tree(0, &mut emit)? && parser.at == end)
}

struct UseTree<'a, 'c, C> {
    text: &'a str,
    tokens: &'a [Terminal],
    at: usize,
    end: usize,
    path: [u8; MAX_IMPORT_SPECIFIER_BYTES],
    length: usize,
    canceled: &'c mut C,
}

impl<C: FnMut() -> bool> UseTree<'_, '_, C> {
    fn word(&self) -> &str {
        if self.at < self.end { self.tokens[self.at].text(self.text) } else { "" }
    }
    fn colons(&self) -> bool {
        self.at + 1 < self.end && self.word() == ":"
            && self.tokens[self.at + 1].text(self.text) == ":"
            && self.tokens[self.at].end == self.tokens[self.at + 1].start
    }
    fn append(&mut self, part: &str) -> bool {
        let separator = if self.length != 0 && !self.path[..self.length].ends_with(b"::") { 2 } else { 0 };
        let Some(end) = self.length.checked_add(separator).and_then(|n| n.checked_add(part.len())) else { return false; };
        if end > self.path.len() { return false; }
        if separator != 0 { self.path[self.length..self.length + 2].copy_from_slice(b"::"); }
        self.path[self.length + separator..end].copy_from_slice(part.as_bytes());
        self.length = end;
        true
    }
    /// Return the exact terminal span, preserving a contiguous raw identifier.
    /// Keyword classification comes from the upstream lexer, not a second
    /// keyword table. The three module qualifiers have grammar roles here.
    fn name(&mut self, alias: bool) -> Option<(usize, usize)> {
        if self.at >= self.end { return None; }
        let token = self.tokens[self.at];
        if self.word() == "r" && self.at + 2 < self.end
            && self.tokens[self.at + 1].text(self.text) == "#" {
            let name = self.tokens[self.at + 2]; let word = name.text(self.text);
            if token.end != self.tokens[self.at + 1].start || self.tokens[self.at + 1].end != name.start
                || !identifier(word) || matches!(word, "crate" | "self" | "super" | "Self" | "_") {
                return None;
            }
            self.at += 3;
            return Some((token.start, name.end));
        }
        let word = token.text(self.text);
        if !identifier(word) || word == "Self" || (!alias && word == "_")
            || (token.kind == Tok::Keyword && word != "_"
                && (alias || !matches!(word, "crate" | "self" | "super"))) {
            return None;
        }
        self.at += 1;
        Some((token.start, token.end))
    }
    fn emit(&mut self, start: usize, end: usize,
        emit: &mut impl FnMut(&str, usize, usize) -> Result<(), ImportError>) -> Result<bool, ImportError> {
        check(self.canceled)?;
        let path = std::str::from_utf8(&self.path[..self.length]).map_err(|_| ImportError::InvalidEvidence)?;
        if path.is_empty() { return Ok(false); }
        emit(path, start, end)?;
        Ok(true)
    }
    fn tree(&mut self, depth: usize,
        emit: &mut impl FnMut(&str, usize, usize) -> Result<(), ImportError>) -> Result<bool, ImportError> {
        check(self.canceled)?;
        if depth > MAX_USE_TREE_DEPTH || self.at >= self.end { return Ok(false); }
        if self.word() == "{" { return self.group(depth, emit); }
        let inherited = self.length;
        let start = self.tokens[self.at].start;
        if self.colons() {
            if inherited != 0 || !self.append("::") { return Ok(false); }
            self.at += 2;
        }
        if self.word() == "{" { return self.group(depth, emit); }
        if self.word() == "*" {
            let end = self.tokens[self.at].end; self.at += 1;
            return if self.append("*") { self.emit(start, end, emit) } else { Ok(false) };
        }
        loop {
            check(self.canceled)?;
            let Some((name_start, name_end)) = self.name(false) else { return Ok(false); };
            let text = self.text;
            let name = &text[name_start..name_end];
            // A final self imports the prefix itself, not prefix::self.
            // Other module qualifiers may only appear in their initial path
            // positions; a group does not reset their existing prefix.
            if name == "self" && self.length != 0 {
                if self.colons() || self.path[..self.length] == *b"::" { return Ok(false); }
            } else {
                if name == "crate" && self.length != 0 { return Ok(false); }
                if name == "super" && !self.super_prefix() { return Ok(false); }
                if !self.append(name) { return Ok(false); }
            }
            if self.colons() {
                self.at += 2;
                if self.word() == "{" { return self.group(depth, emit); }
                if self.word() == "*" {
                    let end = self.tokens[self.at].end; self.at += 1;
                    return if self.append("*") { self.emit(start, end, emit) } else { Ok(false) };
                }
                continue;
            }
            if self.word() == "as" {
                self.at += 1;
                if self.name(true).is_none() { return Ok(false); }
            }
            return self.emit(start, name_end, emit);
        }
    }
    fn super_prefix(&self) -> bool {
        if self.length == 0 { return true; }
        let Ok(path) = std::str::from_utf8(&self.path[..self.length]) else { return false; };
        let mut parts = path.split("::");
        matches!(parts.next(), Some("self" | "super")) && parts.all(|part| part == "super")
    }
    fn group(&mut self, depth: usize,
        emit: &mut impl FnMut(&str, usize, usize) -> Result<(), ImportError>) -> Result<bool, ImportError> {
        if depth == MAX_USE_TREE_DEPTH { return Ok(false); }
        self.at += 1;
        if self.word() == "}" { self.at += 1; return Ok(true); }
        loop {
            check(self.canceled)?;
            let prefix = self.length;
            if !self.tree(depth + 1, emit)? { return Ok(false); }
            self.length = prefix;
            match self.word() {
                "}" => { self.at += 1; return Ok(true); }
                "," => {
                    self.at += 1;
                    if self.word() == "}" { self.at += 1; return Ok(true); }
                }
                _ => return Ok(false),
            }
        }
    }
}
