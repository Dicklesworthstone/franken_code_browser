#![forbid(unsafe_code)]

//! Explicit file-backed export. Discovery retains bounded metadata; each source
//! member is captured, appended and released before the next member is opened.
//! The archive is never materialized as one application allocation.

use std::{fs::{self, OpenOptions}, io, path::Path};
use fcb::{ByteLength, ByteOffset, ByteRange, SourceRevision};
use fcb::source::{CancelFlag, NormalizedPath, SourceError};
use fcb::search::{CaptureRequest, ExtentConsistency, ExtentError, ExtentReadState,
    ExtentStepBudget, FileRangeReader, RawPath, ResourceBudget, RootId, SearchManifestId};
use fcb::search::workspace::{RootGrant, RuleLimits, WorkspaceCatalog, WorkspaceStage};
use fcb::search::snapshot::{SnapshotData, SnapshotEntry, SnapshotStreamError,
    SnapshotStreamWriter, SnapshotWriteSummary};
use fcb_core::ResourceLease;
use crate::{AppError, allocation, file_id, input, owner, revision, workspace, EXIT_OK, EXIT_PARTIAL};
use crate::output::{Output, OutputError};
use super::{Effect, Failure, Settings, MAX_CALLS, begin, sync_written};

const CAPTURE_EXTENT_BYTES: usize = 1024 * 1024;

impl From<SnapshotStreamError> for Failure {
    fn from(error: SnapshotStreamError) -> Self {
        Self { code: error.code().to_owned(), canceled: error.is_canceled() }
    }
}

