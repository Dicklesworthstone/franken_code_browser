#![forbid(unsafe_code)]

//! Optional logical source context from the same capture as a search hit.
//! The extent reader owns decoding, scalar/CRLF boundaries and source maps.
//! Context is prepared only for returned rows under their response allowance;
//! omission cannot change hit identity, query coverage or exact activation.

use std::ops::Range;
use fcb::{BrowserSession, ByteOffset, QueryGeneration};
use fcb_core::DecodedUtf8Range;
use fcb::search::{ExtentText, ExtentViewError, ObservedExtent};
use fcb::source::detect_encoding;
use super::*;

pub const MAX_ATLAS_EXCERPT_SOURCE_BYTES: usize = 8192;
pub const MAX_ATLAS_EXCERPT_TEXT_BYTES: usize = 8192;
const CONTEXT_BYTES: u64 = 160;
const MATCH_PADDING_BYTES: u64 = 4;
const DECODE_HALO_BYTES: u64 = 8;
// Covers the fixed field names, four full-width decimal strings and booleans.
// A JSON text byte expands by at most six bytes under Output::quoted.
const FIELD_OVERHEAD_BYTES: usize = 512;
const NULL_FIELD: &str = ",\"excerpt\":null";

pub(super) struct Excerpts<'budget> {
    budget: &'budget ResourceBudget,
    allocations: Range<u64>,
    remaining: usize,
}
impl<'budget> Excerpts<'budget> {
    pub(super) fn new(budget: &'budget ResourceBudget, allocations: Range<u64>) -> Self {
        Self { budget, allocations, remaining: 0 }
    }

    /// Allowance beyond a complete metadata-only response, not merely the
    /// space before its still-unencoded rows, diagnostics and closing fields.
    pub(super) fn admit_extra(&mut self, bytes: usize) { self.remaining = bytes; }

    pub(super) fn field(&mut self, out: &mut Output, capture: &CompleteCapture,
        generation: u64, selected: ByteRange, expected_text: Option<&str>,
        canceled: &mut impl FnMut() -> bool) -> Result<(), AtlasSearchError> {
        check(canceled)?;
        if self.remaining < NULL_FIELD.len() { return Ok(()); }
        let prepared = if self.remaining >= FIELD_OVERHEAD_BYTES {
            let first = self.allocations.next().ok_or(AtlasSearchError::IdentityExhausted)?;
            let second = self.allocations.next().ok_or(AtlasSearchError::IdentityExhausted)?;
            let allocations = [first, second].map(|id| ResourceAllocationId::new(id)
                .map_err(|_| AtlasSearchError::IdentityExhausted));
            match prepare(capture, generation, selected, expected_text, self.budget,
                [allocations[0]?, allocations[1]?], canceled) {
                Ok(prepared) => prepared,
                Err(ExtentViewError::Canceled) => return Err(AtlasSearchError::Canceled),
                Err(_) => None,
            }
        } else { None };
        check(canceled)?;
        let before = out.checkpoint();
        if let Some(prepared) = prepared.filter(|item| item.encoded_bound() <= self.remaining) {
            prepared.encode(out)?;
        } else {
            out.literal(NULL_FIELD)?;
        }
        self.remaining = self.remaining.checked_sub(out.checkpoint() - before)
            .ok_or(AppError::Admission)?;
        Ok(())
    }
}

struct Prepared {
    text: ExtentText,
    selected: DecodedUtf8Range,
    clipped_start: bool,
    clipped_end: bool,
}
impl Prepared {
    fn encoded_bound(&self) -> usize {
        // text length was checked before publication, so this cannot overflow.
        FIELD_OVERHEAD_BYTES + self.text.text().len() * 6
    }
    fn encode(&self, out: &mut Output) -> Result<(), OutputError> {
        out.literal(",\"excerpt\":{\"text\":")?;
        out.quoted(self.text.text())?;
        out.literal(",\"original_range\":")?; out.range(self.text.range())?;
        out.literal(",\"match_utf8_range\":{\"start\":")?;
        out.integer(self.selected.start().get())?;
        out.literal(",\"end\":")?; out.integer(self.selected.end().get())?;
        out.literal("},\"has_replacements\":")?; out.boolean(self.text.has_replacements())?;
        out.literal(",\"clipped_start\":")?; out.boolean(self.clipped_start)?;
        out.literal(",\"clipped_end\":")?; out.boolean(self.clipped_end)?;
        out.literal("}")
    }
}

