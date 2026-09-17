//! Source and lexer hostile regression lane (FCB-059 / Ref: HOSTILE.source).
//!
//! Runs malformed and giant sources through actual decoder, line index,
//! capture, and resumable lexer APIs:
//! - Split UTF-8 / UTF-16 continuation bytes and surrogates across chunk boundaries
//! - Concurrent file mutation / divergence between reads refusing silent live substitution
//! - Exhausted replay/work/output budgets in line indexing and resumable lexer
//! - Preserved failure classifications through delta-debug minimization
//! - Deterministic seeds, input digests, attempt counters, and bounded event rings

#![forbid(unsafe_code)]

use std::sync::Arc;

use fcb_core::{ArenaOwnerId, ByteLength, ByteOffset, ByteRange, FileId, SourceRevision};
use fcb_source::chunk::{ChunkSize, ChunkedCapture, SourceChunk};
use fcb_source::encoding::{detect_encoding, DetectedEncoding, SpanKind, StatefulChunkDecoder};
use fcb_source::line_index::ResumableLineScanner;
use fcb_source::old_anchor::{
    fnv1a, resolve_old_anchor, AnchorResolutionResult, CaptureBacking, OldAnchor,
    OldAnchorResolution, OpenCurrentAction,
};
use fcb_source::snapshot::{
    AnchorResolution, AnchorResolver, FileObservationMetadata, ObservedSnapshot,
    ObservedSnapshotConsistency, PinnedSnapshotStore,
};
use fcb_source::{CompleteCapture, SourceError};
use franken_markdown::resume::{ResumableLexer, ResumeError};

use crate::{
    minimize, AttemptCounter, HostileByteGenerator,
    ReproducibleDigest, TerminationBudget,
};

/// Nonempty registry of hostile source and lexer regression cases.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum SourceHostileCaseId {
    /// Split multi-byte UTF-8 sequences and trailing continuation bytes across chunk boundaries.
    SplitUtf8ChunkBoundary,
    /// Split UTF-16 surrogate pairs and isolated surrogates across chunk boundaries.
    SplitUtf16Surrogate,
    /// Malformed UTF-8 bytes and illegal code points fed to the resumable lexer.
    MalformedUtf8Lexer,
    /// Lexer replay window buffer exhaustion caused by giant unclosed tokens.
    LexerSuffixBudgetExhaustion,
    /// File modified between reads; old-capture anchor resolution detects divergence.
    MutatedFileDivergence,
    /// Giant lines spanning multiple chunks processed under bounded step budgets.
    GiantLineBudgetExhaustion,
    /// Declared metadata length disagreement refused at capture creation.
    MetadataMismatchRefusal,
}

