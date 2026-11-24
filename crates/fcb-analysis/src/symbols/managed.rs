#![forbid(unsafe_code)]

//! Java and C# declaration candidates over the pinned FMD language lexers.
//!
//! FMD owns token, comment, string, text-block and interpolation boundaries.
//! This bounded source-specific grammar recognizes type/member signatures;
//! it is not a compiler, name resolver or a second lexer. Method bodies,
//! initializers, enum constants and local declarations remain opaque. C#
//! conditional directives are not evaluated: candidates can belong to either
//! branch, and the common heuristic evidence label remains mandatory.
//!
//! Unsupported pre-lexical Java Unicode escapes and C# escaped identifiers
//! are refused explicitly, rather than reinterpreting upstream terminals.
//! All delimiters and source/name ranges are checked before publication.

use franken_markdown::highlight::{Span, Tok, highlight_into};
use super::source_map::SourceMap;

use super::{MAX_RECURSIVE_SCOPES, SymbolCandidate, SymbolError, SymbolLanguage, mapped};
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
struct Name { start: usize, end: usize }

#[derive(Clone, Copy)]
enum Context {
    Root,
    Namespace,
    Type { name: Name, record: bool, annotation: bool },
}

#[derive(Clone, Copy)]
struct Scope { cursor: usize, end: usize, parent: Option<u64>, context: Context }

struct Declaration {
    name: Name,
    kind: OutlineItemKind,
    end: usize,
    next: usize,
    body: Option<(usize, usize, Context)>,
}

struct Tokens<'text> {
    language: SymbolLanguage,
    text: &'text str,
    spans: Vec<Span>,
    closes: Vec<usize>,
}

