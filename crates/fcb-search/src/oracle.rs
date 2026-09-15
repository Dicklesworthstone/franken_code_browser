#![forbid(unsafe_code)]

//! Authoritative reference scan oracle and filter evaluator (FCB-026.B / fcb-8ii.2).
//!
//! §17.3, §17.6, §17.11:
//! Small scopes and verification tests use an authoritative direct scan oracle.
//! The reference scan oracle guarantees:
//! - Filters (path, language, conjunction, exclusion) determine document eligibility
//!   WITHOUT multiplying anchor occurrences.
//! - Primary search needle occurrences are reported with exact original byte ranges,
//!   decoded ranges, and occurrence identities.
//! - Unsupported / invalid encodings preserve honest coverage rather than claiming
//!   false clean zero matches.
//! - Verifies indexed/optimized search results against the reference scan oracle.

use fcb_core::FileId;
use fcb_source::CompleteCapture;

use crate::{
    DirectSourceScanner, LangFilterKind, ParsedQuery, PathFilterKind, QueryError, QueryOptions,
    SearchCoverage, SearchResult,
};

/// A document candidate submitted to the reference scan oracle.
#[derive(Clone, Debug)]
pub struct SearchDocument<'a> {
    pub file_id: FileId,
    pub path: &'a str,
    pub capture: &'a CompleteCapture,
}

impl<'a> SearchDocument<'a> {
    pub fn new(file_id: FileId, path: &'a str, capture: &'a CompleteCapture) -> Self {
        Self {
            file_id,
            path,
            capture,
        }
    }
}

/// Error returned when a candidate search result disagrees with reference oracle truth.
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

/// Authoritative reference scanner and verification oracle.
pub struct ReferenceScanOracle;

