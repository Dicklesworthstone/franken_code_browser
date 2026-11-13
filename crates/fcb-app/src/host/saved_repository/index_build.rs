#![forbid(unsafe_code)]

//! Explicit FCBD construction from the archive handle already retained by this
//! saved session. The archive pathname is only a label: it is never reopened.
//! This uses the production resident builder, inversion and paged encoder, not
//! an out-of-core sort. Quota refusals stay visible as uncovered members.
//!
//! A build never attaches its output or changes an accepted query. Its separate
//! monotonic operation and one retained terminal receipt let a host reconcile a
//! lost reply WITHOUT repeating a filesystem write. Receipt/output capacity is
//! admitted before construction, including the old receipt and accepted index.
//! After publication starts, cancellation cannot erase the observed effect.

use std::borrow::Cow;
use super::*;
use fcb::search::{IndexError, IndexLimits, QueryError};
use fcb::search::snapshot_index::{SnapshotIndex, SavedIndexStats, MAX_INDEX_GRAMS};
use fcb::search::snapshot_index::paged::{PagedIndexArtifact, MAX_PAGED_POSTINGS_BYTES};
use crate::{EXIT_CANCELED, EXIT_ERROR};
use crate::host::desk::persistence::{publish_new_bytes, CheckpointIoError, CheckpointSaveEffect};

pub const MAX_INDEX_BUILD_FILE_BYTES: usize = 256 * 1024;
pub const MAX_INDEX_BUILD_SOURCE_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_INDEX_BUILD_GRAMS: usize = MAX_INDEX_GRAMS;
const MAX_BUILD_RECEIPT_BYTES: usize = 256 * 1024;
const MAX_FILE_GRAMS: usize = 65_536;
const MAX_SCRATCH_BYTES: usize = 1024 * 1024;

/// Explicit ceilings, not promises of full coverage. Zero is allowed: sources
/// refused by any quota remain in the artifact and use direct verification.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedIndexBuildLimits {
    pub max_file_bytes: usize,
    pub max_source_bytes: u64,
    pub max_grams: usize,
}
impl Default for SavedIndexBuildLimits {
    fn default() -> Self { Self { max_file_bytes: MAX_INDEX_BUILD_FILE_BYTES,
        max_source_bytes: MAX_INDEX_BUILD_SOURCE_BYTES, max_grams: MAX_INDEX_BUILD_GRAMS } }
}
impl SavedIndexBuildLimits {
    fn engine(self) -> Result<IndexLimits, SavedRepositoryError> {
        if self.max_file_bytes > MAX_INDEX_BUILD_FILE_BYTES
            || self.max_source_bytes > MAX_INDEX_BUILD_SOURCE_BYTES
            || self.max_grams > MAX_INDEX_BUILD_GRAMS {
            return Err(SavedRepositoryError::InvalidLimits);
        }
        Ok(IndexLimits { max_source_bytes_per_file: self.max_file_bytes,
            max_source_bytes_total: self.max_source_bytes, max_total_grams: self.max_grams,
            max_grams_per_file: MAX_FILE_GRAMS, max_scratch_bytes: MAX_SCRATCH_BYTES })
    }
}

#[derive(Clone, Copy)]
enum BuildFailure { Index(SnapshotIndexError), Encoding(PagedPostingError), Write(CheckpointIoError) }
impl BuildFailure {
    fn is_canceled(self) -> bool {
        match self {
            Self::Index(e) | Self::Encoding(PagedPostingError::Index(e)) => matches!(e,
                SnapshotIndexError::Canceled | SnapshotIndexError::Archive(PagedSnapshotError::Canceled)
                | SnapshotIndexError::Build(IndexError::Canceled | IndexError::Query(QueryError::Canceled))),
            Self::Write(e) => e.is_canceled(), _ => false,
        }
    }
    fn code(self) -> Cow<'static, str> {
        match self {
            // These branches can only occur BEFORE any publication. Formatting
            // their bounded engine codes cannot obscure a filesystem effect.
            Self::Index(e) => Cow::Owned(e.to_string()), Self::Encoding(e) => Cow::Owned(e.to_string()),
            // The writer's possible failures require no allocation after create.
            Self::Write(e) => Cow::Borrowed(match e {
                CheckpointIoError::DestinationExists => "SAVED_INDEX_DESTINATION_EXISTS",
                CheckpointIoError::InvalidPath => "SAVED_INDEX_DESTINATION_PATH",
                CheckpointIoError::IoCallLimit => "SAVED_INDEX_WRITE_CALL_LIMIT",
                CheckpointIoError::Canceled => "SAVED_INDEX_WRITE_CANCELED",
                _ => "SAVED_INDEX_WRITE_IO",
            }),
        }
    }
}
impl From<SnapshotIndexError> for BuildFailure { fn from(e: SnapshotIndexError) -> Self { Self::Index(e) } }
impl From<PagedPostingError> for BuildFailure { fn from(e: PagedPostingError) -> Self { Self::Encoding(e) } }

