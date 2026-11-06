#![forbid(unsafe_code)]

//! Shared fragment qualification over complete, immutable captures. Upstream
//! Markdown owns parsing, canonical heading identities and source spans. This
//! adapter decodes a requested URI identity once, qualifies the whole heading
//! catalog, and returns original ranges without publishing a reader generation.

use fcb::document::links::{MAX_DOCUMENT_LINK_DESTINATION_BYTES, MAX_DOCUMENT_LINK_SOURCE_BYTES};
use fcb::document::reader::{DocumentReadError, DocumentReadOptions, DocumentReader};
use fcb::source::{CompleteCapture, SourceError, path::decode_uri_component_bytes};
use fcb_core::{ByteRange, DocumentGeneration, DocumentId, ResourceAllocationId, ResourceBudget};
use crate::AppError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FragmentResolution {
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

/// Decode only the fragment portion, exactly once. A nonempty decoded string
/// is a requested canonical identity, never a locally synthesized heading slug.
pub(crate) fn prepare_fragment(captured: bool, suffix: &str)
    -> Result<(FragmentResolution, Option<String>), AppError> {
    let Some((_, encoded)) = suffix.split_once('#') else { return Ok((FragmentResolution::None, None)); };
    if encoded.is_empty() {
        return Ok((if captured { FragmentResolution::DocumentStart }
            else { FragmentResolution::Unresolved }, None));
    }
    let bytes = match decode_uri_component_bytes(encoded, MAX_DOCUMENT_LINK_DESTINATION_BYTES) {
        Ok(bytes) => bytes,
        Err(SourceError::PayloadTooLarge) => return Err(AppError::Admission),
        Err(_) => return Ok((FragmentResolution::Invalid, None)),
    };
    let slug = match String::from_utf8(bytes) {
        Ok(slug) if !slug.chars().any(char::is_control) => slug,
        _ => return Ok((FragmentResolution::Invalid, None)),
    };
    Ok((FragmentResolution::Unresolved, Some(slug)))
}

pub(crate) fn markdown(path: &[u8]) -> bool {
    let name = path.rsplit(|&byte| byte == b'/').next().unwrap_or(path);
    let extension = name.iter().rposition(|&byte| byte == b'.').map(|index| &name[index + 1..]).unwrap_or(&[]);
    extension.eq_ignore_ascii_case(b"md") || extension.eq_ignore_ascii_case(b"markdown")
}

/// A temporary borrowed layout qualified once for any number of incoming
/// fragments. Refused layouts retain no parse output and cannot claim absence.
pub(crate) struct QualifiedDocumentFragments<'capture> {
    reader: Option<DocumentReader<'capture>>,
    refusal: FragmentResolution,
}
impl<'capture> QualifiedDocumentFragments<'capture> {
    pub fn prepare(capture: &'capture CompleteCapture, id: DocumentId,
        generation: DocumentGeneration, budget: &ResourceBudget,
        allocations: [ResourceAllocationId; 2], canceled: &mut impl FnMut() -> bool)
        -> Result<Self, DocumentReadError> {
        let [layout_id, provenance_id] = allocations;
        let options = DocumentReadOptions { max_source_bytes: MAX_DOCUMENT_LINK_SOURCE_BYTES,
            ..DocumentReadOptions::default() };
        let reader = match DocumentReader::prepare(capture, id, generation, options,
            budget, layout_id, &mut *canceled) {
            Ok(reader) => reader,
            Err(DocumentReadError::SourceLimit | DocumentReadError::EngineBudget | DocumentReadError::ResourceDenied) => {
                return Ok(Self::refused(FragmentResolution::Limited));
            }
            Err(DocumentReadError::InvalidUtf8 | DocumentReadError::InvalidOutput | DocumentReadError::EngineFailure) => {
                return Ok(Self::refused(FragmentResolution::Unsupported));
            }
            Err(error) => return Err(error),
        };
        match reader.qualify_heading_source_ranges(budget, provenance_id, &mut *canceled) {
            Ok(()) => (),
            Err(DocumentReadError::EngineBudget | DocumentReadError::ResourceDenied) => {
                return Ok(Self::refused(FragmentResolution::Limited));
            }
            Err(DocumentReadError::InvalidOutput) => {
                return Ok(Self::refused(FragmentResolution::Unsupported));
            }
            Err(error) => return Err(error),
        }
        Ok(Self { reader: Some(reader), refusal: FragmentResolution::Unresolved })
    }

    fn refused(refusal: FragmentResolution) -> Self { Self { reader: None, refusal } }

    pub fn resolve(&self, slug: &str, canceled: &mut impl FnMut() -> bool)
        -> Result<FragmentResolution, DocumentReadError> {
        if canceled() { return Err(DocumentReadError::Canceled); }
        let Some(reader) = &self.reader else { return Ok(self.refusal); };
        let mut selected = None;
        for heading in reader.headings() {
            if canceled() { return Err(DocumentReadError::Canceled); }
            if heading.slug != slug { continue; }
            // Canonical identities, including duplicate suffixes, belong to
            // upstream. A collision is explicit, never first-match navigation.
            if selected.is_some() { return Ok(FragmentResolution::Ambiguous); }
            selected = Some(heading);
        }
        let Some(heading) = selected else { return Ok(FragmentResolution::MissingHeading); };
        let range = reader.original_span(heading.source_span)?;
        Ok(if range.start() == range.end() { FragmentResolution::Unsupported }
            else { FragmentResolution::Resolved(range) })
    }
}
