#![forbid(unsafe_code)]

//! Literal JavaScript load expressions over FrankenMarkdown terminals.
//! These are lexical candidates: an unqualified `require` may be shadowed,
//! and an `import()` spelling may occur in a TypeScript type query. No runtime
//! execution or compiler binding is established. Member calls, constructors,
//! computed arguments, template specifiers and multiple arguments are outside
//! this grammar. Computed/template/multiple-argument load expressions record
//! diagnostics; member and constructor calls are not import candidates.

use super::{check, ImportError, ImportKind, Statements, Terminal, Tok,
    MAX_STATEMENT_BYTES, MAX_STATEMENT_TERMINALS};


/// The pinned JavaScript lexer treats a closing ')' as an expression value.
/// JavaScript also permits a regex statement after a control condition; at that
/// boundary the owner can expose regex text as code. Reject that unsupported
/// context before any facts publish. This is a bounded grammar check over
/// upstream terminals, not a local attempt to classify/scan regex literals.
/// Automatic semicolon insertion can also put a regex statement after a value
/// token at a line boundary (declarations, labeled jumps and TypeScript types).
/// Conservatively refuse owner-exposed slash operators across that boundary;
/// same-line division and ordinary control bodies remain supported.
pub(super) fn check_context(text: &str, tokens: &[Terminal],
    canceled: &mut impl FnMut() -> bool) -> Result<(), ImportError> {
    let mut controls = [false; MAX_STATEMENT_TERMINALS];
    let mut depth = 0usize;
    let mut after_control = false;
    for (i, token) in tokens.iter().enumerate() {
        check(canceled)?;
        let word = token.text(text);
        if token.kind == Tok::Operator && word == "/" {
            let after_line = i > 0 && text[tokens[i - 1].end..token.start].chars()
                .any(|scalar| matches!(scalar, '\n' | '\r' | '\u{2028}' | '\u{2029}'));
            if after_control || after_line {
                return Err(ImportError::UnsupportedLexicalContext);
            }
        }
        after_control = false;
        if token.kind != Tok::Punct { continue; }
        match word {
            "(" => {
                if depth == controls.len() { return Err(ImportError::UnsupportedLexicalContext); }
                let keyword = |at: usize, expected: &str| tokens.get(at)
                    .is_some_and(|token| token.kind == Tok::Keyword && token.text(text) == expected);
                let control = i.checked_sub(1).is_some_and(|previous|
                    ["if", "while", "for", "with", "switch", "catch"].iter().any(|word| keyword(previous, word))
                    && (previous == 0 || !matches!(tokens[previous - 1].text(text), "." | "?")));
                let for_await = i >= 2 && keyword(i - 1, "await") && keyword(i - 2, "for")
                    && (i == 2 || !matches!(tokens[i - 3].text(text), "." | "?"));
                controls[depth] = control || for_await; depth += 1;
            }
            ")" if depth > 0 => { depth -= 1; after_control = controls[depth]; }
            _ => {}
        }
    }
    Ok(())
}

impl Statements<'_> {
    /// Recognize a bare require token and an upstream code parenthesis. JSX
    /// children use Plain text, so their apparent parentheses cannot create
    /// calls. A comment between the name and '(' is valid: it was classified
    /// and removed by the upstream lexer, while the evidence retains its bytes.
    pub(super) fn javascript_require(&self, at: usize) -> bool {
        self.tokens.get(at).is_some_and(|token| matches!(token.kind, Tok::Plain | Tok::Func))
            && self.word(at) == "require"
            && self.tokens.get(at + 1).is_some_and(|token| token.kind == Tok::Punct)
            && self.word(at + 1) == "("
            && (at == 0 || !matches!(self.word(at - 1), "." | "?" | "new" | "function"))
    }

    /// A single quoted argument, with optional trailing comma. A closing
    /// parenthesis is mandatory; concatenation and additional arguments never
    /// become a resolvable path merely because the first token was a string.
    /// Return the first unconsumed terminal, retaining call-expression ranges
    /// rather than pretending the whole enclosing statement is an import.
    pub(super) fn javascript_call(&mut self, at: usize, kind: ImportKind,
        canceled: &mut impl FnMut() -> bool) -> Result<usize, ImportError> {
        check(canceled)?;
        let literal = at + 2;
        let close = if self.word(at + 3) == "," { at + 4 } else { at + 3 };
        let Some(token) = self.tokens.get(literal).copied() else {
            self.diagnostics += 1; return Ok(at + 1);
        };
        if self.tokens.get(at + 1).is_none_or(|token| token.kind != Tok::Punct)
            || self.word(at + 1) != "(" || token.kind != Tok::Str
            || self.tokens.get(close).is_none_or(|token| token.kind != Tok::Punct)
            || self.word(close) != ")" {
            self.diagnostics += 1; return Ok(at + 1);
        }
        let end = self.tokens[close].end;
        if end - self.tokens[at].start > MAX_STATEMENT_BYTES {
            self.diagnostics += 1; return Ok(close + 1);
        }
        let text = token.text(self.text);
        let quote = text.as_bytes().first().copied();
        if !matches!(quote, Some(b'\'' | b'"')) || text.len() < 2
            || text.as_bytes().last().copied() != quote {
            self.diagnostics += 1; return Ok(close + 1);
        }
        let destination = &text[1..text.len() - 1];
        let allowed = !destination.contains('\\') && !destination.chars().any(char::is_control)
            && !destination.as_bytes().contains(&quote.ok_or(ImportError::InvalidEvidence)?);
        self.add(kind, self.tokens[at].start, end, token.start + 1, token.end - 1, destination, allowed)?;
        Ok(close + 1)
    }
}
