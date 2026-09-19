#![forbid(unsafe_code)]

//! Source token presentation adapter. The upstream lexer owns every classification.
//! Offsets are UTF-16 code units for native attributed strings, not byte offsets.
use std::mem::size_of;
use fcb_core::{ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use franken_markdown::highlight::{self, Span, Tok};

pub const MAX_SOURCE_HIGHLIGHT_BYTES: usize = 4 * 1024 * 1024;
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SourceHighlightError { Limit, Admission, InvalidSpans }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceTokenRun { pub start: u64, pub length: u64, pub role: &'static str }
pub struct SourceHighlight {
    pub language: &'static str,
    pub syntax_supported: bool,
    pub runs: Vec<SourceTokenRun>,
    _lease: ResourceLease,
}

/// Highlight one complete supplied UTF-8 observation. This synchronous worker
/// operation does no I/O and must never run in a redraw callback. It reserves
/// the worst case of one span/run per input byte before calling the lexer.
pub fn source_highlight(text: &str, language: &str, budget: &ResourceBudget,
    allocation: ResourceAllocationId, owner: fcb_core::ArenaOwnerId)
    -> Result<SourceHighlight, SourceHighlightError> {
    if text.len() > MAX_SOURCE_HIGHLIGHT_BYTES { return Err(SourceHighlightError::Limit); }
    let capacity = text.len().max(1);
    let charge = capacity.checked_mul(size_of::<Span>() + size_of::<SourceTokenRun>())
        .and_then(|n| n.checked_add(size_of::<SourceHighlight>()))
        .ok_or(SourceHighlightError::Admission)?;
    let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
        .map_err(|_| SourceHighlightError::Admission)?;
    let language = franken_markdown::LanguageId::from_query(language)
        .map_or("plain", |id| id.canonical_name());
    let supported = highlight::is_supported(language);
    let mut spans = Vec::new();
    spans.try_reserve_exact(capacity).map_err(|_| SourceHighlightError::Admission)?;
    if spans.capacity() > capacity { return Err(SourceHighlightError::Admission); }
    highlight::highlight_into(language, text, &mut spans);
    if spans.capacity() > capacity { return Err(SourceHighlightError::InvalidSpans); }
    let mut runs = Vec::new();
    runs.try_reserve_exact(spans.len()).map_err(|_| SourceHighlightError::Admission)?;
    if runs.capacity() > capacity { return Err(SourceHighlightError::Admission); }
    let (mut next_byte, mut next_utf16) = (0, 0u64);
    for span in spans {
        if span.start != next_byte || span.end < span.start {
            return Err(SourceHighlightError::InvalidSpans);
        }
        let fragment = text.get(span.start..span.end).ok_or(SourceHighlightError::InvalidSpans)?;
        let length = fragment.encode_utf16().count() as u64;
        if length != 0 { runs.push(SourceTokenRun { start: next_utf16, length, role: role(span.kind) }); }
        next_byte = span.end;
        next_utf16 = next_utf16.checked_add(length).ok_or(SourceHighlightError::InvalidSpans)?;
    }
    if next_byte != text.len() { return Err(SourceHighlightError::InvalidSpans); }
    Ok(SourceHighlight { language, syntax_supported: supported, runs, _lease: lease })
}
fn role(kind: Tok) -> &'static str {
    match kind { Tok::Plain => "plain", Tok::Keyword => "keyword", Tok::Type => "type",
        Tok::Func => "function", Tok::Str => "string", Tok::Number => "number",
        Tok::Comment => "comment", Tok::Operator => "operator", Tok::Punct => "punctuation" }
}
