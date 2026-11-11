#![forbid(unsafe_code)]

//! Shared fragment qualification over complete, immutable captures. Upstream
//! Markdown owns parsing, canonical heading identities and source spans. This
//! adapter decodes a requested URI identity once, qualifies the whole heading
//! catalog, and returns original ranges without publishing a reader generation.
//! Non-Markdown line anchors use the source adapter's physical-line scanner.

use fcb::document::links::{MAX_DOCUMENT_LINK_DESTINATION_BYTES, MAX_DOCUMENT_LINK_SOURCE_BYTES};
use fcb::document::reader::{DocumentReadError, DocumentReadOptions, DocumentReader};
use fcb::source::{CompleteCapture, SourceError, detect_encoding, path::decode_uri_component_bytes,
    line_index::{LineNumber, LineWindowScanner, LineWindowStatus}};
use fcb_core::{ByteOffset, ByteRange, DocumentGeneration, DocumentId, ResourceAllocationId, ResourceBudget};
use crate::AppError;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum FragmentResolution {
    None, DocumentStart, Resolved(ByteRange), ResolvedLines(ByteRange), MissingHeading, MissingLine, Ambiguous,
    Limited, Unsupported, Invalid, Unresolved,
}
impl FragmentResolution {
    pub const fn name(self) -> &'static str {
        match self {
            Self::None => "none", Self::DocumentStart => "document-start",
            Self::Resolved(_) => "resolved", Self::MissingHeading => "missing-heading",
            Self::ResolvedLines(_) => "resolved-lines", Self::MissingLine => "missing-line",
            Self::Ambiguous => "ambiguous", Self::Limited => "limited",
            Self::Unsupported => "unsupported", Self::Invalid => "invalid", Self::Unresolved => "unresolved",
        }
    }
    pub const fn range(self) -> Option<ByteRange> {
        match self { Self::Resolved(range) | Self::ResolvedLines(range) => Some(range), _ => None }
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

/// An explicit source-line identity, distinct from an upstream Markdown slug.
/// Decode the URI before parsing; accept only canonical, one-based ASCII forms.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SourceLineFragment { pub first: LineNumber, pub last: LineNumber }
impl SourceLineFragment {
    pub fn parse(slug: &str) -> Result<Self, FragmentResolution> {
        let Some(body) = slug.strip_prefix('L') else { return Err(FragmentResolution::Unsupported); };
        // Ordinary named fragments such as "License" are not line requests.
        if body.as_bytes().first().is_some_and(|byte| byte.is_ascii_alphabetic()) {
            return Err(FragmentResolution::Unsupported);
        }
        let (first, last) = match body.split_once("-L") {
            Some((first, last)) => (line_number(first)?, line_number(last)?),
            None => { let line = line_number(body)?; (line, line) }
        };
        if last < first { return Err(FragmentResolution::Invalid); }
        Ok(Self { first, last })
    }
}

fn line_number(text: &str) -> Result<LineNumber, FragmentResolution> {
    if text.is_empty() || text.starts_with('0') { return Err(FragmentResolution::Invalid); }
    let mut value = 0_u64;
    for byte in text.bytes() {
        if !byte.is_ascii_digit() { return Err(FragmentResolution::Invalid); }
        value = value.checked_mul(10).and_then(|n| n.checked_add(u64::from(byte - b'0')))
            .ok_or(FragmentResolution::Invalid)?;
    }
    LineNumber::new(value).map_err(|_| FragmentResolution::Invalid)
}

pub(crate) const MAX_SOURCE_LINE_FRAGMENT_BYTES: usize = super::reader::MAX_READER_SOURCE_BYTES;
const LINE_FRAGMENT_STEP: usize = 64 * 1024;

/// Qualify monotonically ordered endpoints in one immutable source. Retain no
/// per-line table: a pending cursor preserves CRLF/UTF-16 carry across requests.
/// Each interval is examined at most twice, including bounded CR lookahead.
pub(crate) struct QualifiedSourceLines<'capture> {
    bytes: &'capture [u8], limit: usize, progress: Option<LineWindowScanner>,
    previous: Option<(LineNumber, FragmentResolution)>, refusal: Option<FragmentResolution>,
}
impl<'capture> QualifiedSourceLines<'capture> {
    pub fn prepare(bytes: &'capture [u8], max_scan_bytes: usize) -> Self {
        let limit = bytes.len().min(max_scan_bytes).min(MAX_SOURCE_LINE_FRAGMENT_BYTES);
        let encoding = detect_encoding(&bytes[..bytes.len().min(3)]);
        let progress = LineWindowScanner::new(LineNumber::new(u64::MAX).expect("nonzero line"), 1, encoding).ok();
        let refusal = if bytes.len() > MAX_SOURCE_LINE_FRAGMENT_BYTES { Some(FragmentResolution::Limited) }
            else if progress.is_none() { Some(FragmentResolution::Unsupported) } else { None };
        Self { bytes, limit, progress, previous: None, refusal }
    }

