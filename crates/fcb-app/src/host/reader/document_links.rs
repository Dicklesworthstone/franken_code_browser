#![forbid(unsafe_code)]

//! Prepared link metadata over the reader's immutable Markdown capture.
//! FrankenMarkdown supplies every occurrence and destination. The retained
//! inventory owns metadata only, not a borrowed pointer into its ReaderSession.
//! Link evidence covers the WHOLE document, never a guessed token or block.
//! Only fragment navigation within this capture is activated here. Local-file
//! and external destinations remain inert; a path label confers no authority.

use std::{fmt::Write, mem::size_of};
use fcb::document::links::{CapturedDocumentLinks, DocumentLinkError,
    MAX_DOCUMENT_LINK_DESTINATION_BYTES, MAX_DOCUMENT_LINKS};
use fcb::source::{RawPath, SourceError, path::decode_uri_component_bytes};
use super::{AppError, ByteLength, ByteRange, CaptureRequest, HostResponse, Output,
    QueryGeneration, ReaderSession, ReaderSessionError, ResourceAllocationId,
    ResourceBudget, ResourceLease, EXIT_OK, EXIT_PARTIAL, MAX_READER_DOCUMENT_PAGE, check};
use super::document::{encode_window, summary};

impl From<DocumentLinkError> for ReaderSessionError {
    fn from(error: DocumentLinkError) -> Self {
        match error { DocumentLinkError::Canceled => Self::Canceled, other => Self::DocumentLinks(other) }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum LinkKind { Heading, Start, Local, External, Unsupported }
impl LinkKind {
    const fn name(self) -> &'static str {
        match self {
            Self::Heading => "same-document-heading", Self::Start => "same-document-start",
            Self::Local => "local-reference", Self::External => "external", Self::Unsupported => "unsupported",
        }
    }
}
struct Link {
    id: u64,
    block_index: usize,
    evidence: ByteRange,
    destination: String,
    display: String,
    kind: LinkKind,
    fragment_slug: Option<String>,
}
pub(super) struct AcceptedDocumentLinks {
    generation: u64,
    request: CaptureRequest,
    links: Vec<Link>,
    seen: u64,
    diagnostics: usize,
    truncated: bool,
    _lease: ResourceLease,
}
impl AcceptedDocumentLinks {
    fn complete(&self) -> bool { !self.truncated && self.diagnostics == 0 }
    fn link(&self, id: u64) -> Result<&Link, ReaderSessionError> {
        // Oversized destinations can leave gaps in upstream occurrence IDs.
        // A display row index must never be substituted for an occurrence ID.
        self.links.binary_search_by_key(&id, |link| link.id).ok()
            .and_then(|position| self.links.get(position)).ok_or(ReaderSessionError::MissingDocumentLink)
    }
    fn retain(parsed: &CapturedDocumentLinks<'_>, request: CaptureRequest, generation: u64,
        budget: &ResourceBudget, allocation: ResourceAllocationId, canceled: &mut impl FnMut() -> bool)
        -> Result<Self, ReaderSessionError> {
        let rows = parsed.links();
        // Original destination, at most 4x escaped display, and decoded fragment
        // coexist. One temporary RawPath copy is separately covered below.
        let text_bytes = rows.iter().try_fold(0_usize, |total, row|
            row.destination().len().checked_mul(6).and_then(|n| total.checked_add(n)))
            .ok_or(AppError::Admission)?;
        let charge = rows.len().checked_mul(size_of::<Link>())
            .and_then(|n| n.checked_add(text_bytes))
            .and_then(|n| n.checked_add(size_of::<Self>() + MAX_DOCUMENT_LINK_DESTINATION_BYTES + 256))
            .ok_or(AppError::Admission)?;
        let lease = budget.try_reserve_managed(request.file().owner(), allocation, ByteLength::new(charge as u64))
            .map_err(|_| AppError::Admission)?;
        let mut links = Vec::new();
        links.try_reserve_exact(rows.len()).map_err(|_| AppError::Admission)?;
        if links.capacity() > rows.len() { return Err(AppError::Admission.into()); }
        for row in rows {
            check(canceled)?;
            let (kind, fragment_slug) = classify(row.destination())?;
            let mut display = text_buffer(row.destination().len().checked_mul(4).ok_or(AppError::Admission)?)?;
            let path = RawPath::from_bytes(row.destination().as_bytes());
            write!(display, "{}", path.display_escaped()).map_err(|_| AppError::Admission)?;
            links.push(Link { id: row.id(), block_index: row.block_index(),
                evidence: row.original_document_range(), destination: copy_text(row.destination())?,
                display, kind, fragment_slug });
        }
        check(canceled)?;
        Ok(Self { generation, request, links, seen: parsed.links_seen(), diagnostics: parsed.diagnostics(),
            truncated: parsed.truncated(), _lease: lease })
    }
}

impl ReaderSession {
    pub fn document_links_generation(&self) -> Option<u64> {
        self.document_links.as_ref().map(|links| links.generation)
    }

