#![forbid(unsafe_code)]

//! Small schema-specific encoder, not a general JSON parser or serializer.
//! Full-width integers are decimal STRINGS; Unix paths carry reversible bytes.
//! Build one bounded document before stdout, never interleave partial JSON
//! documents. Failed writes cannot be repaired by appending a second document.

use std::{io::{self, Write}, mem::size_of, path::Path};
use fcb_core::{ArenaOwnerId, ByteLength, ResourceAllocationId, ResourceBudget, ResourceLease};
use fcb::source::RawPath;

pub const MAX_RESPONSE_BYTES: usize = 8 * 1024 * 1024;
pub const MAX_ENCODED_BYTES: usize = 16 * 1024 * 1024;
const PATH_SCRATCH_BYTES: usize = 256 * 1024;
const HEX: &[u8; 16] = b"0123456789abcdef";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum OutputError { Limit, Allocation, UnsupportedPath }
impl OutputError {
    pub const fn code(self) -> &'static str {
        match self { Self::Limit => "CLI_OUTPUT_LIMIT", Self::Allocation => "CLI_OUTPUT_ADMISSION",
            Self::UnsupportedPath => "CLI_PATH_ENCODING_UNSUPPORTED" }
    }
}
impl std::fmt::Display for OutputError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str(self.code()) }
}
impl std::error::Error for OutputError {}

