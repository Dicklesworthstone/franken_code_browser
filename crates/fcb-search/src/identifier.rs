#![forbid(unsafe_code)]

//! The shared whole-token text policy for captured and streaming occurrences.
//!
//! This is not a language lexer or compiler reference resolver. Comments and
//! literals participate. Non-ASCII non-whitespace/non-control runs stay intact
//! conservatively, including combining marks and punctuation. Matching defaults
//! to exact case-sensitive equality, with optional Unicode scalar lowercase of
//! the complete token. There is no full case folding, canonical normalization or
//! Unicode identifier-completeness claim.

use std::borrow::Cow;

pub const MAX_IDENTIFIER_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidIdentifier;

/// The same token boundary policy applies to every occurrence search route.
pub fn token_scalar(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '$')
        || (!c.is_ascii() && !c.is_whitespace() && !c.is_control())
}

pub fn validate_identifier(name: &str) -> Result<(), InvalidIdentifier> {
    if name.is_empty() || name.len() > MAX_IDENTIFIER_BYTES
        || !name.chars().all(token_scalar) || name.as_bytes()[0].is_ascii_digit() {
        return Err(InvalidIdentifier);
    }
    Ok(())
}

/// Allocation-free whole-token comparison. Only bounded query state and a
/// mismatch bit survive a long token; source-sized token strings are never kept.
/// The caller owns decoding, original-byte provenance, work/count budgets and
/// cancellation. A successful `push` closes the preceding token on a delimiter.
/// Call `finish` only at an actual end of input, never a chunk or budget boundary.
pub struct IdentifierCursor<'name> {
    name: Cow<'name, str>,
    case_sensitive: bool,
    in_token: bool,
    matched: usize,
    lowercase_pending: Option<std::char::ToLowercase>,
    rejected: bool,
}

impl<'name> IdentifierCursor<'name> {
    pub fn new(name: &'name str) -> Result<Self, InvalidIdentifier> {
        Self::new_matching(name, true)
    }

    /// Compare complete original tokens, optionally through Unicode scalar
    /// lowercase. Expansion units may span source scalars, but a partial
    /// expansion is never a complete token. Boundaries use the original scalars.
    /// This does not add full case folding or canonical normalization.
    pub fn new_matching(name: &'name str, case_sensitive: bool) -> Result<Self, InvalidIdentifier> {
        validate_identifier(name)?;
        Ok(Self { name: Cow::Borrowed(name), case_sensitive, in_token: false,
            matched: 0, lowercase_pending: None, rejected: false })
    }

