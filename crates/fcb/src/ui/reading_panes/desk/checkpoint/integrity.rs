#![forbid(unsafe_code)]

//! Read-only integrity verification of an already saved desk. A successful core
//! save receipt supplies the expected length and digest. Rechecking only the
//! trailer cannot detect damaged source or notes in the body.
//!
//! This worker operation streams at most 64 MiB with 64 KiB fixed scratch, a
//! finite read-call allowance and cooperative cancellation. It neither restores
//! a desk nor grants access to source paths. It verifies byte identity against
//! the receipt; semantic validation of a newly imported desk remains restore's
//! responsibility. The supplied reader must start at the checkpoint's beginning.

use std::io::{self, Read};

use crate::store::envelope::{Sha256, Sha256Digest, CHECKSUM_LEN, FRAME_LEN, HEADER_LEN};
use super::{MAX_CHECKPOINT_BYTES, SCHEMA};

const READ_BYTES: usize = 64 * 1024;
const MAX_READ_CALLS: u64 = 65_536;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CheckpointIntegrityError {
    Limit, InvalidDocument, UnsupportedVersion, Integrity, Io,
    ReadCallLimit, InvalidReadCount, Canceled,
}
impl CheckpointIntegrityError {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Limit => "DESK_CHECKPOINT_LIMIT",
            Self::InvalidDocument => "DESK_CHECKPOINT_INVALID",
            Self::UnsupportedVersion => "DESK_CHECKPOINT_VERSION",
            Self::Integrity => "DESK_CHECKPOINT_INTEGRITY",
            Self::Io => "DESK_CHECKPOINT_IO",
            Self::ReadCallLimit => "DESK_CHECKPOINT_IO_CALL_LIMIT",
            Self::InvalidReadCount => "DESK_CHECKPOINT_INVALID_READ_COUNT",
            Self::Canceled => "DESK_CHECKPOINT_CANCELED",
        }
    }
}
impl std::fmt::Display for CheckpointIntegrityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.code())
    }
}
impl std::error::Error for CheckpointIntegrityError {}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct CheckpointIntegrity {
    pub bytes_read: u64,
    pub read_calls: u64,
    pub digest: Sha256Digest,
}

/// Verify the complete header/payload digest AND trailer against a prior
/// successful save receipt. No partial result can authorize copy retirement.
/// An incomplete read, extra byte, invalid version, cancellation or changed body
/// returns an error. Callers owning mutable files also validate file metadata
/// before and after this operation.
pub fn verify_checkpoint_integrity(
    input: &mut impl Read,
    expected_bytes: u64,
    expected_digest: Sha256Digest,
    mut canceled: impl FnMut() -> bool,
) -> Result<CheckpointIntegrity, CheckpointIntegrityError> {
    if !(FRAME_LEN as u64..=MAX_CHECKPOINT_BYTES as u64).contains(&expected_bytes) {
        return Err(CheckpointIntegrityError::Limit);
    }
    let mut reader = BoundedReader { input, calls: 0, bytes: 0 };
    let mut header = [0; HEADER_LEN];
    reader.exact(&mut header, &mut canceled)?;
    if header[..4] != SCHEMA.magic
        || u32::from_le_bytes(header[4..8].try_into().expect("fixed header")) != SCHEMA.schema_id
    {
        return Err(CheckpointIntegrityError::InvalidDocument);
    }
    let major = u16::from_le_bytes(header[8..10].try_into().expect("fixed header"));
    let minor = u16::from_le_bytes(header[10..12].try_into().expect("fixed header"));
    if major != SCHEMA.major || minor > SCHEMA.minor || header[12..16] != [0; 4] {
        return Err(CheckpointIntegrityError::UnsupportedVersion);
    }
    let mut remaining = u64::from_le_bytes(header[16..24].try_into().expect("fixed header"));
    if remaining != expected_bytes - FRAME_LEN as u64 {
        return Err(CheckpointIntegrityError::InvalidDocument);
    }
    let mut hash = Sha256::new();
    hash.update(&header);
    let mut scratch = [0; READ_BYTES];
    while remaining != 0 {
        let count = remaining.min(READ_BYTES as u64) as usize;
        reader.exact(&mut scratch[..count], &mut canceled)?;
        hash.update(&scratch[..count]);
        remaining -= count as u64;
    }
    let mut trailer = [0; CHECKSUM_LEN];
    reader.exact(&mut trailer, &mut canceled)?;
    if reader.read(&mut [0], &mut canceled)? != 0 {
        return Err(CheckpointIntegrityError::InvalidDocument);
    }
    let digest = hash.finalize();
    if digest != expected_digest || digest.as_bytes() != &trailer {
        return Err(CheckpointIntegrityError::Integrity);
    }
    if canceled() { return Err(CheckpointIntegrityError::Canceled); }
    Ok(CheckpointIntegrity { bytes_read: reader.bytes, read_calls: reader.calls, digest })
}

