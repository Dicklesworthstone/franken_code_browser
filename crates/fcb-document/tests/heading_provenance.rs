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


#[test]
fn segmented_document_audit_reserves_one_full_ast_plus_only_the_live_slice() {
    let owner = ArenaOwnerId::new(806).unwrap();
    let paragraph = "Ordinary source remains exact after a bounded independent provenance audit.\n\n";
    let source = format!("# Segmented document\n\n{}## Tail\n", paragraph.repeat(2200));
    assert!(source.len() > 128 * 1024);
    let capture = CompleteCapture::new(CaptureRequest::new(FileId::new(owner, 1).unwrap(),
        SourceRevision::new(owner, 1).unwrap()).unwrap(), ByteLength::new(source.len() as u64),
        Arc::from(source.as_bytes())).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    let reader = DocumentReader::prepare(&capture, DocumentId::new(owner, 1).unwrap(),
        DocumentGeneration::new(owner, 1).unwrap(),
        DocumentReadOptions { max_source_bytes: 256 * 1024, ..Default::default() },
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    // No reference definitions: the unchanged conservative parser coefficient
    // is 512 bytes per input byte. The second AST is only one short paragraph,
    // not another whole document. This budget cannot admit two full ASTs.
    let full = source.len() as u64 * 512;
    let audit_capacity = full + 256 * 1024 + paragraph.len() as u64 * 512 + 4096;
    assert!(audit_capacity < full * 2);
    let audit = ResourceBudget::new(owner, ByteLength::new(audit_capacity)).unwrap();
    reader.qualify_heading_source_ranges(&audit, ResourceAllocationId::new(2).unwrap(), || false).unwrap();
    assert!(audit.accounting().peak_reserved().get() > full + 256 * 1024,
        "the isolated parse must have its own admitted live workspace");
    assert!(audit.accounting().peak_reserved().get() <= audit_capacity);
    assert_eq!(audit.accounting().reserved().get(), 0);
    assert_eq!(audit.accounting().active_allocations(), 0);
    assert_eq!(reader.heading("tail").unwrap().title, "Tail");
    assert_eq!(reader.capture().bytes(), source.as_bytes());
}

#[test]
fn isolated_ast_growth_refusal_and_cancellation_release_scratch_and_keep_the_reader() {
    let owner = ArenaOwnerId::new(807).unwrap();
    let source = "a".repeat(32 * 1024);
    let capture = CompleteCapture::new(CaptureRequest::new(FileId::new(owner, 1).unwrap(),
        SourceRevision::new(owner, 1).unwrap()).unwrap(), ByteLength::new(source.len() as u64),
        Arc::from(source.as_bytes())).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    let reader = DocumentReader::prepare(&capture, DocumentId::new(owner, 1).unwrap(),
        DocumentGeneration::new(owner, 1).unwrap(), DocumentReadOptions::default(),
        &budget, ResourceAllocationId::new(1).unwrap(), || false).unwrap();
    let full = source.len() as u64 * 512 + 256 * 1024;
    let denied = ResourceBudget::new(owner, ByteLength::new(full + 1024)).unwrap();
    let mut saw_full_ast_admission = false;
    assert_eq!(reader.qualify_heading_source_ranges(&denied, ResourceAllocationId::new(2).unwrap(), || {
        saw_full_ast_admission |= denied.accounting().reserved().get() == full;
        false
    }), Err(DocumentReadError::ResourceDenied));
    assert!(saw_full_ast_admission);
    assert_eq!(denied.accounting().peak_reserved().get(), full,
        "failed slice growth must not change the ledger");
    assert_eq!(denied.accounting().reserved().get(), 0);
    let audit = ResourceBudget::new(owner, ByteLength::new(full * 2)).unwrap();
    let mut stopped_at_slice = false;
    assert_eq!(reader.qualify_heading_source_ranges(&audit, ResourceAllocationId::new(3).unwrap(), || {
        stopped_at_slice |= audit.accounting().reserved().get() > full;
        stopped_at_slice
    }), Err(DocumentReadError::Canceled));
    assert!(stopped_at_slice);
    assert_eq!(audit.accounting().reserved().get(), 0);
    assert_eq!(audit.accounting().active_allocations(), 0);
    assert_eq!(reader.capture().bytes(), source.as_bytes());
    assert!(reader.window(0, 1).unwrap().lines()[0].text.starts_with('a'));
}
