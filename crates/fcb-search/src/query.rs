#![forbid(unsafe_code)]

//! Bounded search query parser, filter AST, and limits (FCB-026.B / fcb-8ii.2).
//!
//! §17.6:
//! A bounded first-party query parser supporting:
//! - Exact phrases: `"exact phrase"` with escaping (`\"`, `\\`).
//! - Field filters:
//!   - `path:<glob-or-prefix>` and `-path:<glob-or-prefix>`.
//!   - `lang:<language-or-ext>` and `-lang:<language-or-ext>`.
//! - Conjunctions (AND terms): terms separated by whitespace are conjunctions.
//! - Exclusions (NOT terms): terms prefixed with `-`.
//! - Nesting and size limits: `MAX_QUERY_LEN` (1024 bytes) and `MAX_QUERY_TOKENS` (64).
//! - Rejection of unclosed quotes (`QUERY_SYNTAX_ERROR`).
//! - Rejection of unqualified regex queries (`QUERY_REGEX_UNQUALIFIED`).
//! - Rejection of empty machine queries (`QUERY_EMPTY`).

use crate::QueryError;

/// Maximum allowed raw query string length in bytes (§17.6).
pub const MAX_QUERY_LEN: usize = 1024;

/// Maximum allowed parsed tokens in a single query (§17.6).
pub const MAX_QUERY_TOKENS: usize = 64;

/// Path inclusion or exclusion filter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum PathFilterKind {
    Include(String),
    Exclude(String),
}

/// Language / file extension inclusion or exclusion filter.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum LangFilterKind {
    Include(String),
    Exclude(String),
}

/// A parsed, validated search query representation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ParsedQuery {
    /// The primary needle to locate in matched sources.
    pub primary_needle: String,
    /// Whether the primary needle is an exact quoted phrase.
    pub is_phrase: bool,
    /// Additional positive terms that must co-occur in the document (conjunctions).
    pub conjunction_terms: Vec<String>,
    /// Terms that must not occur in the document (exclusions).
    pub exclusion_terms: Vec<String>,
    /// Path inclusion and exclusion constraints.
    pub path_filters: Vec<PathFilterKind>,
    /// Language / extension inclusion and exclusion constraints.
    pub lang_filters: Vec<LangFilterKind>,
    /// Original unparsed raw query.
    pub raw_query: String,
}

impl ParsedQuery {
    /// Parse a query string into a [`ParsedQuery`] enforcing §17.6 bounds and semantics.
    pub fn parse(query: &str) -> Result<Self, QueryError> {
        if query.len() > MAX_QUERY_LEN {
            return Err(QueryError::NeedleTooLong);
        }

        let trimmed = query.trim();
        if trimmed.is_empty() {
            return Err(QueryError::EmptyNeedle);
        }

        // Detect explicit regex prefixes and reject until qualified (§17.6)
        if trimmed.starts_with("re:")
            || trimmed.starts_with("regex:")
            || trimmed.starts_with("/^")
        {
            return Err(QueryError::RegexUnqualified);
        }

        let mut primary_needle = String::new();
        let mut is_phrase = false;
        let mut conjunction_terms = Vec::new();
        let mut exclusion_terms = Vec::new();
        let mut path_filters = Vec::new();
        let mut lang_filters = Vec::new();

        let chars: Vec<char> = trimmed.chars().collect();
        let mut idx = 0;
        let mut token_count = 0;

        while idx < chars.len() {
            // Skip whitespace
            while idx < chars.len() && chars[idx].is_whitespace() {
                idx += 1;
            }
            if idx >= chars.len() {
                break;
            }

            token_count += 1;
            if token_count > MAX_QUERY_TOKENS {
                return Err(QueryError::LimitExceeded);
            }

            // Check if this token is an exclusion of a phrase: -"...
            let is_negated = chars[idx] == '-';
            let start_after_minus = if is_negated { idx + 1 } else { idx };

            if start_after_minus < chars.len() && chars[start_after_minus] == '"' {
                // Parse quoted phrase
                idx = start_after_minus + 1;
                let mut phrase = String::new();
                let mut closed = false;

                while idx < chars.len() {
                    let c = chars[idx];
                    if c == '\\' {
                        if idx + 1 < chars.len() {
                            let next_c = chars[idx + 1];
                            if next_c == '"' || next_c == '\\' {
                                phrase.push(next_c);
                                idx += 2;
                                continue;
                            }
                        }
                        phrase.push('\\');
                        idx += 1;
                    } else if c == '"' {
                        closed = true;
                        idx += 1;
                        break;
                    } else {
                        phrase.push(c);
                        idx += 1;
                    }
                }

                if !closed {
                    return Err(QueryError::SyntaxError);
                }

                if phrase.is_empty() {
                    return Err(QueryError::EmptyNeedle);
                }

                if is_negated {
                    exclusion_terms.push(phrase);
                } else if primary_needle.is_empty() {
                    primary_needle = phrase;
                    is_phrase = true;
                } else {
                    conjunction_terms.push(phrase);
                }
                continue;
            }

            // Unquoted token: read until whitespace
            let token_start = idx;
            while idx < chars.len() && !chars[idx].is_whitespace() {
                idx += 1;
            }
            let token_str: String = chars[token_start..idx].iter().collect();

            // Check for regex prefix inside token
            if token_str.starts_with("re:") || token_str.starts_with("regex:") {
                return Err(QueryError::RegexUnqualified);
            }

            // Check field filters
            if let Some(path_val) = token_str.strip_prefix("path:") {
                if path_val.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                path_filters.push(PathFilterKind::Include(path_val.to_string()));
            } else if let Some(path_val) = token_str.strip_prefix("-path:") {
                if path_val.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                path_filters.push(PathFilterKind::Exclude(path_val.to_string()));
            } else if let Some(lang_val) = token_str.strip_prefix("lang:") {
                if lang_val.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                lang_filters.push(LangFilterKind::Include(lang_val.to_lowercase()));
            } else if let Some(lang_val) = token_str.strip_prefix("-lang:") {
                if lang_val.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                lang_filters.push(LangFilterKind::Exclude(lang_val.to_lowercase()));
            } else if let Some(type_val) = token_str.strip_prefix("type:") {
                if type_val.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                lang_filters.push(LangFilterKind::Include(type_val.to_lowercase()));
            } else if let Some(type_val) = token_str.strip_prefix("-type:") {
                if type_val.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                lang_filters.push(LangFilterKind::Exclude(type_val.to_lowercase()));
            } else if let Some(neg_term) = token_str.strip_prefix('-') {
                if neg_term.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                exclusion_terms.push(neg_term.to_string());
            } else if let Some(pos_term) = token_str.strip_prefix('+') {
                if pos_term.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                if primary_needle.is_empty() {
                    primary_needle = pos_term.to_string();
                } else {
                    conjunction_terms.push(pos_term.to_string());
                }
            } else {
                // Plain word
                if primary_needle.is_empty() {
                    primary_needle = token_str;
                } else {
                    conjunction_terms.push(token_str);
                }
            }
        }

        if primary_needle.is_empty() {
            return Err(QueryError::EmptyNeedle);
        }

        Ok(Self {
            primary_needle,
            is_phrase,
            conjunction_terms,
            exclusion_terms,
            path_filters,
            lang_filters,
            raw_query: query.to_string(),
        })
    }
}
