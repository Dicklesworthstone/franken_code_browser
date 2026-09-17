//! Hostile input generation, bounded mutation, and failure-preserving
//! minimization for FCB conformance lanes (FCB-059.A).
//!
//! Everything here is deterministic and dependency-free: a fixed-seed
//! generator reproduces the same hostile inputs on every host, digests are
//! stable functions of input bytes, and the minimizer shrinks a failing input
//! without ever changing its failure classification. The production
//! adapter over the real FCB-021 lexical engine lives in the upstream
//! franken_markdown test suite, which wires this crate's minimizer to
//! its own resumable engine.

#![forbid(unsafe_code)]
#![deny(missing_debug_implementations)]

use std::cell::Cell;

/// Deterministic SplitMix64 generator: same seed, same sequence, every host.
#[derive(Clone, Debug)]
pub struct SeededRng {
    state: u64,
}

impl SeededRng {
    /// Create a generator from an arbitrary seed.
    pub const fn new(seed: u64) -> Self {
        Self { state: seed }
    }

    /// Next 64-bit value (SplitMix64 finalizer).
    pub fn next_u64(&mut self) -> u64 {
        self.state = self.state.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.state;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    /// Uniform index in `0..bound`; `bound == 0` yields zero.
    pub fn below(&mut self, bound: usize) -> usize {
        if bound == 0 {
            return 0;
        }
        (self.next_u64() % bound as u64) as usize
    }

    /// Pick an element uniformly.
    pub fn pick<'a, T>(&mut self, items: &'a [T]) -> &'a T {
        &items[self.below(items.len())]
    }
}

/// A reproducible 128-bit digest: two independent FNV-1a lanes plus the byte
/// length, so truncation, reordering and extension all change the value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct ReproducibleDigest {
    lane_a: u64,
    lane_b: u64,
    len: u64,
}

impl ReproducibleDigest {
    const OFFSET_A: u64 = 0xCBF2_9CE4_8422_2325;
    const OFFSET_B: u64 = 0x6C62_272E_07BB_0142;
    const PRIME: u64 = 0x0000_0100_0000_01B3;

    /// Digest raw bytes.
    pub fn of_bytes(bytes: &[u8]) -> Self {
        let mut a = Self::OFFSET_A;
        let mut b = Self::OFFSET_B;
        for (index, byte) in bytes.iter().enumerate() {
            a ^= u64::from(*byte);
            a = a.wrapping_mul(Self::PRIME);
            // Second lane mixes position so translocations collide less.
            b ^= u64::from(*byte).rotate_left((index % 64) as u32);
            b = b.wrapping_mul(Self::PRIME);
        }
        Self {
            lane_a: a,
            lane_b: b,
            len: bytes.len() as u64,
        }
    }

    /// Digest a string's UTF-8 bytes.
    pub fn of_str(text: &str) -> Self {
        Self::of_bytes(text.as_bytes())
    }

    /// Lowercase hex of both lanes and the length.
    #[must_use]
    pub fn to_hex(self) -> String {
        format!("{:016x}{:016x}:{:x}", self.lane_a, self.lane_b, self.len)
    }
}

/// Bias table for hostile byte generation: control characters, UTF-8
/// continuation and lead bytes, delimiters and quote/fence starters.
const HOSTILE_BYTE_POOL: &[u8] = &[
    0x00, 0x01, 0x09, 0x0A, 0x0D, 0x22, 0x27, 0x5C, 0x60, 0x7E, 0x23, 0x2A, 0x2F,
    0x3C, 0x3E, 0x41, 0x7A, 0x30, 0x39, 0x80, 0xBF, 0xC3, 0xA9, 0xE6, 0xA5, 0xF0,
    0x9F, 0x98, 0x80, 0xFF, 0xFE, 0xC0,
];

/// Deterministic hostile byte streams: biased toward the bytes that break
/// lexical engines (control characters, UTF-8 boundaries, quote and fence
/// starters) while remaining fully reproducible from the seed.
#[derive(Clone, Debug)]
pub struct HostileByteGenerator {
    rng: SeededRng,
    max_len: usize,
}

impl HostileByteGenerator {
    /// Create a generator producing streams of at most `max_len` bytes.
    pub const fn new(seed: u64, max_len: usize) -> Self {
        let bounded = if max_len < 1 { 1 } else { max_len };
        Self {
            rng: SeededRng::new(seed),
            max_len: bounded,
        }
    }

    /// Generate one hostile byte stream.
    pub fn generate(&mut self) -> Vec<u8> {
        let len = 1 + self.rng.below(self.max_len);
        let mut out = Vec::with_capacity(len);
        for _ in 0..len {
            out.push(*self.rng.pick(HOSTILE_BYTE_POOL));
        }
        out
    }

    /// Mutate an existing stream: bit flips, byte replacement, truncation and
    /// insertion — each selected deterministically.
    pub fn mutate(&mut self, source: &[u8]) -> Vec<u8> {
        if source.is_empty() {
            return self.generate();
        }
        let mut out = source.to_vec();
        let mutations = 1 + self.rng.below(3);
        for _ in 0..mutations {
            let index = self.rng.below(out.len());
            match self.rng.below(4) {
                0 => out[index] ^= 1 << self.rng.below(8),
                1 => out[index] = *self.rng.pick(HOSTILE_BYTE_POOL),
                2 => out.truncate(index.max(1)),
                _ => out.insert(index, *self.rng.pick(HOSTILE_BYTE_POOL)),
            }
            if out.is_empty() {
                out.push(0x20);
            }
        }
        out
    }
}

