//! Versioned canonical binary envelopes and SHA-256 digests (FCB-080).
//!
//! Provides deterministic binary envelopes for cache records, arena
//! snapshots, and metadata entries, matching the factored upstream
//! `fmn-hash` specification without foreign dependencies:
//!
//! - **Fixed little-endian** encoding for all numeric primitives.
//! - **Positional field order** driven by [`EnvelopeWriter`] call sequence.
//! - **Primitive canonicalization**: `-0.0 -> +0.0` and NaN collapsing to
//!   canonical quiet NaN.
//! - **Self-describing header**: 4-byte magic, 32-bit schema id, 16-bit
//!   major and minor version, payload length.
//! - **Trailing SHA-256 integrity checksum**: covering the full preceding
//!   document (header + payload).
//! - **Size limits**: total document and per-field bounds preventing
//!   allocation bombs.
//! - **Authorization separation**: valid checksum NEVER grants authorization;
//!   schema and consumer identity must match expected contracts.

use std::fmt;

pub const DEFAULT_MAGIC: [u8; 4] = *b"FCBE";
pub const HEADER_LEN: usize = 24;
pub const CHECKSUM_LEN: usize = 32;
pub const FRAME_LEN: usize = HEADER_LEN + CHECKSUM_LEN;

const CANONICAL_NAN_F32_BITS: u32 = 0x7fc0_0000;
const CANONICAL_NAN_F64_BITS: u64 = 0x7ff8_0000_0000_0000;

#[inline]
pub fn canonicalize_f32(value: f32) -> f32 {
    if value == 0.0 {
        0.0
    } else if value.is_nan() {
        f32::from_bits(CANONICAL_NAN_F32_BITS)
    } else {
        value
    }
}

#[inline]
pub fn canonicalize_f64(value: f64) -> f64 {
    if value == 0.0 {
        0.0
    } else if value.is_nan() {
        f64::from_bits(CANONICAL_NAN_F64_BITS)
    } else {
        value
    }
}

/// Standard SHA-256 digest (32 bytes).
#[derive(Clone, Copy, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct Sha256Digest([u8; 32]);

impl Sha256Digest {
    pub const fn new(bytes: [u8; 32]) -> Self {
        Self(bytes)
    }

    pub const fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    pub fn to_hex(&self) -> String {
        let mut s = String::with_capacity(64);
        for byte in &self.0 {
            use std::fmt::Write as _;
            let _ = write!(s, "{byte:02x}");
        }
        s
    }

    pub fn from_hex(hex: &str) -> Result<Self, HexError> {
        if hex.len() != 64 {
            return Err(HexError::InvalidLength);
        }
        let bytes = hex.as_bytes();
        let mut out = [0u8; 32];
        for i in 0..32 {
            let hi = hex_nibble(bytes[i * 2])?;
            let lo = hex_nibble(bytes[i * 2 + 1])?;
            out[i] = (hi << 4) | lo;
        }
        Ok(Self(out))
    }
}

fn hex_nibble(byte: u8) -> Result<u8, HexError> {
    match byte {
        b'0'..=b'9' => Ok(byte - b'0'),
        b'a'..=b'f' => Ok(byte - b'a' + 10),
        b'A'..=b'F' => Ok(byte - b'A' + 10),
        _ => Err(HexError::InvalidCharacter),
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum HexError {
    InvalidLength,
    InvalidCharacter,
}

impl fmt::Display for HexError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidLength => f.write_str("hex string must be exactly 64 characters"),
            Self::InvalidCharacter => f.write_str("non-hex character in digest string"),
        }
    }
}

impl std::error::Error for HexError {}

impl fmt::Debug for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Sha256Digest({})", self.to_hex())
    }
}

impl fmt::Display for Sha256Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_hex())
    }
}

/// FIPS 180-4 standard SHA-256 implementation.
pub struct Sha256 {
    state: [u32; 8],
    buffer: [u8; 64],
    buffer_len: usize,
    total_len: u64,
}

const K256: [u32; 64] = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
];

impl Default for Sha256 {
    fn default() -> Self {
        Self::new()
    }
}

impl Sha256 {
    pub const fn new() -> Self {
        Self {
            state: [
                0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a,
                0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
            ],
            buffer: [0u8; 64],
            buffer_len: 0,
            total_len: 0,
        }
    }

