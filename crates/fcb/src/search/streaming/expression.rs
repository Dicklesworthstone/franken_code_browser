#![forbid(unsafe_code)]

//! Content predicates over the same observation as the retained primary hits.
//! This module has no reader, source provider, path lookup, or publication route.

use super::*;
use fcb_search::{ParsedQuery, MAX_QUERY_LEN, MAX_QUERY_TOKENS};

pub(super) struct ExpressionNeedle {
    terms: Vec<PredicateNeedle>,
}

struct PredicateNeedle {
    excluded: bool,
    utf8: Vec<u8>,
    little: Vec<u8>,
    big: Vec<u8>,
}

impl PredicateNeedle {
    fn new(text: String, excluded: bool) -> Result<Self, StreamReadError> {
        let capacity = text.encode_utf16().count() * 2;
        let mut little = reserve(capacity)?;
        let mut big = reserve(capacity)?;
        for unit in text.encode_utf16() {
            little.extend_from_slice(&unit.to_le_bytes());
            big.extend_from_slice(&unit.to_be_bytes());
        }
        Ok(Self { excluded, utf8: text.into_bytes(), little, big })
    }

    fn encoded(&self, encoding: DetectedEncoding) -> &[u8] {
        match encoding {
            DetectedEncoding::Utf16Le => &self.little,
            DetectedEncoding::Utf16Be => &self.big,
            _ => &self.utf8,
        }
    }
}

impl StreamingNeedle {
    /// Compile the bounded parser's content expression for one continuous read.
    ///
    /// The primary term supplies retained occurrence witnesses. Every additional
    /// positive term must occur and every excluded term must be absent from the
    /// SAME observed input. All terms share the exact-text or Unicode-lowercase
    /// semantics of text_matching; phrases and overlaps retain that meaning.
    ///
    /// The caller must apply path_filters and lang_filters to its authorized
    /// file manifest BEFORE opening/admitting readers. This content matcher has
    /// no path, language metadata, or filesystem authority.
    ///
    /// Public AST fields are revalidated against raw_query under the parser's
    /// byte/token limits. Construct a changed query by parsing its changed text.
    /// Candidate hits stay private until the declared input completes and all
    /// content predicates qualify. Even non-counting readers scan and count the
    /// whole observation; max_matches limits retained witnesses only.
    pub fn expression(owner: ArenaOwnerId, query: &ParsedQuery, case_sensitive: bool,
        budget: &ResourceBudget, allocation: ResourceAllocationId) -> Result<Self, StreamReadError> {
        if query.raw_query.len() > MAX_QUERY_LEN || query.raw_query.is_empty() {
            return Err(StreamReadError::InvalidExpression);
        }
        // Reserve before reparsing. This includes parser/string growth and peak
        // overlap with all encoded predicates and the retained primary pattern;
        // its size depends only on the bounded query, never on source length.
        let charge = size_of::<Self>() + size_of::<ParsedQuery>() + size_of::<ExpressionNeedle>()
            + MAX_QUERY_TOKENS * (8 * size_of::<String>() + size_of::<PredicateNeedle>())
            + MAX_QUERY_LEN * 32;
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| StreamReadError::ResourceDenied)?;
        let parsed = ParsedQuery::parse(&query.raw_query).map_err(|_| StreamReadError::InvalidExpression)?;
        if parsed != *query { return Err(StreamReadError::InvalidExpression); }
        let mut needle = Self::build_reserved(owner, parsed.primary_needle.as_bytes(),
            Some(&parsed.primary_needle), lease)?;
        needle.set_case_sensitive(case_sensitive)?;
        let mut terms = reserve(parsed.conjunction_terms.len() + parsed.exclusion_terms.len())?;
        for text in parsed.conjunction_terms {
            terms.push(PredicateNeedle::new(text, false)?);
        }
        for text in parsed.exclusion_terms {
            terms.push(PredicateNeedle::new(text, true)?);
        }
        needle.expression = Some(ExpressionNeedle { terms });
        Ok(needle)
    }

    pub const fn is_expression(&self) -> bool { self.expression.is_some() }
}