    pub fn resolve(&mut self, fragment: SourceLineFragment, canceled: &mut impl FnMut() -> bool)
        -> Result<FragmentResolution, AppError> {
        let first = match self.line(fragment.first, canceled)? {
            FragmentResolution::ResolvedLines(range) => range,
            refusal => return Ok(refusal),
        };
        let last = match self.line(fragment.last, canceled)? {
            FragmentResolution::ResolvedLines(range) => range,
            refusal => return Ok(refusal),
        };
        Ok(FragmentResolution::ResolvedLines(ByteRange::new(first.start(), last.end())
            .map_err(|_| AppError::InvalidRange)?))
    }

    pub fn line(&mut self, line: LineNumber, canceled: &mut impl FnMut() -> bool)
        -> Result<FragmentResolution, AppError> {
        if canceled() { return Err(AppError::Canceled); }
        if let Some((previous, resolution)) = self.previous {
            if line < previous { return Err(AppError::InvalidRange); }
            if line == previous { return Ok(resolution); }
        }
        if let Some(refusal) = self.refusal {
            self.previous = Some((line, refusal));
            return Ok(refusal);
        }
        let progress = self.progress.as_mut().ok_or(AppError::InvalidRange)?;
        let mut selected = progress.retarget(line, 1).map_err(|_| AppError::InvalidRange)?;
        while !selected.is_finished() && selected.bytes_scanned() < self.limit as u64 {
            if canceled() { return Err(AppError::Canceled); }
            let at = selected.bytes_scanned() as usize;
            selected.step(ByteOffset::new(at as u64), &self.bytes[at..self.limit], LINE_FRAGMENT_STEP)
                .map_err(|_| AppError::InvalidRange)?;
        }
        if !selected.is_finished() && selected.bytes_scanned() == self.bytes.len() as u64 {
            selected.finish().map_err(|_| AppError::InvalidRange)?;
        }
        let resolution = match selected.status() {
            LineWindowStatus::Resolved { last, range, .. } if last == line => {
                // Do not retain the selected scanner's bare-CR lookahead: it
                // can already be inside the next requested physical line.
                let end = range.end().get() as usize;
                while progress.bytes_scanned() < end as u64 {
                    if canceled() { return Err(AppError::Canceled); }
                    let at = progress.bytes_scanned() as usize;
                    progress.step(ByteOffset::new(at as u64), &self.bytes[at..end], LINE_FRAGMENT_STEP)
                        .map_err(|_| AppError::InvalidRange)?;
                }
                FragmentResolution::ResolvedLines(range)
            }
            LineWindowStatus::Resolved { .. } | LineWindowStatus::Missing => FragmentResolution::MissingLine,
            LineWindowStatus::Pending => FragmentResolution::Limited,
        };
        if canceled() { return Err(AppError::Canceled); }
        if !matches!(resolution, FragmentResolution::ResolvedLines(_)) { self.refusal = Some(resolution); }
        self.previous = Some((line, resolution));
        Ok(resolution)
    }
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

#[cfg(test)]
mod tests {
    use super::*;

    fn resolve(bytes: &[u8], slug: &str, limit: usize) -> FragmentResolution {
        QualifiedSourceLines::prepare(bytes, limit)
            .resolve(SourceLineFragment::parse(slug).unwrap(), &mut || false).unwrap()
    }
    fn lines(start: u64, end: u64) -> FragmentResolution {
        FragmentResolution::ResolvedLines(ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).unwrap())
    }

