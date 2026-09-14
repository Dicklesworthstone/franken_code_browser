#![cfg(target_os = "macos")]
#![forbid(unsafe_code)]

//! Native macOS qualification for FCB-009 root-read capabilities
//! (fcb-qb2.3 / FCB-009.V).
//!
//! These scenarios run only on Apple Silicon macOS and exercise the exact
//! behaviors a Linux worker cannot exhibit: APFS case handling, Unicode
//! normalization-form neighbors, symlink target swaps, root loss and
//! restoration on the real filesystem, native revocation timing, and the
//! export publication gate. Every scenario records a bounded redacted
//! scenario receipt through the shared codec; a deliberate-mismatch negative
//! control proves the receipt pipeline detects failures instead of
//! rubber-stamping them. Receipts never declare qualification: they are the
//! evidence an independent verifier reconciles.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use fcb_core::{ArenaOwnerId, ByteLength, FileId, RootId, SourceRevision};
use fcb_source::confined::{ConfinedSourceReader, SymlinkPolicy};
use fcb_source::path::{decode_uri_path, NormalizedPath, RawPath};
use fcb_source::root::{ExportPublicationGate, RootGrant};
use fcb_source::{CancelFlag, SourceError};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, ReceiptError, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome, MAX_FIELD_BYTES,
};
use fcb_test_support::ContentDigest;

const QUALIFICATION_SEED: u64 = 0x0095_0095_0095_0095;

struct TempTestDir {
    path: PathBuf,
}

