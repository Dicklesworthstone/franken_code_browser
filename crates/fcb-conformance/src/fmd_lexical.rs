//! Production adapter: the hostile harness driving the real FCB-021
//! resumable lexical engine (`franken_markdown::resume::ResumableLexer`).
//!
//! Enabled only through the `fmd-lexical` feature, which pulls the explicit
//! upstream path dependency; the shipping closure excludes it. The adapter's
//! failure classification maps engine refusals to stable codes so hostile
//! campaigns can reproduce and minimize real lexical failures without ever
//! changing their meaning.

use crate::{HostileByteGenerator, ReproducibleDigest, SeededRng};
use franken_markdown::resume::{ResumableLexer, ResumeError};

/// Stable classification codes for real lexical failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexicalFailure {
    /// Malformed UTF-8 was refused at a byte offset.
    InvalidUtf8,
    /// The held suffix exceeded the configured bound.
    SuffixTooLong,
    /// The lexer was fed after finishing.
    AlreadyFinished,
    /// The language has no lexical route.
    UnsupportedLanguage,
}

impl LexicalFailure {
    /// Stable machine-readable code used by minimization comparisons.
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidUtf8 => "INVALID_UTF8",
            Self::SuffixTooLong => "SUFFIX_TOO_LONG",
            Self::AlreadyFinished => "ALREADY_FINISHED",
            Self::UnsupportedLanguage => "UNSUPPORTED_LANGUAGE",
        }
    }
}

fn classify(error: ResumeError) -> LexicalFailure {
    match error {
        ResumeError::InvalidUtf8 { .. } => LexicalFailure::InvalidUtf8,
        ResumeError::SuffixTooLong { .. } => LexicalFailure::SuffixTooLong,
        ResumeError::AlreadyFinished => LexicalFailure::AlreadyFinished,
        ResumeError::UnsupportedLanguage => LexicalFailure::UnsupportedLanguage,
    }
}

/// The outcome of one hostile feed against the real engine.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LexOutcome {
    /// The engine accepted the chunk; spans may have been released.
    Accepted {
        /// Spans released by this feed.
        emitted: usize,
        /// Held suffix bytes after the feed.
        pending: usize,
    },
    /// The engine refused with a stable classification.
    Refused {
        /// Stable refusal code.
        code: &'static str,
    },
}

/// A hostile campaign target wrapping the real resumable engine.
#[derive(Debug)]
pub struct FmdResumableAdapter {
    lexer: ResumableLexer,
}

impl FmdResumableAdapter {
    /// Attach to a supported language with the default held-suffix cap.
    pub fn new(lang: &str) -> Result<Self, LexicalFailure> {
        Ok(Self {
            lexer: ResumableLexer::new(lang).map_err(classify)?,
        })
    }

    /// Feed one hostile chunk; refusal codes are stable classifications.
    pub fn feed(&mut self, chunk: &[u8]) -> LexOutcome {
        match self.lexer.feed(chunk) {
            Ok(report) => LexOutcome::Accepted {
                emitted: report.spans_emitted,
                pending: report.pending_bytes,
            },
            Err(error) => LexOutcome::Refused {
                code: classify(error).code(),
            },
        }
    }

    /// Flush the held suffix at EOF.
    pub fn finish(&mut self) -> LexOutcome {
        match self.lexer.finish() {
            Ok(()) => LexOutcome::Accepted {
                emitted: 0,
                pending: 0,
            },
            Err(error) => LexOutcome::Refused {
                code: classify(error).code(),
            },
        }
    }

    /// The digest of the engine's held-suffix byte count, for reproducibility
    /// evidence (the engine does not expose held bytes directly).
    pub fn held_digest(&self) -> ReproducibleDigest {
        ReproducibleDigest::of_bytes(&self.lexer.pending_bytes().to_le_bytes())
    }
}

/// Convenience: run hostile byte streams through the real engine until one
/// produces the `INVALID_UTF8` refusal, returning the failing stream.
///
/// Uses `seed` for full reproducibility; every call with the same seed and
/// generator limits yields the same campaign.
pub fn first_invalid_utf8_campaign(
    seed: u64,
    streams: usize,
    max_stream_len: usize,
) -> Option<(Vec<u8>, &'static str)> {
    let mut rng = SeededRng::new(seed);
    let mut adapter = FmdResumableAdapter::new("rust").ok()?;
    for _ in 0..streams {
        let mut generator = HostileByteGenerator::new(rng.next_u64(), max_stream_len);
        let stream = generator.generate();
        if let LexOutcome::Refused { code } = adapter.feed(&stream) {
            return Some((stream, code));
        }
    }
    None
}
