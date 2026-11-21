#![forbid(unsafe_code)]

//! Go's package/import prefix over upstream-classified terminals. Import
//! aliases do not change the destination. Strings/comments stay opaque, and
//! no imports are discovered inside declarations, function bodies or literals.

use super::{ImportError, ImportKind, MAX_STATEMENT_BYTES, MAX_STATEMENT_TERMINALS,
    Span, Statements, Tok, check, identifier};

pub(super) fn validate_context(text: &str, spans: &[Span],
    canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
    let mut end = 0;
    for span in spans {
        check(canceled)?;
        let word = text.get(span.start..span.end).ok_or(ImportError::InvalidEvidence)?;
        if span.start != end || word.is_empty() { return Err(ImportError::InvalidEvidence); }
        if span.kind == Tok::Comment && word.starts_with("/*") && !word.ends_with("*/") {
            return Err(ImportError::UnsupportedLexicalContext);
        }
        if span.kind == Tok::Str {
            let quote = word.as_bytes()[0];
            if !matches!(quote, b'`' | b'\'' | b'"') || word.len() < 2
                || word.as_bytes().last() != Some(&quote)
                || (quote != b'`' && (word.contains('\n') || word.contains('\r')
                    || word.as_bytes()[..word.len() - 1].iter().rev().take_while(|&&b| b == b'\\').count() % 2 != 0)) {
                return Err(ImportError::UnsupportedLexicalContext);
            }
        }
        if span.kind == Tok::Plain && word.chars().all(char::is_whitespace)
            && !word.bytes().all(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n')) {
            return Err(ImportError::UnsupportedLexicalContext);
        }
        end = span.end;
    }
    if end != text.len() { return Err(ImportError::InvalidEvidence); }
    Ok(())
}

impl Statements<'_> {
    fn go_newline(&self, at: usize) -> bool {
        at > 0 && at < self.tokens.len()
            && self.text[self.tokens[at - 1].end..self.tokens[at].start].contains('\n')
    }

    fn go_name(&self, at: usize) -> bool {
        self.tokens.get(at).is_some_and(|token| !matches!(token.kind, Tok::Keyword | Tok::Str))
            && identifier(self.word(at))
    }

    /// Exclusive literal end and next terminal. A newline after an alias is
    /// semicolon insertion, not permission to join the following string.
    fn go_spec(&self, start: usize, grouped: bool) -> Option<(usize, usize)> {
        let literal = if self.tokens.get(start)?.kind == Tok::Str { start }
            else if self.word(start) == "." || (self.go_name(start) && !self.go_newline(start + 1)) { start + 1 }
            else { return None; };
        let token = *self.tokens.get(literal)?;
        let word = token.text(self.text);
        if token.kind != Tok::Str || !matches!(word.as_bytes().first().copied(), Some(b'"' | b'`')) { return None; }
        let next = literal + 1;
        if next == self.tokens.len() || self.go_newline(next) || (grouped && self.word(next) == ")") {
            Some((literal, next))
        } else if self.word(next) == ";" { Some((literal, next + 1)) }
        else { None }
    }

    fn go_add(&mut self, start: usize, literal: usize, next: usize) -> Result<(), ImportError> {
        let token = self.tokens[literal];
        let word = token.text(self.text);
        let destination = &word[1..word.len() - 1];
        // Escaped interpreted paths and CR-bearing raw strings require Go
        // string interpretation. Retain their exact spelling as evidence, but
        // do not invent a decoded filesystem or package destination.
        let allowed = !destination.is_empty() && !destination.contains('\\')
            && !destination.chars().any(|c| c.is_control() || c.is_whitespace())
            && !destination.contains(['"', '`']);
        let end = if next > literal + 1 { self.tokens[next - 1].end } else { token.end };
        self.add(ImportKind::GoImport, self.tokens[start].start, end,
            token.start + 1, token.end - 1, destination, allowed)
    }

    pub(super) fn go(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        if self.tokens.is_empty() { return Ok(()); }
        if !self.keyword(0, "package") || !self.go_name(1) || self.go_newline(1) {
            self.diagnostics += 1; return Ok(());
        }
        let mut at = 2;
        if self.word(at) == ";" { at += 1; }
        else if at < self.tokens.len() && !self.go_newline(at) {
            self.diagnostics += 1; return Ok(());
        }
        while self.keyword(at, "import") {
            check(canceled)?;
            let start = at;
            if self.word(at + 1) != "(" {
                let Some((literal, next)) = self.go_spec(at + 1, false) else {
                    self.diagnostics += 1; return Ok(());
                };
                self.go_add(start, literal, next)?;
                at = next; continue;
            }
            at += 2;
            let first = at;
            // Validate the whole group before publishing any of its entries.
            // An incomplete group is not a complete import inventory.
            while at < self.tokens.len() && self.word(at) != ")" {
                check(canceled)?;
                if at - start > MAX_STATEMENT_TERMINALS
                    || self.tokens[at].end - self.tokens[start].start > MAX_STATEMENT_BYTES {
                    self.diagnostics += 1; return Ok(());
                }
                let Some((_, next)) = self.go_spec(at, true) else {
                    self.diagnostics += 1; return Ok(());
                };
                at = next;
            }
            if self.word(at) != ")" {
                self.diagnostics += 1; return Ok(());
            }
            let end = at;
            at += 1;
            if self.word(at) == ";" { at += 1; }
            else if at < self.tokens.len() && !self.go_newline(at) {
                self.diagnostics += 1; return Ok(());
            }
            let mut entry = first;
            while entry < end {
                check(canceled)?;
                let (literal, next) = self.go_spec(entry, true).ok_or(ImportError::InvalidEvidence)?;
                self.go_add(entry, literal, next)?;
                entry = next;
            }
        }
        Ok(())
    }
}
