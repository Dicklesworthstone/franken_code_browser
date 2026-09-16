#![forbid(unsafe_code)]

use std::sync::Arc;

use fcb_core::{DocumentGeneration, DocumentId, FileId, SourceRevision};
use fcb_source::{CompleteCapture, ObservationDigest};
use franken_markdown::{
    FlowBudgets, FlowConstraints, HeadlessFlowConsumer, ResumableFlowDisplay,
    StepResult,
};

use crate::error::DocumentError;

/// Constraints for layout and rendering of a document in FCB.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentViewConstraints {
    pub viewport_width: u32,
    pub line_height: u32,
    pub char_width: u32,
    pub max_viewport_lines: Option<usize>,
}

impl Default for DocumentViewConstraints {
    fn default() -> Self {
        Self {
            viewport_width: 80,
            line_height: 16,
            char_width: 1,
            max_viewport_lines: None,
        }
    }
}

impl From<DocumentViewConstraints> for FlowConstraints {
    fn from(c: DocumentViewConstraints) -> Self {
        Self {
            viewport_width: c.viewport_width,
            line_height: c.line_height,
            char_width: c.char_width,
            max_viewport_lines: c.max_viewport_lines,
        }
    }
}

/// Work and resource budgets for document layout.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DocumentBudgets {
    pub max_blocks: usize,
    pub max_bytes: usize,
    pub max_lines: usize,
    pub max_table_cells: usize,
}

impl Default for DocumentBudgets {
    fn default() -> Self {
        Self {
            max_blocks: 100_000,
            max_bytes: 32 * 1024 * 1024,
            max_lines: 500_000,
            max_table_cells: 50_000,
        }
    }
}

impl From<DocumentBudgets> for FlowBudgets {
    fn from(b: DocumentBudgets) -> Self {
        Self {
            max_blocks: b.max_blocks,
            max_bytes: b.max_bytes,
            max_lines: b.max_lines,
            max_table_cells: b.max_table_cells,
        }
    }
}

/// One rendered line in continuous document flow.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentFlowLine {
    pub text: String,
    pub block_index: usize,
    pub local_line: usize,
    pub y_offset: u32,
    pub height: u32,
    pub is_continuation: bool,
}

/// Headless layout and measurement output produced by the upstream flow engine.
#[derive(Clone, Debug)]
pub struct HeadlessDocumentOutput {
    pub total_width: u32,
    pub total_height: u32,
    pub lines: Vec<DocumentFlowLine>,
    pub consumed_blocks: usize,
    pub consumed_bytes: usize,
    pub source_map: franken_markdown::DocumentSourceMap,
    pub semantic_fixture: String,
}

/// Synchronous-resumable document layout stepper.
pub struct ResumableDocumentLayout {
    inner: ResumableFlowDisplay,
    generation: DocumentGeneration,
    steps_taken: usize,
}

impl ResumableDocumentLayout {
    pub fn new(source: &str, generation: DocumentGeneration, batch_size: usize) -> Result<Self, DocumentError> {
        let inner = ResumableFlowDisplay::new(source, batch_size);
        Ok(Self {
            inner,
            generation,
            steps_taken: 0,
        })
    }

    pub fn generation(&self) -> DocumentGeneration {
        self.generation
    }

    pub fn steps_taken(&self) -> usize {
        self.steps_taken
    }

    pub fn is_finished(&self) -> bool {
        self.inner.is_finished()
    }

    pub fn step(&mut self) -> Result<Option<StepResult>, DocumentError> {
        self.steps_taken += 1;
        self.inner.step().map_err(DocumentError::from)
    }

    pub fn to_display_list(&self) -> franken_markdown::DisplayList {
        self.inner.to_display_list()
    }
}

/// Authoritative session tracking one document's source capture and layout state.
///
/// Refuses stale requests immediately without re-parsing or cloning the AST.
#[derive(Clone, Debug)]
pub struct DocumentSession {
    id: DocumentId,
    file_id: FileId,
    revision: SourceRevision,
    digest: ObservationDigest,
    generation: DocumentGeneration,
    source_text: Arc<str>,
}

