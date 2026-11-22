#![forbid(unsafe_code)]

//! Matching `()`, `[]` and `{}` in one complete captured source.
//!
//! FrankenMarkdown owns lexical boundaries. Only its punctuation terminals
//! participate; comments, strings, template contents and other opaque tokens
//! never supply a mate. These are lexical navigation candidates, not compiler
//! structure. Both directions use the same bounded stack and original-source
//! mapping, including UTF-16 and a leading BOM. Angle brackets are excluded.

mod lexical;
#[cfg(test)]
mod tests;

use std::mem::size_of;
use fcb_core::{ByteLength, ByteRange, DecodedUtf8Offset, DecodedUtf8Range, FileId,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureEncodingMap, CaptureRequest, DetectedEncoding, MappingSpan,
    SpanKind, detect_encoding};
use franken_markdown::highlight::{Span, Tok, highlight_into};
use crate::symbols::SymbolLanguage;

/// The upstream batch lexer is bracketed by cancellation checks. Larger files
/// remain readable, but do not receive a whole-capture delimiter claim here.
pub const MAX_DELIMITER_SOURCE_BYTES: usize = 512 * 1024;
pub const MAX_DELIMITER_DEPTH: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelimiterError {
    SourceLimit, ComplexityLimit, InvalidSelection, UnsupportedLanguage,
    UnsupportedEncoding, UnsupportedLexicalContext, InvalidEvidence,
    ResourceDenied, Canceled,
}
impl DelimiterError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::SourceLimit => "DELIMITER_SOURCE_LIMIT",
            Self::ComplexityLimit => "DELIMITER_COMPLEXITY_LIMIT",
            Self::InvalidSelection => "DELIMITER_INVALID_SELECTION",
            Self::UnsupportedLanguage => "DELIMITER_UNSUPPORTED_LANGUAGE",
            Self::UnsupportedEncoding => "DELIMITER_UNSUPPORTED_ENCODING",
            Self::UnsupportedLexicalContext => "DELIMITER_UNSUPPORTED_LEXICAL_CONTEXT",
            Self::InvalidEvidence => "DELIMITER_INVALID_EVIDENCE",
            Self::ResourceDenied => "DELIMITER_RESOURCE_DENIED",
            Self::Canceled => "DELIMITER_CANCELED",
        }
    }
}
impl std::fmt::Display for DelimiterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for DelimiterError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum DelimiterState { Matched, Unmatched, Mismatched, NotCode, NotDelimiter }
impl DelimiterState {
    pub const fn name(self) -> &'static str {
        match self {
            Self::Matched => "matched", Self::Unmatched => "unmatched",
            Self::Mismatched => "mismatched", Self::NotCode => "not-code",
            Self::NotDelimiter => "not-delimiter",
        }
    }
}

/// Source-specific receipt. A matching range is present only for `Matched`;
/// malformed nesting never produces a guessed destination.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DelimiterMatch {
    file: FileId,
    revision: SourceRevision,
    language: SymbolLanguage,
    state: DelimiterState,
    selected: ByteRange,
    matching: Option<ByteRange>,
}
impl DelimiterMatch {
    pub const fn file(self) -> FileId { self.file }
    pub const fn revision(self) -> SourceRevision { self.revision }
    pub const fn language(self) -> SymbolLanguage { self.language }
    pub const fn state(self) -> DelimiterState { self.state }
    pub const fn selected_range(self) -> ByteRange { self.selected }
    pub const fn matching_range(self) -> Option<ByteRange> { self.matching }
    pub const fn evidence_level(self) -> &'static str { "lexical-delimiter-candidate" }
}

