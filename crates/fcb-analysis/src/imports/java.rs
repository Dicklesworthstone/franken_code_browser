#![forbid(unsafe_code)]

//! Java compilation-unit import declarations over upstream lexical terminals.
//! Comments, character/string literals and text blocks stay FMD-owned. This
//! grammar recognizes the optional package declaration and the import prefix;
//! it never scans method/type bodies for plausible-looking import statements.
//! A malformed import stops the prefix with explicit partial coverage.
//!
//! Java's pre-token Unicode escapes and bare-CR line comments are not qualified
//! by the pinned lexical route. Names use its letter/alphanumeric/underscore/
//! dollar subset. Other Java identifier categories are not guessed. Unicode
//! names remain exact lexical evidence, without authorizing path resolution.

use super::{ImportError, ImportKind, Statements, Tok, check,
    MAX_IMPORT_SPECIFIER_BYTES, MAX_STATEMENT_BYTES, MAX_STATEMENT_TERMINALS};
use franken_markdown::highlight::Span;

const MAX_ANNOTATION_DEPTH: usize = 64;

pub(super) fn check_source(text: &str, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
    let bytes = text.as_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        if i & 4095 == 0 { check(canceled)?; }
        if (byte == b'\\' && bytes.get(i + 1) == Some(&b'u'))
            || (byte == b'\r' && bytes.get(i + 1) != Some(&b'\n')) {
            return Err(ImportError::UnsupportedLexicalContext);
        }
    }
    check(canceled)
}

pub(super) fn validate_context(text: &str, spans: &[Span], canceled: &mut impl FnMut() -> bool)
    -> Result<(), ImportError> {
    // The upstream lexical palette groups more Unicode whitespace than Java's
    // source grammar admits. Do not silently normalize it between identifiers.
    // The same characters inside comments/literals remain opaque valid content.
    for span in spans {
        check(canceled)?;
        let part = text.get(span.start..span.end).ok_or(ImportError::InvalidEvidence)?;
        if span.kind == Tok::Comment && part.starts_with("/*")
            && (part.len() < 4 || !part.ends_with("*/")) {
            return Err(ImportError::UnsupportedLexicalContext);
        }
        if span.kind == Tok::Str {
            // Refuse unfinished upstream literals before their newline or EOF
            // can expose annotation delimiters as a new declaration prefix.
            let delimiter = if part.starts_with("\"\"\"") { "\"\"\"" }
                else if part.starts_with('"') { "\"" }
                else if part.starts_with('\'') { "'" }
                else { return Err(ImportError::UnsupportedLexicalContext); };
            if part.len() < delimiter.len() * 2 || !part.ends_with(delimiter)
                || (delimiter.len() == 1 && part.contains(['\n', '\r']))
                || part.as_bytes()[..part.len() - delimiter.len()].iter().rev()
                    .take_while(|&&byte| byte == b'\\').count() % 2 != 0 {
                return Err(ImportError::UnsupportedLexicalContext);
            }
        }
        if !matches!(span.kind, Tok::Comment | Tok::Str)
            && part.chars().any(|c| c.is_whitespace() && !matches!(c, ' ' | '\t' | '\n' | '\r' | '\u{c}')) {
            return Err(ImportError::UnsupportedLexicalContext);
        }
    }
    Ok(())
}

struct Name {
    start: usize, end: usize, next: usize, parts: usize, wildcard: bool,
    bytes: [u8; MAX_IMPORT_SPECIFIER_BYTES], len: usize,
}
impl Name {
    fn append(&mut self, text: &str) -> bool {
        if text.len() > self.bytes.len() - self.len { return false; }
        self.bytes[self.len..self.len + text.len()].copy_from_slice(text.as_bytes());
        self.len += text.len();
        true
    }
    fn destination(&self) -> Result<&str, ImportError> {
        std::str::from_utf8(&self.bytes[..self.len]).map_err(|_| ImportError::InvalidEvidence)
    }
}