fn prepare(capture: &CompleteCapture, generation: u64, selected: ByteRange,
    expected_text: Option<&str>, budget: &ResourceBudget, allocations: [ResourceAllocationId; 2],
    canceled: &mut impl FnMut() -> bool) -> Result<Option<Prepared>, ExtentViewError> {
    if canceled() { return Err(ExtentViewError::Canceled); }
    let length = capture.bytes().len() as u64;
    if selected.is_empty() || selected.end().get() > length { return Ok(None); }
    let start = selected.start().get().saturating_sub(CONTEXT_BYTES + MATCH_PADDING_BYTES);
    let end = selected.end().get().saturating_add(CONTEXT_BYTES + MATCH_PADDING_BYTES).min(length);
    let visible = range(start, end)?;
    if visible.len().get() > MAX_ATLAS_EXCERPT_SOURCE_BYTES as u64 { return Ok(None); }
    let captured = range(start.saturating_sub(DECODE_HALO_BYTES),
        end.saturating_add(DECODE_HALO_BYTES).min(length))?;
    let (first, last) = captured.as_usize_bounds().map_err(|_| ExtentViewError::InvalidRange)?;
    let raw = capture.bytes().get(first..last).ok_or(ExtentViewError::InvalidRange)?;
    let request = (*capture.request()).with_range(captured).map_err(|_| ExtentViewError::InvalidRange)?;
    let extent = ObservedExtent::from_bytes(request, ByteLength::new(length), raw, budget, allocations[0])?;
    let owner = capture.request().file().owner();
    let view = BrowserSession::new(owner).open_extent(extent)?;
    let encoding = detect_encoding(capture.bytes());
    let generation = QueryGeneration::new(owner, generation).map_err(|_| ExtentViewError::InvalidRange)?;
    let text = view.decode(visible, encoding, generation, budget, allocations[1], &mut *canceled)?;
    if text.text().len() > MAX_ATLAS_EXCERPT_TEXT_BYTES
        || text.range().len().get() > MAX_ATLAS_EXCERPT_SOURCE_BYTES as u64 { return Ok(None); }
    let mapped = text.source_to_text(selected)?;
    if mapped.start() >= mapped.end() { return Ok(None); }
    let resolved = text.text_selection(mapped)?;
    let (first, last) = selected.as_usize_bounds().map_err(|_| ExtentViewError::InvalidRange)?;
    if resolved.original_range != selected || resolved.original_bytes != &capture.bytes()[first..last]
        || resolved.contains_replacements
        || expected_text.is_some_and(|expected| expected.as_bytes() != resolved.text.as_bytes()) {
        return Ok(None);
    }
    if canceled() { return Err(ExtentViewError::Canceled); }
    let clipped_start = text.range().start().get() > encoding.bom_bytes_len();
    let clipped_end = text.range().end().get() < length;
    Ok(Some(Prepared { text, selected: mapped, clipped_start, clipped_end }))
}

fn range(start: u64, end: u64) -> Result<ByteRange, ExtentViewError> {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end))
        .map_err(|_| ExtentViewError::InvalidRange)
}

#[cfg(test)]
mod tests {
    use super::*;
    use fcb::search::CaptureRequest;
    use std::sync::Arc;

    fn owner() -> ArenaOwnerId { ArenaOwnerId::new(40199).unwrap() }
    fn allocation(value: u64) -> ResourceAllocationId { ResourceAllocationId::new(value).unwrap() }
    fn budget(bytes: u64) -> ResourceBudget {
        ResourceBudget::new(owner(), ByteLength::new(bytes)).unwrap()
    }
    fn capture(bytes: &[u8]) -> CompleteCapture {
        let request = CaptureRequest::new(FileId::new(owner(), 1).unwrap(),
            SourceRevision::new(owner(), 1).unwrap()).unwrap();
        CompleteCapture::new(request, ByteLength::new(bytes.len() as u64), Arc::from(bytes)).unwrap()
    }
    fn utf16(text: &str, little: bool) -> Vec<u8> {
        "\u{feff}".encode_utf16().chain(text.encode_utf16())
            .flat_map(|unit| if little { unit.to_le_bytes() } else { unit.to_be_bytes() }).collect()
    }

