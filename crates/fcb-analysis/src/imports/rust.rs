#![forbid(unsafe_code)]

//! Source-specific use-tree grammar over FrankenMarkdown's validated terminals.
//! A whole statement is validated before any normalized leaf is published.
//! There is no local string/comment lexer, compiler scope or Cargo discovery.

use super::{ImportError, ImportKind, MAX_IMPORT_SPECIFIER_BYTES, MAX_STATEMENT_BYTES,
    MAX_STATEMENT_TERMINALS, Statements, Terminal, Tok, check, identifier};

const MAX_USE_TREE_DEPTH: usize = 32;
pub(super) const MAX_INLINE_MODULES: usize = 32;
const MAX_SCOPE_DEPTH: usize = 128;
pub(super) const CONTEXT_WORKSPACE_BYTES: usize = std::mem::size_of::<RustScopes>() + MAX_SCOPE_DEPTH;

impl Statements<'_> {
    pub(super) fn rust(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        // Path overrides can change the interpretation of other conventional
        // links. Keep exact evidence but do not infer their affected module tree.
        let mut overrides = false;
        let mut conditional_source = false;
        for i in 0..self.tokens.len() {
            if i & 255 == 0 { check(canceled)?; }
            if self.word(i) != "#" { continue; }
            let inner = self.word(i + 1) == "!";
            let bracket = i + if inner { 2 } else { 1 };
            if self.word(bracket) != "[" { continue; }
            overrides |= matches!(self.word(bracket + 1), "path" | "cfg_attr");
            conditional_source |= inner && matches!(self.word(bracket + 1), "cfg" | "cfg_attr");
        }
        // Validate the whole delimiter structure before granting any inline
        // ancestry. An unclosed body cannot retroactively qualify earlier rows,
        // and a mismatched/over-depth close cannot expose a false root scope.
        let balanced = self.rust_balanced_scopes(canceled)?;
        let mut scopes = RustScopes::new(balanced && !conditional_source);
        let mut include_data_end = 0usize;
        let mut i = 0;
        while i < self.tokens.len() {
            check(canceled)?;
            if i >= include_data_end {
                // Another macro's token tree may be arbitrary data. Literal
                // include sites keep their existing physical-source path rule.
                if let Some(end) = self.rust_file_include(i)? {
                    // A braced item invocation needs no following semicolon.
                    // Expression includes do not terminate their enclosing item.
                    if i == scopes.item_start && self.word(i + 2) == "{" {
                        scopes.statement_end(end);
                    }
                    i = end;
                    continue;
                }
                if self.word(i) == "!" && i > 0 && identifier(self.word(i - 1)) {
                    let group = if self.word(i - 1) == "macro_rules" && identifier(self.word(i + 1)) {
                        i + 2
                    } else { i + 1 };
                    if matches!(self.word(group), "(" | "[" | "{") {
                        include_data_end = self.rust_token_tree_end(group, canceled)?;
                    }
                }
            }
            if self.keyword(i, "mod") && !self.rust_raw_tail(i) {
                let raw = self.word(i + 1) == "r" && self.word(i + 2) == "#" && identifier(self.word(i + 3))
                    && self.tokens[i + 1].end == self.tokens[i + 2].start
                    && self.tokens[i + 2].end == self.tokens[i + 3].start;
                let name_end = i + if raw { 3 } else { 1 };
                let delimiter = name_end + 1;
                if raw || identifier(self.word(i + 1)) {
                    let name = self.word(name_end);
                    let name_allowed = if raw { !matches!(name, "crate" | "self" | "super" | "Self" | "_") }
                        else { self.tokens[name_end].kind != Tok::Keyword && !matches!(name, "Self" | "_") };
                    let start = self.tokens[i + 1].start; let end = self.tokens[name_end].end;
                    let text = self.text;
                    if self.word(delimiter) == ";" {
                        if let Some((attribute, literal_start, literal_end, literal_allowed)) = self.rust_path_attribute(i, delimiter) {
                            self.add(ImportKind::RustPathModule, attribute, self.tokens[delimiter].end,
                                literal_start, literal_end, &text[literal_start..literal_end],
                                balanced && scopes.depth == 0 && !conditional_source && literal_allowed && name_allowed)?;
                        } else {
                            let context = if !overrides && name_allowed { scopes.context() } else { None };
                            self.rust_add(ImportKind::RustModule, self.tokens[i].start, self.tokens[delimiter].end,
                                start, end, &text[start..end], context)?;
                        }
                        scopes.statement_end(delimiter + 1);
                        i = delimiter + 1; continue;
                    }
                    if self.word(delimiter) == "{" {
                        // Only a named item directly in a qualified module can
                        // establish ancestry. Attributed headers, local blocks,
                        // unsupported visibility and macro data stay unmodeled.
                        let inline = name_allowed && name.is_ascii() && !overrides
                            && i >= include_data_end && self.rust_plain_module_header(scopes.item_start, i);
                        let name = inline.then_some(&text[start..end]);
                        if !scopes.enter(b'}', delimiter + 1, name, false) && balanced { self.diagnostics += 1; }
                        i = delimiter + 1; continue;
                    }
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
                    // and any half-valid output if a later branch is bad.
                    if walk_use(text, tokens, i + 1, j, canceled, |_, _, _| Ok(()))? {
                        let context = if !overrides { scopes.context() } else { None };
                        if !walk_use(text, tokens, i + 1, j, canceled, |path, start, end| {
                            self.rust_add(ImportKind::RustUse, statement_start, statement_end,
                                start, end, path, context)
                        })? { return Err(ImportError::InvalidEvidence); }
                    } else {
                        self.diagnostics += 1;
                        let start = tokens[i + 1].start; let end = tokens[j - 1].end;
                        self.add(ImportKind::RustUse, statement_start, statement_end,
                            start, end, &text[start..end], false)?;
                    }
                    scopes.statement_end(j + 1);
                    i = j + 1; continue;
                }
                self.diagnostics += 1;
            }
            match self.word(i) {
                "{" | "(" | "[" => {
                    let close = match self.word(i) { "{" => b'}', "(" => b')', _ => b']' };
                    let inner_attribute = self.word(i) == "[" && i >= 2 && self.word(i - 2) == "#"
                        && self.word(i - 1) == "!" && scopes.item_start == i - 2;
                    if !scopes.enter(close, i + 1, None, inner_attribute) && balanced { self.diagnostics += 1; }
                }
                "}" | ")" | "]" => {
                    if !scopes.leave(self.word(i).as_bytes()[0], i + 1) && balanced { self.diagnostics += 1; }
                }
                ";" => scopes.statement_end(i + 1),
                _ => {}
            }
            i += 1;
        }
        Ok(())
    }

    fn rust_add(&mut self, kind: ImportKind, start: usize, end: usize, spec_start: usize,
        spec_end: usize, destination: &str, context: Option<&str>) -> Result<(), ImportError> {
        let before = self.facts.len();
        self.add(kind, start, end, spec_start, spec_end, destination, context.is_some())?;
        // Output truncation or an invalid specifier must not rewrite the last
        // previously accepted fact. Every context allocation is charged up front.
        if self.facts.len() == before { return Ok(()); }
        if let Some(context) = context {
            let mut owned = String::new();
            owned.try_reserve_exact(context.len()).map_err(|_| ImportError::ResourceDenied)?;
            if owned.capacity() > context.len() { return Err(ImportError::ResourceDenied); }
            owned.push_str(context);
            self.facts.last_mut().ok_or(ImportError::InvalidEvidence)?.rust_module_context = Some(owned);
        }
        Ok(())
    }

    fn rust_plain_module_header(&self, start: usize, module: usize) -> bool {
        start == module
            || start.checked_add(1) == Some(module) && self.keyword(start, "pub")
            || start.checked_add(4) == Some(module) && self.keyword(start, "pub")
                && self.word(start + 1) == "(" && matches!(self.word(start + 2), "crate" | "self" | "super")
                && self.word(start + 3) == ")"
    }

    fn rust_balanced_scopes(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<bool, ImportError> {
        let mut expected = [0u8; MAX_SCOPE_DEPTH];
        let mut depth = 0usize;
        for i in 0..self.tokens.len() {
            if i & 255 == 0 { check(canceled)?; }
            match self.word(i) {
                "(" | "[" | "{" => {
                    if depth == expected.len() { self.diagnostics += 1; return Ok(false); }
                    expected[depth] = match self.word(i) { "(" => b')', "[" => b']', _ => b'}' };
                    depth += 1;
                }
                ")" | "]" | "}" => {
                    if depth == 0 || expected[depth - 1] != self.word(i).as_bytes()[0] {
                        self.diagnostics += 1; return Ok(false);
                    }
                    depth -= 1;
                }
                _ => {}
            }
        }
        if depth != 0 { self.diagnostics += 1; }
        Ok(depth == 0)
    }

    // Inspect only the upstream string terminal and this fixed attribute/item
    // shape. A preceding attribute, computed argument, unsupported visibility,
    // or raw string is deliberately not reinterpreted. Comments remain part of
    // the original attribute-through-semicolon evidence, never of the path.
    fn rust_path_attribute(&self, module: usize, semicolon: usize) -> Option<(usize, usize, usize, bool)> {
        let before = if module >= 1 && self.keyword(module - 1, "pub") {
            module - 1
        } else if module >= 4 && self.keyword(module - 4, "pub")
            && self.word(module - 3) == "(" && self.word(module - 1) == ")"
            && matches!(self.word(module - 2), "crate" | "self" | "super") {
            module - 4
        } else { module };
        let attribute = before.checked_sub(6)?;
        if self.word(attribute) != "#" || self.word(attribute + 1) != "["
            || self.word(attribute + 2) != "path" || self.word(attribute + 3) != "="
            || self.word(attribute + 5) != "]"
            || (attribute != 0 && !matches!(self.word(attribute - 1), ";" | "}")) {
            return None;
        }
        let literal = self.tokens[attribute + 4];
        let spelling = literal.text(self.text);
        if literal.kind != Tok::Str || spelling.len() < 3
            || !spelling.starts_with('"') || !spelling.ends_with('"') {
            return None;
        }
        let start = literal.start + 1; let end = literal.end - 1;
        let allowed = !self.text[start..end].chars().any(|c| c == '\\' || c == '"' || c.is_control())
            && self.tokens[semicolon].end - self.tokens[attribute].start <= MAX_STATEMENT_BYTES;
        Some((self.tokens[attribute].start, start, end, allowed))
    }

    fn rust_raw_tail(&self, at: usize) -> bool {
        at >= 2 && self.word(at - 2) == "r" && self.word(at - 1) == "#"
            && self.tokens[at - 2].end == self.tokens[at - 1].start
            && self.tokens[at - 1].end == self.tokens[at].start
    }
}


