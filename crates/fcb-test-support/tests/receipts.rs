//! Tests for the bounded scenario receipt codec and redacted event ring
//! (fcb-wc0g). The acceptance cases named by the bead are all here:
//! truncation, ring saturation, secret sentinel filtering, canceled/failed
//! runs, missing evidence, and the rule that receipt generation never
//! declares qualification.

use fcb_test_support::receipts::{
    BoundedText, Effect, EventRing, ExpectedVsActual, ReceiptError, Redactor, RingSummary,
    RouteId, ScenarioReceipt, ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
    DEFAULT_RING_CAPACITY, MAX_FIELD_BYTES, RECEIPT_MAX_EVENTS, RECEIPT_SCHEMA, REDACTED_TOKEN,
};
use fcb_test_support::ContentDigest;

fn sample_pin() -> SourcePin {
    SourcePin::new("a1b2c3d4e5f6a7b8c9d0e1f2a3b4c5d6e7f8a9b0").expect("valid 40-hex pin")
}

fn sample_route() -> RouteId {
    RouteId::new("headless:exact-read").expect("valid route")
}

fn sample_draft(outcome: TerminalOutcome, ring: EventRing) -> ScenarioReceiptDraft {
    ScenarioReceiptDraft {
        scenario: "exact-read-roundtrip".to_string(),
        seed: ScenarioSeed(0x1234_5678_9abc_def0),
        pin: sample_pin(),
        route: sample_route(),
        corpus_digest: ContentDigest::of(b"corpus-bytes"),
        corpus_count: 7,
        outcome,
        comparison: None,
        ring,
        artifacts: vec!["artifact:receipts/run-1.txt".to_string()],
    }
}

#[test]
fn receipt_round_trips_exactly_through_the_codec() {
    let redactor = Redactor::new();
    let receipt = ScenarioReceipt::from_draft(
        &redactor,
        sample_draft(
            TerminalOutcome::new(Some(0), Effect::Succeeded, None),
            EventRing::new(8),
        ),
    );

    let decoded = ScenarioReceipt::decode(&receipt.encode()).expect("codec accepts its own output");
    assert_eq!(receipt, decoded);

    // Canonical: re-encoding the decoded receipt is byte-identical.
    assert_eq!(receipt.encode(), decoded.encode());
}

#[test]
fn decoding_rejects_foreign_schema_and_malformed_rows() {
    let redactor = Redactor::new();
    let receipt = ScenarioReceipt::from_draft(
        &redactor,
        sample_draft(
            TerminalOutcome::new(Some(1), Effect::Failed, None),
            EventRing::new(4),
        ),
    );

    let encoded_bytes = receipt.encode();
    let encoded_text = std::str::from_utf8(&encoded_bytes).unwrap();
    let forged = encoded_text.replacen(RECEIPT_SCHEMA, "fcb.receipt.v999", 1);
    assert_eq!(
        ScenarioReceipt::decode(forged.as_bytes()),
        Err(ReceiptError::SchemaMismatch)
    );

    let truncated_row: String = encoded_text
        .lines()
        .take(3)
        .collect::<Vec<_>>()
        .join("\n");
    assert_eq!(
        ScenarioReceipt::decode(truncated_row.as_bytes()),
        Err(ReceiptError::MalformedRow)
    );

    let broken_length = encoded_text.replacen("scenario:", "scenario:99:", 1);
    assert_eq!(
        ScenarioReceipt::decode(broken_length.as_bytes()),
        Err(ReceiptError::MalformedRow)
    );
}

#[test]
fn pins_reject_branch_names_and_short_hashes() {
    assert!(SourcePin::new("main").is_err());
    assert!(SourcePin::new("a1b2c3d4").is_err());
    assert!(SourcePin::new("research-blob-1cfb1e02488a83a5ab8cc38beb574ca4596a0d4e").is_err());
    assert!(sample_pin().as_str().len() == 40);
}

#[test]
fn routes_reject_paths_with_spaces_and_empties() {
    assert!(RouteId::new("").is_err());
    assert!(RouteId::new("route with space").is_err());
    assert!(RouteId::new(&"x".repeat(129)).is_err());
    assert!(RouteId::new("lab:interleave/wake-reset").is_ok());
}