struct BoundedReader<'a, R> {
    input: &'a mut R,
    calls: u64,
    bytes: u64,
}
impl<R: Read> BoundedReader<'_, R> {
    fn read(&mut self, into: &mut [u8], canceled: &mut impl FnMut() -> bool)
        -> Result<usize, CheckpointIntegrityError> {
        loop {
            if canceled() { return Err(CheckpointIntegrityError::Canceled); }
            if self.calls == MAX_READ_CALLS { return Err(CheckpointIntegrityError::ReadCallLimit); }
            self.calls += 1;
            match self.input.read(into) {
                Ok(n) if n <= into.len() => {
                    self.bytes += n as u64;
                    return Ok(n);
                }
                Ok(_) => return Err(CheckpointIntegrityError::InvalidReadCount),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {},
                Err(_) => return Err(CheckpointIntegrityError::Io),
            }
        }
    }
    fn exact(&mut self, mut into: &mut [u8], canceled: &mut impl FnMut() -> bool)
        -> Result<(), CheckpointIntegrityError> {
        while !into.is_empty() {
            let n = self.read(into, canceled)?;
            if n == 0 { return Err(CheckpointIntegrityError::InvalidDocument); }
            into = &mut into[n..];
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    // Independent fixed wire vector shared with native recovery fixtures:
    // FCBK/1.1 empty desk, four zero counts, absent history, no active pane.
    fn fixture() -> (Vec<u8>, Sha256Digest) {
        let digest = Sha256Digest::new([
            0x90, 0x4c, 0x94, 0x2b, 0xd4, 0x0b, 0xf9, 0x3f,
            0xce, 0x38, 0x97, 0xfc, 0xad, 0x25, 0xa9, 0x03,
            0xd1, 0x93, 0xee, 0xfb, 0xca, 0x9f, 0xb4, 0x34,
            0x75, 0xdf, 0xe8, 0xd6, 0x5e, 0x33, 0xcb, 0xaf,
        ]);
        let mut bytes = b"FCBKDESK\x01\x00\x01\x00\x00\x00\x00\x00".to_vec();
        bytes.extend_from_slice(&48u64.to_le_bytes());
        bytes.extend_from_slice(&[0; 32]);
        bytes.extend_from_slice(&u64::MAX.to_le_bytes());
        bytes.extend_from_slice(&0u64.to_le_bytes());
        bytes.extend_from_slice(digest.as_bytes());
        (bytes, digest)
    }

    #[test]
    fn full_body_verification_rejects_damage_with_an_unchanged_receipt_and_trailer() {
        let (bytes, digest) = fixture();
        let verified = verify_checkpoint_integrity(&mut Cursor::new(&bytes), bytes.len() as u64,
            digest, || false).unwrap();
        assert_eq!(verified.bytes_read, bytes.len() as u64);
        for offset in HEADER_LEN..bytes.len() - CHECKSUM_LEN {
            let mut damaged = bytes.clone();
            damaged[offset] ^= 1;
            assert_eq!(&damaged[..HEADER_LEN], &bytes[..HEADER_LEN]);
            assert_eq!(&damaged[bytes.len() - CHECKSUM_LEN..], digest.as_bytes());
            assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(&damaged), bytes.len() as u64,
                digest, || false), Err(CheckpointIntegrityError::Integrity));
        }
    }

    #[test]
    fn a_new_valid_checksum_cannot_substitute_different_bytes_for_the_receipt() {
        let (mut bytes, digest) = fixture();
        bytes[HEADER_LEN] = 1;
        let end = bytes.len() - CHECKSUM_LEN;
        let changed = Sha256::digest(&bytes[..end]);
        bytes[end..].copy_from_slice(changed.as_bytes());
        assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(&bytes), bytes.len() as u64,
            digest, || false), Err(CheckpointIntegrityError::Integrity));
    }

    #[test]
    fn short_and_interrupted_reads_preserve_the_full_checksum() {
        struct Chopped { input: Cursor<Vec<u8>>, calls: usize }
        impl Read for Chopped {
            fn read(&mut self, into: &mut [u8]) -> io::Result<usize> {
                self.calls += 1;
                if self.calls % 3 == 0 { return Err(io::ErrorKind::Interrupted.into()); }
                let n = into.len().min(3);
                self.input.read(&mut into[..n])
            }
        }
        let (bytes, digest) = fixture();
        let length = bytes.len() as u64;
        let mut input = Chopped { input: Cursor::new(bytes), calls: 0 };
        let verified = verify_checkpoint_integrity(&mut input, length, digest, || false).unwrap();
        assert_eq!(verified.bytes_read, length);
        assert_eq!(verified.read_calls, input.calls as u64);
    }

    #[test]
    fn truncation_growth_and_limits_cannot_confirm_a_copy() {
        let (bytes, digest) = fixture();
        for end in 0..bytes.len() {
            assert!(verify_checkpoint_integrity(&mut Cursor::new(&bytes[..end]), bytes.len() as u64,
                digest, || false).is_err());
        }
        let mut grown = bytes.clone(); grown.push(0);
        assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(grown), bytes.len() as u64,
            digest, || false), Err(CheckpointIntegrityError::InvalidDocument));
        for length in [0, FRAME_LEN as u64 - 1, MAX_CHECKPOINT_BYTES as u64 + 1, u64::MAX] {
            let mut input = Cursor::new(&bytes);
            assert_eq!(verify_checkpoint_integrity(&mut input, length, digest, || false),
                Err(CheckpointIntegrityError::Limit));
            assert_eq!(input.position(), 0);
        }
    }

    #[test]
    fn payload_spanning_multiple_windows_checks_both_sides_of_every_boundary() {
        use crate::store::envelope::EnvelopeWriter;

        // Exercise the production envelope writer and verifier with a framed
        // payload longer than two scratch windows. Semantic desk restoration
        // is deliberately separate from this receipt-bound byte check.
        let payload: Vec<u8> = (0..READ_BYTES * 2 + 17).map(|n| (n % 251) as u8).collect();
        let mut out = EnvelopeWriter::new(SCHEMA);
        out.put_bytes(&payload);
        let bytes = out.finish();
        let end = bytes.len() - CHECKSUM_LEN;
        let digest = Sha256Digest::new(bytes[end..].try_into().unwrap());
        let verified = verify_checkpoint_integrity(&mut Cursor::new(&bytes), bytes.len() as u64,
            digest, || false).unwrap();
        assert_eq!(verified.bytes_read, bytes.len() as u64);
        assert_eq!(verified.digest, digest);
        assert!(verified.read_calls > 4, "the payload requires multiple reads");

        for offset in [HEADER_LEN + READ_BYTES - 1, HEADER_LEN + READ_BYTES,
            HEADER_LEN + READ_BYTES + 1, HEADER_LEN + 2 * READ_BYTES - 1,
            HEADER_LEN + 2 * READ_BYTES, end - 1] {
            let mut damaged = bytes.clone();
            damaged[offset] ^= 1;
            assert_eq!(&damaged[..HEADER_LEN], &bytes[..HEADER_LEN]);
            assert_eq!(&damaged[end..], digest.as_bytes());
            assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(&damaged), bytes.len() as u64,
                digest, || false), Err(CheckpointIntegrityError::Integrity), "offset {offset}");
        }
    }

    #[test]
    fn header_identity_versions_flags_and_declared_lengths_are_validated() {
        let (bytes, digest) = fixture();
        for (offset, error) in [
            (0, CheckpointIntegrityError::InvalidDocument),
            (4, CheckpointIntegrityError::InvalidDocument),
            (8, CheckpointIntegrityError::UnsupportedVersion),
            (10, CheckpointIntegrityError::UnsupportedVersion),
            (12, CheckpointIntegrityError::UnsupportedVersion),
            (16, CheckpointIntegrityError::InvalidDocument),
        ] {
            let mut damaged = bytes.clone();
            damaged[offset] ^= 2;
            assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(&damaged), bytes.len() as u64,
                digest, || false), Err(error), "header offset {offset}");
        }
        for length in [bytes.len() as u64 - 1, bytes.len() as u64 + 1] {
            assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(&bytes), length, digest, || false),
                Err(CheckpointIntegrityError::InvalidDocument));
        }

        // The documented 1.0 layout remains valid with its OWN receipt; a
        // supported older minor must not be rejected as an unknown extension.
        let mut legacy = bytes;
        legacy[10] = 0;
        let end = legacy.len() - CHECKSUM_LEN;
        let legacy_digest = Sha256::digest(&legacy[..end]);
        legacy[end..].copy_from_slice(legacy_digest.as_bytes());
        let verified = verify_checkpoint_integrity(&mut Cursor::new(&legacy), legacy.len() as u64,
            legacy_digest, || false).unwrap();
        assert_eq!(verified.digest, legacy_digest);
        assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(&legacy), legacy.len() as u64,
            digest, || false), Err(CheckpointIntegrityError::Integrity));
    }

    #[test]
    fn cancellation_and_hostile_readers_never_publish_integrity() {
        let (bytes, digest) = fixture();
        let mut checks = 0;
        assert_eq!(verify_checkpoint_integrity(&mut Cursor::new(&bytes), bytes.len() as u64, digest,
            || { checks += 1; checks == 3 }), Err(CheckpointIntegrityError::Canceled));
        struct Interrupted;
        impl Read for Interrupted {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> { Err(io::ErrorKind::Interrupted.into()) }
        }
        assert_eq!(verify_checkpoint_integrity(&mut Interrupted, bytes.len() as u64, digest, || false),
            Err(CheckpointIntegrityError::ReadCallLimit));
        struct Invalid;
        impl Read for Invalid {
            fn read(&mut self, into: &mut [u8]) -> io::Result<usize> { Ok(into.len() + 1) }
        }
        assert_eq!(verify_checkpoint_integrity(&mut Invalid, bytes.len() as u64, digest, || false),
            Err(CheckpointIntegrityError::InvalidReadCount));
    }
}