#[derive(Clone, Copy)]
struct RustScope {
    close: u8, qualified: bool, inner_attribute: bool,
    path_len: usize, modules: usize, item_start: usize,
}
struct RustScopes {
    stack: [RustScope; MAX_SCOPE_DEPTH], depth: usize,
    path: [u8; MAX_IMPORT_SPECIFIER_BYTES], path_len: usize, modules: usize,
    qualified: bool, poisoned: bool, item_start: usize,
}
impl RustScopes {
    fn new(qualified: bool) -> Self {
        Self { stack: [RustScope { close: 0, qualified: false, inner_attribute: false,
            path_len: 0, modules: 0, item_start: 0 }; MAX_SCOPE_DEPTH],
            depth: 0, path: [0; MAX_IMPORT_SPECIFIER_BYTES], path_len: 0, modules: 0,
            qualified, poisoned: false, item_start: 0 }
    }
    fn context(&self) -> Option<&str> {
        if !self.qualified || self.poisoned { return None; }
        std::str::from_utf8(&self.path[..self.path_len]).ok()
    }
    fn statement_end(&mut self, after: usize) {
        if self.qualified { self.item_start = after; }
    }
    fn enter(&mut self, close: u8, after: usize, inline: Option<&str>, inner_attribute: bool) -> bool {
        if self.poisoned { return true; }
        if self.depth == self.stack.len() { self.poisoned = true; return false; }
        self.stack[self.depth] = RustScope { close, qualified: self.qualified, inner_attribute,
            path_len: self.path_len, modules: self.modules, item_start: self.item_start };
        self.depth += 1;
        let parent_qualified = self.qualified;
        self.qualified = false;
        self.item_start = after;
        if let Some(name) = inline.filter(|_| parent_qualified) {
            let separator = if self.path_len == 0 { 0 } else { 2 };
            let end = self.path_len + separator + name.len();
            if self.modules == MAX_INLINE_MODULES || end > self.path.len() { return false; }
            if separator != 0 { self.path[self.path_len..self.path_len + 2].copy_from_slice(b"::"); }
            self.path[self.path_len + separator..end].copy_from_slice(name.as_bytes());
            self.path_len = end; self.modules += 1; self.qualified = true;
        }
        true
    }
    fn leave(&mut self, close: u8, after: usize) -> bool {
        if self.poisoned { return true; }
        if self.depth == 0 || self.stack[self.depth - 1].close != close {
            self.poisoned = true; return false;
        }
        self.depth -= 1;
        let frame = self.stack[self.depth];
        self.path_len = frame.path_len; self.modules = frame.modules;
        self.qualified = frame.qualified; self.item_start = frame.item_start;
        if self.qualified && (close == b'}' || frame.inner_attribute) { self.item_start = after; }
        true
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