    /// Explicit bounded worker preparation. Rebuilding with another retained
    /// limit preserves parser occurrence IDs. Failure/cancellation preserves the
    /// previous inventory; successful document reflow/clear invalidates it.
    pub fn prepare_document_links(&mut self, generation: u64, max_links: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        self.document_snapshot(generation)?;
        if max_links > MAX_DOCUMENT_LINKS { return Err(DocumentLinkError::InvalidLimits.into()); }
        check(&mut canceled)?;
        let [parse_id, retain_id] = self.allocations()?;
        let request = *self.capture.request();
        let query = QueryGeneration::new(self.owner(), generation)
            .map_err(|_| ReaderSessionError::IdentityExhausted)?;
        let candidate = {
            let parsed = CapturedDocumentLinks::build(self.capture.bytes(), request, query,
                max_links, &self.budget, parse_id, &mut canceled)?;
            AcceptedDocumentLinks::retain(&parsed, request, generation, &self.budget, retain_id, &mut canceled)?
        };
        let mut out = self.output("document-links-prepare")?;
        summary(&mut out, self.document_snapshot(generation)?)?;
        encode_inventory(&mut out, &candidate)?;
        encode_page(&mut out, &candidate, 0, 64, &mut canceled)?;
        out.literal("}\n")?;
        let code = if candidate.complete() { EXIT_OK } else { EXIT_PARTIAL };
        let response = self.finish_output(out, code, &mut canceled)?;
        check(&mut canceled)?;
        self.document_links = Some(candidate);
        Ok(response)
    }

    /// Paging uses retained metadata only: no source read, Markdown parse, URI
    /// resolution, or new source pin. Source authority stays with this reader.
    pub fn document_links(&mut self, generation: u64, first: usize, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        page_limit(count)?; check(&mut canceled)?;
        self.link_snapshot(generation)?;
        let mut out = self.output("document-links")?;
        summary(&mut out, self.document_snapshot(generation)?)?;
        let links = self.link_snapshot(generation)?;
        encode_inventory(&mut out, links)?;
        encode_page(&mut out, links, first, count, &mut canceled)?;
        out.literal("}\n")?;
        let code = if links.complete() { EXIT_OK } else { EXIT_PARTIAL };
        self.finish_output(out, code, &mut canceled)
    }

    /// Safe adapter seam for a separately authorized root/retained-manifest
    /// service. The returned destination is parser-decoded text from this exact
    /// accepted occurrence. It is not a native path, grant, or permission to open.
    pub fn document_link_destination(&self, generation: u64, id: u64)
        -> Result<&str, ReaderSessionError> {
        Ok(&self.link_snapshot(generation)?.link(id)?.destination)
    }

    /// Activate only a same-capture fragment. A local/external reference returns
    /// an explicit inert result; neither the reader's label nor a URL is opened.
    /// Heading qualification precedes all source-range publication, including
    /// the missing-heading decision, so partial provenance is never absence.
    pub fn document_link(&mut self, generation: u64, id: u64, count: usize,
        mut canceled: impl FnMut() -> bool) -> Result<HostResponse, ReaderSessionError> {
        page_limit(count)?; check(&mut canceled)?;
        let kind = self.link_snapshot(generation)?.link(id)?.kind;
        if kind == LinkKind::Heading { self.qualify_document_source_actions(generation, &mut canceled)?; }
        let first = match kind {
            LinkKind::Heading => {
                let slug = self.link_snapshot(generation)?.link(id)?.fragment_slug.as_deref()
                    .ok_or(DocumentLinkError::InvalidEvidence)?;
                Some(self.document_snapshot(generation)?.window_at_heading(slug, count)?.first_index())
            }
            LinkKind::Start => Some(0),
            _ => None,
        };
        let mut out = self.output("document-link")?;
        let document = self.document_snapshot(generation)?;
        let links = self.link_snapshot(generation)?;
        let link = links.link(id)?;
        summary(&mut out, document)?; encode_inventory(&mut out, links)?;
        out.literal(",\"link\":")?; encode_link(&mut out, link)?;
        out.literal(",\"activated\":")?; out.boolean(first.is_some())?;
        if let Some(first) = first {
            if let Some(slug) = &link.fragment_slug {
                let heading = document.heading(slug)?;
                out.literal(",\"heading_slug\":")?; out.quoted(&heading.slug)?;
                out.literal(",\"heading_original_range\":")?; out.range(document.original_span(heading.source_span)?)?;
            }
            encode_window(&mut out, document, first, count, &mut canceled)?;
        }
        out.literal("}\n")?;
        self.finish_output(out, if first.is_some() { EXIT_OK } else { EXIT_PARTIAL }, &mut canceled)
    }

