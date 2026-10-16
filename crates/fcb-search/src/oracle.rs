#![forbid(unsafe_code)]

//! Reference capture scans and query-filter evaluation (FCB-026.B / fcb-8ii.2).
//!
//! All content probes and primary scans share the operation's raw-byte budget.
//! Path/language filters cost no source bytes. A partial probe cannot establish
//! absence, and unavailable decoding never becomes a successful negative result.
//! Hit identity includes the source revision and per-capture occurrence ID.

use fcb_core::FileId;
use fcb_source::CompleteCapture;

use crate::{
    DirectSourceScanner, LangFilterKind, ParsedQuery, PathFilterKind, QueryError, QueryOptions,
    SearchCoverage, SearchResult,
};

/// A document in the caller's immutable, authorized capture universe.
#[derive(Clone, Debug)]
pub struct SearchDocument<'a> {
    pub file_id: FileId,
    pub path: &'a str,
    pub capture: &'a CompleteCapture,
}

impl<'a> SearchDocument<'a> {
    pub fn new(file_id: FileId, path: &'a str, capture: &'a CompleteCapture) -> Self {
        Self { file_id, path, capture }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OracleMismatchError {
    pub reason: String,
}

impl std::fmt::Display for OracleMismatchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Oracle mismatch: {}", self.reason)
    }
}
impl std::error::Error for OracleMismatchError {}

pub struct ReferenceScanOracle;

impl ReferenceScanOracle {
    pub fn matches_path_filters(path: &str, filters: &[PathFilterKind]) -> bool {
        filters.iter().all(|filter| {
            let (pattern, include) = match filter {
                PathFilterKind::Include(pattern) => (pattern, true),
                PathFilterKind::Exclude(pattern) => (pattern, false),
            };
            // Fail closed even for callers using this predicate directly.
            !pattern.is_empty() && pattern.len() <= crate::MAX_QUERY_LEN
                && path_matches(path, pattern) == include
        })
    }

    pub fn matches_lang_filters(path: &str, filters: &[LangFilterKind]) -> bool {
        // A dot in a directory name does not give an extension to its children.
        let filename = path.rsplit('/').next().unwrap_or(path);
        let ext = filename.rsplit_once('.').map_or("", |(_, ext)| ext).to_lowercase();
        filters.iter().all(|filter| match filter {
            LangFilterKind::Include(lang) => Self::lang_matches(&ext, lang),
            LangFilterKind::Exclude(lang) => !Self::lang_matches(&ext, lang),
        })
    }

    pub fn scan_document(
        doc: &SearchDocument<'_>, query: &ParsedQuery, options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        Self::scan_document_with_cancel(doc, query, options, || false)
    }

    /// Evaluate filters and the primary needle under one budget and cancellation
    /// scope. Predicate occurrences never multiply the primary anchor hits.
    pub fn scan_document_with_cancel(
        doc: &SearchDocument<'_>, query: &ParsedQuery, options: &QueryOptions,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<SearchResult, QueryError> {
        validate_query(query)?;
        if doc.file_id != doc.capture.request().file() {
            return Err(QueryError::InvalidRange);
        }
        let mut result = SearchResult::empty(SearchCoverage::Exhaustive);
        if canceled() {
            result.coverage = SearchCoverage::CanceledEarly;
            return Ok(result);
        }
        if !Self::matches_path_filters(doc.path, &query.path_filters)
            || !Self::matches_lang_filters(doc.path, &query.lang_filters) {
            return Ok(result);
        }
        if options.max_matches == 0 && !doc.capture.bytes().is_empty() {
            result.coverage = SearchCoverage::TruncatedAtLimit { max_matches: 0 };
            return Ok(result);
        }

        let terms = query.exclusion_terms.iter().map(|term| (term, false))
            .chain(query.conjunction_terms.iter().map(|term| (term, true)));
        for (term, required) in terms {
            let probe_options = remaining_options(options, result.scanned_bytes, 1);
            let mut probe = DirectSourceScanner::scan_complete_capture_with_cancel(
                doc.capture, term, &probe_options, &mut canceled,
            )?;
            let scanned = add_scanned(result.scanned_bytes, probe.scanned_bytes, options)?;
            let found = probe.match_count() > 0;
            let complete = probe.is_complete();
            if !probe.unsupported_files.is_empty()
                || matches!(probe.coverage, SearchCoverage::CanceledEarly) {
                probe.matches.clear();
                probe.total_matches_counted = 0;
                set_scanned(&mut probe, scanned);
                return Ok(probe);
            }
            result.scanned_bytes = scanned;
            if (found && !required) || (!found && required && complete) {
                // One excluded occurrence, or proven absence of a required
                // term, establishes ineligibility even without a primary scan.
                return Ok(result);
            }
            if !found && !complete {
                // Neither a missing conjunction nor absence of an exclusion
                // can be inferred from an unsearched suffix.
                probe.matches.clear();
                probe.total_matches_counted = 0;
                set_scanned(&mut probe, scanned);
                return Ok(probe);
            }
        }

        let primary_options = remaining_options(options, result.scanned_bytes, options.max_matches);
        let mut primary = DirectSourceScanner::scan_complete_capture_with_cancel(
            doc.capture, &query.primary_needle, &primary_options, &mut canceled,
        )?;
        let scanned = add_scanned(result.scanned_bytes, primary.scanned_bytes, options)?;
        set_scanned(&mut primary, scanned);
        Ok(primary)
    }

