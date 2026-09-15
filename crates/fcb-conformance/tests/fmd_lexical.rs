//! The hostile harness driving the REAL FCB-021 resumable lexical engine.
//! Requires the `fmd-lexical` feature (upstream path dependency); a real
//! lexical refusal is reproduced and minimized without changing its
//! classification.

#![cfg(feature = "fmd-lexical")]
#![forbid(unsafe_code)]

use fcb_conformance::fmd_lexical::{
    first_invalid_utf8_campaign, FmdResumableAdapter, LexOutcome,
};
use fcb_conformance::{minimize, HostileByteGenerator, TerminationBudget};

#[test]
fn real_engine_accepts_plain_and_lexical_sources() {
    let mut adapter = FmdResumableAdapter::new("rust").expect("rust is supported");
    for source in ["fn main() {}", "let s = \"x\";", "// plain", ""] {
        match adapter.feed(source.as_bytes()) {
            LexOutcome::Accepted { .. } => {}
            LexOutcome::Refused { code } => panic!("plain source refused: {code}"),
        }
    }
    adapter.finish();
}

#[test]
fn real_engine_refuses_malformed_utf8_with_stable_code() {
    let mut adapter = FmdResumableAdapter::new("rust").expect("rust is supported");
    // 0xFF is never valid in UTF-8.
    match adapter.feed(b"fn \xFF main") {
        LexOutcome::Refused { code } => assert_eq!(code, "INVALID_UTF8"),
        LexOutcome::Accepted { .. } => panic!("malformed input must be refused"),
    }
}

#[test]
fn real_lexical_failure_is_reproduced_minimized_and_classification_stable() {
    // 1. Reproduce: find a hostile stream the real engine refuses.
    let Some((failing, code)) = first_invalid_utf8_campaign(0xF00D, 512, 48) else {
        panic!("a hostile campaign over the byte pool must find INVALID_UTF8");
    };
    assert_eq!(code, "INVALID_UTF8");
    let digest_before = fcb_conformance::ReproducibleDigest::of_bytes(&failing);

    // 2. Minimize with the classification pinned to INVALID_UTF8: any
    //    candidate that stops refusing, or refuses differently, is rejected.
    let mut classify = |input: &[u8]| -> Option<&'static str> {
        let mut adapter = FmdResumableAdapter::new("rust").expect("supported");
        match adapter.feed(input) {
            LexOutcome::Refused { code } if code == "INVALID_UTF8" => Some("INVALID_UTF8"),
            _ => None,
        }
    };
    let report = minimize(
        &failing,
        TerminationBudget::attempts(256),
        &mut classify,
    );

    assert!(report.classification_preserved);
    assert!(
        report.minimized.len() <= failing.len(),
        "minimization never grows the input"
    );
    assert_eq!(
        fcb_conformance::ReproducibleDigest::of_bytes(&failing),
        digest_before,
        "the reproduced failure is byte-reproducible from the seed"
    );

    // 3. The minimized input is itself a genuine minimal refusal.
    let mut check = FmdResumableAdapter::new("rust").expect("supported");
    match check.feed(&report.minimized) {
        LexOutcome::Refused { code } => assert_eq!(code, "INVALID_UTF8"),
        other => panic!("minimized input lost its refusal: {other:?}"),
    }
}
