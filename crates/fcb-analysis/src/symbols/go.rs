#![forbid(unsafe_code)]

//! Go declaration navigation over the pinned FrankenMarkdown lexer.
//!
//! Upstream owns comments, raw/interpreted strings, runes and identifiers. This
//! adapter recognizes package-level functions/methods, type declarations and
//! const/var bindings, including grouped declarations. Function bodies, type
//! members and initializer expressions remain opaque. Receiver names are not
//! resolved to types, and candidates never claim compiler binding/completeness.
//! Delimiter traversal and declaration walking are iterative and bounded.

use fcb_source::CaptureEncodingMap;
use franken_markdown::highlight::{Span, Tok, highlight_into};

use super::{MAX_RECURSIVE_SCOPES, SymbolCandidate, SymbolError, mapped};
use crate::OutlineItemKind;

const UNPAIRED: usize = usize::MAX;

fn reserved<T>(capacity: usize) -> Result<Vec<T>, SymbolError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| SymbolError::ResourceDenied)?;
    if values.capacity() > capacity {
        return Err(SymbolError::ResourceDenied);
    }
    Ok(values)
}

fn checkpoint(canceled: &mut impl FnMut() -> bool) -> Result<(), SymbolError> {
    if canceled() {
        Err(SymbolError::Canceled)
    } else {
        Ok(())
    }
}

struct Tokens<'text> {
    text: &'text str,
    spans: Vec<Span>,
    closes: Vec<usize>,
}

#[derive(Clone, Copy)]
struct End {
    /// Exclusive token limit, without an explicit semicolon.
    tokens: usize,
    /// Exclusive source-byte limit, including an explicit semicolon.
    byte: usize,
    next: usize,
}

