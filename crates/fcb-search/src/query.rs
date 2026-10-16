#![forbid(unsafe_code)]

//! Bounded search query parser, filter AST, and limits (FCB-026.B / fcb-8ii.2).
//!
//! §17.6:
//! A bounded first-party query parser supporting:
//! - Exact phrases: `"exact phrase"` with escaping (`\"`, `\\`).
//! - Field filters:
//!   - `path:<glob-or-prefix>` and `-path:<glob-or-prefix>`.
//!   - `lang:<language-or-ext>` and `-lang:<language-or-ext>`.
//!   - Quoted field values, including `path:"directory with spaces/"`.
//! - Conjunctions (AND terms): terms separated by whitespace are conjunctions.
//! - Exclusions (NOT terms): terms prefixed with `-`.
//! - Size limits: `MAX_QUERY_LEN` (1024 bytes) and `MAX_QUERY_TOKENS` (64).
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
    /// Whether the primary needle contains an exact quoted phrase.
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
        if query.trim().is_empty() {
            return Err(QueryError::EmptyNeedle);
        }

        let mut parsed = Self {
            primary_needle: String::new(),
            is_phrase: false,
            conjunction_terms: Vec::new(),
            exclusion_terms: Vec::new(),
            path_filters: Vec::new(),
            lang_filters: Vec::new(),
            raw_query: query.to_string(),
        };
        let mut chars = query.chars().peekable();
        let mut token_count = 0;
        while let Some(token) = next_token(&mut chars)? {
            token_count += 1;
            if token_count > MAX_QUERY_TOKENS {
                return Err(QueryError::LimitExceeded);
            }

            // Only unquoted syntax is an operator. In particular, "-word"
            // and "path:word" remain literal content, not an exclusion/filter.
            let signed = token.unquoted_prefix > 0
                && matches!(token.value.as_bytes().first(), Some(b'-' | b'+'));
            let negated = signed && token.value.starts_with('-');
            let prefix = token.unquoted_prefix - usize::from(signed);
            let value = if signed { &token.value[1..] } else { &token.value };
            if value.is_empty() {
                return Err(if token.quoted {
                    QueryError::EmptyNeedle
                } else {
                    QueryError::SyntaxError
                });
            }

            let directive = |name: &str| prefix >= name.len() && value.starts_with(name);
            if directive("re:") || directive("regex:") || directive("/^") {
                return Err(QueryError::RegexUnqualified);
            }
            if directive("path:") {
                let path = &value["path:".len()..];
                if path.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                parsed.path_filters.push(if negated {
                    PathFilterKind::Exclude(path.to_string())
                } else {
                    PathFilterKind::Include(path.to_string())
                });
            } else if directive("lang:") || directive("type:") {
                let lang = &value["lang:".len()..];
                if lang.is_empty() {
                    return Err(QueryError::SyntaxError);
                }
                parsed.lang_filters.push(if negated {
                    LangFilterKind::Exclude(lang.to_lowercase())
                } else {
                    LangFilterKind::Include(lang.to_lowercase())
                });
            } else if negated {
                parsed.exclusion_terms.push(value.to_string());
            } else if parsed.primary_needle.is_empty() {
                parsed.primary_needle = value.to_string();
                parsed.is_phrase = token.quoted;
            } else {
                parsed.conjunction_terms.push(value.to_string());
            }
        }

        if parsed.primary_needle.is_empty() {
            return Err(QueryError::EmptyNeedle);
        }
        Ok(parsed)
    }
}

/// Retain where quoting starts so decoded text cannot acquire operator meaning.
struct Token {
    value: String,
    unquoted_prefix: usize,
    quoted: bool,
}