/// Token-shaped fragments drawn from the FCB-021 lexical alphabet: fence
/// starters, string delimiters, escapes, comment openers and closers,
/// keywords, numbers and multi-byte identifiers. Concatenations of these are
/// precisely the inputs that break resumable lexers.
pub const LEXICAL_FRAGMENTS: &[&str] = &[
    "fn", "let", "struct", "impl", "\"", "\"\"", "\"\"\"", "'", "\\'", "/*",
    "*/", "//", "///", "```", "~~~", "\n", "\r\n", "{", "}", "(", ")", "[",
    "]", ";", ",", "+=", "->", "<<=", "0x1F", "1e10", "1_000", "é", "日",
    "\\u{1F600}", "\\n", " ", "  ", "\t",
];

/// Deterministic hostile *source* streams over the lexical fragment
/// alphabet.
#[derive(Clone, Debug)]
pub struct HostileStructureGenerator {
    rng: SeededRng,
    max_fragments: usize,
}

impl HostileStructureGenerator {
    /// Create a generator emitting at most `max_fragments` fragments.
    pub const fn new(seed: u64, max_fragments: usize) -> Self {
        let bounded = if max_fragments < 1 { 1 } else { max_fragments };
        Self {
            rng: SeededRng::new(seed),
            max_fragments: bounded,
        }
    }

    /// Generate one hostile source string.
    pub fn generate(&mut self) -> String {
        let count = 1 + self.rng.below(self.max_fragments);
        let mut out = String::new();
        for _ in 0..count {
            out.push_str(self.rng.pick(LEXICAL_FRAGMENTS));
        }
        out
    }

    /// Mutate a source string by splicing hostile fragments into it at
    /// character boundaries.
    pub fn splice(&mut self, source: &str) -> String {
        let mut out = source.to_owned();
        let splices = 1 + self.rng.below(3);
        for _ in 0..splices {
            let byte_index = self.rng.below(out.len() + 1);
            let mut index = byte_index;
            while index > 0 && !out.is_char_boundary(index) {
                index -= 1;
            }
            let fragment = self.rng.pick(LEXICAL_FRAGMENTS);
            out.insert_str(index, fragment);
        }
        out
    }
}

/// How far a minimization run may proceed.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TerminationBudget {
    /// Maximum classify attempts; zero means no attempts are allowed.
    pub max_attempts: usize,
}

impl TerminationBudget {
    /// A budget permitting `max_attempts` classify attempts.
    pub const fn attempts(max_attempts: usize) -> Self {
        Self { max_attempts }
    }
}

/// Report of a minimization run.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MinimizeReport {
    /// The smallest failing input found (never larger than the source).
    pub minimized: Vec<u8>,
    /// Classify attempts actually spent.
    pub attempts: usize,
    /// Whether the budget ran out before no further reduction was found.
    pub exhausted_budget: bool,
    /// Whether the final input still carries the original classification.
    pub classification_preserved: bool,
}

/// Reduce a failing input while preserving its failure classification.
///
/// `classify` returns `Some(classification)` when the input still fails with
/// that exact classification and `None` when it stops failing. A candidate is
/// kept only when the classification is unchanged, so minimization can never
/// convert one failure into another or into a pass. Delta-debugging proceeds
/// from large halved chunks down to single bytes and stops at the budget or
/// at a fixed point.
pub fn minimize<C: Copy + Eq>(
    source: &[u8],
    budget: TerminationBudget,
    classify: &mut dyn FnMut(&[u8]) -> Option<C>,
) -> MinimizeReport {
    let mut current = source.to_vec();
    let mut attempts = 0usize;
    let mut exhausted = false;
    let original = match classify(&current) {
        Some(classification) => classification,
        None => {
            // The source does not fail; minimization is meaningless.
            return MinimizeReport {
                minimized: current,
                attempts,
                exhausted_budget: false,
                classification_preserved: false,
            };
        }
    };
    let mut remaining = budget.max_attempts;

    let mut chunk_size = current.len().max(1) / 2;
    'outer: while chunk_size >= 1 {
        let mut index = 0usize;
        while index < current.len() {
            if remaining == 0 {
                exhausted = true;
                break 'outer;
            }
            remaining -= 1;
            attempts += 1;
            let candidate_end = (index + chunk_size).min(current.len());
            let mut candidate = Vec::with_capacity(current.len());
            candidate.extend_from_slice(&current[..index]);
            candidate.extend_from_slice(&current[candidate_end..]);
            if classify(&candidate) == Some(original) {
                current = candidate;
                continue;
            }
            index += 1;
        }
        if chunk_size == 1 {
            break;
        }
        chunk_size = (chunk_size / 2).max(1);
    }

    MinimizeReport {
        minimized: current,
        attempts,
        exhausted_budget: exhausted,
        classification_preserved: true,
    }
}

/// Counter for verifying that a hostile campaign respected its budgets.
#[derive(Debug)]
pub struct AttemptCounter {
    remaining: Cell<usize>,
}

impl AttemptCounter {
    /// A counter with `total` allowed attempts.
    pub const fn new(total: usize) -> Self {
        Self {
            remaining: Cell::new(total),
        }
    }

    /// Consume one attempt; `false` when the budget is exhausted.
    pub fn spend(&self) -> bool {
        let left = self.remaining.get();
        if left == 0 {
            return false;
        }
        self.remaining.set(left - 1);
        true
    }

    /// Attempts still available.
    pub const fn remaining(&self) -> usize {
        self.remaining.get()
    }
}

/// A single bounded event log entry recorded during hostile execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostileLogEvent {
    pub step: usize,
    pub phase: &'static str,
    pub detail: String,
}

pub mod hostile_document;
pub mod hostile_source;

