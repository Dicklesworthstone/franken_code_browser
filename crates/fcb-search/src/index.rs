#![forbid(unsafe_code)]

//! Ephemeral source-capture indexes (FCB-027 / FCB-085).
//!
//! The host supplies immutable, authorized captures, in FileId order. Borrowing
//! pins that exact universe: this module never reopens a path or discovers files.
//! Each file has an immutable sorted set of byte trigrams. A missing trigram is
//! a negative certificate only for a fully indexed, compatible representation.
//! Quota-refused files remain in the universe and take the direct-scan route.
//!
//! Index construction and query steps belong on a source/search worker, not an
//! interaction callback. Per-file sorting is bounded by the scratch admission;
//! it is not preemptible. This is an in-memory index, not a persistence format.

/// Immutable segment images for explicit, digest-bound persistence adapters.
pub mod export;

use std::mem::size_of;

use fcb_core::{
    ArenaOwnerId, ByteLength, FileId, ResourceAllocationId,
    ResourceBudget, ResourceLease,
};
use fcb_source::DetectedEncoding;

use crate::{
    ParsedQuery, QueryError, QueryOptions, SearchDocument,
    SearchMode, UnicodeNormalization,
};

/// A source-universe revision, deliberately distinct from a query generation.
/// Hosts allocate a fresh, non-reused value for each changed membership/capture
/// snapshot. Neither equal paths nor equal lengths establish this identity.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub struct SearchManifestId {
    owner: ArenaOwnerId,
    revision: u64,
}

impl SearchManifestId {
    pub fn new(owner: ArenaOwnerId, revision: u64) -> Result<Self, IndexError> {
        if revision == 0 { return Err(IndexError::InvalidManifest); }
        Ok(Self { owner, revision })
    }
    pub const fn owner(self) -> ArenaOwnerId { self.owner }
    pub const fn revision(self) -> u64 { self.revision }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum MembershipState {
    /// The caller has enumerated the complete eligible, authorized universe.
    Closed,
    /// Additional members may still be discovered. No global completeness claim.
    Discovering,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ManifestLimits {
    pub max_files: usize,
    pub max_path_bytes: usize,
}

impl Default for ManifestLimits {
    fn default() -> Self { Self { max_files: 1_000_000, max_path_bytes: 16_384 } }
}

/// Validated immutable membership. Paths are borrowed UTF-8 *search keys*, not
/// native-path authority. FileId and the captured SourceRevision name hits.
/// Native adapters retain their reversible raw paths separately.
///
/// `unavailable` contains eligible identities for which the host could not pin
/// a capture. Both lists must be strictly FileId-sorted and disjoint. Exclusions
/// decided by the host before forming this scope are not unavailable captures.
#[derive(Clone, Copy, Debug)]
pub struct SearchManifest<'a> {
    id: SearchManifestId,
    documents: &'a [SearchDocument<'a>],
    unavailable: &'a [FileId],
    membership: MembershipState,
}