pub struct Output {
    bytes: Vec<u8>,
    limit: usize,
    _lease: ResourceLease,
}
impl Output {
    pub fn new(owner: ArenaOwnerId, limit: usize, budget: &ResourceBudget,
        allocation: ResourceAllocationId) -> Result<Self, OutputError> {
        if limit == 0 || limit > MAX_ENCODED_BYTES { return Err(OutputError::Limit); }
        let charged = limit + PATH_SCRATCH_BYTES + size_of::<Self>();
        let lease = budget.try_reserve_managed(owner, allocation, ByteLength::new(charged as u64))
            .map_err(|_| OutputError::Allocation)?;
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(limit).map_err(|_| OutputError::Allocation)?;
        if bytes.capacity() > limit { return Err(OutputError::Allocation); }
        Ok(Self { bytes, limit, _lease: lease })
    }
    pub fn as_bytes(&self) -> &[u8] { &self.bytes }
    pub fn clear(&mut self) { self.bytes.clear(); }
    pub(crate) fn literal(&mut self, text: &str) -> Result<(), OutputError> { self.append(text.as_bytes()) }
    fn append(&mut self, bytes: &[u8]) -> Result<(), OutputError> {
        if bytes.len() > self.limit - self.bytes.len() { return Err(OutputError::Limit); }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
    pub(crate) fn quoted(&mut self, text: &str) -> Result<(), OutputError> {
        self.append(b"\"")?;
        for byte in text.bytes() {
            match byte {
                b'"' => self.append(b"\\\"")?, b'\\' => self.append(b"\\\\")?,
                b'\n' => self.append(b"\\n")?, b'\r' => self.append(b"\\r")?, b'\t' => self.append(b"\\t")?,
                0..=31 => self.append(&[b'\\', b'u', b'0', b'0', HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]])?,
                _ => self.append(&[byte])?,
            }
        }
        self.append(b"\"")
    }
    /// All integer-valued wire fields use canonical decimal strings, even small
    /// instances, so their type cannot change when a later value exceeds 2^53.
    pub(crate) fn integer(&mut self, mut value: u64) -> Result<(), OutputError> {
        let mut buffer = [0u8; 20];
        let mut start = buffer.len();
        loop {
            start -= 1; buffer[start] = b'0' + (value % 10) as u8; value /= 10;
            if value == 0 { break; }
        }
        self.append(b"\"")?; self.append(&buffer[start..])?; self.append(b"\"")
    }
    pub(crate) fn boolean(&mut self, value: bool) -> Result<(), OutputError> {
        self.literal(if value { "true" } else { "false" })
    }
    pub(crate) fn hex(&mut self, bytes: &[u8]) -> Result<(), OutputError> {
        self.append(b"\"")?;
        for &byte in bytes { self.append(&[HEX[(byte >> 4) as usize], HEX[(byte & 15) as usize]])?; }
        self.append(b"\"")
    }
    pub(crate) fn range(&mut self, range: fcb::ByteRange) -> Result<(), OutputError> {
        self.literal("{\"start\":")?; self.integer(range.start().get())?;
        self.literal(",\"end\":")?; self.integer(range.end().get())?; self.literal("}")
    }
    pub(crate) fn path(&mut self, path: &Path) -> Result<(), OutputError> {
        #[cfg(unix)] {
            use std::os::unix::ffi::OsStrExt;
            let bytes = path.as_os_str().as_bytes();
            if bytes.len() > 16_384 { return Err(OutputError::Limit); }
            self.literal("{\"encoding\":\"unix-bytes\",\"hex\":")?; self.hex(bytes)?;
            self.literal(",\"display\":")?;
            // Display is explicitly secondary to the reversible native payload.
            // Scratch is bounded and precharged separately from the response.
            let raw = RawPath::from_bytes(bytes);
            self.quoted(&raw.display_escaped().to_string())?;
            self.literal("}")
        }
        #[cfg(not(unix))] { let _ = path; Err(OutputError::UnsupportedPath) }
    }
    /// Human output cannot send terminal escape sequences or bidi controls from
    /// a hostile source. This is an escaped display, NOT an original-byte copy.
    pub(crate) fn human_text(&mut self, text: &str) -> Result<(), OutputError> {
        for ch in text.chars() {
            if (ch.is_control() && !matches!(ch, '\n' | '\t'))
                || matches!(ch, '\u{061c}' | '\u{200e}' | '\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
                for escaped in ch.escape_unicode() {
                    let mut buffer = [0u8; 4]; self.literal(escaped.encode_utf8(&mut buffer))?;
                }
            } else { let mut buffer = [0u8; 4]; self.literal(ch.encode_utf8(&mut buffer))?; }
        }
        Ok(())
    }

    /// Bounded number and size of writes, including Interrupted retries. The
    /// supplied writer may still block inside one foreign write: no wall-clock
    /// deadline is claimed. This runs on the CLI's thread, never an AppKit or
    /// shared-publication callback. On failure, stdout is incomplete output.
    pub fn deliver(&self, writer: &mut impl Write, max_calls: usize,
        mut canceled: impl FnMut() -> bool) -> io::Result<()> {
        let mut offset = 0;
        let mut calls = 0;
        while offset < self.bytes.len() {
            if canceled() { return Err(io::Error::new(io::ErrorKind::Interrupted, "CLI_CANCELED")); }
            if calls == max_calls { return Err(io::Error::new(io::ErrorKind::TimedOut, "CLI_OUTPUT_CALL_LIMIT")); }
            let end = self.bytes.len().min(offset.saturating_add(16 * 1024));
            let offered = end - offset;
            calls += 1;
            match writer.write(&self.bytes[offset..end]) {
                Ok(0) => return Err(io::Error::new(io::ErrorKind::WriteZero, "CLI_OUTPUT_NO_PROGRESS")),
                Ok(count) if count <= offered => offset += count,
                Ok(_) => return Err(io::Error::new(io::ErrorKind::InvalidData, "CLI_OUTPUT_INVALID_COUNT")),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => {},
                Err(error) => return Err(error),
            }
        }
        // No flush is required: deliver writes directly to the host's writer;
        // buffering/flush ownership remains explicit at its caller boundary.
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn output(limit: usize) -> Output {
        let owner = ArenaOwnerId::new(1).unwrap();
        let budget = ResourceBudget::new(owner, ByteLength::new(16 * 1024 * 1024)).unwrap();
        Output::new(owner, limit, &budget, ResourceAllocationId::new(1).unwrap()).unwrap()
    }
    #[test]
    fn json_escapes_all_c0_quotes_and_backslashes_without_changing_unicode() {
        let mut out = output(8192);
        let value = "\0\u{0001}\t\n\r\u{001f}\"\\é😀";
        out.quoted(value).unwrap();
        assert_eq!(std::str::from_utf8(out.as_bytes()).unwrap(), "\"\\u0000\\u0001\\t\\n\\r\\u001f\\\"\\\\é😀\"");
        for ch in ['\u{007f}', '\u{202e}', '\u{ffff}', '\u{10ffff}'] {
            out.clear(); out.quoted(&ch.to_string()).unwrap();
            assert!(std::str::from_utf8(out.as_bytes()).is_ok());
        }
    }
    #[test]
    fn decimal_wire_fields_preserve_adjacent_large_ids() {
        for number in [0, 1, (1 << 53) + 1, u64::MAX] {
            let mut out = output(32); out.integer(number).unwrap();
            assert_eq!(out.as_bytes(), format!("\"{number}\"").as_bytes());
        }
    }
    #[test]
    fn byte_paths_never_round_trip_through_the_display_label() {
        #[cfg(unix)] {
            use std::os::unix::ffi::OsStringExt;
            let path = std::path::PathBuf::from(std::ffi::OsString::from_vec(vec![b'a', 0xff, b'\n', b'"']));
            let mut out = output(1024); out.path(&path).unwrap();
            let text = std::str::from_utf8(out.as_bytes()).unwrap();
            assert!(text.contains("\"hex\":\"61ff0a22\""));
            assert!(!text.contains('\n')); assert!(!text.contains('\u{fffd}'));
        }
    }
    #[test]
    fn capacity_is_enforced_before_any_external_write() {
        let mut out = output(4); out.literal("1234").unwrap();
        assert_eq!(out.literal("5"), Err(OutputError::Limit));
        assert_eq!(out.as_bytes(), b"1234");
        out.clear(); out.literal("ok").unwrap();
    }
    struct Slow { bytes: Vec<u8>, stop: usize }
    impl Write for Slow {
        fn write(&mut self, input: &[u8]) -> io::Result<usize> {
            if self.bytes.len() == self.stop { return Err(io::ErrorKind::BrokenPipe.into()); }
            self.bytes.push(input[0]); Ok(1)
        }
        fn flush(&mut self) -> io::Result<()> { panic!("caller owns flushing") }
    }
    #[test]
    fn slow_closed_canceled_and_nonprogress_writers_are_bounded() {
        let mut out = output(64); out.literal("{\"done\":true}\n").unwrap();
        let mut slow = Slow { bytes: Vec::new(), stop: 100 };
        out.deliver(&mut slow, 64, || false).unwrap();
        assert_eq!(slow.bytes, out.as_bytes());
        let mut broken = Slow { bytes: Vec::new(), stop: 3 };
        assert_eq!(out.deliver(&mut broken, 64, || false).unwrap_err().kind(), io::ErrorKind::BrokenPipe);
        assert_eq!(broken.bytes.len(), 3);
        let mut limited = Slow { bytes: Vec::new(), stop: 100 };
        assert_eq!(out.deliver(&mut limited, 2, || false).unwrap_err().kind(), io::ErrorKind::TimedOut);
        assert_eq!(limited.bytes.len(), 2);
        let mut untouched = Vec::new();
        assert!(out.deliver(&mut untouched, 64, || true).is_err()); assert!(untouched.is_empty());
        struct Interrupted;
        impl Write for Interrupted {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> { Err(io::ErrorKind::Interrupted.into()) }
            fn flush(&mut self) -> io::Result<()> { Ok(()) }
        }
        assert_eq!(out.deliver(&mut Interrupted, 4, || false).unwrap_err().kind(), io::ErrorKind::TimedOut);
    }
    #[test]
    fn terminal_controls_are_escaped_only_in_the_human_representation() {
        let mut out = output(1024); out.human_text("a\u{001b}[2J\u{202e}\nb").unwrap();
        assert_eq!(out.as_bytes(), b"a\\u{1b}[2J\\u{202e}\nb");
    }
}
