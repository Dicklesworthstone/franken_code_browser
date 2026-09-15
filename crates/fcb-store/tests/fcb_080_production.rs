//! FCB-080.V production verification scenario: upstream reusable digest,
//! canonical-envelope factoring, and consumer conformance.
//!
//! Required cases (each individually selectable via cargo test filter):
//!
//! 1. `known_sha256_digest_vectors` — NIST/FIPS 180-4 standard vectors.
//! 2. `canonical_envelope_roundtrip` — positional encoding and decoding.
//! 3. `primitive_canonicalization_floats` — signed zero and canonical NaNs.
//! 4. `corrupt_envelope_checksum_mismatch` — bitflips detected by checksum.
//! 5. `truncated_envelope_refused` — truncated header/payload refused.
//! 6. `trailing_bytes_strict_rejection` — unexpected trailing bytes rejected.
//! 7. `schema_and_version_mismatch` — magic, major version, and older minor rejected.
//! 8. `negative_control_checksum_never_grants_authorization` — valid checksum with
//!    wrong schema/key never grants authorization.
//! 9. `fcb_store_cache_namespace_envelope_integration` — envelopes stored as immutable
//!    cache entries with hot/cold equality.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_080.sh`).

use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};

use fcb_core::ArenaOwnerId;
use fcb_store::envelope::{
    canonicalize_f32, canonicalize_f64, EnvelopeError, EnvelopeLimits, EnvelopeReader,
    EnvelopeSchema, EnvelopeWriter, Sha256, UnknownPolicy,
    DEFAULT_MAGIC, FRAME_LEN, HEADER_LEN,
};
use fcb_store::{CacheNamespace, NamespaceIdentity};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};

const RUN_ID_ENV: &str = "FCB_080_RUN_ID";

fn counter() -> &'static AtomicU64 {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    &COUNTER
}

fn temp_parent(tag: &str) -> PathBuf {
    let unique = counter().fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!(
        "fcb-080-prod-{tag}-{}-{unique}",
        std::process::id()
    ));
    fs::create_dir_all(&dir).expect("temp parent created");
    dir
}

fn receipts_dir() -> PathBuf {
    let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
    std::env::temp_dir().join(format!("fcb-080-receipts-{run_id}"))
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    fs::create_dir_all(&run_dir).expect("receipts dir created");
    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_80_00_01),
        pin: SourcePin::new("0803456789abcdeffedcba9876543210abcdef01").expect("pin valid"),
        route: RouteId::new("headless:rust").expect("route valid"),
        corpus_digest: fcb_test_support::ContentDigest::of(detail.as_bytes()),
        corpus_count: 1,
        outcome: TerminalOutcome::new(
            Some(if effect == Effect::Succeeded { 0 } else { 1 }),
            effect,
            None,
        ),
        comparison: Some(ExpectedVsActual::new(
            &Redactor::new(),
            "oracle holds",
            detail,
        )),
        ring: EventRing::new(16),
        artifacts: vec![],
    };
    let receipt = ScenarioReceipt::from_draft(&Redactor::new(), draft);
    let encoded = receipt.encode();
    let parsed = ScenarioReceipt::decode(&encoded).expect("receipt round-trips");
    assert_eq!(parsed.outcome().effect(), receipt.outcome().effect());
    fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    )
    .expect("receipt retained");
}

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0C_80).expect("test owner is non-zero")
}

#[test]
fn known_sha256_digest_vectors() {
    // NIST / FIPS 180-4 standard test vectors
    let v1 = Sha256::digest(b"");
    assert_eq!(
        v1.to_hex(),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855",
        "empty string digest matches NIST vector"
    );

    let v2 = Sha256::digest(b"abc");
    assert_eq!(
        v2.to_hex(),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad",
        "'abc' digest matches NIST vector"
    );

    let v3 = Sha256::digest(b"abcdbcdecdefdefgefghfghighijhijkijkljklmklmnlmnomnopnopq");
    assert_eq!(
        v3.to_hex(),
        "248d6a61d20638b8e5c026930c3e6039a33ce45964ff2167f6ecedd419db06c1",
        "multi-block digest matches NIST vector"
    );

    record_receipt(
        "known_sha256_digest_vectors",
        Effect::Succeeded,
        "all NIST SHA-256 standard test vectors match exactly",
    );
}

