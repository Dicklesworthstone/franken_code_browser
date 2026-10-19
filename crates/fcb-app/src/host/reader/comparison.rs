#![forbid(unsafe_code)]

//! Explicit cross-reader comparison. The caller holds both readers, including
//! their source reservations, throughout this synchronous worker operation.
//! A fresh private analysis domain receives two Arc-backed capture aliases;
//! original reader authority is preserved in every returned source descriptor.
//! No bytes are copied, rehashed, reopened, normalized, or inferred from paths.

use fcb::analysis::comparison::{CaptureComparison, ComparisonError, ComparisonQuality,
    ComparisonRelation, CorrespondenceKind, MAX_DIFF_EDITS, MAX_DIFF_SOURCE_BYTES, MAX_DIFF_WORK};
pub use fcb::analysis::comparison::ComparisonLimits as ReaderComparisonLimits;
use super::{ReaderSession, ReaderSessionError, ArenaOwnerId, FileId, SourceRevision,
    CaptureRequest, QueryGeneration, ResourceBudget, ResourceAllocationId, ByteLength,
    Output, OutputError, HostResponse, DetectedEncoding,
    MANAGED_BYTES, EXIT_OK, EXIT_PARTIAL, check};

impl ReaderSession {
    /// Compare the two explicitly supplied retained sources. `analysis_owner`
    /// must be fresh and distinct from BOTH reader owners; it is not a reader
    /// handle or an authorization to read either source's live path. `generation`
    /// increases across every admitted attempt on the before reader, including
    /// cancellation. No query, outline, document or selection state is changed.
    ///
    /// The bounded response contains the entire partition of both sources,
    /// including unresolved interiors when refinement exceeds a budget. Ranges
    /// use original bytes and may split encoded text. They authorize navigation
    /// only while the host still retains the matching original reader identities.
    pub fn compare_retained(&mut self, after: &Self, analysis_owner: ArenaOwnerId,
        generation: u64, limits: ReaderComparisonLimits, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, ReaderSessionError> {
        if self.owner() == after.owner() || analysis_owner == self.owner()
            || analysis_owner == after.owner() { return Err(ComparisonError::OwnerMismatch.into()); }
        if generation == 0 || generation <= self.last_comparison_attempt {
            return Err(ComparisonError::Stale.into());
        }
        if limits.max_source_bytes > MAX_DIFF_SOURCE_BYTES
            || self.capture.bytes().len() > limits.max_source_bytes
            || after.capture.bytes().len() > limits.max_source_bytes
            || limits.max_edit_distance > MAX_DIFF_EDITS || limits.max_work > MAX_DIFF_WORK {
            return Err(ComparisonError::Limits.into());
        }
        self.last_comparison_attempt = generation;
        check(&mut canceled)?;
        let budget = ResourceBudget::new(analysis_owner, ByteLength::new(MANAGED_BYTES))
            .map_err(|_| ComparisonError::ResourceDenied)?;
        let request = |id| -> Result<CaptureRequest, ReaderSessionError> {
            Ok(CaptureRequest::new(FileId::new(analysis_owner, id)
                .map_err(|_| ReaderSessionError::IdentityExhausted)?,
                SourceRevision::new(analysis_owner, 1)
                    .map_err(|_| ReaderSessionError::IdentityExhausted)?)?)
        };
        // This is an explicitly granted transfer into the analysis domain, not
        // a relaxation of CaptureComparison's same-owner invariant. Borrowing
        // both sessions keeps the original source leases alive until return.
        let before_capture = self.capture.share_with_request(request(1)?)?;
        let after_capture = after.capture.share_with_request(request(2)?)?;
        let query = QueryGeneration::new(analysis_owner, generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let comparison = CaptureComparison::build(&before_capture, &after_capture, query,
            limits, &budget, ResourceAllocationId::new(1)
                .map_err(|_| ReaderSessionError::IdentityExhausted)?, &mut canceled)?;
        let stats = comparison.stats();
        let exact = comparison.quality() == ComparisonQuality::Exact;
        let mut out = self.output("compare")?;
        out.literal(",\"comparison_owner\":")?; out.integer(analysis_owner.get())?;
        out.literal(",\"comparison_generation\":")?; out.integer(generation)?;
        out.literal(",\"comparison_complete\":")?; out.boolean(exact)?;
        out.literal(",\"comparison_quality\":")?;
        out.quoted(match comparison.quality() {
            ComparisonQuality::Exact => "exact", ComparisonQuality::WorkLimit => "work-limit",
            ComparisonQuality::EditLimit => "edit-limit",
        })?;
        out.literal(",\"relation\":")?;
        out.quoted(match comparison.relation() {
            ComparisonRelation::Identical => "identical", ComparisonRelation::Different => "different",
            ComparisonRelation::Undetermined => "undetermined",
        })?;
        out.literal(",\"coordinate_domain\":\"original-bytes-may-split-text\",\"alignment_semantics\":\"deterministic-not-identity-proof\",\"source_reopened\":false")?;
        out.literal(",\"max_source_bytes\":")?; out.integer(limits.max_source_bytes as u64)?;
        out.literal(",\"max_edit_distance\":")?; out.integer(limits.max_edit_distance as u64)?;
        out.literal(",\"max_work\":")?; out.integer(limits.max_work)?;
        out.literal(",\"work_units\":")?; out.integer(stats.work_units)?;
        out.literal(",\"edit_distance\":")?;
        match stats.edit_distance { Some(value) => out.integer(value as u64)?, None => out.literal("null")? }
        out.literal(",\"before_source\":")?; source(&mut out, self)?;
        out.literal(",\"after_source\":")?; source(&mut out, after)?;
        out.literal(",\"total_spans\":")?; out.integer(comparison.spans().len() as u64)?;
        out.literal(",\"spans\":[")?;
        for (index, span) in comparison.spans().iter().enumerate() {
            check(&mut canceled)?;
            if index > 0 { out.literal(",")?; }
            out.literal("{\"span\":")?; out.integer(index as u64)?;
            out.literal(",\"kind\":")?;
            out.quoted(match span.kind() {
                CorrespondenceKind::Equal => "equal", CorrespondenceKind::Changed => "changed",
                CorrespondenceKind::Unresolved => "unresolved",
            })?;
            out.literal(",\"before_range\":")?; out.range(span.before())?;
            out.literal(",\"after_range\":")?; out.range(span.after())?;
            out.literal("}")?;
        }
        out.literal("]}\n")?;
        self.finish_output(out, if exact { EXIT_OK } else { EXIT_PARTIAL }, &mut canceled)
    }
}

fn source(out: &mut Output, reader: &ReaderSession) -> Result<(), OutputError> {
    out.literal("{\"owner\":")?; out.integer(reader.owner().get())?;
    out.literal(",\"file_id\":")?; out.integer(reader.capture.request().file().get())?;
    out.literal(",\"source_revision\":")?; out.integer(reader.capture.request().revision().get())?;
    out.literal(",\"path\":")?; out.path(reader.path())?;
    out.literal(",\"captured_bytes\":")?; out.integer(reader.length())?;
    out.literal(",\"capture_sha256\":")?; out.quoted(&reader.capture_sha256.to_hex())?;
    out.literal(",\"encoding\":")?;
    out.quoted(match reader.encoding {
        DetectedEncoding::Utf16Le => "utf16le", DetectedEncoding::Utf16Be => "utf16be",
        DetectedEncoding::Utf8 { .. } => "utf8", _ => "unsupported",
    })?;
    out.literal("}")
}
