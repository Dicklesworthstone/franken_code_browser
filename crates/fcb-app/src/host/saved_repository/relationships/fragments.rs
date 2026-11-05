#![forbid(unsafe_code)]

//! Fragment qualification is a projection of the upstream document reader's
//! canonical heading identities and original source spans. There is no local
//! slugger, Markdown parser, live-file lookup or fallback line-number parser.

use std::{mem::size_of, sync::Arc};
use fcb_core::{DocumentGeneration, DocumentId};
use fcb::document::reader::{DocumentReadError, DocumentReadOptions, DocumentReader};
use fcb::source::{CompleteCapture, SourceError, path::decode_uri_component_bytes};
use super::{AcceptedRelationships, AppError, ByteLength, ByteRange, CaptureRequest,
    FileId, Link, MAX_DOCUMENT_LINK_DESTINATION_BYTES, MAX_DOCUMENT_LINK_SOURCE_BYTES,
    Output, OutputError, PagedCapture, PagedMemberData, ReaderSessionError, Resolution,
    SavedRelationshipError, SavedRepositoryError, SavedRepositorySession, SourceRevision,
    check, markdown, member_id};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(in crate::host::saved_repository) enum FragmentResolution {
    None, DocumentStart, Resolved(ByteRange), MissingHeading, Ambiguous,
    Limited, Unsupported, Invalid, Unresolved,
}
impl FragmentResolution {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none", Self::DocumentStart => "document-start",
            Self::Resolved(_) => "resolved", Self::MissingHeading => "missing-heading",
            Self::Ambiguous => "ambiguous", Self::Limited => "limited",
            Self::Unsupported => "unsupported", Self::Invalid => "invalid", Self::Unresolved => "unresolved",
        }
    }
    pub const fn range(self) -> Option<ByteRange> {
        match self { Self::Resolved(range) => Some(range), _ => None }
    }
}

pub(super) fn prepare_fragment(resolution: Resolution, suffix: &str)
    -> Result<(FragmentResolution, Option<String>), SavedRelationshipError> {
    let Some((_, encoded)) = suffix.split_once('#') else { return Ok((FragmentResolution::None, None)); };
    if encoded.is_empty() {
        return Ok((if resolution == Resolution::Captured { FragmentResolution::DocumentStart }
            else { FragmentResolution::Unresolved }, None));
    }
    let bytes = match decode_uri_component_bytes(encoded, MAX_DOCUMENT_LINK_DESTINATION_BYTES) {
        Ok(bytes) => bytes,
        Err(SourceError::PayloadTooLarge) => return Err(AppError::Admission.into()),
        Err(_) => return Ok((FragmentResolution::Invalid, None)),
    };
    let slug = match String::from_utf8(bytes) {
        Ok(slug) if !slug.chars().any(char::is_control) => slug,
        _ => return Ok((FragmentResolution::Invalid, None)),
    };
    // This is only the decoded identity requested by the URL, never a slug
    // synthesized from a heading title. Qualification happens below.
    Ok((FragmentResolution::Unresolved, Some(slug)))
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
    /// Resolve each referenced captured Markdown member once, on the worker.
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
            else { link.fragment = FragmentResolution::Unsupported; }
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
        if length > MAX_DOCUMENT_LINK_SOURCE_BYTES || length as u64 > max_source_bytes.saturating_sub(*admitted_source_bytes) {
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
        // PagedCapture's verified backing is private. Admit this one temporary
        // adapter copy before allocating it; no source payload survives build.
        let _source = self.budget.try_reserve_managed(owner, source_id,
            ByteLength::new((length + size_of::<CompleteCapture>() + 64) as u64)).map_err(|_| AppError::Admission)?;
        let request = CaptureRequest::new(file, revision).map_err(|_| AppError::InvalidRange)?;
        let complete = CompleteCapture::new(request, ByteLength::new(length as u64), Arc::from(capture.bytes()))
            .map_err(|_| AppError::InvalidRange)?;
        let options = DocumentReadOptions { max_source_bytes: MAX_DOCUMENT_LINK_SOURCE_BYTES, ..DocumentReadOptions::default() };
        let reader = match DocumentReader::prepare(&complete, id, generation, options, &self.budget, layout_id, &mut *canceled) {
            Ok(reader) => reader,
            Err(DocumentReadError::SourceLimit | DocumentReadError::EngineBudget | DocumentReadError::ResourceDenied) => {
                set_resolution(candidate, links, FragmentResolution::Limited); return Ok(());
            }
            Err(DocumentReadError::InvalidUtf8 | DocumentReadError::InvalidOutput | DocumentReadError::EngineFailure) => {
                set_resolution(candidate, links, FragmentResolution::Unsupported); return Ok(());
            }
            Err(DocumentReadError::Canceled) => return Err(SavedRepositoryError::Canceled.into()),
            Err(error) => return Err(ReaderSessionError::Document(error).into()),
        };
        match reader.qualify_heading_source_ranges(&self.budget, provenance_id, &mut *canceled) {
            Ok(()) => (),
            Err(DocumentReadError::EngineBudget | DocumentReadError::ResourceDenied) => {
                set_resolution(candidate, links, FragmentResolution::Limited); return Ok(());
            }
            Err(DocumentReadError::InvalidOutput) => {
                set_resolution(candidate, links, FragmentResolution::Unsupported); return Ok(());
            }
            Err(DocumentReadError::Canceled) => return Err(SavedRepositoryError::Canceled.into()),
            Err(error) => return Err(ReaderSessionError::Document(error).into()),
        }
        for &id in links {
            check(canceled)?;
            let link = &mut candidate.links[id];
            let slug = link.fragment_slug.as_deref().ok_or(SavedRelationshipError::UnresolvedTarget)?;
            // The upstream identity is authoritative, including its duplicate
            // suffixes. Refuse a collision instead of picking the first heading.
            let mut matches = reader.headings().iter().filter(|heading| heading.slug == slug);
            link.fragment = match (matches.next(), matches.next()) {
                (None, _) => FragmentResolution::MissingHeading,
                (Some(_), Some(_)) => FragmentResolution::Ambiguous,
                (Some(heading), None) => {
                    let range = reader.original_span(heading.source_span).map_err(ReaderSessionError::Document)?;
                    if range.start() == range.end() { FragmentResolution::Unsupported }
                    else { FragmentResolution::Resolved(range) }
                }
            };
        }
        Ok(())
    }
}

fn set_resolution(candidate: &mut AcceptedRelationships, links: &[usize], resolution: FragmentResolution) {
    for &id in links { candidate.links[id].fragment = resolution; }
}