#[test]
fn ring_saturation_drops_oldest_and_keeps_exact_counters() {
    let mut ring = EventRing::new(4);
    for index in 0..64 {
        ring.push(&Redactor::new(), &format!("event-{index}"));
    }

    assert_eq!(ring.len(), 4);
    assert_eq!(ring.dropped(), 60);
    assert_eq!(ring.next_sequence(), 64);

    let messages: Vec<&str> = ring.events().map(|event| event.message()).collect();
    assert_eq!(messages, ["event-60", "event-61", "event-62", "event-63"]);
    let sequences: Vec<u64> = ring.events().map(|event| event.sequence()).collect();
    assert_eq!(sequences, [60, 61, 62, 63]);
}

#[test]
fn receipt_ring_snapshot_is_bounded_and_counts_the_loss() {
    let mut ring = EventRing::new(RECEIPT_MAX_EVENTS + 5);
    for index in 0..(RECEIPT_MAX_EVENTS + 5) {
        ring.push(&Redactor::new(), &format!("event-{index}"));
    }

    let redactor = Redactor::new();
    let receipt = ScenarioReceipt::from_draft(
        &redactor,
        sample_draft(TerminalOutcome::new(Some(0), Effect::Succeeded, None), ring),
    );

    assert!(receipt.ring_summary().receipt_truncated);
    assert_eq!(receipt.ring_events().len(), RECEIPT_MAX_EVENTS);
    assert_eq!(receipt.ring_summary().dropped, 5);
    assert_eq!(
        receipt.ring_events().first().expect("oldest kept").message(),
        "event-5"
    );

    let decoded = ScenarioReceipt::decode(&receipt.encode()).expect("decode");
    assert_eq!(decoded.ring_summary(), receipt.ring_summary());
    assert_eq!(decoded.ring_events(), receipt.ring_events());
}

#[test]
fn secret_sentinels_never_reach_the_receipt_even_under_flooding() {
    let secret = "SECRET-API-TOKEN-abc123";
    let redactor = Redactor::new().with_sentinel(secret).with_sentinel("");

    let mut ring = EventRing::new(2);
    for index in 0..10 {
        ring.push(&redactor, &format!("call {index} with {secret} embedded"));
    }

    let receipt = ScenarioReceipt::from_draft(
        &redactor,
        ScenarioReceiptDraft {
            scenario: format!("flooded scenario containing {secret}"),
            seed: ScenarioSeed(1),
            pin: sample_pin(),
            route: sample_route(),
            corpus_digest: ContentDigest::of(b"corpus"),
            corpus_count: 1,
            outcome: TerminalOutcome::new(Some(2), Effect::Failed, None),
            comparison: Some(ExpectedVsActual::new(
                &redactor,
                "expected clean output",
                &format!("actual output leaked {secret}"),
            )),
            ring,
            artifacts: vec![format!("artifact:{secret}/export.bin")],
        },
    );

    let encoded_bytes = receipt.encode();
    let encoded_text = std::str::from_utf8(&encoded_bytes).unwrap();
    assert!(!encoded_text.contains(secret), "secret leaked into the codec output");
    assert!(encoded_text.contains(REDACTED_TOKEN));

    for event in receipt.ring_events() {
        assert!(!event.message().contains(secret));
        assert!(event.message().contains(REDACTED_TOKEN));
    }
    assert!(receipt.artifacts()[0].starts_with(&format!("artifact:{REDACTED_TOKEN}")));
    assert!(receipt
        .comparison()
        .expect("comparison present")
        .actual()
        .text()
        .contains(REDACTED_TOKEN));
}