impl<'text> Tokens<'text> {
    fn new(text: &'text str, canceled: &mut impl FnMut() -> bool) -> Result<Self, SymbolError> {
        let capacity = text.len().max(1);
        let mut spans = reserved(capacity)?;
        checkpoint(canceled)?;
        highlight_into("go", text, &mut spans);
        checkpoint(canceled)?;
        if spans.capacity() > capacity {
            return Err(SymbolError::ResourceDenied);
        }
        let mut previous = 0;
        for span in &spans {
            checkpoint(canceled)?;
            let Some(word) = text.get(span.start..span.end) else {
                return Err(SymbolError::InvalidEvidence);
            };
            if span.start != previous || span.start >= span.end {
                return Err(SymbolError::InvalidEvidence);
            }
            // Validate closure of upstream-classified opaque tokens. In
            // particular, a broken interpreted string stops at LF upstream;
            // its apparent following declarations must not become candidates.
            if span.kind == Tok::Comment && word.starts_with("/*") && !word.ends_with("*/") {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
            if span.kind == Tok::Str {
                let quote = word.as_bytes()[0];
                if !matches!(quote, b'`' | b'\'' | b'"')
                    || word.len() < 2
                    || word.as_bytes().last() != Some(&quote)
                    || (quote != b'`'
                        && word.as_bytes()[..word.len() - 1]
                            .iter()
                            .rev()
                            .take_while(|&&byte| byte == b'\\')
                            .count()
                            % 2
                            != 0)
                {
                    return Err(SymbolError::UnsupportedLexicalContext);
                }
            }
            if span.kind == Tok::Plain
                && word.chars().all(char::is_whitespace)
                && !word
                    .bytes()
                    .all(|byte| matches!(byte, b' ' | b'\t' | b'\r' | b'\n'))
            {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
            previous = span.end;
        }
        if previous != text.len() {
            return Err(SymbolError::InvalidEvidence);
        }
        // Strings are opaque terminals, not removed text which could join two
        // unrelated pieces of syntax. Comments remain in the gaps for newline
        // (semicolon insertion) decisions, even after their tokens are removed.
        spans.retain(|span| {
            span.kind != Tok::Comment
                && !text[span.start..span.end].chars().all(char::is_whitespace)
        });
        let mut closes = reserved(spans.len())?;
        closes.resize(spans.len(), UNPAIRED);
        let mut stack = reserved(MAX_RECURSIVE_SCOPES)?;
        for (i, span) in spans.iter().enumerate() {
            checkpoint(canceled)?;
            if span.kind != Tok::Punct {
                continue;
            }
            let word = &text[span.start..span.end];
            match word {
                "(" | "[" | "{" => {
                    if stack.len() == MAX_RECURSIVE_SCOPES {
                        return Err(SymbolError::ComplexityLimit);
                    }
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
        if !stack.is_empty() {
            return Err(SymbolError::InvalidEvidence);
        }
        Ok(Self {
            text,
            spans,
            closes,
        })
    }

    fn word(&self, i: usize) -> &'text str {
        self.spans
            .get(i)
            .map_or("", |span| &self.text[span.start..span.end])
    }

    fn after_group(&self, i: usize) -> usize {
        if self.closes[i] == UNPAIRED {
            i + 1
        } else {
            self.closes[i] + 1
        }
    }

    fn identifier(&self, i: usize) -> bool {
        let Some(span) = self.spans.get(i) else {
            return false;
        };
        if !matches!(span.kind, Tok::Plain | Tok::Type | Tok::Func) {
            return false;
        }
        let mut chars = self.word(i).chars();
        if !chars.next().is_some_and(|c| c.is_alphabetic() || c == '_')
            || !chars.all(|c| c.is_alphanumeric() || c == '_')
        {
            return false;
        }
        // Never offer a partial spelling when the pinned owner split an
        // unsupported identifier into adjacent terminals.
        !self.spans.get(i + 1).is_some_and(|next| {
            next.start == span.end
                && (!self.word(i + 1).is_ascii()
                    || self
                        .word(i + 1)
                        .chars()
                        .next()
                        .is_some_and(|c| c.is_alphanumeric() || c == '_'))
        })
    }

    fn can_end_statement(&self, i: usize) -> bool {
        self.identifier(i)
            || matches!(self.spans[i].kind, Tok::Number | Tok::Str)
            || matches!(
                self.word(i),
                ")" | "]" | "}" | "break" | "continue" | "fallthrough" | "return"
            )
            || (i > 0
                && matches!(self.word(i), "+" | "-")
                && self.word(i - 1) == self.word(i)
                && self.spans[i - 1].end == self.spans[i].start)
    }

    fn boundary(&self, previous: usize, next: usize) -> bool {
        self.can_end_statement(previous)
            && self.text[self.spans[previous].end..self.spans[next].start].contains('\n')
    }

    fn end(
        &self,
        start: usize,
        end: usize,
        canceled: &mut impl FnMut() -> bool,
    ) -> Result<End, SymbolError> {
        let mut i = start;
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == ";" {
                return Ok(End {
                    tokens: i,
                    byte: self.spans[i].end,
                    next: i + 1,
                });
            }
            let next = self.after_group(i);
            if next >= end || self.boundary(next - 1, next) {
                return Ok(End {
                    tokens: next,
                    byte: self.spans[next - 1].end,
                    next,
                });
            }
            i = next;
        }
        Err(SymbolError::InvalidEvidence)
    }

    fn declaration_keyword(&self, i: usize) -> bool {
        self.spans
            .get(i)
            .is_some_and(|span| span.kind == Tok::Keyword)
            && matches!(
                self.word(i),
                "package" | "import" | "func" | "type" | "var" | "const"
            )
    }

    fn function(
        &self,
        start: usize,
        end: usize,
        canceled: &mut impl FnMut() -> bool,
    ) -> Result<Option<(usize, OutlineItemKind, End)>, SymbolError> {
        let mut name = start + 1;
        let method = self.word(name) == "(";
        if method {
            if self.closes[name] == name + 1 {
                return Ok(None);
            }
            name = self.after_group(name);
        }
        if !self.identifier(name) || self.word(name) == "_" {
            return Ok(None);
        }
        let mut parameters = name + 1;
        if self.word(parameters) == "[" {
            parameters = self.after_group(parameters);
        }
        if parameters >= end || self.word(parameters) != "(" {
            return Ok(None);
        }
        let mut previous = self.closes[parameters];
        let mut i = previous + 1;
        while i < end {
            checkpoint(canceled)?;
            if self.word(i) == ";" {
                return Ok(Some((
                    name,
                    if method {
                        OutlineItemKind::Method
                    } else {
                        OutlineItemKind::Function
                    },
                    End {
                        tokens: i,
                        byte: self.spans[i].end,
                        next: i + 1,
                    },
                )));
            }
            if self.boundary(previous, i) {
                break;
            }
            // A malformed signature cannot borrow a later declaration's body.
            // An anonymous function result type (func (...)) is a valid type.
            if self.declaration_keyword(i) && !(self.word(i) == "func" && self.word(i + 1) == "(") {
                break;
            }
            if self.word(i) == "{" && !matches!(self.word(previous), "struct" | "interface") {
                let next = self.after_group(i);
                return Ok(Some((
                    name,
                    if method {
                        OutlineItemKind::Method
                    } else {
                        OutlineItemKind::Function
                    },
                    End {
                        tokens: next,
                        byte: self.spans[next - 1].end,
                        next,
                    },
                )));
            }
            // Anonymous result types, parameters, and generic arguments do not
            // introduce item-bearing scopes. The function body stays opaque.
            i = self.after_group(i);
            previous = i - 1;
        }
        Ok(Some((
            name,
            if method {
                OutlineItemKind::Method
            } else {
                OutlineItemKind::Function
            },
            End {
                tokens: i,
                byte: self.spans[previous].end,
                next: i,
            },
        )))
    }
}

struct Output<'a> {
    map: &'a CaptureEncodingMap,
    line_starts: &'a [usize],
    candidates: Vec<SymbolCandidate>,
    limit: usize,
    limited: bool,
}

impl Output<'_> {
    fn add(
        &mut self,
        tokens: &Tokens<'_>,
        name: usize,
        kind: OutlineItemKind,
        start: usize,
        end: usize,
    ) -> Result<(), SymbolError> {
        if tokens.word(name) == "_" {
            return Ok(());
        }
        if self.candidates.len() == self.limit {
            self.limited = true;
            return Ok(());
        }
        let span = tokens.spans[name];
        let mut spelling = String::new();
        spelling
            .try_reserve_exact(span.end - span.start)
            .map_err(|_| SymbolError::ResourceDenied)?;
        if spelling.capacity() > span.end - span.start {
            return Err(SymbolError::ResourceDenied);
        }
        spelling.push_str(tokens.word(name));
        self.candidates.push(SymbolCandidate {
            id: self.candidates.len() as u64 + 1,
            parent: None,
            depth: 0,
            name: spelling,
            kind,
            range: mapped(self.map, start as u64, end as u64)?,
            name_range: Some(mapped(self.map, span.start as u64, span.end as u64)?),
            line: self.line_starts.partition_point(|&line| line <= start) as u64,
        });
        Ok(())
    }

    fn spec(
        &mut self,
        tokens: &Tokens<'_>,
        keyword: &str,
        start: usize,
        end: End,
        evidence_start: usize,
        inherited_const: bool,
        canceled: &mut impl FnMut() -> bool,
    ) -> Result<bool, SymbolError> {
        if start >= end.tokens || !tokens.identifier(start) {
            return Ok(false);
        }
        if keyword == "type" {
            let mut body = start + 1;
            if body < end.tokens && tokens.word(body) == "[" {
                body = tokens.after_group(body);
            }
            if body >= end.tokens {
                return Ok(false);
            }
            let kind = match tokens.word(body) {
                "struct" if tokens.word(body + 1) == "{" => OutlineItemKind::Struct,
                "interface" if tokens.word(body + 1) == "{" => OutlineItemKind::Interface,
                "=" if body + 1 == end.tokens => return Ok(false),
                _ => OutlineItemKind::TypeAlias,
            };
            self.add(tokens, start, kind, evidence_start, end.byte)?;
            return Ok(false);
        }
        let mut after_names = start + 1;
        while after_names < end.tokens && tokens.word(after_names) == "," {
            checkpoint(canceled)?;
            if after_names + 1 >= end.tokens || !tokens.identifier(after_names + 1) {
                return Ok(false);
            }
            after_names += 2;
        }
        let mut has_value = false;
        let mut i = after_names;
        while i < end.tokens {
            checkpoint(canceled)?;
            if tokens.word(i) == "=" {
                has_value = i + 1 < end.tokens;
                break;
            }
            i = tokens.after_group(i);
        }
        if keyword == "const" && !has_value && !(inherited_const && after_names == end.tokens) {
            return Ok(false);
        }
        if keyword == "var"
            && (after_names == end.tokens || (tokens.word(after_names) == "=" && !has_value))
        {
            return Ok(false);
        }
        let mut name = start;
        while name < after_names {
            checkpoint(canceled)?;
            self.add(
                tokens,
                name,
                if keyword == "const" {
                    OutlineItemKind::Const
                } else {
                    OutlineItemKind::Variable
                },
                evidence_start,
                end.byte,
            )?;
            name += 2;
        }
        Ok(has_value || inherited_const)
    }
}

pub(super) fn extract(
    map: &CaptureEncodingMap,
    line_starts: &[usize],
    limit: usize,
    canceled: &mut impl FnMut() -> bool,
) -> Result<(Vec<SymbolCandidate>, bool), SymbolError> {
    let tokens = Tokens::new(map.decoded_text(), canceled)?;
    let mut output = Output {
        map,
        line_starts,
        candidates: reserved(limit)?,
        limit,
        limited: false,
    };
    let mut i = 0;
    while i < tokens.spans.len() {
        checkpoint(canceled)?;
        if tokens.word(i) == ";" {
            i += 1;
            continue;
        }
        let keyword = tokens.word(i);
        if tokens.spans[i].kind == Tok::Keyword && keyword == "func" {
            if let Some((name, kind, end)) = tokens.function(i, tokens.spans.len(), canceled)? {
                output.add(&tokens, name, kind, tokens.spans[i].start, end.byte)?;
                i = end.next;
                continue;
            }
        } else if tokens.spans[i].kind == Tok::Keyword
            && matches!(keyword, "type" | "const" | "var")
        {
            if tokens.word(i + 1) == "(" {
                let close = tokens.closes[i + 1];
                let mut spec = i + 2;
                let mut inherited_const = false;
                while spec < close {
                    checkpoint(canceled)?;
                    if tokens.word(spec) == ";" {
                        spec += 1;
                        continue;
                    }
                    let end = tokens.end(spec, close, canceled)?;
                    inherited_const = output.spec(
                        &tokens,
                        keyword,
                        spec,
                        end,
                        tokens.spans[spec].start,
                        inherited_const,
                        canceled,
                    )?;
                    spec = end.next;
                }
                i = close + 1;
                continue;
            }
            let end = tokens.end(i, tokens.spans.len(), canceled)?;
            output.spec(
                &tokens,
                keyword,
                i + 1,
                end,
                tokens.spans[i].start,
                false,
                canceled,
            )?;
            i = end.next;
            continue;
        }
        // Unknown top-level statements and their nested delimiters are opaque;
        // never start scanning declarations in the middle of an initializer.
        i = tokens.end(i, tokens.spans.len(), canceled)?.next;
    }
    Ok((output.candidates, output.limited))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::symbols::{CapturedSymbols, SymbolLanguage};
    use fcb_core::{
        ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget,
        SourceRevision,
    };
    use fcb_source::CaptureRequest;

    fn owner() -> ArenaOwnerId {
        ArenaOwnerId::new(873).unwrap()
    }
    fn budget() -> ResourceBudget {
        ResourceBudget::new(owner(), ByteLength::new(128 * 1024 * 1024)).unwrap()
    }
    fn build<'a>(
        source: &'a [u8],
        limit: usize,
        budget: &ResourceBudget,
        canceled: impl FnMut() -> bool,
    ) -> Result<CapturedSymbols<'a>, SymbolError> {
        CapturedSymbols::build(
            source,
            CaptureRequest::new(
                FileId::new(owner(), 1).unwrap(),
                SourceRevision::new(owner(), 1).unwrap(),
            )
            .unwrap(),
            QueryGeneration::new(owner(), 1).unwrap(),
            SymbolLanguage::Go,
            None,
            limit,
            budget,
            ResourceAllocationId::new(1).unwrap(),
            canceled,
        )
    }
    fn names(outline: &CapturedSymbols<'_>) -> Vec<String> {
        outline
            .candidates()
            .iter()
            .map(|item| item.name().to_owned())
            .collect()
    }

    #[test]
    fn go_comments_raw_strings_and_local_declarations_are_not_symbols() {
        let source = br#"package sample
/*
func Commented() {}
type Fake struct{}
*/
var Template = `
func StringDeclaration() {}
type NotReal struct{}
`
var callback = func() { type Local struct{}; var hidden = 1 }
func Real() { type Nested struct{}; const hidden = 1 }
"#;
        let budget = budget();
        let outline = build(source, 64, &budget, || false).unwrap();
        assert_eq!(names(&outline), ["Template", "callback", "Real"]);
        for item in outline.candidates() {
            assert_eq!(
                outline.name_bytes(item.id()).unwrap().unwrap(),
                item.name().as_bytes()
            );
            assert_eq!(item.evidence_level(), "heuristic-outline-candidate");
            assert_eq!(item.parent_id(), None);
        }
        // The old line-only rule produces the forbidden name: this fixture is
        // a negative control for the actual false-declaration failure.
        assert!(
            std::str::from_utf8(source)
                .unwrap()
                .lines()
                .any(|line| line.starts_with("func StringDeclaration"))
        );
        assert!(
            !names(&outline)
                .iter()
                .any(|name| name == "StringDeclaration")
        );
    }

    #[test]
    fn go_grouped_types_bindings_and_generic_names_have_exact_evidence() {
        let source = b"package sample\ntype (\n Box[T any] struct { Value T }\n Reader interface { Read() error }\n Alias = Box[int]\n Buffer [32]byte\n)\nconst (\n First = iota\n Second\n _\n Third, Fourth = 3, 4\n)\nvar (\n Left, Right int\n Value = Box[int]{Value: 1}\n)\nfunc Map[T any](items []T) []T { return items }\nfunc (b *Box[T]) Read() T { return b.Value }\n";
        let budget = budget();
        let outline = build(source, 64, &budget, || false).unwrap();
        assert_eq!(
            names(&outline),
            [
                "Box", "Reader", "Alias", "Buffer", "First", "Second", "Third", "Fourth", "Left",
                "Right", "Value", "Map", "Read"
            ]
        );
        assert_eq!(outline.candidates()[0].kind(), &OutlineItemKind::Struct);
        assert_eq!(outline.candidates()[1].kind(), &OutlineItemKind::Interface);
        assert_eq!(outline.candidates()[11].kind(), &OutlineItemKind::Function);
        assert_eq!(outline.candidates()[12].kind(), &OutlineItemKind::Method);
        for item in outline.candidates() {
            assert_eq!(
                outline.name_bytes(item.id()).unwrap().unwrap(),
                item.name().as_bytes()
            );
        }
        assert!(
            outline
                .source_bytes(12)
                .unwrap()
                .starts_with(b"func Map[T any]")
        );
        assert!(
            outline
                .source_bytes(13)
                .unwrap()
                .ends_with(b"return b.Value }")
        );
        assert_eq!(outline.candidates()[1].line(), 4);
    }

    #[test]
    fn go_return_types_multiline_signatures_and_semicolons_do_not_steal_bodies() {
        let source = b"package sample; func Make(\n value int,\n) struct { N int } { return struct { N int }{value} }; func Next() {}\nfunc Callback() func() struct{} { return nil }\nfunc Assembly() int\nfunc Last() {}\n";
        let budget = budget();
        let outline = build(source, 64, &budget, || false).unwrap();
        assert_eq!(
            names(&outline),
            ["Make", "Next", "Callback", "Assembly", "Last"]
        );
        assert!(outline.source_bytes(1).unwrap().ends_with(b"{value} }"));
        assert_eq!(outline.source_bytes(4).unwrap(), b"func Assembly() int");
        assert_eq!(outline.source_bytes(5).unwrap(), b"func Last() {}");
        let comment_newline = b"var first = 1 /*\n*/ var second = 2\n";
        drop(outline);
        assert_eq!(
            names(&build(comment_newline, 64, &budget, || false).unwrap()),
            ["first", "second"]
        );
    }

    #[test]
    fn go_utf16_names_and_ranges_map_back_to_original_units() {
        for little in [false, true] {
            let source = "// 😀\r\nfunc 读取[T any](value T) T { return value }\r\n";
            let mut bytes = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            for unit in source.encode_utf16() {
                bytes.extend_from_slice(&if little {
                    unit.to_le_bytes()
                } else {
                    unit.to_be_bytes()
                });
            }
            let budget = budget();
            let outline = build(&bytes, 64, &budget, || false).unwrap();
            assert_eq!(names(&outline), ["读取"]);
            assert_eq!(outline.candidates()[0].line(), 2);
            let expected: Vec<_> = "读取"
                .encode_utf16()
                .flat_map(|unit| {
                    if little {
                        unit.to_le_bytes()
                    } else {
                        unit.to_be_bytes()
                    }
                })
                .collect();
            assert_eq!(outline.name_bytes(1).unwrap().unwrap(), expected);
            let expected_start = 2 + source[..source.find("读取").unwrap()]
                .encode_utf16()
                .count()
                * 2;
            assert_eq!(
                outline.candidates()[0].name_range().unwrap().start().get(),
                expected_start as u64
            );
        }
    }

    #[test]
    fn go_limits_cancellation_and_bad_context_release_the_reserved_state() {
        let budget = budget();
        let source = b"func First() {}\nfunc Second() {}\n";
        let outline = build(source, 1, &budget, || false).unwrap();
        assert_eq!(names(&outline), ["First"]);
        assert!(outline.output_limited());
        drop(outline);
        assert_eq!(budget.accounting().reserved().get(), 0);
        let mut polls = 0;
        assert!(matches!(
            build(source, 64, &budget, || {
                polls += 1;
                polls == 10
            }),
            Err(SymbolError::Canceled)
        ));
        for source in [
            b"var x = `unclosed\nfunc Fake() {}".as_slice(),
            b"/* unclosed\nfunc Fake() {}",
            b"var x = \"broken\nfunc Fake() {}",
            b"var x = \"escaped\\\"",
            b"func Valid() {}\n\xc2\xa0func Fake() {}",
        ] {
            assert!(
                matches!(
                    build(source, 64, &budget, || false),
                    Err(SymbolError::UnsupportedLexicalContext)
                ),
                "{source:?}"
            );
        }
        for source in [
            b"func Bad(] {}".as_slice(),
            b"func Bad() {",
            b"type Box struct }",
        ] {
            assert!(matches!(
                build(source, 64, &budget, || false),
                Err(SymbolError::InvalidEvidence)
            ));
        }
        let nested = format!("var x = {}0{}", "(".repeat(129), ")".repeat(129));
        assert!(matches!(
            build(nested.as_bytes(), 64, &budget, || false),
            Err(SymbolError::ComplexityLimit)
        ));
        let sequential = "func Item() {}\n".repeat(129);
        assert_eq!(
            build(sequential.as_bytes(), 256, &budget, || false)
                .unwrap()
                .candidates()
                .len(),
            129
        );
        assert_eq!(budget.accounting().reserved().get(), 0);
    }

    #[test]
    fn go_declared_name_limit_counts_only_real_nonblank_candidates() {
        let source = b"const ( _ = iota; Real; )\nvar Left, Right = 1, 2\n";
        let budget = budget();
        let exact = build(source, 3, &budget, || false).unwrap();
        assert_eq!(names(&exact), ["Real", "Left", "Right"]);
        assert!(!exact.output_limited());
        drop(exact);
        let limited = build(source, 2, &budget, || false).unwrap();
        assert_eq!(names(&limited), ["Real", "Left"]);
        assert!(limited.output_limited());
    }
}
