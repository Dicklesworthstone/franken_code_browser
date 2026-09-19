#![forbid(unsafe_code)]

//! Constant-space, resumable line-window location over one observed byte stream.
//!
//! Unlike the retained index, this cursor stores neither source bytes nor an
//! entry per line. Each step examines at most its explicit byte budget. The
//! caller owns I/O, cancellation, source identity, and retention of the selected
//! bytes. Offsets describe the supplied sequence, not an atomic live-file view.
//! CR, LF, and CRLF terminate lines; a final terminator creates no phantom line.
//! UTF-16 is examined as code units, never as ASCII bytes inside those units.

use fcb_core::{ByteOffset, ByteRange};
use crate::DetectedEncoding;
use super::LineNumber;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineWindowError {
    InvalidWindow,
    UnsupportedEncoding,
    NoncontiguousInput,
    CounterOverflow,
}

impl LineWindowError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::InvalidWindow => "LINE_WINDOW_INVALID",
            Self::UnsupportedEncoding => "LINE_WINDOW_ENCODING_UNSUPPORTED",
            Self::NoncontiguousInput => "LINE_WINDOW_NONCONTIGUOUS",
            Self::CounterOverflow => "LINE_WINDOW_OVERFLOW",
        }
    }
}

impl std::fmt::Display for LineWindowError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for LineWindowError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LineWindowStatus {
    Pending,
    /// The returned lines include their original terminators. `last` can be
    /// smaller than the requested last line only when real EOF was supplied.
    Resolved { first: LineNumber, last: LineNumber, range: ByteRange },
    /// Real EOF established that the requested first line does not exist.
    Missing,
}

/// An allocation-free cursor. Construction and stepping perform no I/O.
///
/// Supply bytes from offset zero in order, using the declared decoder. A BOM
/// remains part of the original first-line range; decoding/stripping it belongs
/// to the source view. Malformed UTF-8 and a trailing UTF-16 byte are retained as
/// source content, not silently discarded or interpreted as line separators.
#[derive(Clone, Debug)]
pub struct LineWindowScanner {
    first: LineNumber,
    last: LineNumber,
    encoding: DetectedEncoding,
    offset: u64,
    line: u64,
    active_line: bool,
    pending_cr: bool,
    half_unit: Option<u8>,
    start: Option<u64>,
    end: Option<u64>,
    finished: bool,
    eof: bool,
}

impl LineWindowScanner {
    pub fn new(first: LineNumber, count: u64, encoding: DetectedEncoding) -> Result<Self, LineWindowError> {
        let last = count.checked_sub(1).and_then(|delta| first.get().checked_add(delta))
            .ok_or(LineWindowError::InvalidWindow)?;
        if encoding == DetectedEncoding::Unsupported { return Err(LineWindowError::UnsupportedEncoding); }
        Ok(Self {
            first, last: LineNumber::new(last).map_err(|_| LineWindowError::InvalidWindow)?, encoding,
            offset: 0, line: 0, active_line: false, pending_cr: false, half_unit: None,
            start: None, end: None, finished: false, eof: false,
        })
    }

    pub const fn bytes_scanned(&self) -> u64 { self.offset }
    pub const fn lines_seen(&self) -> u64 { self.line }
    pub const fn is_finished(&self) -> bool { self.finished }
    pub const fn reached_eof(&self) -> bool { self.eof }

    /// Reuse a pending cursor for a strictly later line in the SAME immutable
    /// byte sequence and encoding. Decoder carry and a pending CR survive.
    /// A current/earlier line cannot be recovered without its start offset;
    /// callers must choose an earlier checkpoint or restart from zero.
    pub fn retarget(&self, first: LineNumber, count: u64) -> Result<Self, LineWindowError> {
        if self.finished || self.eof || first.get() <= self.line {
            return Err(LineWindowError::InvalidWindow);
        }
        let mut next = Self::new(first, count, self.encoding)?;
        next.offset = self.offset;
        next.line = self.line;
        next.active_line = self.active_line;
        next.pending_cr = self.pending_cr;
        next.half_unit = self.half_unit;
        Ok(next)
    }

    /// With a split UTF-16 unit this can precede this step's input by one byte.
    /// A streaming consumer retains one carry byte until this offset is known.
    pub fn selected_start(&self) -> Option<ByteOffset> { self.start.map(ByteOffset::new) }

