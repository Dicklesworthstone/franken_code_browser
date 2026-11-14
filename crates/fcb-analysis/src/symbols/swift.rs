#![forbid(unsafe_code)]

//! Conservative Swift declaration navigation over the pinned FMD Swift lexer.
//!
//! The upstream lexer owns all token, nested-comment, raw/multiline-string and
//! interpolation boundaries. This adapter only recognizes declaration shapes.
//! Function bodies, property/accessor bodies, closures, attributes and imports
//! are opaque. Results are candidates, never compiler definitions or a complete
//! Swift inventory. Local declarations, enum cases, tuple bindings and macros
//! are deliberately outside this route.
//!
//! Every token is validated against the captured decoded text before use.
//! Delimiters are matched iteratively with a fixed depth bound; malformed input
//! is refused rather than publishing invented closing ranges. All output is
//! mapped back through the capture's encoding map.

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

fn checkpoint(canceled: &mut impl FnMut() -> bool) -> Result<(), SymbolError> {
    if canceled() { Err(SymbolError::Canceled) } else { Ok(()) }
}

struct Tokens<'text> {
    text: &'text str,
    spans: Vec<Span>,
    closes: Vec<usize>,
}

#[derive(Clone, Copy)]
struct Name { start: usize, end: usize }

struct Declaration {
    name: Name,
    kind: OutlineItemKind,
    end: usize,
    next: usize,
    body: Option<(usize, usize)>,
    protocol: bool,
}

struct Tail { end: usize, next: usize, body: Option<(usize, usize)> }

#[derive(Clone, Copy)]
struct Scope { cursor: usize, end: usize, parent: Option<u64>, protocol: bool }

