#![forbid(unsafe_code)]

//! Annotation continuity must preserve original source evidence. A digest is a
//! change detector; only a retained, identity-checked byte witness can authorize
//! attachment to another revision. These tests use the public production API.

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::{AnnotationId, AnnotationReattachment, AnnotationRegistry, CaptureRequest,
    CompleteCapture, OldAnchor, OrphanReason, SourceAnnotation, SourceError, fnv1a};

fn owner() -> ArenaOwnerId { ArenaOwnerId::new(98_301).unwrap() }
fn file(value: u64) -> FileId { FileId::new(owner(), value).unwrap() }
fn revision(value: u64) -> SourceRevision { SourceRevision::new(owner(), value).unwrap() }
fn capture(file: FileId, revision: SourceRevision, bytes: &[u8]) -> CompleteCapture {
    CompleteCapture::new(CaptureRequest::new(file, revision).unwrap(),
        ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
}
fn note(id: u64, file: FileId, revision: SourceRevision, offset: usize, witness: &[u8]) -> SourceAnnotation {
    SourceAnnotation::new(AnnotationId(id), file, revision, OldAnchor::new(offset, revision.get()),
        fnv1a(witness), "Keep this exact source evidence".to_string())
}
fn assert_orphan(registry: &AnnotationRegistry, annotation: &SourceAnnotation,
    reason: OrphanReason, current: SourceRevision) {
    assert!(registry.get_active(annotation.id).is_none());
    let orphan = registry.get_orphaned(annotation.id).unwrap();
    assert_eq!(&orphan.annotation, annotation, "orphaning must preserve the original note and anchor");
    assert_eq!(orphan.reason, reason);
    assert_eq!(orphan.orphaned_at_revision, Some(current));
}

#[test]
fn digest_only_equal_bytes_cannot_authorize_another_revision() {
    let original = note(1, file(1), revision(1), 5, b"important");
    let mut registry = AnnotationRegistry::new();
    registry.add_annotation(original.clone());
    let result = registry.reattach_for_file_modification(file(1), revision(2),
        b"head important tail", b"important".len());
    assert!(matches!(result.as_slice(), [AnnotationReattachment::Orphaned(_)]));
    assert_orphan(&registry, &original, OrphanReason::ExactWitnessUnavailable, revision(2));
    assert_eq!(registry.active_count(), 0);
    assert_eq!(registry.orphaned_count(), 1);
}

#[test]
fn retained_original_bytes_reattach_across_changes_outside_the_anchor() {
    let original = capture(file(1), revision(1), b"head important old tail");
    let current = capture(file(1), revision(2), b"head important new tail");
    let annotation = note(1, file(1), revision(1), 5, b"important");
    let unrelated = note(2, file(2), revision(1), 5, b"important");
    let mut registry = AnnotationRegistry::new();
    registry.add_annotation(annotation.clone());
    registry.add_annotation(unrelated.clone());
    let result = registry.reattach_for_file_modification_verified(&original, &current, 9).unwrap();
    assert_eq!(result, [AnnotationReattachment::Reattached {
        annotation_id: annotation.id, file_id: file(1), new_revision: revision(2),
        new_anchor: OldAnchor::new(5, 2),
    }]);
    let attached = registry.get_active(annotation.id).unwrap();
    assert_eq!(attached.revision, revision(2));
    assert_eq!(attached.anchor, OldAnchor::new(5, 2));
    assert_eq!(attached.expected_digest, annotation.expected_digest);
    assert_eq!(attached.text, annotation.text);
    assert_eq!(registry.get_active(unrelated.id), Some(&unrelated));
    assert_eq!(registry.orphaned_count(), 0);
    assert_eq!(original.bytes(), b"head important old tail");
    assert_eq!(current.bytes(), b"head important new tail");

    // The accepted revision becomes the required witness for the next edit.
    let latest = capture(file(1), revision(3), b"head important another tail");
    registry.reattach_for_file_modification_verified(&current, &latest, 9).unwrap();
    assert_eq!(registry.get_active(annotation.id).unwrap().revision, revision(3));
}

#[test]
fn changed_anchor_bytes_preserve_the_original_annotation() {
    let original = capture(file(1), revision(1), b"head important tail");
    let current = capture(file(1), revision(2), b"head different tail");
    let annotation = note(1, file(1), revision(1), 5, b"important");
    let mut registry = AnnotationRegistry::new();
    registry.add_annotation(annotation.clone());
    let result = registry.reattach_for_file_modification_verified(&original, &current, 9).unwrap();
    assert!(matches!(result.as_slice(), [AnnotationReattachment::Orphaned(_)]));
    assert_orphan(&registry, &annotation, OrphanReason::ContentDiverged, revision(2));
}

#[test]
fn original_revision_anchor_and_digest_must_all_match_the_retained_witness() {
    let original = capture(file(1), revision(1), b"head important tail");
    let current = capture(file(1), revision(3), b"head important tail");
    let wrong_revision = note(1, file(1), revision(2), 5, b"important");
    let mut wrong_anchor = note(2, file(1), revision(1), 5, b"important");
    wrong_anchor.anchor.revision = 2;
    let mut wrong_owner = note(3, file(1), revision(1), 5, b"important");
    wrong_owner.revision = SourceRevision::new(ArenaOwnerId::new(98_302).unwrap(), 1).unwrap();
    let wrong_digest = note(4, file(1), revision(1), 5, b"different");
    let mut registry = AnnotationRegistry::new();
    for annotation in [&wrong_revision, &wrong_anchor, &wrong_owner, &wrong_digest] {
        registry.add_annotation(annotation.clone());
    }
    registry.reattach_for_file_modification_verified(&original, &current, 9).unwrap();
    for annotation in [&wrong_revision, &wrong_anchor, &wrong_owner, &wrong_digest] {
        assert_orphan(&registry, annotation, OrphanReason::ExactWitnessUnavailable, revision(3));
    }

    // A digest that happens to agree with current bytes does not substitute for
    // the original retained evidence named by the annotation.
    let changed = capture(file(1), revision(2), b"head different tail");
    let forged = note(5, file(1), revision(1), 5, b"different");
    registry.add_annotation(forged.clone());
    registry.reattach_for_file_modification_verified(&original, &changed, 9).unwrap();
    assert_orphan(&registry, &forged, OrphanReason::ExactWitnessUnavailable, revision(2));
}

#[test]
fn invalid_capture_pairs_are_rejected_before_registry_changes() {
    let original = capture(file(1), revision(1), b"important");
    let current = capture(file(1), revision(2), b"important");
    let annotation = note(1, file(1), revision(1), 0, b"important");
    let mut registry = AnnotationRegistry::new();
    registry.add_annotation(annotation.clone());
    let foreign_owner = ArenaOwnerId::new(98_302).unwrap();
    let foreign = capture(FileId::new(foreign_owner, 1).unwrap(),
        SourceRevision::new(foreign_owner, 2).unwrap(), b"important");
    let unrelated = capture(file(2), revision(2), b"important");
    let ranged_request = CaptureRequest::new(file(1), revision(2)).unwrap()
        .with_range(ByteRange::new(ByteOffset::new(5), ByteOffset::new(14)).unwrap()).unwrap();
    let ranged = CompleteCapture::new(ranged_request, ByteLength::new(9), Arc::from(&b"important"[..])).unwrap();

    for (candidate, expected) in [
        (&foreign, SourceError::ForeignOwner),
        (&unrelated, SourceError::MetadataMismatch),
        (&ranged, SourceError::InvalidRange),
        (&original, SourceError::StaleCapture),
    ] {
        assert_eq!(registry.reattach_for_file_modification_verified(&original, candidate, 9), Err(expected));
        assert_eq!(registry.get_active(annotation.id), Some(&annotation));
        assert_eq!(registry.orphaned_count(), 0);
    }
    assert_eq!(registry.reattach_for_file_modification_verified(&ranged, &current, 9),
        Err(SourceError::InvalidRange));
    assert_eq!(registry.reattach_for_file_modification_verified(&original, &current, 0),
        Err(SourceError::InvalidRange));
    assert_eq!(registry.get_active(annotation.id), Some(&annotation));
    assert_eq!(registry.orphaned_count(), 0);
}

#[test]
fn unavailable_original_ranges_and_overflowing_anchors_never_reattach() {
    let original = capture(file(1), revision(1), b"short");
    let current = capture(file(1), revision(2), b"head important tail");
    let missing_witness = note(1, file(1), revision(1), 5, b"important");
    let overflowing = note(2, file(1), revision(1), usize::MAX, b"important");
    let beyond = note(3, file(1), revision(1), current.bytes().len(), b"important");
    let mut registry = AnnotationRegistry::new();
    for annotation in [&missing_witness, &overflowing, &beyond] {
        registry.add_annotation(annotation.clone());
    }
    registry.reattach_for_file_modification_verified(&original, &current, 9).unwrap();
    assert_orphan(&registry, &missing_witness, OrphanReason::ExactWitnessUnavailable, revision(2));
    assert_orphan(&registry, &overflowing, OrphanReason::OffsetOutOfBounds, revision(2));
    assert_orphan(&registry, &beyond, OrphanReason::OffsetOutOfBounds, revision(2));

    // The digest-only compatibility route must also use checked range math.
    registry.add_annotation(overflowing.clone());
    registry.reattach_for_file_modification(file(1), revision(2), current.bytes(), 9);
    assert_orphan(&registry, &overflowing, OrphanReason::OffsetOutOfBounds, revision(2));
}