    pub fn status(&self) -> LineWindowStatus {
        if !self.finished { return LineWindowStatus::Pending; }
        match (self.start, self.end) {
            (Some(start), Some(end)) => LineWindowStatus::Resolved {
                first: self.first,
                last: LineNumber::new(self.line).expect("a selected line is nonzero"),
                range: ByteRange::new(ByteOffset::new(start), ByteOffset::new(end))
                    .expect("selected offsets are monotonic"),
            },
            _ => LineWindowStatus::Missing,
        }
    }

    /// Consume no more than `byte_budget` bytes, returning the actual count.
    /// Zero is a no-op. A split code unit/CRLF is pending, not EOF. At most one
    /// code unit beyond the selected terminator is examined to distinguish bare
    /// CR from CRLF; it is not included in the returned range.
    pub fn step(&mut self, offset: ByteOffset, bytes: &[u8], byte_budget: usize) -> Result<usize, LineWindowError> {
        if offset.get() != self.offset { return Err(LineWindowError::NoncontiguousInput); }
        if self.finished { return Ok(0); }
        let count = bytes.len().min(byte_budget);
        let count_u64 = u64::try_from(count).map_err(|_| LineWindowError::CounterOverflow)?;
        self.offset.checked_add(count_u64).ok_or(LineWindowError::CounterOverflow)?;
        let mut consumed = 0;
        for &byte in &bytes[..count] {
            let start = self.offset;
            self.offset += 1; consumed += 1;
            if self.encoding.is_utf16() {
                if let Some(first) = self.half_unit.take() {
                    let unit = if self.encoding == DetectedEncoding::Utf16Le {
                        u16::from_le_bytes([first, byte])
                    } else { u16::from_be_bytes([first, byte]) };
                    self.unit(unit, start - 1, self.offset)?;
                } else { self.half_unit = Some(byte); }
            } else { self.unit(u16::from(byte), start, self.offset)?; }
            if self.finished { break; }
        }
        Ok(consumed)
    }

    fn unit(&mut self, unit: u16, start: u64, end: u64) -> Result<(), LineWindowError> {
        if self.pending_cr {
            self.pending_cr = false;
            if unit == 10 {
                if self.line == self.last.get() { self.resolve(end); }
                return Ok(());
            }
            if self.line == self.last.get() { self.resolve(start); return Ok(()); }
        }
        if !self.active_line {
            self.line = self.line.checked_add(1).ok_or(LineWindowError::CounterOverflow)?;
            self.active_line = true;
            if self.line == self.first.get() { self.start = Some(start); }
        }
        match unit {
            13 => { self.pending_cr = true; self.active_line = false; }
            10 => { self.active_line = false; if self.line == self.last.get() { self.resolve(end); } }
            _ => {}
        }
        Ok(())
    }

    fn resolve(&mut self, end: u64) { self.end = Some(end); self.finished = true; }