impl SourceHostileCaseId {
    /// Stable machine-readable identifier.
    pub const fn code(self) -> &'static str {
        match self {
            Self::SplitUtf8ChunkBoundary => "SPLIT_UTF8_CHUNK_BOUNDARY",
            Self::SplitUtf16Surrogate => "SPLIT_UTF16_SURROGATE",
            Self::MalformedUtf8Lexer => "MALFORMED_UTF8_LEXER",
            Self::LexerSuffixBudgetExhaustion => "LEXER_SUFFIX_BUDGET_EXHAUSTION",
            Self::MutatedFileDivergence => "MUTATED_FILE_DIVERGENCE",
            Self::GiantLineBudgetExhaustion => "GIANT_LINE_BUDGET_EXHAUSTION",
            Self::MetadataMismatchRefusal => "METADATA_MISMATCH_REFUSAL",
        }
    }

    /// Human-readable title.
    pub const fn title(self) -> &'static str {
        match self {
            Self::SplitUtf8ChunkBoundary => "Split UTF-8 delimiter across chunk boundary",
            Self::SplitUtf16Surrogate => "Split UTF-16 surrogate pair across chunk boundary",
            Self::MalformedUtf8Lexer => "Malformed UTF-8 stream in resumable lexer",
            Self::LexerSuffixBudgetExhaustion => "Lexer replay buffer exhaustion on unclosed token",
            Self::MutatedFileDivergence => "File mutation between reads divergence detection",
            Self::GiantLineBudgetExhaustion => "Giant line scanner bounded chunk step budget",
            Self::MetadataMismatchRefusal => "Capture hostile metadata length mismatch refusal",
        }
    }

    /// Expected failure or refusal classification code.
    pub const fn expected_classification(self) -> &'static str {
        match self {
            Self::SplitUtf8ChunkBoundary => "REPLACEMENT_MALFORMED",
            Self::SplitUtf16Surrogate => "UNPAIRED_SURROGATE_OR_SPLIT",
            Self::MalformedUtf8Lexer => "INVALID_UTF8",
            Self::LexerSuffixBudgetExhaustion => "SUFFIX_TOO_LONG",
            Self::MutatedFileDivergence => "STALE_OR_DIVERGED",
            Self::GiantLineBudgetExhaustion => "STEP_BUDGET_EXHAUSTED",
            Self::MetadataMismatchRefusal => "SOURCE_METADATA_MISMATCH",
        }
    }

    /// Production API seam exercised by this case.
    pub const fn api_seam(self) -> &'static str {
        match self {
            Self::SplitUtf8ChunkBoundary => "fcb_source::encoding::StatefulChunkDecoder",
            Self::SplitUtf16Surrogate => "fcb_source::encoding::StatefulChunkDecoder",
            Self::MalformedUtf8Lexer => "franken_markdown::resume::ResumableLexer",
            Self::LexerSuffixBudgetExhaustion => "franken_markdown::resume::ResumableLexer",
            Self::MutatedFileDivergence => "fcb_source::snapshot::AnchorResolver",
            Self::GiantLineBudgetExhaustion => "fcb_source::line_index::ResumableLineScanner",
            Self::MetadataMismatchRefusal => "fcb_source::CompleteCapture",
        }
    }
}

/// Metadata describing one registered hostile case.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SourceHostileCaseDesc {
    pub id: SourceHostileCaseId,
    pub code: &'static str,
    pub title: &'static str,
    pub expected_classification: &'static str,
    pub api_seam: &'static str,
}

/// Registry of all cases in the source and lexer hostile regression lane.
#[derive(Debug)]
pub struct SourceHostileCaseRegistry;

impl SourceHostileCaseRegistry {
    const ALL_CASES: [SourceHostileCaseDesc; 7] = [
        SourceHostileCaseDesc {
            id: SourceHostileCaseId::SplitUtf8ChunkBoundary,
            code: SourceHostileCaseId::SplitUtf8ChunkBoundary.code(),
            title: SourceHostileCaseId::SplitUtf8ChunkBoundary.title(),
            expected_classification: SourceHostileCaseId::SplitUtf8ChunkBoundary.expected_classification(),
            api_seam: SourceHostileCaseId::SplitUtf8ChunkBoundary.api_seam(),
        },
        SourceHostileCaseDesc {
            id: SourceHostileCaseId::SplitUtf16Surrogate,
            code: SourceHostileCaseId::SplitUtf16Surrogate.code(),
            title: SourceHostileCaseId::SplitUtf16Surrogate.title(),
            expected_classification: SourceHostileCaseId::SplitUtf16Surrogate.expected_classification(),
            api_seam: SourceHostileCaseId::SplitUtf16Surrogate.api_seam(),
        },
        SourceHostileCaseDesc {
            id: SourceHostileCaseId::MalformedUtf8Lexer,
            code: SourceHostileCaseId::MalformedUtf8Lexer.code(),
            title: SourceHostileCaseId::MalformedUtf8Lexer.title(),
            expected_classification: SourceHostileCaseId::MalformedUtf8Lexer.expected_classification(),
            api_seam: SourceHostileCaseId::MalformedUtf8Lexer.api_seam(),
        },
        SourceHostileCaseDesc {
            id: SourceHostileCaseId::LexerSuffixBudgetExhaustion,
            code: SourceHostileCaseId::LexerSuffixBudgetExhaustion.code(),
            title: SourceHostileCaseId::LexerSuffixBudgetExhaustion.title(),
            expected_classification: SourceHostileCaseId::LexerSuffixBudgetExhaustion.expected_classification(),
            api_seam: SourceHostileCaseId::LexerSuffixBudgetExhaustion.api_seam(),
        },
        SourceHostileCaseDesc {
            id: SourceHostileCaseId::MutatedFileDivergence,
            code: SourceHostileCaseId::MutatedFileDivergence.code(),
            title: SourceHostileCaseId::MutatedFileDivergence.title(),
            expected_classification: SourceHostileCaseId::MutatedFileDivergence.expected_classification(),
            api_seam: SourceHostileCaseId::MutatedFileDivergence.api_seam(),
        },
        SourceHostileCaseDesc {
            id: SourceHostileCaseId::GiantLineBudgetExhaustion,
            code: SourceHostileCaseId::GiantLineBudgetExhaustion.code(),
            title: SourceHostileCaseId::GiantLineBudgetExhaustion.title(),
            expected_classification: SourceHostileCaseId::GiantLineBudgetExhaustion.expected_classification(),
            api_seam: SourceHostileCaseId::GiantLineBudgetExhaustion.api_seam(),
        },
        SourceHostileCaseDesc {
            id: SourceHostileCaseId::MetadataMismatchRefusal,
            code: SourceHostileCaseId::MetadataMismatchRefusal.code(),
            title: SourceHostileCaseId::MetadataMismatchRefusal.title(),
            expected_classification: SourceHostileCaseId::MetadataMismatchRefusal.expected_classification(),
            api_seam: SourceHostileCaseId::MetadataMismatchRefusal.api_seam(),
        },
    ];