    fn link_snapshot(&self, generation: u64) -> Result<&AcceptedDocumentLinks, ReaderSessionError> {
        self.document_snapshot(generation)?;
        let links = self.document_links.as_ref().ok_or(ReaderSessionError::MissingDocumentLinks)?;
        if links.generation != generation { return Err(DocumentLinkError::StaleGeneration.into()); }
        if links.request != *self.capture.request() { return Err(DocumentLinkError::StaleSource.into()); }
        Ok(links)
    }
}

fn page_limit(count: usize) -> Result<(), ReaderSessionError> {
    if !(1..=MAX_READER_DOCUMENT_PAGE).contains(&count) { return Err(ReaderSessionError::InvalidLimits); }
    Ok(())
}
fn text_buffer(capacity: usize) -> Result<String, ReaderSessionError> {
    let mut text = String::new();
    text.try_reserve_exact(capacity).map_err(|_| AppError::Admission)?;
    if text.capacity() > capacity { return Err(AppError::Admission.into()); }
    Ok(text)
}
fn copy_text(value: &str) -> Result<String, ReaderSessionError> {
    let mut text = text_buffer(value.len())?; text.push_str(value); Ok(text)
}

fn classify(destination: &str) -> Result<(LinkKind, Option<String>), ReaderSessionError> {
    let value = destination.trim();
    if value.chars().any(char::is_control) { return Ok((LinkKind::Unsupported, None)); }
    if value.is_empty() || value == "#" { return Ok((LinkKind::Start, None)); }
    if let Some(encoded) = value.strip_prefix('#') {
        let decoded = match decode_uri_component_bytes(encoded, MAX_DOCUMENT_LINK_DESTINATION_BYTES) {
            Ok(bytes) => bytes,
            Err(SourceError::PayloadTooLarge) => return Err(AppError::Admission.into()),
            Err(_) => return Ok((LinkKind::Unsupported, None)),
        };
        let slug = match String::from_utf8(decoded) {
            Ok(slug) if !slug.is_empty() && !slug.chars().any(char::is_control) => slug,
            _ => return Ok((LinkKind::Unsupported, None)),
        };
        return Ok((LinkKind::Heading, Some(slug)));
    }
    // A query-only reference could request different source semantics. This
    // retained-document route does not silently apply or discard that query.
    if value.starts_with('?') { return Ok((LinkKind::Unsupported, None)); }
    // Classification only, matching the saved-relationship adapter. This is
    // not a scheme allowlist or permission for an OS/network handoff. Relative
    // path resolution belongs to resolve_document_destination at a root seam.
    let external = value.starts_with("//") || value.split_once(':').is_some_and(|(scheme, _)| {
        scheme.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
            && scheme.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'+' | b'-' | b'.'))
    });
    Ok((if external { LinkKind::External } else { LinkKind::Local }, None))
}

fn encode_inventory(out: &mut Output, links: &AcceptedDocumentLinks) -> Result<(), ReaderSessionError> {
    out.literal(",\"links_ready\":true,\"links_complete\":")?; out.boolean(links.complete())?;
    out.literal(",\"links_seen\":")?; out.integer(links.seen)?;
    out.literal(",\"retained_links\":")?; out.integer(links.links.len() as u64)?;
    out.literal(",\"links_truncated\":")?; out.boolean(links.truncated)?;
    out.literal(",\"parser_diagnostics\":")?; out.integer(links.diagnostics as u64)?;
    out.literal(",\"evidence_precision\":\"whole-captured-markdown-document\"")?;
    Ok(())
}
fn encode_page(out: &mut Output, links: &AcceptedDocumentLinks, first: usize, count: usize,
    canceled: &mut impl FnMut() -> bool) -> Result<(), ReaderSessionError> {
    if first > links.links.len() { return Err(ReaderSessionError::InvalidRange); }
    let end = first.saturating_add(count).min(links.links.len());
    out.literal(",\"first_link\":")?; out.integer(first as u64)?;
    out.literal(",\"links\":[")?;
    for (index, link) in links.links[first..end].iter().enumerate() {
        check(canceled)?;
        if index > 0 { out.literal(",")?; }
        encode_link(out, link)?;
    }
    out.literal("],\"next_link\":")?;
    if end < links.links.len() { out.integer(end as u64)?; } else { out.literal("null")?; }
    Ok(())
}
fn encode_link(out: &mut Output, link: &Link) -> Result<(), ReaderSessionError> {
    out.literal("{\"link_id\":")?; out.integer(link.id)?;
    out.literal(",\"block_index\":")?; out.integer(link.block_index as u64)?;
    out.literal(",\"evidence_range\":")?; out.range(link.evidence)?;
    out.literal(",\"destination\":")?; out.quoted(&link.destination)?;
    out.literal(",\"destination_display\":")?; out.quoted(&link.display)?;
    out.literal(",\"kind\":")?; out.quoted(link.kind.name())?;
    out.literal(",\"fragment_slug\":")?;
    if let Some(slug) = &link.fragment_slug { out.quoted(slug)?; } else { out.literal("null")?; }
    out.literal("}")?;
    Ok(())
}
