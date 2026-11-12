#![forbid(unsafe_code)]

//! The shared whole-token text policy for captured and streaming occurrences.
//!
//! This is not a language lexer or compiler reference resolver. Comments and
//! literals participate. Non-ASCII non-whitespace/non-control runs stay intact
//! conservatively, including combining marks and punctuation. Matching is exact
//! and case-sensitive; there is no normalization or Unicode identifier claim.

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

/// Allocation-free exact token comparison. Only a bounded query offset and
/// mismatch bit survive a long token; source-sized token strings are never kept.
/// The caller owns decoding, original-byte provenance, work/count budgets and
/// cancellation. A successful `push` closes the preceding token on a delimiter.
/// Call `finish` only at an actual end of input, never a chunk or budget boundary.
pub struct IdentifierCursor<'name> {
    name: &'name str,
    in_token: bool,
    matched: usize,
    rejected: bool,
}

impl<'name> IdentifierCursor<'name> {
    pub fn new(name: &'name str) -> Result<Self, InvalidIdentifier> {
        validate_identifier(name)?;
        Ok(Self { name, in_token: false, matched: 0, rejected: false })
    }

    pub const fn in_token(&self) -> bool { self.in_token }

    /// Returns true exactly when this delimiter establishes a whole-token hit.
    pub fn push(&mut self, scalar: char) -> bool {
        if !token_scalar(scalar) { return self.finish(); }
        self.in_token = true;
        if !self.rejected {
            if self.name[self.matched..].chars().next() == Some(scalar) {
                self.matched += scalar.len_utf8();
            } else {
                self.rejected = true;
            }
        }
        false
    }

    /// Establish the final token at a real end of the declared observation.
    pub fn finish(&mut self) -> bool {
        let matched = self.in_token && !self.rejected && self.matched == self.name.len();
        self.in_token = false;
        self.matched = 0;
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
    fn invalid_names_are_rejected_without_source_or_allocation() {
        for name in ["", "1foo", "foo bar", "foo-bar", "foo\n", "foo\0"] {
            assert!(validate_identifier(name).is_err(), "{name:?}");
        }
        assert!(validate_identifier(&"x".repeat(MAX_IDENTIFIER_BYTES)).is_ok());
        assert!(validate_identifier(&"x".repeat(MAX_IDENTIFIER_BYTES + 1)).is_err());
    }
}
