#![forbid(unsafe_code)]

//! Bounded JavaScript/TypeScript declaration navigation over the pinned FMD
//! lexers, including their distinct JSX/TSX routes.
//!
//! Upstream owns comments, strings, templates, regexes and JSX boundaries.
//! This source-specific recognizer traverses module and class/interface scopes;
//! function bodies, initializers, computed names and destructuring stay opaque.
//! It recognizes a useful declaration subset, not compiler definitions or a
//! complete symbol inventory. All names and enclosing evidence are mapped back
//! to the same original capture, including UTF-16 sources.

use franken_markdown::highlight::{Span, Tok, highlight_into};
use fcb_source::CaptureEncodingMap;

use super::{MAX_RECURSIVE_SCOPES, SymbolCandidate, SymbolError, SymbolLanguage, mapped};
use crate::{OutlineItemKind, imports::{ImportError, validate_javascript_context}};

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

#[derive(Clone, Copy, Eq, PartialEq)]
enum Context { Module, Class, Interface }

#[derive(Clone, Copy)]
struct Scope { cursor: usize, end: usize, parent: Option<u64>, context: Context }

struct Declaration {
    name: Name,
    kind: OutlineItemKind,
    next: usize,
    body: Option<(usize, usize, Context)>,
}

struct Tokens<'text> {
    text: &'text str,
    language: SymbolLanguage,
    spans: Vec<Span>,
    closes: Vec<usize>,
}

