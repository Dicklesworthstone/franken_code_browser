#![forbid(unsafe_code)]

//! Verified offline members and hits enter the ordinary source-owning desk.
//! Archive labels confer no live filesystem authority. A selected member is
//! reverified on each import; accepted desk captures survive archive detachment,
//! corruption, query replacement and checkpoint restoration independently.
//! All loading, copying and destruction are bounded worker operations.

use fcb::ByteRange;
use crate::host::desk::{DeskSession, DeskSessionError, DeskError,
    imports::{DeskImport, ImportedSourceId}};
use super::{SavedRepositorySession, SavedRepositoryError, PagedCapture, PagedHit,
    PagedMemberData, PagedSearchError, PagedSnapshotError, FileId, SourceRevision, ByteLength,
    RawPath, Sha256Digest, HostResponse, OutputError, AppError, check};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SavedDeskError { Saved(SavedRepositoryError), Desk(DeskSessionError), Expression(expression::ExpressionError) }
impl std::fmt::Display for SavedDeskError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self { Self::Saved(e) => write!(f, "{e}"), Self::Desk(e) => write!(f, "{e}"),
            Self::Expression(e) => write!(f, "{e}") }
    }
}
impl std::error::Error for SavedDeskError {}
impl From<SavedRepositoryError> for SavedDeskError { fn from(e: SavedRepositoryError) -> Self { Self::Saved(e) } }
impl From<DeskSessionError> for SavedDeskError { fn from(e: DeskSessionError) -> Self { Self::Desk(e) } }
impl From<DeskError> for SavedDeskError { fn from(e: DeskError) -> Self { Self::Desk(e.into()) } }
impl From<PagedSearchError> for SavedDeskError { fn from(e: PagedSearchError) -> Self { Self::Saved(e.into()) } }
impl From<PagedSnapshotError> for SavedDeskError { fn from(e: PagedSnapshotError) -> Self { Self::Saved(e.into()) } }
impl From<OutputError> for SavedDeskError { fn from(e: OutputError) -> Self { Self::Saved(e.into()) } }
impl From<AppError> for SavedDeskError { fn from(e: AppError) -> Self { Self::Saved(e.into()) } }
impl From<expression::ExpressionError> for SavedDeskError {
    fn from(e: expression::ExpressionError) -> Self { Self::Expression(e) }
}
impl SavedDeskError {
    pub fn is_canceled(self) -> bool { match self { Self::Saved(e) => e.is_canceled(), Self::Desk(e) => e.is_canceled(),
        Self::Expression(expression::ExpressionError::Canceled) => true,
        Self::Expression(expression::ExpressionError::Search(e)) => SavedRepositoryError::Query(e).is_canceled(),
        Self::Expression(_) => false,
    } }
}

/// An accepted import, not a deferred instruction to look up a live pathname.
/// Original query identity is retained separately from the desk's fresh IDs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SavedDeskOpen {
    pub imported: DeskImport,
    archive: Sha256Digest,
    member: usize,
    source_digest: Sha256Digest,
    query: Option<(u64, u64)>,
    expression: Option<(u64, u64)>,
    selection: Option<ByteRange>,
    member_bytes_read: u64,
    member_read_calls: u64,
}
impl SavedDeskOpen {
    pub const fn archive_digest(&self) -> Sha256Digest { self.archive }
    pub const fn member(&self) -> usize { self.member }
    pub const fn selection(&self) -> Option<ByteRange> { self.selection }
}

impl SavedRepositorySession {
    pub fn open_member_desk(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        ordinal: usize, mut canceled: impl FnMut() -> bool) -> Result<SavedDeskOpen, SavedDeskError> {
        self.open_desk(desk, expected, attempt, ordinal, None, &mut canceled)
    }
    pub fn open_hit_desk(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        generation: u64, hit_id: u64, mut canceled: impl FnMut() -> bool) -> Result<SavedDeskOpen, SavedDeskError> {
        validate_desk(desk, expected, attempt)?; check(&mut canceled)?;
        let position = hit_id.checked_sub(1).and_then(|v| usize::try_from(v).ok())
            .ok_or(SavedRepositoryError::MissingHit)?;
        let hit = *self.accepted(generation)?.report.hits().get(position).ok_or(SavedRepositoryError::MissingHit)?;
        self.open_desk(desk, expected, attempt, hit.ordinal(), Some((hit_id, hit)), &mut canceled)
    }
    fn open_desk(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        ordinal: usize, selected: Option<(u64, PagedHit)>, canceled: &mut impl FnMut() -> bool)
        -> Result<SavedDeskOpen, SavedDeskError> {
        self.validate_desk_member(desk, expected, attempt, ordinal, canceled)?;
        let [load_id, pin_id] = self.allocations()?;
        let before = self.archive.load_stats();
        let capture = if let Some((_, hit)) = selected {
            PagedCapture::open_hit(&mut self.archive, hit, hit.generation(), &self.budget, [load_id, pin_id], &mut *canceled)?
        } else {
            let id = u64::try_from(ordinal).ok().and_then(|v| v.checked_add(1)).ok_or(SavedRepositoryError::IdentityExhausted)?;
            let file = FileId::new(self.owner(), id).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
            let revision = SourceRevision::new(self.owner(), id).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
            PagedCapture::load(&mut self.archive, ordinal, file, revision, &self.budget, [load_id, pin_id], &mut *canceled)?
        };
        let after = self.archive.load_stats();
        self.import_desk_capture(desk, expected, attempt, ordinal, &capture,
            selected.map(|(_, hit)| hit.original_range()),
            selected.map(|(id, hit)| (hit.generation().get(), id)),
            (after.bytes_read - before.bytes_read, after.read_calls - before.read_calls), canceled)
    }