/// The admitted encoder owns the complete immutable identity/destination/build
/// prefix. Retaining the typed outcome BEFORE finishing its tail means even an
/// unexpected encoding/transport failure leaves a recoverable terminal record.
pub(super) struct RetainedIndexBuild {
    operation: u64,
    receipt: Output,
    tail_start: usize,
    tail_complete: bool,
    effect: CheckpointSaveEffect,
    bytes_written: u64,
    complete_artifact: bool,
    partial: bool,
    failure: Option<BuildFailure>,
}
impl RetainedIndexBuild {
    fn exit_code(&self) -> u8 {
        match self.failure {
            Some(e) if e.is_canceled() => EXIT_CANCELED, Some(_) => EXIT_ERROR,
            None if self.partial => EXIT_PARTIAL, None => EXIT_OK,
        }
    }
    fn encode_tail(&mut self) -> Result<(), SavedRepositoryError> {
        if self.tail_complete { return Ok(()); }
        self.receipt.restore(self.tail_start)?;
        self.receipt.literal(",\"status\":")?;
        self.receipt.quoted(if self.failure.is_some() { "error" } else { "ok" })?;
        self.receipt.literal(",\"effect\":")?; self.receipt.quoted(self.effect.code())?;
        self.receipt.literal(",\"bytes_written\":")?; self.receipt.integer(self.bytes_written)?;
        self.receipt.literal(",\"attach_eligible\":")?;
        self.receipt.boolean(self.complete_artifact && matches!(self.effect,
            CheckpointSaveEffect::Written | CheckpointSaveEffect::FileSynced | CheckpointSaveEffect::DirectorySynced))?;
        self.receipt.literal(",\"error\":")?;
        match self.failure {
            Some(error) => {
                self.receipt.literal("{\"code\":")?; self.receipt.quoted(&error.code())?;
                self.receipt.literal(",\"next_action\":\"Inspect the retained effect and any created destination before an explicit new operation; never replay a lost build reply.\"}")?;
            }
            None => self.receipt.literal("null")?,
        }
        self.receipt.literal("}\n")?; self.tail_complete = true;
        Ok(())
    }
}

impl SavedRepositorySession {
    pub const fn last_index_build_attempt(&self) -> u64 { self.last_index_build_attempt }
    pub fn last_index_build_operation(&self) -> Option<u64> { self.index_build.as_ref().map(|r| r.operation) }