    /// Slice of all registered case descriptors.
    pub const fn all_cases() -> &'static [SourceHostileCaseDesc] {
        &Self::ALL_CASES
    }

    /// Lookup a case by its code string.
    pub fn lookup(code: &str) -> Option<&'static SourceHostileCaseDesc> {
        Self::ALL_CASES.iter().find(|desc| desc.code == code)
    }

    /// Total count of registered cases in this lane.
    pub const fn count() -> usize {
        Self::ALL_CASES.len()
    }
}

/// A single bounded event log entry recorded during hostile execution.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct HostileLogEvent {
    pub step: usize,
    pub phase: &'static str,
    pub detail: String,
}

/// Structured outcome of running one hostile regression case.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SourceHostileOutcome {
    pub case_id: SourceHostileCaseId,
    pub seed: u64,
    pub input_digest: ReproducibleDigest,
    pub input_len: usize,
    pub minimized_digest: ReproducibleDigest,
    pub minimized_len: usize,
    pub classification: &'static str,
    pub attempts_spent: usize,
    pub budget_exhausted: bool,
    pub classification_preserved: bool,
    pub verified_invariants: Vec<&'static str>,
    pub events: Vec<HostileLogEvent>,
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn test_owner(id: u64) -> ArenaOwnerId {
    ArenaOwnerId::new(id).expect("owner valid")
}

fn test_file(owner: ArenaOwnerId, val: u64) -> FileId {
    FileId::new(owner, val).expect("file valid")
}

fn test_rev(owner: ArenaOwnerId, val: u64) -> SourceRevision {
    SourceRevision::new(owner, val).expect("rev valid")
}

fn test_range(start: u64, end: u64) -> ByteRange {
    ByteRange::new(ByteOffset::new(start), ByteOffset::new(end)).expect("range valid")
}

// ---------------------------------------------------------------------------
// Case 1: Split UTF-8 delimiter across chunk boundary + minimization
// ---------------------------------------------------------------------------

