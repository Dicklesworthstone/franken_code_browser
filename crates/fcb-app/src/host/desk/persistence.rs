#![forbid(unsafe_code)]

//! Explicit source-bearing desk save/restore. No autosave, implicit destination,
//! source-path reopening, overwrite, deletion, external process or networking.
//! An exclusive new file preserves every earlier checkpoint. Interrupted files
//! remain visible and fail checksum validation; they are NEVER called a save.
//! File and parent sync calls are reported precisely, not a universal hardware
//! crash-durability guarantee. The supplied destination is host authority; labels
//! inside a checkpoint confer no filesystem capability.

use std::{fs::{File, OpenOptions}, io::{self, Read, Write}, path::{Path, PathBuf}};
use super::{DeskSession, DeskSessionError, DeskChange, DeskError, HostResponse,
    Output, ByteLength, ResourceBudget, ResourceAllocationId, ResourceLease};
use fcb::ui::reading_panes::desk::checkpoint::{CheckpointError, MAX_CHECKPOINT_BYTES};
use fcb::store::envelope::Sha256Digest;
use crate::{input, AppError, EXIT_OK, EXIT_ERROR, EXIT_CANCELED};

const IO_CHUNK: usize = 64 * 1024;
const MAX_IO_CALLS: usize = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointIoError {
    Session(DeskSessionError), Checkpoint(CheckpointError), DestinationExists,
    ExportLimit, InvalidPath, Io, Changed, IoCallLimit, Canceled,
}
impl CheckpointIoError {
    pub fn code(self) -> String {
        match self {
            Self::Session(e) => e.to_string(), Self::Checkpoint(e) => e.to_string(),
            Self::DestinationExists => "DESK_SAVE_DESTINATION_EXISTS".into(),
            Self::ExportLimit => "DESK_SAVE_SOURCE_EXPORT_LIMIT".into(),
            Self::InvalidPath => "DESK_CHECKPOINT_PATH".into(), Self::Io => "DESK_CHECKPOINT_IO".into(),
            Self::Changed => "DESK_CHECKPOINT_CHANGED".into(), Self::IoCallLimit => "DESK_CHECKPOINT_IO_CALL_LIMIT".into(),
            Self::Canceled => "DESK_CHECKPOINT_CANCELED".into(),
        }
    }
    pub fn is_canceled(self) -> bool {
        match self { Self::Canceled => true, Self::Session(e) => e.is_canceled(),
            Self::Checkpoint(e) => e.is_canceled(), _ => false }
    }
}
impl std::fmt::Display for CheckpointIoError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(&self.code()) }
}
impl std::error::Error for CheckpointIoError {}
impl From<DeskSessionError> for CheckpointIoError { fn from(e: DeskSessionError) -> Self { Self::Session(e) } }
impl From<CheckpointError> for CheckpointIoError { fn from(e: CheckpointError) -> Self { Self::Checkpoint(e) } }
impl From<AppError> for CheckpointIoError { fn from(e: AppError) -> Self { Self::Session(e.into()) } }

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointSaveEffect { None, Created, Written, FileSynced, DirectorySynced }
impl CheckpointSaveEffect {
    pub const fn code(self) -> &'static str {
        match self {
            Self::None => "none", Self::Created => "destination-created-incomplete",
            Self::Written => "complete-file-sync-unconfirmed", Self::FileSynced => "complete-file-sync-requested",
            Self::DirectorySynced => "complete-file-and-parent-sync-requested",
        }
    }
}

/// A write-aware result exists even when cancellation/I/O failure followed file
/// creation. Deliver it without another cancellation gate; delivery failure is
/// NOT rollback. No method deletes or overwrites an incomplete destination.
#[derive(Clone, Copy, Debug)]
pub struct CheckpointSave {
    effect: CheckpointSaveEffect,
    bytes_written: u64,
    document_bytes: u64,
    source_bytes: u64,
    digest: Option<Sha256Digest>,
    error: Option<CheckpointIoError>,
}
impl CheckpointSave {
    pub const fn effect(&self) -> CheckpointSaveEffect { self.effect }
    pub const fn bytes_written(&self) -> u64 { self.bytes_written }
    pub const fn document_bytes(&self) -> u64 { self.document_bytes }
    pub const fn source_bytes(&self) -> u64 { self.source_bytes }
    pub const fn digest(&self) -> Option<Sha256Digest> { self.digest }
    pub const fn error(&self) -> Option<CheckpointIoError> { self.error }
    pub fn exit_code(&self) -> u8 {
        match self.error { None => EXIT_OK, Some(e) if e.is_canceled() => EXIT_CANCELED, _ => EXIT_ERROR }
    }
}