    /// Search the supplied closed capture universe with a global byte budget.
    /// A full result buffer is not itself proof of truncation: continue with a
    /// bounded lookahead until another occurrence is found or the scope ends.
    /// Source-qualified hit identities are preserved across documents.
    pub fn scan_collection(
        docs: &[SearchDocument<'_>], query: &ParsedQuery, options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        Self::scan_collection_with_cancel(docs, query, options, || false)
    }

    pub fn scan_collection_with_cancel(
        docs: &[SearchDocument<'_>], query: &ParsedQuery, options: &QueryOptions,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<SearchResult, QueryError> {
        validate_query(query)?;
        let mut result = SearchResult::empty(SearchCoverage::Exhaustive);
        if canceled() {
            result.coverage = SearchCoverage::CanceledEarly;
            return Ok(result);
        }
        for doc in docs {
            let remaining_hits = options.max_matches.saturating_sub(result.matches.len());
            let doc_limit = if options.max_matches == 0 { 0 } else { remaining_hits.max(1) };
            let doc_options = remaining_options(options, result.scanned_bytes, doc_limit);
            let child = Self::scan_document_with_cancel(doc, query, &doc_options, &mut canceled)?;
            result.scanned_bytes = add_scanned(result.scanned_bytes, child.scanned_bytes, options)?;
            result.unsupported_files.try_reserve(child.unsupported_files.len())
                .map_err(|_| QueryError::LimitExceeded)?;
            result.unsupported_files.extend(child.unsupported_files);
            // Count only the one extra occurrence needed to prove truncation,
            // even when a document-local lookahead observed more occurrences.
            let counted = child.total_matches_counted.min(remaining_hits.saturating_add(1));
            result.total_matches_counted = result.total_matches_counted.checked_add(counted)
                .ok_or(QueryError::LimitExceeded)?;
            let retained = child.matches.len().min(remaining_hits);
            result.matches.try_reserve_exact(retained).map_err(|_| QueryError::LimitExceeded)?;
            result.matches.extend(child.matches.into_iter().take(retained));

            if matches!(child.coverage, SearchCoverage::CanceledEarly) {
                result.coverage = SearchCoverage::CanceledEarly;
                break;
            }
            if result.total_matches_counted > options.max_matches
                || matches!(child.coverage, SearchCoverage::TruncatedAtLimit { .. }) {
                result.coverage = SearchCoverage::TruncatedAtLimit { max_matches: options.max_matches };
                break;
            }
            if matches!(child.coverage, SearchCoverage::BudgetExhausted { .. }) {
                result.coverage = SearchCoverage::BudgetExhausted { bytes_scanned: result.scanned_bytes };
                break;
            }
        }
        Ok(result)
    }

    /// Compare semantic search results, including immutable source identity.
    /// Scan byte counts may differ between an optimized route and the oracle;
    /// completeness, counted hits and exact hit provenance may not differ.
    pub fn verify_oracle_match(
        candidate: &SearchResult, oracle: &SearchResult,
    ) -> Result<(), OracleMismatchError> {
        if candidate.matches.len() != oracle.matches.len() {
            return mismatch("Match count mismatch");
        }
        if candidate.total_matches_counted != oracle.total_matches_counted {
            return mismatch("Total matches counted mismatch");
        }
        if candidate.coverage != oracle.coverage {
            return mismatch("Coverage mismatch");
        }
        if candidate.unsupported_files != oracle.unsupported_files {
            return mismatch("Unsupported files mismatch");
        }
        for (index, (actual, expected)) in candidate.matches.iter().zip(&oracle.matches).enumerate() {
            let field = if actual.file_id != expected.file_id {
                "file_id"
            } else if actual.revision != expected.revision {
                "source revision"
            } else if actual.occurrence_id != expected.occurrence_id {
                "occurrence identity"
            } else if actual.original_byte_range != expected.original_byte_range {
                "byte range"
            } else if actual.decoded_range != expected.decoded_range {
                "decoded range"
            } else if actual.matched_text != expected.matched_text {
                "matched text"
            } else if actual.multiplicity != expected.multiplicity {
                "multiplicity"
            } else {
                continue;
            };
            return Err(OracleMismatchError { reason: format!("Match {index} {field} mismatch") });
        }
        Ok(())
    }

    fn lang_matches(ext: &str, lang: &str) -> bool {
        let lang = lang.to_lowercase();
        match lang.as_str() {
            "rust" | "rs" => ext == "rs",
            "python" | "py" => ext == "py",
            "javascript" | "js" => ext == "js",
            "typescript" | "ts" => ext == "ts",
            "c" => ext == "c" || ext == "h",
            "cpp" | "cxx" | "cc" => ext == "cpp" || ext == "cxx" || ext == "cc" || ext == "hpp",
            "markdown" | "md" => ext == "md" || ext == "markdown",
            other => ext == other,
        }
    }
}

/// Match only the documented wildcard subset. Non-glob filters keep their
/// existing substring semantics; slash-free globs match the final component.
/// `*` and `?` do not cross `/`; a whole-component `**` can cross directories,
/// and `**/` includes zero directories. Backslashes/brackets/braces are literal.
fn path_matches(path: &str, pattern: &str) -> bool {
    if pattern.is_empty() || pattern.len() > crate::MAX_QUERY_LEN {
        return false;
    }
    if !pattern.contains(['*', '?']) {
        return path.contains(pattern);
    }
    let path = if pattern.contains('/') {
        path
    } else {
        path.rsplit('/').next().unwrap_or(path)
    };
    // Common extension filters should not pay for the general state machine.
    if let Some(suffix) = pattern.strip_prefix('*') {
        if !suffix.contains(['*', '?', '/']) {
            return path.ends_with(suffix);
        }
    }

    // NFA simulation: bounded stack storage and O(path scalars * pattern bytes)
    // work, with no recursion, backtracking or per-candidate heap allocation.
    // Positions are UTF-8 boundaries, so `?` consumes a scalar, not one byte.
    let mut active = [false; crate::MAX_QUERY_LEN + 1];
    active[0] = true;
    close_glob_states(pattern, &mut active, true);
    for ch in path.chars() {
        let mut next_active = [false; crate::MAX_QUERY_LEN + 1];
        let mut offset = 0;
        while offset < pattern.len() {
            let (token, next) = glob_token(pattern, offset);
            if active[offset] {
                match token {
                    GlobToken::Literal(expected) if ch == expected => next_active[next] = true,
                    GlobToken::One if ch != '/' => next_active[next] = true,
                    GlobToken::Star if ch != '/' => next_active[offset] = true,
                    GlobToken::Any | GlobToken::Directories => next_active[offset] = true,
                    _ => {}
                }
            }
            offset = next;
        }
        close_glob_states(pattern, &mut next_active, ch == '/');
        active = next_active;
    }
    active[pattern.len()]
}

#[derive(Clone, Copy)]
enum GlobToken { Literal(char), One, Star, Any, Directories }

fn glob_token(pattern: &str, offset: usize) -> (GlobToken, usize) {
    let ch = pattern[offset..].chars().next().expect("offset is before pattern end");
    if ch == '?' {
        return (GlobToken::One, offset + 1);
    }
    if ch != '*' {
        return (GlobToken::Literal(ch), offset + ch.len_utf8());
    }
    let bytes = pattern.as_bytes();
    let mut end = offset + 1;
    while end < bytes.len() && bytes[end] == b'*' { end += 1; }
    let component_start = offset == 0 || bytes[offset - 1] == b'/';
    if end - offset >= 2 && component_start {
        if end == bytes.len() { return (GlobToken::Any, end); }
        if bytes[end] == b'/' { return (GlobToken::Directories, end + 1); }
    }
    (GlobToken::Star, end)
}

fn close_glob_states(pattern: &str, active: &mut [bool; crate::MAX_QUERY_LEN + 1],
    component_start: bool)
{
    let mut offset = 0;
    while offset < pattern.len() {
        let (token, next) = glob_token(pattern, offset);
        if active[offset] && (matches!(token, GlobToken::Star | GlobToken::Any)
            || (matches!(token, GlobToken::Directories) && component_start)) {
            active[next] = true;
        }
        offset = next;
    }
}

fn mismatch(reason: &str) -> Result<(), OracleMismatchError> {
    Err(OracleMismatchError { reason: reason.to_owned() })
}

fn remaining_options(options: &QueryOptions, scanned: u64, max_matches: usize) -> QueryOptions {
    let mut remaining = options.clone();
    remaining.max_matches = max_matches;
    remaining.max_bytes_scanned = options.max_bytes_scanned.map(|limit| limit.saturating_sub(scanned));
    remaining
}

fn add_scanned(previous: u64, additional: u64, options: &QueryOptions) -> Result<u64, QueryError> {
    let total = previous.checked_add(additional).ok_or(QueryError::LimitExceeded)?;
    if options.max_bytes_scanned.is_some_and(|limit| total > limit) {
        return Err(QueryError::LimitExceeded);
    }
    Ok(total)
}

fn set_scanned(result: &mut SearchResult, scanned: u64) {
    result.scanned_bytes = scanned;
    if matches!(result.coverage, SearchCoverage::BudgetExhausted { .. }) {
        result.coverage = SearchCoverage::BudgetExhausted { bytes_scanned: scanned };
    }
}

fn validate_query(query: &ParsedQuery) -> Result<(), QueryError> {
    crate::validate_needle(query.primary_needle.as_bytes())?;
    // Public AST fields can be constructed without calling ParsedQuery::parse.
    let terms = query.conjunction_terms.len().checked_add(query.exclusion_terms.len())
        .and_then(|n| n.checked_add(query.path_filters.len()))
        .and_then(|n| n.checked_add(query.lang_filters.len()))
        .and_then(|n| n.checked_add(1)).ok_or(QueryError::LimitExceeded)?;
    if terms > crate::MAX_QUERY_TOKENS { return Err(QueryError::LimitExceeded); }
    for term in query.conjunction_terms.iter().chain(&query.exclusion_terms) {
        crate::validate_needle(term.as_bytes())?;
    }
    let paths = query.path_filters.iter().map(|filter| match filter {
        PathFilterKind::Include(value) | PathFilterKind::Exclude(value) => value,
    });
    let languages = query.lang_filters.iter().map(|filter| match filter {
        LangFilterKind::Include(value) | LangFilterKind::Exclude(value) => value,
    });
    for value in paths.chain(languages) {
        if value.is_empty() { return Err(QueryError::SyntaxError); }
        if value.len() > crate::MAX_QUERY_LEN { return Err(QueryError::NeedleTooLong); }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use super::*;
    use fcb_core::{ArenaOwnerId, ByteLength, QueryGeneration, SourceRevision};
    use fcb_source::{CaptureRequest, DetectedEncoding};
    use crate::SearchMode;

    fn options() -> QueryOptions {
        let owner = ArenaOwnerId::new(1).unwrap();
        QueryOptions::new(QueryGeneration::new(owner, 1).unwrap()).with_mode(SearchMode::RawBytes)
    }

    fn capture(id: u64, bytes: &[u8]) -> CompleteCapture {
        let owner = ArenaOwnerId::new(1).unwrap();
        let request = CaptureRequest::new(FileId::new(owner, id).unwrap(),
            SourceRevision::new(owner, 1).unwrap()).unwrap();
        CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
    }

    fn doc(capture: &CompleteCapture) -> SearchDocument<'_> {
        SearchDocument::new(capture.request().file(), "src/main.rs", capture)
    }

    #[test]
    fn collection_shares_one_byte_budget_across_captures() {
        let first = capture(1, b"abc");
        let second = capture(2, b"abca");
        let mut options = options();
        options.max_bytes_scanned = Some(4);
        let result = ReferenceScanOracle::scan_collection(&[doc(&first), doc(&second)],
            &ParsedQuery::parse("a").unwrap(), &options).unwrap();
        assert_eq!(result.scanned_bytes, 4);
        assert_eq!(result.match_count(), 2);
        assert_eq!(result.matches[1].file_id, second.request().file());
        assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: 4 });
    }

