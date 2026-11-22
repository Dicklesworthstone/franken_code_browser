#![forbid(unsafe_code)]

//! Admission checks on the pinned owner's complete token stream. These checks
//! refuse unqualified boundaries; they never repair, split or reclassify a
//! comment/string token. Literal contents remain opaque to delimiter matching.

use super::{DelimiterError, Span, SymbolLanguage, Tok, check};
use crate::imports::{ImportError, validate_javascript_context};

fn javascript(language: SymbolLanguage) -> bool {
    matches!(language, SymbolLanguage::JavaScript | SymbolLanguage::TypeScript
        | SymbolLanguage::Jsx | SymbolLanguage::Tsx)
}

pub(super) fn check_source(text: &str, language: SymbolLanguage,
    canceled: &mut impl FnMut() -> bool) -> Result<(), DelimiterError> {
    let bytes = text.as_bytes();
    if javascript(language) && text.starts_with("#!") {
        return Err(DelimiterError::UnsupportedLexicalContext);
    }
    for (ordinal, (i, ch)) in text.char_indices().enumerate() {
        if ordinal & 1023 == 0 { check(canceled)?; }
        // The pinned line-comment routes use LF. Refuse alternate line
        // terminators rather than inventing context after an apparent comment.
        if matches!(ch, '\0' | '\u{85}' | '\u{2028}' | '\u{2029}')
            || (ch == '\r' && bytes.get(i + 1) != Some(&b'\n'))
            || (language == SymbolLanguage::Java && ch == '\\' && bytes.get(i + 1) == Some(&b'u')) {
            return Err(DelimiterError::UnsupportedLexicalContext);
        }
        if language != SymbolLanguage::Cpp { continue; }
        // C/C++ translation phases can change lexical boundaries before the
        // upstream owner sees bytes. This route performs no preprocessing.
        if ch == '?' && bytes.get(i + 1) == Some(&b'?')
            && bytes.get(i + 2).is_some_and(|byte| b"=/'()!<>-".contains(byte)) {
            return Err(DelimiterError::UnsupportedLexicalContext);
        }
        if ch == '\\' {
            let mut next = i + 1;
            while bytes.get(next).is_some_and(|b| matches!(b, b' ' | b'\t' | 0x0b | 0x0c)) {
                if next & 4095 == 0 { check(canceled)?; }
                next += 1;
            }
            if bytes.get(next).is_some_and(|b| matches!(b, b'\n' | b'\r')) {
                return Err(DelimiterError::UnsupportedLexicalContext);
            }
        }
    }
    check(canceled)
}

pub(super) fn validate(text: &str, language: SymbolLanguage, spans: &[Span],
    canceled: &mut impl FnMut() -> bool) -> Result<(), DelimiterError> {
    let mut previous = 0;
    let mut significant: Option<Span> = None;
    for span in spans {
        check(canceled)?;
        let word = text.get(span.start..span.end).ok_or(DelimiterError::InvalidEvidence)?;
        if span.start != previous || span.start >= span.end {
            return Err(DelimiterError::InvalidEvidence);
        }
        previous = span.end;
        if span.kind == Tok::Comment {
            if word.starts_with("/*") && (word.len() < 4 || !word.ends_with("*/")) {
                return Err(DelimiterError::UnsupportedLexicalContext);
            }
            // The Rust batch owner clamps nested block-comment depth at 64.
            if language == SymbolLanguage::Rust && word.starts_with("/*")
                && word.match_indices("/*").count() >= 64 {
                return Err(DelimiterError::UnsupportedLexicalContext);
            }
            continue;
        }
        if span.kind == Tok::Str {
            if !closed_literal(word, language) { return Err(DelimiterError::UnsupportedLexicalContext); }
            if javascript(language) {
                // The owner's template interpolation does not qualify regexes;
                // their braces/backticks can expose fictitious code terminals.
                if word.starts_with('`')
                    && word.find("${").is_some_and(|at| word[at + 2..].contains('/')) {
                    return Err(DelimiterError::UnsupportedLexicalContext);
                }
                if word.starts_with('/') && significant.is_some_and(|last| last.kind == Tok::Punct
                    && &text[last.start..last.end] == "}") {
                    return Err(DelimiterError::UnsupportedLexicalContext);
                }
            }
        } else {
            // Digraphs and escaped identifiers are outside this ASCII-pair
            // route. Skipping a digraph brace could attach an ASCII brace to
            // the wrong outer block, even when all returned bytes are exact.
            if language == SymbolLanguage::Cpp {
                let remaining = &text[span.start..];
                if remaining.starts_with("\\u") || remaining.starts_with("\\U")
                    || ["<%", "%>", "<:", ":>", "%:"].iter().any(|mark| remaining.starts_with(mark)) {
                    return Err(DelimiterError::UnsupportedLexicalContext);
                }
            }
            // Swift regex literals are not qualified by this pinned owner:
            // their brackets currently look like punctuation. Without a
            // parser the slash/division boundary is ambiguous, so refuse code
            // slash terminals instead of navigating inside a possible regex.
            if language == SymbolLanguage::Swift && word.contains('/') {
                return Err(DelimiterError::UnsupportedLexicalContext);
            }
            if language == SymbolLanguage::Rust && word == "cr"
                && text.as_bytes().get(span.end).is_some_and(|b| matches!(b, b'#' | b'"')) {
                return Err(DelimiterError::UnsupportedLexicalContext);
            }
            if (javascript(language) && word == "\\")
                || (language == SymbolLanguage::CSharp && (text[span.start..].starts_with("\\u")
                    || text[span.start..].starts_with("\\U"))) {
                return Err(DelimiterError::UnsupportedLexicalContext);
            }
        }
        if !word.chars().all(char::is_whitespace) { significant = Some(*span); }
    }
    if previous != text.len() { return Err(DelimiterError::InvalidEvidence); }
    if language == SymbolLanguage::Cpp {
        // The pin declines some legal raw-string delimiters. The split prefix
        // is evidence of an unsupported boundary, never a license to scan it.
        for adjacent in spans.windows(2) {
            check(canceled)?;
            if adjacent[0].kind != Tok::Str && adjacent[1].kind == Tok::Str
                && adjacent[0].end == adjacent[1].start
                && matches!(&text[adjacent[0].start..adjacent[0].end], "R" | "u8R" | "uR" | "UR" | "LR") {
                return Err(DelimiterError::UnsupportedLexicalContext);
            }
        }
    }
    if javascript(language) {
        validate_javascript_context(text, spans, canceled).map_err(|error| match error {
            ImportError::Canceled => DelimiterError::Canceled,
            ImportError::ResourceDenied => DelimiterError::ResourceDenied,
            ImportError::UnsupportedLexicalContext => DelimiterError::UnsupportedLexicalContext,
            _ => DelimiterError::InvalidEvidence,
        })?;
    }
    check(canceled)
}