impl DeskSession {
    /// Export ALL retained desk source bytes and personal state. The explicit
    /// byte ceiling is the host/user's source-disclosure consent, not a request
    /// to truncate. Uses a NEW destination only, mode 0600 on Unix. Saving does
    /// not change the model revision, history, selection or accepted query.
    pub fn save_checkpoint(&mut self, expected: u64, destination: &Path, max_source_bytes: u64,
        mut canceled: impl FnMut() -> bool) -> CheckpointSave {
        let mut outcome = CheckpointSave { effect: CheckpointSaveEffect::None, bytes_written: 0,
            document_bytes: 0, source_bytes: self.desk.retained_source_bytes(), digest: None, error: None };
        let result = (|| -> Result<(), CheckpointIoError> {
            if expected != self.desk.revision() { return Err(DeskSessionError::Desk(DeskError::StaleRevision).into()); }
            stop(&mut canceled)?;
            let destination = checked_path(destination)?;
            if outcome.source_bytes > max_source_bytes { return Err(CheckpointIoError::ExportLimit); }
            let allocation = self.next_id()?;
            let checkpoint = self.desk.checkpoint(expected, allocation, &mut canceled)?;
            outcome.document_bytes = checkpoint.bytes().len() as u64;
            outcome.digest = Some(checkpoint.digest());
            let parent = File::open(destination.parent().ok_or(CheckpointIoError::InvalidPath)?)
                .map_err(|_| CheckpointIoError::Io)?;
            if !parent.metadata().map_err(|_| CheckpointIoError::Io)?.is_dir() { return Err(CheckpointIoError::InvalidPath); }
            stop(&mut canceled)?;
            let mut options = OpenOptions::new(); options.write(true).create_new(true);
            #[cfg(unix)] { use std::os::unix::fs::OpenOptionsExt; options.mode(0o600); }
            let mut file = options.open(&destination).map_err(|e| if e.kind() == io::ErrorKind::AlreadyExists {
                CheckpointIoError::DestinationExists
            } else { CheckpointIoError::Io })?;
            outcome.effect = CheckpointSaveEffect::Created;
            write_payload(&mut file, checkpoint.bytes(), &mut outcome, &mut canceled)?;
            stop(&mut canceled)?;
            file.sync_all().map_err(|_| CheckpointIoError::Io)?;
            outcome.effect = CheckpointSaveEffect::FileSynced;
            stop(&mut canceled)?;
            parent.sync_all().map_err(|_| CheckpointIoError::Io)?;
            outcome.effect = CheckpointSaveEffect::DirectorySynced;
            Ok(())
        })();
        outcome.error = result.err();
        outcome
    }

    /// Restore one explicitly selected regular checkpoint file. Working-tree
    /// paths within its source labels are never opened. Failed file reads,
    /// corrupt envelopes and canceled imports leave old panes AND query intact.
    pub fn restore_checkpoint_file(&mut self, expected: u64, attempt: u64, path: &Path,
        mut canceled: impl FnMut() -> bool) -> Result<DeskChange, CheckpointIoError> {
        self.validate_mutation(expected, attempt)?;
        stop(&mut canceled)?;
        let allocation = self.next_id()?;
        let loaded = load_checkpoint(path, self.desk.owner(), &self.budget, allocation, &mut canceled)?;
        self.restore_checkpoint_bytes(expected, attempt, &loaded.bytes, &mut canceled)
    }