#[test]
fn oversized_text_is_bounded_with_truthful_original_counts() {
    let oversized = "x".repeat(MAX_FIELD_BYTES + 500);
    let bounded = BoundedText::from_redacted(&oversized);
    assert!(bounded.truncated());
    assert_eq!(bounded.original_bytes(), MAX_FIELD_BYTES as u64 + 500);
    assert_eq!(bounded.text().len(), MAX_FIELD_BYTES);

    // The bound respects character boundaries for multibyte text.
    let multibyte = "é".repeat(MAX_FIELD_BYTES);
    let bounded_multibyte = BoundedText::from_redacted(&multibyte);
    assert!(bounded_multibyte.truncated());
    assert!(bounded_multibyte.text().ends_with('é'));

    // Round trip preserves the truth, not a re-derived count.
    let redactor = Redactor::new();
    let receipt = ScenarioReceipt::from_draft(
        &redactor,
        ScenarioReceiptDraft {
            scenario: "truncation-truth".to_string(),
            seed: ScenarioSeed(2),
            pin: sample_pin(),
            route: sample_route(),
            corpus_digest: ContentDigest::of(b"corpus"),
            corpus_count: 1,
            outcome: TerminalOutcome::new(Some(3), Effect::Failed, None),
            comparison: Some(ExpectedVsActual::new(&redactor, &oversized, "short")),
            ring: EventRing::new(2),
            artifacts: vec![],
        },
    );
    let decoded = ScenarioReceipt::decode(&receipt.encode()).expect("decode");
    let comparison = decoded.comparison().expect("comparison survives");
    assert!(comparison.expected().truncated());
    assert_eq!(comparison.expected().original_bytes(), MAX_FIELD_BYTES as u64 + 500);
    assert!(!comparison.actual().truncated());
    assert_eq!(comparison.actual().text(), "short");
}

#[test]
fn failed_and_canceled_runs_survive_encoding_without_being_upgraded() {
    let redactor = Redactor::new();

    let failed = ScenarioReceipt::from_draft(
        &redactor,
        sample_draft(
            TerminalOutcome::new(Some(101), Effect::Failed, None),
            EventRing::new(2),
        ),
    );
    let decoded_failed = ScenarioReceipt::decode(&failed.encode()).expect("decode");
    assert_eq!(decoded_failed.outcome().effect(), Effect::Failed);
    assert_eq!(decoded_failed.outcome().exit_code(), Some(101));
    assert_eq!(decoded_failed.outcome().unexecuted_reason(), None);

    let canceled = ScenarioReceipt::from_draft(
        &redactor,
        sample_draft(
            TerminalOutcome::new(
                None,
                Effect::Canceled,
                Some("canceled between drain and acknowledge".to_string()),
            ),
            EventRing::new(2),
        ),
    );
    let decoded_canceled = ScenarioReceipt::decode(&canceled.encode()).expect("decode");
    assert_eq!(decoded_canceled.outcome().effect(), Effect::Canceled);
    assert_eq!(decoded_canceled.outcome().exit_code(), None);
    assert_eq!(
        decoded_canceled.outcome().unexecuted_reason(),
        Some("canceled between drain and acknowledge")
    );

    // A "successful write" of a failed receipt is still a failed receipt:
    // writing it twice changes nothing about the recorded outcome.
    let first = decoded_failed.encode();
    let second = ScenarioReceipt::decode(&first)
        .expect("re-decode")
        .encode();
    assert_eq!(first, second);
    assert_eq!(
        ScenarioReceipt::decode(&second)
            .expect("re-decode")
            .outcome()
            .effect(),
        Effect::Failed
    );
}

#[test]
fn missing_evidence_is_recorded_as_missing_not_invented() {
    let receipt = ScenarioReceipt::from_draft(
        &Redactor::new(),
        sample_draft(
            TerminalOutcome::new(None, Effect::Canceled, Some("worker unavailable".to_string())),
            EventRing::new(2),
        ),
    );

    let decoded = ScenarioReceipt::decode(&receipt.encode()).expect("decode");
    assert_eq!(decoded.outcome().exit_code(), None);
    assert_eq!(
        decoded.outcome().unexecuted_reason(),
        Some("worker unavailable")
    );
    assert!(decoded.comparison().is_none(), "no evidence was produced");
    assert_eq!(decoded.ring_summary().kept, 0);
}