impl<'text> Tokens<'text> {
    fn new(text: &'text str, language: SymbolLanguage, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, SymbolError> {
        if !matches!(language, SymbolLanguage::Java | SymbolLanguage::CSharp) {
            return Err(SymbolError::InvalidEvidence);
        }
        // The pinned lexer does not perform Java's pre-token Unicode escape
        // translation. Even escapes inside apparent comments can change tokens.
        // Its line-comment boundary is LF, so refuse unsupported terminators.
        if (language == SymbolLanguage::Java && text.contains("\\u"))
            || text.as_bytes().iter().enumerate().any(|(i, &b)| b == b'\r'
                && text.as_bytes().get(i + 1) != Some(&b'\n'))
            || (language == SymbolLanguage::CSharp && text.chars().any(|c| matches!(c, '\u{85}' | '\u{2028}' | '\u{2029}'))) {
            return Err(SymbolError::UnsupportedLexicalContext);
        }
        let mut spans = reserved(text.len().max(1))?;
        checkpoint(canceled)?;
        highlight_into(language.name(), text, &mut spans);
        checkpoint(canceled)?;
        if spans.capacity() > text.len().max(1) { return Err(SymbolError::ResourceDenied); }
        let mut previous = 0;
        for span in &spans {
            checkpoint(canceled)?;
            if span.start != previous || span.start >= span.end || text.get(span.start..span.end).is_none() {
                return Err(SymbolError::InvalidEvidence);
            }
            // C# string escapes are owned upstream and remain opaque. Only
            // identifier escapes outside strings/comments need a refusal.
            if language == SymbolLanguage::CSharp && !matches!(span.kind, Tok::Comment | Tok::Str)
                && (text[span.start..].starts_with("\\u") || text[span.start..].starts_with("\\U")) {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
            previous = span.end;
        }
        if previous != text.len() { return Err(SymbolError::InvalidEvidence); }
        let mut directive_end = 0;
        spans.retain(|span| {
            let word = &text[span.start..span.end];
            if language == SymbolLanguage::CSharp && span.kind == Tok::Keyword && word.starts_with('#') {
                directive_end = text[span.start..].find('\n').map_or(text.len(), |offset| span.start + offset);
            }
            span.start >= directive_end && span.kind != Tok::Comment && !word.chars().all(char::is_whitespace)
        });
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
        Ok(Self { language, text, spans, closes })
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
        let contextual = match self.language {
            SymbolLanguage::Java => matches!(word, "record" | "sealed" | "permits" | "var" | "yield"),
            SymbolLanguage::CSharp => matches!(word, "async" | "await" | "dynamic" | "get" | "set" | "init"
                | "partial" | "var" | "when" | "where" | "yield"),
            _ => false,
        };
        if !matches!(span.kind, Tok::Plain | Tok::Type | Tok::Func) && !contextual { return None; }
        let (start, name) = if self.language == SymbolLanguage::CSharp {
            word.strip_prefix('@').map_or((span.start, word), |name| (span.start + 1, name))
        } else { (span.start, word) };
        let mut chars = name.chars();
        let dollar = self.language == SymbolLanguage::Java;
        if !chars.next().is_some_and(|c| c.is_alphabetic() || c == '_' || (dollar && c == '$'))
            || !chars.all(|c| c.is_alphanumeric() || c == '_' || (dollar && c == '$')) {
            return None;
        }
        Some(Name { start, end: span.end })
    }

    fn same_name(&self, a: Name, b: Name) -> bool {
        self.text[a.start..a.end] == self.text[b.start..b.end]
    }

    fn modifier(&self, i: usize) -> bool {
        match self.language {
            SymbolLanguage::Java => matches!(self.word(i), "public" | "protected" | "private" | "abstract"
                | "static" | "final" | "native" | "synchronized" | "transient" | "volatile" | "strictfp"
                | "default" | "sealed"),
            SymbolLanguage::CSharp => matches!(self.word(i), "public" | "protected" | "private" | "internal"
                | "abstract" | "static" | "sealed" | "virtual" | "override" | "new" | "extern" | "unsafe"
                | "readonly" | "partial" | "async" | "required" | "file" | "ref" | "scoped"),
            _ => false,
        }
    }

    fn prefix(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        while i < end {
            checkpoint(canceled)?;
            if self.modifier(i) { i += 1; continue; }
            if self.language == SymbolLanguage::Java {
                if self.word(i) == "non" && self.word(i + 1) == "-" && self.word(i + 2) == "sealed" {
                    i += 3; continue;
                }
                if self.word(i) == "@" && self.word(i + 1) == "interface" { return Ok(i + 1); }
                if self.word(i).starts_with('@') {
                    i += 1;
                    if self.word(i - 1) == "@" && self.name(i).is_some() { i += 1; }
                    while i + 1 < end && self.word(i) == "." && self.name(i + 1).is_some() { i += 2; }
                    if i < end && self.word(i) == "(" { i = self.after_group(i); }
                    continue;
                }
            } else if self.word(i) == "[" { i = self.after_group(i); continue; }
            break;
        }
        Ok(i)
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
                ">" | ">>" | ">>>" => {
                    let Some(remaining) = depth.checked_sub(self.word(i).len()) else { return Ok(None); };
                    depth = remaining;
                    if depth == 0 { return Ok(Some(i + 1)); }
                }
                "{" | "}" | ";" | "=" | "=>" => return Ok(None),
                _ => {}
            }
            i = self.after_group(i);
        }
        Ok(None)
    }

    fn primitive(&self, i: usize) -> bool {
        match self.language {
            SymbolLanguage::Java => matches!(self.word(i), "boolean" | "byte" | "char" | "short" | "int"
                | "long" | "float" | "double" | "void"),
            SymbolLanguage::CSharp => matches!(self.word(i), "bool" | "byte" | "sbyte" | "char" | "decimal"
                | "double" | "float" | "int" | "uint" | "long" | "ulong" | "short" | "ushort"
                | "object" | "string" | "void"),
            _ => false,
        }
    }

    fn type_end(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        if i >= end { return Ok(None); }
        if self.language == SymbolLanguage::CSharp && self.word(i) == "(" {
            // Tuple return types are one already balanced upstream-token group.
            i = self.after_group(i);
        } else {
            if self.name(i).is_none() && !self.primitive(i) { return Ok(None); }
            i += 1;
            loop {
                let Some(next) = self.generic_end(i, end, canceled)? else { return Ok(None); };
                i = next;
                let separator = if self.word(i) == "." { 1 }
                    else if self.language == SymbolLanguage::CSharp && self.word(i) == ":" && self.word(i + 1) == ":" { 2 }
                    else { 0 };
                if separator == 0 { break; }
                i += separator;
                if i >= end || self.name(i).is_none() { return Ok(None); }
                i += 1;
            }
        }
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == "[" {
                let close = self.closes[i];
                if (i + 1..close).any(|j| self.word(j) != ",") { return Ok(None); }
                if self.language == SymbolLanguage::Java && close != i + 1 { return Ok(None); }
                i = close + 1;
            } else if self.language == SymbolLanguage::CSharp && matches!(self.word(i), "?" | "*") { i += 1; }
            else { break; }
        }
        Ok(Some(i))
    }

    fn constraints_end(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        while i < end && self.word(i) == "where" {
            checkpoint(canceled)?;
            if self.name(i + 1).is_none() || self.word(i + 2) != ":" { return Ok(None); }
            i += 3;
            loop {
                checkpoint(canceled)?;
                if matches!(self.word(i), "class" | "struct" | "default") {
                    i += 1;
                    if self.word(i) == "?" { i += 1; }
                } else if self.word(i) == "new" && self.word(i + 1) == "(" && self.closes[i + 1] == i + 2 { i += 3; }
                else {
                    let Some(next) = self.type_end(i, end, canceled)? else { return Ok(None); };
                    i = next;
                }
                if self.word(i) != "," { break; }
                i += 1;
            }
        }
        Ok(Some(i))
    }

    fn type_declaration(&self, mut i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let keyword = self.word(i);
        let record = keyword == "record";
        let mut kind = match keyword {
            "class" | "record" => OutlineItemKind::Class,
            "interface" => OutlineItemKind::Interface,
            "enum" => OutlineItemKind::Enum,
            "struct" if self.language == SymbolLanguage::CSharp => OutlineItemKind::Struct,
            _ => return Ok(None),
        };
        let annotation = self.language == SymbolLanguage::Java && keyword == "interface" && i > 0 && self.word(i - 1) == "@";
        i += 1;
        if record && self.language == SymbolLanguage::CSharp && matches!(self.word(i), "class" | "struct") {
            if self.word(i) == "struct" { kind = OutlineItemKind::Struct; }
            i += 1;
        }
        let Some(name) = self.name(i) else { return Ok(None); };
        let Some(next) = self.generic_end(i + 1, scope.end, canceled)? else { return Ok(None); };
        i = next;
        if self.word(i) == "(" && (record || self.language == SymbolLanguage::CSharp) { i = self.after_group(i); }
        else if record && self.language == SymbolLanguage::Java { return Ok(None); }
        while i < scope.end {
            checkpoint(canceled)?;
            let clause = self.word(i);
            if self.language == SymbolLanguage::Java && matches!(clause, "extends" | "implements" | "permits")
                || self.language == SymbolLanguage::CSharp && clause == ":" {
                i += 1;
                loop {
                    let Some(next) = self.type_end(i, scope.end, canceled)? else { return Ok(None); };
                    i = next;
                    if self.language == SymbolLanguage::CSharp && self.word(i) == "(" { i = self.after_group(i); }
                    if self.word(i) != "," { break; }
                    i += 1;
                }
            } else { break; }
        }
        if self.language == SymbolLanguage::CSharp {
            let Some(next) = self.constraints_end(i, scope.end, canceled)? else { return Ok(None); };
            i = next;
        }
        if self.word(i) == ";" && record && self.language == SymbolLanguage::CSharp {
            return Ok(Some(Declaration { name, kind, end: self.spans[i].end, next: i + 1, body: None }));
        }
        if i >= scope.end || self.word(i) != "{" { return Ok(None); }
        let close = self.closes[i];
        let mut start = i + 1;
        if keyword == "enum" {
            // Java enum constant bodies are anonymous classes, not named type
            // members. Only declarations following the separating ';' qualify.
            start = close;
            if self.language == SymbolLanguage::Java {
                let mut cursor = i + 1;
                while cursor < close {
                    checkpoint(canceled)?;
                    if self.word(cursor) == ";" { start = cursor + 1; break; }
                    cursor = self.after_group(cursor);
                }
            }
        }
        Ok(Some(Declaration { name, kind, end: self.spans[close].end, next: close + 1,
            body: Some((start, close, Context::Type { name, record, annotation })) }))
    }

    fn namespace(&self, i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        if self.language != SymbolLanguage::CSharp || self.word(i) != "namespace"
            || matches!(scope.context, Context::Type { .. }) { return Ok(None); }
        let Some(mut name) = self.name(i + 1) else { return Ok(None); };
        let mut next = i + 2;
        while next + 1 < scope.end && self.word(next) == "." {
            checkpoint(canceled)?;
            let Some(part) = self.name(next + 1) else { return Ok(None); };
            name.end = part.end;
            next += 2;
        }
        let (start, end, finish) = match self.word(next) {
            "{" => (next + 1, self.closes[next], self.closes[next] + 1),
            ";" if matches!(scope.context, Context::Root) => (next + 1, scope.end, scope.end),
            _ => return Ok(None),
        };
        Ok(Some(Declaration { name, kind: OutlineItemKind::Module,
            end: self.spans[finish - 1].end, next: finish, body: Some((start, end, Context::Namespace)) }))
    }

    fn arrow(&self, i: usize) -> bool {
        self.language == SymbolLanguage::CSharp && self.word(i) == "=" && self.word(i + 1) == ">"
            && self.spans[i].end == self.spans[i + 1].start
    }

    fn expression_end(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == ";" { return Ok(Some(i + 1)); }
            i = self.after_group(i);
        }
        Ok(None)
    }

    fn method_tail(&self, mut i: usize, scope: Scope, constructor: bool, annotation: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<usize>, SymbolError> {
        if self.language == SymbolLanguage::Java {
            while self.word(i) == "[" && self.closes[i] == i + 1 { i += 2; }
            if self.word(i) == "throws" {
                i += 1;
                loop {
                    let Some(next) = self.type_end(i, scope.end, canceled)? else { return Ok(None); };
                    i = next;
                    if self.word(i) != "," { break; }
                    i += 1;
                }
            }
            if annotation && self.word(i) == "default" { return self.expression_end(i + 1, scope.end, canceled); }
        } else {
            if constructor && self.word(i) == ":" {
                if !matches!(self.word(i + 1), "base" | "this") || self.word(i + 2) != "(" { return Ok(None); }
                i = self.after_group(i + 2);
            }
            let Some(next) = self.constraints_end(i, scope.end, canceled)? else { return Ok(None); };
            i = next;
            if self.arrow(i) { return self.expression_end(i + 2, scope.end, canceled); }
        }
        Ok(match self.word(i) { "{" => Some(self.after_group(i)), ";" => Some(i + 1), _ => None })
    }

    fn accessors(&self, open: usize, canceled: &mut impl FnMut() -> bool) -> Result<bool, SymbolError> {
        let end = self.closes[open];
        let mut i = open + 1;
        let mut count = 0;
        while i < end {
            checkpoint(canceled)?;
            i = self.prefix(i, end, canceled)?;
            if !matches!(self.word(i), "get" | "set" | "init") { return Ok(false); }
            i += 1;
            if self.word(i) == ";" { i += 1; }
            else if self.word(i) == "{" { i = self.after_group(i); }
            else if self.arrow(i) {
                let Some(next) = self.expression_end(i + 2, end, canceled)? else { return Ok(false); };
                i = next;
            } else { return Ok(false); }
            count += 1;
        }
        Ok(count > 0)
    }

    fn member(&self, mut i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let Context::Type { name: type_name, record, annotation } = scope.context else { return Ok(None); };
        if self.language == SymbolLanguage::Java {
            let Some(next) = self.generic_end(i, scope.end, canceled)? else { return Ok(None); };
            i = next;
        }
        let direct = self.name(i);
        let constructor = direct.is_some_and(|name| self.same_name(name, type_name))
            && (self.word(i + 1) == "(" || (record && self.language == SymbolLanguage::Java && self.word(i + 1) == "{"));
        let (name, mut next) = if constructor { (direct.ok_or(SymbolError::InvalidEvidence)?, i + 1) }
        else {
            let Some(next) = self.type_end(i, scope.end, canceled)? else { return Ok(None); };
            let Some(name) = self.name(next) else { return Ok(None); };
            (name, next + 1)
        };
        let mut name = name;
        if self.language == SymbolLanguage::CSharp && !constructor {
            // Explicit interface implementation names still select the exact
            // terminal member spelling; this is not a resolved interface link.
            loop {
                let Some(after_generic) = self.generic_end(next, scope.end, canceled)? else { return Ok(None); };
                next = after_generic;
                if self.word(next) != "." { break; }
                let Some(part) = self.name(next + 1) else { return Ok(None); };
                name = part;
                next += 2;
            }
        }
        if constructor && record && self.language == SymbolLanguage::Java && self.word(next) == "{" {
            let finish = self.after_group(next);
            return Ok(Some(Declaration { name, kind: OutlineItemKind::Initializer,
                end: self.spans[finish - 1].end, next: finish, body: None }));
        }
        if self.word(next) == "(" {
            let Some(finish) = self.method_tail(self.after_group(next), scope, constructor, annotation, canceled)? else { return Ok(None); };
            return Ok(Some(Declaration { name, kind: if constructor { OutlineItemKind::Initializer } else { OutlineItemKind::Method },
                end: self.spans[finish - 1].end, next: finish, body: None }));
        }
        if self.language == SymbolLanguage::CSharp && !constructor {
            let finish = if self.arrow(next) { self.expression_end(next + 2, scope.end, canceled)? }
                else if self.word(next) == "{" && self.accessors(next, canceled)? {
                    let after = self.after_group(next);
                    if self.word(after) == "=" { self.expression_end(after + 1, scope.end, canceled)? } else { Some(after) }
                } else { None };
            if let Some(finish) = finish {
                return Ok(Some(Declaration { name, kind: OutlineItemKind::Property,
                    end: self.spans[finish - 1].end, next: finish, body: None }));
            }
        }
        Ok(None)
    }

    fn declaration(&self, start: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let i = self.prefix(start, scope.end, canceled)?;
        if i >= scope.end { return Ok(None); }
        if let Some(value) = self.namespace(i, scope, canceled)? { return Ok(Some(value)); }
        if let Some(value) = self.type_declaration(i, scope, canceled)? { return Ok(Some(value)); }
        self.member(i, scope, canceled)
    }

    fn skip_statement(&self, mut i: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == ";" { return Ok(i + 1); }
            if self.word(i) == "{" { return Ok(self.after_group(i)); }
            i = self.after_group(i);
        }
        Ok(end)
    }
}

