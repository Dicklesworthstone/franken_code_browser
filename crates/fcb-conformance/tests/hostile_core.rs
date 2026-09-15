//! Focused tests for the hostile generator, mutation and minimizer core.
//! Determinism, bounds, classification preservation and budget exhaustion.

#![forbid(unsafe_code)]

use fcb_conformance::{
    minimize, HostileByteGenerator, HostileStructureGenerator, ReproducibleDigest, SeededRng,
    TerminationBudget,
};

#[test]
fn seeded_rng_is_reproducible_per_seed() {
    let mut a = SeededRng::new(42);
    let mut b = SeededRng::new(42);
    let mut c = SeededRng::new(43);
    for _ in 0..32 {
        let (x1, y1) = (a.next_u64(), a.next_u64());
        let (x2, y2) = (b.next_u64(), b.next_u64());
        assert_eq!(x1, x2, "same seed, same sequence");
        assert_eq!(y1, y2);
        assert_ne!(x1, y1, "the sequence must advance");
        let _ = c.next_u64();
    }
}

#[test]
fn hostile_bytes_respect_bounds_and_stay_deterministic() {
    let mut a = HostileByteGenerator::new(7, 64);
    let mut b = HostileByteGenerator::new(7, 64);
    for _ in 0..32 {
        let sa = a.generate();
        let sb = b.generate();
        assert_eq!(sa, sb, "same seed must yield the same stream");
        assert!(!sa.is_empty());
        assert!(sa.len() <= 64);
    }
}

#[test]
fn hostile_structure_splices_stay_valid_utf8() {
    let mut generator = HostileStructureGenerator::new(9, 24);
    let source = generator.generate();
    assert!(!source.is_empty());
    let spliced = generator.splice(&source);
    assert!(std::str::from_utf8(spliced.as_bytes()).is_ok());
}

#[test]
fn digests_are_stable_length_sensitive_and_hex_stable() {
    let a = ReproducibleDigest::of_bytes(b"hostile input");
    let again = ReproducibleDigest::of_bytes(b"hostile input");
    let different = ReproducibleDigest::of_bytes(b"hostile inpuz");
    assert_eq!(a, again);
    assert_ne!(a, different);
    // Truncation changes the digest.
    assert_ne!(a, ReproducibleDigest::of_bytes(b"hostile inpu"));
    assert_eq!(a.to_hex(), again.to_hex());
}

#[test]
fn minimizer_preserves_classification_and_shrinks() {
    // The "lexical failure": a NUL byte anywhere in the stream.
    let mut classify = |input: &[u8]| -> Option<&'static str> {
        if input.contains(&0x00) {
            Some("NUL_BYTE")
        } else {
            None
        }
    };
    let mut source = b"clean clean clean\x00clean clean clean".to_vec();
    source.extend(std::iter::repeat(b'x').take(64));
    let report = minimize(&source, TerminationBudget::attempts(512), &mut classify);
    assert!(report.classification_preserved);
    assert!(!report.exhausted_budget);
    assert!(
        report.minimized.len() < source.len(),
        "minimization must shrink a padded failure"
    );
    assert!(classify(&report.minimized) == Some("NUL_BYTE"));
}

#[test]
fn minimizer_exhausts_its_budget_safely() {
    let mut classify = |input: &[u8]| -> Option<&'static str> {
        if input.contains(&0x00) {
            Some("NUL_BYTE")
        } else {
            None
        }
    };
    let mut source = b"a\x00bcdefghijklmnop".to_vec();
    source.extend(std::iter::repeat(b'z').take(512));
    let report = minimize(&source, TerminationBudget::attempts(3), &mut classify);
    assert!(report.exhausted_budget);
    assert_eq!(report.attempts, 3);
    assert!(report.classification_preserved);
    assert!(classify(&report.minimized) == Some("NUL_BYTE"));
}

#[test]
fn non_failing_input_short_circuits_immediately() {
    // A source that does not fail makes minimization meaningless: the report
    // says so, the initial classification is a precondition (not an
    // attempt), and nothing is changed.
    let report = minimize(
        b"anything",
        TerminationBudget::attempts(10),
        &mut |_input: &[u8]| -> Option<&'static str> { None },
    );
    assert!(!report.classification_preserved);
    assert_eq!(report.attempts, 0);
    assert_eq!(report.minimized, b"anything".to_vec());
}