impl<'text> Tokens<'text> {
    fn new(text: &'text str, language: SymbolLanguage, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, SymbolError> {
        if !matches!(language, SymbolLanguage::JavaScript | SymbolLanguage::TypeScript
            | SymbolLanguage::Jsx | SymbolLanguage::Tsx) {
            return Err(SymbolError::InvalidEvidence);
        }
        // The pinned owner ends line comments at LF and does not recognize
        // hashbangs or escaped identifier spellings. Do not reinterpret them.
        if text.starts_with("#!") || text.chars().any(|c| matches!(c, '\u{2028}' | '\u{2029}'))
            || text.as_bytes().iter().enumerate().any(|(i, &b)| b == b'\r'
                && text.as_bytes().get(i + 1) != Some(&b'\n')) {
            return Err(SymbolError::UnsupportedLexicalContext);
        }
        let capacity = text.len().max(1);
        let mut spans = reserved(capacity)?;
        checkpoint(canceled)?;
        highlight_into(language.name(), text, &mut spans);
        checkpoint(canceled)?;
        if spans.capacity() > capacity { return Err(SymbolError::ResourceDenied); }
        let mut previous = 0;
        let mut significant: Option<Span> = None;
        for span in &spans {
            checkpoint(canceled)?;
            let Some(word) = text.get(span.start..span.end) else { return Err(SymbolError::InvalidEvidence); };
            if span.start != previous || span.start >= span.end { return Err(SymbolError::InvalidEvidence); }
            if !matches!(span.kind, Tok::Comment | Tok::Str) && word == "\\" {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
            // The pinned template owner does not model regexes inside
            // interpolation. Regex braces/backticks can prematurely terminate
            // its opaque string and expose apparent code. Refuse slash-bearing
            // interpolation tails (also conservatively URLs/comments) until the
            // upstream owner qualifies that context; ordinary templates remain
            // supported without locally re-lexing their contents.
            if span.kind == Tok::Str && word.starts_with('\u{60}')
                && word.find("${").is_some_and(|start| word[start + 2..].contains('/')) {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
            // FMD treats every closing brace as a block close. A slash
            // following an object literal can therefore consume division plus
            // the opening slash of a later regex as one string, exposing its
            // contents as code. Without a compiler parse that brace/slash
            // boundary is ambiguous; a separating semicolon is supported.
            if span.kind == Tok::Str && word.starts_with('/')
                && significant.is_some_and(|previous| previous.kind == Tok::Punct
                    && &text[previous.start..previous.end] == "}") {
                return Err(SymbolError::UnsupportedLexicalContext);
            }
            if span.kind != Tok::Comment && !word.chars().all(char::is_whitespace) { significant = Some(*span); }
            previous = span.end;
        }
        if previous != text.len() { return Err(SymbolError::InvalidEvidence); }
        // Share the existing import adapter's regex/control/ASI guard. A
        // highlighter's ambiguous slash must not expose fictitious declarations.
        validate_javascript_context(text, &spans, canceled).map_err(|error| match error {
            ImportError::Canceled => SymbolError::Canceled,
            ImportError::ResourceDenied => SymbolError::ResourceDenied,
            ImportError::UnsupportedLexicalContext => SymbolError::UnsupportedLexicalContext,
            _ => SymbolError::InvalidEvidence,
        })?;
        // Opaque strings and JSX text remain terminals. Removing them could
        // accidentally join syntax from opposite sides of an expression.
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
        Ok(Self { text, language, spans, closes })
    }

    fn typescript(&self) -> bool {
        matches!(self.language, SymbolLanguage::TypeScript | SymbolLanguage::Tsx)
    }

    fn word(&self, i: usize) -> &'text str {
        self.spans.get(i).map_or("", |span| &self.text[span.start..span.end])
    }

    fn keyword(&self, i: usize, word: &str) -> bool {
        self.spans.get(i).is_some_and(|span| span.kind == Tok::Keyword) && self.word(i) == word
    }

    fn after_group(&self, i: usize) -> usize {
        if self.closes[i] == UNPAIRED { i + 1 } else { self.closes[i] + 1 }
    }

    fn newline(&self, previous: usize, next: usize) -> bool {
        self.text[self.spans[previous].end..self.spans[next].start].contains('\n')
    }

    fn name(&self, i: usize, member: bool) -> Option<Name> {
        let private = member && self.word(i) == "#";
        let at = i + usize::from(private);
        let span = *self.spans.get(at)?;
        if private && self.spans[i].end != span.start { return None; }
        let word = self.word(at);
        let contextual = matches!(word, "async" | "get" | "set" | "of" | "from" | "type" | "namespace"
            | "readonly" | "abstract" | "override" | "declare" | "global" | "as" | "satisfies");
        if !matches!(span.kind, Tok::Plain | Tok::Type | Tok::Func)
            && !(span.kind == Tok::Keyword && (member || contextual)) { return None; }
        let mut chars = word.chars();
        let start = |c: char| c.is_alphabetic() || matches!(c, '_' | '$' | '\u{200c}' | '\u{200d}');
        if !chars.next().is_some_and(start) || !chars.all(|c| start(c) || c.is_numeric()) { return None; }
        // Combining marks/escapes not owned as one identifier by this pin must
        // not publish a partial identifier with an apparently exact name span.
        if self.spans.get(at + 1).is_some_and(|next| next.start == span.end
            && (self.word(at + 1) == "\\" || !self.word(at + 1).is_ascii()
                || self.word(at + 1).chars().next().is_some_and(|c| start(c) || c.is_numeric()))) {
            return None;
        }
        Some(Name { start: self.spans[i].start, end: span.end, next: at + 1 })
    }

    fn prefix(&self, mut i: usize, end: usize, context: Context,
        canceled: &mut impl FnMut() -> bool) -> Result<usize, SymbolError> {
        let mut work = 0usize;
        while i < end {
            checkpoint(canceled)?;
            work += 1;
            if work > MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
            if self.word(i) == "@" {
                let Some(name) = self.name(i + 1, false) else { break; };
                i = name.next;
                while i + 1 < end && self.word(i) == "." && self.name(i + 1, false).is_some() {
                    checkpoint(canceled)?;
                    work += 1;
                    if work > MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
                    i += 2;
                }
                if i < end && self.word(i) == "(" { i = self.after_group(i); }
                continue;
            }
            let word = self.word(i);
            let modifier = if context == Context::Module {
                matches!(word, "export" | "default" | "declare" | "abstract")
                    || (word == "async" && self.keyword(i + 1, "function")
                        && !self.newline(i, i + 1))
                    || (word == "const" && self.typescript() && self.keyword(i + 1, "enum"))
            } else {
                matches!(word, "public" | "protected" | "private" | "static" | "abstract"
                    | "declare" | "override" | "readonly" | "accessor")
                    && !matches!(self.word(i + 1), "(" | ":" | "=" | ";" | "?" | "" )
                    || (word == "async" && i + 1 < end && !self.newline(i, i + 1)
                        && (self.name(i + 1, true).is_some() || self.word(i + 1) == "*"))
            };
            if !modifier { break; }
            i += 1;
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
                    let Some(next) = depth.checked_sub(self.word(i).len()) else { return Ok(None); };
                    depth = next;
                    if depth == 0 { return Ok(Some(i + 1)); }
                }
                ";" => return Ok(None),
                _ => {}
            }
            i = self.after_group(i);
        }
        Ok(None)
    }

    fn module_start(&self, i: usize) -> bool {
        self.spans.get(i).is_some_and(|span| span.kind == Tok::Keyword)
            && matches!(self.word(i), "export" | "declare" | "abstract" | "async" | "function"
                | "class" | "interface" | "type" | "namespace" | "module" | "enum"
                | "const" | "let" | "var" | "import")
    }

    fn member_start(&self, i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<bool, SymbolError> {
        let mut i = self.prefix(i, scope.end, scope.context, canceled)?;
        if self.word(i) == "*" { i += 1; }
        if matches!(self.word(i), "get" | "set") && self.name(i + 1, true).is_some() { i += 1; }
        let Some(name) = self.name(i, true) else { return Ok(false); };
        Ok(matches!(self.word(name.next), "(" | "<" | "?" | ":" | "=" | "!" | ";"))
    }

    fn value_end(&self, i: usize) -> bool {
        let span = self.spans[i];
        match span.kind {
            Tok::Str | Tok::Number | Tok::Type | Tok::Func => true,
            Tok::Plain => self.name(i, false).is_some(),
            Tok::Keyword => matches!(self.word(i), "this" | "super" | "true" | "false" | "null"
                | "undefined" | "any" | "unknown" | "never" | "void" | "number" | "string"
                | "boolean" | "bigint" | "symbol" | "object") || self.name(i, false).is_some(),
            Tok::Punct => matches!(self.word(i), ")" | "]" | "}"),
            Tok::Operator => matches!(self.word(i), "++" | "--"),
            Tok::Comment => false,
        }
    }

    fn continuation(&self, i: usize) -> bool {
        let span = self.spans[i];
        (span.kind == Tok::Operator && !matches!(self.word(i), "++" | "--" | "!" | "~" | "..."))
            || matches!(self.word(i), "(" | "[" | "." | "as" | "satisfies")
            || (span.kind == Tok::Str && self.word(i).starts_with('\u{60}'))
    }

    // Group only terminals already classified as JSX by the owner. Attribute
    // and child expression containers are opaque paired code groups. This keeps
    // an entire outer element/fragment in an arrow's evidence; an opening or
    // nested closing tag alone is not the end of the expression.
    fn jsx_end(&self, at: usize, end: usize, previous_group: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<usize>, SymbolError> {
        let tag_name = |i: usize| self.spans.get(i)
            .is_some_and(|span| matches!(span.kind, Tok::Keyword | Tok::Type));
        if !matches!(self.language, SymbolLanguage::Jsx | SymbolLanguage::Tsx)
            || self.word(at) != "<" || self.spans[at].kind != Tok::Operator
            || (!tag_name(at + 1) && self.word(at + 1) != ">")
            || previous_group || (at > 0 && self.value_end(at - 1)) {
            return Ok(None);
        }
        let mut i = at;
        let mut depth = 0usize;
        while i < end {
            checkpoint(canceled)?;
            if self.spans[i].kind == Tok::Operator && self.word(i) == "<" {
                if !tag_name(i + 1) && self.word(i + 1) != ">" {
                    return Err(SymbolError::UnsupportedLexicalContext);
                }
                if depth == MAX_RECURSIVE_SCOPES { return Err(SymbolError::ComplexityLimit); }
                depth += 1;
                i += 1;
                while i < end {
                    checkpoint(canceled)?;
                    if self.spans[i].kind == Tok::Operator && self.word(i) == "/>" {
                        depth -= 1; i += 1; break;
                    }
                    if self.spans[i].kind == Tok::Operator && self.word(i) == ">" { i += 1; break; }
                    i = self.after_group(i);
                }
                if depth == 0 { return Ok(Some(i)); }
            } else if self.spans[i].kind == Tok::Operator && self.word(i) == "</" {
                let close = if self.word(i + 1) == ">" { i + 1 }
                    else if tag_name(i + 1) && self.word(i + 2) == ">" { i + 2 }
                    else { return Err(SymbolError::UnsupportedLexicalContext); };
                if close >= end { return Err(SymbolError::UnsupportedLexicalContext); }
                depth = depth.checked_sub(1).ok_or(SymbolError::InvalidEvidence)?;
                i = close + 1;
                if depth == 0 { return Ok(Some(i)); }
            } else { i = self.after_group(i); }
        }
        Err(SymbolError::UnsupportedLexicalContext)
    }

    // A recognized TS call/instantiation type-argument group is an expression
    // value. Keep that fact local: an ordinary comparison's '>' still requires
    // a right operand, including a named function expression on the next line.
    fn expression_generic(&self, i: usize, end: usize, previous_group: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<usize>, SymbolError> {
        if !self.typescript() || self.word(i) != "<" || i == 0
            || !(previous_group || self.name(i - 1, false).is_some()
                || matches!(self.word(i - 1), ")" | "]")) {
            return Ok(None);
        }
        let Some(next) = self.generic_end(i, end, canceled)? else { return Ok(None); };
        // A comparison can have a later '>' in an unrelated statement. Never
        // borrow that delimiter across declarations or assignment expressions.
        let mut part = i + 1;
        while part + 1 < next {
            checkpoint(canceled)?;
            if self.word(part) == "=" || (self.spans[part].kind == Tok::Keyword
                && matches!(self.word(part), "function" | "class" | "const" | "let" | "var" | "export"
                    | "interface" | "namespace" | "module" | "enum" | "return" | "throw" | "if"
                    | "else" | "for" | "while" | "switch" | "try" | "catch" | "finally" | "do"
                    | "break" | "continue")) {
                return Ok(None);
            }
            part = self.after_group(part);
        }
        if next == end || matches!(self.word(next), ";" | "," | "(" | "[" | "." | "?.")
            || (next < end && self.continuation(next))
            || (next < end && self.newline(next - 1, next)) {
            Ok(Some(next))
        } else { Ok(None) }
    }

    fn expression_end(&self, start: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        let mut i = start;
        let mut previous_group = false;
        while i < end {
            checkpoint(canceled)?;
            if matches!(self.word(i), ";" | ",") { break; }
            if i > start && self.newline(i - 1, i)
                && (previous_group || self.value_end(i - 1)) && !self.continuation(i) { break; }
            if let Some(next) = self.jsx_end(i, end, previous_group, canceled)? {
                i = next; previous_group = true;
            } else if let Some(next) = self.expression_generic(i, end, previous_group, canceled)? {
                i = next; previous_group = true;
            } else { i = self.after_group(i); previous_group = false; }
        }
        Ok(i)
    }

    fn expression_finish(&self, start: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        let finish = self.expression_end(start, end, canceled)?;
        if finish == start { return Ok(None); }
        Ok(Some(finish + usize::from(finish < end && self.word(finish) == ";")))
    }

    // Return types can include grouped object/tuple/function types and generic
    // arguments. A brace in operand position belongs to a type; a later brace
    // begins the implementation. This grammar does not infer compiler types.
    fn type_end(&self, start: usize, scope: Scope, arrow: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<usize>, SymbolError> {
        let mut i = start;
        let mut operand = true;
        while i < scope.end {
            checkpoint(canceled)?;
            let word = self.word(i);
            if matches!(word, ";" | "," | "=") || (arrow && word == "=>") || (word == "{" && !operand) {
                return Ok((i > start && !operand).then_some(i));
            }
            if i > start && !operand && self.newline(i - 1, i)
                && (if scope.context == Context::Module { self.module_start(i) }
                    else { self.member_start(i, scope, canceled)? }) {
                return Ok(Some(i));
            }
            match word {
                "(" | "[" | "{" => { i = self.after_group(i); operand = false; }
                "<" => {
                    let Some(next) = self.generic_end(i, scope.end, canceled)? else { return Ok(None); };
                    i = next; operand = false;
                }
                "|" | "&" | "?" | ":" | "=>" | "." | "extends" | "is" | "keyof" | "typeof"
                    | "readonly" | "infer" | "new" | "abstract" | "asserts" => { i += 1; operand = true; }
                _ if self.name(i, true).is_some() || matches!(self.spans[i].kind, Tok::Str | Tok::Number) => {
                    // Adjacent type operands require an operator. A newline
                    // can begin an ordinary statement as well as a declaration.
                    if !operand {
                        return Ok((i > start && self.newline(i - 1, i)).then_some(i));
                    }
                    i += 1; operand = false;
                }
                _ => return Ok(None),
            }
        }
        Ok((i > start && !operand).then_some(i))
    }

    fn signature_end(&self, mut i: usize, scope: Scope, signature: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<usize>, SymbolError> {
        if self.typescript() && self.word(i) == ":" {
            let Some(next) = self.type_end(i + 1, scope, false, canceled)? else { return Ok(None); };
            i = next;
        }
        if self.word(i) == "{" && i < scope.end { return Ok(Some(self.after_group(i))); }
        if signature {
            if i < scope.end && matches!(self.word(i), ";" | ",") { return Ok(Some(i + 1)); }
            if i == scope.end || (i > 0 && self.newline(i - 1, i)) { return Ok(Some(i)); }
        }
        Ok(None)
    }

    fn function_end(&self, at: usize, scope: Scope, named: bool,
        canceled: &mut impl FnMut() -> bool) -> Result<Option<(Option<Name>, usize)>, SymbolError> {
        let mut i = at + 1;
        if self.word(i) == "*" { i += 1; }
        let name = self.name(i, false);
        if let Some(name) = name { i = name.next; }
        else if named { return Ok(None); }
        if self.typescript() {
            let Some(next) = self.generic_end(i, scope.end, canceled)? else { return Ok(None); };
            i = next;
        }
        if self.word(i) != "(" || i >= scope.end { return Ok(None); }
        let Some(next) = self.signature_end(self.after_group(i), scope, self.typescript() && named, canceled)?
            else { return Ok(None); };
        Ok(Some((name, next)))
    }

    fn type_declaration(&self, at: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let (kind, context) = match self.word(at) {
            "class" if self.keyword(at, "class") => (OutlineItemKind::Class, Some(Context::Class)),
            "interface" if self.typescript() && self.keyword(at, "interface") => (OutlineItemKind::Interface, Some(Context::Interface)),
            "enum" if self.typescript() && self.keyword(at, "enum") => (OutlineItemKind::Enum, None),
            "namespace" | "module" if self.typescript() && self.spans[at].kind == Tok::Keyword =>
                (OutlineItemKind::Module, Some(Context::Module)),
            _ => return Ok(None),
        };
        let Some(name) = self.name(at + 1, false) else { return Ok(None); };
        let mut i = name.next;
        if self.typescript() {
            let Some(next) = self.generic_end(i, scope.end, canceled)? else { return Ok(None); };
            i = next;
        }
        while i < scope.end {
            checkpoint(canceled)?;
            if self.word(i) == "{" {
                let close = self.closes[i];
                return Ok(Some(Declaration { name, kind, next: close + 1,
                    body: context.map(|context| (i + 1, close, context)) }));
            }
            if matches!(self.word(i), ";" | "=") || self.module_start(i) { return Ok(None); }
            if self.typescript() && self.word(i) == "<" {
                let Some(next) = self.generic_end(i, scope.end, canceled)? else { return Ok(None); };
                i = next;
            } else { i = self.after_group(i); }
        }
        Ok(None)
    }

    fn arrow_end(&self, at: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        let mut i = at;
        if self.word(i) == "async" && i + 1 < scope.end && self.word(i + 1) != "=>"
            && !self.newline(i, i + 1) { i += 1; }
        if self.typescript() {
            let Some(next) = self.generic_end(i, scope.end, canceled)? else { return Ok(None); };
            i = next;
        }
        i = if i < scope.end && self.word(i) == "(" { self.after_group(i) }
            else if let Some(name) = self.name(i, false) { name.next }
            else { return Ok(None); };
        if self.typescript() && self.word(i) == ":" {
            let Some(next) = self.type_end(i + 1, scope, true, canceled)? else { return Ok(None); };
            i = next;
        }
        if self.word(i) != "=>" || i + 1 >= scope.end || self.newline(i - 1, i) { return Ok(None); }
        i += 1;
        if self.word(i) == "{" {
            let finish = self.after_group(i);
            Ok(Some(finish + usize::from(finish < scope.end && self.word(finish) == ";")))
        } else { self.expression_finish(i, scope.end, canceled) }
    }

    fn binding_value(&self, at: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<usize>, SymbolError> {
        let mut i = at;
        if self.keyword(i, "async") && self.keyword(i + 1, "function") && !self.newline(i, i + 1) { i += 1; }
        if self.keyword(i, "function") {
            return Ok(self.function_end(i, scope, false, canceled)?.map(|(_, next)|
                next + usize::from(next < scope.end && self.word(next) == ";")));
        }
        self.arrow_end(at, scope, canceled)
    }

    fn binding(&self, at: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let Some(name) = self.name(at + 1, false) else { return Ok(None); };
        let mut i = name.next;
        if self.typescript() && self.word(i) == ":" {
            let Some(next) = self.type_end(i + 1, scope, false, canceled)? else { return Ok(None); };
            i = next;
        }
        if self.word(i) != "=" { return Ok(None); }
        let Some(next) = self.binding_value(i + 1, scope, canceled)? else { return Ok(None); };
        Ok(Some(Declaration { name, kind: OutlineItemKind::Function, next, body: None }))
    }

    fn member(&self, mut i: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let accessor = matches!(self.word(i), "get" | "set") && self.name(i + 1, true).is_some();
        if accessor { i += 1; }
        if self.word(i) == "*" { i += 1; }
        let Some(name) = self.name(i, true) else { return Ok(None); };
        let mut next = name.next;
        if self.typescript() && matches!(self.word(next), "?" | "!") { next += 1; }
        if self.typescript() {
            let Some(after) = self.generic_end(next, scope.end, canceled)? else { return Ok(None); };
            next = after;
        }
        if next < scope.end && self.word(next) == "(" {
            let Some(next) = self.signature_end(self.after_group(next), scope, self.typescript(), canceled)?
                else { return Ok(None); };
            let kind = if accessor { OutlineItemKind::Property }
                else if scope.context == Context::Class && &self.text[name.start..name.end] == "constructor" {
                    OutlineItemKind::Initializer
                } else { OutlineItemKind::Method };
            return Ok(Some(Declaration { name, kind, next, body: None }));
        }
        if accessor { return Ok(None); }
        let typed = self.typescript() && self.word(next) == ":";
        if typed {
            let Some(after) = self.type_end(next + 1, scope, false, canceled)? else { return Ok(None); };
            next = after;
        }
        let mut kind = OutlineItemKind::Property;
        if self.word(next) == "=" {
            if let Some(after) = self.binding_value(next + 1, scope, canceled)? {
                next = after; kind = OutlineItemKind::Method;
            } else {
                let Some(after) = self.expression_finish(next + 1, scope.end, canceled)? else { return Ok(None); };
                next = after;
            }
        } else if next < scope.end && matches!(self.word(next), ";" | ",") {
            next += 1;
        } else if !typed || (next < scope.end && !self.newline(next - 1, next)) {
            return Ok(None);
        }
        Ok(Some(Declaration { name, kind, next, body: None }))
    }

    fn declaration(&self, start: usize, scope: Scope, canceled: &mut impl FnMut() -> bool)
        -> Result<Option<Declaration>, SymbolError> {
        let i = self.prefix(start, scope.end, scope.context, canceled)?;
        if i >= scope.end { return Ok(None); }
        if scope.context != Context::Module { return self.member(i, scope, canceled); }
        if let Some(value) = self.type_declaration(i, scope, canceled)? { return Ok(Some(value)); }
        if self.keyword(i, "function") {
            let Some((Some(name), next)) = self.function_end(i, scope, true, canceled)? else { return Ok(None); };
            return Ok(Some(Declaration { name, kind: OutlineItemKind::Function, next, body: None }));
        }
        if self.typescript() && self.keyword(i, "type") {
            let Some(name) = self.name(i + 1, false) else { return Ok(None); };
            let Some(next) = self.generic_end(name.next, scope.end, canceled)? else { return Ok(None); };
            if self.word(next) != "=" { return Ok(None); }
            let Some(mut next) = self.type_end(next + 1, scope, false, canceled)? else { return Ok(None); };
            if next < scope.end {
                if self.word(next) == ";" { next += 1; }
                else if !self.newline(next - 1, next) || self.continuation(next) || self.word(next) == "," { return Ok(None); }
            }
            return Ok(Some(Declaration { name, kind: OutlineItemKind::TypeAlias, next, body: None }));
        }
        if matches!(self.word(i), "const" | "let" | "var") && self.spans[i].kind == Tok::Keyword {
            return self.binding(i, scope, canceled);
        }
        Ok(None)
    }

    fn skip_statement(&self, start: usize, end: usize, canceled: &mut impl FnMut() -> bool)
        -> Result<usize, SymbolError> {
        if matches!(self.word(start), ";" | ",") { return Ok(start + 1); }
        // An unknown block, control body or initializer is opaque. Do not turn
        // names in its contents into siblings of the enclosing declaration.
        if self.word(start) == "{" { return Ok(self.after_group(start)); }
        let mut i = start;
        let mut previous_group = false;
        while i < end {
            checkpoint(canceled)?;
            if matches!(self.word(i), ";" | ",") { return Ok(i + 1); }
            if i > start && self.newline(i - 1, i)
                && (previous_group || self.value_end(i - 1)) && !self.continuation(i) {
                return Ok(i);
            }
            if self.word(i) == "{" { return Ok(self.after_group(i)); }
            if let Some(next) = self.jsx_end(i, end, previous_group, canceled)? {
                i = next; previous_group = true;
            } else if let Some(next) = self.expression_generic(i, end, previous_group, canceled)? {
                i = next; previous_group = true;
            } else { i = self.after_group(i); previous_group = false; }
        }
        Ok(end)
    }
}

pub(super) fn extract(map: &CaptureEncodingMap, language: SymbolLanguage, line_starts: &[usize], limit: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(Vec<SymbolCandidate>, bool), SymbolError> {
    let tokens = Tokens::new(map.decoded_text(), language, canceled)?;
    let mut candidates = reserved(limit)?;
    let mut scopes = reserved(MAX_RECURSIVE_SCOPES + 1)?;
    scopes.push(Scope { cursor: 0, end: tokens.spans.len(), parent: None, context: Context::Module });
    while let Some(scope) = scopes.last().copied() {
        checkpoint(canceled)?;
        let i = scope.cursor;
        if i >= scope.end { scopes.pop(); continue; }
        let next = if let Some(declaration) = tokens.declaration(i, scope, canceled)? {
            if candidates.len() == limit { return Ok((candidates, true)); }
            if declaration.next <= i || declaration.next > scope.end { return Err(SymbolError::InvalidEvidence); }
            let start = tokens.spans[i].start;
            let end = tokens.spans[declaration.next - 1].end;
            let name = declaration.name;
            if name.start < start || name.end > end || name.start >= name.end { return Err(SymbolError::InvalidEvidence); }
            if name.end - name.start > 4096 { return Err(SymbolError::ComplexityLimit); }
            let id = candidates.len() as u64 + 1;
            candidates.push(SymbolCandidate { id, parent: scope.parent, depth: scopes.len() - 1,
                name: tokens.text[name.start..name.end].to_owned(), kind: declaration.kind,
                range: mapped(map, start as u64, end as u64)?,
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
