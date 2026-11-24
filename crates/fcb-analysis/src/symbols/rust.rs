#![forbid(unsafe_code)]

//! Bounded Rust declaration candidates over the pinned FMD lexer.
//!
//! FMD owns comment, literal and identifier boundaries. Delimiter pairing and
//! item-bearing scope traversal are iterative: sequential items do not spend
//! the nesting budget. Function bodies, field lists, initializers, attributes
//! and macro token trees remain opaque. Conditional attributes are not
//! evaluated, and every result retains the heuristic candidate evidence label.

use franken_markdown::highlight::{Span, Tok, highlight_into};
use super::source_map::SourceMap;

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

#[derive(Clone, Copy)]
struct Name { start: usize, end: usize, next: usize }

#[derive(Clone, Copy)]
enum Context { Items, Impl, Trait, Foreign }

#[derive(Clone, Copy)]
struct Scope { cursor: usize, end: usize, parent: Option<u64>, depth: usize, context: Context }

struct Declaration {
    keyword: usize,
    name: Option<Name>,
    kind: OutlineItemKind,
    end: usize,
}

struct Item {
    declaration: Option<Declaration>,
    next: usize,
    body: Option<(usize, usize, Context)>,
}

struct Tokens<'text> {
    text: &'text str,
    spans: Vec<Span>,
    closes: Vec<usize>,
}