    #[test]
    fn replacement_context_is_labeled_without_changing_an_exact_valid_match() {
        let mut little = vec![0xff, 0xfe, 0x00, 0xd8];
        little.extend(" needle\n".encode_utf16().flat_map(u16::to_le_bytes));
        let mut big = vec![0xfe, 0xff, 0xd8, 0x00];
        big.extend(" needle\n".encode_utf16().flat_map(u16::to_be_bytes));
        let budget = budget(32 * 1024 * 1024);
        for (bytes, selected) in [
            (b"bad\xff needle\n".to_vec(), range(5, 11).unwrap()),
            (little, range(6, 18).unwrap()),
            (big, range(6, 18).unwrap()),
        ] {
            let source = capture(&bytes);
            let prepared = prepare(&source, 1, selected, Some("needle"), &budget,
                [allocation(1), allocation(2)], &mut || false).unwrap().unwrap();
            assert!(prepared.text.has_replacements());
            let resolved = prepared.text.text_selection(prepared.selected).unwrap();
            assert_eq!(resolved.text, "needle");
            assert_eq!(resolved.original_range, selected);
            let (start, end) = selected.as_usize_bounds().unwrap();
            assert_eq!(resolved.original_bytes, &bytes[start..end]);
            assert!(!resolved.contains_replacements);
            assert!(budget.accounting().reserved().get() > 0);
            drop(prepared);
            assert_eq!(budget.accounting().reserved().get(), 0);
        }
    }

    #[test]
    fn unrepresentable_or_oversized_context_is_optional_and_releases_scratch() {
        let decoded_large = utf16(&"中".repeat(3000), true);
        let cases = [
            (b"needle".to_vec(), range(0, 6).unwrap(), Some("other")),
            ("éneedle".as_bytes().to_vec(), range(1, 3).unwrap(), None),
            (vec![0xff, b'n'], range(0, 1).unwrap(), None),
            (vec![b'x'; 8193], range(0, 8193).unwrap(), None),
            (decoded_large, range(2, 6002).unwrap(), None),
            (b"needle".to_vec(), range(6, 7).unwrap(), None),
            (b"needle".to_vec(), range(3, 3).unwrap(), None),
        ];
        let decode_budget = budget(32 * 1024 * 1024);
        let output_budget = budget(2 * 1024 * 1024);
        for (index, (bytes, selected, expected)) in cases.into_iter().enumerate() {
            let source = capture(&bytes);
            let mut out = Output::new(owner(), 65536, &output_budget, allocation(500 + index as u64)).unwrap();
            out.literal("{\"hit_id\":\"1\"").unwrap();
            let first = 1000 + index as u64 * 2;
            let mut excerpts = Excerpts::new(&decode_budget, first..first + 2);
            excerpts.admit_extra(65536 - out.checkpoint() - 2);
            excerpts.field(&mut out, &source, 1, selected, expected, &mut || false).unwrap();
            out.literal("}\n").unwrap();
            assert_eq!(out.as_bytes(), b"{\"hit_id\":\"1\",\"excerpt\":null}\n");
            assert_eq!(decode_budget.accounting().reserved().get(), 0);
        }
    }

    #[test]
    fn admission_refusal_omits_preview_without_spending_capture_authority() {
        let source = capture(b"before needle after");
        let decode_budget = budget(1);
        let output_budget = budget(2 * 1024 * 1024);
        let mut out = Output::new(owner(), 4096, &output_budget, allocation(500)).unwrap();
        out.literal("{\"hit_id\":\"1\"").unwrap();
        let mut excerpts = Excerpts::new(&decode_budget, 1..3);
        excerpts.admit_extra(4000);
        excerpts.field(&mut out, &source, 1, range(7, 13).unwrap(), Some("needle"), &mut || false).unwrap();
        out.literal("}\n").unwrap();
        assert_eq!(out.as_bytes(), b"{\"hit_id\":\"1\",\"excerpt\":null}\n");
        assert_eq!(decode_budget.accounting().reserved().get(), 0);
        assert_eq!(source.bytes(), b"before needle after");
    }

