#![forbid(unsafe_code)]

//! Single-physical-line C/C++ include grammar over the pinned FMD lexer.
//! The lexer owns strings, comments and raw strings. No preprocessor runs:
//! conditional branches remain lexical observations, macros stay unresolved,
//! and quoted paths are never C-unescaped or searched through compiler roots.

use super::{ImportError, ImportKind, Statements, Tok, MAX_STATEMENT_BYTES,
    MAX_STATEMENT_TERMINALS, check};

/// The pinned lexer does not perform translation-phase line splicing or
/// trigraph replacement. Refuse these captures before it can expose a directive
/// from inside a continued comment/string. This is an admission guard, not an
/// alternate lexer or a source-rewriting preprocessing pass.
pub(super) fn check_source(text: &str, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
    let bytes = text.as_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        if i & 1023 == 0 { check(canceled)?; }
        if byte == b'\r' && bytes.get(i + 1) != Some(&b'\n') {
            return Err(ImportError::UnsupportedLexicalContext);
        }
        if byte == b'?' && bytes.get(i + 1) == Some(&b'?')
            && bytes.get(i + 2).is_some_and(|third| b"=/'()!<>-".contains(third)) {
            return Err(ImportError::UnsupportedLexicalContext);
        }
        if byte == b'\\' {
            let mut next = i + 1;
            // Also decline the whitespace-before-newline compiler extension.
            while bytes.get(next).is_some_and(|&b| matches!(b, b' ' | b'\t' | 0x0b | 0x0c)) {
                if next & 1023 == 0 { check(canceled)?; }
                next += 1;
            }
            if bytes.get(next).is_some_and(|&b| matches!(b, b'\n' | b'\r')) {
                return Err(ImportError::UnsupportedLexicalContext);
            }
        }
    }
    check(canceled)
}

impl Statements<'_> {
    pub(super) fn cpp(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        // Legal C++ raw delimiters are broader than this pinned lexer accepts.
        // A recognized raw string is one opaque Str token. An adjacent raw
        // prefix and ordinary quote token means the owner declined that shape;
        // refuse rather than extracting apparent includes from its contents.
        for pair in self.tokens.windows(2) {
            check(canceled)?;
            if pair[0].kind != Tok::Str && pair[1].kind == Tok::Str
                && pair[0].end == pair[1].start
                && matches!(pair[0].text(self.text), "R" | "u8R" | "uR" | "UR" | "LR")
                && pair[1].text(self.text).starts_with('"') {
                return Err(ImportError::UnsupportedLexicalContext);
            }
        }
        let text = self.text;
        let mut first = 0;
        let mut line_start = 0;
        for line in text.split_inclusive('\n') {
            check(canceled)?;
            let physical_end = line_start + line.len();
            let mut end = physical_end;
            if end > line_start && text.as_bytes()[end - 1] == b'\n' { end -= 1; }
            if end > line_start && text.as_bytes()[end - 1] == b'\r' { end -= 1; }
            let mut next = first;
            while next < self.tokens.len() && self.tokens[next].start < physical_end {
                check(canceled)?;
                next += 1;
            }
            // A raw string that ends on this line is an earlier significant
            // token, even though its start belonged to a previous line.
            if first < next && (first == 0 || self.tokens[first - 1].end <= line_start)
                && self.tokens[first].start >= line_start && self.word(first) == "#" {
                self.cpp_directive(first, next, end, canceled)?;
            }
            first = next;
            line_start = physical_end;
        }
        Ok(())
    }

    fn cpp_directive(&mut self, first: usize, next: usize, line_end: usize,
        canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
        if first + 1 == next { return Ok(()); } // The empty # directive.
        let text = self.text;
        let keyword = first + 1;
        let mut argument = keyword;
        // FMD may split '# include' across adjacent classified spans. Join
        // only byte-adjacent identifier terminals; never join across whitespace
        // or a comment to turn distinct words into the include directive.
        while argument < next && self.tokens[argument].kind != Tok::Str
            && self.word(argument).bytes().all(|b| b == b'_' || b.is_ascii_alphanumeric())
            && (argument == keyword || self.tokens[argument - 1].end == self.tokens[argument].start) {
            check(canceled)?;
            argument += 1;
        }
        if argument == keyword
            || &text[self.tokens[keyword].start..self.tokens[argument - 1].end] != "include" {
            return Ok(());
        }
        if argument == next || next - first > MAX_STATEMENT_TERMINALS
            || self.tokens[next - 1].end > line_end
            || self.tokens[next - 1].end - self.tokens[first].start > MAX_STATEMENT_BYTES {
            self.diagnostics += 1;
            return Ok(());
        }
        let start = self.tokens[first].start;
        let last = self.tokens[next - 1];
        let token = self.tokens[argument];
        let spelling = token.text(text);
        if argument + 1 == next && token.kind == Tok::Str && spelling.starts_with('"') {
            if spelling.len() < 2 || !spelling.ends_with('"') {
                self.diagnostics += 1;
                return Ok(());
            }
            let destination = &spelling[1..spelling.len() - 1];
            let allowed = !destination.chars().any(|c| c.is_control() || matches!(c, '\\' | '"'));
            self.add(ImportKind::CppQuotedInclude, start, last.end, token.start + 1,
                token.end - 1, destination, allowed)?;
        } else if self.word(argument) == "<" && self.word(next - 1) == ">" {
            let destination = &text[token.end..last.start];
            let allowed = !destination.chars().any(|c| c.is_control() || matches!(c, '\\' | '"' | '<' | '>'));
            self.add(ImportKind::CppAngleInclude, start, last.end, token.end,
                last.start, destination, allowed)?;
        } else {
            self.add(ImportKind::CppComputedInclude, start, last.end, token.start,
                last.end, &text[token.start..last.end], false)?;
        }
        Ok(())
    }
}
