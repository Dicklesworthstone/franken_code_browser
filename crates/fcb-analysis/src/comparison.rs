#![forbid(unsafe_code)]

//! Byte-exact correspondence between two explicitly retained captures (FCB-056.A).
//! Bounded Myers search, with verified common prefix/suffix and an explicit
//! unresolved interior when work/edit-distance admission is exhausted. No path
//! lookup, source recapture, Git invocation, normalization, or line decoding.
//! Original-byte ranges may cut a Unicode scalar; they are NOT text positions.
//!
//! A deterministic alignment of repeated bytes is not proof of file continuity,
//! a rename, or annotation identity. Callers must separately authorize any
//! reattachment. This synchronous operation belongs on a worker; every byte
//! comparison and frontier cell spends a finite work allowance.

use std::mem::size_of;
use fcb_core::{ByteLength, ByteOffset, ByteRange, QueryGeneration,
    ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb_source::{CaptureRequest, CompleteCapture};

pub const MAX_DIFF_EDITS: usize = 512;
pub const MAX_DIFF_SOURCE_BYTES: usize = 16 * 1024 * 1024;
pub const MAX_DIFF_WORK: u64 = 64 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ComparisonLimits {
    pub max_source_bytes: usize,
    pub max_edit_distance: usize,
    pub max_work: u64,
}
impl Default for ComparisonLimits {
    fn default() -> Self {
        Self { max_source_bytes: MAX_DIFF_SOURCE_BYTES, max_edit_distance: 256,
            max_work: 8 * 1024 * 1024 }
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparisonError { Limits, OwnerMismatch, ResourceDenied, Canceled, Stale, InvalidRange }
impl ComparisonError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limits => "COMPARISON_LIMIT", Self::OwnerMismatch => "COMPARISON_OWNER_MISMATCH",
            Self::ResourceDenied => "COMPARISON_RESOURCE_DENIED", Self::Canceled => "COMPARISON_CANCELED",
            Self::Stale => "COMPARISON_STALE", Self::InvalidRange => "COMPARISON_INVALID_RANGE",
        }
    }
}
impl std::fmt::Display for ComparisonError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for ComparisonError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparisonQuality { Exact, WorkLimit, EditLimit }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ComparisonRelation { Identical, Different, Undetermined }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CorrespondenceKind { Equal, Changed, Unresolved }
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Correspondence {
    kind: CorrespondenceKind,
    before: ByteRange,
    after: ByteRange,
}
impl Correspondence {
    pub const fn kind(self) -> CorrespondenceKind { self.kind }
    pub const fn before(self) -> ByteRange { self.before }
    pub const fn after(self) -> ByteRange { self.after }
}
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct ComparisonStats {
    pub work_units: u64,
    pub frontier_cells: u64,
    pub prefix_equal_bytes: usize,
    pub suffix_equal_bytes: usize,
    /// Shortest insert/delete BYTE distance, only when refinement completed.
    pub edit_distance: Option<usize>,
}

