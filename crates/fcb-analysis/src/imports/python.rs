#![forbid(unsafe_code)]

//! Bounded Python statement grammar over FrankenMarkdown's validated terminals.
//! Strings (including triple/raw/byte/f-strings) and comments stay upstream-owned.
//! This is lexical import evidence, not Python execution or a symbol resolver.
//!
//! Dotted destinations omit inter-token whitespace and explicit continuations.
//! For "from . import helper", ".helper" is a lexical module candidate; the
//! specifier span is exactly "helper", while the statement preserves the dots.
//! An explicit module ("from .module import Name") never infers a Name file.
//! Unicode spellings remain evidence but cannot authorize path lookup: Python's
//! identifier normalization and complete XID grammar are not implemented here.

use super::{ImportError, ImportKind, Statements, Terminal, Tok, check, identifier, reserve,
    MAX_IMPORT_SPECIFIER_BYTES, MAX_STATEMENT_BYTES, MAX_STATEMENT_TERMINALS};

#[derive(Clone, Copy)]
struct Name { start: usize, end: usize }
enum Form {
    Import { names: Name },
    From { module: Name, bare_relative: bool, names: Option<Name> },
}
struct LogicalStatement { next: usize, end: usize, valid: bool }

impl Statements<'_> {
    pub(super) fn python(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        // Reused for one bounded logical statement. No per-name scratch heap.
        let mut logical = reserve::<usize>(MAX_STATEMENT_TERMINALS)?;
        let mut i = 0;
        let mut depth = 0usize;
        let mut suite = false;
        while i < self.tokens.len() {
            check(canceled)?;
            let line_start = i == 0 || self.python_line_break(i);
            if depth == 0 && line_start {
                suite = matches!(self.word(i), "if" | "elif" | "else" | "for" | "while" | "with"
                    | "try" | "except" | "finally" | "def" | "class" | "async" | "case");
            }
            let boundary = depth == 0 && (line_start || self.word(i - 1) == ";"
                || (suite && self.word(i - 1) == ":"));
            if boundary && (self.keyword(i, "import") || self.keyword(i, "from")) {
                let statement = self.python_statement(i, &mut logical, canceled)?;
                let text = self.text;
                let tokens = self.tokens;
                let mut grammar = Grammar { text, tokens, logical: &logical, at: 0 };
                if let Some(form) = statement.valid.then(|| grammar.parse()).flatten() {
                    match form {
                        Form::Import { names } => {
                            grammar.at = names.start;
                            while grammar.at < names.end {
                                check(canceled)?;
                                let name = grammar.module().ok_or(ImportError::InvalidEvidence)?;
                                grammar.alias().ok_or(ImportError::InvalidEvidence)?;
                                self.python_add(ImportKind::PythonImport, i, statement.end,
                                    &logical, None, name, true)?;
                                grammar.take(",");
                            }
                        }
                        Form::From { module, bare_relative, names } => {
                            if bare_relative && names.is_some() {
                                let names = names.ok_or(ImportError::InvalidEvidence)?;
                                grammar.at = names.start;
                                while grammar.at < names.end {
                                    check(canceled)?;
                                    let start = grammar.at;
                                    grammar.name().ok_or(ImportError::InvalidEvidence)?;
                                    let name = Name { start, end: grammar.at };
                                    grammar.alias().ok_or(ImportError::InvalidEvidence)?;
                                    self.python_add(ImportKind::PythonFrom, i, statement.end,
                                        &logical, Some(module), name, true)?;
                                    grammar.take(",");
                                }
                            } else {
                                self.python_add(ImportKind::PythonFrom, i, statement.end,
                                    &logical, None, module, !bare_relative)?;
                            }
                        }
                    }
                } else {
                    self.diagnostics += 1;
                }
                i = statement.next;
                continue;
            }
            match self.word(i) {
                "(" | "[" | "{" => depth = depth.saturating_add(1),
                ")" | "]" | "}" => depth = depth.saturating_sub(1),
                _ => {}
            }
            i += 1;
        }
        Ok(())
    }

    fn python_line_break(&self, at: usize) -> bool {
        if at == 0 { return true; }
        let previous = self.tokens[at - 1];
        let mut gap = &self.text[previous.end..self.tokens[at].start];
        if self.python_continuation(at - 1) {
            // Only the immediately adjacent newline is joined. A blank/comment
            // line after it still terminates an unparenthesized statement.
            gap = gap.strip_prefix("\r\n").or_else(|| gap.strip_prefix('\n'))
                .or_else(|| gap.strip_prefix('\r')).unwrap_or(gap);
        }
        gap.contains('\n') || gap.contains('\r')
    }

    fn python_continuation(&self, at: usize) -> bool {
        self.word(at) == "\\" && (self.text[self.tokens[at].end..].starts_with('\n')
            || self.text[self.tokens[at].end..].starts_with('\r'))
    }

    fn python_statement(&self, start: usize, logical: &mut Vec<usize>,
        canceled: &mut impl FnMut() -> bool) -> Result<LogicalStatement, ImportError> {
        logical.clear();
        let mut i = start;
        let mut end = self.tokens[start].end;
        let mut depth = 0usize;
        let mut valid = true;
        while i < self.tokens.len() {
            check(canceled)?;
            if i > start && depth == 0 && self.python_line_break(i) { break; }
            if i - start >= MAX_STATEMENT_TERMINALS
                || self.tokens[i].end - self.tokens[start].start > MAX_STATEMENT_BYTES {
                valid = false;
            }
            let word = self.word(i);
            if word == ";" && depth == 0 {
                end = self.tokens[i].end;
                i += 1;
                break;
            }
            if self.python_continuation(i) {
                if i + 1 == self.tokens.len() || (depth == 0 && self.python_line_break(i + 1)) { valid = false; }
            } else if logical.len() < MAX_STATEMENT_TERMINALS {
                logical.push(i);
            } else {
                valid = false;
            }
            match word {
                "(" | "[" | "{" => depth = depth.saturating_add(1),
                ")" | "]" | "}" => {
                    if depth == 0 { valid = false; } else { depth -= 1; }
                }
                _ => {}
            }
            end = self.tokens[i].end;
            i += 1;
        }
        Ok(LogicalStatement { next: i, end, valid: valid && depth == 0 })
    }

    #[allow(clippy::too_many_arguments)]
    fn python_add(&mut self, kind: ImportKind, statement: usize, end: usize,
        logical: &[usize], prefix: Option<Name>, name: Name, allowed: bool) -> Result<(), ImportError> {
        let mut bytes = [0u8; MAX_IMPORT_SPECIFIER_BYTES];
        let mut used = 0usize;
        for part in prefix.into_iter().chain(std::iter::once(name)) {
            for &at in &logical[part.start..part.end] {
                let word = self.word(at).as_bytes();
                if word.len() > bytes.len() - used {
                    self.diagnostics += 1;
                    return Ok(());
                }
                bytes[used..used + word.len()].copy_from_slice(word);
                used += word.len();
            }
        }
        let destination = std::str::from_utf8(&bytes[..used]).map_err(|_| ImportError::InvalidEvidence)?;
        self.add(kind, self.tokens[statement].start, end, self.tokens[logical[name.start]].start,
            self.tokens[logical[name.end - 1]].end, destination, allowed && destination.is_ascii())
    }
}

