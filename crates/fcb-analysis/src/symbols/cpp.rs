#![forbid(unsafe_code)]

//! Bounded C/C++ declaration navigation over the pinned FMD C++ lexer.
//!
//! FMD owns token, comment, string and raw-string boundaries. This adapter
//! recognizes declaration shapes without preprocessing or compiler name binding.
//! Namespaces, named aggregates, aliases and ordinary callable declarations keep
//! exact source/name spans. Bodies, initializers, enum contents and macro lines
//! remain opaque; function-pointer declarators and operator/requires forms are
//! outside this conservative subset. Conditional branches are lexical candidates,
//! never claims about the active compiler configuration.
//!
//! Translation-phase constructs unsupported by the pinned lexer are refused,
//! following the same admission policy as the C/C++ import adapter. Delimiters
//! are matched iteratively before any candidate is published.

use franken_markdown::highlight::{Span, Tok, highlight_into};
use fcb_source::CaptureEncodingMap;

use super::{MAX_RECURSIVE_SCOPES, SymbolCandidate, SymbolError, mapped};
use crate::OutlineItemKind;

const UNPAIRED: usize = usize::MAX;

fn reserved<T>(capacity: usize) -> Result<Vec<T>, SymbolError> {
    let mut values = Vec::new();
    values.try_reserve_exact(capacity).map_err(|_| SymbolError::ResourceDenied)?;
    if values.capacity() > capacity { return Err(SymbolError::ResourceDenied); }
    Ok(values)
}

fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), SymbolError> {
    if canceled() { Err(SymbolError::Canceled) } else { Ok(()) }
}

/// These are admission guards, not a replacement lexer/preprocessor. A splice
/// can change even a line-comment boundary before the owner sees tokens.
fn check_source(text: &str, canceled: &mut impl FnMut() -> bool) -> Result<(), SymbolError> {
    let bytes = text.as_bytes();
    for (i, &byte) in bytes.iter().enumerate() {
        if i & 1023 == 0 { check(canceled)?; }
        if byte == b'\r' && bytes.get(i + 1) != Some(&b'\n') {
            return Err(SymbolError::UnsupportedLexicalContext);
        }
        if byte == b'?' && bytes.get(i + 1) == Some(&b'?')
            && bytes.get(i + 2).is_some_and(|third| b"=/'()!<>-".contains(third)) {
            return Err(SymbolError::UnsupportedLexicalContext);
        }
        if byte == b'\\' {
            let mut next = i + 1;
            while bytes.get(next).is_some_and(|&b| matches!(b, b' ' | b'\t' | 0x0b | 0x0c)) {
                if next & 1023 == 0 { check(canceled)?; }
                next += 1;
            }
            if bytes.get(next).is_some_and(|&b| matches!(b, b'\n' | b'\r')) {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
        }
    }
    check(canceled)
}

#[derive(Clone, Copy)]
struct Name { start: usize, end: usize }

#[derive(Clone, Copy)]
struct Scope {
    cursor: usize,
    end: usize,
    parent: Option<u64>,
    depth: usize,
    type_name: Option<Name>,
}

#[derive(Clone, Copy)]
struct Body { open: usize, close: usize, type_name: Option<Name> }

struct Declaration {
    name: Name,
    kind: OutlineItemKind,
    end: usize,
    next: usize,
    body: Option<Body>,
}

struct Prefix { cursor: usize, linkage: bool }

struct QualifiedName {
    name: Name,
    previous: Option<Name>,
    next: usize,
    destructor: bool,
}

struct Tokens<'text> {
    text: &'text str,
    spans: Vec<Span>,
    closes: Vec<usize>,
}