    /// Transfer an already admitted bounded name into a resumable worker.
    pub fn new_owned(name: String) -> Result<IdentifierCursor<'static>, InvalidIdentifier> {
        Self::new_owned_matching(name, true)
    }

    pub fn new_owned_matching(name: String, case_sensitive: bool)
        -> Result<IdentifierCursor<'static>, InvalidIdentifier> {
        validate_identifier(&name)?;
        Ok(IdentifierCursor { name: Cow::Owned(name), case_sensitive, in_token: false,
            matched: 0, lowercase_pending: None, rejected: false })
    }

    pub const fn in_token(&self) -> bool { self.in_token }

    /// Whether the current token is still a matching prefix. A caller retaining
    /// original spellings can discard a rejected token without buffering its tail.
    pub const fn is_candidate(&self) -> bool { self.in_token && !self.rejected }

    /// Returns true exactly when this delimiter establishes a whole-token hit.
    pub fn push(&mut self, scalar: char) -> bool {
        if !token_scalar(scalar) { return self.finish(); }
        self.in_token = true;
        if !self.rejected {
            if self.case_sensitive {
                if self.name[self.matched..].chars().next() == Some(scalar) {
                    self.matched += scalar.len_utf8();
                } else {
                    self.rejected = true;
                }
            } else {
                for unit in scalar.to_lowercase() {
                    if self.next_lowercase_unit() != Some(unit) {
                        self.rejected = true;
                        break;
                    }
                }
            }
        }
        false
    }

    fn next_lowercase_unit(&mut self) -> Option<char> {
        loop {
            if let Some(unit) = self.lowercase_pending.as_mut().and_then(Iterator::next) {
                return Some(unit);
            }
            let scalar = self.name[self.matched..].chars().next()?;
            self.matched += scalar.len_utf8();
            self.lowercase_pending = Some(scalar.to_lowercase());
        }
    }

    /// Establish the final token at a real end of the declared observation.
    pub fn finish(&mut self) -> bool {
        let matched = self.in_token && !self.rejected && self.matched == self.name.len()
            && self.lowercase_pending.as_ref().is_none_or(|units| units.clone().next().is_none());
        self.in_token = false;
        self.matched = 0;
        self.lowercase_pending = None;
        self.rejected = false;
        matched
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn count(text: &str, name: &str) -> usize {
        let mut cursor = IdentifierCursor::new(name).unwrap();
        text.chars().filter(|&ch| cursor.push(ch)).count() + usize::from(cursor.finish())
    }

    #[test]
    fn exact_boundaries_match_the_existing_reference_policy() {
        assert_eq!(count("foo foobar _foo foo_ $foo 1foo Foo foo", "foo"), 2);
        assert_eq!(count("foo(); // foo\n\"foo\"", "foo"), 3);
        assert_eq!(count("e\u{301} e e—next e", "e"), 2);
        assert_eq!(count("e\u{301} e\u{301}x e\u{301}", "e\u{301}"), 2);
        assert_eq!(count("$item $item2 item", "$item"), 1);
    }

    #[test]
    fn a_pending_token_is_never_accepted_at_a_pause() {
        let mut cursor = IdentifierCursor::new("foo").unwrap();
        for ch in "foo".chars() { assert!(!cursor.push(ch)); }
        assert!(cursor.in_token());
        assert!(!cursor.push('b'));
        assert!(!cursor.push(' '));
        for ch in "foo".chars() { assert!(!cursor.push(ch)); }
        assert!(cursor.finish());
        assert!(!cursor.finish());
    }

    #[test]
    fn lowercase_requires_the_entire_original_token() {
        let count = |text: &str, name: &str| {
            let mut cursor = IdentifierCursor::new_matching(name, false).unwrap();
            text.chars().filter(|&ch| cursor.push(ch)).count() + usize::from(cursor.finish())
        };
        assert_eq!(count("foo Foo FOO fOo foobar FOO_ _foo $foo 1foo", "foo"), 4);
        assert_eq!(count("k K K Kx _K", "K"), 3);
        assert_eq!(count("İ I i i\u{307} I\u{307}", "i"), 2);
        assert_eq!(count("İ I i i\u{307} I\u{307}", "İ"), 3);
        assert_eq!(count("İ I i i\u{307} I\u{307}", "i\u{307}"), 3);
        assert_eq!(count("ß ẞ ss SS", "ß"), 2);
        assert_eq!(count("É é E\u{301} e\u{301}", "é"), 2);
    }

    #[test]
    fn lowercase_candidates_remain_bounded_across_pauses_and_rejected_tails() {
        let mut cursor = IdentifierCursor::new_owned_matching("İ".to_owned(), false).unwrap();
        assert!(!cursor.push('i'));
        assert!(cursor.is_candidate());
        assert!(!cursor.finish(), "half the query expansion is not a complete token");
        assert!(!cursor.push('i'));
        assert!(!cursor.push('\u{307}'));
        assert!(cursor.is_candidate());
        assert!(cursor.push(' '));
        assert!(!cursor.is_candidate());
        assert!(!cursor.push('İ'));
        assert!(cursor.is_candidate());
        for _ in 0..10_000 {
            assert!(!cursor.push('x'));
            assert!(cursor.in_token());
            assert!(!cursor.is_candidate());
        }
        assert!(!cursor.push(' '));
        assert!(!cursor.push('İ'));
        assert!(cursor.finish());
        assert!(!cursor.finish());
    }

    #[test]
    fn invalid_names_are_rejected_without_source_or_allocation() {
        for name in ["", "1foo", "foo bar", "foo-bar", "foo\n", "foo\0"] {
            assert!(validate_identifier(name).is_err(), "{name:?}");
        }
        assert!(validate_identifier(&"x".repeat(MAX_IDENTIFIER_BYTES)).is_ok());
        assert!(validate_identifier(&"x".repeat(MAX_IDENTIFIER_BYTES + 1)).is_err());
    }
}