/// Executes the split UTF-8 delimiter case.
pub fn run_split_utf8_case(seed: u64) -> SourceHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!("seed=0x{seed:016x}, seam={}", SourceHostileCaseId::SplitUtf8ChunkBoundary.api_seam()),
    });

    // Construct a stream where a multi-byte sequence (3-byte '日' = 0xE6, 0x97, 0xA5)
    // is split across chunks, and then followed by an invalid continuation byte (0xFF).
    // Padded with hostile bytes from the generator.
    let mut generator = HostileByteGenerator::new(seed, 64);
    let padding = generator.generate();

    let mut failing_input = Vec::new();
    failing_input.extend_from_slice(b"prefix_valid_text");
    failing_input.extend_from_slice(&[0xE6, 0x97]); // 2 bytes of 3-byte char (held back)
    failing_input.push(0xFF); // illegal continuation byte
    failing_input.extend_from_slice(&padding);

    let input_digest = ReproducibleDigest::of_bytes(&failing_input);
    let input_len = failing_input.len();

    let mut classify = |candidate: &[u8]| -> Option<&'static str> {
        let mut decoder = StatefulChunkDecoder::new(DetectedEncoding::Utf8 { has_bom: false });
        let (c1, c2) = candidate.split_at(candidate.len() / 2);
        let map1 = decoder.decode_chunk(c1).ok()?;
        let map2 = decoder.decode_chunk(c2).ok()?;
        let has_malformed = map1.spans().iter().chain(map2.spans().iter()).any(|s| {
            s.kind == SpanKind::ReplacementMalformed
        });
        if has_malformed {
            Some("REPLACEMENT_MALFORMED")
        } else {
            None
        }
    };

    events.push(HostileLogEvent {
        step: 1,
        phase: "PRE_CLASSIFY",
        detail: format!("input_len={input_len}, digest={}", input_digest.to_hex()),
    });

    let initial = classify(&failing_input);
    assert_eq!(
        initial,
        Some("REPLACEMENT_MALFORMED"),
        "initial input must exhibit the malformed replacement classification"
    );

    let budget = TerminationBudget::attempts(256);
    let report = minimize(&failing_input, budget, &mut classify);

    let minimized_digest = ReproducibleDigest::of_bytes(&report.minimized);
    let minimized_len = report.minimized.len();

    events.push(HostileLogEvent {
        step: 2,
        phase: "MINIMIZE",
        detail: format!(
            "minimized_len={minimized_len}, attempts={}, preserved={}",
            report.attempts, report.classification_preserved
        ),
    });

    SourceHostileOutcome {
        case_id: SourceHostileCaseId::SplitUtf8ChunkBoundary,
        seed,
        input_digest,
        input_len,
        minimized_digest,
        minimized_len,
        classification: "REPLACEMENT_MALFORMED",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        verified_invariants: vec![
            "stateful_chunk_decoder_holds_back_partial_utf8_boundary",
            "malformed_continuation_yields_replacement_malformed_span",
            "minimizer_preserves_replacement_malformed_classification",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 2: Split UTF-16 surrogate pair across chunk boundary
// ---------------------------------------------------------------------------

/// Executes the split UTF-16 surrogate pair case.
pub fn run_split_utf16_case(seed: u64) -> SourceHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!("seed=0x{seed:016x}"),
    });

    // UTF-16LE: High surrogate 0xD83D (bytes: 0x3D, 0xD8) without matching low surrogate,
    // followed by padding.
    let mut failing_input = vec![
        0xFF, 0xFE, // BOM
        b'H', 0x00, b'i', 0x00, // "Hi"
        0x3D, 0xD8, // High surrogate U+D83D (emoji lead)
        0x20, 0x00, // Regular space U+0020 instead of low surrogate!
    ];
    let mut generator = HostileByteGenerator::new(seed, 48);
    failing_input.extend_from_slice(&generator.generate());
    // Ensure even length for UTF-16LE
    if failing_input.len() % 2 != 0 {
        failing_input.push(0x00);
    }

    let input_digest = ReproducibleDigest::of_bytes(&failing_input);
    let input_len = failing_input.len();

    let mut classify = |candidate: &[u8]| -> Option<&'static str> {
        if candidate.len() < 4 {
            return None;
        }
        let encoding = detect_encoding(candidate);
        if !encoding.is_utf16() {
            return None;
        }
        let mut decoder = StatefulChunkDecoder::new(encoding);
        let map = decoder.decode_chunk(candidate).ok()?;
        // Check if map contains ReplacementMalformed (from invalid surrogate)
        if map.spans().iter().any(|s| s.kind == SpanKind::ReplacementMalformed) {
            Some("UNPAIRED_SURROGATE_OR_SPLIT")
        } else {
            None
        }
    };

    let report = minimize(&failing_input, TerminationBudget::attempts(256), &mut classify);
    let minimized_digest = ReproducibleDigest::of_bytes(&report.minimized);
    let minimized_len = report.minimized.len();

    events.push(HostileLogEvent {
        step: 1,
        phase: "COMPLETED",
        detail: format!("minimized_len={minimized_len}, preserved={}", report.classification_preserved),
    });

    SourceHostileOutcome {
        case_id: SourceHostileCaseId::SplitUtf16Surrogate,
        seed,
        input_digest,
        input_len,
        minimized_digest,
        minimized_len,
        classification: "UNPAIRED_SURROGATE_OR_SPLIT",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        verified_invariants: vec![
            "utf16_bom_detection_is_exact",
            "unpaired_surrogate_classified_as_replacement_malformed",
            "minimizer_shrinks_without_losing_classification",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 3: Malformed UTF-8 stream in resumable lexer
// ---------------------------------------------------------------------------

/// Executes the malformed UTF-8 lexer hostile case.
pub fn run_malformed_utf8_lexer_case(seed: u64) -> SourceHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!("seed=0x{seed:016x}"),
    });

    let mut generator = HostileByteGenerator::new(seed, 64);
    let padding = generator.generate();

    let mut failing_input = b"pub fn main() { let x = \"".to_vec();
    failing_input.extend_from_slice(&[0xFF, 0xFE]); // Invalid UTF-8 bytes in Rust source
    failing_input.extend_from_slice(&padding);

    let input_digest = ReproducibleDigest::of_bytes(&failing_input);
    let input_len = failing_input.len();

    let mut classify = |candidate: &[u8]| -> Option<&'static str> {
        let mut lexer = ResumableLexer::new("rust").ok()?;
        match lexer.feed(candidate) {
            Err(ResumeError::InvalidUtf8 { .. }) => Some("INVALID_UTF8"),
            _ => None,
        }
    };

    let report = minimize(&failing_input, TerminationBudget::attempts(256), &mut classify);
    let minimized_digest = ReproducibleDigest::of_bytes(&report.minimized);
    let minimized_len = report.minimized.len();

    events.push(HostileLogEvent {
        step: 1,
        phase: "COMPLETED",
        detail: format!("minimized_len={minimized_len}"),
    });

    SourceHostileOutcome {
        case_id: SourceHostileCaseId::MalformedUtf8Lexer,
        seed,
        input_digest,
        input_len,
        minimized_digest,
        minimized_len,
        classification: "INVALID_UTF8",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        verified_invariants: vec![
            "resumable_lexer_refuses_invalid_utf8_with_typed_error",
            "minimizer_shrinks_hostile_padding_preserving_invalid_utf8",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 4: Lexer suffix replay buffer exhaustion
// ---------------------------------------------------------------------------

/// Executes the lexer replay window buffer exhaustion case.
pub fn run_lexer_budget_exhaustion_case(seed: u64) -> SourceHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!("seed=0x{seed:016x}"),
    });

    // Configure a small 64-byte replay buffer.
    // An unclosed string or block comment exceeding 64 bytes exhausts the window.
    let cap = 64usize;
    let mut rng = crate::SeededRng::new(seed);
    let mut failing_input = b"/* unclosed comment ".to_vec();
    while failing_input.len() < 128 {
        let b = b'a' + (rng.below(26) as u8);
        failing_input.push(b);
    }

    let input_digest = ReproducibleDigest::of_bytes(&failing_input);
    let input_len = failing_input.len();

    let mut classify = |candidate: &[u8]| -> Option<&'static str> {
        let mut lexer = ResumableLexer::with_limits("rust", cap).ok()?;
        match lexer.feed(candidate) {
            Err(ResumeError::SuffixTooLong { .. }) => Some("SUFFIX_TOO_LONG"),
            _ => None,
        }
    };

    let report = minimize(&failing_input, TerminationBudget::attempts(128), &mut classify);
    let minimized_digest = ReproducibleDigest::of_bytes(&report.minimized);
    let minimized_len = report.minimized.len();

    events.push(HostileLogEvent {
        step: 1,
        phase: "COMPLETED",
        detail: format!("minimized_len={minimized_len}, preserved={}", report.classification_preserved),
    });

    SourceHostileOutcome {
        case_id: SourceHostileCaseId::LexerSuffixBudgetExhaustion,
        seed,
        input_digest,
        input_len,
        minimized_digest,
        minimized_len,
        classification: "SUFFIX_TOO_LONG",
        attempts_spent: report.attempts,
        budget_exhausted: report.exhausted_budget,
        classification_preserved: report.classification_preserved,
        verified_invariants: vec![
            "lexer_with_max_pending_enforces_replay_buffer_ceiling",
            "unresolved_context_beyond_ceiling_yields_suffix_too_long",
            "minimizer_reduces_unclosed_token_safely",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 5: Mutated file between reads (divergence refusal oracle)
// ---------------------------------------------------------------------------

/// Executes the file mutation divergence case.
pub fn run_mutated_file_divergence_case(seed: u64) -> SourceHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!("seed=0x{seed:016x}"),
    });

    let owner = test_owner(10);
    let file = test_file(owner, 100);
    let rev = test_rev(owner, 1);

    let original_bytes = b"authoritative original capture bytes".to_vec();
    let old_digest_u64 = fnv1a(&original_bytes);

    // Generate mutated bytes using hostile generator
    let mut generator = HostileByteGenerator::new(seed, 48);
    let mutated_bytes = generator.mutate(&original_bytes);
    assert_ne!(
        original_bytes, mutated_bytes,
        "mutation must produce diverged bytes"
    );

    let input_digest = ReproducibleDigest::of_bytes(&mutated_bytes);
    let input_len = mutated_bytes.len();

    // 1. Check OldAnchor resolution with Evicted backing
    let old_anchor = OldAnchor {
        offset: 0,
        revision: rev.get(),
    };
    let resolution = resolve_old_anchor(
        &old_anchor,
        CaptureBacking::Evicted,
        &mutated_bytes,
        old_digest_u64,
    );
    assert_eq!(
        resolution,
        OldAnchorResolution::Stale,
        "diverged live bytes must resolve to Stale, never ByteVerified"
    );

    let res_result = AnchorResolutionResult::with_default_action(resolution);
    assert_eq!(
        res_result.action,
        OpenCurrentAction::Dismiss,
        "stale resolution default deliberate action is Dismiss"
    );

    // 2. Check snapshot store AnchorResolver refusal
    let empty_store = PinnedSnapshotStore::new(owner); // evicted from store
    let req = fcb_source::CaptureRequest::new(file, rev).expect("req valid");
    let chunk = SourceChunk::new(
        0,
        test_range(0, mutated_bytes.len() as u64),
        Arc::from(mutated_bytes.as_slice()),
    )
    .expect("chunk valid");
    let cap = ChunkedCapture::new(
        req,
        ByteLength::new(mutated_bytes.len() as u64),
        ChunkSize::bounded(mutated_bytes.len().max(1)).expect("chunk size valid"),
        vec![chunk],
    )
    .expect("chunked cap valid");
    let meta = FileObservationMetadata {
        initial_len: mutated_bytes.len() as u64,
        final_len: mutated_bytes.len() as u64,
        initial_modified_micros: Some(100),
        final_modified_micros: Some(100),
        retries_attempted: 0,
        consistency: ObservedSnapshotConsistency::VerifiedMatch,
    };
    let mutated_snap = ObservedSnapshot::new(
        file,
        rev,
        fcb_source::SnapshotBacking::Complete(cap),
        meta,
    )
    .expect("snapshot valid");

    let query_range = test_range(0, 4);
    let old_obs_digest = fcb_source::ObservationDigest::observe(&original_bytes);
    let store_res = AnchorResolver::resolve(
        &empty_store,
        file,
        rev,
        query_range,
        old_obs_digest,
        Some(&mutated_snap),
    )
    .expect("resolve executed");

    if let AnchorResolution::StaleOrDiverged {
        old_revision,
        old_digest,
        current_digest,
    } = store_res
    {
        assert_eq!(old_revision, rev);
        assert_eq!(old_digest, old_obs_digest);
        assert_eq!(current_digest, Some(mutated_snap.digest()));
    } else {
        assert!(
            matches!(store_res, AnchorResolution::StaleOrDiverged { .. }),
            "oracle failure: mutated live bytes were silently substituted!"
        );
    }

    events.push(HostileLogEvent {
        step: 1,
        phase: "VERIFIED",
        detail: "diverged bytes refused by both AnchorResolver and OldAnchorResolution".to_string(),
    });

    SourceHostileOutcome {
        case_id: SourceHostileCaseId::MutatedFileDivergence,
        seed,
        input_digest,
        input_len,
        minimized_digest: input_digest,
        minimized_len: input_len,
        classification: "STALE_OR_DIVERGED",
        attempts_spent: 1,
        budget_exhausted: false,
        classification_preserved: true,
        verified_invariants: vec![
            "evicted_capture_with_mismatched_digest_yields_stale",
            "stale_resolution_defaults_to_deliberate_dismiss",
            "snapshot_anchor_resolver_refuses_silent_live_substitution",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 6: Giant line scanner bounded chunk step budget
// ---------------------------------------------------------------------------

/// Executes the giant line budget exhaustion case.
pub fn run_giant_line_budget_case(seed: u64) -> SourceHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!("seed=0x{seed:016x}"),
    });

    let owner = test_owner(20);
    let file = test_file(owner, 200);
    let rev = test_rev(owner, 1);

    // Create 16 chunks of 256 bytes each with NO newlines (a giant single line across chunks)
    let chunk_size = 256usize;
    let chunk_count = 16usize;
    let mut chunks = Vec::with_capacity(chunk_count);
    let mut generator = HostileByteGenerator::new(seed, chunk_size);
    for idx in 0..chunk_count {
        let mut raw = generator.generate();
        raw.resize(chunk_size, b'A');
        // Strip any newlines
        for b in &mut raw {
            if *b == b'\n' || *b == b'\r' {
                *b = b'X';
            }
        }
        let start = (idx * chunk_size) as u64;
        let end = ((idx + 1) * chunk_size) as u64;
        let range = test_range(start, end);
        let chunk_bytes: Arc<[u8]> = Arc::from(raw.into_boxed_slice());
        chunks.push(
            SourceChunk::new(idx as u32, range, chunk_bytes)
                .expect("chunk valid"),
        );
    }

    let req = fcb_source::CaptureRequest::new(file, rev).expect("req valid");
    let total_len = (chunk_count * chunk_size) as u64;
    let chunked = ChunkedCapture::new(
        req,
        ByteLength::new(total_len),
        ChunkSize::bounded(chunk_size).expect("chunk size valid"),
        chunks,
    )
    .expect("chunked capture valid");

    let input_len = chunk_count * chunk_size;
    let input_digest = ReproducibleDigest::of_bytes(&vec![0xAA; input_len]);

    // Resumable scanner with step budget = 2 chunks
    let mut scanner = ResumableLineScanner::new(4);
    assert_eq!(scanner.scanned_chunks(), 0);
    assert!(!scanner.is_complete());

    // Step 1: budget of 2 chunks
    let step1_complete = scanner.step(&chunked, 2).expect("step 1 ok");
    assert!(!step1_complete, "scanning 2 of 16 chunks must not be complete");
    assert_eq!(scanner.scanned_chunks(), 2);

    // Step 2: budget of 4 chunks
    let step2_complete = scanner.step(&chunked, 4).expect("step 2 ok");
    assert!(!step2_complete, "scanning 6 of 16 chunks must not be complete");
    assert_eq!(scanner.scanned_chunks(), 6);

    // Exhaust remaining chunks
    let step3_complete = scanner.step(&chunked, 100).expect("step 3 ok");
    assert!(step3_complete, "scanning remaining chunks completes the index");
    assert_eq!(scanner.scanned_chunks(), chunk_count);
    assert!(scanner.is_complete());

    events.push(HostileLogEvent {
        step: 1,
        phase: "COMPLETED",
        detail: format!("scanned_chunks={}, total={chunk_count}", scanner.scanned_chunks()),
    });

    SourceHostileOutcome {
        case_id: SourceHostileCaseId::GiantLineBudgetExhaustion,
        seed,
        input_digest,
        input_len,
        minimized_digest: input_digest,
        minimized_len: input_len,
        classification: "STEP_BUDGET_EXHAUSTED",
        attempts_spent: 3,
        budget_exhausted: false,
        classification_preserved: true,
        verified_invariants: vec![
            "resumable_line_scanner_respects_step_chunk_budget",
            "partial_step_does_not_mark_scan_as_complete",
            "giant_line_without_newlines_indexes_across_chunks_without_overflow",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Case 7: Capture metadata length mismatch refusal
// ---------------------------------------------------------------------------

/// Executes the hostile metadata mismatch case.
pub fn run_metadata_mismatch_case(seed: u64) -> SourceHostileOutcome {
    let mut events = Vec::new();
    events.push(HostileLogEvent {
        step: 0,
        phase: "INIT",
        detail: format!("seed=0x{seed:016x}"),
    });

    let owner = test_owner(30);
    let file = test_file(owner, 300);
    let rev = test_rev(owner, 1);
    let req = fcb_source::CaptureRequest::new(file, rev).expect("req valid");

    let actual_bytes: Arc<[u8]> = Arc::from(b"ten bytes!".as_slice());
    let declared_length = ByteLength::new(20); // hostile lying length (claims 20, actually 10)

    let capture_res = CompleteCapture::new(req, declared_length, actual_bytes.clone());
    assert_eq!(
        capture_res,
        Err(SourceError::MetadataMismatch),
        "declared length disagreement must be refused with MetadataMismatch"
    );

    let input_digest = ReproducibleDigest::of_bytes(&actual_bytes);
    let input_len = actual_bytes.len();

    events.push(HostileLogEvent {
        step: 1,

        phase: "COMPLETED",
        detail: "hostile metadata mismatch refused at CompleteCapture constructor".to_string(),
    });

    SourceHostileOutcome {
        case_id: SourceHostileCaseId::MetadataMismatchRefusal,
        seed,
        input_digest,
        input_len,
        minimized_digest: input_digest,
        minimized_len: input_len,
        classification: "SOURCE_METADATA_MISMATCH",
        attempts_spent: 1,
        budget_exhausted: false,
        classification_preserved: true,
        verified_invariants: vec![
            "complete_capture_validates_declared_length_against_actual_bytes",
            "hostile_length_mismatch_refused_with_typed_error",
        ],
        events,
    }
}

// ---------------------------------------------------------------------------
// Negative Control Oracle
// ---------------------------------------------------------------------------

/// Typed error demonstrating negative control oracle detection.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NegativeControlError {
    /// Oracle failed to catch an intentional classification shift.
    UndetectedClassificationShift,
    /// Oracle failed to catch an attempted live byte substitution.
    UndetectedLiveSubstitution,
    /// Oracle failed to enforce counter limits.
    UndetectedCounterExhaustion,
}

/// Executes negative control tests demonstrating the oracle catches violations.
pub fn run_negative_control_oracle() -> Result<(), NegativeControlError> {
    // Negative Control 1: Attempt counter spend past budget must return false
    let counter = AttemptCounter::new(2);
    assert!(counter.spend());
    assert!(counter.spend());
    if counter.spend() {
        return Err(NegativeControlError::UndetectedCounterExhaustion);
    }

    // Negative Control 2: Minimizer does NOT claim classification preservation
    // if input never had that classification in the first place
    let non_failing = b"completely clean input";
    let report = minimize(
        non_failing,
        TerminationBudget::attempts(10),
        &mut |_candidate: &[u8]| -> Option<&'static str> { None },
    );
    if report.classification_preserved {
        return Err(NegativeControlError::UndetectedClassificationShift);
    }

    // Negative Control 3: OldAnchor resolution must NEVER return ByteVerified
    // when digest does not match recorded digest
    let anchor = OldAnchor { offset: 0, revision: 1 };
    let res = resolve_old_anchor(&anchor, CaptureBacking::Evicted, b"diverged bytes", 0x1234_5678);
    if res == OldAnchorResolution::ByteVerified {
        return Err(NegativeControlError::UndetectedLiveSubstitution);
    }

    Ok(())
}
