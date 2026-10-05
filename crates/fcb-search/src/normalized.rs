#![forbid(unsafe_code)]

//! Incremental matching for the existing, explicitly limited normalization
//! repertoire. Source decoding stays in fcb-source and text.rs. Only a needle's
//! worth of contributing source scalars survives a decoding window.

use std::collections::VecDeque;

use crate::{QueryError, SearchMode, UnicodeNormalization, canonical_decompose_char};

/// One original scalar. Expansions may contribute several matching units with
/// this same provenance; their occurrence identities must remain distinct.
#[derive(Clone, Copy, Debug)]
pub(super) struct SourceScalar {
    pub ch: char,
    pub raw_start: u64,
    pub raw_end: u64,
    pub decoded_start: u64,
    pub decoded_end: u64,
}

#[derive(Clone, Copy, Debug)]
pub(super) struct Occurrence {
    pub raw_start: u64,
    pub raw_end: u64,
    pub decoded_start: u64,
    pub decoded_end: u64,
}

#[derive(Clone, Copy, Debug)]
enum Transform {
    Lower,
    Fold,
    Canonical { case_sensitive: bool },
}

impl Transform {
    fn units(self, ch: char) -> Units {
        match self {
            Self::Fold if matches!(ch, 'ß' | 'ẞ') => Units::Expansion(&['s', 's']),
            Self::Lower | Self::Fold => Units::Lower(ch.to_lowercase()),
            Self::Canonical { .. } => match canonical_decompose_char(ch) {
                Some(chars) => Units::Expansion(chars),
                None => Units::Single(Some(ch)),
            },
        }
    }

    fn equal(self, left: char, right: char) -> bool {
        match self {
            // Preserve the existing canonical comparison contract: a lowered
            // expansion is one comparison key, not extra canonical occurrences.
            Self::Canonical { case_sensitive: false } => left.to_lowercase().eq(right.to_lowercase()),
            _ => left == right,
        }
    }
}

/// An allocation-free scalar transform. Using ToLowercase directly avoids
/// assuming a fixed maximum expansion length for future toolchain tables.
pub(super) enum Units {
    Single(Option<char>),
    Expansion(&'static [char]),
    Lower(std::char::ToLowercase),
}

impl Iterator for Units {
    type Item = char;

    fn next(&mut self) -> Option<char> {
        match self {
            Self::Single(ch) => ch.take(),
            Self::Expansion(chars) => {
                let (&first, rest) = chars.split_first()?;
                *chars = rest;
                Some(first)
            }
            Self::Lower(chars) => chars.next(),
        }
    }
}

/// KMP over transformed scalar units. The failure table and source ring are
/// O(transformed needle length); no source-sized normalized string is built.
pub(super) struct NormalizedCursor {
    transform: Transform,
    needle: Vec<char>,
    failure: Vec<usize>,
    matched: usize,
    source: VecDeque<SourceScalar>,
}

impl NormalizedCursor {
    pub fn new(query: &str, mode: SearchMode) -> Result<Option<Self>, QueryError> {
        let transform = match mode {
            SearchMode::RawBytes | SearchMode::DecodedText {
                case_sensitive: true, normalization: UnicodeNormalization::Exact,
            } => return Ok(None),
            SearchMode::DecodedText { normalization: UnicodeNormalization::CaseFold, .. } => Transform::Fold,
            SearchMode::DecodedText { normalization: UnicodeNormalization::Canonical, case_sensitive } =>
                Transform::Canonical { case_sensitive },
            SearchMode::DecodedText { .. } => Transform::Lower,
        };
        crate::validate_needle(query.as_bytes())?;
        let mut needle = Vec::new();
        // Bound transformed query storage independently of toolchain expansion
        // tables. The source is never subject to a whole-file admission ceiling.
        let max_units = crate::stream::MAX_STREAM_NEEDLE_BYTES.checked_mul(3)
            .ok_or(QueryError::LimitExceeded)?;
        for ch in query.chars().flat_map(|ch| transform.units(ch)) {
            if needle.len() == max_units { return Err(QueryError::NeedleTooLong); }
            needle.try_reserve(1).map_err(|_| QueryError::LimitExceeded)?;
            needle.push(ch);
        }
        if needle.is_empty() { return Err(QueryError::EmptyNeedle); }
        let mut failure = Vec::new();
        failure.try_reserve_exact(needle.len()).map_err(|_| QueryError::LimitExceeded)?;
        failure.resize(needle.len(), 0);
        let mut prefix = 0;
        for index in 1..needle.len() {
            while prefix > 0 && !transform.equal(needle[index], needle[prefix]) {
                prefix = failure[prefix - 1];
            }
            if transform.equal(needle[index], needle[prefix]) { prefix += 1; }
            failure[index] = prefix;
        }
        let mut source = VecDeque::new();
        source.try_reserve_exact(needle.len()).map_err(|_| QueryError::LimitExceeded)?;
        Ok(Some(Self { transform, needle, failure, matched: 0, source }))
    }

    pub fn units(&self, ch: char) -> Units { self.transform.units(ch) }

