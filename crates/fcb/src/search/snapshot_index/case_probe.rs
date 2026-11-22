#![forbid(unsafe_code)]

//! Bounded necessary conditions over the existing raw-byte grams. A lowercase
//! query uses at most eight ASCII spellings per gram; only a separately
//! qualified ASCII source may borrow those negative certificates. Unicode
//! matching, occurrence identity and original spelling stay with the verifier.

use std::ops::Range;
use super::Coverage;

pub(super) const MAX_PROBE_KEYS: usize = 24;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum MatchPolicy { Bytes, ExactText, AsciiLowercase }
impl MatchPolicy {
    pub(super) fn text(case_sensitive: bool) -> Self {
        if case_sensitive { Self::ExactText } else { Self::AsciiLowercase }
    }
    pub(super) fn compatible(self, coverage: Coverage) -> bool {
        match self {
            Self::Bytes => coverage.indexed(),
            Self::ExactText => coverage.utf8(),
            Self::AsciiLowercase => coverage == Coverage::Ascii,
        }
    }
}

pub(super) struct GramProbes {
    pub(super) keys: [u32; MAX_PROBE_KEYS],
    pub(super) groups: [Range<usize>; 3],
    pub(super) count: usize,
}
impl GramProbes {
    pub(super) fn new(needle: &[u8], policy: MatchPolicy) -> Self {
        let mut probes = Self { keys: [0; MAX_PROBE_KEYS], groups: [0..0, 0..0, 0..0], count: 0 };
        if needle.len() < 3 || (policy == MatchPolicy::AsciiLowercase && !needle.is_ascii()) { return probes; }
        let last = needle.len() - 3;
        for (group, offset) in [0, last / 2, last].into_iter().enumerate() {
            let bytes = &needle[offset..offset + 3];
            let start = probes.count;
            if policy == MatchPolicy::AsciiLowercase {
                let lower = [bytes[0].to_ascii_lowercase(), bytes[1].to_ascii_lowercase(), bytes[2].to_ascii_lowercase()];
                let folded = gram(&lower);
                let bits = gram(&lower.map(|byte| if byte.is_ascii_lowercase() { 0x20 } else { 0 }));
                let mut variant = bits;
                loop {
                    probes.keys[probes.count] = folded ^ variant;
                    probes.count += 1;
                    if variant == 0 { break; }
                    variant = (variant - 1) & bits;
                }
            } else {
                probes.keys[probes.count] = gram(bytes);
                probes.count += 1;
            }
            probes.groups[group] = start..probes.count;
        }
        probes
    }
    pub(super) fn fallback_all(&self) -> bool { self.count == 0 }
    /// Sum is a conservative union-size estimate. Duplicate members make a
    /// driver less selective, never incomplete; their output is deduplicated.
    pub(super) fn rarest(&self, lists: &[Range<usize>; MAX_PROBE_KEYS]) -> Range<usize> {
        self.groups.iter().min_by_key(|group| (**group).clone().map(|i| lists[i].len()).sum::<usize>())
            .cloned().unwrap_or(0..0)
    }
}

fn gram(bytes: &[u8]) -> u32 { u32::from_be_bytes([0, bytes[0], bytes[1], bytes[2]]) }
