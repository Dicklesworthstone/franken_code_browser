#![forbid(unsafe_code)]

//! Original-byte proof for native exact-hit activation. Construct once for each
//! matching capture, before its hits can be published. Paging never hashes or
//! reopens a source. The enclosing RetainedFile reservation accounts for this
//! fixed-size value; hashing uses only the first-party incremental SHA-256 state.

use fcb::store::{Sha256, Sha256Digest};
use crate::output::Output;
use super::{AtlasSearchError, check};

pub(super) struct CaptureWitness {
    digest: Sha256Digest,
    byte_length: u64,
}
impl CaptureWitness {
    pub(super) fn new(bytes: &[u8], canceled: &mut impl FnMut() -> bool)
        -> Result<Self, AtlasSearchError> {
        check(canceled)?;
        let mut hash = Sha256::new();
        for chunk in bytes.chunks(64 * 1024) {
            check(canceled)?;
            hash.update(chunk);
        }
        check(canceled)?;
        Ok(Self { digest: hash.finalize(), byte_length: bytes.len() as u64 })
    }

    pub(super) fn byte_length(&self) -> u64 { self.byte_length }

    pub(super) fn encode(&self, out: &mut Output) -> Result<(), AtlasSearchError> {
        out.literal(",\"capture_sha256\":")?; out.quoted(&self.digest.to_hex())?;
        out.literal(",\"capture_byte_length\":")?; out.integer(self.byte_length)?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn original_bytes_not_decoded_or_normalized_text_are_hashed() {
        let cases: &[(&[u8], &str)] = &[
            (b"", "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"),
            (b"abc", "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"),
            (b"\xff\xfea\0b\0c\0", "aec4cd5ed4c05ed5cf9dea5a8b30f5e655ad87b937b36e3c059840ce3cf3642f"),
        ];
        for &(bytes, expected) in cases {
            let witness = CaptureWitness::new(bytes, &mut || false).unwrap();
            assert_eq!(witness.digest.to_hex(), expected);
            assert_eq!(witness.byte_length(), bytes.len() as u64);
        }
        let composed = CaptureWitness::new("é".as_bytes(), &mut || false).unwrap();
        let decomposed = CaptureWitness::new("e\u{301}".as_bytes(), &mut || false).unwrap();
        assert_ne!(composed.digest.to_hex(), decomposed.digest.to_hex());
        let malformed = CaptureWitness::new(b"a\xff\0", &mut || false).unwrap();
        assert_eq!(malformed.byte_length(), 3);
    }

    #[test]
    fn chunk_boundaries_do_not_change_the_witness() {
        let bytes: Vec<u8> = (0..(2 * 64 * 1024 + 17)).map(|n| (n % 251) as u8).collect();
        let mut one = Sha256::new(); one.update(&bytes);
        let mut polls = 0;
        let witness = CaptureWitness::new(&bytes, &mut || { polls += 1; false }).unwrap();
        assert_eq!(witness.digest.to_hex(), one.finalize().to_hex());
        assert_eq!(polls, 5); // Admission, three bounded chunks, final publication gate.
    }

    #[test]
    fn canceled_hash_never_becomes_a_publishable_witness() {
        assert!(matches!(CaptureWitness::new(b"", &mut || true), Err(AtlasSearchError::Canceled)));
        let bytes = vec![0; 2 * 64 * 1024 + 1];
        for stop_at in 1..=5 {
            let mut polls = 0;
            let result = CaptureWitness::new(&bytes, &mut || { polls += 1; polls == stop_at });
            assert!(matches!(result, Err(AtlasSearchError::Canceled)));
            assert_eq!(polls, stop_at);
        }
    }
}