impl<'a> SearchManifest<'a> {
    pub fn new(
        id: SearchManifestId,
        documents: &'a [SearchDocument<'a>],
        unavailable: &'a [FileId],
        membership: MembershipState,
        limits: ManifestLimits,
    ) -> Result<Self, IndexError> {
        let members = documents.len().checked_add(unavailable.len())
            .ok_or(IndexError::LimitExceeded)?;
        if members > limits.max_files { return Err(IndexError::LimitExceeded); }
        let mut previous = None;
        for doc in documents {
            if doc.file_id.owner() != id.owner
                || doc.capture.request().revision().owner() != id.owner {
                return Err(IndexError::OwnerMismatch);
            }
            if doc.file_id != doc.capture.request().file()
                || previous.is_some_and(|last| last >= doc.file_id) {
                return Err(IndexError::InvalidManifest);
            }
            if doc.path.len() > limits.max_path_bytes { return Err(IndexError::LimitExceeded); }
            previous = Some(doc.file_id);
        }
        previous = None;
        let mut present = 0;
        for &file in unavailable {
            if file.owner() != id.owner { return Err(IndexError::OwnerMismatch); }
            if previous.is_some_and(|last| last >= file) {
                return Err(IndexError::InvalidManifest);
            }
            while present < documents.len() && documents[present].file_id < file { present += 1; }
            if present < documents.len() && documents[present].file_id == file {
                return Err(IndexError::InvalidManifest);
            }
            previous = Some(file);
        }
        Ok(Self { id, documents, unavailable, membership })
    }
    pub const fn id(self) -> SearchManifestId { self.id }
    pub const fn documents(self) -> &'a [SearchDocument<'a>] { self.documents }
    pub const fn unavailable(self) -> &'a [FileId] { self.unavailable }
    pub const fn membership(self) -> MembershipState { self.membership }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum IndexError {
    InvalidManifest,
    OwnerMismatch,
    LimitExceeded,
    AllocationFailed,
    ResourceDenied,
    Canceled,
    StaleQuery,
    Query(QueryError),
}
impl From<QueryError> for IndexError {
    fn from(error: QueryError) -> Self { Self::Query(error) }
}
impl std::fmt::Display for IndexError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::InvalidManifest => "INDEX_INVALID_MANIFEST",
            Self::OwnerMismatch => "INDEX_OWNER_MISMATCH",
            Self::LimitExceeded => "INDEX_LIMIT_EXCEEDED",
            Self::AllocationFailed => "INDEX_ALLOCATION_FAILED",
            Self::ResourceDenied => "INDEX_RESOURCE_DENIED",
            Self::Canceled => "INDEX_CANCELED",
            Self::StaleQuery => "INDEX_STALE_QUERY",
            Self::Query(error) => error.code(),
        })
    }
}
impl std::error::Error for IndexError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct IndexLimits {
    pub max_source_bytes_per_file: usize,
    pub max_source_bytes_total: u64,
    pub max_grams_per_file: usize,
    pub max_total_grams: usize,
    pub max_scratch_bytes: usize,
}
impl Default for IndexLimits {
    fn default() -> Self {
        Self {
            max_source_bytes_per_file: 256 * 1024,
            max_source_bytes_total: 256 * 1024 * 1024,
            max_grams_per_file: 65_536,
            max_total_grams: 2 * 1024 * 1024,
            max_scratch_bytes: 1024 * 1024,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UncoveredReason { SourceLimit, ScratchLimit, FileGramLimit, TotalGramLimit, BuildByteLimit }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SegmentCoverage { Indexed, Uncovered(UncoveredReason) }

#[derive(Clone, Copy, Debug)]
struct Segment {
    start: usize,
    len: usize,
    utf8: bool,
    coverage: SegmentCoverage,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct IndexStatistics {
    pub indexed_files: usize,
    pub uncovered_files: usize,
    pub source_bytes_examined: u64,
    pub unique_grams: usize,
    /// Index-owned capacity, not borrowed captures or total process footprint.
    pub retained_bytes: u64,
    pub peak_reserved_bytes: u64,
}

/// A capture-bound collection of immutable per-file substring segments.
/// Query preparation allocates no repository-sized candidate list. Three needle
/// trigrams are probed per file; exact verification eliminates false positives.
/// The metadata traversal is O(files), not a claimed global inverted index.
#[derive(Debug)]
pub struct EphemeralIndex<'a> {
    manifest: SearchManifest<'a>,
    segments: Vec<Segment>,
    grams: Vec<u32>,
    statistics: IndexStatistics,
    // Last field: index buffers are destroyed before their reservation releases.
    _lease: ResourceLease,
}

impl<'a> EphemeralIndex<'a> {
    /// Reserve metadata, worst-admitted gram capacity and reusable scratch
    /// *before* constructing them. A canceled candidate never publishes a
    /// partial replacement and releases its own lease, not an older index's.
    pub fn build(
        manifest: SearchManifest<'a>,
        limits: IndexLimits,
        budget: &ResourceBudget,
        allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool,
    ) -> Result<Self, IndexError> {
        if canceled() { return Err(IndexError::Canceled); }
        let mut gram_capacity = 0usize;
        let mut scratch_capacity = 0usize;
        let mut admitted_bytes = 0u64;
        for doc in manifest.documents {
            if canceled() { return Err(IndexError::Canceled); }
            let size = doc.capture.bytes().len();
            if source_refusal(size, limits).is_some() { continue; }
            if size as u64 > limits.max_source_bytes_total.saturating_sub(admitted_bytes) { continue; }
            admitted_bytes += size as u64;
            let count = size.saturating_sub(2);
            scratch_capacity = scratch_capacity.max(count);
            gram_capacity = gram_capacity.saturating_add(count.min(limits.max_grams_per_file))
                .min(limits.max_total_grams);
        }
        let planned = allocation_bytes(manifest.documents.len(), gram_capacity, scratch_capacity)?;
        let lease = budget.try_reserve_managed(manifest.id.owner, allocation, ByteLength::new(planned))
            .map_err(|_| IndexError::ResourceDenied)?;
        let mut segments = Vec::new();
        let mut grams = Vec::new();
        let mut scratch = Vec::new();
        reserve(&mut segments, manifest.documents.len())?;
        reserve(&mut grams, gram_capacity)?;
        reserve(&mut scratch, scratch_capacity)?;
        let actual = allocation_bytes(segments.capacity(), grams.capacity(), scratch.capacity())?;
        // try_reserve_exact may return extra allocator capacity. Do not publish
        // an allocation outside the admitted envelope under a smaller charge.
        if actual > planned { return Err(IndexError::ResourceDenied); }
        let mut statistics = IndexStatistics { peak_reserved_bytes: planned, ..Default::default() };
        for doc in manifest.documents {
            if canceled() { return Err(IndexError::Canceled); }
            let bytes = doc.capture.bytes();
            let mut refusal = source_refusal(bytes.len(), limits);
            if refusal.is_none() && bytes.len() as u64 > limits.max_source_bytes_total
                .saturating_sub(statistics.source_bytes_examined) {
                refusal = Some(UncoveredReason::BuildByteLimit);
            }
            let mut segment = Segment {
                start: grams.len(), len: 0, utf8: false,
                coverage: SegmentCoverage::Indexed,
            };
            if refusal.is_none() {
                statistics.source_bytes_examined += bytes.len() as u64;
                scratch.clear();
                for (offset, triple) in bytes.windows(3).enumerate() {
                    if offset % 16_384 == 0 && canceled() { return Err(IndexError::Canceled); }
                    scratch.push(gram(triple));
                }
                if canceled() { return Err(IndexError::Canceled); }
                scratch.sort_unstable();
                scratch.dedup();
                if canceled() { return Err(IndexError::Canceled); }
                if scratch.len() > limits.max_grams_per_file {
                    refusal = Some(UncoveredReason::FileGramLimit);
                } else if scratch.len() > gram_capacity.saturating_sub(grams.len()) {
                    refusal = Some(UncoveredReason::TotalGramLimit);
                } else {
                    segment.utf8 = std::str::from_utf8(bytes).is_ok();
                    segment.len = scratch.len();
                    grams.extend_from_slice(&scratch);
                }
            }
            if let Some(reason) = refusal {
                segment.coverage = SegmentCoverage::Uncovered(reason);
                statistics.uncovered_files += 1;
            } else {
                statistics.indexed_files += 1;
            }
            segments.push(segment);
        }
        if canceled() { return Err(IndexError::Canceled); }
        statistics.unique_grams = grams.len();
        // No shrink_to_fit copy/overlap: charge the actual retained capacities.
        drop(scratch);
        statistics.retained_bytes = allocation_bytes(segments.capacity(), grams.capacity(), 0)?;
        lease.reconcile(ByteLength::new(statistics.retained_bytes))
            .map_err(|_| IndexError::ResourceDenied)?;
        Ok(Self { manifest, segments, grams, statistics, _lease: lease })
    }

    pub const fn manifest(&self) -> SearchManifest<'a> { self.manifest }
    pub const fn statistics(&self) -> IndexStatistics { self.statistics }
    pub fn segment_coverage(&self, file: FileId) -> Option<SegmentCoverage> {
        self.manifest.documents.binary_search_by_key(&file, |doc| doc.file_id)
            .ok().map(|index| self.segments[index].coverage)
    }

    /// A false return is an exact negative certificate for the primary needle
    /// only. True means "verify", never "there is a match". Unknown encodings
    /// and normalization cannot borrow a byte-index negative certificate.
    pub fn may_match(
        &self, document_index: usize, query: &ParsedQuery, options: &QueryOptions,
    ) -> Result<bool, IndexError> {
        self.validate_options(options)?;
        let segment = self.segments.get(document_index).ok_or(IndexError::InvalidManifest)?;
        let needle = query.primary_needle.as_bytes();
        if needle.is_empty() { return Err(QueryError::EmptyNeedle.into()); }
        if needle.len() < 3 || !self.compatible(segment, options) { return Ok(true); }
        let keys = &self.grams[segment.start..segment.start + segment.len];
        let last = needle.len() - 3;
        for offset in [0, last / 2, last] {
            if keys.binary_search(&gram(&needle[offset..offset + 3])).is_err() { return Ok(false); }
        }
        Ok(true)
    }

    pub(crate) fn uses_fallback(&self, document_index: usize, options: &QueryOptions, needle_len: usize) -> bool {
        needle_len < 3 || !self.compatible(&self.segments[document_index], options)
    }

    pub(crate) fn validate_options(&self, options: &QueryOptions) -> Result<(), IndexError> {
        if options.generation.owner() != self.manifest.id.owner { return Err(IndexError::OwnerMismatch); }
        Ok(())
    }

    fn compatible(&self, segment: &Segment, options: &QueryOptions) -> bool {
        if segment.coverage != SegmentCoverage::Indexed { return false; }
        match options.mode {
            SearchMode::RawBytes => true,
            SearchMode::DecodedText { case_sensitive: true, normalization: UnicodeNormalization::Exact } => {
                segment.utf8 && matches!(options.declared_encoding,
                    None | Some(DetectedEncoding::Utf8 { .. }))
            }
            _ => false,
        }
    }
}

fn source_refusal(size: usize, limits: IndexLimits) -> Option<UncoveredReason> {
    if size > limits.max_source_bytes_per_file { return Some(UncoveredReason::SourceLimit); }
    if size.saturating_sub(2) > limits.max_scratch_bytes / size_of::<u32>() {
        return Some(UncoveredReason::ScratchLimit);
    }
    None
}
fn gram(bytes: &[u8]) -> u32 {
    (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2])
}
fn reserve<T>(items: &mut Vec<T>, count: usize) -> Result<(), IndexError> {
    items.try_reserve_exact(count).map_err(|_| IndexError::AllocationFailed)
}
fn allocation_bytes(segments: usize, grams: usize, scratch: usize) -> Result<u64, IndexError> {
    let bytes = segments.checked_mul(size_of::<Segment>())
        .and_then(|n| grams.checked_add(scratch).and_then(|g| g.checked_mul(size_of::<u32>()))
            .and_then(|g| n.checked_add(g)))
        .and_then(|n| n.checked_add(size_of::<EphemeralIndex<'_>>()))
        .ok_or(IndexError::LimitExceeded)?;
    u64::try_from(bytes).map_err(|_| IndexError::LimitExceeded)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;
    use fcb_core::{QueryGeneration, SourceRevision};
    use fcb_source::{CaptureRequest, CompleteCapture};

    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(61).unwrap() }
    fn capture(number: u64, text: &[u8]) -> CompleteCapture {
        CompleteCapture::new(CaptureRequest::new(FileId::new(owner(), number).unwrap(),
            SourceRevision::new(owner(), number).unwrap()).unwrap(),
            ByteLength::new(text.len() as u64), Arc::from(text)).unwrap()
    }
    fn manifest<'a>(docs: &'a [SearchDocument<'a>]) -> SearchManifest<'a> {
        SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), docs, &[],
            MembershipState::Closed, ManifestLimits::default()).unwrap()
    }
    fn budget() -> ResourceBudget { ResourceBudget::new(owner(), ByteLength::new(1_000_000)).unwrap() }
    fn allocation(id: u64) -> ResourceAllocationId { ResourceAllocationId::new(id).unwrap() }
    fn options() -> QueryOptions { QueryOptions::new(QueryGeneration::new(owner(), 3).unwrap()) }

    #[test]
    fn manifest_rejects_duplicates_wrong_captures_and_unavailable_overlap() {
        let c = capture(1, b"one");
        let d = SearchDocument::new(c.request().file(), "one.rs", &c);
        assert!(SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(),
            &[d.clone(), d.clone()], &[], MembershipState::Closed, ManifestLimits::default()).is_err());
        let wrong = SearchDocument::new(FileId::new(owner(), 2).unwrap(), "one.rs", &c);
        assert!(SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &[wrong], &[],
            MembershipState::Closed, ManifestLimits::default()).is_err());
        assert!(SearchManifest::new(SearchManifestId::new(owner(), 1).unwrap(), &[d],
            &[c.request().file()], MembershipState::Closed, ManifestLimits::default()).is_err());
    }