impl<'text> Tokens<'text> {
    fn new(text: &'text str, line_starts: &[usize], canceled: &mut impl FnMut() -> bool)
        -> Result<Self, SymbolError> {
        check_source(text, canceled)?;
        let mut spans = reserved(text.len().max(1))?;
        check(canceled)?;
        highlight_into("cpp", text, &mut spans);
        check(canceled)?;
        let mut previous = 0;
        for span in &spans {
            check(canceled)?;
            if span.start != previous || span.start >= span.end || text.get(span.start..span.end).is_none() {
                return Err(SymbolError::InvalidEvidence);
            }
            previous = span.end;
        }
        if previous != text.len() { return Err(SymbolError::InvalidEvidence); }
        // Legal raw delimiters include punctuation that this pinned lexer
        // declines. Never reinterpret their contents as ordinary declarations.
        for pair in spans.windows(2) {
            check(canceled)?;
            if pair[0].kind != Tok::Str && pair[1].kind == Tok::Str
                && pair[0].end == pair[1].start
                && matches!(&text[pair[0].start..pair[0].end], "R" | "u8R" | "uR" | "UR" | "LR")
                && text[pair[1].start..pair[1].end].starts_with('"') {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
        }
        let mut significant = reserved(spans.len())?;
        let mut directive_end = 0;
        let mut last_significant_end = 0;
        for span in spans {
            check(canceled)?;
            if span.start < directive_end {
                // A multiline raw literal is one preprocessing token. The
                // owner's padded bare-# scanner may also consume a next-line
                // word into a Plain gap. Neither can be safely truncated at
                // this physical-line boundary.
                if span.kind != Tok::Comment && span.end > directive_end
                    && !text[directive_end..span.end].chars().all(char::is_whitespace) {
                    return Err(SymbolError::UnsupportedLexicalContext);
                }
                continue;
            }
            if span.kind == Tok::Comment { continue; }
            let word = &text[span.start..span.end];
            if word.chars().all(char::is_whitespace) { continue; }
            let line_index = line_starts.partition_point(|&start| start <= span.start).saturating_sub(1);
            let line_start = line_starts[line_index];
            if span.kind != Tok::Str && word.starts_with('#') && last_significant_end <= line_start {
                directive_end = text[span.start..].find('\n').map_or(text.len(), |n| span.start + n);
                // The owner's directive scanner can cross a bare '#'+newline.
                // Refuse that ambiguous token stream instead of losing source
                // from the next physical line while skipping this directive.
                if span.end > directive_end {
                    return Err(SymbolError::UnsupportedLexicalContext);
                }
                continue;
            }
            if span.kind != Tok::Str {
                let remaining = &text[span.start..];
                if remaining.starts_with("\\u") || remaining.starts_with("\\U")
                    || ["<%", "%>", "<:", ":>", "%:"].iter().any(|mark| remaining.starts_with(mark)) {
                    return Err(SymbolError::UnsupportedLexicalContext);
                }
            }
            last_significant_end = span.end;
            significant.push(span);
        }
        let spans = significant;
        let mut closes = reserved(spans.len())?;
        closes.resize(spans.len(), UNPAIRED);
        let mut stack = reserved(MAX_RECURSIVE_SCOPES)?;
        for (i, span) in spans.iter().enumerate() {
            check(canceled)?;
            if span.kind != Tok::Punct { continue; }
            let word = &text[span.start..span.end];
            match word {
                "(" | "[" | "{" => {
                    if stack.len() == MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
                    stack.push((i, word));
                }
                ")" | "]" | "}" => {
                    let (open, expected) = stack.pop().ok_or(SymbolError::InvalidEvidence)?;
                    if !matches!((expected, word), ("(", ")") | ("[", "]") | ("{", "}")) {
                        return Err(SymbolError::InvalidEvidence);
                    }
                    closes[open] = i;
                }
                _ => {}
            }
        }
        if !stack.is_empty() { return Err(SymbolError::InvalidEvidence); }
        Ok(Self { text, spans, closes })
    }

    fn word(&self, i: usize) -> &'text str {
        self.spans.get(i).map_or("", |span| &self.text[span.start..span.end])
    }

    fn after_group(&self, i: usize) -> usize {
        if self.closes[i] == UNPAIRED { i + 1 } else { self.closes[i] + 1 }
    }

    fn name(&self, i: usize) -> Option<Name> {
        let span = *self.spans.get(i)?;
        if !matches!(span.kind, Tok::Plain | Tok::Type | Tok::Func) { return None; }
        let mut chars = self.word(i).chars();
        let start = |c: char| c.is_alphabetic() || matches!(c, '_' | '\u{200c}' | '\u{200d}');
        if !chars.next().is_some_and(start) || !chars.all(|c| start(c) || c.is_numeric()) {
            return None;
        }
        Some(Name { start: span.start, end: span.end })
    }

    fn attributes(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        while i + 1 < end && self.word(i) == "[" && self.word(i + 1) == "[" {
            check(canceled)?;
            i = self.after_group(i);
        }
        Ok(i)
    }

    fn generic_end(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        if self.word(i) != "<" { return Ok(Some(i)); }
        let mut depth = 0usize;
        while i < end {
            check(canceled)?;
            match self.word(i) {
                "<" => {
                    depth += 1;
                    if depth > MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
                }
                ">" | ">>" => {
                    let Some(next) = depth.checked_sub(self.word(i).len()) else { return Ok(None); };
                    depth = next;
                    if depth == 0 { return Ok(Some(i + 1)); }
                }
                ";" | "{" | "}" | "=" | "<<" => return Ok(None),
                _ => {}
            }
            i = self.after_group(i);
        }
        Ok(None)
    }

    fn prefix(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Prefix>, SymbolError> {
        let mut linkage = false;
        while i < end {
            check(canceled)?;
            let attributed = self.attributes(i, end, canceled)?;
            if attributed != i { i = attributed; continue; }
            match self.word(i) {
                "template" => {
                    if self.word(i + 1) != "<" { return Ok(None); }
                    let Some(next) = self.generic_end(i + 1, end, canceled)? else { return Ok(None); };
                    i = next;
                }
                "extern" => {
                    i += 1;
                    if matches!(self.word(i), "\"C\"" | "\"C++\"") {
                        linkage = true;
                        i += 1;
                    }
                }
                "explicit" => {
                    i += 1;
                    if self.word(i) == "(" { i = self.after_group(i); }
                }
                "alignas" | "_Alignas" | "__attribute__" | "__declspec" => {
                    if self.word(i + 1) != "(" { return Ok(None); }
                    i = self.after_group(i + 1);
                }
                "inline" | "export" | "friend" | "virtual" | "static" | "constexpr"
                | "consteval" | "constinit" | "register" | "thread_local" | "_Noreturn" => { i += 1; }
                _ => break,
            }
        }
        Ok(Some(Prefix { cursor: i, linkage }))
    }

    fn qualified_name(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<QualifiedName>, SymbolError> {
        let global = self.word(i) == "::";
        if global { i += 1; }
        let mut previous = None;
        loop {
            check(canceled)?;
            let destructor = self.word(i) == "~";
            let first = i;
            if destructor { i += 1; }
            let Some(mut name) = self.name(i) else { return Ok(None); };
            if destructor { name.start = self.spans[first].start; }
            let Some(next) = self.generic_end(i + 1, end, canceled)? else { return Ok(None); };
            if self.word(next) != "::" {
                return Ok(Some(QualifiedName { name, previous, next, destructor }));
            }
            if destructor { return Ok(None); }
            previous = Some(name);
            i = next + 1;
            if self.word(i) == "template" { i += 1; }
            if i >= end { return Ok(None); }
        }
    }

    fn primitive(&self, i: usize) -> bool {
        matches!(self.word(i), "void" | "bool" | "_Bool" | "char" | "char8_t" | "char16_t"
            | "char32_t" | "wchar_t" | "short" | "int" | "long" | "signed" | "unsigned"
            | "float" | "double" | "_Complex" | "_Imaginary")
    }

    fn type_end(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        while i < end && matches!(self.word(i), "const" | "volatile" | "typename") { i += 1; }
        if i >= end { return Ok(None); }
        if self.primitive(i) {
            while i < end && self.primitive(i) { check(canceled)?; i += 1; }
        } else if self.word(i) == "auto" {
            i += 1;
        } else if matches!(self.word(i), "decltype" | "typeof" | "__typeof__") && self.word(i + 1) == "(" {
            i = self.after_group(i + 1);
        } else {
            if matches!(self.word(i), "struct" | "class" | "enum" | "union") {
                i += 1;
                if matches!(self.word(i), "class" | "struct") { i += 1; }
            }
            let Some(name) = self.qualified_name(i, end, canceled)? else { return Ok(None); };
            if name.destructor { return Ok(None); }
            i = name.next;
        }
        while i < end {
            check(canceled)?;
            if matches!(self.word(i), "const" | "volatile" | "*" | "&" | "&&" | "restrict"
                | "__restrict" | "__restrict__") { i += 1; }
            else {
                let next = self.attributes(i, end, canceled)?;
                if next == i { break; }
                i = next;
            }
        }
        Ok(Some(i))
    }

    fn namespace(&self, i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        if self.word(i) != "namespace" { return Ok(None); }
        let Some(mut name) = self.name(i + 1) else { return Ok(None); };
        let mut next = i + 2;
        while next + 1 < scope.end && self.word(next) == "::" {
            check(canceled)?;
            let Some(part) = self.name(next + 1) else { return Ok(None); };
            name.end = part.end;
            next += 2;
        }
        next = self.attributes(next, scope.end, canceled)?;
        if self.word(next) == "=" {
            let Some(target) = self.qualified_name(next + 1, scope.end, canceled)? else { return Ok(None); };
            if target.destructor || self.word(target.next) != ";" { return Ok(None); }
            return Ok(Some(Declaration { name, kind: OutlineItemKind::Module,
                end: self.spans[target.next].end, next: target.next + 1, body: None }));
        }
        if self.word(next) != "{" { return Ok(None); }
        let close = self.closes[next];
        Ok(Some(Declaration { name, kind: OutlineItemKind::Module, end: self.spans[close].end,
            next: close + 1, body: Some(Body { open: next, close, type_name: None }) }))
    }

    fn aggregate(&self, mut i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let is_typedef = self.word(i) == "typedef";
        if is_typedef { i += 1; }
        let kind = match self.word(i) {
            "class" => OutlineItemKind::Class,
            "struct" => OutlineItemKind::Struct,
            "enum" => OutlineItemKind::Enum,
            _ => return Ok(None),
        };
        let is_enum = kind == OutlineItemKind::Enum;
        i += 1;
        if is_enum && matches!(self.word(i), "class" | "struct") { i += 1; }
        i = self.attributes(i, scope.end, canceled)?;
        let Some(name) = self.name(i) else { return Ok(None); };
        let Some(mut next) = self.generic_end(i + 1, scope.end, canceled)? else { return Ok(None); };
        next = self.attributes(next, scope.end, canceled)?;
        if self.word(next) == "final" { next += 1; }
        if self.word(next) == ":" {
            next += 1;
            while next < scope.end && !matches!(self.word(next), "{" | ";") {
                check(canceled)?;
                if matches!(self.word(next), "=" | "}") { return Ok(None); }
                if self.word(next) == "<" {
                    let Some(after) = self.generic_end(next, scope.end, canceled)? else { return Ok(None); };
                    next = after;
                } else { next = self.after_group(next); }
            }
        }
        if self.word(next) == ";" && !is_typedef {
            return Ok(Some(Declaration { name, kind, end: self.spans[next].end,
                next: next + 1, body: None }));
        }
        if self.word(next) != "{" { return Ok(None); }
        let close = self.closes[next];
        let mut after = close + 1;
        // A named C typedef aggregate remains identified by its exact tag.
        // Simple typedef aliases without a body are handled separately below.
        if is_typedef && self.name(after).is_some() && self.word(after + 1) == ";" { after += 1; }
        if self.word(after) == ";" { after += 1; }
        Ok(Some(Declaration { name, kind, end: self.spans[after - 1].end, next: after,
            body: (!is_enum).then_some(Body { open: next, close, type_name: Some(name) }) }))
    }

    fn alias(&self, i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let (name, mut next) = match self.word(i) {
            "using" => {
                let Some(name) = self.name(i + 1) else { return Ok(None); };
                if self.word(i + 2) != "=" { return Ok(None); }
                let Some(after) = self.type_end(i + 3, scope.end, canceled)? else { return Ok(None); };
                (name, after)
            }
            "typedef" => {
                let Some(after) = self.type_end(i + 1, scope.end, canceled)? else { return Ok(None); };
                let Some(name) = self.name(after) else { return Ok(None); };
                (name, after + 1)
            }
            _ => return Ok(None),
        };
        // Array typedefs are a declaration shape, not function-pointer parsing.
        while self.word(next) == "[" { check(canceled)?; next = self.after_group(next); }
        if self.word(next) != ";" { return Ok(None); }
        Ok(Some(Declaration { name, kind: OutlineItemKind::TypeAlias,
            end: self.spans[next].end, next: next + 1, body: None }))
    }

    fn ellipsis(&self, i: usize) -> bool {
        self.word(i) == "." && self.word(i + 1) == "." && self.word(i + 2) == "."
    }

    fn parameters(&self, open: usize, canceled: &mut impl FnMut() -> bool) -> Result<bool, SymbolError> {
        let close = self.closes[open];
        let mut i = open + 1;
        while i < close {
            check(canceled)?;
            if self.ellipsis(i) { return Ok(i + 3 == close); }
            let Some(after) = self.type_end(i, close, canceled)? else { return Ok(false); };
            i = after;
            if self.ellipsis(i) { i += 3; }
            if self.name(i).is_some() { i += 1; }
            while self.word(i) == "[" { check(canceled)?; i = self.after_group(i); }
            if self.word(i) == "=" {
                i += 1;
                let first = i;
                while i < close && self.word(i) != "," {
                    check(canceled)?;
                    // A default's paired expression remains opaque. A template
                    // call is skipped only when its angle group is recognized.
                    if self.word(i) == "<" {
                        if let Some(next) = self.generic_end(i, close, canceled)? { i = next; continue; }
                    }
                    i = self.after_group(i);
                }
                if i == first { return Ok(false); }
            }
            if i == close { return Ok(true); }
            if self.word(i) != "," { return Ok(false); }
            i += 1;
            if i == close { return Ok(false); }
        }
        Ok(true)
    }

    fn initializers(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        // The caller consumed ':' after a constructor's parameter list.
        loop {
            check(canceled)?;
            let Some(name) = self.qualified_name(i, end, canceled)? else { return Ok(None); };
            i = name.next;
            if name.destructor || !matches!(self.word(i), "(" | "{") { return Ok(None); }
            i = self.after_group(i);
            if self.ellipsis(i) { i += 3; }
            if self.word(i) != "," { return Ok(Some(i)); }
            i += 1;
        }
    }

    fn callable_tail(&self, mut i: usize, end: usize, constructor: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<(usize, usize)>, SymbolError> {
        while i < end {
            check(canceled)?;
            let next = self.attributes(i, end, canceled)?;
            if next != i { i = next; continue; }
            match self.word(i) {
                "const" | "volatile" | "&" | "&&" | "override" | "final" => { i += 1; }
                "noexcept" | "throw" => {
                    i += 1;
                    if self.word(i) == "(" { i = self.after_group(i); }
                }
                "->" => {
                    let Some(next) = self.type_end(i + 1, end, canceled)? else { return Ok(None); };
                    i = next;
                }
                ":" if constructor => {
                    let Some(next) = self.initializers(i + 1, end, canceled)? else { return Ok(None); };
                    i = next;
                }
                "=" if matches!(self.word(i + 1), "default" | "delete" | "0") && self.word(i + 2) == ";" => {
                    return Ok(Some((self.spans[i + 2].end, i + 3)));
                }
                ";" => return Ok(Some((self.spans[i].end, i + 1))),
                "{" => {
                    let close = self.closes[i];
                    return Ok(Some((self.spans[close].end, close + 1)));
                }
                _ => return Ok(None),
            }
        }
        Ok(None)
    }

    fn callable(&self, i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let mut constructor = false;
        let mut destructor = false;
        let mut selected = None;
        if let Some(name) = self.qualified_name(i, scope.end, canceled)? {
            let spelling = &self.text[name.name.start..name.name.end];
            let expected = name.previous.or(scope.type_name)
                .map(|name| &self.text[name.start..name.end]);
            if self.word(name.next) == "(" && expected.is_some_and(|expected|
                spelling.strip_prefix('~').unwrap_or(spelling) == expected) {
                constructor = !name.destructor;
                destructor = name.destructor;
                selected = Some(name);
            }
        }
        let name = match selected {
            Some(name) => name,
            None => {
                let Some(after_type) = self.type_end(i, scope.end, canceled)? else { return Ok(None); };
                let Some(name) = self.qualified_name(after_type, scope.end, canceled)? else { return Ok(None); };
                if name.destructor { return Ok(None); }
                name
            }
        };
        if self.word(name.next) != "(" || !self.parameters(name.next, canceled)? { return Ok(None); }
        let Some((end, next)) = self.callable_tail(self.after_group(name.next), scope.end, constructor, canceled)?
            else { return Ok(None); };
        let kind = if constructor { OutlineItemKind::Initializer }
            else if destructor { OutlineItemKind::Deinitializer }
            else if scope.type_name.is_some() { OutlineItemKind::Method }
            else { OutlineItemKind::Function };
        Ok(Some(Declaration { name: name.name, kind, end, next, body: None }))
    }

    fn skip_statement(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        while i < end {
            check(canceled)?;
            if self.word(i) == ";" { return Ok(i + 1); }
            if self.word(i) == "{" {
                let next = self.after_group(i);
                return Ok(if self.word(next) == ";" { next + 1 } else { next });
            }
            i = self.after_group(i);
        }
        Ok(end)
    }
}

pub(super) fn extract(map: &CaptureEncodingMap, line_starts: &[usize], limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(Vec<SymbolCandidate>, bool), SymbolError> {
    let tokens = Tokens::new(map.decoded_text(), line_starts, canceled)?;
    let mut candidates = reserved(limit)?;
    let mut scopes = reserved(MAX_RECURSIVE_SCOPES + 1)?;
    scopes.push(Scope { cursor: 0, end: tokens.spans.len(), parent: None, depth: 0, type_name: None });
    while let Some(scope) = scopes.last().copied() {
        check(canceled)?;
        let start = scope.cursor;
        if start >= scope.end { scopes.pop(); continue; }
        if matches!(tokens.word(start), "public" | "private" | "protected") && tokens.word(start + 1) == ":" {
            scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = start + 2;
            continue;
        }
        let Some(prefix) = tokens.prefix(start, scope.end, canceled)? else {
            scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = tokens.skip_statement(start, scope.end, canceled)?;
            continue;
        };
        let i = prefix.cursor;
        // Anonymous namespaces and explicit language linkage are transparent
        // lexical scopes. They cannot invent a named parent or alter depth.
        let transparent = if tokens.word(i) == "namespace" && tokens.word(i + 1) == "{" { Some(i + 1) }
            else if prefix.linkage && tokens.word(i) == "{" { Some(i) } else { None };
        if let Some(open) = transparent {
            let close = tokens.closes[open];
            scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = close + 1;
            scopes.push(Scope { cursor: open + 1, end: close, ..scope });
            continue;
        }
        let declaration = if let Some(declaration) = tokens.namespace(i, scope, canceled)? { Some(declaration) }
            else if let Some(declaration) = tokens.aggregate(i, scope, canceled)? { Some(declaration) }
            else if let Some(declaration) = tokens.alias(i, scope, canceled)? { Some(declaration) }
            else { tokens.callable(i, scope, canceled)? };
        let next = if let Some(declaration) = declaration {
            if candidates.len() == limit { return Ok((candidates, true)); }
            let name = declaration.name;
            let begin = tokens.spans[start].start;
            if name.start < begin || name.end > declaration.end || name.start >= name.end {
                return Err(SymbolError::InvalidEvidence);
            }
            if name.end - name.start > 4096 { return Err(SymbolError::ComplexityLimit); }
            let id = candidates.len() as u64 + 1;
            candidates.push(SymbolCandidate { id, parent: scope.parent, depth: scope.depth,
                name: tokens.text[name.start..name.end].to_owned(), kind: declaration.kind,
                range: mapped(map, begin as u64, declaration.end as u64)?,
                name_range: Some(mapped(map, name.start as u64, name.end as u64)?),
                line: line_starts.partition_point(|&line| line <= begin) as u64 });
            if let Some(body) = declaration.body {
                scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = declaration.next;
                scopes.push(Scope { cursor: body.open + 1, end: body.close, parent: Some(id),
                    depth: scope.depth + 1, type_name: body.type_name });
                continue;
            }
            declaration.next
        } else { tokens.skip_statement(start, scope.end, canceled)? };
        if next <= start { return Err(SymbolError::InvalidEvidence); }
        scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = next;
    }
    Ok((candidates, false))
}
