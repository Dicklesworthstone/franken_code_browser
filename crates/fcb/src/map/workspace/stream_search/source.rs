#![forbid(unsafe_code)]

//! Borrowed metadata shared by CLI and retained native atlases. This adapter
//! never constructs a layout, clones catalog entries, or opens source files.

use fcb_core::FileId;
use crate::map::{AtlasNodeId, PartitionLayout};
use crate::search::workspace::WorkspaceCatalog;
use super::super::{WorkspaceAtlas, WorkspaceAtlasError, retained::RetainedWorkspaceAtlas};

mod sealed {
    pub trait Sealed {}
    impl Sealed for super::WorkspaceAtlas<'_> {}
    impl Sealed for super::RetainedWorkspaceAtlas {}
}

/// The read-only metadata authority used by whole-file atlas search. Only
/// library-built atlases implement this trait. Search authority is the complete
/// base catalog, including explicitly partial discovery; a query may select a
/// frozen ordinal projection without changing any base identity. Display-filtered
/// authority is rejected before the source-opening callback can run.
pub trait AtlasStreamSource: sealed::Sealed {
    fn catalog(&self) -> &WorkspaceCatalog;
    fn layout(&self) -> &PartitionLayout;
    fn file_count(&self) -> usize;
    fn node_for_file(&self, file: FileId) -> Result<AtlasNodeId, WorkspaceAtlasError>;
    fn validate_active(&self) -> Result<(), WorkspaceAtlasError>;
    fn discovery_complete(&self) -> bool { self.catalog().discovery_complete() }
}

impl AtlasStreamSource for WorkspaceAtlas<'_> {
    fn catalog(&self) -> &WorkspaceCatalog { WorkspaceAtlas::catalog(self) }
    fn layout(&self) -> &PartitionLayout { WorkspaceAtlas::layout(self) }
    fn file_count(&self) -> usize { WorkspaceAtlas::file_count(self) }
    fn node_for_file(&self, file: FileId) -> Result<AtlasNodeId, WorkspaceAtlasError> {
        WorkspaceAtlas::node_for_file(self, file)
    }
    fn validate_active(&self) -> Result<(), WorkspaceAtlasError> { WorkspaceAtlas::validate_active(self) }
}

impl AtlasStreamSource for RetainedWorkspaceAtlas {
    fn catalog(&self) -> &WorkspaceCatalog { RetainedWorkspaceAtlas::catalog(self) }
    fn layout(&self) -> &PartitionLayout { RetainedWorkspaceAtlas::layout(self) }
    fn file_count(&self) -> usize { RetainedWorkspaceAtlas::file_count(self) }
    fn node_for_file(&self, file: FileId) -> Result<AtlasNodeId, WorkspaceAtlasError> {
        RetainedWorkspaceAtlas::node_for_file(self, file)
    }
    fn validate_active(&self) -> Result<(), WorkspaceAtlasError> { RetainedWorkspaceAtlas::validate_active(self) }
}