/// Check the boundary of an already opaque upstream token. This neither finds
/// literals in source nor interprets/interpolates their contents.
fn closed_literal(word: &str, language: SymbolLanguage) -> bool {
    let bytes = word.as_bytes();
    if javascript(language) && bytes.first() == Some(&b'/') {
        let end = bytes.iter().rposition(|b| !b.is_ascii_alphabetic()).unwrap_or(0);
        return end > 0 && bytes[end] == b'/' && !escaped(&bytes[..end]);
    }
    if language == SymbolLanguage::Go && bytes.first() == Some(&b'`') {
        return bytes.len() >= 2 && bytes.last() == Some(&b'`');
    }
    if javascript(language) && bytes.first() == Some(&b'`') {
        return bytes.len() >= 2 && bytes.last() == Some(&b'`') && !escaped(&bytes[..bytes.len() - 1]);
    }
    let Some(quote_at) = bytes.iter().position(|b| matches!(b, b'\'' | b'"')) else { return false; };
    let quote = bytes[quote_at];
    let prefix = &word[..quote_at];
    let mut hashes = 0;
    let mut width = 1;
    let mut backslash_escapes = true;
    match language {
        SymbolLanguage::Rust => {
            if prefix.starts_with('r') || prefix.starts_with("br") {
                let Some(rest) = prefix.strip_prefix("br").or_else(|| prefix.strip_prefix('r')) else { return false; };
                if quote != b'"' || !rest.bytes().all(|b| b == b'#') { return false; }
                hashes = rest.len(); backslash_escapes = false;
            } else if !matches!(prefix, "" | "b") { return false; }
        }
        SymbolLanguage::Python => {
            if quote_at > 2 || !prefix.bytes().all(|b| matches!(b, b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F')) {
                return false;
            }
            if bytes.get(quote_at..quote_at + 3).is_some_and(|s| s.iter().all(|&b| b == quote)) { width = 3; }
        }
        SymbolLanguage::Swift => {
            if quote != b'"' || !prefix.bytes().all(|b| b == b'#') { return false; }
            hashes = prefix.len(); backslash_escapes = hashes == 0;
            if bytes.get(quote_at..quote_at + 3) == Some(b"\"\"\"") { width = 3; }
        }
        SymbolLanguage::CSharp => {
            if !prefix.bytes().all(|b| matches!(b, b'$' | b'@')) { return false; }
            let quotes = bytes[quote_at..].iter().take_while(|&&b| b == b'"').count();
            if quotes >= 3 { width = quotes; }
            backslash_escapes = !prefix.contains('@') && width == 1;
        }
        SymbolLanguage::Java => {
            if !prefix.is_empty() { return false; }
            if bytes.get(..3) == Some(b"\"\"\"") { width = 3; }
        }
        SymbolLanguage::Cpp => {
            if prefix.ends_with('R') {
                if !matches!(prefix, "R" | "u8R" | "uR" | "UR" | "LR") || quote != b'"' { return false; }
                let Some(open) = word[quote_at + 1..].find('(').map(|at| quote_at + 1 + at) else { return false; };
                let delimiter = &bytes[quote_at + 1..open];
                let tail = delimiter.len() + 2;
                if bytes.len() < open + 1 + tail { return false; }
                let close = bytes.len() - tail;
                return bytes[close] == b')' && &bytes[close + 1..bytes.len() - 1] == delimiter
                    && bytes.last() == Some(&b'"');
            }
            if !matches!(prefix, "" | "u8" | "u" | "U" | "L") { return false; }
        }
        _ => if !prefix.is_empty() { return false; },
    }
    if bytes.len() < quote_at + width * 2 + hashes { return false; }
    let close = bytes.len() - hashes - width;
    bytes[close..close + width].iter().all(|&b| b == quote)
        && bytes[close + width..].iter().all(|&b| b == b'#')
        && (!backslash_escapes || !escaped(&bytes[..close]))
}

fn escaped(prefix: &[u8]) -> bool { prefix.iter().rev().take_while(|&&b| b == b'\\').count() % 2 != 0 }