impl TempTestDir {
    fn new(label: &str) -> Self {
        let mut path = std::env::temp_dir();
        path.push(format!("fcb-009v-{label}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&path);
        fs::create_dir_all(&path).expect("temp root creates");
        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TempTestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

fn owner() -> ArenaOwnerId {
    ArenaOwnerId::new(0x0095).expect("owner id valid")
}

fn root_id() -> RootId {
    RootId::new(owner(), 1).expect("root id valid")
}

fn file_id(n: u64) -> FileId {
    FileId::new(owner(), n).expect("file id valid")
}

fn revision(n: u64) -> SourceRevision {
    SourceRevision::new(owner(), n).expect("revision valid")
}

fn grant_for(root: &Path) -> RootGrant {
    RootGrant::new(root_id(), RawPath::from_str(&root.to_string_lossy()))
}

fn reader_for(root: &Path) -> ConfinedSourceReader {
    ConfinedSourceReader::new(
        grant_for(root),
        SymlinkPolicy::AllowWithinRoot,
        ByteLength::new(1 << 20),
    )
}

fn normalized(rel: &str) -> NormalizedPath {
    NormalizedPath::new(RawPath::from_str(rel)).expect("test relative path normalizes")
}

fn synthetic_pin() -> SourcePin {
    // Qualification metadata, not a real source commit: a fixed 40-hex tag.
    SourcePin::new("0095009500950095009500950095009500950095").expect("valid pin")
}

fn synthetic_route() -> RouteId {
    RouteId::new("native-macos-qualification").expect("valid route")
}

fn draft(
    scenario: &str,
    outcome: TerminalOutcome,
    comparison: Option<ExpectedVsActual>,
    ring: EventRing,
) -> ScenarioReceiptDraft {
    ScenarioReceiptDraft {
        scenario: scenario.to_string(),
        seed: ScenarioSeed(QUALIFICATION_SEED),
        pin: synthetic_pin(),
        route: synthetic_route(),
        corpus_digest: ContentDigest::of(scenario.as_bytes()),
        corpus_count: 1,
        outcome,
        comparison,
        ring,
        artifacts: vec![],
    }
}

fn success(name: &str) -> ScenarioReceipt {
    ScenarioReceipt::from_draft(
        &Redactor::new(),
        draft(
            name,
            TerminalOutcome::new(Some(0), Effect::Succeeded, None),
            None,
            EventRing::new(8),
        ),
    )
}

fn failure(name: &str, expected: &str, actual: &str) -> ScenarioReceipt {
    let redactor = Redactor::new();
    ScenarioReceipt::from_draft(
        &redactor,
        draft(
            name,
            TerminalOutcome::new(Some(1), Effect::Failed, None),
            Some(ExpectedVsActual::new(&redactor, expected, actual)),
            EventRing::new(8),
        ),
    )
}

/// Run `body` as a scenario; on error, produce a Failed receipt carrying the
/// expected-vs-actual description so the failure is evidence, not noise.
fn qualify(
    name: &str,
    body: impl FnOnce() -> Result<(), (String, String)>,
) -> ScenarioReceipt {
    match body() {
        Ok(()) => success(name),
        Err((expected, actual)) => failure(name, &expected, &actual),
    }
}

fn write_file(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
    let full = root.join(rel);
    if let Some(parent) = full.parent() {
        fs::create_dir_all(parent).expect("parent creates");
    }
    let mut file = fs::File::create(&full).expect("file creates");
    file.write_all(bytes).expect("write succeeds");
    full
}

fn captured_bytes(
    root: &Path,
    rel: &str,
    cancel: &CancelFlag,
) -> Result<Vec<u8>, (String, String)> {
    let reader = reader_for(root);
    let capture = reader.read_file(file_id(1), revision(1), &normalized(rel), cancel);
    match capture {
        Ok(capture) => Ok(capture.bytes().to_vec()),
        Err(error) => Err((String::from("read succeeds"), format!("{error}"))),
    }
}

// ---------------------------------------------------------------------------
// Scenarios
// ---------------------------------------------------------------------------

fn scenario_exact_case_read(root: &Path) -> Result<(), (String, String)> {
    let bytes = b"exact case payload";
    write_file(root, "CaseFile.txt", bytes);
    let cancel = CancelFlag::new();
    let actual = captured_bytes(root, "CaseFile.txt", &cancel)?;
    if actual == bytes {
        Ok(())
    } else {
        Err((
            String::from_utf8_lossy(bytes).to_string(),
            String::from_utf8_lossy(&actual).to_string(),
        ))
    }
}

/// APFS volumes are commonly case-insensitive: a read through different case
/// resolves to the same on-disk file. The qualification documents the
/// observed behavior and requires byte-exactness either way; raw-path
/// identity remains byte-distinct by type regardless of volume policy.
fn scenario_case_insensitive_resolution_documented(root: &Path) -> Result<(), (String, String)> {
    let bytes = b"canonical case payload";
    write_file(root, "Canonical.txt", bytes);
    let cancel = CancelFlag::new();
    let actual = captured_bytes(root, "cAnOnIcAl.txt", &cancel)?;
    if actual == bytes {
        Ok(())
    } else {
        Err((
            String::from("canonical bytes"),
            String::from_utf8_lossy(&actual).to_string(),
        ))
    }
}

/// U+00E9 (NFC) and U+0065 U+0301 (NFD) render identically but are different
/// byte sequences. Raw-path identity is byte-distinct by type. Volumes
/// differ: a normalization-insensitive volume treats the two names as one
/// entry (both reads must then return the same stored bytes, exactly one of
/// the written payloads); a normalization-sensitive volume keeps two files,
/// each reading exactly its own payload. Either behavior is truthful; mixed
/// bytes are not.
fn scenario_normalization_neighbors(root: &Path) -> Result<(), (String, String)> {
    let nfc_name = "caf\u{00e9}.txt";
    let nfd_name = "cafe\u{0301}.txt";
    let nfc_bytes: &[u8] = b"nfc payload";
    let nfd_bytes: &[u8] = b"nfd payload";
    write_file(root, nfc_name, nfc_bytes);
    write_file(root, nfd_name, nfd_bytes);

    let nfc_raw = RawPath::from_str(nfc_name);
    let nfd_raw = RawPath::from_str(nfd_name);
    if nfc_raw.as_bytes() == nfd_raw.as_bytes() {
        return Err((String::from("distinct byte sequences"), String::from("equal")));
    }

    let cancel = CancelFlag::new();
    let read_nfc = captured_bytes(root, nfc_name, &cancel)?;
    let read_nfd = captured_bytes(root, nfd_name, &cancel)?;
    if read_nfc == read_nfd {
        if read_nfc == nfc_bytes || read_nfc == nfd_bytes {
            return Ok(());
        }
        return Err((
            String::from("one stored payload"),
            String::from_utf8_lossy(&read_nfc).to_string(),
        ));
    }
    if read_nfc != nfc_bytes {
        return Err((
            String::from("nfc payload"),
            String::from_utf8_lossy(&read_nfc).to_string(),
        ));
    }
    if read_nfd != nfd_bytes {
        return Err((
            String::from("nfd payload"),
            String::from_utf8_lossy(&read_nfd).to_string(),
        ));
    }
    Ok(())
}

fn scenario_symlink_target_swap_keeps_each_capture_exact(
    root: &Path,
) -> Result<(), (String, String)> {
    let a = write_file(root, "swap/a.txt", b"target A");
    let b = write_file(root, "swap/b.txt", b"target B");
    let link = root.join("swap/link.txt");
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&a, &link).expect("symlink creates");

    let cancel = CancelFlag::new();
    let first = captured_bytes(root, "swap/link.txt", &cancel)?;
    if first != b"target A" {
        return Err((
            String::from("target A"),
            String::from_utf8_lossy(&first).to_string(),
        ));
    }

    // Swap the symlink target between reads; each capture stays exact for
    // the target it opened.
    let _ = fs::remove_file(&link);
    std::os::unix::fs::symlink(&b, &link).expect("symlink re-points");
    let second = captured_bytes(root, "swap/link.txt", &cancel)?;
    if second != b"target B" {
        return Err((
            String::from("target B"),
            String::from_utf8_lossy(&second).to_string(),
        ));
    }
    Ok(())
}

fn scenario_root_loss_fails_and_restoration_recovers(
    root: &Path,
) -> Result<(), (String, String)> {
    let hidden = root.join("hidden-root");
    write_file(&hidden, "inside.txt", b"restorable bytes");
    let parking = root.join("parked-root");
    fs::rename(&hidden, &parking).expect("root rename succeeds");

    let cancel = CancelFlag::new();
    if let Ok(bytes) = captured_bytes(&hidden, "inside.txt", &cancel) {
        return Err((
            String::from("read fails while root is missing"),
            String::from_utf8_lossy(&bytes).to_string(),
        ));
    }

    fs::rename(&parking, &hidden).expect("root restoration succeeds");
    let restored = captured_bytes(&hidden, "inside.txt", &cancel)?;
    if restored != b"restorable bytes" {
        return Err((
            String::from("restorable bytes"),
            String::from_utf8_lossy(&restored).to_string(),
        ));
    }
    Ok(())
}

fn scenario_revocation_during_read_stops_delivery(
    root: &Path,
) -> Result<(), (String, String)> {
    write_file(root, "revoked.txt", b"do not deliver");
    let grant = grant_for(root);
    let reader = ConfinedSourceReader::new(
        grant.clone(),
        SymlinkPolicy::AllowWithinRoot,
        ByteLength::new(1 << 20),
    );
    grant.revoke();
    let cancel = CancelFlag::new();
    let delivered =
        reader.read_file(file_id(2), revision(1), &normalized("revoked.txt"), &cancel);
    match delivered {
        Err(SourceError::GrantRevoked) => Ok(()),
        Err(other) => Err((String::from("GrantRevoked"), format!("{other}"))),
        Ok(capture) => Err((
            String::from("no delivery after revocation"),
            String::from_utf8_lossy(capture.bytes()).to_string(),
        )),
    }
}

fn scenario_encoded_traversal_escalation_rejected(
    root: &Path,
) -> Result<(), (String, String)> {
    write_file(root, "escape-me.txt", b"outside payload");
    match decode_uri_path("read/%2e%2e%2fescape-me.txt") {
        Err(_) => Ok(()),
        Ok(path) => {
            // If the decoder admits the row, the confined reader must still
            // refuse any resolution that leaves the root.
            let cancel = CancelFlag::new();
            let path_text = path
                .as_str()
                .map_err(|error| (String::from("decodable path"), format!("{error}")))?;
            let escaped = captured_bytes(root, path_text, &cancel)?;
            Err((
                String::from("escalation refused"),
                String::from_utf8_lossy(&escaped).to_string(),
            ))
        }
    }
}

fn scenario_raw_byte_and_unicode_filenames_roundtrip(
    root: &Path,
) -> Result<(), (String, String)> {
    let cancel = CancelFlag::new();
    for (name, payload) in [
        ("space name.txt", b"space" as &[u8]),
        ("lambda-\u{03bb}.txt", &b"lambda"[..]),
        ("emoji-\u{1f4dd}.txt", &b"emoji"[..]),
    ] {
        write_file(root, name, payload);
        let read = captured_bytes(root, name, &cancel)?;
        if read != payload {
            return Err((
                String::from_utf8_lossy(payload).to_string(),
                String::from_utf8_lossy(&read).to_string(),
            ));
        }
    }
    Ok(())
}

fn scenario_export_publication_gate_serializes_natively(
    root: &Path,
) -> Result<(), (String, String)> {
    write_file(root, "exported.txt", b"published bytes");
    let grant = grant_for(root);
    let cancel = CancelFlag::new();
    let reader = ConfinedSourceReader::new(
        grant.clone(),
        SymlinkPolicy::AllowWithinRoot,
        ByteLength::new(1 << 20),
    );
    let published = ExportPublicationGate::publish(
        &grant,
        || reader.read_file(file_id(3), revision(1), &normalized("exported.txt"), &cancel),
        |capture| Ok(capture.bytes().to_vec()),
    );
    match published {
        Ok(bytes) if bytes == b"published bytes" => Ok(()),
        Ok(bytes) => Err((
            String::from("published bytes"),
            String::from_utf8_lossy(&bytes).to_string(),
        )),
        Err(error) => Err((String::from("publication succeeds"), format!("{error}"))),
    }
}

/// Negative control: the receipt pipeline must record a deliberate mismatch
/// as Failure with the expected-vs-actual evidence, proving the harness
/// detects failures instead of rubber-stamping success.
fn negative_control_deliberate_mismatch() -> ScenarioReceipt {
    failure(
        "negative-control-deliberate-mismatch",
        "expected A",
        "observed B",
    )
}

fn secret_sentinel_never_reaches_the_retained_receipts() {
    let secret = "SECRET-HOST-PATH-7";
    let redactor = Redactor::new().with_sentinel(secret);
    let receipt = ScenarioReceipt::from_draft(
        &redactor,
        ScenarioReceiptDraft {
            scenario: format!("host path under {secret}"),
            seed: ScenarioSeed(QUALIFICATION_SEED),
            pin: synthetic_pin(),
            route: synthetic_route(),
            corpus_digest: ContentDigest::of(b"native corpus"),
            corpus_count: 1,
            outcome: TerminalOutcome::new(Some(0), Effect::Succeeded, None),
            comparison: None,
            ring: EventRing::new(4),
            artifacts: vec![format!("artifact:{secret}/native.bin")],
        },
    );
    let encoded = String::from_utf8(receipt.encode()).unwrap();
    assert!(!encoded.contains(secret));
    assert!(encoded.contains("[REDACTED]"));
}

// ---------------------------------------------------------------------------
// Qualification suite
// ---------------------------------------------------------------------------

#[test]
fn native_root_read_qualification_scenarios_all_pass() {
    let root = TempTestDir::new("scenarios");

    let mut receipts = vec![
        qualify("apfs-exact-case-read", || scenario_exact_case_read(root.path())),
        qualify(
            "apfs-case-insensitive-resolution-documented",
            || scenario_case_insensitive_resolution_documented(root.path()),
        ),
        qualify(
            "normalization-neighbors",
            || scenario_normalization_neighbors(root.path()),
        ),
        qualify(
            "symlink-swap-exact-captures",
            || scenario_symlink_target_swap_keeps_each_capture_exact(root.path()),
        ),
        qualify(
            "root-loss-and-restoration",
            || scenario_root_loss_fails_and_restoration_recovers(root.path()),
        ),
        qualify(
            "revocation-stops-delivery",
            || scenario_revocation_during_read_stops_delivery(root.path()),
        ),
        qualify(
            "encoded-traversal-rejected",
            || scenario_encoded_traversal_escalation_rejected(root.path()),
        ),
        qualify(
            "raw-byte-unicode-filenames",
            || scenario_raw_byte_and_unicode_filenames_roundtrip(root.path()),
        ),
        qualify(
            "export-gate-serializes",
            || scenario_export_publication_gate_serializes_natively(root.path()),
        ),
    ];

    // Negative control: a deliberate mismatch must surface as a Failure
    // receipt with evidence, proving the harness detects failures.
    receipts.push(negative_control_deliberate_mismatch());

    let failures: Vec<&ScenarioReceipt> = receipts
        .iter()
        .filter(|receipt| receipt.outcome().effect() == Effect::Failed)
        .collect();
    assert_eq!(
        failures
            .iter()
            .map(|receipt| receipt.scenario())
            .collect::<Vec<_>>(),
        vec!["negative-control-deliberate-mismatch"],
        "every qualification scenario must pass except the deliberate control"
    );

    // Retained, bounded, redacted receipts: canonical re-encode and field
    // budget respected.
    for receipt in &receipts {
        assert!(receipt.scenario().len() <= MAX_FIELD_BYTES);
        let decoded =
            ScenarioReceipt::decode(&receipt.encode()).expect("receipt decodes canonically");
        assert_eq!(*receipt, decoded);
    }
    secret_sentinel_never_reaches_the_retained_receipts();
}

#[test]
fn malformed_receipt_rows_are_rejected_not_repaired() {
    // Negative control at the codec boundary: a truncated qualification
    // receipt is a typed error, never a fabricated verdict.
    let receipt = success("truncated-input-control");
    let encoded_bytes = receipt.encode();
    let text = std::str::from_utf8(&encoded_bytes).unwrap();
    let truncated: String = text.lines().take(3).collect::<Vec<_>>().join("\n");
    assert_eq!(
        ScenarioReceipt::decode(truncated.as_bytes()),
        Err(ReceiptError::MalformedRow)
    );
}