enum PredicateCursor<'needle> {
    Exact(ByteSearchCursor<'needle>),
    Lowercase(NormalizedCursor),
}

struct Predicate<'needle> {
    excluded: bool,
    seen: bool,
    cursor: PredicateCursor<'needle>,
}

pub(super) struct ExpressionWork<'needle> {
    terms: Vec<Predicate<'needle>>,
}

impl<'needle> ExpressionWork<'needle> {
    pub(super) fn charge() -> usize {
        // Lowercase tables may expand a scalar. Match the shared cursor's
        // twelve-original-bytes-per-query-byte admission bound, with geometric
        // vector growth and peak reallocation overlap conservatively included.
        size_of::<Self>() + MAX_QUERY_TOKENS * size_of::<Predicate<'needle>>()
            + MAX_QUERY_LEN * 12 * (2 * size_of::<char>() + size_of::<usize>() + size_of::<SourceScalar>())
            + size_of::<fcb_search::stream::ByteMatch>()
    }

    pub(super) fn new(needle: &'needle ExpressionNeedle, mode: StreamingMode,
        encoding: DetectedEncoding) -> Result<Self, StreamReadError> {
        let mut terms = reserve(needle.terms.len())?;
        for term in &needle.terms {
            let cursor = match mode {
                StreamingMode::ExactText => PredicateCursor::Exact(ByteSearchCursor::new(term.encoded(encoding))?),
                StreamingMode::LowercaseText => {
                    let text = std::str::from_utf8(&term.utf8).map_err(|_| StreamReadError::InvalidExpression)?;
                    let bound = matched_text_capacity(text, lowercase_mode())
                        .map_err(|_| StreamReadError::InvalidExpression)?;
                    if bound > text.len() * 12 { return Err(StreamReadError::InvalidExpression); }
                    PredicateCursor::Lowercase(NormalizedCursor::new(text, lowercase_mode())
                        .map_err(|_| StreamReadError::AllocationFailed)?.ok_or(StreamReadError::InvalidExpression)?)
                }
                _ => return Err(StreamReadError::InvalidExpression),
            };
            terms.push(Predicate { excluded: term.excluded, seen: false, cursor });
        }
        Ok(Self { terms })
    }

    /// Feed precisely the validated raw prefix consumed by the primary cursor.
    /// A false return means cancellation, never a negative predicate result.
    pub(super) fn feed_exact(&mut self, bytes: &[u8], header: u64,
        encoding: Option<DetectedEncoding>, canceled: &mut impl FnMut() -> bool) -> Result<bool, StreamReadError> {
        for term in &mut self.terms {
            if canceled() { return Ok(false); }
            if term.seen { continue; }
            let PredicateCursor::Exact(cursor) = &mut term.cursor else {
                return Err(StreamReadError::InvalidExpression);
            };
            let mut consumed = 0;
            while consumed < bytes.len() && !term.seen {
                let batch = cursor.step(&bytes[consumed..], bytes.len() - consumed, 1)?;
                if batch.consumed == 0 { return Err(StreamReadError::InvalidRange); }
                consumed += batch.consumed;
                term.seen = batch.hits.iter().any(|hit| hit.start >= header
                    && (!encoding.is_some_and(|encoding| encoding.is_utf16()) || hit.start % 2 == 0));
            }
        }
        Ok(true)
    }

    /// Called once per validated source scalar, including a scalar whose primary
    /// lowercase expansion will finish in the next hit-limited work quantum.
    pub(super) fn feed_scalar(&mut self, scalar: SourceScalar) -> Result<(), StreamReadError> {
        for term in &mut self.terms {
            if term.seen { continue; }
            let PredicateCursor::Lowercase(cursor) = &mut term.cursor else {
                return Err(StreamReadError::InvalidExpression);
            };
            for unit in cursor.units(scalar.ch) {
                if cursor.push(unit, scalar).is_some() {
                    term.seen = true;
                    break;
                }
            }
        }
        Ok(())
    }

    pub(super) fn qualifies(&self) -> bool {
        self.terms.iter().all(|term| term.seen != term.excluded)
    }
}