    /// Build a NEW source-derived index at an explicit destination. The returned
    /// pin comes from the trusted production encoder, never from an input index.
    /// Successful and effect-bearing failed/canceled writes have the SAME
    /// retained receipt API. Call on a worker; this is bounded resident work.
    ///
    /// Invalid/stale/pre-admission requests return an ordinary error and preserve
    /// the previous receipt. An admitted terminal outcome replaces that one
    /// receipt. The attempt high-water is independent of attach/query generations.
    pub fn build_index(&mut self, operation: u64, destination: &Path, limits: SavedIndexBuildLimits,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, SavedRepositoryError> {
        if operation == 0 || operation <= self.last_index_build_attempt {
            return Err(SavedRepositoryError::StaleIndexBuild);
        }
        self.last_index_build_attempt = operation;
        check(&mut canceled)?;
        let engine_limits = limits.engine()?;
        if destination.as_os_str().is_empty() || destination.as_os_str().len() > 16_384 {
            return Err(SavedRepositoryError::InvalidLimits);
        }
        if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
        let destination = input::absolute(destination)?;
        if destination.as_os_str().len() > 16_384 { return Err(SavedRepositoryError::InvalidLimits); }

        // All response IDs, lease charges and physical String capacities exist
        // before the writer can create anything. The old accepted data and old
        // terminal receipt remain charged throughout this candidate build.
        let [output_id, reply_id, index_id, member_id, capture_id, scratch_id, inverse_id, artifact_id] = self.allocations()?;
        let mut out = Output::new(self.owner(), MAX_BUILD_RECEIPT_BYTES, &self.budget, output_id)?;
        let (reply, reply_lease) = reply_storage(self.owner(), &self.budget, reply_id)?;
        encode_prefix(self, &mut out, operation, &destination, limits)?;
        let before = self.archive.load_stats();
        let mut stats = None;
        let built = (|| -> Result<PagedIndexArtifact, BuildFailure> {
            let forward = SnapshotIndex::build(&mut self.archive, engine_limits, &self.budget,
                [index_id, member_id, capture_id, scratch_id], &mut canceled)?;
            stats = Some(forward.stats());
            let inverse = forward.invert(&self.budget, inverse_id, &mut canceled)?;
            drop(forward);
            let artifact = inverse.encode_paged(&self.budget, artifact_id, &mut canceled)?;
            drop(inverse);
            Ok(artifact)
        })();
        let after = self.archive.load_stats();
        let coverage_complete = stats.is_some_and(|s| self.archive.directory().discovery_complete()
            && s.uncovered_files == 0 && s.unavailable_files == 0);
        encode_build(&mut out, stats, built.as_ref().ok(), coverage_complete,
            after.bytes_read.saturating_sub(before.bytes_read), after.read_calls.saturating_sub(before.read_calls))?;
        // A fixed outcome tail fits without growth. This explicit headroom gate
        // occurs before create, even if later fields are extended by a caller.
        if out.remaining_bytes() < 4096 { return Err(AppError::Admission.into()); }
        let tail_start = out.checkpoint();
        let mut retained = RetainedIndexBuild { operation, receipt: out, tail_start, tail_complete: false,
            effect: CheckpointSaveEffect::None, bytes_written: 0, complete_artifact: built.is_ok(),
            partial: !coverage_complete, failure: None };
        match built {
            Ok(artifact) => {
                retained.failure = publish_new_bytes(&destination, artifact.bytes(), &mut retained.effect,
                    &mut retained.bytes_written, &mut canceled).err().map(BuildFailure::Write);
            }
            Err(error) => retained.failure = Some(error),
        }
        // No cancellation, admission, allocation or pathname read may intervene
        // between the writer's terminal outcome and retaining its operation.
        self.index_build = Some(retained);
        self.build_response(reply, reply_lease)
    }

    /// Read the retained terminal receipt only; never reread source bytes,
    /// inspect/recreate the output file, attach an index or replay construction.
    /// It is byte-identical to a successfully delivered original build reply.
    pub fn index_build_receipt(&mut self, operation: u64, mut canceled: impl FnMut() -> bool)
        -> Result<HostResponse, SavedRepositoryError> {
        check(&mut canceled)?;
        let retained = self.index_build.as_ref().ok_or(SavedRepositoryError::MissingIndexBuild)?;
        if operation != retained.operation { return Err(SavedRepositoryError::StaleIndexBuild); }
        let [id] = self.allocations()?;
        let (reply, lease) = reply_storage(self.owner(), &self.budget, id)?;
        check(&mut canceled)?;
        self.build_response(reply, lease)
    }

    fn build_response(&mut self, mut text: String, lease: ResourceLease) -> Result<HostResponse, SavedRepositoryError> {
        let retained = self.index_build.as_mut().ok_or(SavedRepositoryError::MissingIndexBuild)?;
        retained.encode_tail()?;
        let bytes = retained.receipt.as_bytes();
        if bytes.len() > text.capacity() { return Err(AppError::Admission.into()); }
        text.push_str(std::str::from_utf8(bytes).map_err(|_| AppError::InvalidRange)?);
        Ok(HostResponse { text, exit_code: retained.exit_code(), _lease: lease })
    }
}

fn reply_storage(owner: ArenaOwnerId, budget: &ResourceBudget, id: ResourceAllocationId)
    -> Result<(String, ResourceLease), SavedRepositoryError> {
    let lease = budget.try_reserve_managed(owner, id,
        ByteLength::new((MAX_BUILD_RECEIPT_BYTES * 3 + 4096) as u64)).map_err(|_| AppError::Admission)?;
    let mut text = String::new();
    text.try_reserve_exact(MAX_BUILD_RECEIPT_BYTES).map_err(|_| AppError::Admission)?;
    if text.capacity() > MAX_BUILD_RECEIPT_BYTES { return Err(AppError::Admission.into()); }
    Ok((text, lease))
}

fn encode_prefix(session: &SavedRepositorySession, out: &mut Output, operation: u64,
    destination: &Path, limits: SavedIndexBuildLimits) -> Result<(), OutputError> {
    out.literal("{\"schema\":\"fcb.saved-repository/1\",\"command\":\"index-build\",\"owner\":")?;
    out.integer(session.owner().get())?;
    out.literal(",\"archive_digest\":")?; out.quoted(&session.archive_digest().to_hex())?;
    out.literal(",\"members_total\":")?; out.integer(session.archive.directory().len() as u64)?;
    out.literal(",\"captured_members\":")?; out.integer(session.archive.directory().captured_files() as u64)?;
    out.literal(",\"discovery_complete\":")?; out.boolean(session.archive.directory().discovery_complete())?;
    out.literal(",\"live_root_authority\":false,\"native_presented\":false,\"operation\":")?; out.integer(operation)?;
    out.literal(",\"last_index_build_attempt\":")?; out.integer(operation)?;
    out.literal(",\"destination\":")?; out.path(destination)?;
    for (key, value) in [("max_file_bytes", limits.max_file_bytes as u64), ("max_source_bytes", limits.max_source_bytes),
        ("max_grams", limits.max_grams as u64), ("max_grams_per_file", MAX_FILE_GRAMS as u64),
        ("max_scratch_bytes", MAX_SCRATCH_BYTES as u64), ("max_index_bytes", MAX_PAGED_POSTINGS_BYTES as u64)] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; out.integer(value)?;
    }
    out.literal(",\"index_kind\":\"demand-paged-postings-v1\",\"index_pin_scope\":\"page-manifest-envelope-sha256\",\"index_attached\":false,\"accepted_index_unchanged\":true,\"accepted_query_unchanged\":true,\"source_derived_sensitive\":true,\"encrypted\":false,\"overwritten\":false,\"power_loss_qualified\":false")
}