/// Analyze only the supplied complete observation. `selected` must contain one
/// complete original-source scalar; an ordinary non-bracket scalar reports
/// `NotDelimiter`. Extents, malformed decoding, split scalars and unsupported
/// lexical contexts fail explicitly. The function performs no source I/O and
/// retains no source-sized state after returning.
#[allow(clippy::too_many_arguments)]
pub fn matching_delimiter(source: &[u8], request: CaptureRequest, language: SymbolLanguage,
    encoding: Option<DetectedEncoding>, selected: ByteRange, budget: &ResourceBudget,
    allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
    -> Result<DelimiterMatch, DelimiterError> {
    check(&mut canceled)?;
    if request.range().is_some() { return Err(DelimiterError::InvalidEvidence); }
    if source.len() > MAX_DELIMITER_SOURCE_BYTES { return Err(DelimiterError::SourceLimit); }
    if selected.is_empty() || selected.end().get() > source.len() as u64 {
        return Err(DelimiterError::InvalidSelection);
    }
    // Reserve before either decoder or upstream tokenizer allocates. Mapping
    // spans include Vec growth and old/new overlap, including malformed input
    // which is rejected only after decoding. Three decoded bytes per raw byte
    // covers replacement expansion; accepted decoded text has its own cap.
    // Four token-sized buffers cover upstream spans, its working overlap and
    // the shared JS lexical guard's terminal buffer. No outline tree is built.
    let decoded_capacity = source.len().checked_mul(3).ok_or(DelimiterError::ResourceDenied)?
        .min(MAX_DELIMITER_SOURCE_BYTES).max(1);
    let charge = source.len().checked_add(1)
        .and_then(|n| n.checked_mul(4 * size_of::<MappingSpan>() + 16))
        .and_then(|n| decoded_capacity.checked_mul(4 * size_of::<Span>()).and_then(|m| n.checked_add(m)))
        .and_then(|n| n.checked_add(size_of::<CaptureEncodingMap>() + MAX_DELIMITER_DEPTH + 4096))
        .ok_or(DelimiterError::ResourceDenied)?;
    let _lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
        .map_err(|_| DelimiterError::ResourceDenied)?;
    let encoding = encoding.unwrap_or_else(|| detect_encoding(source));
    if encoding == DetectedEncoding::Unsupported { return Err(DelimiterError::UnsupportedEncoding); }
    let map = CaptureEncodingMap::build_with_encoding(source, encoding)
        .map_err(|_| DelimiterError::UnsupportedEncoding)?;
    check(&mut canceled)?;
    if map.spans().iter().any(|span| matches!(span.kind, SpanKind::ReplacementMalformed | SpanKind::EscapedByte)) {
        return Err(DelimiterError::UnsupportedEncoding);
    }
    let text = map.decoded_text();
    if text.len() > MAX_DELIMITER_SOURCE_BYTES { return Err(DelimiterError::SourceLimit); }
    let decoded = map.byte_range_to_decoded_utf8_range(selected)
        .map_err(|_| DelimiterError::InvalidSelection)?;
    if map.decoded_utf8_range_to_byte_range(decoded).map_err(|_| DelimiterError::InvalidSelection)? != selected {
        return Err(DelimiterError::InvalidSelection);
    }
    let start = usize::try_from(decoded.start().get()).map_err(|_| DelimiterError::InvalidSelection)?;
    let end = usize::try_from(decoded.end().get()).map_err(|_| DelimiterError::InvalidSelection)?;
    let selected_text = text.get(start..end).ok_or(DelimiterError::InvalidSelection)?;
    let mut scalars = selected_text.chars();
    let scalar = scalars.next().ok_or(DelimiterError::InvalidSelection)?;
    if scalars.next().is_some() { return Err(DelimiterError::InvalidSelection); }
    let receipt = |state, matching| DelimiterMatch { file: request.file(), revision: request.revision(),
        language, state, selected, matching };
    if !is_delimiter(scalar) { return Ok(receipt(DelimiterState::NotDelimiter, None)); }

    lexical::check_source(text, language, &mut canceled)?;
    let capacity = text.len().max(1);
    let mut spans = Vec::new();
    spans.try_reserve_exact(capacity).map_err(|_| DelimiterError::ResourceDenied)?;
    if spans.capacity() > capacity { return Err(DelimiterError::ResourceDenied); }
    check(&mut canceled)?;
    highlight_into(language.name(), text, &mut spans);
    check(&mut canceled)?;
    if spans.capacity() > capacity { return Err(DelimiterError::ResourceDenied); }
    lexical::validate(text, language, &spans, &mut canceled)?;
    let selected_token = spans.get(spans.partition_point(|span| span.end <= start))
        .ok_or(DelimiterError::InvalidEvidence)?;
    if selected_token.start > start || selected_token.end < end { return Err(DelimiterError::InvalidEvidence); }
    if selected_token.kind != Tok::Punct { return Ok(receipt(DelimiterState::NotCode, None)); }

    // Iterate only upstream punctuation. Do not scan opaque token contents or
    // resynthesize a token stream from stripped source text.
    let punctuation = spans.iter().filter(|span| span.kind == Tok::Punct).flat_map(|span| {
        text[span.start..span.end].char_indices().map(move |(offset, ch)| (span.start + offset, ch))
    });
    let forward = is_open(scalar);
    let (state, counterpart) = if forward {
        walk(punctuation.filter(|(offset, _)| *offset > start), scalar, true, &mut canceled)?
    } else {
        walk(punctuation.rev().filter(|(offset, _)| *offset < start), scalar, false, &mut canceled)?
    };
    let matching = counterpart.map(|offset| {
        let decoded = DecodedUtf8Range::new(DecodedUtf8Offset::new(offset as u64),
            DecodedUtf8Offset::new(offset as u64 + 1)).map_err(|_| DelimiterError::InvalidEvidence)?;
        map.decoded_utf8_range_to_byte_range(decoded).map_err(|_| DelimiterError::InvalidEvidence)
    }).transpose()?;
    check(&mut canceled)?;
    Ok(receipt(state, matching))
}

fn check(canceled: &mut impl FnMut() -> bool) -> Result<(), DelimiterError> {
    if canceled() { Err(DelimiterError::Canceled) } else { Ok(()) }
}
fn is_open(ch: char) -> bool { matches!(ch, '(' | '[' | '{') }
fn is_delimiter(ch: char) -> bool { matches!(ch, '(' | ')' | '[' | ']' | '{' | '}') }
fn pair(open: char, close: char) -> bool { matches!((open, close), ('(', ')') | ('[', ']') | ('{', '}')) }

fn walk(punctuation: impl Iterator<Item = (usize, char)>, selected: char, forward: bool,
    canceled: &mut impl FnMut() -> bool) -> Result<(DelimiterState, Option<usize>), DelimiterError> {
    let mut stack = [0u8; MAX_DELIMITER_DEPTH];
    stack[0] = selected as u8;
    let mut depth = 1;
    for (ordinal, (offset, ch)) in punctuation.enumerate() {
        if ordinal & 255 == 0 { check(canceled)?; }
        if !is_delimiter(ch) { continue; }
        if is_open(ch) == forward {
            if depth == stack.len() { return Err(DelimiterError::ComplexityLimit); }
            stack[depth] = ch as u8;
            depth += 1;
        } else {
            let top = stack[depth - 1] as char;
            if !(if forward { pair(top, ch) } else { pair(ch, top) }) {
                return Ok((DelimiterState::Mismatched, None));
            }
            depth -= 1;
            if depth == 0 { return Ok((DelimiterState::Matched, Some(offset))); }
        }
    }
    check(canceled)?;
    Ok((DelimiterState::Unmatched, None))
}