pub(super) fn execute(settings: &Settings, out: &mut Output, budget: &ResourceBudget,
    effect: &mut Effect, canceled: &mut impl FnMut() -> bool) -> Result<u8, Failure> {
    if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
    let _paths = budget.try_reserve_managed(owner(), allocation(102), ByteLength::new(256 * 1024))
        .map_err(|_| Failure::new("SNAPSHOT_RESOURCE_DENIED"))?;
    let requested = input::absolute(settings.source.as_deref().ok_or_else(|| Failure::new("CLI_MISSING_SOURCE"))?)?;
    let metadata = fs::symlink_metadata(&requested).map_err(|_| Failure::new("SNAPSHOT_ROOT_UNAVAILABLE"))?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() { return Err(Failure::new("SNAPSHOT_ROOT_NOT_DIRECTORY")); }
    let root = fs::canonicalize(requested).map_err(|_| Failure::new("SNAPSHOT_ROOT_UNAVAILABLE"))?;
    let destination = input::absolute(settings.output.as_deref().ok_or_else(|| Failure::new("SNAPSHOT_OUTPUT_REQUIRED"))?)?;
    match fs::symlink_metadata(&destination) {
        Ok(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_EXISTS")),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {},
        Err(_) => return Err(Failure::new("SNAPSHOT_DESTINATION_UNAVAILABLE")),
    }
    let grant = RootGrant::new(RootId::new(owner(), 1).map_err(|_| Failure::new("SNAPSHOT_OWNER"))?, RawPath::from_path(&root));
    let id = SearchManifestId::new(owner(), 1).map_err(AppError::from)?;
    // Discovery is metadata-only. Its capture limits stay zero and never widen
    // the separate live-workspace search/capture admission profile.
    let mut catalog = if settings.respect_ignores {
        WorkspaceCatalog::open_rule_aware(grant, id, file_id(), settings.limits, RuleLimits::default(),
            budget, [allocation(101), allocation(119)])
    } else {
        WorkspaceCatalog::open(grant, id, file_id(), settings.limits, settings.include_excluded, budget, allocation(101))
    }.map_err(AppError::from)?;
    let cancel = CancelFlag::new();
    while catalog.stage() == WorkspaceStage::Discovering {
        if canceled() { return Err(Failure::canceled()); }
        catalog.step(&cancel).map_err(AppError::from)?;
    }
    catalog.validate_active().map_err(AppError::from)?;
    if canceled() { return Err(Failure::canceled()); }
    let mut options = OpenOptions::new(); options.read(true).write(true).create_new(true);
    #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
    let mut file = options.open(&destination).map_err(|error| Failure::new(if error.kind() == io::ErrorKind::AlreadyExists {
        "SNAPSHOT_DESTINATION_EXISTS" } else { "SNAPSHOT_CREATE_FAILED" }))?;
    *effect = Effect::Incomplete;
    let mut writer = SnapshotStreamWriter::new(&mut file, owner(), catalog.discovery_complete(),
        catalog.policy_name(), catalog.entries().len(), settings.save_limits, budget, allocation(103), &mut *canceled)?;
    let mut io = IoCounts::default();
    for (ordinal, entry) in catalog.entries().iter().enumerate() {
        if canceled() { return Err(Failure::canceled()); }
        catalog.validate_active().map_err(AppError::from)?;
        let captured = if entry.observed_bytes() > settings.save_limits.max_file_bytes as u64 {
            Err("WORKSPACE_FILE_BYTE_LIMIT")
        } else if entry.observed_bytes() > (settings.save_limits.max_source_bytes as u64).saturating_sub(io.bytes) {
            // Failed reads spend allowance too; repeated changing/unreadable
            // members cannot make the requested source-I/O budget unbounded.
            Err("WORKSPACE_TOTAL_SOURCE_LIMIT")
        } else {
            let id = catalog.file_id(ordinal).ok_or_else(|| Failure::new("SNAPSHOT_OWNER"))?;
            let request = CaptureRequest::new(id, revision()).map_err(|error| Failure::new(error.code()))?;
            match capture(&root, request, entry.path(), entry.observed_bytes(), &mut io, budget, canceled) {
                Ok(source) => Ok(source),
                Err(SourceError::Canceled) => return Err(Failure::canceled()),
                Err(error) => Err(error.code()),
            }
        };
        catalog.validate_active().map_err(AppError::from)?;
        let data = match &captured {
            Ok(source) => SnapshotData::Captured(&source.bytes),
            Err(reason) => SnapshotData::Unavailable(reason),
        };
        writer.append(SnapshotEntry { path: entry.path().as_bytes(),
            observed_bytes: entry.observed_bytes(), data }, &mut *canceled)?;
        // `captured` and its lease drop here, before the next source is opened.
    }
    catalog.validate_active().map_err(AppError::from)?;
    let (_, saved) = writer.finish(&mut *canceled)?;
    // A complete checksum is an observable write effect even if the user
    // cancels the following sync. Transition before another cancellation poll.
    sync_written(&file, &destination, effect, canceled)?;
    let complete = saved.discovery_complete && saved.captured_files == saved.files;
    if settings.json {
        begin(out, "snapshot-save")?; out.literal(",\"effect\":")?; out.quoted(effect.name())?;
        out.literal(",\"destination\":")?; out.path(&destination)?;
        summary(out, &saved, catalog.policy_name())?;
        workspace::rule_output::fields(out, &catalog)?;
        out.literal(",\"payload_bytes_read\":")?; out.integer(io.bytes)?;
        out.literal(",\"read_calls\":")?; out.integer(io.calls)?;
        out.literal(",\"largest_captured_member_bytes\":")?; out.integer(io.largest_captured)?;
        out.literal(",\"payload_residency\":\"one-captured-member\",\"archive_bytes\":")?; out.integer(saved.archive_bytes)?;
        out.literal(",\"archive_hash_bytes_read\":")?; out.integer(saved.stats.bytes_read)?;
        out.literal(",\"source_contains_plaintext\":true,\"power_loss_qualified\":false}\n")?;
    } else {
        out.literal("Saved source-containing snapshot ")?; out.path(&destination)?;
        out.literal(if complete { "\nComplete observed membership and captures.\n" } else { "\nPARTIAL saved scope: discovery or captures are incomplete.\n" })?;
        workspace::rule_output::human(out, &catalog)?;
        out.literal("One captured member retained at a time; no overwrite; sync requested; no power-loss durability qualification.\n")?;
    }
    Ok(if complete { EXIT_OK } else { EXIT_PARTIAL })
}

#[derive(Default)]
struct IoCounts { bytes: u64, calls: u64, largest_captured: u64 }
struct CapturedSource { bytes: Vec<u8>, _lease: ResourceLease }

fn capture(root: &Path, request: CaptureRequest, path: &NormalizedPath, observed: u64,
    io: &mut IoCounts, budget: &ResourceBudget, canceled: &mut impl FnMut() -> bool)
    -> Result<CapturedSource, SourceError> {
    if canceled() { return Err(SourceError::Canceled); }
    let charge = observed.checked_add(128 * 1024).ok_or(SourceError::PayloadTooLarge)?;
    let lease = budget.try_reserve_managed(owner(), allocation(104), ByteLength::new(charge))
        .map_err(|_| SourceError::CaptureUnavailable)?;
    let native = workspace::checked_source_path(root, path)?;
    let (file, before) = input::open_regular(&native).map_err(|_| SourceError::CaptureUnavailable)?;
    if before.len() != observed { return Err(SourceError::MetadataMismatch); }
    let modified = before.modified().map_err(|_| SourceError::CaptureUnavailable)?;
    let observer = file.try_clone().map_err(|_| SourceError::CaptureUnavailable)?;
    let length = usize::try_from(observed).map_err(|_| SourceError::PayloadTooLarge)?;
    let mut bytes = Vec::new();
    bytes.try_reserve_exact(length).map_err(|_| SourceError::CaptureUnavailable)?;
    if bytes.capacity() > length { return Err(SourceError::CaptureUnavailable); }
    let mut reader = FileRangeReader::new(request.file(), file).map_err(source_error)?;
    while bytes.len() < length {
        if canceled() { return Err(SourceError::Canceled); }
        let start = bytes.len(); let end = length.min(start + CAPTURE_EXTENT_BYTES);
        let range = ByteRange::new(ByteOffset::new(start as u64), ByteOffset::new(end as u64))
            .map_err(|_| SourceError::InvalidRange)?;
        // A range reader refuses reused observation revisions, even when the
        // range differs. Every bounded extent needs a fresh observation ID.
        let next = request.revision().get().checked_add((start / CAPTURE_EXTENT_BYTES) as u64)
            .ok_or(SourceError::IdentityExhausted)?;
        let extent_revision = SourceRevision::new(request.file().owner(), next)
            .map_err(|_| SourceError::IdentityExhausted)?;
        let selected = CaptureRequest::new(request.file(), extent_revision)?.with_range(range)?;
        let mut read = reader.begin(selected, budget, allocation(109)).map_err(source_error)?;
        while read.state() == ExtentReadState::Pending {
            if io.calls >= MAX_CALLS { return Err(SourceError::CaptureUnavailable); }
            let before = read.stats();
            let result = read.step(ExtentStepBudget { max_bytes: 64 * 1024,
                max_calls: (MAX_CALLS - io.calls).min(32) as usize }, &mut *canceled);
            let after = read.stats();
            io.bytes += after.bytes_read - before.bytes_read;
            io.calls += after.read_calls - before.read_calls;
            result.map_err(source_error)?;
        }
        let extent = read.finish(&mut *canceled).map_err(source_error)?;
        if !extent.request_filled() || extent.range() != range || extent.observed_length().get() != observed
            || extent.final_length().map(|n| n.get()) != Some(observed)
            || extent.consistency() != ExtentConsistency::UnchangedMetadata {
            return Err(SourceError::MetadataMismatch);
        }
        bytes.extend_from_slice(extent.bytes());
    }
    let after = observer.metadata().map_err(|_| SourceError::CaptureUnavailable)?;
    if after.len() != observed || after.modified().ok() != Some(modified) { return Err(SourceError::MetadataMismatch); }
    if canceled() { return Err(SourceError::Canceled); }
    io.largest_captured = io.largest_captured.max(bytes.len() as u64);
    Ok(CapturedSource { bytes, _lease: lease })
}

fn source_error(error: ExtentError) -> SourceError {
    match error {
        ExtentError::Canceled => SourceError::Canceled,
        ExtentError::Source(error) => error,
        _ => SourceError::CaptureUnavailable,
    }
}

fn summary(out: &mut Output, saved: &SnapshotWriteSummary, policy: &str) -> Result<(), OutputError> {
    out.literal(",\"source_scope\":\"saved-observations-only\",\"live_roots_accessed\":true,\"snapshot_digest\":")?;
    out.quoted(&saved.digest.to_hex())?;
    out.literal(",\"policy\":")?; out.quoted(policy)?;
    out.literal(",\"discovery_complete\":")?; out.boolean(saved.discovery_complete)?;
    out.literal(",\"known_files\":")?; out.integer(saved.files as u64)?;
    out.literal(",\"captured_files\":")?; out.integer(saved.captured_files as u64)?;
    out.literal(",\"unavailable_files_count\":")?; out.integer((saved.files - saved.captured_files) as u64)?;
    out.literal(",\"captured_bytes\":")?; out.integer(saved.source_bytes)
}