    #[test]
    fn content_filters_and_primary_share_the_document_budget() {
        let source = capture(1, b"needle required");
        let mut options = options();
        options.max_bytes_scanned = Some(source.bytes().len() as u64 + 3);
        let result = ReferenceScanOracle::scan_document(&doc(&source),
            &ParsedQuery::parse("needle required").unwrap(), &options).unwrap();
        assert!(result.matches.is_empty());
        assert_eq!(result.scanned_bytes, source.bytes().len() as u64 + 3);
        assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: result.scanned_bytes });
    }

    #[test]
    fn incomplete_predicates_never_become_complete_rejections_or_acceptances() {
        let source = capture(1, b"needle required excluded");
        let mut options = options();
        options.max_bytes_scanned = Some(6);
        for query in ["needle required", "needle -excluded"] {
            let result = ReferenceScanOracle::scan_document(&doc(&source),
                &ParsedQuery::parse(query).unwrap(), &options).unwrap();
            assert!(result.matches.is_empty());
            assert!(!result.is_complete());
            assert_eq!(result.coverage, SearchCoverage::BudgetExhausted { bytes_scanned: 6 });
        }
    }

    #[test]
    fn unavailable_text_predicate_preserves_coverage_and_declared_encoding() {
        let source = capture(1, b"needle required");
        let options = options().with_mode(SearchMode::default()).with_encoding(DetectedEncoding::Unsupported);
        let result = ReferenceScanOracle::scan_collection(&[doc(&source)],
            &ParsedQuery::parse("needle required").unwrap(), &options).unwrap();
        assert!(!result.is_complete());
        assert_eq!(result.unsupported_files, [source.request().file()]);
        assert_eq!(result.match_count(), 0);
    }

    #[test]
    fn exact_global_limit_can_be_complete_even_with_later_nonmatching_documents() {
        let first = capture(1, b"a-a");
        let second = capture(2, b"nothing here");
        let options = options().with_max_matches(2);
        let query = ParsedQuery::parse("a").unwrap();
        let result = ReferenceScanOracle::scan_collection(&[doc(&first), doc(&second)], &query, &options).unwrap();
        assert!(result.is_complete());
        assert_eq!(result.match_count(), 2);
        let third = capture(3, b"aaa");
        let result = ReferenceScanOracle::scan_collection(&[doc(&first), doc(&third)], &query, &options).unwrap();
        assert_eq!(result.matches.len(), 2);
        assert_eq!(result.match_count(), 3);
        assert_eq!(result.coverage, SearchCoverage::TruncatedAtLimit { max_matches: 2 });
    }

    #[test]
    fn zero_limit_and_cancellation_do_not_scan_source() {
        let source = capture(1, b"needle required");
        let query = ParsedQuery::parse("needle required").unwrap();
        let options = options().with_max_matches(0);
        let result = ReferenceScanOracle::scan_collection(&[doc(&source)], &query, &options).unwrap();
        assert_eq!(result.scanned_bytes, 0);
        assert!(result.matches.is_empty());
        assert_eq!(result.coverage, SearchCoverage::TruncatedAtLimit { max_matches: 0 });
        let result = ReferenceScanOracle::scan_collection_with_cancel(&[doc(&source)], &query, &options, || true).unwrap();
        assert_eq!(result.scanned_bytes, 0);
        assert_eq!(result.coverage, SearchCoverage::CanceledEarly);
    }

    #[test]
    fn mismatched_document_identity_is_rejected() {
        let source = capture(1, b"needle");
        let other = capture(2, b"needle");
        let wrong = SearchDocument::new(other.request().file(), "wrong.rs", &source);
        assert_eq!(ReferenceScanOracle::scan_document(&wrong, &ParsedQuery::parse("needle").unwrap(), &options()),
            Err(QueryError::InvalidRange));
    }

    #[test]
    fn oracle_rejects_stale_revision_and_wrong_occurrence_negative_controls() {
        let source = capture(1, b"needle");
        let result = ReferenceScanOracle::scan_document(&doc(&source),
            &ParsedQuery::parse("needle").unwrap(), &options()).unwrap();
        assert!(ReferenceScanOracle::verify_oracle_match(&result, &result).is_ok());
        let mut stale = result.clone();
        stale.matches[0].revision = SourceRevision::new(ArenaOwnerId::new(1).unwrap(), 2).unwrap();
        assert!(ReferenceScanOracle::verify_oracle_match(&stale, &result).is_err());
        let mut wrong = result.clone();
        wrong.matches[0].occurrence_id += 1;
        assert!(ReferenceScanOracle::verify_oracle_match(&wrong, &result).is_err());
    }

    #[test]
    fn glob_filters_respect_directory_boundaries_and_recursive_components() {
        for (path, pattern, expected) in [
            ("src/main.rs", "*.rs", true),
            ("src/main.rs.bak", "*.rs", false),
            ("src/main.rs", "src/*.rs", true),
            ("src/nested/main.rs", "src/*.rs", false),
            ("src/main.rs", "src/**/*.rs", true),
            ("src/a/b/main.rs", "src/**/*.rs", true),
            ("other/src/main.rs", "src/**/*.rs", false),
            ("main.rs", "**/*.rs", true),
            ("a/b/main.rs", "**/*.rs", true),
            ("generated/out.rs", "**/generated/**", true),
            ("src/generated/out.rs", "**/generated/**", true),
            ("src/not_generated/out.rs", "**/generated/**", false),
            ("src/generatedish/out.rs", "**/generated/**", false),
            ("foo", "**/foo", true),
            ("a/foo", "**/foo", true),
            ("afoo", "**/foo", false),
            ("src/afoo", "src/**/foo", false),
            ("src/foo", "src/**/**/foo", true),
            ("src/a/b/foo", "src/**/**/foo", true),
            ("src/a/bfoo", "src/**/**/foo", false),
        ] {
            assert_eq!(path_matches(path, pattern), expected, "{pattern:?} on {path:?}");
        }
    }

    #[test]
    fn wildcard_units_are_unicode_scalars_and_native_backslashes_are_literal() {
        for (path, pattern, expected) in [
            ("src/é.rs", "src/?.rs", true),
            ("src/資料.rs", "src/?.rs", false),
            ("src/資料.rs", "src/??.rs", true),
            ("src/e\u{301}.rs", "src/?.rs", false),
            ("src/e\u{301}.rs", "src/??.rs", true),
            ("src/a/b.rs", "src/???.rs", false),
            (r"src/a\b.rs", r"src/a\?.rs", true),
            ("src/a/b.rs", r"src/a\?.rs", false),
            ("src/[a]/b.rs", "src/[a]/?.rs", true),
        ] {
            assert_eq!(path_matches(path, pattern), expected, "{pattern:?} on {path:?}");
        }
    }

    #[test]
    fn plain_path_filters_keep_substring_semantics() {
        assert!(path_matches("packages/tool/src/main.rs", "src/"));
        assert!(path_matches("packages/tool/src/main.rs", "tool"));
        assert!(!path_matches("packages/tool/src/main.rs", "tests/"));
        assert!(!path_matches("packages/tool/src/main.rs", "SRC/"));
    }

    #[test]
    fn quoted_glob_scope_excludes_files_before_spending_the_byte_budget() {
        let captures: Vec<_> = (1..=5).map(|id| capture(id, b"needle")).collect();
        let paths = [
            "Sources/My App/lib.rs", "Sources/My App/nested/mod.rs",
            "Sources/My App/generated/output.rs", "Sources/My App/readme.md",
            "Other App/lib.rs",
        ];
        let docs: Vec<_> = captures.iter().zip(paths).map(|(capture, path)|
            SearchDocument::new(capture.request().file(), path, capture)).collect();
        let query = ParsedQuery::parse(
            r#"needle path:"Sources/My App/**/*.rs" -path:**/generated/**"#,
        ).unwrap();
        let mut options = options();
        options.max_bytes_scanned = Some(12);
        let result = ReferenceScanOracle::scan_collection(&docs, &query, &options).unwrap();
        assert!(result.is_complete());
        assert_eq!(result.scanned_bytes, 12);
        assert_eq!(result.match_count(), 2);
        assert_eq!(result.matches.iter().map(|hit| hit.file_id).collect::<Vec<_>>(),
            [captures[0].request().file(), captures[1].request().file()]);
    }

    #[test]
    fn manually_constructed_filters_cannot_bypass_parser_bounds() {
        for value in [String::new(), "*".repeat(crate::MAX_QUERY_LEN + 1)] {
            let expected = if value.is_empty() { QueryError::SyntaxError } else { QueryError::NeedleTooLong };
            for filter in [PathFilterKind::Include(value.clone()), PathFilterKind::Exclude(value.clone())] {
                let mut query = ParsedQuery::parse("needle").unwrap();
                query.path_filters.push(filter);
                assert_eq!(ReferenceScanOracle::scan_collection(&[], &query, &options()), Err(expected));
                assert!(!ReferenceScanOracle::matches_path_filters("src/main.rs", &query.path_filters));
            }
            let mut query = ParsedQuery::parse("needle").unwrap();
            query.lang_filters.push(LangFilterKind::Exclude(value));
            assert_eq!(ReferenceScanOracle::scan_collection(&[], &query, &options()), Err(expected));
        }
    }

    #[test]
    fn repeated_wildcard_prefixes_do_not_require_backtracking() {
        let pattern = format!("{}*b", "*a".repeat(crate::MAX_QUERY_LEN / 2 - 1));
        assert_eq!(pattern.len(), crate::MAX_QUERY_LEN);
        assert!(!path_matches(&"a".repeat(1024), &pattern));
        assert!(path_matches(&format!("{}b", "a".repeat(1024)), &pattern));
    }

    /// Deliberately separate component-recursive oracle, only for tiny cases.
    /// Production must not use this exponential reference implementation.
    fn reference_component(text: &[char], pattern: &[char]) -> bool {
        match pattern.split_first() {
            None => text.is_empty(),
            Some((&'*', rest)) => reference_component(text, rest)
                || (!text.is_empty() && reference_component(&text[1..], pattern)),
            Some((&ch, rest)) => !text.is_empty() && (ch == '?' || ch == text[0])
                && reference_component(&text[1..], rest),
        }
    }

    fn reference_components(path: &[&str], pattern: &[&str]) -> bool {
        let Some((&part, rest)) = pattern.split_first() else { return path.is_empty(); };
        if part.len() >= 2 && part.bytes().all(|ch| ch == b'*') {
            reference_components(path, rest)
                || (!path.is_empty() && reference_components(&path[1..], pattern))
        } else {
            !path.is_empty() && (rest.is_empty() || path.len() > 1)
                && reference_component(&path[0].chars().collect::<Vec<_>>(), &part.chars().collect::<Vec<_>>())
                && reference_components(&path[1..], rest)
        }
    }

    fn reference_path(path: &str, pattern: &str) -> bool {
        if pattern.is_empty() { return false; }
        if !pattern.contains(['*', '?']) { return path.contains(pattern); }
        let path = if pattern.contains('/') { path } else { path.rsplit('/').next().unwrap_or(path) };
        reference_components(&path.split('/').collect::<Vec<_>>(), &pattern.split('/').collect::<Vec<_>>())
    }

    fn small_words(alphabet: &[char], depth: usize) -> Vec<String> {
        let mut words = vec![String::new()];
        let mut level = vec![String::new()];
        for _ in 0..depth {
            let mut next = Vec::new();
            for prefix in &level {
                for &ch in alphabet {
                    let mut word = prefix.clone();
                    word.push(ch);
                    next.push(word);
                }
            }
            words.extend(next.iter().cloned());
            level = next;
        }
        words
    }

    #[test]
    fn bounded_glob_machine_agrees_with_independent_component_oracle() {
        let paths = small_words(&['a', 'b', '/', 'é'], 4);
        let patterns = small_words(&['a', '/', '*', '?', 'é'], 4);
        for pattern in &patterns {
            for path in &paths {
                assert_eq!(path_matches(path, pattern), reference_path(path, pattern),
                    "{pattern:?} on {path:?}");
            }
        }
    }
}