    pub fn update(&mut self, mut data: &[u8]) {
        self.total_len = self.total_len.saturating_add(data.len() as u64);
        while !data.is_empty() {
            if self.buffer_len == 0 && data.len() >= 64 {
                self.process_block(data[..64].try_into().unwrap());
                data = &data[64..];
            } else {
                let to_copy = (64 - self.buffer_len).min(data.len());
                self.buffer[self.buffer_len..self.buffer_len + to_copy].copy_from_slice(&data[..to_copy]);
                self.buffer_len += to_copy;
                data = &data[to_copy..];
                if self.buffer_len == 64 {
                    let block = self.buffer;
                    self.process_block(&block);
                    self.buffer_len = 0;
                }
            }
        }
    }

    fn process_block(&mut self, block: &[u8; 64]) {
        let mut w = [0u32; 64];
        for i in 0..16 {
            w[i] = u32::from_be_bytes(block[i * 4..i * 4 + 4].try_into().unwrap());
        }
        for i in 16..64 {
            let s0 = w[i - 15].rotate_right(7) ^ w[i - 15].rotate_right(18) ^ (w[i - 15] >> 3);
            let s1 = w[i - 2].rotate_right(17) ^ w[i - 2].rotate_right(19) ^ (w[i - 2] >> 10);
            w[i] = w[i - 16].wrapping_add(s0).wrapping_add(w[i - 7]).wrapping_add(s1);
        }

        let mut a = self.state[0];
        let mut b = self.state[1];
        let mut c = self.state[2];
        let mut d = self.state[3];
        let mut e = self.state[4];
        let mut f = self.state[5];
        let mut g = self.state[6];
        let mut h = self.state[7];

        for i in 0..64 {
            let s1 = e.rotate_right(6) ^ e.rotate_right(11) ^ e.rotate_right(25);
            let ch = (e & f) ^ ((!e) & g);
            let temp1 = h.wrapping_add(s1).wrapping_add(ch).wrapping_add(K256[i]).wrapping_add(w[i]);
            let s0 = a.rotate_right(2) ^ a.rotate_right(13) ^ a.rotate_right(22);
            let maj = (a & b) ^ (a & c) ^ (b & c);
            let temp2 = s0.wrapping_add(maj);

            h = g;
            g = f;
            f = e;
            e = d.wrapping_add(temp1);
            d = c;
            c = b;
            b = a;
            a = temp1.wrapping_add(temp2);
        }

        self.state[0] = self.state[0].wrapping_add(a);
        self.state[1] = self.state[1].wrapping_add(b);
        self.state[2] = self.state[2].wrapping_add(c);
        self.state[3] = self.state[3].wrapping_add(d);
        self.state[4] = self.state[4].wrapping_add(e);
        self.state[5] = self.state[5].wrapping_add(f);
        self.state[6] = self.state[6].wrapping_add(g);
        self.state[7] = self.state[7].wrapping_add(h);
    }

    pub fn finalize(mut self) -> Sha256Digest {
        let bit_len = self.total_len.wrapping_mul(8);
        self.buffer[self.buffer_len] = 0x80;
        self.buffer_len += 1;

        if self.buffer_len > 56 {
            for b in &mut self.buffer[self.buffer_len..64] {
                *b = 0;
            }
            let block = self.buffer;
            self.process_block(&block);
            self.buffer_len = 0;
        }

        for b in &mut self.buffer[self.buffer_len..56] {
            *b = 0;
        }
        self.buffer[56..64].copy_from_slice(&bit_len.to_be_bytes());
        let block = self.buffer;
        self.process_block(&block);

        let mut out = [0u8; 32];
        for (i, word) in self.state.iter().enumerate() {
            out[i * 4..i * 4 + 4].copy_from_slice(&word.to_be_bytes());
        }
        Sha256Digest(out)
    }

    pub fn digest(data: &[u8]) -> Sha256Digest {
        let mut hasher = Self::new();
        hasher.update(data);
        hasher.finalize()
    }
}

/// Schema identity binding a canonical envelope.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvelopeSchema {
    pub magic: [u8; 4],
    pub schema_id: u32,
    pub major: u16,
    pub minor: u16,
}