    #[test]
    fn substring_candidates_have_no_false_negatives_and_eliminate_absent_grams() {
        let c = capture(1, b"banana bandana");
        let docs = [SearchDocument::new(c.request().file(), "fruit.rs", &c)];
        let index = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &budget(), allocation(1), || false).unwrap();
        for start in 0..c.bytes().len() {
            for end in start + 1..=c.bytes().len() {
                let needle = std::str::from_utf8(&c.bytes()[start..end]).unwrap();
                // Parse a quoted phrase so spaces remain part of the needle.
                let query = ParsedQuery::parse(&format!("\"{needle}\"")).unwrap();
                assert!(index.may_match(0, &query, &options()).unwrap());
            }
        }
        assert!(!index.may_match(0, &ParsedQuery::parse("missing").unwrap(), &options()).unwrap());
    }

    #[test]
    fn every_quota_refusal_keeps_a_direct_scan_candidate() {
        let c = capture(1, b"abcdefghi");
        let docs = [SearchDocument::new(c.request().file(), "file.rs", &c)];
        let query = ParsedQuery::parse("abc").unwrap();
        for limits in [
            IndexLimits { max_source_bytes_per_file: 2, ..Default::default() },
            IndexLimits { max_scratch_bytes: 0, ..Default::default() },
            IndexLimits { max_grams_per_file: 0, ..Default::default() },
            IndexLimits { max_total_grams: 0, ..Default::default() },
            IndexLimits { max_source_bytes_total: 0, ..Default::default() },
        ] {
            let index = EphemeralIndex::build(manifest(&docs), limits, &budget(), allocation(1), || false).unwrap();
            assert_eq!(index.statistics().uncovered_files, 1);
            assert!(index.may_match(0, &query, &options()).unwrap());
        }
    }

    #[test]
    fn incompatible_semantics_never_use_raw_negative_certificates() {
        let c = capture(1, "Straße".as_bytes());
        let docs = [SearchDocument::new(c.request().file(), "file.rs", &c)];
        let index = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &budget(), allocation(1), || false).unwrap();
        let query = ParsedQuery::parse("STRASSE").unwrap();
        for normalization in [UnicodeNormalization::Exact, UnicodeNormalization::CaseFold, UnicodeNormalization::Canonical] {
            let opts = options().with_mode(SearchMode::DecodedText { case_sensitive: false, normalization });
            assert!(index.may_match(0, &query, &opts).unwrap());
        }
        assert!(index.may_match(0, &query, &options().with_encoding(DetectedEncoding::Utf16Le)).unwrap());
    }

    #[test]
    fn malformed_utf8_is_a_text_candidate_even_when_grams_are_absent() {
        let c = capture(1, b"ab\xffcd");
        let docs = [SearchDocument::new(c.request().file(), "file.rs", &c)];
        let index = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &budget(), allocation(1), || false).unwrap();
        let query = ParsedQuery::parse("missing").unwrap();
        assert!(index.may_match(0, &query, &options()).unwrap());
        assert!(!index.may_match(0, &query, &options().with_mode(SearchMode::RawBytes)).unwrap());
    }

    #[test]
    fn leases_cover_old_new_overlap_and_cancellation_releases_only_candidate() {
        let c = capture(1, b"abcdefghi");
        let docs = [SearchDocument::new(c.request().file(), "file.rs", &c)];
        let budget = budget();
        let first = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &budget, allocation(1), || false).unwrap();
        let retained = budget.accounting().reserved().get();
        assert_eq!(retained, first.statistics().retained_bytes);
        let second = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &budget, allocation(2), || false).unwrap();
        assert_eq!(budget.accounting().reserved().get(), retained * 2);
        drop(second);
        let mut checks = 0;
        let canceled = EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &budget, allocation(3), || {
            checks += 1; checks == 4
        });
        assert!(matches!(canceled, Err(IndexError::Canceled)));
        assert_eq!(budget.accounting().reserved().get(), retained);
        drop(first);
        assert_eq!(budget.accounting().reserved().get(), 0);
    }

    #[test]
    fn insufficient_shared_budget_refuses_before_any_index_is_published() {
        let c = capture(1, b"abcdefghi");
        let docs = [SearchDocument::new(c.request().file(), "file.rs", &c)];
        let budget = ResourceBudget::new(owner(), ByteLength::new(1)).unwrap();
        assert!(matches!(EphemeralIndex::build(manifest(&docs), IndexLimits::default(), &budget,
            allocation(1), || false), Err(IndexError::ResourceDenied)));
        assert_eq!(budget.accounting().reserved().get(), 0);
    }
}