/// Whitespace separates tokens only outside quotes. Adjacent quoted/unquoted
/// fragments form one token, so a closing quote cannot silently add an AND term.
/// Backslashes outside quotes are native filename bytes, not shell escapes.
fn next_token(chars: &mut std::iter::Peekable<std::str::Chars<'_>>)
    -> Result<Option<Token>, QueryError>
{
    while chars.peek().is_some_and(|c| c.is_whitespace()) {
        chars.next();
    }
    if chars.peek().is_none() {
        return Ok(None);
    }
    let mut value = String::new();
    let mut first_quote = None;
    let mut in_quotes = false;
    while let Some(&c) = chars.peek() {
        if !in_quotes && c.is_whitespace() {
            break;
        }
        chars.next();
        match c {
            '"' => {
                first_quote.get_or_insert(value.len());
                in_quotes = !in_quotes;
            }
            '\\' if in_quotes => {
                if let Some(&escaped @ ('"' | '\\')) = chars.peek() {
                    value.push(escaped);
                    chars.next();
                } else {
                    // Only quote/backslash escapes are interpreted. Preserve
                    // unknown escapes rather than changing native path bytes.
                    value.push('\\');
                }
            }
            _ => value.push(c),
        }
    }
    if in_quotes {
        return Err(QueryError::SyntaxError);
    }
    Ok(Some(Token {
        unquoted_prefix: first_quote.unwrap_or(value.len()),
        quoted: first_quote.is_some(),
        value,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn quoted_path_values_are_filters_not_content_terms() {
        let query = ParsedQuery::parse(r#"needle path:"Sources/My App/**/*.swift" -path:"Generated Files/""#).unwrap();
        assert_eq!(query.primary_needle, "needle");
        assert!(query.conjunction_terms.is_empty());
        assert!(query.exclusion_terms.is_empty());
        assert_eq!(query.path_filters, [
            PathFilterKind::Include("Sources/My App/**/*.swift".into()),
            PathFilterKind::Exclude("Generated Files/".into()),
        ]);
    }

    #[test]
    fn quoted_language_aliases_and_explicit_positive_filters() {
        let query = ParsedQuery::parse(r#"+path:"src dir/" lang:"RuSt" -type:"PY" needle"#).unwrap();
        assert_eq!(query.path_filters, [PathFilterKind::Include("src dir/".into())]);
        assert_eq!(query.lang_filters, [
            LangFilterKind::Include("rust".into()), LangFilterKind::Exclude("py".into()),
        ]);
        assert_eq!(query.primary_needle, "needle");
    }

    #[test]
    fn signed_phrases_and_conjunctions_keep_their_polarity() {
        let query = ParsedQuery::parse(r#"+"primary phrase" +required -"excluded phrase" "other phrase""#).unwrap();
        assert_eq!(query.primary_needle, "primary phrase");
        assert!(query.is_phrase);
        assert_eq!(query.conjunction_terms, ["required", "other phrase"]);
        assert_eq!(query.exclusion_terms, ["excluded phrase"]);
    }

    #[test]
    fn quoted_operators_are_literal_content() {
        let query = ParsedQuery::parse(r#""path:src/" "-excluded" "+required" "regex:a.*""#).unwrap();
        assert_eq!(query.primary_needle, "path:src/");
        assert_eq!(query.conjunction_terms, ["-excluded", "+required", "regex:a.*"]);
        assert!(query.path_filters.is_empty());
        assert!(query.exclusion_terms.is_empty());
    }

    #[test]
    fn escaped_quotes_backslashes_and_unicode_survive_field_decoding() {
        let query = ParsedQuery::parse(r#"needle path:"資料/My \"App\"\\cache/""#).unwrap();
        assert_eq!(query.path_filters, [PathFilterKind::Include("資料/My \"App\"\\cache/".into())]);
        let query = ParsedQuery::parse(r#"needle path:"src\name/""#).unwrap();
        assert_eq!(query.path_filters, [PathFilterKind::Include(r"src\name/".into())]);
    }

    #[test]
    fn adjacent_fragments_are_one_token_not_accidental_conjunctions() {
        let query = ParsedQuery::parse(r#""hello "world path:"src dir"/nested/"#).unwrap();
        assert_eq!(query.primary_needle, "hello world");
        assert!(query.conjunction_terms.is_empty());
        assert_eq!(query.path_filters, [PathFilterKind::Include("src dir/nested/".into())]);
    }

    #[test]
    fn unclosed_quotes_are_rejected_in_every_token_position() {
        for query in ["\"needle", "needle path:\"src dir", "needle -path:\"src dir",
            "needle lang:\"rust", "needle ab\"cd", "needle -\"excluded"] {
            assert_eq!(ParsedQuery::parse(query), Err(QueryError::SyntaxError), "{query}");
        }
    }

    #[test]
    fn empty_fields_and_bare_signs_are_rejected() {
        for query in ["needle path:", "needle -lang:", "needle +", "needle -",
            r#"needle path:"""#, r#"needle -path:"""#, r#"needle type:"""#] {
            assert_eq!(ParsedQuery::parse(query), Err(QueryError::SyntaxError), "{query}");
        }
        for query in ["", "  ", r#""""#, r#"needle -"""#, "path:src/"] {
            assert_eq!(ParsedQuery::parse(query), Err(QueryError::EmptyNeedle), "{query}");
        }
    }

    #[test]
    fn regex_operators_cannot_hide_behind_a_sign_or_later_token() {
        for query in ["re:foo", "regex:foo", "/^foo", "needle re:foo",
            "needle -regex:foo", "needle +re:foo", "needle /^foo"] {
            assert_eq!(ParsedQuery::parse(query), Err(QueryError::RegexUnqualified), "{query}");
        }
        for query in [r#""regex:foo""#, r#""/^foo""#, r#""-re:foo""#] {
            assert!(ParsedQuery::parse(query).is_ok(), "{query}");
        }
    }

    #[test]
    fn unknown_fields_and_native_backslashes_remain_literal() {
        let query = ParsedQuery::parse(r"symbol:Name C:\source\file").unwrap();
        assert_eq!(query.primary_needle, "symbol:Name");
        assert_eq!(query.conjunction_terms, [r"C:\source\file"]);
    }

    #[test]
    fn token_limit_counts_phrases_and_filters_not_their_words() {
        let valid = std::iter::repeat_n("\"two words\"", MAX_QUERY_TOKENS).collect::<Vec<_>>().join(" ");
        let query = ParsedQuery::parse(&valid).unwrap();
        assert_eq!(query.conjunction_terms.len(), MAX_QUERY_TOKENS - 1);
        let invalid = format!("{valid} x");
        assert_eq!(ParsedQuery::parse(&invalid), Err(QueryError::LimitExceeded));
        let filtered = format!("needle {}", "path:x ".repeat(MAX_QUERY_TOKENS));
        assert_eq!(ParsedQuery::parse(&filtered), Err(QueryError::LimitExceeded));
    }

    #[test]
    fn byte_limit_and_original_query_are_preserved() {
        let exact = "é".repeat(MAX_QUERY_LEN / 2);
        assert_eq!(ParsedQuery::parse(&exact).unwrap().primary_needle, exact);
        assert_eq!(ParsedQuery::parse(&format!("{exact}x")), Err(QueryError::NeedleTooLong));
        let raw = "  needle\tpath:\"src dir/\"\n";
        assert_eq!(ParsedQuery::parse(raw).unwrap().raw_query, raw);
    }
}