    #[test]
    fn canonical_source_lines_do_not_reinterpret_named_fragments_or_overflow() {
        for (slug, first, last) in [("L1", 1, 1), ("L42-L57", 42, 57), ("L2-L2", 2, 2),
            ("L18446744073709551615", u64::MAX, u64::MAX)] {
            let fragment = SourceLineFragment::parse(slug).unwrap();
            assert_eq!((fragment.first.get(), fragment.last.get()), (first, last));
        }
        for slug in ["L", "L0", "L01", "L+2", "L-1", "L2-L1", "L1-L0", "L1-L02",
            "L1-L2-L3", "L1-2", "L1-l2", "L2extra", "L18446744073709551616"] {
            assert_eq!(SourceLineFragment::parse(slug), Err(FragmentResolution::Invalid), "{slug}");
        }
        for slug in ["entry", "License", "l2", "%4C2"] {
            assert_eq!(SourceLineFragment::parse(slug), Err(FragmentResolution::Unsupported), "{slug}");
        }
    }

    #[test]
    fn ordered_endpoints_preserve_bom_crlf_bare_cr_and_utf16_original_bytes() {
        for encoding in 0..3 {
            let text = "\u{feff}a\r\n\u{010a}🦀\rb\nlast";
            let encode = |text: &str| -> Vec<u8> { match encoding {
                0 => text.as_bytes().to_vec(),
                1 => text.encode_utf16().flat_map(u16::to_le_bytes).collect(),
                _ => text.encode_utf16().flat_map(u16::to_be_bytes).collect(),
            } };
            let bytes = encode(text);
            let physical = ["\u{feff}a\r\n", "\u{010a}🦀\r", "b\n", "last"];
            let mut qualified = QualifiedSourceLines::prepare(&bytes, bytes.len());
            let mut start = 0;
            for (index, line) in physical.iter().enumerate() {
                let end = start + encode(line).len() as u64;
                let number = LineNumber::new(index as u64 + 1).unwrap();
                assert_eq!(qualified.line(number, &mut || false).unwrap(), lines(start, end));
                // Overlapping ranges share endpoints without rescanning them.
                assert_eq!(qualified.line(number, &mut || false).unwrap(), lines(start, end));
                start = end;
            }
            assert_eq!(qualified.line(LineNumber::new(5).unwrap(), &mut || false).unwrap(), FragmentResolution::MissingLine);
            assert_eq!(resolve(&bytes, "L2-L3", bytes.len()),
                lines(encode(physical[0]).len() as u64, (bytes.len() - encode("last").len()) as u64));
        }
    }

    #[test]
    fn missing_final_endpoints_and_scan_limits_never_publish_shortened_ranges() {
        for bytes in [b"".as_slice(), b"one\n", b"one\r", b"one\r\n", b"one"] {
            assert_eq!(resolve(bytes, "L2", bytes.len()), FragmentResolution::MissingLine);
            assert_eq!(resolve(bytes, "L1-L2", bytes.len()), FragmentResolution::MissingLine);
        }
        assert_eq!(resolve(b"a\nb\nc\n", "L1", 2), lines(0, 2));
        assert_eq!(resolve(b"a\nb\nc\n", "L2", 3), FragmentResolution::Limited);
        assert_eq!(resolve(b"a\nb\nc\n", "L1-L2", 3), FragmentResolution::Limited);
        assert_eq!(resolve(b"a\rb", "L1", 2), FragmentResolution::Limited); // CR needs lookahead or real EOF.
        assert_eq!(resolve(b"\xff\xfea\0\n\0\xff", "L2", 7), lines(6, 7));
    }

    #[test]
    fn endpoint_work_is_cancelable_between_bounded_chunks() {
        let bytes = vec![b'x'; LINE_FRAGMENT_STEP * 3];
        let mut qualified = QualifiedSourceLines::prepare(&bytes, bytes.len());
        let mut polls = 0;
        assert_eq!(qualified.resolve(SourceLineFragment::parse("L1").unwrap(), &mut || {
            polls += 1; polls == 3
        }), Err(AppError::Canceled));
        assert_eq!(polls, 3);
    }
}
