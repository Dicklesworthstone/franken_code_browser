#![forbid(unsafe_code)]

//! Explicit, portable reading-desk checkpoints (FCB-041 / FCB-050).
//!
//! Uses the production store envelope/checksum, not a second serializer or hash
//! engine. Includes EACH retained capture once, including sources referenced only
//! by closed-pane history/bookmarks. Labels are not filesystem authority. This
//! module never opens files. Hosts must obtain consent before exporting sources.
//! Encoding/decoding are bounded worker operations; the existing envelope's
//! checksum pass is synchronous, with cancellation checked before and after it.

use super::*;
use crate::store::envelope::{EnvelopeError, EnvelopeLimits, EnvelopeReader,
    EnvelopeSchema, EnvelopeWriter, Sha256Digest, UnknownPolicy, FRAME_LEN};

pub const MAX_CHECKPOINT_BYTES: usize = 64 * 1024 * 1024;
const SCHEMA: EnvelopeSchema = EnvelopeSchema {
    magic: *b"FCBK", schema_id: 0x4b53_4544, major: 1, minor: 0,
};
const NONE: u64 = u64::MAX;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointError { Desk(DeskError), InvalidDocument, Limit, UnsupportedVersion, Integrity }
impl CheckpointError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Desk(e) => e.code(), Self::InvalidDocument => "DESK_CHECKPOINT_INVALID",
            Self::Limit => "DESK_CHECKPOINT_LIMIT", Self::UnsupportedVersion => "DESK_CHECKPOINT_VERSION",
            Self::Integrity => "DESK_CHECKPOINT_INTEGRITY",
        }
    }
    pub const fn is_canceled(self) -> bool { matches!(self, Self::Desk(DeskError::Canceled)) }
}
impl std::fmt::Display for CheckpointError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for CheckpointError {}
impl From<DeskError> for CheckpointError { fn from(e: DeskError) -> Self { Self::Desk(e) } }
impl From<EnvelopeError> for CheckpointError {
    fn from(e: EnvelopeError) -> Self {
        match e {
            EnvelopeError::ChecksumMismatch { .. } => Self::Integrity,
            EnvelopeError::LimitExceeded(_) => Self::Limit,
            EnvelopeError::BadMagic(_) | EnvelopeError::SchemaMismatch { .. }
                | EnvelopeError::MajorMismatch { .. } | EnvelopeError::MinorOlder { .. } => Self::UnsupportedVersion,
            _ => Self::InvalidDocument,
        }
    }
}

/// Owns its entire encoding reservation, including writer/finished-buffer overlap.
/// No source is silently omitted to meet a byte budget. Contains source payloads
/// and personal labels; an integrity digest is not encryption or authorization.
pub struct DeskCheckpoint { bytes: Vec<u8>, source_bytes: u64, _lease: ResourceLease }
impl DeskCheckpoint {
    pub fn bytes(&self) -> &[u8] { &self.bytes }
    pub const fn source_bytes(&self) -> u64 { self.source_bytes }
    pub fn digest(&self) -> Sha256Digest {
        Sha256Digest::new(self.bytes[self.bytes.len() - 32..].try_into().expect("finished envelope"))
    }
}
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct RestoredDesk { pub change: DeskChange, pub next_source_identity: u64 }