impl<'text> Tokens<'text> {
    fn new(text: &'text str, canceled: &mut impl FnMut() -> bool) -> Result<Self, SymbolError> {
        let mut spans = reserved(text.len().max(1))?;
        checkpoint(canceled)?;
        highlight_into("rust", text, &mut spans);
        checkpoint(canceled)?;
        if spans.capacity() > text.len().max(1) { return Err(SymbolError::ResourceDenied); }
        let mut previous = 0;
        for span in &spans {
            checkpoint(canceled)?;
            let Some(word) = text.get(span.start..span.end) else { return Err(SymbolError::InvalidEvidence); };
            if span.start != previous || span.start >= span.end { return Err(SymbolError::InvalidEvidence); }
            if !crate::imports::qualified_rust_span(span.kind, word, text.as_bytes().get(span.end).copied()) {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
            previous = span.end;
        }
        if previous != text.len() { return Err(SymbolError::InvalidEvidence); }
        spans.retain(|span| span.kind != Tok::Comment
            && !text[span.start..span.end].chars().all(char::is_whitespace));
        let mut closes = reserved(spans.len())?;
        closes.resize(spans.len(), UNPAIRED);
        let mut stack = reserved(MAX_RECURSIVE_SCOPES)?;
        for (i, span) in spans.iter().enumerate() {
            checkpoint(canceled)?;
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
        let word = self.word(i);
        let raw = word.starts_with("r#");
        if !matches!(span.kind, Tok::Plain | Tok::Type | Tok::Func) && !raw { return None; }
        let mut chars = word.strip_prefix("r#").unwrap_or(word).chars();
        if !chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
            || !chars.all(|c| c.is_alphanumeric() || c == '_') { return None; }
        // Never publish a partial name if the upstream lexer split an
        // unsupported identifier spelling into adjacent terminals.
        if self.spans.get(i + 1).is_some_and(|next| next.start == span.end
            && (self.word(i + 1) == "#" || !self.word(i + 1).is_ascii()
                || self.word(i + 1).chars().next().is_some_and(|c| c.is_alphanumeric() || c == '_'))) {
            return None;
        }
        Some(Name { start: span.start, end: span.end, next: i + 1 })
    }

    fn prefix(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        while i < end {
            checkpoint(canceled)?;
            match self.word(i) {
                "#" => {
                    let bracket = i + 1 + usize::from(self.word(i + 1) == "!");
                    if bracket >= end || self.word(bracket) != "[" { break; }
                    i = self.after_group(bracket);
                }
                "pub" => {
                    i += 1;
                    if i < end && self.word(i) == "(" { i = self.after_group(i); }
                }
                "async" | "unsafe" | "default" | "auto" | "safe" => i += 1,
                "const" if matches!(self.word(i + 1), "fn" | "unsafe" | "async" | "extern") => i += 1,
                "extern" => {
                    let mut next = i + 1;
                    if self.spans.get(next).is_some_and(|span| span.kind == Tok::Str) { next += 1; }
                    if self.word(next) != "fn" { break; }
                    i = next;
                }
                _ => break,
            }
        }
        Ok(i)
    }

    fn arrow_end(&self, i: usize) -> bool {
        i > 0 && self.word(i - 1) == "-" && self.spans[i - 1].end == self.spans[i].start
    }

    fn generic_end(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        if self.word(i) != "<" { return Ok(Some(i)); }
        let mut depth = 0usize;
        while i < end {
            checkpoint(canceled)?;
            match self.word(i) {
                "<" => {
                    depth += 1;
                    if depth > MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
                }
                ">" if !self.arrow_end(i) => {
                    depth = depth.checked_sub(1).ok_or(SymbolError::InvalidEvidence)?;
                    if depth == 0 { return Ok(Some(i + 1)); }
                }
                ";" => return Ok(None),
                _ => {}
            }
            i = self.after_group(i);
        }
        Ok(None)
    }

    fn terminator(&self, mut i: usize, end: usize, body: bool, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        while i < end {
            checkpoint(canceled)?;
            match self.word(i) {
                ";" => return Ok(Some(i)),
                "{" if body => return Ok(Some(i)),
                // A missing terminator must not borrow the next item's body.
                // Bare function types have `fn (` rather than a declared name.
                "fn" if body && self.word(i + 1) != "(" => return Ok(None),
                "pub" | "mod" | "struct" | "enum" | "trait" | "type" | "static" if body => return Ok(None),
                "<" if body => {
                    let Some(next) = self.generic_end(i, end, canceled)? else { return Ok(None); };
                    i = next;
                }
                _ => i = self.after_group(i),
            }
        }
        Ok(None)
    }

    fn skip_item(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == ";" { return Ok(i + 1); }
            if self.word(i) == "{" { return Ok(self.after_group(i)); }
            i = self.after_group(i);
        }
        Ok(end)
    }

    fn item(&self, scope: Scope, canceled: &mut impl FnMut() -> bool) -> Result<Item, SymbolError> {
        let keyword = self.prefix(scope.cursor, scope.end, canceled)?;
        let word = self.word(keyword);
        if word == "extern" {
            let mut body = keyword + 1;
            if self.spans.get(body).is_some_and(|span| span.kind == Tok::Str) { body += 1; }
            if body < scope.end && self.word(body) == "{" {
                return Ok(Item { declaration: None, next: self.after_group(body),
                    body: Some((body + 1, self.closes[body], Context::Foreign)) });
            }
        }
        let kind = match word {
            "fn" if matches!(scope.context, Context::Impl | Context::Trait) => Some(OutlineItemKind::Method),
            "fn" => Some(OutlineItemKind::Function),
            "mod" => Some(OutlineItemKind::Module),
            "struct" | "union" => Some(OutlineItemKind::Struct),
            "enum" => Some(OutlineItemKind::Enum),
            "trait" => Some(OutlineItemKind::Trait),
            "impl" => Some(OutlineItemKind::Impl),
            "type" => Some(OutlineItemKind::TypeAlias),
            "const" | "static" => Some(OutlineItemKind::Const),
            _ => None,
        };
        if let Some(kind) = kind {
            let name_index = keyword + 1 + usize::from(word == "static" && self.word(keyword + 1) == "mut");
            let name = if word == "impl" { None } else { self.name(name_index) };
            if word == "impl" || name.is_some() {
                let mut tail = name.map_or(keyword + 1, |name| name.next);
                if word == "fn" {
                    let Some(next) = self.generic_end(tail, scope.end, canceled)? else {
                        return Ok(Item { declaration: None, next: self.skip_item(scope.cursor, scope.end, canceled)?, body: None });
                    };
                    tail = next;
                    if tail >= scope.end || self.word(tail) != "(" {
                        return Ok(Item { declaration: None, next: self.skip_item(scope.cursor, scope.end, canceled)?, body: None });
                    }
                    tail = self.after_group(tail);
                }
                let has_body = !matches!(word, "type" | "const" | "static");
                if let Some(end) = self.terminator(tail, scope.end, has_body, canceled)? {
                    if word != "impl" || (end > keyword + 1 && self.word(end) == "{") {
                        let body_context = match word {
                            "mod" => Some(Context::Items), "impl" => Some(Context::Impl),
                            "trait" => Some(Context::Trait), _ => None,
                        };
                        let body = body_context.filter(|_| self.word(end) == "{")
                            .map(|context| (end + 1, self.closes[end], context));
                        return Ok(Item { declaration: Some(Declaration { keyword, name, kind, end }),
                            next: self.after_group(end), body });
                    }
                }
            }
        }
        Ok(Item { declaration: None, next: self.skip_item(scope.cursor, scope.end, canceled)?, body: None })
    }

    fn display_name(&self, declaration: &Declaration, map: &SourceMap<'_>, canceled: &mut impl FnMut() -> bool)
        -> Result<String, SymbolError> {
        if let Some(name) = declaration.name {
            return map.copy_name(&self.text[name.start..name.end]);
        }
        // Impl blocks have a synthesized label, so deliberately no exact name
        // evidence. Their whole declaration still has an exact original range.
        let capacity = (declaration.keyword..declaration.end).try_fold(0usize, |size, i|
            size.checked_add(self.word(i).len() + 1).ok_or(SymbolError::ResourceDenied))?;
        let mut value = map.name_buffer(capacity)?;
        for i in declaration.keyword..declaration.end {
            checkpoint(canceled)?;
            if !value.is_empty() { value.push(' '); }
            value.push_str(self.word(i));
        }
        Ok(value)
    }
}

pub(super) fn extract(map: &SourceMap<'_>, line_starts: &[usize], max_items: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(Vec<SymbolCandidate>, bool), SymbolError> {
    let tokens = Tokens::new(map.decoded_text(), canceled)?;
    let mut out = reserved(max_items)?;
    let mut scopes = reserved(MAX_RECURSIVE_SCOPES + 1)?;
    scopes.push(Scope { cursor: 0, end: tokens.spans.len(), parent: None, depth: 0, context: Context::Items });
    while let Some(scope) = scopes.last().copied() {
        checkpoint(canceled)?;
        if scope.cursor == scope.end { scopes.pop(); continue; }
        if scope.cursor > scope.end { return Err(SymbolError::InvalidEvidence); }
        let item = tokens.item(scope, canceled)?;
        if item.next <= scope.cursor || item.next > scope.end { return Err(SymbolError::InvalidEvidence); }
        scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = item.next;
        let mut parent = scope.parent;
        let mut depth = scope.depth;
        if let Some(declaration) = item.declaration {
            if out.len() == max_items { return Ok((out, true)); }
            let start = tokens.spans[declaration.keyword].start;
            let end = tokens.spans[tokens.after_group(declaration.end) - 1].end;
            let name_range = declaration.name.map(|name| mapped(map, name.start as u64, name.end as u64)).transpose()?;
            let name = tokens.display_name(&declaration, map, canceled)?;
            let id = out.len() as u64 + 1;
            out.push(SymbolCandidate { id, parent: scope.parent, depth: scope.depth, name,
                kind: declaration.kind, range: mapped(map, start as u64, end as u64)?, name_range,
                line: line_starts.partition_point(|&offset| offset <= start) as u64 });
            parent = Some(id);
            depth += 1;
        }
        if let Some((cursor, end, context)) = item.body {
            if scopes.len() == MAX_RECURSIVE_SCOPES + 1 { return Err(SymbolError::ComplexityLimit); }
            scopes.push(Scope { cursor, end, parent, depth, context });
        }
    }
    checkpoint(canceled)?;
    Ok((out, false))
}