impl EnvelopeSchema {
    pub const fn new(schema_id: u32, major: u16, minor: u16) -> Self {
        Self {
            magic: DEFAULT_MAGIC,
            schema_id,
            major,
            minor,
        }
    }

    pub const fn with_magic(magic: [u8; 4], schema_id: u32, major: u16, minor: u16) -> Self {
        Self {
            magic,
            schema_id,
            major,
            minor,
        }
    }
}

/// Bounded limits for parsing envelopes to defend against allocation bombs.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct EnvelopeLimits {
    pub max_document_bytes: usize,
    pub max_payload_bytes: usize,
    pub max_field_bytes: usize,
}

impl Default for EnvelopeLimits {
    fn default() -> Self {
        Self {
            max_document_bytes: 64 * 1024 * 1024,
            max_payload_bytes: 32 * 1024 * 1024,
            max_field_bytes: 16 * 1024 * 1024,
        }
    }
}

/// Policy for handling unexpected trailing bytes in envelopes.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum UnknownPolicy {
    /// Reject any document with unconsumed trailing bytes.
    Strict,
    /// Allow unconsumed trailing bytes when major versions match.
    Lenient,
}

/// Errors raised during envelope decoding and validation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum EnvelopeError {
    TooShort,
    BadMagic([u8; 4]),
    SchemaMismatch { expected: u32, actual: u32 },
    MajorMismatch { expected: u16, actual: u16 },
    MinorOlder { expected: u16, actual: u16 },
    ChecksumMismatch { expected: Sha256Digest, computed: Sha256Digest },
    TrailingBytes(usize),
    LimitExceeded(&'static str),
    NonFiniteFloat,
    InvalidUtf8,
    UnexpectedEof,
}

impl fmt::Display for EnvelopeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooShort => f.write_str("envelope too short to contain valid header and checksum"),
            Self::BadMagic(m) => write!(f, "bad envelope magic: {m:?}"),
            Self::SchemaMismatch { expected, actual } => {
                write!(f, "schema id mismatch: expected {expected:#x}, found {actual:#x}")
            }
            Self::MajorMismatch { expected, actual } => {
                write!(f, "major version mismatch: expected {expected}, found {actual}")
            }
            Self::MinorOlder { expected, actual } => {
                write!(f, "document minor {actual} is older than expected {expected}")
            }
            Self::ChecksumMismatch { expected, computed } => {
                write!(f, "envelope checksum mismatch: expected {expected}, computed {computed}")
            }
            Self::TrailingBytes(count) => write!(f, "{count} unexpected trailing bytes in envelope"),
            Self::LimitExceeded(msg) => write!(f, "envelope limit exceeded: {msg}"),
            Self::NonFiniteFloat => f.write_str("non-finite float in strict finite mode"),
            Self::InvalidUtf8 => f.write_str("invalid UTF-8 in string field"),
            Self::UnexpectedEof => f.write_str("unexpected EOF while reading envelope payload"),
        }
    }
}

impl std::error::Error for EnvelopeError {}

/// Writer for building canonical binary envelopes with trailing checksums.
#[derive(Clone, Debug, PartialEq)]
pub struct EnvelopeWriter {
    schema: EnvelopeSchema,
    payload: Vec<u8>,
}

impl EnvelopeWriter {
    pub fn new(schema: EnvelopeSchema) -> Self {
        Self {
            schema,
            payload: Vec::new(),
        }
    }

    pub fn put_u8(&mut self, value: u8) {
        self.payload.push(value);
    }

    pub fn put_u16(&mut self, value: u16) {
        self.payload.extend_from_slice(&value.to_le_bytes());
    }

    pub fn put_u32(&mut self, value: u32) {
        self.payload.extend_from_slice(&value.to_le_bytes());
    }

    pub fn put_u64(&mut self, value: u64) {
        self.payload.extend_from_slice(&value.to_le_bytes());
    }

    pub fn put_i64(&mut self, value: i64) {
        self.payload.extend_from_slice(&value.to_le_bytes());
    }

    pub fn put_f32(&mut self, value: f32) {
        let canonical = canonicalize_f32(value);
        self.payload.extend_from_slice(&canonical.to_bits().to_le_bytes());
    }