impl DocumentSession {
    /// Creates a document session from a validated complete capture.
    pub fn new(
        id: DocumentId,
        capture: &CompleteCapture,
        generation: DocumentGeneration,
    ) -> Result<Self, DocumentError> {
        let file_id = capture.request().file();
        let revision = capture.request().revision();

        if id.owner() != file_id.owner()
            || id.owner() != revision.owner()
            || id.owner() != generation.owner()
        {
            return Err(DocumentError::OwnerMismatch);
        }

        let utf8 = std::str::from_utf8(capture.bytes()).map_err(|_| DocumentError::InvalidUtf8)?;

        Ok(Self {
            id,
            file_id,
            revision,
            digest: capture.digest(),
            generation,
            source_text: Arc::from(utf8),
        })
    }

    pub fn id(&self) -> DocumentId {
        self.id
    }

    pub fn file_id(&self) -> FileId {
        self.file_id
    }

    pub fn revision(&self) -> SourceRevision {
        self.revision
    }

    pub fn digest(&self) -> ObservationDigest {
        self.digest
    }

    pub fn generation(&self) -> DocumentGeneration {
        self.generation
    }

    pub fn source_text(&self) -> &str {
        &self.source_text
    }

    /// Validates an incoming request against this session's identity.
    ///
    /// Refuses stale or mismatched requests with zero parsing and zero AST cloning.
    pub fn validate_request(
        &self,
        file_id: FileId,
        revision: SourceRevision,
        digest: ObservationDigest,
        generation: DocumentGeneration,
    ) -> Result<(), DocumentError> {
        if file_id != self.file_id {
            return Err(DocumentError::MismatchedFile {
                expected: self.file_id,
                actual: file_id,
            });
        }
        if revision != self.revision {
            return Err(DocumentError::StaleRevision {
                expected: self.revision,
                actual: revision,
            });
        }
        if digest != self.digest {
            return Err(DocumentError::MismatchedDigest {
                expected: self.digest,
                actual: digest,
            });
        }
        if generation != self.generation {
            return Err(DocumentError::StaleRequest {
                expected: self.generation,
                actual: generation,
            });
        }
        Ok(())
    }

    /// Consumes the document through the upstream headless flow consumer.
    ///
    /// Validates the generation first; stale requests are rejected without work.
    pub fn consume_headless(
        &self,
        request_generation: DocumentGeneration,
        constraints: DocumentViewConstraints,
        budgets: DocumentBudgets,
    ) -> Result<HeadlessDocumentOutput, DocumentError> {
        if request_generation != self.generation {
            return Err(DocumentError::StaleRequest {
                expected: self.generation,
                actual: request_generation,
            });
        }

        let consumer = HeadlessFlowConsumer::new(constraints.into(), budgets.into());
        let output = consumer.consume_source(&self.source_text)?;

        let lines = output
            .lines
            .into_iter()
            .map(|l| DocumentFlowLine {
                text: l.text,
                block_index: l.block_index,
                local_line: l.local_line,
                y_offset: l.y_offset,
                height: l.height,
                is_continuation: l.is_continuation,
            })
            .collect();

        let semantic_fixture = output.to_semantic_fixture();

        Ok(HeadlessDocumentOutput {
            total_width: output.total_width,
            total_height: output.total_height,
            lines,
            consumed_blocks: output.consumed_blocks,
            consumed_bytes: output.consumed_bytes,
            source_map: output.source_map,
            semantic_fixture,
        })
    }

    /// Creates a synchronous-resumable flow display stepper for this session.
    pub fn create_resumable(
        &self,
        request_generation: DocumentGeneration,
        batch_size: usize,
    ) -> Result<ResumableDocumentLayout, DocumentError> {
        if request_generation != self.generation {
            return Err(DocumentError::StaleRequest {
                expected: self.generation,
                actual: request_generation,
            });
        }

        ResumableDocumentLayout::new(&self.source_text, self.generation, batch_size)
    }
}