impl<'text> Tokens<'text> {
    fn new(text: &'text str, canceled: &mut impl FnMut() -> bool) -> Result<Self, SymbolError> {
        // A nonempty span consumes at least one byte. Reserve before the
        // non-resumable upstream call so it cannot grow unaccounted token state.
        let mut spans = reserved(text.len().max(1))?;
        checkpoint(canceled)?;
        highlight_into("swift", text, &mut spans);
        checkpoint(canceled)?;
        let mut previous = 0;
        for span in &spans {
            checkpoint(canceled)?;
            if span.start != previous || span.start >= span.end || text.get(span.start..span.end).is_none() {
                return Err(SymbolError::InvalidEvidence);
            }
            previous = span.end;
        }
        if previous != text.len() { return Err(SymbolError::InvalidEvidence); }
        // Keep string spans as opaque tokens; removing them could join unrelated
        // syntax across a literal. Comments and whitespace only separate tokens.
        spans.retain(|span| span.kind != Tok::Comment
            && !text[span.start..span.end].chars().all(char::is_whitespace));
        let mut closes = reserved(spans.len())?;
        closes.resize(spans.len(), UNPAIRED);
        let mut stack = reserved(MAX_RECURSIVE_SCOPES)?;
        for (i, span) in spans.iter().enumerate() {
            checkpoint(canceled)?;
            if span.kind != Tok::Punct { continue; }
            let terminal = &text[span.start..span.end];
            match terminal {
                "(" | "[" | "{" => {
                    if stack.len() == MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
                    stack.push((i, terminal));
                }
                ")" | "]" | "}" => {
                    let (open, expected) = stack.pop().ok_or(SymbolError::InvalidEvidence)?;
                    if !matches!((expected, terminal), ("(", ")") | ("[", "]") | ("{", "}")) {
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
        let span = self.spans[i];
        &self.text[span.start..span.end]
    }

    fn after_group(&self, i: usize) -> usize {
        if self.closes[i] == UNPAIRED { i + 1 } else { self.closes[i] + 1 }
    }

    fn newline(&self, previous: usize, next: usize) -> bool {
        self.text[self.spans[previous].end..self.spans[next].start]
            .bytes().any(|byte| byte == b'\n' || byte == b'\r')
    }

    fn name(&self, i: usize) -> Option<Name> {
        let span = *self.spans.get(i)?;
        if !matches!(span.kind, Tok::Plain | Tok::Type | Tok::Func) { return None; }
        let word = self.word(i);
        // The selection for a single escaped name excludes its backticks. The
        // display spelling is exactly the selected text, including in UTF-16.
        let (start, end, name) = if let Some(name) = word.strip_prefix('\u{60}').and_then(|s| s.strip_suffix('\u{60}')) {
            (span.start + 1, span.end - 1, name)
        } else { (span.start, span.end, word) };
        let mut chars = name.chars();
        if !chars.next().is_some_and(|c| c.is_alphabetic() || c == '_' || c == '$')
            || !chars.all(|c| c.is_alphanumeric() || c == '_' || c == '$') {
            return None;
        }
        Some(Name { start, end })
    }

    fn function_name(&self, i: usize, end: usize) -> Option<(Name, usize)> {
        if let Some(name) = self.name(i) { return Some((name, i + 1)); }
        // Common operator declarations retain their exact original spelling.
        let span = *self.spans.get(i)?;
        if span.kind != Tok::Operator { return None; }
        let mut next = i + 1;
        while next < end && self.spans[next].kind == Tok::Operator
            && self.spans[next - 1].end == self.spans[next].start { next += 1; }
        Some((Name { start: span.start, end: self.spans[next - 1].end }, next))
    }

    fn generic_end(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        if i >= end || self.word(i) != "<" { return Ok(Some(i)); }
        let mut depth = 0usize;
        while i < end {
            checkpoint(canceled)?;
            match self.word(i) {
                "<" => {
                    depth += 1;
                    if depth > MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
                }
                ">" | ">>" => {
                    let count = self.word(i).len();
                    let Some(remaining) = depth.checked_sub(count) else { return Ok(None); };
                    depth = remaining;
                    if depth == 0 { return Ok(Some(i + 1)); }
                }
                "{" | ";" => return Ok(None),
                _ => {}
            }
            i = self.after_group(i);
        }
        Ok(None)
    }

    fn parameters_end(&self, start: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        let Some(i) = self.generic_end(start, end, canceled)? else { return Ok(None); };
        Ok((i < end && self.word(i) == "(").then(|| self.closes[i] + 1))
    }

    fn modifier(&self, i: usize, end: usize) -> bool {
        match self.word(i) {
            "public" | "private" | "fileprivate" | "internal" | "open" | "package"
            | "static" | "final" | "override" | "required" | "convenience" | "dynamic"
            | "nonisolated" | "isolated" | "mutating" | "nonmutating" | "optional"
            | "indirect" | "lazy" | "weak" | "unowned" | "prefix" | "postfix"
            | "borrowing" | "consuming" | "distributed" => true,
            // "class func" and "class var" are members, not named type declarations.
            "class" => i + 1 < end && matches!(self.word(i + 1),
                "func" | "var" | "subscript" | "override" | "final" | "dynamic"),
            _ => false,
        }
    }

    fn starts_declaration(&self, i: usize, end: usize) -> bool {
        matches!(self.word(i), "struct" | "class" | "enum" | "actor" | "protocol" | "extension"
            | "func" | "init" | "deinit" | "subscript" | "var" | "let" | "typealias" | "associatedtype"
            | "import" | "case")
            || self.modifier(i, end) || self.word(i).starts_with('@') || self.word(i).starts_with('#')
    }

    fn continues(&self, previous: usize, next: usize) -> bool {
        matches!(self.word(previous), "=" | ":" | "," | "->" | "where" | "&" | "<")
            || self.spans[previous].kind == Tok::Operator
            || matches!(self.word(next), "{" | "." | "," | ":" | "where" | "async" | "throws" | "rethrows" | "->")
            || self.spans[next].kind == Tok::Operator
    }

    fn tail(&self, start: usize, end: usize, bodies: bool, canceled: &mut impl FnMut() -> bool)
        -> Result<Tail, SymbolError> {
        let mut i = start;
        let mut previous = start - 1;
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == ";" {
                return Ok(Tail { end: self.spans[i].end, next: i + 1, body: None });
            }
            // Do not lend a later declaration's body to a malformed header,
            // even when both declarations appear on the same physical line.
            if matches!(self.word(i), "func" | "struct" | "enum" | "extension" | "typealias"
                | "associatedtype" | "deinit" | "var" | "let")
                || (self.newline(previous, i)
                    && (self.starts_declaration(i, end) || !self.continues(previous, i))) {
                break;
            }
            if self.word(i) == "{" && bodies {
                let close = self.closes[i];
                return Ok(Tail { end: self.spans[close].end, next: close + 1, body: Some((i, close)) });
            }
            let next = self.after_group(i);
            previous = next - 1;
            i = next;
        }
        Ok(Tail { end: self.spans[previous].end, next: i, body: None })
    }

    fn declaration(&self, i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        if self.spans[i].kind != Tok::Keyword { return Ok(None); }
        let end = scope.end;
        let keyword = self.word(i);
        let type_kind = match keyword {
            "struct" => Some(OutlineItemKind::Struct), "class" => Some(OutlineItemKind::Class),
            "enum" => Some(OutlineItemKind::Enum), "actor" => Some(OutlineItemKind::Actor),
            "protocol" => Some(OutlineItemKind::Protocol), "extension" => Some(OutlineItemKind::Extension),
            _ => None,
        };
        if let Some(kind) = type_kind {
            let Some(mut name) = self.name(i + 1) else { return Ok(None); };
            let mut next = i + 2;
            if keyword == "extension" {
                while next + 1 < end && self.word(next) == "." {
                    checkpoint(canceled)?;
                    if self.name(next + 1).is_none() { return Ok(None); }
                    // Qualified extensions retain their complete source spelling,
                    // including any separators/backticks, rather than normalizing
                    // a name that no longer agrees with its selected source.
                    name.start = self.spans[i + 1].start;
                    name.end = self.spans[next + 1].end;
                    next += 2;
                }
            }
            let tail = self.tail(next, end, true, canceled)?;
            if tail.body.is_none() { return Ok(None); }
            return Ok(Some(Declaration { name, kind, end: tail.end, next: tail.next,
                body: tail.body, protocol: keyword == "protocol" }));
        }
        let (name, kind, next, bodyless) = match keyword {
            "func" => {
                let Some((name, next)) = self.function_name(i + 1, end) else { return Ok(None); };
                let Some(next) = self.parameters_end(next, end, canceled)? else { return Ok(None); };
                (name, if scope.parent.is_some() { OutlineItemKind::Method } else { OutlineItemKind::Function },
                    next, scope.protocol)
            }
            "init" | "subscript" if scope.parent.is_some() => {
                let mut next = i + 1;
                if keyword == "init" && next < end && matches!(self.word(next), "?" | "!") { next += 1; }
                let Some(next) = self.parameters_end(next, end, canceled)? else { return Ok(None); };
                (Name { start: self.spans[i].start, end: self.spans[i].end },
                    if keyword == "init" { OutlineItemKind::Initializer } else { OutlineItemKind::Subscript },
                    next, scope.protocol)
            }
            "deinit" if scope.parent.is_some() => (
                Name { start: self.spans[i].start, end: self.spans[i].end },
                OutlineItemKind::Deinitializer, i + 1, false),
            "var" | "let" => {
                let Some(name) = self.name(i + 1) else { return Ok(None); };
                let next = i + 2;
                if next >= end || !matches!(self.word(next), ":" | "=") { return Ok(None); }
                (name, if scope.parent.is_some() { OutlineItemKind::Property }
                    else if keyword == "var" { OutlineItemKind::Variable } else { OutlineItemKind::Const },
                    next, true)
            }
            "typealias" | "associatedtype" => {
                let Some(name) = self.name(i + 1) else { return Ok(None); };
                let Some(next) = self.generic_end(i + 2, end, canceled)? else { return Ok(None); };
                if keyword == "typealias" && (next >= end || self.word(next) != "=") { return Ok(None); }
                if keyword == "associatedtype" && !scope.protocol { return Ok(None); }
                let tail = self.tail(next, end, false, canceled)?;
                return Ok(Some(Declaration { name, kind: OutlineItemKind::TypeAlias,
                    end: tail.end, next: tail.next, body: None, protocol: false }));
            }
            _ => return Ok(None),
        };
        let tail = self.tail(next, end, true, canceled)?;
        if tail.body.is_none() && !bodyless { return Ok(None); }
        Ok(Some(Declaration { name, kind, end: tail.end, next: tail.next, body: None, protocol: false }))
    }

    fn skip_statement(&self, start: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        let mut i = start;
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == ";" { return Ok(i + 1); }
            let next = self.after_group(i);
            if next >= end || self.newline(next - 1, next) { return Ok(next); }
            i = next;
        }
        Ok(end)
    }
}

pub(super) fn extract(map: &CaptureEncodingMap, line_starts: &[usize], limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(Vec<SymbolCandidate>, bool), SymbolError> {
    let tokens = Tokens::new(map.decoded_text(), canceled)?;
    let mut candidates = reserved(limit)?;
    let mut scopes = reserved(MAX_RECURSIVE_SCOPES + 1)?;
    scopes.push(Scope { cursor: 0, end: tokens.spans.len(), parent: None, protocol: false });
    while let Some(scope) = scopes.last().copied() {
        checkpoint(canceled)?;
        let i = scope.cursor;
        if i >= scope.end { scopes.pop(); continue; }
        let next = if tokens.word(i).starts_with('@') || tokens.modifier(i, scope.end) {
            let next = i + 1;
            if next < scope.end && tokens.word(next) == "(" { tokens.after_group(next) } else { next }
        } else if let Some(declaration) = tokens.declaration(i, scope, canceled)? {
            if candidates.len() == limit { return Ok((candidates, true)); }
            let name = declaration.name;
            let start = tokens.spans[i].start;
            if name.start < start || name.end > declaration.end || name.start >= name.end {
                return Err(SymbolError::InvalidEvidence);
            }
            if name.end - name.start > 4096 { return Err(SymbolError::ComplexityLimit); }
            let id = candidates.len() as u64 + 1;
            candidates.push(SymbolCandidate { id, parent: scope.parent, depth: scopes.len() - 1,
                name: tokens.text[name.start..name.end].to_owned(), kind: declaration.kind,
                range: mapped(map, start as u64, declaration.end as u64)?,
                name_range: Some(mapped(map, name.start as u64, name.end as u64)?),
                line: line_starts.partition_point(|&line| line <= start) as u64 });
            if let Some((open, close)) = declaration.body {
                scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = declaration.next;
                scopes.push(Scope { cursor: open + 1, end: close, parent: Some(id), protocol: declaration.protocol });
                continue;
            }
            declaration.next
        } else { tokens.skip_statement(i, scope.end, canceled)? };
        if next <= i { return Err(SymbolError::InvalidEvidence); }
        scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = next;
    }
    Ok((candidates, false))
}