    pub fn put_f64(&mut self, value: f64) {
        let canonical = canonicalize_f64(value);
        self.payload.extend_from_slice(&canonical.to_bits().to_le_bytes());
    }

    pub fn put_bytes(&mut self, bytes: &[u8]) {
        self.put_u64(bytes.len() as u64);
        self.payload.extend_from_slice(bytes);
    }

    pub fn put_str(&mut self, s: &str) {
        self.put_bytes(s.as_bytes());
    }

    /// Finish building the envelope, appending the header and trailing SHA-256.
    pub fn finish(self) -> Vec<u8> {
        let payload_len = self.payload.len() as u64;
        let total_doc_len = HEADER_LEN + self.payload.len() + CHECKSUM_LEN;
        let mut document = Vec::with_capacity(total_doc_len);

        // Header: 24 bytes
        document.extend_from_slice(&self.schema.magic);
        document.extend_from_slice(&self.schema.schema_id.to_le_bytes());
        document.extend_from_slice(&self.schema.major.to_le_bytes());
        document.extend_from_slice(&self.schema.minor.to_le_bytes());
        document.extend_from_slice(&0u16.to_le_bytes()); // flags
        document.extend_from_slice(&0u16.to_le_bytes()); // reserved
        document.extend_from_slice(&payload_len.to_le_bytes());

        // Payload
        document.extend_from_slice(&self.payload);

        // Trailing checksum over [0 .. 24 + payload_len]
        let checksum = Sha256::digest(&document);
        document.extend_from_slice(checksum.as_bytes());

        document
    }
}

/// Reader for parsing and validating canonical binary envelopes.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct EnvelopeReader<'a> {
    schema: EnvelopeSchema,
    payload: &'a [u8],
    cursor: usize,
    limits: EnvelopeLimits,
    policy: UnknownPolicy,
}

impl<'a> EnvelopeReader<'a> {
    /// Open and validate an envelope. Validates checksum, magic, schema ID,
    /// major version, payload length, and document bounds.
    pub fn open(
        document: &'a [u8],
        expected_schema: EnvelopeSchema,
        limits: EnvelopeLimits,
        policy: UnknownPolicy,
    ) -> Result<Self, EnvelopeError> {
        if document.len() < FRAME_LEN {
            return Err(EnvelopeError::TooShort);
        }
        if document.len() > limits.max_document_bytes {
            return Err(EnvelopeError::LimitExceeded("document exceeds max_document_bytes"));
        }

        let payload_end = document.len() - CHECKSUM_LEN;
        let stored_checksum = Sha256Digest(
            document[payload_end..]
                .try_into()
                .map_err(|_| EnvelopeError::TooShort)?,
        );

        // Verify checksum first
        let computed_checksum = Sha256::digest(&document[..payload_end]);
        if computed_checksum != stored_checksum {
            return Err(EnvelopeError::ChecksumMismatch {
                expected: stored_checksum,
                computed: computed_checksum,
            });
        }

        // Header validation
        let magic: [u8; 4] = document[0..4].try_into().unwrap();
        if magic != expected_schema.magic {
            return Err(EnvelopeError::BadMagic(magic));
        }

        let schema_id = u32::from_le_bytes(document[4..8].try_into().unwrap());
        if schema_id != expected_schema.schema_id {
            // Checksum was valid, but schema does NOT match: checksum never grants authorization!
            return Err(EnvelopeError::SchemaMismatch {
                expected: expected_schema.schema_id,
                actual: schema_id,
            });
        }

        let major = u16::from_le_bytes(document[8..10].try_into().unwrap());
        if major != expected_schema.major {
            return Err(EnvelopeError::MajorMismatch {
                expected: expected_schema.major,
                actual: major,
            });
        }

        let minor = u16::from_le_bytes(document[10..12].try_into().unwrap());
        if minor < expected_schema.minor {
            return Err(EnvelopeError::MinorOlder {
                expected: expected_schema.minor,
                actual: minor,
            });
        }

        let payload_len = u64::from_le_bytes(document[16..24].try_into().unwrap()) as usize;
        let actual_payload_len = payload_end - HEADER_LEN;
        if payload_len > limits.max_payload_bytes {
            return Err(EnvelopeError::LimitExceeded("payload exceeds max_payload_bytes"));
        }
        if payload_len > actual_payload_len {
            return Err(EnvelopeError::UnexpectedEof);
        }

        let payload = &document[HEADER_LEN..HEADER_LEN + payload_len];

        // Trailing bytes check
        if actual_payload_len > payload_len && policy == UnknownPolicy::Strict {
            return Err(EnvelopeError::TrailingBytes(actual_payload_len - payload_len));
        }

        Ok(Self {
            schema: EnvelopeSchema {
                magic,
                schema_id,
                major,
                minor,
            },
            payload,
            cursor: 0,
            limits,
            policy,
        })
    }