pub(super) fn extract(map: &SourceMap<'_>, language: SymbolLanguage, line_starts: &[usize], limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(Vec<SymbolCandidate>, bool), SymbolError> {
    let tokens = Tokens::new(map.decoded_text(), language, canceled)?;
    let mut candidates = reserved(limit)?;
    let mut scopes = reserved(MAX_RECURSIVE_SCOPES + 1)?;
    scopes.push(Scope { cursor: 0, end: tokens.spans.len(), parent: None, context: Context::Root });
    while let Some(scope) = scopes.last().copied() {
        checkpoint(canceled)?;
        let i = scope.cursor;
        if i >= scope.end { scopes.pop(); continue; }
        let next = if let Some(declaration) = tokens.declaration(i, scope, canceled)? {
            if candidates.len() == limit { return Ok((candidates, true)); }
            let name = declaration.name;
            let start = tokens.spans[i].start;
            if name.start < start || name.end > declaration.end || name.start >= name.end {
                return Err(SymbolError::InvalidEvidence);
            }
            if name.end - name.start > 4096 { return Err(SymbolError::ComplexityLimit); }
            let id = candidates.len() as u64 + 1;
            candidates.push(SymbolCandidate { id, parent: scope.parent, depth: scopes.len() - 1,
                name: map.copy_name(&tokens.text[name.start..name.end])?, kind: declaration.kind,
                range: mapped(map, start as u64, declaration.end as u64)?,
                name_range: Some(mapped(map, name.start as u64, name.end as u64)?),
                line: line_starts.partition_point(|&line| line <= start) as u64 });
            if let Some((cursor, end, context)) = declaration.body {
                if scopes.len() == MAX_RECURSIVE_SCOPES + 1 { return Err(SymbolError::ComplexityLimit); }
                scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = declaration.next;
                scopes.push(Scope { cursor, end, parent: Some(id), context });
                continue;
            }
            declaration.next
        } else { tokens.skip_statement(i, scope.end, canceled)? };
        if next <= i || next > scope.end { return Err(SymbolError::InvalidEvidence); }
        scopes.last_mut().ok_or(SymbolError::InvalidEvidence)?.cursor = next;
    }
    Ok((candidates, false))
}