impl ReferenceScanOracle {
    /// Check whether a file path satisfies all path filter constraints.
    pub fn matches_path_filters(path: &str, filters: &[PathFilterKind]) -> bool {
        for filter in filters {
            match filter {
                PathFilterKind::Include(prefix_or_glob) => {
                    if !Self::path_matches(path, prefix_or_glob) {
                        return false;
                    }
                }
                PathFilterKind::Exclude(prefix_or_glob) => {
                    if Self::path_matches(path, prefix_or_glob) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Check whether a file path satisfies all language filter constraints.
    pub fn matches_lang_filters(path: &str, filters: &[LangFilterKind]) -> bool {
        let ext = path
            .rsplit('.')
            .next()
            .unwrap_or("")
            .to_lowercase();

        for filter in filters {
            match filter {
                LangFilterKind::Include(target_lang) => {
                    if !Self::lang_matches(&ext, target_lang) {
                        return false;
                    }
                }
                LangFilterKind::Exclude(target_lang) => {
                    if Self::lang_matches(&ext, target_lang) {
                        return false;
                    }
                }
            }
        }
        true
    }

    /// Scan a single document under a [`ParsedQuery`] using reference oracle semantics.
    ///
    /// Evaluates path and language filters, exclusion terms, conjunction terms,
    /// and matches the primary needle without multiplying anchor occurrences.
    pub fn scan_document(
        doc: &SearchDocument<'_>,
        query: &ParsedQuery,
        options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        let raw_len = doc.capture.bytes().len() as u64;

        // 1. Path filter
        if !Self::matches_path_filters(doc.path, &query.path_filters) {
            return Ok(SearchResult::empty(SearchCoverage::Exhaustive));
        }

        // 2. Language filter
        if !Self::matches_lang_filters(doc.path, &query.lang_filters) {
            return Ok(SearchResult::empty(SearchCoverage::Exhaustive));
        }

        // Fast probe options for conjunction and exclusion checks
        let probe_options = QueryOptions::new(options.generation)
            .with_mode(options.mode)
            .with_max_matches(1);

        // 3. Exclusion filters: if document matches any excluded term, it is rejected
        for excluded_term in &query.exclusion_terms {
            let res = DirectSourceScanner::scan_complete_capture(
                doc.capture,
                excluded_term,
                &probe_options,
            )?;
            if res.match_count() > 0 {
                // Rejected by exclusion filter
                return Ok(SearchResult::empty(SearchCoverage::Exhaustive));
            }
        }

        // 4. Conjunction filters: all additional terms must occur in document
        for conj_term in &query.conjunction_terms {
            let res = DirectSourceScanner::scan_complete_capture(
                doc.capture,
                conj_term,
                &probe_options,
            )?;
            if res.match_count() == 0 {
                // Rejected because required conjunction term is missing
                return Ok(SearchResult::empty(SearchCoverage::Exhaustive));
            }
        }

        // 5. Scan for primary needle without multiplying occurrences
        let mut result = DirectSourceScanner::scan_complete_capture(
            doc.capture,
            &query.primary_needle,
            options,
        )?;

        // If capture was unsupported, ensure scanned_bytes is set
        if !result.unsupported_files.is_empty() {
            result.scanned_bytes = raw_len;
        }

        Ok(result)
    }

    /// Scan a collection of documents under a [`ParsedQuery`], aggregating results
    /// and honoring `options.max_matches` truncation bounds.
    pub fn scan_collection(
        docs: &[SearchDocument<'_>],
        query: &ParsedQuery,
        options: &QueryOptions,
    ) -> Result<SearchResult, QueryError> {
        let mut all_matches = Vec::new();
        let mut total_counted = 0usize;
        let mut total_scanned_bytes = 0u64;
        let mut unsupported_files = Vec::new();
        let mut truncated = false;

        for doc in docs {
            let remaining_limit = options.max_matches.saturating_sub(all_matches.len());
            if remaining_limit == 0 && options.max_matches > 0 {
                truncated = true;
                break;
            }

            let doc_options = QueryOptions::new(options.generation)
                .with_mode(options.mode)
                .with_max_matches(remaining_limit);

            let doc_res = Self::scan_document(doc, query, &doc_options)?;
            total_scanned_bytes += doc_res.scanned_bytes;
            unsupported_files.extend_from_slice(&doc_res.unsupported_files);

            for m in doc_res.matches {
                total_counted += 1;
                if all_matches.len() < options.max_matches {
                    all_matches.push(m);
                } else {
                    truncated = true;
                }
            }

            if all_matches.len() >= options.max_matches {
                truncated = true;
                break;
            }
        }

        let coverage = if truncated {
            SearchCoverage::TruncatedAtLimit {
                max_matches: options.max_matches,
            }
        } else {
            SearchCoverage::Exhaustive
        };

        Ok(SearchResult {
            matches: all_matches,
            total_matches_counted: total_counted,
            coverage,
            scanned_bytes: total_scanned_bytes,
            unsupported_files,
        })
    }

    /// Authoritative oracle verification: compares candidate result against reference oracle truth.
    pub fn verify_oracle_match(
        candidate: &SearchResult,
        oracle: &SearchResult,
    ) -> Result<(), OracleMismatchError> {
        if candidate.matches.len() != oracle.matches.len() {
            return Err(OracleMismatchError {
                reason: format!(
                    "Match count mismatch: candidate has {}, oracle has {}",
                    candidate.matches.len(),
                    oracle.matches.len()
                ),
            });
        }

        if candidate.total_matches_counted != oracle.total_matches_counted {
            return Err(OracleMismatchError {
                reason: format!(
                    "Total matches counted mismatch: candidate has {}, oracle has {}",
                    candidate.total_matches_counted,
                    oracle.total_matches_counted
                ),
            });
        }

        if candidate.coverage != oracle.coverage {
            return Err(OracleMismatchError {
                reason: format!(
                    "Coverage mismatch: candidate has {:?}, oracle has {:?}",
                    candidate.coverage,
                    oracle.coverage
                ),
            });
        }

        if candidate.unsupported_files != oracle.unsupported_files {
            return Err(OracleMismatchError {
                reason: format!(
                    "Unsupported files mismatch: candidate has {:?}, oracle has {:?}",
                    candidate.unsupported_files,
                    oracle.unsupported_files
                ),
            });
        }

        for (i, (c_match, o_match)) in candidate.matches.iter().zip(&oracle.matches).enumerate() {
            if c_match.file_id != o_match.file_id {
                return Err(OracleMismatchError {
                    reason: format!(
                        "Match {} file_id mismatch: candidate={:?}, oracle={:?}",
                        i, c_match.file_id, o_match.file_id
                    ),
                });
            }

            if c_match.original_byte_range != o_match.original_byte_range {
                return Err(OracleMismatchError {
                    reason: format!(
                        "Match {} byte range mismatch: candidate={:?}, oracle={:?}",
                        i, c_match.original_byte_range, o_match.original_byte_range
                    ),
                });
            }

            if c_match.decoded_range != o_match.decoded_range {
                return Err(OracleMismatchError {
                    reason: format!(
                        "Match {} decoded range mismatch: candidate={:?}, oracle={:?}",
                        i, c_match.decoded_range, o_match.decoded_range
                    ),
                });
            }

            if c_match.matched_text != o_match.matched_text {
                return Err(OracleMismatchError {
                    reason: format!(
                        "Match {} matched text mismatch: candidate={:?}, oracle={:?}",
                        i, c_match.matched_text, o_match.matched_text
                    ),
                });
            }

            if c_match.multiplicity != o_match.multiplicity {
                return Err(OracleMismatchError {
                    reason: format!(
                        "Match {} multiplicity mismatch: candidate={}, oracle={}",
                        i, c_match.multiplicity, o_match.multiplicity
                    ),
                });
            }
        }

        Ok(())
    }

    fn path_matches(path: &str, pattern: &str) -> bool {
        if let Some(suffix) = pattern.strip_prefix('*') {
            path.ends_with(suffix)
        } else if pattern.ends_with('/') {
            path.starts_with(pattern) || path.contains(pattern)
        } else {
            path.contains(pattern)
        }
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