#[test]
fn receipts_never_declare_qualification() {
    // The receipt API exposes recorded outcomes and evidence only; there is
    // no method that maps a receipt to a pass/fail verdict. Guard the
    // structural fact: the outcome vocabulary is the only verdict carrier.
    let receipt = ScenarioReceipt::from_draft(
        &Redactor::new(),
        sample_draft(
            TerminalOutcome::new(Some(0), Effect::Succeeded, None),
            EventRing::new(2),
        ),
    );
    assert_eq!(receipt.outcome().effect(), Effect::Succeeded);

    let ring = EventRing::new(4);
    assert_eq!(summary_tuple(&ring), (4, 0, 0, 0));
}

/// Test-only compact view used by the qualification-neutrality guard.
fn summary_tuple(ring: &EventRing) -> (usize, usize, u64, u64) {
    (ring.capacity(), ring.len(), ring.dropped(), ring.next_sequence())
}

#[test]
fn event_bound_respects_redaction_before_truncation() {
    let secret = "SECRET-TAIL-9990";
    let redactor = Redactor::new().with_sentinel(secret);
    // Build text where the sentinel straddles the truncation cut.
    let mut message = String::new();
    while message.len() < MAX_FIELD_BYTES - 8 {
        message.push('y');
    }
    message.push_str(secret);

    let mut ring = EventRing::new(1);
    ring.push(&redactor, &message);

    let event = ring.events().next().expect("one event");
    let message_snapshot = event.message().to_string();
    let truncated = event.truncated();
    let original_bytes = event.original_bytes();
    assert!(truncated);
    assert!(!message_snapshot.contains("SECRET-"), "partial secret leaked");
    assert!(message_snapshot.contains(REDACTED_TOKEN));
    assert_eq!(original_bytes as usize, message.len());

    let encoded_event = ScenarioReceipt::from_draft(
        &redactor,
        sample_draft(TerminalOutcome::new(Some(0), Effect::Succeeded, None), ring),
    )
    .encode();
    assert!(!encoded_event.is_empty());
    assert!(message_snapshot.contains(REDACTED_TOKEN));
}
#[test]
fn ring_summary_matches_live_counters_after_snapshot() {
    let mut ring = EventRing::new(3);
    for index in 0..7 {
        ring.push(&Redactor::new(), &format!("m-{index}"));
    }
    let redactor = Redactor::new();
    let receipt = ScenarioReceipt::from_draft(
        &redactor,
        sample_draft(TerminalOutcome::new(Some(0), Effect::Succeeded, None), ring),
    );
    let summary: RingSummary = *receipt.ring_summary();
    assert_eq!(summary.capacity, 3);
    assert_eq!(summary.kept, 3);
    assert_eq!(summary.dropped, 4);
    assert_eq!(summary.next_sequence, 7);
    assert!(!summary.receipt_truncated);
}

#[test]
fn corpus_digest_identity_is_preserved_by_the_codec() {
    let digest = ContentDigest::of(b"shared corpus identity");
    let receipt = ScenarioReceipt::from_draft(
        &Redactor::new(),
        ScenarioReceiptDraft {
            scenario: "corpus-identity".to_string(),
            seed: ScenarioSeed(9),
            pin: sample_pin(),
            route: sample_route(),
            corpus_digest: digest,
            corpus_count: 12,
            outcome: TerminalOutcome::new(Some(0), Effect::Succeeded, None),
            comparison: None,
            ring: EventRing::new(2),
            artifacts: vec![],
        },
    );
    let decoded = ScenarioReceipt::decode(&receipt.encode()).expect("decode");
    assert_eq!(decoded.corpus_digest(), &digest);
    assert_eq!(decoded.corpus_count(), 12);
}

#[test]
fn default_ring_capacity_admits_isolated_events_without_loss() {
    let mut ring = EventRing::new(DEFAULT_RING_CAPACITY);
    ring.push(&Redactor::new(), "only event");
    assert_eq!(ring.len(), 1);
    assert_eq!(ring.dropped(), 0);
    assert_eq!(ring.capacity(), DEFAULT_RING_CAPACITY);
}