impl ReadingDesk {
    /// Capture the complete current workspace of readers without I/O. Output
    /// order is deterministic for one state; transient query results are not
    /// serialized as authoritative future search results.
    pub fn checkpoint(&self, expected: u64, allocation: ResourceAllocationId,
        mut canceled: impl FnMut() -> bool) -> Result<DeskCheckpoint, CheckpointError> {
        self.validate(expected)?;
        check(&mut canceled)?;
        let state = &self.state;
        let source_bytes = self.retained_source_bytes();
        let labels: usize = state.sources.iter().map(|s| s.capture.logical_path().len()).sum();
        let notes: usize = state.bookmarks.iter().map(|b| b.label.len()).sum();
        // A checked upper bound on the exact positional encoding, then a
        // conservative allowance for the existing writer's geometric growth.
        let bound = usize::try_from(source_bytes).ok().and_then(|n| n.checked_add(labels))
            .and_then(|n| n.checked_add(notes + FRAME_LEN + 128 + state.sources.len() * 64
                + state.panes.len() * 128 + state.history.len() * 64 + state.bookmarks.len() * 64))
            .ok_or(CheckpointError::Limit)?;
        if bound > MAX_CHECKPOINT_BYTES { return Err(CheckpointError::Limit); }
        let lease = self.budget.try_reserve_managed(self.owner, allocation,
            ByteLength::new((4 * bound + size_of::<DeskCheckpoint>()) as u64))
            .map_err(|_| DeskError::ResourceDenied)?;
        let mut out = EnvelopeWriter::new(SCHEMA);
        out.put_u64(state.sources.len() as u64);
        out.put_u64(state.panes.len() as u64);
        out.put_u64(state.history.len() as u64);
        out.put_u64(state.bookmarks.len() as u64);
        out.put_u64(state.cursor.map_or(NONE, |n| n as u64));
        out.put_u64(state.panes.active_pane_id().unwrap_or(0));
        for retained in &state.sources {
            check(&mut canceled)?;
            let source = &retained.capture;
            out.put_u64(source.file().get()); out.put_u64(source.revision().get());
            out.put_str(source.logical_path()); out.put_bytes(source.bytes());
        }
        for pane in state.panes.iter() {
            check(&mut canceled)?;
            out.put_u64(pane.id); out.put_u8(u8::from(pane.is_pinned));
            for value in [pane.position.0, pane.position.1, pane.size.0, pane.size.1,
                pane.scroll_offset.0, pane.scroll_offset.1] { out.put_f32(value); }
            put_location(&mut out, state, location(state, self.owner, self.id(pane.id))?)?;
        }
        for at in &state.history { put_location(&mut out, state, *at)?; }
        for mark in &state.bookmarks { out.put_str(&mark.label); put_location(&mut out, state, mark.location)?; }
        check(&mut canceled)?;
        let bytes = out.finish();
        if bytes.len() > bound || bytes.capacity() > 4 * bound { return Err(CheckpointError::Limit); }
        check(&mut canceled)?;
        Ok(DeskCheckpoint { bytes, source_bytes, _lease: lease })
    }

