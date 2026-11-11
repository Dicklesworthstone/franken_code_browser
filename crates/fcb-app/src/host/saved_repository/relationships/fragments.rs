#![forbid(unsafe_code)]

//! Markdown qualification projects upstream canonical heading identities and
//! original spans. Explicit source-line anchors use the shared physical-line
//! scanner on verified archive bytes. Neither route performs live-file lookup.

use std::{mem::size_of, sync::Arc};
use fcb_core::{ByteRange, DocumentGeneration, DocumentId};
use fcb::document::reader::DocumentReadError;
use fcb::source::{CompleteCapture, line_index::LineNumber};
use crate::host::document_fragment::{self, QualifiedDocumentFragments,
    QualifiedSourceLines, SourceLineFragment, MAX_SOURCE_LINE_FRAGMENT_BYTES};
pub(in crate::host::saved_repository) use crate::host::document_fragment::FragmentResolution;
use super::{AcceptedRelationships, AppError, ByteLength, CaptureRequest,
    FileId, Link, MAX_DOCUMENT_LINK_SOURCE_BYTES,
    Output, OutputError, PagedCapture, PagedMemberData, ReaderSessionError, Resolution,
    SavedRelationshipError, SavedRepositoryError, SavedRepositorySession, SourceRevision,
    check, markdown, member_id};

pub(super) fn prepare_fragment(resolution: Resolution, suffix: &str)
    -> Result<(FragmentResolution, Option<String>), SavedRelationshipError> {
    Ok(document_fragment::prepare_fragment(resolution == Resolution::Captured, suffix)?)
}

pub(super) fn encode_fragment(out: &mut Output, link: &Link) -> Result<(), OutputError> {
    out.literal(",\"fragment_resolution\":")?; out.quoted(link.fragment.name())?;
    out.literal(",\"fragment_slug\":")?;
    match &link.fragment_slug { Some(slug) => out.quoted(slug)?, None => out.literal("null")? }
    out.literal(",\"target_range\":")?;
    match link.fragment.range() { Some(range) => out.range(range)?, None => out.literal("null")? }
    Ok(())
}