    fn validate_desk_member(&self, desk: &DeskSession, expected: u64, attempt: u64,
        ordinal: usize, canceled: &mut impl FnMut() -> bool) -> Result<(), SavedDeskError> {
        validate_desk(desk, expected, attempt)?; check(canceled)?;
        if desk.model().owner() == self.owner() { return Err(DeskError::OwnerMismatch.into()); }
        let member = self.archive.directory().member(ordinal).ok_or(SavedRepositoryError::MissingHit)?;
        let length = match member.data {
            PagedMemberData::Captured { byte_length, .. } => byte_length,
            PagedMemberData::Unavailable(_) => return Err(PagedSnapshotError::Missing.into()),
        };
        if length as u64 > desk.model().limits().source_bytes || member.path.len() > 16_384 {
            return Err(DeskError::InvalidLocation.into());
        }
        Ok(())
    }

    /// One common import transaction for direct members, literal hits and
    /// expression witnesses. Callers supply only an already verified capture.
    fn import_desk_capture(&mut self, desk: &mut DeskSession, expected: u64, attempt: u64,
        ordinal: usize, capture: &PagedCapture, selection: Option<ByteRange>,
        query: Option<(u64, u64)>, io: (u64, u64), canceled: &mut impl FnMut() -> bool)
        -> Result<SavedDeskOpen, SavedDeskError> {
        let [label_id] = self.allocations()?;
        let path = self.archive.directory().member(ordinal).ok_or(SavedRepositoryError::MissingHit)?.path;
        // No intermediate source clone. The receiving import route accounts
        // for its own copy, or reuses the same archive member's exact identity.
        let _label = self.budget.try_reserve_managed(self.owner(), label_id,
            ByteLength::new((16 * path.len() + 512) as u64)).map_err(|_| AppError::Admission)?;
        let label = RawPath::from_bytes(path).display_escaped().to_string();
        let origin = ImportedSourceId { file: capture.file(), revision: capture.revision() };
        let archive = self.archive_digest(); let source_digest = capture.source_digest();
        check(canceled)?;
        let imported = desk.import_source(expected, attempt, origin, &label, capture.bytes(),
            selection.map_or(0, |r| r.start().get()), selection, &mut *canceled)?;
        // No fallible work or cancellation gate after the accepted transaction.
        Ok(SavedDeskOpen { imported, archive, member: ordinal, source_digest, query, expression: None, selection,
            member_bytes_read: io.0, member_read_calls: io.1 })
    }

    /// Encode an already accepted import. A response failure is not rollback.
    /// Metadata comes from the receipt; no archive/source load is performed.
    pub fn desk_open_response(&mut self, desk: &DeskSession, opened: &SavedDeskOpen)
        -> Result<HostResponse, SavedDeskError> {
        if opened.archive != self.archive_digest() || opened.imported.origin.file.owner() != self.owner()
            || opened.imported.file.owner() != desk.model().owner()
            || opened.imported.change.revision != desk.model().revision() { return Err(DeskError::StaleRevision.into()); }
        let mut out = self.output("open-desk")?;
        out.literal(",\"desk_owner\":")?; out.integer(desk.model().owner().get())?;
        out.literal(",\"model_revision\":")?; out.integer(opened.imported.change.revision)?;
        out.literal(",\"member\":")?; out.integer(opened.member as u64)?;
        out.literal(",\"source_digest\":")?; out.quoted(&opened.source_digest.to_hex())?;
        out.literal(",\"source_observation\":\"verified-saved-member\",\"live_source_reopened\":false,\"member_bytes_read\":")?;
        out.integer(opened.member_bytes_read)?;
        out.literal(",\"member_read_calls\":")?; out.integer(opened.member_read_calls)?;
        out.literal(",\"origin_file_id\":")?; out.integer(opened.imported.origin.file.get())?;
        out.literal(",\"origin_source_revision\":")?; out.integer(opened.imported.origin.revision.get())?;
        out.literal(",\"file_id\":")?; out.integer(opened.imported.file.get())?;
        out.literal(",\"source_revision\":")?; out.integer(opened.imported.revision.get())?;
        out.literal(",\"reused_capture\":")?; out.boolean(opened.imported.reused_capture)?;
        out.literal(",\"pane\":")?; super::optional(&mut out, opened.imported.change.active.map(|p| p.get()))?;
        out.literal(",\"query_generation\":")?; super::optional(&mut out, opened.query.map(|q| q.0))?;
        out.literal(",\"hit_id\":")?; super::optional(&mut out, opened.query.map(|q| q.1))?;
        out.literal(",\"expression_generation\":")?; super::optional(&mut out, opened.expression.map(|q| q.0))?;
        out.literal(",\"expression_hit_id\":")?; super::optional(&mut out, opened.expression.map(|q| q.1))?;
        out.literal(",\"selection_namespace\":")?;
        out.quoted(if opened.expression.is_some() { "saved-expression" } else if opened.query.is_some() { "saved-literal" } else { "saved-member" })?;
        out.literal(",\"selection\":")?;
        match opened.selection { Some(r) => out.range(r)?, None => out.literal("null")? }
        out.literal("}\n")?;
        Ok(self.finish(out, false, &mut || false)?)
    }
}
fn validate_desk(desk: &DeskSession, expected: u64, attempt: u64) -> Result<(), SavedDeskError> {
    if expected != desk.model().revision() { return Err(DeskError::StaleRevision.into()); }
    if attempt == 0 || attempt <= desk.model().last_attempt() { return Err(DeskError::StaleAttempt.into()); }
    Ok(())
}

/// Document-wide expression results and verified witness activation.
pub mod expression;
pub use expression::{SavedExpression, SavedExpressionOptions};