/// Owns only bounded correspondence/trace reservations; borrows exact source.
/// Segments partition both captures completely, even when an interior is
/// unresolved. Equal spans are always byte-verified, never a hash-only guess.
pub struct CaptureComparison<'a> {
    before: &'a CompleteCapture,
    after: &'a CompleteCapture,
    generation: QueryGeneration,
    spans: Vec<Correspondence>,
    quality: ComparisonQuality,
    relation: ComparisonRelation,
    stats: ComparisonStats,
    _lease: ResourceLease,
}
impl<'a> CaptureComparison<'a> {
    pub fn build(before: &'a CompleteCapture, after: &'a CompleteCapture,
        generation: QueryGeneration, limits: ComparisonLimits, budget: &ResourceBudget,
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool) -> Result<Self, ComparisonError> {
        let owner = generation.owner();
        if before.request().file().owner() != owner || after.request().file().owner() != owner {
            return Err(ComparisonError::OwnerMismatch);
        }
        let (a, b) = (before.bytes(), after.bytes());
        if limits.max_source_bytes > MAX_DIFF_SOURCE_BYTES || a.len() > limits.max_source_bytes
            || b.len() > limits.max_source_bytes || limits.max_edit_distance > MAX_DIFF_EDITS
            || limits.max_work > MAX_DIFF_WORK { return Err(ComparisonError::Limits); }
        if canceled() { return Err(ComparisonError::Canceled); }
        let d = limits.max_edit_distance;
        let trace_capacity = (d + 1) * (d + 2) / 2;
        let span_capacity = 2 * d + 5;
        let charge = size_of::<Self>() + trace_capacity * size_of::<usize>()
            + 2 * span_capacity * size_of::<Correspondence>();
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charge as u64))
            .map_err(|_| ComparisonError::ResourceDenied)?;
        let mut spans = reserve(span_capacity)?;
        let mut reverse = reserve(span_capacity)?;
        let mut trace = reserve(trace_capacity)?;
        let mut work = Work { used: 0, limit: limits.max_work };
        let mut stats = ComparisonStats::default();
        let mut different = a.len() != b.len();
        let mut prefix = 0;
        while prefix < a.len().min(b.len()) {
            if !work.spend(&mut canceled)? { break; }
            if a[prefix] != b[prefix] { different = true; break; }
            prefix += 1;
        }
        let mut suffix = 0;
        while suffix < a.len().min(b.len()) - prefix {
            if !work.spend(&mut canceled)? { break; }
            if a[a.len() - 1 - suffix] != b[b.len() - 1 - suffix] { different = true; break; }
            suffix += 1;
        }
        let old_end = a.len() - suffix;
        let new_end = b.len() - suffix;
        append(&mut spans, CorrespondenceKind::Equal, 0, prefix, 0, prefix);
        let n = old_end - prefix;
        let m = new_end - prefix;
        let quality = if n == 0 || m == 0 {
            append(&mut spans, CorrespondenceKind::Changed, prefix, old_end, prefix, new_end);
            stats.edit_distance = Some(n + m);
            ComparisonQuality::Exact
        } else {
            let mut result = ComparisonQuality::EditLimit;
            'search: for distance in 0..=d {
                for diagonal in (-(distance as isize)..=distance as isize).step_by(2) {
                    if !work.spend(&mut canceled)? { result = ComparisonQuality::WorkLimit; break 'search; }
                    stats.frontier_cells += 1;
                    let candidate = predecessor(&trace, distance, diagonal, n, m);
                    let Some((mut x, _, _)) = candidate else { trace.push(usize::MAX); continue; };
                    let mut y = (x as isize - diagonal) as usize;
                    while x < n && y < m {
                        if !work.spend(&mut canceled)? { result = ComparisonQuality::WorkLimit; break 'search; }
                        if a[prefix + x] != b[prefix + y] { different = true; break; }
                        x += 1; y += 1;
                    }
                    trace.push(x);
                    if x == n && y == m {
                        // Bound backtracking and append work as well as search.
                        let reconstruction = 4 * (distance as u64 + 1);
                        if reconstruction > work.limit - work.used {
                            result = ComparisonQuality::WorkLimit; break 'search;
                        }
                        work.used += reconstruction;
                        let (mut x, mut y) = (n, m);
                        for level in (0..=distance).rev() {
                            if canceled() { return Err(ComparisonError::Canceled); }
                            let k = x as isize - y as isize;
                            let (sx, px, py) = predecessor(&trace, level, k, n, m)
                                .ok_or(ComparisonError::InvalidRange)?;
                            let sy = (sx as isize - k) as usize;
                            if x > sx { reverse.push(span(CorrespondenceKind::Equal,
                                prefix + sx, prefix + x, prefix + sy, prefix + y)); }
                            if level > 0 { reverse.push(span(CorrespondenceKind::Changed,
                                prefix + px, prefix + sx, prefix + py, prefix + sy)); }
                            x = px; y = py;
                        }
                        for item in reverse.iter().rev() {
                            append(&mut spans, item.kind, item.before.start().get() as usize,
                                item.before.end().get() as usize, item.after.start().get() as usize,
                                item.after.end().get() as usize);
                        }
                        stats.edit_distance = Some(distance);
                        result = ComparisonQuality::Exact;
                        break 'search;
                    }
                }
            }
            if result != ComparisonQuality::Exact {
                append(&mut spans, CorrespondenceKind::Unresolved, prefix, old_end, prefix, new_end);
            }
            result
        };
        append(&mut spans, CorrespondenceKind::Equal, old_end, a.len(), new_end, b.len());
        if canceled() { return Err(ComparisonError::Canceled); }
        stats.work_units = work.used;
        stats.prefix_equal_bytes = prefix;
        stats.suffix_equal_bytes = suffix;
        let relation = if quality == ComparisonQuality::Exact {
            if stats.edit_distance == Some(0) { ComparisonRelation::Identical } else { ComparisonRelation::Different }
        } else if different { ComparisonRelation::Different } else { ComparisonRelation::Undetermined };
        // Trace and reverse buffers drop now; the lease conservatively retains
        // their admitted capacity until the result is released, with no copying.
        Ok(Self { before, after, generation, spans, quality, relation, stats, _lease: lease })
    }
    pub fn before(&self) -> &'a CompleteCapture { self.before }
    pub fn after(&self) -> &'a CompleteCapture { self.after }
    pub const fn generation(&self) -> QueryGeneration { self.generation }
    pub const fn quality(&self) -> ComparisonQuality { self.quality }
    pub const fn relation(&self) -> ComparisonRelation { self.relation }
    pub const fn stats(&self) -> ComparisonStats { self.stats }
    pub fn spans(&self) -> &[Correspondence] { &self.spans }
    pub fn before_bytes(&self, ordinal: usize) -> Option<&'a [u8]> {
        let range = self.spans.get(ordinal)?.before.as_usize_bounds().ok()?;
        self.before.bytes().get(range.0..range.1)
    }
    pub fn after_bytes(&self, ordinal: usize) -> Option<&'a [u8]> {
        let range = self.spans.get(ordinal)?.after.as_usize_bounds().ok()?;
        self.after.bytes().get(range.0..range.1)
    }
    pub fn validate_delivery(&self, before: CaptureRequest, after: CaptureRequest,
        generation: QueryGeneration) -> Result<(), ComparisonError> {
        if &before != self.before.request() || &after != self.after.request() || generation != self.generation {
            return Err(ComparisonError::Stale);
        }
        Ok(())
    }
    /// Only nonempty ranges wholly inside a verified equal span are mapped.
    /// Ambiguous insertion boundaries/cross-edit ranges return None. This is
    /// correspondence in THIS alignment, not automatic annotation reattachment.
    pub fn corresponding_after(&self, range: ByteRange) -> Option<ByteRange> {
        if range.is_empty() { return None; }
        let item = self.spans.iter().find(|item| item.kind == CorrespondenceKind::Equal
            && item.before.start() <= range.start() && item.before.end() >= range.end())?;
        let start = item.after.start().get() + range.start().get() - item.before.start().get();
        ByteRange::new(ByteOffset::new(start), ByteOffset::new(start + range.len().get())).ok()
    }
}