impl SavedRepositorySession {
    /// Resolve each referenced captured member once, on the worker.
    /// The same total source-read budget covers extraction AND qualification.
    /// A source/flow cap never turns an unexamined heading into "missing".
    pub(super) fn qualify_relationship_fragments(&mut self, candidate: &mut AcceptedRelationships,
        max_source_bytes: u64, admitted_source_bytes: &mut u64, canceled: &mut impl FnMut() -> bool)
        -> Result<(), SavedRelationshipError> {
        let [scratch_id] = self.allocations()?;
        let capacity = candidate.links.len();
        let charge = capacity.checked_mul(size_of::<usize>()).and_then(|n| n.checked_add(128))
            .ok_or(AppError::Admission)?;
        let _scratch = self.budget.try_reserve_managed(self.owner(), scratch_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut pending = Vec::new();
        pending.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
        if pending.capacity() > capacity { return Err(AppError::Admission.into()); }
        for (id, link) in candidate.links.iter_mut().enumerate() {
            check(canceled)?;
            if link.resolution != Resolution::Captured || link.fragment != FragmentResolution::Unresolved
                || link.fragment_slug.is_none() { continue; }
            let member = self.archive.directory().member(link.target_member.ok_or(SavedRelationshipError::MissingLink)?)
                .ok_or(SavedRelationshipError::MissingLink)?;
            if markdown(member.path) { pending.push(id); }
            else {
                match SourceLineFragment::parse(link.fragment_slug.as_deref().ok_or(SavedRelationshipError::UnresolvedTarget)?) {
                    Ok(_) => pending.push(id),
                    Err(refusal) => {
                        link.fragment = refusal;
                        if refusal == FragmentResolution::Invalid { link.fragment_slug = None; }
                    }
                }
            }
        }
        pending.sort_unstable_by_key(|&id| candidate.links[id].target_member);
        let mut start = 0;
        while start < pending.len() {
            check(canceled)?;
            let ordinal = candidate.links[pending[start]].target_member.ok_or(SavedRelationshipError::MissingLink)?;
            let end = start + pending[start..].partition_point(|&id| candidate.links[id].target_member == Some(ordinal));
            self.qualify_relationship_member(candidate, &pending[start..end], ordinal,
                max_source_bytes, admitted_source_bytes, canceled)?;
            start = end;
        }
        Ok(())
    }

    fn qualify_relationship_member(&mut self, candidate: &mut AcceptedRelationships, links: &[usize], ordinal: usize,
        max_source_bytes: u64, admitted_source_bytes: &mut u64, canceled: &mut impl FnMut() -> bool)
        -> Result<(), SavedRelationshipError> {
        let member = self.archive.directory().member(ordinal).ok_or(SavedRelationshipError::MissingLink)?;
        let length = match member.data {
            PagedMemberData::Captured { byte_length, .. } => byte_length,
            PagedMemberData::Unavailable(_) => return Err(SavedRelationshipError::TargetUnavailable),
        };
        let is_markdown = markdown(member.path);
        let source_limit = if is_markdown { MAX_DOCUMENT_LINK_SOURCE_BYTES } else { MAX_SOURCE_LINE_FRAGMENT_BYTES };
        if length > source_limit || length as u64 > max_source_bytes.saturating_sub(*admitted_source_bytes) {
            set_resolution(candidate, links, FragmentResolution::Limited);
            return Ok(());
        }
        let [load_id, pin_id, source_id, layout_id, provenance_id] = self.allocations()?;
        let owner = self.owner();
        let value = member_id(ordinal)?;
        let file = FileId::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let revision = SourceRevision::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let id = DocumentId::new(owner, value).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        let generation = DocumentGeneration::new(owner, candidate.generation).map_err(|_| SavedRepositoryError::IdentityExhausted)?;
        *admitted_source_bytes += length as u64;
        let capture = PagedCapture::load(&mut self.archive, ordinal, file, revision,
            &self.budget, [load_id, pin_id], &mut *canceled)?;
        if links.iter().any(|&id| candidate.links[id].target_digest != Some(capture.source_digest())) {
            return Err(SavedRelationshipError::UnresolvedTarget);
        }
        if !is_markdown {
            return self.qualify_source_line_fragments(candidate, links, capture.bytes(), canceled);
        }
        // PagedCapture's verified backing is private. Admit this one temporary
        // adapter copy before allocating it; no source payload survives build.
        let _source = self.budget.try_reserve_managed(owner, source_id,
            ByteLength::new((length + size_of::<CompleteCapture>() + 64) as u64)).map_err(|_| AppError::Admission)?;
        let request = CaptureRequest::new(file, revision).map_err(|_| AppError::InvalidRange)?;
        let complete = CompleteCapture::new(request, ByteLength::new(length as u64), Arc::from(capture.bytes()))
            .map_err(|_| AppError::InvalidRange)?;
        let qualified = QualifiedDocumentFragments::prepare(&complete, id, generation,
            &self.budget, [layout_id, provenance_id], &mut *canceled).map_err(fragment_error)?;
        for &id in links {
            check(canceled)?;
            let link = &mut candidate.links[id];
            let slug = link.fragment_slug.as_deref().ok_or(SavedRelationshipError::UnresolvedTarget)?;
            link.fragment = qualified.resolve(slug, &mut *canceled).map_err(fragment_error)?;
        }
        Ok(())
    }

    fn qualify_source_line_fragments(&mut self, candidate: &mut AcceptedRelationships,
        links: &[usize], bytes: &[u8], canceled: &mut impl FnMut() -> bool)
        -> Result<(), SavedRelationshipError> {
        // A member can have thousands of overlapping incoming line ranges.
        // Sort their bounded endpoints, then scan the captured stream once per
        // interval rather than re-examining the whole file for every link.
        let capacity = links.len().checked_mul(2).ok_or(AppError::Admission)?;
        let charge = capacity.checked_mul(size_of::<(LineNumber, bool, usize)>())
            .and_then(|n| n.checked_add(128)).ok_or(AppError::Admission)?;
        let [scratch_id] = self.allocations()?;
        let _scratch = self.budget.try_reserve_managed(self.owner(), scratch_id, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut endpoints = Vec::new();
        endpoints.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
        if endpoints.capacity() > capacity { return Err(AppError::Admission.into()); }
        for &id in links {
            check(canceled)?;
            let slug = candidate.links[id].fragment_slug.as_deref().ok_or(SavedRelationshipError::UnresolvedTarget)?;
            let fragment = SourceLineFragment::parse(slug).map_err(|_| SavedRelationshipError::UnresolvedTarget)?;
            endpoints.push((fragment.first, false, id));
            endpoints.push((fragment.last, true, id));
        }
        endpoints.sort_unstable(); // Starts precede ends at the same line.
        let mut qualified = QualifiedSourceLines::prepare(bytes, MAX_SOURCE_LINE_FRAGMENT_BYTES);
        for (line, is_end, id) in endpoints {
            check(canceled)?;
            let resolution = qualified.line(line, canceled)?;
            let link = &mut candidate.links[id];
            link.fragment = if !is_end { resolution } else {
                match (link.fragment, resolution) {
                    (FragmentResolution::ResolvedLines(first), FragmentResolution::ResolvedLines(last)) => {
                        FragmentResolution::ResolvedLines(ByteRange::new(first.start(), last.end())
                            .map_err(|_| AppError::InvalidRange)?)
                    }
                    (FragmentResolution::ResolvedLines(_), refusal) => refusal,
                    (refusal, _) => refusal,
                }
            };
        }
        Ok(())
    }
}

fn fragment_error(error: DocumentReadError) -> SavedRelationshipError {
    match error { DocumentReadError::Canceled => SavedRepositoryError::Canceled.into(),
        other => ReaderSessionError::Document(other).into() }
}

fn set_resolution(candidate: &mut AcceptedRelationships, links: &[usize], resolution: FragmentResolution) {
    for &id in links { candidate.links[id].fragment = resolution; }
}