    /// Host-supplied checkpoint bytes; no I/O. Only a successfully accepted
    /// import retires the old search. Query-attempt high-water is preserved so
    /// delayed results from before restoration cannot become current again.
    pub fn restore_checkpoint_bytes(&mut self, expected: u64, attempt: u64, bytes: &[u8],
        canceled: impl FnMut() -> bool) -> Result<DeskChange, CheckpointIoError> {
        self.validate_mutation(expected, attempt)?;
        let allocation = self.next_id()?;
        let restored = self.desk.restore_checkpoint(expected, attempt, bytes, allocation, canceled)?;
        self.next_source = self.next_source.max(restored.next_source_identity);
        self.query = None;
        Ok(restored.change)
    }

    /// Encode the already-final write receipt. This deliberately has no cancel
    /// callback. A native host receives the typed outcome even if encoding or
    /// its later transport fails; filesystem effects cannot be rolled back.
    pub fn checkpoint_save_response(&mut self, saved: &CheckpointSave) -> Result<HostResponse, DeskSessionError> {
        let id = self.next_id()?;
        let mut out = Output::new(self.desk.owner(), 16 * 1024, &self.budget, id)?;
        out.literal("{\"schema\":\"fcb.reading-desk/1\",\"command\":\"save\",\"status\":")?;
        out.quoted(if saved.error.is_none() { "ok" } else { "error" })?;
        out.literal(",\"owner\":")?; out.integer(self.desk.owner().get())?;
        out.literal(",\"model_revision\":")?; out.integer(self.desk.revision())?;
        out.literal(",\"last_attempt\":")?; out.integer(self.desk.last_attempt())?;
        out.literal(",\"effect\":")?; out.quoted(saved.effect.code())?;
        out.literal(",\"bytes_written\":")?; out.integer(saved.bytes_written)?;
        out.literal(",\"checkpoint_bytes\":")?; out.integer(saved.document_bytes)?;
        out.literal(",\"source_bytes\":")?; out.integer(saved.source_bytes)?;
        out.literal(",\"checkpoint_digest\":")?;
        match saved.digest { Some(d) => out.quoted(&d.to_hex())?, None => out.literal("null")? }
        out.literal(",\"contains_source_payloads\":true,\"contains_personal_labels\":true,\"encrypted\":false,\"overwritten\":false,\"native_presented\":false,\"error\":")?;
        match saved.error {
            Some(e) => { out.literal("{\"code\":")?; out.quoted(&e.code())?;
                out.literal(",\"next_action\":\"Inspect any created destination before retrying; use a new destination and keep earlier checkpoints.\"}")?; }
            None => out.literal("null")?,
        }
        out.literal("}\n")?;
        self.finish(out, saved.exit_code(), &mut || false)
    }
}