impl Statements<'_> {
    pub(super) fn java(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        let mut i = 0;
        // Package annotations may contain balanced annotation arguments and
        // opaque upstream strings. An annotated type ends the import prefix.
        while self.word(i) == "@" && !self.keyword(i + 1, "interface") {
            check(canceled)?;
            let Some(next) = self.java_annotation_end(i, canceled)? else {
                self.diagnostics += 1;
                return Ok(());
            };
            i = next;
        }
        if i != 0 && !self.keyword(i, "package") { return Ok(()); }
        if self.keyword(i, "package") {
            let Some(name) = self.java_name(i + 1, i, false, canceled)? else {
                self.diagnostics += 1;
                return Ok(());
            };
            if self.word(name.next) != ";" || !self.java_within_statement(i, name.next) {
                self.diagnostics += 1;
                return Ok(());
            }
            i = name.next + 1;
        }
        while i < self.tokens.len() {
            check(canceled)?;
            if !self.keyword(i, "import") {
                // A duplicate/late package declaration cannot establish a
                // second compilation-unit namespace under the first capture.
                if self.keyword(i, "package") { self.diagnostics += 1; }
                break;
            }
            let is_static = self.keyword(i + 1, "static");
            let Some(name) = self.java_name(i + 1 + usize::from(is_static), i, true, canceled)? else {
                self.diagnostics += 1;
                break;
            };
            if self.word(name.next) != ";" || !self.java_within_statement(i, name.next)
                || (is_static && !name.wildcard && name.parts < 2) {
                self.diagnostics += 1;
                break;
            }
            let destination = name.destination()?;
            self.add(if is_static { ImportKind::JavaStaticImport } else { ImportKind::JavaImport },
                self.tokens[i].start, self.tokens[name.next].end, name.start, name.end,
                destination, destination.is_ascii() && (is_static || !name.wildcard))?;
            i = name.next + 1;
        }
        Ok(())
    }

    fn java_within_statement(&self, start: usize, at: usize) -> bool {
        at.checked_sub(start).is_some_and(|count| count < MAX_STATEMENT_TERMINALS)
            && self.tokens.get(start).zip(self.tokens.get(at)).is_some_and(|(start, at)|
                at.end.checked_sub(start.start).is_some_and(|bytes| bytes <= MAX_STATEMENT_BYTES))
    }

    fn java_identifier(&self, at: usize) -> bool {
        let Some(token) = self.tokens.get(at) else { return false; };
        let word = self.word(at);
        // The upstream lexer colors these contextual words as keywords. They
        // can still occur in a qualified package/import spelling.
        if !matches!(token.kind, Tok::Plain | Tok::Type | Tok::Func)
            && !(token.kind == Tok::Keyword && matches!(word, "record" | "sealed" | "permits" | "var" | "yield")) {
            return false;
        }
        if word == "_" { return false; }
        let mut chars = word.chars();
        chars.next().is_some_and(|c| c.is_alphabetic() || matches!(c, '_' | '$'))
            && chars.all(|c| c.is_alphanumeric() || matches!(c, '_' | '$'))
    }

    fn java_name(&self, start: usize, statement: usize, allow_wildcard: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<Name>, ImportError> {
        if !self.java_identifier(start) || !self.java_within_statement(statement, start) { return Ok(None); }
        let mut name = Name { start: self.tokens[start].start, end: self.tokens[start].end,
            next: start + 1, parts: 1, wildcard: false, bytes: [0; MAX_IMPORT_SPECIFIER_BYTES], len: 0 };
        if !name.append(self.word(start)) { return Ok(None); }
        while self.word(name.next) == "." {
            check(canceled)?;
            let at = name.next + 1;
            if !self.java_within_statement(statement, at) || !name.append(".") { return Ok(None); }
            if self.word(at) == "*" && allow_wildcard {
                if !name.append("*") { return Ok(None); }
                name.wildcard = true;
            } else {
                if !self.java_identifier(at) || !name.append(self.word(at)) { return Ok(None); }
                name.parts += 1;
            }
            name.end = self.tokens[at].end;
            name.next = at + 1;
            if name.wildcard { break; }
        }
        Ok(Some(name))
    }

    fn java_annotation_end(&self, start: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, ImportError> {
        let Some(name) = self.java_name(start + 1, start, false, canceled)? else { return Ok(None); };
        let mut i = name.next;
        if self.word(i) != "(" { return Ok(Some(i)); }
        let mut stack = [b'\0'; MAX_ANNOTATION_DEPTH];
        let mut depth = 0;
        loop {
            check(canceled)?;
            if !self.java_within_statement(start, i) { return Ok(None); }
            // Strings stay opaque even if their bytes contain delimiters.
            let word = if self.tokens[i].kind == Tok::Str { "" } else { self.word(i) };
            match word {
                "(" | "[" | "{" => {
                    if depth == stack.len() { return Ok(None); }
                    stack[depth] = word.as_bytes()[0];
                    depth += 1;
                }
                ")" | "]" | "}" => {
                    if depth == 0 { return Ok(None); }
                    depth -= 1;
                    if !matches!((stack[depth], word), (b'(', ")") | (b'[', "]") | (b'{', "}")) { return Ok(None); }
                    if depth == 0 { return Ok(Some(i + 1)); }
                }
                _ => {}
            }
            i += 1;
        }
    }
}
