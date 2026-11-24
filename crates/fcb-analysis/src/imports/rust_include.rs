#![forbid(unsafe_code)]

//! Exact, literal file references in Rust's three file-inclusion macro spellings.
//! This recognizes source syntax. It does not expand macros or prove which macro
//! an unqualified name binds to.

use super::{ImportError, ImportKind, Statements, Tok, MAX_STATEMENT_BYTES, check};

impl Statements<'_> {
    /// Indices are over owner-classified terminals. In particular, an apparent
    /// macro name inside one string/comment cannot become an invocation.
    pub(super) fn rust_file_include(&mut self, at: usize) -> Result<Option<usize>, ImportError> {
        let kind = match self.word(at) {
            "include" => ImportKind::RustInclude,
            "include_str" => ImportKind::RustIncludeStr,
            "include_bytes" => ImportKind::RustIncludeBytes,
            _ => return Ok(None),
        };
        // A qualified/custom macro or metavariable has different binding evidence.
        if self.word(at + 1) != "!" || at > 0 && matches!(self.word(at - 1), "." | "$" | "#")
            || at >= 2 && self.word(at - 1) == ":" && self.word(at - 2) == ":" {
            return Ok(None);
        }
        let close = match self.word(at + 2) {
            "(" => ")", "[" => "]", "{" => "}", _ => return Ok(None),
        };
        let Some(token) = self.tokens.get(at + 3).copied() else { return Ok(None); };
        let last = at + if self.word(at + 4) == "," { 5 } else { 4 };
        if token.kind != Tok::Str || self.word(last) != close
            || self.tokens[last].end - self.tokens[at].start > MAX_STATEMENT_BYTES {
            self.diagnostics += 1;
            return Ok(None);
        }
        let spelling = token.text(self.text);
        let Some(inner) = spelling.strip_prefix('"').and_then(|value| value.strip_suffix('"')) else {
            self.diagnostics += 1;
            return Ok(None);
        };
        if inner.is_empty() || inner.contains(['"', '\\']) || inner.chars().any(char::is_control) {
            self.diagnostics += 1;
            return Ok(None);
        }
        let start = token.start + 1;
        let end = token.end - 1;
        let text = self.text;
        self.add(kind, self.tokens[at].start, self.tokens[last].end, start, end, &text[start..end], true)?;
        Ok(Some(last + 1))
    }

    /// Nested token trees in arbitrary macro input (including macro_rules!
    /// templates) are data. Skip them for include-site recognition without
    /// changing the existing mod/use evidence policy. A malformed group or
    /// excessive nesting never exposes its tail as fresh top-level input.
    pub(super) fn rust_token_tree_end(&mut self, start: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, ImportError>
    {
        let mut expected = [0u8; 128];
        let mut depth = 0usize;
        for i in start..self.tokens.len() {
            if (i - start) % 256 == 0 { check(canceled)?; }
            let word = self.word(i);
            match word {
                "(" | "[" | "{" => {
                    if depth == expected.len() { break; }
                    expected[depth] = match word { "(" => b')', "[" => b']', _ => b'}' };
                    depth += 1;
                }
                ")" | "]" | "}" => {
                    if depth == 0 || expected[depth - 1] != word.as_bytes()[0] {
                        break;
                    }
                    depth -= 1;
                    if depth == 0 { return Ok(i + 1); }
                }
                _ => {}
            }
        }
        self.diagnostics += 1;
        Ok(self.tokens.len())
    }
}