    /// Finalize ONLY after real EOF. A byte/work limit must leave it pending.
    pub fn finish(&mut self) -> Result<LineWindowStatus, LineWindowError> {
        self.eof = true;
        if !self.finished {
            if self.half_unit.take().is_some() {
                self.unit(u16::MAX, self.offset - 1, self.offset)?;
            }
            if !self.finished { self.resolve(self.offset); }
        }
        Ok(self.status())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn retargeted_checkpoints_match_zero_origin_across_decoder_boundaries() {
        let text = "\u{feff}one\r\n🦀two\rthree\nlast";
        for encoding in [utf8(), DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
            let bytes = if encoding.is_utf16() {
                text.encode_utf16().flat_map(|unit| if encoding == DetectedEncoding::Utf16Le {
                    unit.to_le_bytes()
                } else { unit.to_be_bytes() }).collect::<Vec<_>>()
            } else { text.as_bytes().to_vec() };
            // Every byte boundary includes split CRLF and split UTF-16 units.
            for boundary in 0..bytes.len() {
                let mut checkpoint = LineWindowScanner::new(LineNumber::new(99).unwrap(), 1, encoding).unwrap();
                checkpoint.step(ByteOffset::new(0), &bytes, boundary).unwrap();
                for first in checkpoint.lines_seen() + 1..=5 {
                    let mut resumed = checkpoint.retarget(LineNumber::new(first).unwrap(), 2).unwrap();
                    while !resumed.is_finished() && resumed.bytes_scanned() < bytes.len() as u64 {
                        let at = resumed.bytes_scanned() as usize;
                        resumed.step(ByteOffset::new(at as u64), &bytes[at..], 1).unwrap();
                    }
                    if !resumed.is_finished() { resumed.finish().unwrap(); }
                    assert_eq!(resumed.status(), locate(&bytes, first, 2, encoding, 1),
                        "encoding={encoding:?}, boundary={boundary}, first={first}");
                }
                if checkpoint.lines_seen() > 0 {
                    assert!(checkpoint.retarget(LineNumber::new(checkpoint.lines_seen()).unwrap(), 1).is_err());
                }
            }
        }
    }

    #[test]
    fn completed_checkpoint_cannot_be_retargeted() {
        let mut scan = LineWindowScanner::new(LineNumber::new(1).unwrap(), 1, utf8()).unwrap();
        scan.step(ByteOffset::new(0), b"one\nsecond", 100).unwrap();
        assert!(scan.retarget(LineNumber::new(2).unwrap(), 1).is_err());
        let mut eof = LineWindowScanner::new(LineNumber::new(9).unwrap(), 1, utf8()).unwrap();
        eof.finish().unwrap();
        assert!(eof.retarget(LineNumber::new(10).unwrap(), 1).is_err());
    }

    fn utf8() -> DetectedEncoding { DetectedEncoding::Utf8 { has_bom: false } }
    fn locate(bytes: &[u8], first: u64, count: u64, encoding: DetectedEncoding, step: usize) -> LineWindowStatus {
        let mut scan = LineWindowScanner::new(LineNumber::new(first).unwrap(), count, encoding).unwrap();
        let mut offset = 0;
        while offset < bytes.len() && !scan.is_finished() {
            let consumed = scan.step(ByteOffset::new(offset as u64), &bytes[offset..], step).unwrap();
            assert!(consumed > 0 && consumed <= step); offset += consumed;
        }
        if !scan.is_finished() { scan.finish().unwrap(); }
        scan.status()
    }
    fn oracle(bytes: &[u8]) -> Vec<(u64, u64)> {
        let mut lines = Vec::new(); let mut start = 0;
        while start < bytes.len() {
            let mut end = start;
            while end < bytes.len() && bytes[end] != b'\r' && bytes[end] != b'\n' { end += 1; }
            if end < bytes.len() {
                let cr = bytes[end] == b'\r'; end += 1;
                if cr && end < bytes.len() && bytes[end] == b'\n' { end += 1; }
            }
            lines.push((start as u64, end as u64)); start = end;
        }
        lines
    }
    #[test]
    fn every_small_chunk_and_window_matches_independent_newline_oracle() {
        for bytes in [b"".as_slice(), b"a", b"\n", b"\r", b"\r\n", b"a\r\nb\rc\nlast",
            b"\n\r\n\r\r\n\n", "\u{feff}a\n\u{1f980}\r\nlast".as_bytes(), b"a\xff\nb"] {
            let lines = oracle(bytes);
            for first in 1..=6 { for count in 1..=4 { for step in 1..=7 {
                let result = locate(bytes, first, count, utf8(), step);
                if first > lines.len() as u64 { assert_eq!(result, LineWindowStatus::Missing); }
                else {
                    let last = (first + count - 1).min(lines.len() as u64);
                    assert_eq!(result, LineWindowStatus::Resolved {
                        first: LineNumber::new(first).unwrap(), last: LineNumber::new(last).unwrap(),
                        range: ByteRange::new(ByteOffset::new(lines[first as usize - 1].0),
                            ByteOffset::new(lines[last as usize - 1].1)).unwrap(),
                    }, "bytes={bytes:?}, first={first}, count={count}, step={step}");
                }
            } } }
        }
    }
    #[test]
    fn utf16_units_surrogates_and_crlf_can_split_at_every_byte() {
        let text = "\u{feff}\u{0a41}\u{1f980}\r\nsecond\rthird\n";
        for encoding in [DetectedEncoding::Utf16Le, DetectedEncoding::Utf16Be] {
            let bytes: Vec<u8> = text.encode_utf16().flat_map(|unit| {
                if encoding == DetectedEncoding::Utf16Le { unit.to_le_bytes() } else { unit.to_be_bytes() }
            }).collect();
            for step in 1..=9 {
                assert_eq!(locate(&bytes, 2, 1, encoding, step), LineWindowStatus::Resolved {
                    first: LineNumber::new(2).unwrap(), last: LineNumber::new(2).unwrap(),
                    range: ByteRange::new(ByteOffset::new(12), ByteOffset::new(26)).unwrap(),
                });
                assert_eq!(locate(&bytes, 4, 1, encoding, step), LineWindowStatus::Missing);
            }
        }
    }
    #[test]
    fn budgets_do_not_finalize_a_far_jump_or_spin() {
        let mut scan = LineWindowScanner::new(LineNumber::new(100).unwrap(), 2, utf8()).unwrap();
        assert_eq!(scan.step(ByteOffset::new(0), b"a\nb\n", 0).unwrap(), 0);
        assert_eq!(scan.bytes_scanned(), 0);
        assert_eq!(scan.step(ByteOffset::new(0), b"a\nb\n", 3).unwrap(), 3);
        assert_eq!(scan.bytes_scanned(), 3);
        assert_eq!(scan.status(), LineWindowStatus::Pending);
        assert!(!scan.reached_eof());
        assert_eq!(scan.finish().unwrap(), LineWindowStatus::Missing);
    }
    #[test]
    fn noncontiguous_input_rejects_without_mutation() {
        let mut scan = LineWindowScanner::new(LineNumber::new(2).unwrap(), 1, utf8()).unwrap();
        scan.step(ByteOffset::new(0), b"a\n", 2).unwrap();
        assert_eq!(scan.step(ByteOffset::new(0), b"wrong", 5), Err(LineWindowError::NoncontiguousInput));
        assert_eq!(scan.bytes_scanned(), 2);
        assert_eq!(scan.step(ByteOffset::new(2), b"right\n", 6).unwrap(), 6);
        assert!(matches!(scan.status(), LineWindowStatus::Resolved { .. }));
    }
    #[test]
    fn lone_utf16_byte_is_retained_as_malformed_content() {
        assert_eq!(locate(&[b'x'], 1, 1, DetectedEncoding::Utf16Le, 1), LineWindowStatus::Resolved {
            first: LineNumber::new(1).unwrap(), last: LineNumber::new(1).unwrap(),
            range: ByteRange::new(ByteOffset::new(0), ByteOffset::new(1)).unwrap(),
        });
    }
    #[test]
    fn invalid_windows_and_unsupported_encodings_are_explicit() {
        assert!(matches!(LineWindowScanner::new(LineNumber::new(1).unwrap(), 0, utf8()), Err(LineWindowError::InvalidWindow)));
        assert!(matches!(LineWindowScanner::new(LineNumber::new(u64::MAX).unwrap(), 2, utf8()), Err(LineWindowError::InvalidWindow)));
        assert!(matches!(LineWindowScanner::new(LineNumber::new(1).unwrap(), 1, DetectedEncoding::Unsupported), Err(LineWindowError::UnsupportedEncoding)));
    }
    #[test]
    fn offset_overflow_is_checked_before_mutation() {
        let mut scan = LineWindowScanner::new(LineNumber::new(1).unwrap(), 1, utf8()).unwrap();
        scan.offset = u64::MAX;
        assert_eq!(scan.step(ByteOffset::new(u64::MAX), b"x", 1), Err(LineWindowError::CounterOverflow));
        assert_eq!(scan.bytes_scanned(), u64::MAX);
    }
    #[test]
    fn resolved_cursor_does_not_consume_the_remaining_source() {
        let mut scan = LineWindowScanner::new(LineNumber::new(1).unwrap(), 1, utf8()).unwrap();
        assert_eq!(scan.step(ByteOffset::new(0), b"first\nuntouched", usize::MAX).unwrap(), 6);
        assert!(!scan.reached_eof());
        assert_eq!(scan.step(ByteOffset::new(6), b"untouched", usize::MAX).unwrap(), 0);
        let result = scan.status();
        assert_eq!(scan.finish().unwrap(), result);
        assert_eq!(scan.finish().unwrap(), result);
    }
}