fn stop(canceled: &mut impl FnMut() -> bool) -> Result<(), CheckpointIoError> {
    if canceled() { Err(CheckpointIoError::Canceled) } else { Ok(()) }
}
fn checked_path(path: &Path) -> Result<PathBuf, CheckpointIoError> {
    if path.as_os_str().is_empty() || path.as_os_str().len() > 16_384 { return Err(CheckpointIoError::InvalidPath); }
    if !input::NATIVE_FILE_SUPPORTED { return Err(AppError::UnsupportedPlatform.into()); }
    Ok(input::absolute(path)?)
}
fn write_payload(writer: &mut impl Write, bytes: &[u8], outcome: &mut CheckpointSave,
    canceled: &mut impl FnMut() -> bool) -> Result<(), CheckpointIoError> {
    let mut at = 0usize; let mut calls = 0usize;
    while at < bytes.len() {
        stop(canceled)?;
        if calls == MAX_IO_CALLS { return Err(CheckpointIoError::IoCallLimit); }
        calls += 1;
        let end = (at + IO_CHUNK).min(bytes.len());
        match writer.write(&bytes[at..end]) {
            Ok(n) if n > 0 && n <= end - at => { at += n; outcome.bytes_written = at as u64; }
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            _ => return Err(CheckpointIoError::Io),
        }
    }
    outcome.effect = CheckpointSaveEffect::Written;
    Ok(())
}
struct LoadedCheckpoint { bytes: Vec<u8>, _lease: ResourceLease }
fn load_checkpoint(path: &Path, owner: fcb::ArenaOwnerId, budget: &ResourceBudget,
    allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool) -> Result<LoadedCheckpoint, CheckpointIoError> {
    let path = checked_path(path)?;
    let (mut file, before) = input::open_regular(&path)?;
    if before.len() > MAX_CHECKPOINT_BYTES as u64 { return Err(CheckpointError::Limit.into()); }
    let modified = before.modified().map_err(|_| CheckpointIoError::Io)?;
    let length = usize::try_from(before.len()).map_err(|_| CheckpointError::Limit)?;
    let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new((length + 256 * 1024) as u64))
        .map_err(|_| AppError::Admission)?;
    let mut bytes = Vec::new(); bytes.try_reserve_exact(length).map_err(|_| AppError::Admission)?;
    if bytes.capacity() > length { return Err(AppError::Admission.into()); }
    bytes.resize(length, 0);
    let mut calls = 0; let mut at = 0;
    while at < length {
        stop(canceled)?;
        if calls == MAX_IO_CALLS { return Err(CheckpointIoError::IoCallLimit); }
        calls += 1;
        let end = (at + IO_CHUNK).min(length);
        match file.read(&mut bytes[at..end]) {
            Ok(0) => return Err(CheckpointIoError::Changed), Ok(n) => at += n,
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(CheckpointIoError::Io),
        }
    }
    // Detect growth without allocating beyond the admitted observation.
    let mut tail = [0u8; 1];
    loop {
        stop(canceled)?;
        if calls == MAX_IO_CALLS { return Err(CheckpointIoError::IoCallLimit); }
        calls += 1;
        match file.read(&mut tail) {
            Ok(0) => break, Ok(_) => return Err(CheckpointIoError::Changed),
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            Err(_) => return Err(CheckpointIoError::Io),
        }
    }
    let after = file.metadata().map_err(|_| CheckpointIoError::Io)?;
    if after.len() != before.len() || after.modified().ok() != Some(modified) { return Err(CheckpointIoError::Changed); }
    stop(canceled)?;
    Ok(LoadedCheckpoint { bytes, _lease: lease })
}

#[cfg(test)]
mod tests {
    use super::*;
    fn outcome() -> CheckpointSave { CheckpointSave { effect: CheckpointSaveEffect::Created,
        bytes_written: 0, document_bytes: 10, source_bytes: 10, digest: None, error: None } }
    #[test]
    fn short_write_then_error_reports_only_actual_written_prefix() {
        struct Broken { count: usize }
        impl Write for Broken {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.count += 1; if self.count == 1 { Ok(bytes.len().min(3)) } else { Err(io::ErrorKind::BrokenPipe.into()) }
            }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let mut state = outcome();
        assert_eq!(write_payload(&mut Broken { count: 0 }, b"0123456789", &mut state, &mut || false), Err(CheckpointIoError::Io));
        assert_eq!(state.bytes_written(), 3); assert_eq!(state.effect(), CheckpointSaveEffect::Created);
    }
    #[test]
    fn interrupted_write_retry_is_bounded_and_never_a_successful_save() {
        struct Interrupted;
        impl Write for Interrupted {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::Interrupted.into()) }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        let mut state = outcome();
        assert_eq!(write_payload(&mut Interrupted, b"data", &mut state, &mut || false), Err(CheckpointIoError::IoCallLimit));
        assert_eq!(state.bytes_written(), 0); assert_eq!(state.effect(), CheckpointSaveEffect::Created);
    }
    #[test]
    fn cancellation_preserves_the_exact_partial_write_effect() {
        let bytes = vec![b'x'; IO_CHUNK + 1]; let mut writer = Vec::new(); let mut state = outcome(); let mut calls = 0;
        assert_eq!(write_payload(&mut writer, &bytes, &mut state, &mut || { calls += 1; calls == 2 }), Err(CheckpointIoError::Canceled));
        assert_eq!(state.bytes_written(), IO_CHUNK as u64); assert_eq!(writer.len(), IO_CHUNK);
        assert_eq!(state.effect(), CheckpointSaveEffect::Created);
    }
}
