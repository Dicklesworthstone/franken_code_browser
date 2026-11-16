#![forbid(unsafe_code)]

//! Python declaration navigation over the pinned FrankenMarkdown lexer.
//!
//! Comments and all string forms remain opaque upstream tokens. This adapter
//! recognizes class/def/async-def headers at logical statement boundaries and
//! follows indentation iteratively. It does not execute Python, normalize
//! identifiers, resolve decorators, or claim compiler definitions. Assignment
//! targets and dynamically created declarations are outside this route.
//! Header/name/body evidence always maps back to the original captured bytes.

use fcb_source::CaptureEncodingMap;
use franken_markdown::highlight::{Span, Tok, highlight_into};

use super::{MAX_RECURSIVE_SCOPES, SymbolCandidate, SymbolError, mapped};
use crate::OutlineItemKind;

const UNPAIRED: usize = usize::MAX;

fn reserve<T>(capacity: usize) -> Result<Vec<T>, SymbolError> {
    let mut values = Vec::new();
    values
        .try_reserve_exact(capacity)
        .map_err(|_| SymbolError::ResourceDenied)?;
    if values.capacity() > capacity {
        return Err(SymbolError::ResourceDenied);
    }
    Ok(values)
}

fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), SymbolError> {
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
struct Statement {
    start: usize,
    end: usize,
    indent: usize,
}

struct Header {
    name: usize,
    class: bool,
    block: bool,
}

struct Scope {
    indent: usize,
    start: usize,
    candidate: Option<usize>,
    class: bool,
    body_seen: bool,
}

impl<'text> Tokens<'text> {
    fn new(text: &'text str, canceled: &mut impl FnMut() -> bool) -> Result<Self, SymbolError> {
        // CapturedSymbols reserves the complete work envelope before this call.
        // One nonempty upstream span consumes at least one source byte.
        let capacity = text.len().max(1);
        let mut spans = reserve(capacity)?;
        check(canceled)?;
        highlight_into("python", text, &mut spans);
        check(canceled)?;
        if spans.capacity() > capacity {
            return Err(SymbolError::ResourceDenied);
        }
        let mut previous = 0;
        for span in &spans {
            check(canceled)?;
            let Some(word) = text.get(span.start..span.end) else {
                return Err(SymbolError::InvalidEvidence);
            };
            if span.start != previous || span.start >= span.end {
                return Err(SymbolError::InvalidEvidence);
            }
            // Upstream stops incomplete single-quoted strings at LF. Do not
            // expose the following declaration-looking lines as candidates.
            // This checks an opaque token's boundary, not its string contents.
            if span.kind == Tok::Str {
                let bytes = word.as_bytes();
                let quote_at = bytes
                    .iter()
                    .position(|&byte| matches!(byte, b'\'' | b'"'))
                    .ok_or(SymbolError::UnsupportedLexicalContext)?;
                if quote_at > 2
                    || !bytes[..quote_at].iter().all(|&byte| {
                        matches!(byte, b'r' | b'R' | b'b' | b'B' | b'u' | b'U' | b'f' | b'F')
                    })
                {
                    return Err(SymbolError::UnsupportedLexicalContext);
                }
                let quote = bytes[quote_at];
                let width = if bytes
                    .get(quote_at..quote_at + 3)
                    .is_some_and(|opening| opening.iter().all(|&byte| byte == quote))
                {
                    3
                } else {
                    1
                };
                if bytes.len() < quote_at + width * 2
                    || !bytes[bytes.len() - width..]
                        .iter()
                        .all(|&byte| byte == quote)
                    || bytes[..bytes.len() - width]
                        .iter()
                        .rev()
                        .take_while(|&&byte| byte == b'\\')
                        .count()
                        % 2
                        != 0
                {
                    return Err(SymbolError::UnsupportedLexicalContext);
                }
            } else if word.bytes().enumerate().any(|(offset, byte)| {
                byte == b'\r' && text.as_bytes().get(span.start + offset + 1) != Some(&b'\n')
            }) {
                // The shared line map and upstream comments use LF/CRLF. A
                // CR-only source must not advertise misleading line evidence.
                return Err(SymbolError::UnsupportedLexicalContext);
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
        // Keep strings and explicit line-continuation tokens. Removing either
        // could join a declaration-looking string with surrounding statements.
        spans.retain(|span| {
            span.kind != Tok::Comment
                && !text[span.start..span.end].chars().all(char::is_whitespace)
        });
        let mut closes = reserve(spans.len())?;
        closes.resize(spans.len(), UNPAIRED);
        let mut stack = reserve(MAX_RECURSIVE_SCOPES)?;
        for (i, span) in spans.iter().enumerate() {
            check(canceled)?;
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

    fn word(&self, at: usize) -> &'text str {
        self.spans
            .get(at)
            .map_or("", |span| &self.text[span.start..span.end])
    }

    fn keyword(&self, at: usize, word: &str) -> bool {
        self.spans
            .get(at)
            .is_some_and(|span| span.kind == Tok::Keyword)
            && self.word(at) == word
    }

    fn continuation(&self, at: usize) -> bool {
        matches!(self.word(at), "\\\n" | "\\\r\n" | "\\\r")
    }

    fn next(&self, mut at: usize, end: usize) -> usize {
        while at < end && self.continuation(at) {
            at += 1;
        }
        at
    }

    fn after_group(&self, at: usize) -> usize {
        if self.closes[at] == UNPAIRED {
            at + 1
        } else {
            self.closes[at] + 1
        }
    }

    fn statement(
        &self,
        start: usize,
        canceled: &mut impl FnMut() -> bool,
    ) -> Result<Statement, SymbolError> {
        let offset = self.spans[start].start;
        let line_start = self.text[..offset].rfind(['\n', '\r']).map_or(0, |i| i + 1);
        let mut indent = 0usize;
        // Python expands tabs to the next multiple of eight. Other leading
        // whitespace is outside the adapter's qualified indentation subset.
        for byte in self.text[line_start..offset].bytes() {
            indent = match byte {
                b' ' => indent + 1,
                b'\t' => (indent / 8 + 1) * 8,
                _ => return Err(SymbolError::UnsupportedLexicalContext),
            };
            if indent > MAX_RECURSIVE_SCOPES * 8 {
                return Err(SymbolError::ComplexityLimit);
            }
        }
        let mut end = start;
        loop {
            check(canceled)?;
            end = self.after_group(end);
            if end == self.spans.len() {
                if self.continuation(end - 1) {
                    return Err(SymbolError::InvalidEvidence);
                }
                break;
            }
            let gap = &self.text[self.spans[end - 1].end..self.spans[end].start];
            // The upstream continuation token already contains its one joined
            // newline. A further blank/comment line still ends the statement.
            if gap.contains(['\n', '\r']) {
                break;
            }
        }
        Ok(Statement { start, end, indent })
    }

    fn name(&self, at: usize) -> bool {
        let Some(span) = self.spans.get(at) else {
            return false;
        };
        if !matches!(span.kind, Tok::Plain | Tok::Type | Tok::Func) {
            return false;
        }
        let mut chars = self.word(at).chars();
        chars.next().is_some_and(|c| c == '_' || c.is_alphabetic())
            && chars.all(|c| c == '_' || c.is_alphanumeric())
    }

    fn header(
        &self,
        statement: Statement,
        canceled: &mut impl FnMut() -> bool,
    ) -> Result<Option<Header>, SymbolError> {
        let mut at = statement.start;
        if self.keyword(at, "async") {
            at = self.next(at + 1, statement.end);
            if !self.keyword(at, "def") {
                return Ok(None);
            }
        }
        let class = self.keyword(at, "class");
        if !class && !self.keyword(at, "def") {
            return Ok(None);
        }
        let name = self.next(at + 1, statement.end);
        if name == statement.end || !self.name(name) {
            return Err(SymbolError::InvalidEvidence);
        }
        at = self.next(name + 1, statement.end);
        // PEP 695 type parameters belong to the header, never the symbol name.
        if at < statement.end && self.word(at) == "[" {
            at = self.next(self.after_group(at), statement.end);
        }
        if at < statement.end && self.word(at) == "(" {
            at = self.next(self.after_group(at), statement.end);
        } else if !class {
            return Err(SymbolError::InvalidEvidence);
        }
        if !class && at < statement.end && self.word(at) == "-" {
            at = self.next(at + 1, statement.end);
            if at == statement.end || self.word(at) != ">" {
                return Err(SymbolError::InvalidEvidence);
            }
            at = self.next(at + 1, statement.end);
            let annotation = at;
            while at < statement.end && self.word(at) != ":" {
                check(canceled)?;
                at = self.next(self.after_group(at), statement.end);
            }
            if at == annotation {
                return Err(SymbolError::InvalidEvidence);
            }
        }
        if at == statement.end || self.word(at) != ":" {
            return Err(SymbolError::InvalidEvidence);
        }
        let block = self.next(at + 1, statement.end) == statement.end;
        Ok(Some(Header { name, class, block }))
    }
}

fn close(
    scope: Scope,
    end: usize,
    map: &CaptureEncodingMap,
    out: &mut [SymbolCandidate],
) -> Result<(), SymbolError> {
    if !scope.body_seen {
        return Err(SymbolError::InvalidEvidence);
    }
    if let Some(index) = scope.candidate {
        out[index].range = mapped(map, scope.start as u64, end as u64)?;
    }
    Ok(())
}

pub(super) fn extract(
    map: &CaptureEncodingMap,
    line_starts: &[usize],
    max_items: usize,
    canceled: &mut impl FnMut() -> bool,
) -> Result<(Vec<SymbolCandidate>, bool), SymbolError> {
    let tokens = Tokens::new(map.decoded_text(), canceled)?;
    let mut out: Vec<SymbolCandidate> = reserve(max_items)?;
    let mut scopes: Vec<Scope> = reserve(MAX_RECURSIVE_SCOPES)?;
    let mut limited = false;
    let mut at = 0;
    let mut previous_end = 0;
    while at < tokens.spans.len() {
        check(canceled)?;
        let statement = tokens.statement(at, canceled)?;
        while scopes
            .last()
            .is_some_and(|scope| statement.indent <= scope.indent)
        {
            close(
                scopes.pop().ok_or(SymbolError::InvalidEvidence)?,
                previous_end,
                map,
                &mut out,
            )?;
        }
        if let Some(scope) = scopes.last_mut() {
            scope.body_seen = true;
        }
        if let Some(header) = tokens.header(statement, canceled)? {
            let start = tokens.spans[statement.start].start;
            let end = tokens.spans[statement.end - 1].end;
            let name = tokens.spans[header.name];
            let index = if out.len() == max_items {
                limited = true;
                None
            } else {
                let mut owned = String::new();
                owned
                    .try_reserve_exact(name.end - name.start)
                    .map_err(|_| SymbolError::ResourceDenied)?;
                if owned.capacity() > name.end - name.start {
                    return Err(SymbolError::ResourceDenied);
                }
                owned.push_str(tokens.word(header.name));
                let parent = scopes
                    .last()
                    .and_then(|scope| scope.candidate)
                    .map(|index| out[index].id);
                let kind = if header.class {
                    OutlineItemKind::Class
                } else if scopes.last().is_some_and(|scope| scope.class) {
                    OutlineItemKind::Method
                } else {
                    OutlineItemKind::Function
                };
                let index = out.len();
                out.push(SymbolCandidate {
                    id: index as u64 + 1,
                    parent,
                    depth: scopes.len(),
                    name: owned,
                    kind,
                    range: mapped(map, start as u64, end as u64)?,
                    name_range: Some(mapped(map, name.start as u64, name.end as u64)?),
                    line: line_starts.partition_point(|&line| line <= start) as u64,
                });
                Some(index)
            };
            if header.block {
                if scopes.len() == MAX_RECURSIVE_SCOPES {
                    return Err(SymbolError::ComplexityLimit);
                }
                scopes.push(Scope {
                    indent: statement.indent,
                    start,
                    candidate: index,
                    class: header.class,
                    body_seen: false,
                });
            }
        }
        previous_end = tokens.spans[statement.end - 1].end;
        at = statement.end;
    }
    while let Some(scope) = scopes.pop() {
        close(scope, previous_end, map, &mut out)?;
    }
    check(canceled)?;
    Ok((out, limited))
}

#[cfg(test)]
mod tests {
    use super::super::{CapturedSymbols, SymbolLanguage};
    use super::*;
    use fcb_core::{
        ArenaOwnerId, ByteLength, FileId, QueryGeneration, ResourceAllocationId, ResourceBudget,
        SourceRevision,
    };
    use fcb_source::CaptureRequest;

    fn budget() -> ResourceBudget {
        ResourceBudget::new(
            ArenaOwnerId::new(839).unwrap(),
            ByteLength::new(128 * 1024 * 1024),
        )
        .unwrap()
    }
    fn build<'a>(
        bytes: &'a [u8],
        max: usize,
        budget: &ResourceBudget,
    ) -> Result<CapturedSymbols<'a>, SymbolError> {
        let owner = ArenaOwnerId::new(839).unwrap();
        CapturedSymbols::build(
            bytes,
            CaptureRequest::new(
                FileId::new(owner, 1).unwrap(),
                SourceRevision::new(owner, 1).unwrap(),
            )
            .unwrap(),
            QueryGeneration::new(owner, 1).unwrap(),
            SymbolLanguage::Python,
            None,
            max,
            budget,
            ResourceAllocationId::new(1).unwrap(),
            || false,
        )
    }

    #[test]
    fn strings_comments_and_continued_expressions_do_not_create_declarations() {
        let text = b"# class FakeComment:\ntext = r'''\nclass FakeRaw:\n    def fake(self): pass\n'''\nother = f\"\"\"\ndef fake_fstring(): pass\n\"\"\"\nvalue = (\n    'def fake_literal(): pass'\n)\nclass Real:\n    \"\"\"def fake_docstring(): pass\"\"\"\n    @decorator(\n        'class FakeArgument:'\n    )\n    async def run(self):\n        return 'real'\n";
        let budget = budget();
        let symbols = build(text, 32, &budget).unwrap();
        assert_eq!(
            symbols
                .candidates()
                .iter()
                .map(|s| s.name())
                .collect::<Vec<_>>(),
            ["Real", "run"]
        );
        let method = &symbols.candidates()[1];
        assert_eq!(method.parent_id(), Some(1));
        assert_eq!(method.kind(), &OutlineItemKind::Method);
        assert_eq!(symbols.name_bytes(method.id()).unwrap().unwrap(), b"run");
        assert!(
            symbols
                .source_bytes(method.id())
                .unwrap()
                .starts_with(b"async def run")
        );
        assert!(
            symbols
                .source_bytes(method.id())
                .unwrap()
                .ends_with(b"return 'real'")
        );
    }

    #[test]
    fn generic_multiline_headers_and_nested_functions_have_exact_names_and_parents() {
        let text = b"class Box[T](\n    object,\n):\n    def transform[U](\n        self, value: U,\n    ) -> tuple[T, U]:\n        def inner():\n            return value\n        return inner()\ndef outside(): pass\n";
        let budget = budget();
        let symbols = build(text, 32, &budget).unwrap();
        assert_eq!(
            symbols
                .candidates()
                .iter()
                .map(|s| s.name())
                .collect::<Vec<_>>(),
            ["Box", "transform", "inner", "outside"]
        );
        assert_eq!(symbols.candidates()[2].parent_id(), Some(2));
        assert_eq!(symbols.candidates()[2].kind(), &OutlineItemKind::Function);
        assert_eq!(symbols.candidates()[3].parent_id(), None);
        for symbol in symbols.candidates() {
            assert_eq!(
                symbols.name_bytes(symbol.id()).unwrap().unwrap(),
                symbol.name().as_bytes()
            );
            assert_eq!(symbol.evidence_level(), "heuristic-outline-candidate");
        }
        assert!(
            symbols
                .source_bytes(1)
                .unwrap()
                .ends_with(b"return inner()")
        );
        assert!(symbols.source_bytes(3).unwrap().ends_with(b"return value"));
    }

    #[test]
    fn explicit_continuations_and_tabs_preserve_logical_headers() {
        let text = b"class \\\nBox:\n\tasync \\\ndef run(\n\t\tself,\n\t):\n\t\tpass\n";
        let budget = budget();
        let symbols = build(text, 32, &budget).unwrap();
        assert_eq!(
            symbols
                .candidates()
                .iter()
                .map(|s| s.name())
                .collect::<Vec<_>>(),
            ["Box", "run"]
        );
        assert_eq!(symbols.candidates()[1].parent_id(), Some(1));
        assert_eq!(symbols.candidates()[1].line(), 3);
    }

    #[test]
    fn indentation_inside_an_opaque_string_does_not_consume_scope_budget() {
        let source = format!(
            "text = '''\n{}def fake(): pass\n'''\ndef actual(): pass\n",
            " ".repeat(2048)
        );
        let budget = budget();
        let symbols = build(source.as_bytes(), 32, &budget).unwrap();
        assert_eq!(symbols.candidates().len(), 1);
        assert_eq!(symbols.name_bytes(1).unwrap().unwrap(), b"actual");
    }

    #[test]
    fn utf16_names_and_body_ranges_copy_original_capture_bytes() {
        for little in [false, true] {
            let text = "# 😀\r\nclass Café:\r\n    def ouvrir(self):\r\n        return 1\r\n";
            let encode = |text: &str| {
                text.encode_utf16()
                    .flat_map(|unit| {
                        if little {
                            unit.to_le_bytes()
                        } else {
                            unit.to_be_bytes()
                        }
                    })
                    .collect::<Vec<_>>()
            };
            let mut source = if little {
                vec![0xff, 0xfe]
            } else {
                vec![0xfe, 0xff]
            };
            source.extend(encode(text));
            let budget = budget();
            let symbols = build(&source, 32, &budget).unwrap();
            assert_eq!(symbols.name_bytes(1).unwrap().unwrap(), encode("Café"));
            assert_eq!(symbols.name_bytes(2).unwrap().unwrap(), encode("ouvrir"));
            assert_eq!(symbols.candidates()[1].line(), 3);
            assert_eq!(
                symbols.source_bytes(2).unwrap(),
                encode("def ouvrir(self):\r\n        return 1")
            );
        }
    }

    #[test]
    fn truncated_headers_delimiters_and_empty_suites_are_not_successful_outlines() {
        for source in [
            "def missing_body():\n",
            "def broken(\n",
            "class Broken extra:\n    pass\n",
            "def missing_colon()\ndef real(): pass\n",
            "def broken():\npass\n",
            "def broken([)]: pass\n",
        ] {
            let budget = budget();
            assert!(
                matches!(
                    build(source.as_bytes(), 32, &budget),
                    Err(SymbolError::InvalidEvidence)
                ),
                "{source}"
            );
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }

    #[test]
    fn incomplete_strings_and_unsupported_whitespace_cannot_expose_declarations() {
        for source in [
            "text = \"broken\ndef fake(): pass\n",
            "text = 'broken\ndef fake(): pass\n",
            "text = f\"broken\ndef fake(): pass\n",
            "text = r\"broken\\\"\ndef fake(): pass\n",
            "text = b'''broken\ndef fake(): pass\n",
            "text = fr\"\"\"broken\ndef fake(): pass\n",
            "def\u{00a0}fake(): pass\n",
            "# comment\rdef fake(): pass\r",
            "def fake(): pass\rdef next(): pass\r",
        ] {
            let budget = budget();
            assert!(
                matches!(
                    build(source.as_bytes(), 32, &budget),
                    Err(SymbolError::UnsupportedLexicalContext)
                ),
                "{source}"
            );
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
        let source = b"empty = ''\ntriple = \"\"\"\"\"\"\nraw = r'\\\\'\nescaped = \"a\\\"b\"\nbytes = br'''bytes'''\ndef real(): pass\n";
        let budget = budget();
        let symbols = build(source, 32, &budget).unwrap();
        assert_eq!(symbols.candidates().len(), 1);
        assert_eq!(symbols.name_bytes(1).unwrap().unwrap(), b"real");
    }

    #[test]
    fn limits_and_cancellation_release_admission_and_keep_parent_ranges() {
        let source = b"class Box:\n    def first(self): pass\n    def second(self): pass\ndef last(): pass\n";
        let budget = budget();
        let symbols = build(source, 1, &budget).unwrap();
        assert_eq!(symbols.candidates().len(), 1);
        assert!(symbols.output_limited());
        assert!(
            symbols
                .source_bytes(1)
                .unwrap()
                .ends_with(b"def second(self): pass")
        );
        drop(symbols);
        assert_eq!(budget.accounting().reserved().get(), 0);
        let map = CaptureEncodingMap::build(source).unwrap();
        let mut calls = 0;
        assert!(matches!(
            extract(&map, &[0], 32, &mut || {
                calls += 1;
                calls == 12
            }),
            Err(SymbolError::Canceled)
        ));
        let deep = format!("value = {}0{}\n", "(".repeat(129), ")".repeat(129));
        assert!(matches!(
            build(deep.as_bytes(), 32, &budget),
            Err(SymbolError::ComplexityLimit)
        ));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