// A second bounded pass validates the complete grammar before publishing any
// fact. A malformed final alias/comma cannot leave a plausible partial result.
struct Grammar<'source, 'line> {
    text: &'source str, tokens: &'source [Terminal], logical: &'line [usize], at: usize,
}
impl Grammar<'_, '_> {
    fn token(&self) -> Option<Terminal> { self.logical.get(self.at).map(|&at| self.tokens[at]) }
    fn take(&mut self, word: &str) -> bool {
        if self.token().is_some_and(|token| token.text(self.text) == word) { self.at += 1; true } else { false }
    }
    fn keyword(&mut self, word: &str) -> bool {
        if self.token().is_some_and(|token| token.kind == Tok::Keyword && token.text(self.text) == word) {
            self.at += 1; true
        } else { false }
    }
    fn is_name(&self) -> bool {
        self.token().is_some_and(|token| matches!(token.kind, Tok::Plain | Tok::Type | Tok::Func)
            && identifier(token.text(self.text)))
    }
    fn name(&mut self) -> Option<()> {
        if !self.is_name() { return None; }
        self.at += 1;
        Some(())
    }
    fn module(&mut self) -> Option<Name> {
        let start = self.at;
        self.name()?;
        while self.take(".") { self.name()?; }
        Some(Name { start, end: self.at })
    }
    fn alias(&mut self) -> Option<()> {
        if self.keyword("as") { self.name()?; }
        Some(())
    }
    fn parse(&mut self) -> Option<Form> {
        if self.keyword("import") {
            let start = self.at;
            self.module()?; self.alias()?;
            while self.take(",") { self.module()?; self.alias()?; }
            return (self.at == self.logical.len()).then_some(Form::Import { names: Name { start, end: self.at } });
        }
        if !self.keyword("from") { return None; }
        let start = self.at;
        while self.take(".") {}
        let dots = self.at - start;
        let bare_relative = !self.is_name();
        if bare_relative {
            if dots == 0 { return None; }
        } else {
            self.module()?;
        }
        let module = Name { start, end: self.at };
        if !self.keyword("import") { return None; }
        if self.take("*") {
            return (self.at == self.logical.len()).then_some(Form::From { module, bare_relative, names: None });
        }
        let parenthesized = self.take("(");
        let names_start = self.at;
        self.name()?; self.alias()?;
        while self.take(",") {
            if parenthesized && self.token().is_some_and(|token| token.text(self.text) == ")") { break; }
            self.name()?; self.alias()?;
        }
        let names = Name { start: names_start, end: self.at };
        if parenthesized && !self.take(")") { return None; }
        (self.at == self.logical.len()).then_some(Form::From { module, bare_relative, names: Some(names) })
    }
}