    /// Advance exactly one transformed unit. Rejected cross-chunk occurrences
    /// still advance KMP, but the caller filters them before counting/capping.
    pub fn push(&mut self, unit: char, source: SourceScalar) -> Option<Occurrence> {
        if self.source.len() == self.needle.len() { self.source.pop_front(); }
        self.source.push_back(source);
        while self.matched > 0 && !self.transform.equal(unit, self.needle[self.matched]) {
            self.matched = self.failure[self.matched - 1];
        }
        if self.transform.equal(unit, self.needle[self.matched]) { self.matched += 1; }
        if self.matched != self.needle.len() { return None; }
        self.matched = self.failure[self.matched - 1];
        let first = self.source.front()?;
        Some(Occurrence { raw_start: first.raw_start, raw_end: source.raw_end,
            decoded_start: first.decoded_start, decoded_end: source.decoded_end })
    }

    /// Call only after an accepted occurrence. Copy each contributing original
    /// scalar once, even when only part of its normalized expansion matched.
    pub fn matched_text(&self) -> Result<String, QueryError> {
        let mut length = 0usize;
        let mut previous = None;
        for scalar in &self.source {
            if previous != Some(scalar.decoded_start) {
                length = length.checked_add(scalar.ch.len_utf8()).ok_or(QueryError::LimitExceeded)?;
                previous = Some(scalar.decoded_start);
            }
        }
        let mut text = String::new();
        text.try_reserve_exact(length).map_err(|_| QueryError::LimitExceeded)?;
        previous = None;
        for scalar in &self.source {
            if previous != Some(scalar.decoded_start) {
                text.push(scalar.ch);
                previous = Some(scalar.decoded_start);
            }
        }
        Ok(text)
    }

    pub fn oldest_raw_start(&self) -> Option<u64> { self.source.front().map(|scalar| scalar.raw_start) }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(text: &str, query: &str, mode: SearchMode) -> Vec<(u64, u64, String)> {
        let mut cursor = NormalizedCursor::new(query, mode).unwrap().unwrap();
        let mut hits = Vec::new();
        for (start, ch) in text.char_indices() {
            let end = (start + ch.len_utf8()) as u64;
            let scalar = SourceScalar { ch, raw_start: start as u64, raw_end: end,
                decoded_start: start as u64, decoded_end: end };
            for unit in cursor.units(ch) {
                if let Some(hit) = cursor.push(unit, scalar) {
                    hits.push((hit.raw_start, hit.raw_end, cursor.matched_text().unwrap()));
                }
                assert!(cursor.source.len() <= cursor.needle.len());
            }
        }
        hits
    }

    #[test]
    fn expansion_occurrences_keep_whole_source_units_and_overlap() {
        let mode = SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::CaseFold };
        assert_eq!(run("ßß", "sss", mode), vec![(0, 4, "ßß".into()), (0, 4, "ßß".into())]);
        assert_eq!(run("ß", "s", mode), vec![(0, 2, "ß".into()), (0, 2, "ß".into())]);
    }

    #[test]
    fn canonical_case_insensitive_keys_do_not_flatten_lowercase_expansions() {
        let mode = SearchMode::DecodedText { case_sensitive: false, normalization: UnicodeNormalization::Canonical };
        assert!(run("İ", "i", mode).is_empty());
        assert!(run("İ", "i\u{0307}", mode).is_empty());
        assert_eq!(run("Ée\u{0301}", "é", mode), vec![(0, 2, "É".into()), (2, 5, "e\u{0301}".into())]);
    }

    #[test]
    fn streaming_units_agree_with_independent_whole_text_oracles() {
        let corpus = ["", "banana", "ßßS", "Straße STRASSE straẞe", "K k İ i\u{0307}",
            "É é e\u{0301} E\u{0301}", "🙂É🙂é", "aaaaababababab"];
        let needles = ["ana", "s", "ss", "sss", "strasse", "k", "İ", "i\u{0307}",
            "é", "e", "\u{0301}", "🙂é", "ababab", "absent"];
        for normalization in [UnicodeNormalization::Exact, UnicodeNormalization::CaseFold, UnicodeNormalization::Canonical] {
            for case_sensitive in [false, true] {
                if normalization == UnicodeNormalization::Exact && case_sensitive { continue; }
                let mode = SearchMode::DecodedText { case_sensitive, normalization };
                for text in corpus {
                    for query in needles {
                        let expected = match normalization {
                            UnicodeNormalization::Exact => crate::find_case_insensitive_substrings(text, query, usize::MAX).unwrap(),
                            UnicodeNormalization::CaseFold => crate::find_unicode_folded_substrings(text, query, usize::MAX).unwrap(),
                            UnicodeNormalization::Canonical => crate::find_canonical_equivalent_substrings(text, query, case_sensitive, usize::MAX),
                        };
                        let expected: Vec<_> = expected.into_iter().map(|hit|
                            (hit.start as u64, hit.end as u64, text[hit.start..hit.end].to_owned())).collect();
                        assert_eq!(run(text, query, mode), expected, "{mode:?}: {text:?} / {query:?}");
                    }
                }
            }
        }
    }
}