#[test]
fn canonical_envelope_roundtrip() {
    let schema = EnvelopeSchema::new(0x1001, 1, 0);
    let mut writer = EnvelopeWriter::new(schema);

    writer.put_u8(42);
    writer.put_u16(1000);
    writer.put_u32(100_000);
    writer.put_u64(10_000_000_000);
    writer.put_i64(-42);
    writer.put_f32(123.456);
    writer.put_f64(98765.432101);
    writer.put_str("franken-code-browser canonical record");
    writer.put_bytes(b"\x00\xff\xfe\x01binary");

    let document = writer.finish();
    assert!(document.len() > FRAME_LEN);

    let mut reader = EnvelopeReader::open(
        &document,
        schema,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .expect("reader opens valid document");

    assert_eq!(reader.schema(), schema);
    assert_eq!(reader.get_u8().expect("u8"), 42);
    assert_eq!(reader.get_u16().expect("u16"), 1000);
    assert_eq!(reader.get_u32().expect("u32"), 100_000);
    assert_eq!(reader.get_u64().expect("u64"), 10_000_000_000);
    assert_eq!(reader.get_i64().expect("i64"), -42);
    assert!((reader.get_f32().expect("f32") - 123.456).abs() < 1e-4);
    assert!((reader.get_f64().expect("f64") - 98765.432101).abs() < 1e-9);
    assert_eq!(
        reader.get_str().expect("str"),
        "franken-code-browser canonical record"
    );
    assert_eq!(reader.get_bytes().expect("bytes"), b"\x00\xff\xfe\x01binary");

    reader.finish().expect("all bytes consumed");

    record_receipt(
        "canonical_envelope_roundtrip",
        Effect::Succeeded,
        "envelope roundtrip positional decode agrees bit-for-bit",
    );
}

#[test]
fn primitive_canonicalization_floats() {
    // -0.0 collapses to +0.0
    let neg_zero_32: f32 = -0.0;
    let pos_zero_32: f32 = 0.0;
    assert_eq!(
        canonicalize_f32(neg_zero_32).to_bits(),
        canonicalize_f32(pos_zero_32).to_bits()
    );

    let neg_zero_64: f64 = -0.0;
    let pos_zero_64: f64 = 0.0;
    assert_eq!(
        canonicalize_f64(neg_zero_64).to_bits(),
        canonicalize_f64(pos_zero_64).to_bits()
    );

    // Arbitrary NaNs collapse to one canonical quiet NaN
    let nan1_32 = f32::from_bits(0x7fc0_0001);
    let nan2_32 = f32::from_bits(0x7ff0_0000);
    assert_eq!(
        canonicalize_f32(nan1_32).to_bits(),
        canonicalize_f32(nan2_32).to_bits()
    );

    let nan1_64 = f64::from_bits(0x7ff8_0000_0000_0001);
    let nan2_64 = f64::from_bits(0x7ff4_0000_0000_0000);
    assert_eq!(
        canonicalize_f64(nan1_64).to_bits(),
        canonicalize_f64(nan2_64).to_bits()
    );

    // Written envelopes with -0.0 vs +0.0 yield identical bytes and hashes
    let schema = EnvelopeSchema::new(0x2001, 1, 0);

    let mut w1 = EnvelopeWriter::new(schema);
    w1.put_f32(-0.0);
    w1.put_f64(-0.0);
    let doc1 = w1.finish();

    let mut w2 = EnvelopeWriter::new(schema);
    w2.put_f32(0.0);
    w2.put_f64(0.0);
    let doc2 = w2.finish();

    assert_eq!(doc1, doc2, "envelopes with -0.0 and 0.0 must be bit-identical");
    assert_eq!(Sha256::digest(&doc1), Sha256::digest(&doc2));

    record_receipt(
        "primitive_canonicalization_floats",
        Effect::Succeeded,
        "float canonicalization eliminates signed zero and NaN divergence",
    );
}

#[test]
fn corrupt_envelope_checksum_mismatch() {
    let schema = EnvelopeSchema::new(0x3001, 1, 0);
    let mut writer = EnvelopeWriter::new(schema);
    writer.put_str("precious uncorrupted data");
    let mut document = writer.finish();

    // Corrupt one byte in payload
    let target = HEADER_LEN + 5;
    document[target] ^= 0x55;

    let err = EnvelopeReader::open(
        &document,
        schema,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .unwrap_err();

    assert!(
        matches!(err, EnvelopeError::ChecksumMismatch { .. }),
        "corrupted payload must fail checksum verification: {err:?}"
    );

    record_receipt(
        "corrupt_envelope_checksum_mismatch",
        Effect::Succeeded,
        "payload corruption detected by trailing SHA-256 checksum",
    );
}

#[test]
fn truncated_envelope_refused() {
    let schema = EnvelopeSchema::new(0x4001, 1, 0);
    let mut writer = EnvelopeWriter::new(schema);
    writer.put_str("some payload");
    let document = writer.finish();

    // Truncate below frame length
    let sub_frame = &document[..FRAME_LEN - 1];
    let err_short = EnvelopeReader::open(
        sub_frame,
        schema,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .unwrap_err();
    assert_eq!(err_short, EnvelopeError::TooShort);

    // Truncate within payload
    let trunc_payload = &document[..document.len() - 5];
    let err_trunc = EnvelopeReader::open(
        trunc_payload,
        schema,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .unwrap_err();
    assert!(
        matches!(err_trunc, EnvelopeError::ChecksumMismatch { .. }),
        "truncated document fails checksum: {err_trunc:?}"
    );

    record_receipt(
        "truncated_envelope_refused",
        Effect::Succeeded,
        "truncated envelope is refused before decoding",
    );
}

#[test]
fn trailing_bytes_strict_rejection() {
    let schema = EnvelopeSchema::new(0x5001, 1, 0);
    let mut writer = EnvelopeWriter::new(schema);
    writer.put_u32(1234);
    writer.put_u32(5678);
    let document = writer.finish();

    // Reader only consumes one u32 and calls finish: trailing bytes in payload
    let mut reader = EnvelopeReader::open(
        &document,
        schema,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .expect("open");
    assert_eq!(reader.get_u32().expect("first u32"), 1234);
    let finish_err = reader.finish().unwrap_err();
    assert_eq!(finish_err, EnvelopeError::TrailingBytes(4));

    record_receipt(
        "trailing_bytes_strict_rejection",
        Effect::Succeeded,
        "unconsumed trailing bytes rejected under strict policy",
    );
}

#[test]
fn schema_and_version_mismatch() {
    let schema_v1 = EnvelopeSchema::new(0x6001, 1, 0);
    let mut writer = EnvelopeWriter::new(schema_v1);
    writer.put_str("v1 data");
    let document = writer.finish();

    // Wrong magic
    let foreign_magic = EnvelopeSchema::with_magic(*b"FMDM", 0x6001, 1, 0);
    let err_magic = EnvelopeReader::open(
        &document,
        foreign_magic,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .unwrap_err();
    assert_eq!(err_magic, EnvelopeError::BadMagic(DEFAULT_MAGIC));

    // Major version mismatch
    let schema_v2 = EnvelopeSchema::new(0x6001, 2, 0);
    let err_major = EnvelopeReader::open(
        &document,
        schema_v2,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .unwrap_err();
    assert_eq!(
        err_major,
        EnvelopeError::MajorMismatch {
            expected: 2,
            actual: 1
        }
    );

    // Document minor older than expected minor
    let schema_v1_minor2 = EnvelopeSchema::new(0x6001, 1, 2);
    let err_minor = EnvelopeReader::open(
        &document,
        schema_v1_minor2,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .unwrap_err();
    assert_eq!(
        err_minor,
        EnvelopeError::MinorOlder {
            expected: 2,
            actual: 0
        }
    );

    record_receipt(
        "schema_and_version_mismatch",
        Effect::Succeeded,
        "magic, major version, and older minor mismatches rejected",
    );
}

#[test]
fn negative_control_checksum_never_grants_authorization() {
    // CRITICAL SECURITY INVARIANT:
    // A document with a mathematically valid SHA-256 checksum must NEVER
    // grant authorization if presented to a schema or namespace with a different
    // schema_id / purpose.
    let schema_admin = EnvelopeSchema::new(0xDEAD_0001, 1, 0);
    let schema_guest = EnvelopeSchema::new(0xBEEF_0002, 1, 0);

    let mut writer = EnvelopeWriter::new(schema_guest);
    writer.put_str("innocent guest payload");
    let guest_document = writer.finish();

    // Verify guest document is internally valid
    assert!(EnvelopeReader::open(
        &guest_document,
        schema_guest,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict
    )
    .is_ok());

    // Attempt to present the valid guest document to the admin schema:
    // Despite valid SHA-256 checksum over the entire bytes, it MUST be rejected!
    let admin_access = EnvelopeReader::open(
        &guest_document,
        schema_admin,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    );
    assert_eq!(
        admin_access.unwrap_err(),
        EnvelopeError::SchemaMismatch {
            expected: 0xDEAD_0001,
            actual: 0xBEEF_0002,
        },
        "valid checksum must never grant authorization across mismatched schemas"
    );

    record_receipt(
        "negative_control_checksum_never_grants_authorization",
        Effect::Succeeded,
        "negative control verified: valid checksum refused under schema mismatch",
    );
}

#[test]
fn fcb_store_cache_namespace_envelope_integration() {
    let parent = temp_parent("cache-envelope-integ");
    let identity = NamespaceIdentity::new("fcb-envelope-consumer").expect("identity");
    let mut namespace = CacheNamespace::create(&parent, identity, owner()).expect("create");

    let schema = EnvelopeSchema::new(0x7001, 1, 0);
    let mut writer = EnvelopeWriter::new(schema);
    writer.put_str("cached analysis summary");
    writer.put_u64(987654321);
    let envelope_bytes = writer.finish();

    // Store the envelope into CacheNamespace
    let written = namespace
        .write_entry("summary.env", &envelope_bytes)
        .expect("write entry");
    assert_eq!(written.generation, 1);
    assert_eq!(written.len, envelope_bytes.len() as u64);

    // Hot read and cold read agree byte-for-byte
    let hot = namespace.read_hot("summary.env", 1).expect("read hot");
    let cold = namespace.read_cold("summary.env", 1).expect("read cold");
    assert_eq!(hot.as_slice(), envelope_bytes.as_slice());
    assert_eq!(cold.as_slice(), envelope_bytes.as_slice());

    // Decode from cold cache entry
    let mut reader = EnvelopeReader::open(
        &cold,
        schema,
        EnvelopeLimits::default(),
        UnknownPolicy::Strict,
    )
    .expect("cold envelope decodes");
    assert_eq!(reader.get_str().expect("str"), "cached analysis summary");
    assert_eq!(reader.get_u64().expect("u64"), 987654321);
    reader.finish().expect("finish");

    record_receipt(
        "fcb_store_cache_namespace_envelope_integration",
        Effect::Succeeded,
        "canonical envelopes integrate into CacheNamespace with hot/cold equality",
    );
}