    pub fn schema(&self) -> EnvelopeSchema {
        self.schema
    }

    pub fn remaining(&self) -> usize {
        self.payload.len().saturating_sub(self.cursor)
    }

    pub fn get_u8(&mut self) -> Result<u8, EnvelopeError> {
        if self.cursor >= self.payload.len() {
            return Err(EnvelopeError::UnexpectedEof);
        }
        let b = self.payload[self.cursor];
        self.cursor += 1;
        Ok(b)
    }

    pub fn get_u16(&mut self) -> Result<u16, EnvelopeError> {
        if self.cursor + 2 > self.payload.len() {
            return Err(EnvelopeError::UnexpectedEof);
        }
        let v = u16::from_le_bytes(self.payload[self.cursor..self.cursor + 2].try_into().unwrap());
        self.cursor += 2;
        Ok(v)
    }

    pub fn get_u32(&mut self) -> Result<u32, EnvelopeError> {
        if self.cursor + 4 > self.payload.len() {
            return Err(EnvelopeError::UnexpectedEof);
        }
        let v = u32::from_le_bytes(self.payload[self.cursor..self.cursor + 4].try_into().unwrap());
        self.cursor += 4;
        Ok(v)
    }

    pub fn get_u64(&mut self) -> Result<u64, EnvelopeError> {
        if self.cursor + 8 > self.payload.len() {
            return Err(EnvelopeError::UnexpectedEof);
        }
        let v = u64::from_le_bytes(self.payload[self.cursor..self.cursor + 8].try_into().unwrap());
        self.cursor += 8;
        Ok(v)
    }

    pub fn get_i64(&mut self) -> Result<i64, EnvelopeError> {
        if self.cursor + 8 > self.payload.len() {
            return Err(EnvelopeError::UnexpectedEof);
        }
        let v = i64::from_le_bytes(self.payload[self.cursor..self.cursor + 8].try_into().unwrap());
        self.cursor += 8;
        Ok(v)
    }

    pub fn get_f32(&mut self) -> Result<f32, EnvelopeError> {
        let bits = self.get_u32()?;
        let val = f32::from_bits(bits);
        Ok(canonicalize_f32(val))
    }

    pub fn get_f64(&mut self) -> Result<f64, EnvelopeError> {
        let bits = self.get_u64()?;
        let val = f64::from_bits(bits);
        Ok(canonicalize_f64(val))
    }

    pub fn get_bytes(&mut self) -> Result<&'a [u8], EnvelopeError> {
        let len = self.get_u64()? as usize;
        if len > self.limits.max_field_bytes {
            return Err(EnvelopeError::LimitExceeded("field exceeds max_field_bytes"));
        }
        if self.cursor + len > self.payload.len() {
            return Err(EnvelopeError::UnexpectedEof);
        }
        let slice = &self.payload[self.cursor..self.cursor + len];
        self.cursor += len;
        Ok(slice)
    }

    pub fn get_str(&mut self) -> Result<&'a str, EnvelopeError> {
        let bytes = self.get_bytes()?;
        std::str::from_utf8(bytes).map_err(|_| EnvelopeError::InvalidUtf8)
    }

    /// Check that all payload bytes have been consumed. Under Strict policy,
    /// unconsumed payload bytes return an error.
    pub fn finish(self) -> Result<(), EnvelopeError> {
        if self.cursor < self.payload.len() && self.policy == UnknownPolicy::Strict {
            Err(EnvelopeError::TrailingBytes(self.payload.len() - self.cursor))
        } else {
            Ok(())
        }
    }
}