fn encode_build(out: &mut Output, stats: Option<SavedIndexStats>, artifact: Option<&PagedIndexArtifact>,
    coverage_complete: bool, member_bytes_read: u64, member_read_calls: u64) -> Result<(), OutputError> {
    out.literal(",\"index_bytes\":")?; out.integer(artifact.map_or(0, |a| a.bytes().len() as u64))?;
    out.literal(",\"index_manifest_bytes\":")?; out.integer(artifact.map_or(0, |a| a.manifest_bytes() as u64))?;
    out.literal(",\"index_pin\":")?;
    match artifact { Some(a) => out.quoted(&a.digest().to_hex())?, None => out.literal("null")? }
    for (key, value) in [("index_members", stats.map(|s| s.members as u64)),
        ("index_indexed_files", stats.map(|s| s.indexed_files as u64)),
        ("index_uncovered_files", stats.map(|s| s.uncovered_files as u64)),
        ("index_unavailable_files", stats.map(|s| s.unavailable_files as u64)),
        ("index_unique_grams", stats.map(|s| s.unique_grams as u64)),
        ("index_build_source_bytes", stats.map(|s| s.build_source_bytes))] {
        out.literal(",")?; out.quoted(key)?; out.literal(":")?; optional(out, value)?;
    }
    out.literal(",\"coverage_complete\":")?; out.boolean(coverage_complete)?;
    out.literal(",\"member_bytes_read\":")?; out.integer(member_bytes_read)?;
    out.literal(",\"member_read_calls\":")?; out.integer(member_read_calls)
}