struct Work { used: u64, limit: u64 }
impl Work {
    fn spend(&mut self, canceled: &mut impl FnMut() -> bool) -> Result<bool, ComparisonError> {
        if canceled() { return Err(ComparisonError::Canceled); }
        if self.used == self.limit { return Ok(false); }
        self.used += 1; Ok(true)
    }
}
fn row(distance: usize) -> usize { distance * (distance + 1) / 2 }
fn previous(trace: &[usize], distance: usize, k: isize) -> Option<usize> {
    if k.unsigned_abs() > distance || (k + distance as isize) % 2 != 0 { return None; }
    let value = *trace.get(row(distance) + ((k + distance as isize) / 2) as usize)?;
    (value != usize::MAX).then_some(value)
}
// Returns the snake's start x and its predecessor endpoint. Paths that leave
// the finite edit grid are ineligible. Ties choose insertion deterministically.
fn predecessor(trace: &[usize], distance: usize, k: isize, n: usize, m: usize) -> Option<(usize, usize, usize)> {
    if distance == 0 { return Some((0, 0, 0)); }
    let right = previous(trace, distance - 1, k - 1).and_then(|x| {
        let y = x as isize + 1 - k;
        (x < n && y >= 0 && y <= m as isize).then_some((x + 1, x, y.max(0) as usize))
    });
    let down = previous(trace, distance - 1, k + 1).and_then(|x| {
        let y = x as isize - k;
        (y > 0 && y <= m as isize).then_some((x, x, y.saturating_sub(1).max(0) as usize))
    });
    match (right, down) {
        (Some(a), Some(b)) => Some(if b.0 >= a.0 { b } else { a }),
        (a, b) => a.or(b),
    }
}
fn span(kind: CorrespondenceKind, a: usize, b: usize, c: usize, d: usize) -> Correspondence {
    Correspondence { kind,
        before: ByteRange::new(ByteOffset::new(a as u64), ByteOffset::new(b as u64)).expect("ordered admitted offsets"),
        after: ByteRange::new(ByteOffset::new(c as u64), ByteOffset::new(d as u64)).expect("ordered admitted offsets") }
}
fn append(items: &mut Vec<Correspondence>, kind: CorrespondenceKind, a: usize, b: usize, c: usize, d: usize) {
    if a == b && c == d { return; }
    if let Some(last) = items.last_mut() {
        if last.kind == kind && last.before.end().get() == a as u64 && last.after.end().get() == c as u64 {
            *last = span(kind, last.before.start().get() as usize, b, last.after.start().get() as usize, d);
            return;
        }
    }
    items.push(span(kind, a, b, c, d));
}
fn reserve<T>(count: usize) -> Result<Vec<T>, ComparisonError> {
    let mut values = Vec::new();
    values.try_reserve_exact(count).map_err(|_| ComparisonError::ResourceDenied)?;
    if values.capacity() > count { return Err(ComparisonError::ResourceDenied); }
    Ok(values)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use fcb_core::{ArenaOwnerId, FileId, SourceRevision};
    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(856).unwrap() }
    fn generation() -> QueryGeneration { QueryGeneration::new(owner(), 1).unwrap() }
    fn capture(rev: u64, bytes: &[u8]) -> CompleteCapture {
        CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), 1).unwrap(), SourceRevision::new(owner(), rev).unwrap()).unwrap(),
            ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
    }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(16 * 1024 * 1024)).unwrap() }
    fn allocation() -> ResourceAllocationId { ResourceAllocationId::new(1).unwrap() }
    fn verify(diff: &CaptureComparison<'_>) {
        let (mut a, mut b) = (0, 0);
        let mut reconstructed = Vec::new();
        for (i, item) in diff.spans().iter().enumerate() {
            assert_eq!(item.before.start().get(), a); assert_eq!(item.after.start().get(), b);
            if item.kind == CorrespondenceKind::Equal {
                assert_eq!(diff.before_bytes(i), diff.after_bytes(i));
                reconstructed.extend_from_slice(diff.before_bytes(i).unwrap());
            } else { reconstructed.extend_from_slice(diff.after_bytes(i).unwrap()); }
            a = item.before.end().get(); b = item.after.end().get();
        }
        assert_eq!(a, diff.before.bytes().len() as u64); assert_eq!(b, diff.after.bytes().len() as u64);
        assert_eq!(reconstructed, diff.after.bytes());
    }
    fn distance(a: &[u8], b: &[u8]) -> usize {
        let mut last = vec![0; b.len() + 1];
        for x in a {
            let mut next = vec![0; b.len() + 1];
            for (j, y) in b.iter().enumerate() { next[j + 1] = if x == y { last[j] + 1 } else { last[j + 1].max(next[j]) }; }
            last = next;
        }
        a.len() + b.len() - 2 * last[b.len()]
    }
    #[test]
    fn exhaustive_small_inputs_match_independent_lcs_distance_and_reconstruct_exact_bytes() {
        let strings: Vec<Vec<u8>> = (0..31usize).map(|i| {
            let n = (usize::BITS - (i + 1).leading_zeros() - 1) as usize;
            (0..n).map(|j| b'a' + ((i + 1) >> j & 1) as u8).collect()
        }).collect();
        let budget = budget();
        for a in &strings { for b in &strings {
            let old = capture(1, a); let new = capture(2, b);
            let diff = CaptureComparison::build(&old, &new, generation(), ComparisonLimits::default(), &budget, allocation(), || false).unwrap();
            verify(&diff); assert_eq!(diff.quality(), ComparisonQuality::Exact);
            assert_eq!(diff.stats().edit_distance, Some(distance(a, b)));
        } }
    }
    #[test]
    fn every_work_limit_preserves_ranges_and_never_labels_unexamined_bytes_equal() {
        let old = capture(1, b"prefix-ababab-old-ababab-suffix");
        let new = capture(2, b"prefix-bababa-new-bababa-suffix");
        let budget = budget();
        for max_work in 0..256 {
            let diff = CaptureComparison::build(&old, &new, generation(), ComparisonLimits { max_work, ..Default::default() },
                &budget, allocation(), || false).unwrap();
            verify(&diff); assert!(diff.stats().work_units <= max_work);
            assert_ne!(diff.relation(), ComparisonRelation::Identical);
            if diff.quality() != ComparisonQuality::Exact { assert!(diff.spans().iter().any(|s| s.kind == CorrespondenceKind::Unresolved)); }
        }
    }
    #[test]
    fn edit_limit_retains_known_context_and_explicit_unknown_middle() {
        let old = capture(1, b"same OLD tail"); let new = capture(2, b"same NEW tail");
        let budget = budget();
        let diff = CaptureComparison::build(&old, &new, generation(), ComparisonLimits { max_edit_distance: 0, ..Default::default() },
            &budget, allocation(), || false).unwrap();
        verify(&diff); assert_eq!(diff.quality(), ComparisonQuality::EditLimit);
        assert_eq!(diff.relation(), ComparisonRelation::Different);
        assert_eq!(diff.spans()[1].kind(), CorrespondenceKind::Unresolved);
    }
    #[test]
    fn unchanged_range_mapping_refuses_cross_edit_and_zero_width_ambiguity() {
        let old = capture(1, b"head tail"); let new = capture(2, b"head INSERT tail");
        let budget = budget();
        let diff = CaptureComparison::build(&old, &new, generation(), ComparisonLimits::default(), &budget, allocation(), || false).unwrap();
        let old_tail = ByteRange::new(ByteOffset::new(5), ByteOffset::new(9)).unwrap();
        assert_eq!(diff.corresponding_after(old_tail).unwrap().start().get(), 12);
        assert!(diff.corresponding_after(ByteRange::new(ByteOffset::new(0), ByteOffset::new(9)).unwrap()).is_none());
        assert!(diff.corresponding_after(ByteRange::new(ByteOffset::new(5), ByteOffset::new(5)).unwrap()).is_none());
        assert!(diff.validate_delivery(*new.request(), *old.request(), generation()).is_err());
    }
    #[test]
    fn arbitrary_bytes_utf16_units_and_repeated_lines_remain_original_ranges() {
        let a = [0xff, 0xfe, b'a', 0, 0, 0xd8, 0, 0xdc, 13, 0, 10, 0];
        let b = [0xff, 0xfe, b'b', 0, 0, 0xd8, 0, 0xdc, 13, 0, 10, 0];
        let budget = budget();
        let old = capture(1, &a); let new = capture(2, &b);
        verify(&CaptureComparison::build(&old, &new, generation(), ComparisonLimits::default(), &budget, allocation(), || false).unwrap());
        let old = capture(1, &b"same\n".repeat(4000)); let new = capture(2, &b"else\n".repeat(4000));
        let diff = CaptureComparison::build(&old, &new, generation(), ComparisonLimits { max_work: 1000, ..Default::default() },
            &budget, allocation(), || false).unwrap();
        verify(&diff); assert!(diff.stats().work_units <= 1000);
    }
    #[test]
    fn denied_and_canceled_candidates_release_capacity_without_a_partial_report() {
        let old = capture(1, b"abc"); let new = capture(2, b"def");
        let tiny = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
        assert!(matches!(CaptureComparison::build(&old, &new, generation(), ComparisonLimits::default(), &tiny, allocation(), || false), Err(ComparisonError::ResourceDenied)));
        let budget = budget();
        assert!(matches!(CaptureComparison::build(&old, &new, generation(), ComparisonLimits::default(), &budget, allocation(),
            || budget.accounting().reserved().get() > 0), Err(ComparisonError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