    /// Atomically replace a desk with a validated checkpoint. All source/pane/
    /// bookmark identities are freshly allocated in THIS owner domain. Old
    /// exported views remain valid, but their identities cannot alias the new
    /// model. Attempts and identity high-water marks are consumed on failure;
    /// visible state is not. Limits are the caller's, never imported policy.
    pub fn restore_checkpoint(&mut self, expected: u64, attempt: u64, bytes: &[u8],
        allocation: ResourceAllocationId, mut canceled: impl FnMut() -> bool)
        -> Result<RestoredDesk, CheckpointError> {
        self.validate(expected)?;
        if attempt == 0 || attempt <= self.last_attempt { return Err(DeskError::StaleAttempt.into()); }
        self.last_attempt = attempt;
        check(&mut canceled)?;
        let limits = EnvelopeLimits { max_document_bytes: MAX_CHECKPOINT_BYTES,
            max_payload_bytes: MAX_CHECKPOINT_BYTES - FRAME_LEN, max_field_bytes: MAX_CHECKPOINT_BYTES };
        let mut input = EnvelopeReader::open(bytes, SCHEMA, limits, UnknownPolicy::Strict)?;
        // This version accepts no extension fields or header flags. Check wide
        // lengths BEFORE the underlying envelope's usize conversions as well.
        if input.schema().minor != 0 || bytes[12..16] != [0, 0, 0, 0] {
            return Err(CheckpointError::UnsupportedVersion);
        }
        let payload = u64::from_le_bytes(bytes[16..24].try_into().map_err(|_| CheckpointError::InvalidDocument)?);
        if payload != (bytes.len() - FRAME_LEN) as u64 { return Err(CheckpointError::InvalidDocument); }
        check(&mut canceled)?;
        let ns = bounded_count(input.get_u64()?, self.limits.sources)?;
        let np = bounded_count(input.get_u64()?, self.limits.panes)?;
        let nh = bounded_count(input.get_u64()?, self.limits.history)?;
        let nb = bounded_count(input.get_u64()?, self.limits.bookmarks)?;
        let cursor = input.get_u64()?;
        let active = input.get_u64()?;
        if (nh == 0 && cursor != NONE) || (nh != 0 && cursor >= nh as u64)
            || ((np == 0) != (active == 0)) { return Err(CheckpointError::InvalidDocument); }
        let mut records: [Option<SourceRecord<'_>>; MAX_DESK_SOURCES] = [None; MAX_DESK_SOURCES];
        let mut total = 0u64;
        let mut labels = 0usize;
        for i in 0..ns {
            check(&mut canceled)?;
            let file = input.get_u64()?; let revision = input.get_u64()?;
            if file == 0 || revision == 0 || records[..i].iter().flatten()
                .any(|r| r.file == file && r.revision == revision) { return Err(CheckpointError::InvalidDocument); }
            let label = get_label(&mut input, MAX_READING_PATH_BYTES)?;
            let data = get_field(&mut input, self.limits.source_bytes)?;
            total = total.checked_add(data.len() as u64).ok_or(CheckpointError::Limit)?;
            if total > self.limits.retained_bytes { return Err(CheckpointError::Limit); }
            labels += label.len();
            records[i] = Some(SourceRecord { file, revision, label, data });
        }
        let first = self.source_high_water.checked_add(1).ok_or(DeskError::IdentityExhausted)?;
        let next_source = first.checked_add(ns as u64).ok_or(DeskError::IdentityExhausted)?;
        self.source_high_water = next_source - 1;
        // Admit old + candidate overlap while the old desk is still retained.
        // Imported captures share a batch lease, conservatively held until its
        // LAST source/exported view retires. Conversion scratch is precharged.
        let lease = self.budget.try_reserve_managed(self.owner, allocation,
            ByteLength::new(total.checked_mul(2).and_then(|n| n.checked_add(
                (2 * labels + ns * (size_of::<RetainedSource>() + 128) + 4096) as u64))
                .ok_or(CheckpointError::Limit)?)).map_err(|_| DeskError::ResourceDenied)?;
        let mut next = State { revision: self.revision(), panes: ReadingPaneManager::new(),
            offsets: Vec::new(), sources: Vec::new(), history: Vec::new(),
            cursor: if nh == 0 { None } else { Some(cursor as usize) }, bookmarks: Vec::new(),
            next_bookmark: self.state.next_bookmark };
        next.panes.next_id = self.state.panes.next_id;
        reserve_state(&mut next, self.limits)?;
        for i in 0..ns {
            check(&mut canceled)?;
            let r = records[i].ok_or(CheckpointError::InvalidDocument)?;
            // Distinct captured revisions of one logical file keep that relation.
            let file_slot = records[..i].iter().position(|v| v.is_some_and(|v| v.file == r.file)).unwrap_or(i);
            let mut owned = Vec::new();
            owned.try_reserve_exact(r.data.len()).map_err(|_| DeskError::ResourceDenied)?;
            if owned.capacity() > r.data.len() { return Err(DeskError::ResourceDenied.into()); }
            owned.extend_from_slice(r.data);
            let capture = SourceCapture::from_bytes(self.owner,
                FileId::new(self.owner, first + file_slot as u64).map_err(|_| DeskError::IdentityExhausted)?,
                SourceRevision::new(self.owner, first + i as u64).map_err(|_| DeskError::IdentityExhausted)?,
                r.label, owned).map_err(|_| CheckpointError::InvalidDocument)?;
            next.sources.push(Arc::new(RetainedSource { capture, _lease: lease.clone() }));
        }
        let mut pane_map = [(0u64, 0u64); MAX_DESK_PANES];
        for i in 0..np {
            check(&mut canceled)?;
            let old_id = input.get_u64()?;
            if old_id == 0 || pane_map[..i].iter().any(|p| p.0 == old_id) { return Err(CheckpointError::InvalidDocument); }
            let pinned = get_bool(&mut input)?;
            let position = (input.get_f32()?, input.get_f32()?);
            let size = (input.get_f32()?, input.get_f32()?);
            let scroll = (input.get_f32()?, input.get_f32()?);
            if ![position.0, position.1, size.0, size.1, scroll.0, scroll.1].iter().all(|v| v.is_finite() && *v >= 0.0)
                || size.0 == 0.0 || size.1 == 0.0 { return Err(CheckpointError::InvalidDocument); }
            let (at, hint) = get_location(&mut input, &next, self.owner)?;
            if hint != old_id { return Err(CheckpointError::InvalidDocument); }
            let id = next.panes.next_id;
            next.panes.next_id = id.checked_add(1).filter(|_| id != 0).ok_or(DeskError::IdentityExhausted)?;
            pane_map[i] = (old_id, id);
            let mut pane = ReadingPane::new(id, at.file, source(&next, at.file, at.revision)?.logical_path().to_owned(), position);
            pane.revision = Some(at.revision); pane.is_pinned = pinned; pane.size = size; pane.scroll_offset = scroll;
            pane.selection = at.selection.map(|r| r.as_usize_bounds().map_err(|_| CheckpointError::InvalidDocument)).transpose()?;
            next.panes.panes.push(pane); next.offsets.push((id, at.offset));
        }
        next.panes.active_pane_id = if active == 0 { None } else {
            Some(pane_map[..np].iter().find(|p| p.0 == active).ok_or(CheckpointError::InvalidDocument)?.1)
        };
        for _ in 0..nh {
            let (mut at, hint) = get_location(&mut input, &next, self.owner)?;
            at.preferred_pane = self.id(remap_hint(&pane_map[..np], hint));
            next.history.push(at);
        }
        for _ in 0..nb {
            let label = get_label(&mut input, MAX_BOOKMARK_LABEL_BYTES)?.to_owned();
            let (mut at, hint) = get_location(&mut input, &next, self.owner)?;
            at.preferred_pane = self.id(remap_hint(&pane_map[..np], hint));
            let id = next.next_bookmark;
            next.next_bookmark = id.checked_add(1).filter(|_| id != 0).ok_or(DeskError::IdentityExhausted)?;
            next.bookmarks.push(DeskBookmark { id, label, location: at });
        }
        if input.remaining() != 0 { return Err(CheckpointError::InvalidDocument); }
        // Reject hidden/unreferenced source payloads, rather than silently
        // discarding part of an allegedly complete personal-state checkpoint.
        for s in &next.sources {
            let f = s.capture.file(); let r = s.capture.revision();
            if !next.panes.iter().any(|p| p.file_id == f && p.revision == Some(r))
                && !next.history.iter().any(|at| at.file == f && at.revision == r)
                && !next.bookmarks.iter().any(|b| b.location.file == f && b.location.revision == r) {
                return Err(CheckpointError::InvalidDocument);
            }
        }
        let change = self.publish(next, attempt, None, &mut canceled)?;
        Ok(RestoredDesk { change, next_source_identity: next_source })
    }
}

#[derive(Clone, Copy)]
struct SourceRecord<'a> { file: u64, revision: u64, label: &'a str, data: &'a [u8] }
fn bounded_count(n: u64, limit: usize) -> Result<usize, CheckpointError> {
    if n > limit as u64 { Err(CheckpointError::Limit) } else { Ok(n as usize) }
}
fn get_field<'a>(input: &mut EnvelopeReader<'a>, limit: u64) -> Result<&'a [u8], CheckpointError> {
    let mut peek = *input;
    let length = peek.get_u64()?;
    if length > limit || length > peek.remaining() as u64 { return Err(CheckpointError::Limit); }
    Ok(input.get_bytes()?)
}
fn get_label<'a>(input: &mut EnvelopeReader<'a>, limit: usize) -> Result<&'a str, CheckpointError> {
    let bytes = get_field(input, limit as u64)?;
    if bytes.is_empty() { return Err(CheckpointError::InvalidDocument); }
    std::str::from_utf8(bytes).map_err(|_| CheckpointError::InvalidDocument)
}
fn get_bool(input: &mut EnvelopeReader<'_>) -> Result<bool, CheckpointError> {
    match input.get_u8()? { 0 => Ok(false), 1 => Ok(true), _ => Err(CheckpointError::InvalidDocument) }
}
fn put_location(out: &mut EnvelopeWriter, state: &State, at: DeskLocation) -> Result<(), CheckpointError> {
    let index = state.sources.iter().position(|s| s.capture.file() == at.file && s.capture.revision() == at.revision)
        .ok_or(CheckpointError::InvalidDocument)?;
    out.put_u64(index as u64); out.put_u64(at.offset);
    out.put_u8(u8::from(at.selection.is_some()));
    if let Some(r) = at.selection { out.put_u64(r.start().get()); out.put_u64(r.end().get()); }
    out.put_u64(at.preferred_pane.get());
    Ok(())
}
fn get_location(input: &mut EnvelopeReader<'_>, state: &State, owner: ArenaOwnerId)
    -> Result<(DeskLocation, u64), CheckpointError> {
    let index = input.get_u64()?;
    if index >= state.sources.len() as u64 { return Err(CheckpointError::InvalidDocument); }
    let capture = &state.sources[index as usize].capture;
    let offset = input.get_u64()?;
    let selection = if get_bool(input)? {
        Some(ByteRange::new(ByteOffset::new(input.get_u64()?), ByteOffset::new(input.get_u64()?))
            .map_err(|_| CheckpointError::InvalidDocument)?)
    } else { None };
    validate_location(capture, offset, selection)?;
    let hint = input.get_u64()?;
    Ok((DeskLocation { file: capture.file(), revision: capture.revision(), offset, selection,
        preferred_pane: DeskPaneId { owner, value: 0 } }, hint))
}
fn remap_hint(map: &[(u64, u64)], old: u64) -> u64 {
    if old == 0 { 0 } else { map.iter().find(|p| p.0 == old).map_or(0, |p| p.1) }
}

#[cfg(test)]
mod tests;
