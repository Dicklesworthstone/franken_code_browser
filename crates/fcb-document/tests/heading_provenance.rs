#![forbid(unsafe_code)]

use std::sync::Arc;
use fcb_core::{ArenaOwnerId, ByteLength, DocumentGeneration, DocumentId, FileId,
    ResourceAllocationId, ResourceBudget, SourceRevision};
use fcb_source::{CaptureRequest, CompleteCapture};
use fcb_document::reader::{DocumentReader, DocumentReadError, DocumentReadOptions};

#[test]
fn exact_heading_audit_has_separate_admission_and_does_not_disable_preview_rendering() {
    let owner = ArenaOwnerId::new(804).unwrap();
    let source = b"# Introduction\n\nOrdinary **formatted** text.\n\n## Target\n";
    let capture = CompleteCapture::new(CaptureRequest::new(FileId::new(owner, 1).unwrap(),
        SourceRevision::new(owner, 1).unwrap()).unwrap(), ByteLength::new(source.len() as u64), Arc::from(source.as_slice())).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    let reader = DocumentReader::prepare(&capture, DocumentId::new(owner, 1).unwrap(),
        DocumentGeneration::new(owner, 1).unwrap(), DocumentReadOptions::default(), &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    reader.qualify_heading_source_ranges(&budget, ResourceAllocationId::new(2).unwrap(), || false).unwrap();
    let denied = ResourceBudget::new(owner, ByteLength::new(1)).unwrap();
    assert_eq!(reader.qualify_heading_source_ranges(&denied, ResourceAllocationId::new(3).unwrap(), || false),
        Err(DocumentReadError::ResourceDenied));
    assert_eq!(reader.qualify_heading_source_ranges(&budget, ResourceAllocationId::new(4).unwrap(), || true),
        Err(DocumentReadError::Canceled));
    assert!(reader.rendered_text().contains("formatted"));
    assert_eq!(reader.capture().bytes(), source);
    assert!(!reader.window(0, 1).unwrap().lines().is_empty());
}

#[test]
fn canonical_slug_collisions_never_choose_the_first_heading() {
    let owner = ArenaOwnerId::new(805).unwrap();
    let source = b"# Repeat\n\n# Repeat\n\n# Repeat-2\n";
    let capture = CompleteCapture::new(CaptureRequest::new(FileId::new(owner, 1).unwrap(),
        SourceRevision::new(owner, 1).unwrap()).unwrap(), ByteLength::new(source.len() as u64), Arc::from(source.as_slice())).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    let reader = DocumentReader::prepare(&capture, DocumentId::new(owner, 1).unwrap(),
        DocumentGeneration::new(owner, 1).unwrap(), DocumentReadOptions::default(), &budget,
        ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    reader.qualify_heading_source_ranges(&budget, ResourceAllocationId::new(2).unwrap(), || false).unwrap();
    assert_eq!(reader.heading("repeat").unwrap().title, "Repeat");
    assert_eq!(reader.heading("repeat-2").err(), Some(DocumentReadError::InvalidOutput));
    assert_eq!(reader.window_at_heading("repeat-2", 1).err(), Some(DocumentReadError::InvalidOutput));
}
