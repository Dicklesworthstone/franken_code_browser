#![forbid(unsafe_code)]

//! Export surfaces for the production ephemeral index. Segment images borrow
//! the actual completed segments for persistence. Owned transfer pins source
//! and moves those same segments for repeated, scoped in-memory queries.
//! A persistence host must bind images to source/manifest digests, retain a
//! trusted artifact identity, and preserve uncovered members when reopening.

#[path = "owned.rs"]
mod owned;
pub use owned::{OwnedEphemeralIndex, SourceRetentionLimits};

use super::{EphemeralIndex, SegmentCoverage};
use fcb_core::{FileId, SourceRevision};

#[derive(Clone, Copy, Debug)]
pub struct SegmentImage<'index> {
    file: FileId,
    revision: SourceRevision,
    utf8: bool,
    coverage: SegmentCoverage,
    grams: &'index [u32],
}
impl SegmentImage<'_> {
    pub const fn file(self) -> FileId { self.file }
    pub const fn revision(self) -> SourceRevision { self.revision }
    pub const fn source_is_utf8(self) -> bool { self.utf8 }
    pub const fn coverage(self) -> SegmentCoverage { self.coverage }
    /// Sorted unique 24-bit byte trigrams. An empty UNcovered segment is not
    /// evidence of absence; callers must retain coverage separately.
    pub fn grams(&self) -> &[u32] { self.grams }
}
impl EphemeralIndex<'_> {
    pub fn segment_image(&self, document_index: usize) -> Option<SegmentImage<'_>> {
        let segment = self.segments.get(document_index)?;
        let document = self.manifest.documents.get(document_index)?;
        Some(SegmentImage { file: document.file_id, revision: document.capture.request().revision(),
            utf8: segment.utf8, coverage: segment.coverage,
            grams: &self.grams[segment.start..segment.start + segment.len] })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{IndexLimits, ManifestLimits, MembershipState, SearchDocument, SearchManifest, SearchManifestId};
    use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget};
    use fcb_source::{CaptureRequest, CompleteCapture};
    use std::sync::Arc;

    #[test]
    fn exported_segment_is_the_same_sorted_set_the_production_prefilter_uses() {
        let owner = ArenaOwnerId::new(851).unwrap();
        let file = FileId::new(owner, 1).unwrap();
        let revision = SourceRevision::new(owner, 2).unwrap();
        let bytes = b"banana\xffbanana";
        let capture = CompleteCapture::new(CaptureRequest::new(file, revision).unwrap(),
            ByteLength::new(bytes.len() as u64), Arc::from(bytes.as_slice())).unwrap();
        let documents = [SearchDocument::new(file, "source", &capture)];
        let manifest = SearchManifest::new(SearchManifestId::new(owner, 1).unwrap(), &documents, &[],
            MembershipState::Closed, ManifestLimits::default()).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(1024 * 1024)).unwrap();
        let index = EphemeralIndex::build(manifest, IndexLimits::default(), &budget,
            ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        let image = index.segment_image(0).unwrap();
        let mut expected: Vec<_> = bytes.windows(3).map(|b| u32::from_be_bytes([0, b[0], b[1], b[2]])).collect();
        expected.sort_unstable(); expected.dedup();
        assert_eq!(image.grams(), expected);
        assert!(!image.source_is_utf8());
        assert_eq!(image.file(), file); assert_eq!(image.revision(), revision);
        assert_eq!(image.coverage(), SegmentCoverage::Indexed);
        assert!(index.segment_image(1).is_none());
    }

    #[test]
    fn rejected_segments_remain_uncovered_instead_of_empty_negative_certificates() {
        let owner = ArenaOwnerId::new(852).unwrap();
        let file = FileId::new(owner, 1).unwrap();
        let capture = CompleteCapture::new(CaptureRequest::new(file, SourceRevision::new(owner, 1).unwrap()).unwrap(),
            ByteLength::new(6), Arc::from(b"banana".as_slice())).unwrap();
        let documents = [SearchDocument::new(file, "source", &capture)];
        let manifest = SearchManifest::new(SearchManifestId::new(owner, 1).unwrap(), &documents, &[],
            MembershipState::Closed, ManifestLimits::default()).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(1024 * 1024)).unwrap();
        let index = EphemeralIndex::build(manifest, IndexLimits { max_total_grams: 0, ..IndexLimits::default() },
            &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
        let image = index.segment_image(0).unwrap();
        assert!(matches!(image.coverage(), SegmentCoverage::Uncovered(_)));
        assert!(image.grams().is_empty());
    }
}