    #[test]
    fn cancellation_at_each_decode_checkpoint_publishes_no_partial_field_or_lease() {
        let source = capture(b"before needle after");
        let decode_budget = budget(32 * 1024 * 1024);
        let output_budget = budget(2 * 1024 * 1024);
        let selected = range(7, 13).unwrap();
        let mut out = Output::new(owner(), 4096, &output_budget, allocation(500)).unwrap();
        let mut polls = 0usize;
        let mut excerpts = Excerpts::new(&decode_budget, 1..3);
        excerpts.admit_extra(4000);
        excerpts.field(&mut out, &source, 1, selected, Some("needle"), &mut || {
            polls += 1; false
        }).unwrap();
        assert!(polls > 3);
        assert_eq!(decode_budget.accounting().reserved().get(), 0);
        for stop in 1..=polls {
            out.clear();
            out.literal("{\"hit_id\":\"1\"").unwrap();
            let mut observed = 0;
            let first = 10 + stop as u64 * 2;
            let mut excerpts = Excerpts::new(&decode_budget, first..first + 2);
            excerpts.admit_extra(4000);
            let result = excerpts.field(&mut out, &source, 1, selected, Some("needle"), &mut || {
                observed += 1; observed == stop
            });
            assert_eq!(result, Err(AtlasSearchError::Canceled), "stop={stop}");
            assert_eq!(out.as_bytes(), b"{\"hit_id\":\"1\"");
            assert_eq!(decode_budget.accounting().reserved().get(), 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn complete_mandatory_page_survives_zero_null_only_and_full_excerpt_allowances() {
        use crate::host::atlas_session::AtlasSessionOptions;
        use std::{fs, sync::atomic::{AtomicU64, Ordering}};
        static NEXT: AtomicU64 = AtomicU64::new(1);
        let root = std::env::temp_dir().join(format!("fcb-excerpt-output-{}-{}-{}",
            std::process::id(), NEXT.fetch_add(1, Ordering::Relaxed),
            std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos()));
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("source.rs"), b"before needle after").unwrap();
        let atlas = AtlasSession::open(owner(), &root, AtlasSessionOptions::default(), || false).unwrap();
        let mut search = RetainedAtlasSearch::new(&atlas).unwrap();
        search.search(&atlas, 1, "needle", AtlasSearchOptions {
            max_matches: 8, max_files: 1, max_file_bytes: 4096, max_source_bytes: 4096,
        }, || false).unwrap();
        let snapshot = search.snapshot(1).unwrap();
        let budget = budget(32 * 1024 * 1024);
        let mut baseline = Output::new(owner(), 65536, &budget, allocation(500)).unwrap();
        baseline.literal("{\"fixture\":true").unwrap();
        encode_page_body(&mut baseline, &atlas, snapshot, 0, 1, None, &mut || false).unwrap();
        let expected = std::str::from_utf8(baseline.as_bytes()).unwrap().to_owned();
        let required = baseline.checkpoint();
        drop(baseline);
        for (index, extra) in [0, NULL_FIELD.len() - 1, NULL_FIELD.len(), 8192].into_iter().enumerate() {
            let mut out = Output::new(owner(), required + extra, &budget, allocation(501 + index as u64)).unwrap();
            out.literal("{\"fixture\":true").unwrap();
            let first = 1000 + index as u64 * 2;
            let mut excerpts = Excerpts::new(&budget, first..first + 2);
            encode_page(&mut out, &atlas, snapshot, 0, 1, &mut excerpts, &mut || false).unwrap();
            let text = std::str::from_utf8(out.as_bytes()).unwrap();
            assert!(text.len() <= required + extra);
            assert!(text.contains("\"hit_id\":\"1\""));
            assert!(text.contains("\"next_offset\":null"));
            assert!(text.ends_with("],\"diagnostics_truncated\":false}\n"));
            if extra < NULL_FIELD.len() {
                assert_eq!(text, expected);
            } else if extra == NULL_FIELD.len() {
                assert_eq!(text.replace(NULL_FIELD, ""), expected);
                assert!(text.contains(NULL_FIELD));
            } else {
                assert!(text.contains("\"excerpt\":{\"text\":\"before needle after\""));
                assert!(text.contains("\"match_utf8_range\":{\"start\":\"7\",\"end\":\"13\"}"));
            }
        }
        assert_eq!(budget.accounting().reserved().get(), 0);
        assert_eq!(search.hit(&atlas, 1, 1).unwrap().original_range, range(7, 13).unwrap());
        // The fixture remains available for independent replay.
    }
}
