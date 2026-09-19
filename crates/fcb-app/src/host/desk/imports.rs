#![forbid(unsafe_code)]

//! Explicit cross-owner source transfer into a reading desk. Imported bytes are
//! copied once into the receiving owner; later occurrences share that capture.
//! The fixed metadata table retains no source payloads. Reusing a key requires
//! exact byte AND label equality, not a hash or pathname-based live reload.
//! Source owners must be fresh for independent providers, as for other host APIs.

use super::*;
use fcb::ui::reading_panes::{MAX_READING_PATH_BYTES, desk::MAX_DESK_SOURCES};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ImportedSourceId { pub file: FileId, pub revision: SourceRevision }
#[derive(Clone, Copy)]
pub(super) struct ImportLink { origin: ImportedSourceId, file: FileId, revision: SourceRevision }

/// Identity translation is explicit: source-provider IDs are never installed
/// as receiving-desk IDs. This receipt describes model acceptance, not pixels or
/// transport delivery. Checkpoints preserve bytes, not a renewed provider grant.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DeskImport {
    pub change: DeskChange,
    pub origin: ImportedSourceId,
    pub file: FileId,
    pub revision: SourceRevision,
    pub reused_capture: bool,
}

impl DeskSession {
    /// Transfer one authorized exact capture into the persistent multi-reader
    /// workflow. `label` is display text, never filesystem authority. The caller
    /// must keep its supplied bytes alive and revalidate its grant in `canceled`
    /// until acceptance. Copy/comparison and publication are worker operations.
    /// Repeated hits reuse even a closed pane's history/bookmark-only capture.
    pub fn import_source(&mut self, expected: u64, attempt: u64, origin: ImportedSourceId,
        label: &str, bytes: &[u8], offset: u64, selection: Option<ByteRange>,
        mut canceled: impl FnMut() -> bool) -> Result<DeskImport, DeskSessionError> {
        self.validate_mutation(expected, attempt)?;
        check(&mut canceled)?;
        if origin.file.owner() != origin.revision.owner() { return Err(DeskError::OwnerMismatch.into()); }
        if label.is_empty() || label.len() > MAX_READING_PATH_BYTES { return Err(DeskError::InvalidLabel.into()); }
        if bytes.len() as u64 > self.desk.limits().source_bytes { return Err(DeskError::SourceLimit.into()); }
        if offset > bytes.len() as u64 || selection.is_some_and(|r| r.end().get() > bytes.len() as u64) {
            return Err(DeskError::InvalidLocation.into());
        }
        // No retained byte clone in this table. Evicted/restored identities do
        // not pin memory and cannot alias new imports; desk IDs never recycle.
        for slot in &mut self.imports {
            if slot.is_some_and(|link| self.desk.retained_capture(expected, link.file, link.revision).is_err()) {
                *slot = None;
            }
        }
        if let Some(link) = self.imports.iter().flatten().find(|link| link.origin == origin).copied() {
            let source = self.desk.retained_capture(expected, link.file, link.revision)?;
            if source.logical_path() != label || source.bytes().len() != bytes.len() {
                return Err(DeskError::IdentityConflict.into());
            }
            for (left, right) in source.bytes().chunks(64 * 1024).zip(bytes.chunks(64 * 1024)) {
                check(&mut canceled)?;
                if left != right { return Err(DeskError::IdentityConflict.into()); }
            }
            // The old desk's reservation owns this shared backing until open's
            // transactional publication either retains it or leaves state intact.
            let source = source.clone();
            let change = self.adopt(expected, attempt, source, offset, selection, &mut canceled)?;
            return Ok(DeskImport { change, origin, file: link.file, revision: link.revision, reused_capture: true });
        }
        let slot = self.imports.iter().position(Option::is_none).ok_or(DeskError::SourceLimit)?;
        if self.desk.retained_source_count() >= self.desk.limits().sources { return Err(DeskError::SourceLimit.into()); }
        if bytes.len() as u64 > self.desk.limits().retained_bytes.saturating_sub(self.desk.retained_source_bytes()) {
            return Err(DeskError::RetainedByteLimit.into());
        }
        let next = self.next_source;
        self.next_source = next.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        let revision = SourceRevision::new(self.desk.owner(), next).map_err(|_| DeskError::IdentityExhausted)?;
        // Captured revisions of one provider file keep their logical relation,
        // while different owners with equal ordinals remain distinct files.
        let file = match self.imports.iter().flatten().find(|link| link.origin.file == origin.file) {
            Some(link) => link.file,
            None => FileId::new(self.desk.owner(), next).map_err(|_| DeskError::IdentityExhausted)?,
        };
        let allocation = self.next_id()?;
        let charge = bytes.len().checked_mul(2).and_then(|n| n.checked_add(2 * label.len() + 4096))
            .ok_or(AppError::Admission)?;
        let _scratch = self.budget.try_reserve_managed(self.desk.owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut owned = Vec::new();
        owned.try_reserve_exact(bytes.len()).map_err(|_| AppError::Admission)?;
        if owned.capacity() > bytes.len() { return Err(AppError::Admission.into()); }
        for part in bytes.chunks(64 * 1024) { check(&mut canceled)?; owned.extend_from_slice(part); }
        check(&mut canceled)?;
        let capture = SourceCapture::from_bytes(self.desk.owner(), file, revision, label, owned)
            .map_err(|_| DeskError::InvalidLocation)?;
        let change = self.adopt(expected, attempt, capture, offset, selection, &mut canceled)?;
        // No fallible operation after publication. Future eviction merely leaves
        // a stale bounded metadata entry, removed at the next import.
        self.imports[slot] = Some(ImportLink { origin, file, revision });
        Ok(DeskImport { change, origin, file, revision, reused_capture: false })
    }
}

// This bound is shared with the core source residency limit, not a growing
// content-addressed cache. The array is charged by DeskSession's size_of.
const _: () = assert!(MAX_DESK_SOURCES == 64);
