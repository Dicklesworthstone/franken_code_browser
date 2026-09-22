#![forbid(unsafe_code)]
#![cfg(all(feature = "map", feature = "search", feature = "markdown", unix))]

//! Production API composition: real discovery/capture/search, then the existing
//! upstream Markdown reader on the selected OLD capture. No native frame claim.
use std::{fs, sync::Arc, time::{SystemTime, UNIX_EPOCH}};
use fcb::{ArenaOwnerId, ByteLength, FileId, Size2D, SourceRevision};
use fcb_core::{DocumentGeneration, DocumentId};
use fcb::map::{LayoutOptions, LayoutRevision, ResourceAllocationId, ResourceBudget, RootId};
use fcb::map::workspace::{WorkspaceAtlas, WorkspaceAtlasLimits};
use fcb::map::workspace::text_search::{AtlasTextLimits, WorkspaceTextSource};
use fcb::search::{CompleteCapture, IndexLimits, QueryGeneration, RawPath, SearchManifestId};
use fcb::search::workspace::{RootGrant, WorkspaceCatalog, WorkspaceCaptures, WorkspaceLimits, WorkspaceStage};
use fcb::source::{CancelFlag, SourceError};
use fcb::document::reader::{DocumentReader, DocumentReadError};

#[test]
fn atlas_hit_opens_old_markdown_after_live_replacement_and_revocation_blocks_new_actions() {
    let owner = ArenaOwnerId::new(812).unwrap();
    let allocation = |n| ResourceAllocationId::new(n).unwrap();
    let generation = |n| QueryGeneration::new(owner, n).unwrap();
    let budget = ResourceBudget::new(owner, ByteLength::new(256 * 1024 * 1024)).unwrap();
    let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos();
    let root = std::env::temp_dir().join(format!("fcb-atlas-document-consumer-{}-{now}", std::process::id()));
    fs::create_dir(&root).unwrap();
    let original = b"# Original documentation\n\nThe **needle** remains exact.\n\n## Usage\n\nUse this version.\n";
    fs::write(root.join("README.md"), original).unwrap();
    let grant = RootGrant::new(RootId::new(owner, 1).unwrap(), RawPath::from_path(&root));
    let mut catalog = WorkspaceCatalog::open(grant.clone(), SearchManifestId::new(owner, 1).unwrap(),
        FileId::new(owner, 1).unwrap(), WorkspaceLimits { max_files: 32,
            max_file_bytes: 8192, max_source_bytes: 16384, ..Default::default() }, false, &budget, allocation(1)).unwrap();
    let cancel = CancelFlag::new();
    for _ in 0..4097 {
        if catalog.stage() != WorkspaceStage::Discovering { break; }
        catalog.step(&cancel).unwrap();
    }
    assert!(catalog.discovery_complete());
    let atlas = WorkspaceAtlas::build(&catalog, &fcb::map::workspace::AtlasScope::All, LayoutRevision::new(owner, 1).unwrap(),
        Size2D::new(1024.0, 768.0).unwrap(), LayoutOptions::modest(), WorkspaceAtlasLimits::default(),
        &budget, allocation(2), || false).unwrap();
    let before = atlas.layout().clone();
    let mut captures = WorkspaceCaptures::new(&catalog, SourceRevision::new(owner, 1).unwrap(), &budget, allocation(3)).unwrap();
    while !captures.finished() {
        captures.step(&cancel, |request, path, allowance| {
            let bytes = fs::read(root.join(path.raw().to_path_buf())).map_err(|_| SourceError::CaptureUnavailable)?;
            if bytes.len() > allowance { return Err(SourceError::PayloadTooLarge); }
            CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes))
        }).unwrap();
    }
    let source = WorkspaceTextSource::new(&atlas, &captures, &budget, allocation(4)).unwrap();
    let index = source.index(IndexLimits::default(), &budget, allocation(5), || false).unwrap();
    let overlay = index.search("needle", generation(1), AtlasTextLimits::default(), &budget,
        [allocation(6), allocation(7), allocation(8)], || false).unwrap();
    assert!(overlay.is_complete()); assert_eq!(overlay.hits().len(), 1);
    let selected = overlay.select_hit(&source, 0, generation(1)).unwrap();
    assert_eq!(selected.original_bytes(), b"needle");
    fs::write(root.join("README.md"), b"# Changed live source\n\nNo old document.\n").unwrap();
    let doc_generation = DocumentGeneration::new(owner, 1).unwrap();
    let reader = DocumentReader::prepare(selected.capture(), DocumentId::new(owner, 1).unwrap(),
        doc_generation, Default::default(), &budget, allocation(9), || false).unwrap();
    assert_eq!(reader.capture().bytes(), original);
    assert!(reader.rendered_text().contains("Original documentation"));
    assert!(reader.window_at_heading("usage", 5).unwrap().lines()[0].rendered_text.contains("Usage"));
    assert_eq!(atlas.layout(), &before, "document work must not repack geography");
    let duplicate = selected.capture().clone();
    assert_eq!(reader.validate_delivery(&duplicate, doc_generation), Err(DocumentReadError::StaleCapture));
    assert!(overlay.validate_delivery(&source, generation(2)).is_err());
    grant.revoke();
    assert!(overlay.select_hit(&source, 0, generation(1)).is_err());
    assert!(overlay.validate_delivery(&source, generation(1)).is_err());
    // Revocation cannot retract bytes already delivered; a host must validate
    // the overlay/grant before any new action or delayed reader publication.
}
