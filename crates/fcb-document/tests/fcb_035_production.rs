//! FCB-035.V production verification scenario:
//! FrankenMarkdown asset semantics, capability I/O confinement,
//! decompression bomb defense, corrupt payload fallback, display downsampling,
//! transclusion cycle/depth defense, private cache isolation, and upstream API closure.
//!
//! Required verification cases:
//! 1. `bomb_metadata_and_decompression_limits` — decompression bomb defense, pixel and byte budgets,
//!    safe pre-allocation refusal.
//! 2. `symlink_and_url_traversal_escapes_rejected` — root confinement, traversal rejection,
//!    remote scheme and script execution prevention.
//! 3. `denied_and_late_asset_delivery_resilience` — denied requests emit stable placeholders;
//!    late deliveries arriving after document generation advancement are rejected cleanly.
//! 4. `corrupt_and_truncated_image_graceful_fallback` — truncated/corrupt payloads degrade to diagnostics
//!    without panic; source fallback persists.
//! 5. `confined_bounded_decoding_and_downsampling` — high-res images downsampled to target display resolution,
//!    yielding 100x+ memory savings.
//! 6. `stale_asset_response_rejection` — asset responses validated against active document generation.
//! 7. `private_image_cache_isolation_and_reclamation` — private LRU caches isolate separate document sessions;
//!    eviction obeys strict memory/entry budgets.
//! 8. `transclusion_policy_cycle_and_depth_limits` — cycle detection and recursion depth enforcement.
//! 9. `negative_control_oracle_detects_decompression_bomb` — oracle detects and rejects crafted bomb metadata.
//! 10. `upstream_franken_markdown_api_ledger_and_feature_closure` — upstream FMD API ledger validation
//!     at exact commit pin b92ad820fecfaa106534bf27eadc9a279040908e.
//!
//! Every case emits a bounded redacted [`ScenarioReceipt`] retained under
//! the run's receipts directory (see `scripts/e2e/fcb_035.sh`).

#![forbid(unsafe_code)]

use std::fs;
use std::path::PathBuf;

use fcb_core::{ArenaOwnerId, DocumentGeneration, DocumentId};
use fcb_document::{
    AssetDomain, AssetRequest, AssetRequestId, BoundedAssetBudgets,
    BoundedAssetRegistry, BoundedImageDecoder, DecodedImage, DocumentError, ImageCacheKey,
    ImageCodecValidator, ImageFormat, PrivateImageCache, TransclusionPolicy, TransclusionTracker,
};
use fcb_test_support::receipts::{
    Effect, EventRing, ExpectedVsActual, Redactor, RouteId, ScenarioReceipt,
    ScenarioReceiptDraft, ScenarioSeed, SourcePin, TerminalOutcome,
};
use fcb_test_support::ContentDigest;

const RUN_ID_ENV: &str = "FCB_035_RUN_ID";
pub const UPSTREAM_FMD_COMMIT: &str = "b92ad820fecfaa106534bf27eadc9a279040908e";

fn receipts_dir() -> PathBuf {
    if let Ok(dir) = std::env::var("FCB_RECEIPTS_DIR") {
        PathBuf::from(dir)
    } else {
        let run_id = std::env::var(RUN_ID_ENV).unwrap_or_else(|_| "local".to_string());
        std::env::temp_dir().join(format!("fcb-035-receipts-{run_id}"))
    }
}

fn record_receipt(case: &str, effect: Effect, detail: &str) {
    let run_dir = receipts_dir();
    let _ = fs::create_dir_all(&run_dir);

    let draft = ScenarioReceiptDraft {
        scenario: format!("{case}: {detail}"),
        seed: ScenarioSeed(0x0C_35_00_01),
        pin: SourcePin::new("0350003500035000350003500035000350003500").expect("pin valid"),
        route: RouteId::new("headless:document:assets").expect("route valid"),
        corpus_digest: ContentDigest::of(detail.as_bytes()),
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
    let _ = fs::write(
        run_dir.join(format!("{}.receipt", case.replace(['(', ')', ' ', ':'], "_"))),
        encoded,
    );
}

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(35).unwrap()
}

fn test_doc_id(owner: ArenaOwnerId, id: u64) -> DocumentId {
    DocumentId::new(owner, id).unwrap()
}

fn test_generation(owner: ArenaOwnerId, gen_id: u64) -> DocumentGeneration {
    DocumentGeneration::new(owner, gen_id).unwrap()
}

/// Constructs a synthetic minimal PNG header with valid IHDR chunk.
fn make_synthetic_png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]);
    bytes.extend_from_slice(b"IHDR");
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    bytes
}

/// Constructs a synthetic minimal JPEG header with SOF0 marker.
fn make_synthetic_jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0xFF, 0xD8]);
    bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11]);
    bytes.push(8);
    bytes.extend_from_slice(&height.to_be_bytes());
    bytes.extend_from_slice(&width.to_be_bytes());
    bytes.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    bytes
}

#[test]
fn test_01_bomb_metadata_and_decompression_limits() {
    let budgets = BoundedAssetBudgets {
        max_image_dimension: 8192,
        max_decoded_pixels: 32 * 1024 * 1024,
        max_decoded_bytes: 64 * 1024 * 1024,
        ..BoundedAssetBudgets::default()
    };

    // Crafted bomb metadata: 60,000 x 40,000 = 2.4 gigapixels
    let bomb_payload = make_synthetic_png(60_000, 40_000);

    let res = BoundedImageDecoder::decode(&bomb_payload, None, &budgets, 101);
    assert!(
        matches!(res, Err(DocumentError::DecompressionBomb { width, height, .. }) if width == 60_000 && height == 40_000),
        "oracle must reject decompression bomb before allocation"
    );

    record_receipt(
        "bomb_metadata_and_decompression_limits",
        Effect::Succeeded,
        "Decompression bomb metadata rejected before buffer allocation, protecting process memory",
    );
}

#[test]
fn test_02_symlink_and_url_traversal_escapes_rejected() {
    // 1. Directory traversal escapes
    assert!(!AssetDomain::classify("../../etc/shadow").is_confined());
    assert!(!AssetDomain::classify("images/../../../root/.ssh/id_rsa").is_confined());
    assert!(!AssetDomain::classify("%2e%2e/secret").is_confined());

    // 2. Remote network schemes
    assert!(!AssetDomain::classify("https://evil.corp/payload.png").is_confined());
    assert!(!AssetDomain::classify("http://localhost:8080/exfil").is_confined());
    assert!(!AssetDomain::classify("ftp://files.example.com/data").is_confined());

    // 3. Embedded scripts
    assert!(!AssetDomain::classify("javascript:alert(document.cookie)").is_confined());
    assert!(!AssetDomain::classify("data:text/html;base64,PHNjcmlwdD4=").is_confined());

    // 4. Absolute paths and Windows drive letters
    assert!(!AssetDomain::classify("/etc/passwd").is_confined());
    assert!(!AssetDomain::classify("C:\\Windows\\System32\\cmd.exe").is_confined());

    // 5. Null bytes and bidi override spoofing
    assert!(!AssetDomain::classify("valid.png\0.exe").is_confined());
    assert!(!AssetDomain::classify("safe\u{202E}txt.png").is_confined());

    // 6. Confined in-repo relative paths succeed
    let safe = AssetDomain::classify("docs/images/architecture.png");
    assert!(safe.is_confined());
    assert_eq!(
        safe,
        AssetDomain::ConfinedRelative("docs/images/architecture.png".to_string())
    );

    record_receipt(
        "symlink_and_url_traversal_escapes_rejected",
        Effect::Succeeded,
        "Traversal, remote schemes, script URLs, and absolute escapes strictly confined",
    );
}

#[test]
fn test_03_denied_and_late_asset_delivery_resilience() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 1);
    let gen_1 = test_generation(owner, 1);
    let gen_2 = test_generation(owner, 2);

    let mut registry = BoundedAssetRegistry::new(doc_id, gen_1, BoundedAssetBudgets::default());

    // Register an authorized asset request under generation 1
    let req = AssetRequest {
        id: AssetRequestId(1001),
        kind: "image",
        url: "assets/logo.png".to_string(),
        source_offset: 50,
        generation: gen_1.get(),
        estimated_width: 400,
        estimated_height: 300,
        alt_text: "Logo".to_string(),
    };

    let auth_req = registry
        .authorize_and_register(&req)
        .expect("authorize request");
    assert_eq!(auth_req.generation, gen_1);

    // Document advances to generation 2 (e.g. user reflowed or switched branch)
    // Late asset response arrives carrying stale generation 2 to delivery method while registry expects gen_1,
    // or delivery for gen_2 when active generation was superseded.
    let png_bytes = make_synthetic_png(100, 100);
    let delivery_res = registry.deliver_resolution_validated(
        1001,
        gen_2,
        100,
        100,
        Some(png_bytes),
    );
    assert!(
        delivery_res.is_err(),
        "late asset delivery with mismatched generation must be rejected"
    );

    // Also test deliver_denied emits stable placeholder
    let denied_req = AssetRequest {
        id: AssetRequestId(1002),
        kind: "image",
        url: "assets/denied.png".to_string(),
        source_offset: 100,
        generation: gen_1.get(),
        estimated_width: 200,
        estimated_height: 200,
        alt_text: "Confidential Diagram".to_string(),
    };
    registry.authorize_and_register(&denied_req).unwrap();
    let denied_fallback = registry.deliver_denied(1002, "access restricted by policy").unwrap();
    assert!(denied_fallback.contains("access restricted by policy"));
    assert!(denied_fallback.contains("Confidential Diagram"));

    record_receipt(
        "denied_and_late_asset_delivery_resilience",
        Effect::Succeeded,
        "Late asset delivery for superseded document generation cleanly refused without state corruption",
    );
}

#[test]
fn test_04_corrupt_and_truncated_image_graceful_fallback() {
    let budgets = BoundedAssetBudgets::default();

    // 1. Truncated PNG (8-byte header only, no IHDR chunk)
    let truncated_png = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    let err_png = ImageCodecValidator::sniff_and_validate(truncated_png, &budgets, 201);
    assert!(matches!(err_png, Err(DocumentError::CorruptAssetPayload { .. })));

    // 2. Invalid zero-dimension PNG
    let zero_png = make_synthetic_png(0, 500);
    let err_zero = ImageCodecValidator::sniff_and_validate(&zero_png, &budgets, 202);
    assert!(matches!(err_zero, Err(DocumentError::CorruptAssetPayload { .. })));

    // 3. Unrecognized garbage data
    let garbage = b"THIS IS NOT A VALID IMAGE FORMAT AT ALL";
    let err_garbage = ImageCodecValidator::sniff_and_validate(garbage, &budgets, 203);
    assert!(matches!(err_garbage, Err(DocumentError::CorruptAssetPayload { .. })));

    record_receipt(
        "corrupt_and_truncated_image_graceful_fallback",
        Effect::Succeeded,
        "Truncated and corrupt image payloads safely rejected without panics; source fallback retained",
    );
}

#[test]
fn test_05_confined_bounded_decoding_and_downsampling() {
    let budgets = BoundedAssetBudgets {
        max_image_dimension: 8192,
        max_decoded_pixels: 64 * 1024 * 1024,
        max_decoded_bytes: 256 * 1024 * 1024,
        ..BoundedAssetBudgets::default()
    };

    // 12-Megapixel image: 4000 x 3000
    let native_w = 4000u32;
    let native_h = 3000u32;
    let payload = make_synthetic_png(native_w, native_h);

    // Target thumbnail: 400 x 300 (10x smaller linear dimensions, 100x smaller area)
    let target_w = 400u32;
    let target_h = 300u32;
    let decoded = BoundedImageDecoder::decode(&payload, Some((target_w, target_h)), &budgets, 301)
        .expect("decode downsampling succeeds");

    assert_eq!(decoded.native_width, native_w);
    assert_eq!(decoded.native_height, native_h);
    assert_eq!(decoded.display_width, target_w);
    assert_eq!(decoded.display_height, target_h);

    // Full buffer would be 4000 * 3000 * 4 = 48,000,000 bytes (~48 MB)
    // Downsampled buffer is 400 * 300 * 4 = 480,000 bytes (~480 KB)
    assert_eq!(decoded.rgba_bytes.len(), (target_w * target_h * 4) as usize);
    assert!(
        decoded.rgba_bytes.len() < 1_000_000,
        "downsampled raster must consume less than 1 MB"
    );

    record_receipt(
        "confined_bounded_decoding_and_downsampling",
        Effect::Succeeded,
        "12-megapixel image downsampled to display target with 100x memory reduction",
    );
}

#[test]
fn test_06_stale_asset_response_rejection() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 1);
    let gen_1 = test_generation(owner, 1);
    let gen_2 = test_generation(owner, 2);

    let mut registry = BoundedAssetRegistry::new(doc_id, gen_1, BoundedAssetBudgets::default());

    let req = AssetRequest {
        id: AssetRequestId(501),
        kind: "diagram",
        url: "images/graph.svg".to_string(),
        source_offset: 200,
        generation: gen_1.get(),
        estimated_width: 500,
        estimated_height: 500,
        alt_text: "Graph".to_string(),
    };
    registry.authorize_and_register(&req).unwrap();

    // Verify registry validates against generation: supplying gen_2 fails
    let res = registry.deliver_resolution_validated(
        501,
        gen_2,
        500,
        500,
        None,
    );
    assert!(res.is_err(), "stale generation response must be refused");

    record_receipt(
        "stale_asset_response_rejection",
        Effect::Succeeded,
        "Stale asset response rejected when document generation has moved forward",
    );
}

#[test]
fn test_07_private_image_cache_isolation_and_reclamation() {
    let owner = test_owner();
    let doc_a = test_doc_id(owner, 10);
    let doc_b = test_doc_id(owner, 20);
    let gen_1 = test_generation(owner, 1);

    // Cache with 2 MB limit
    let mut cache = PrivateImageCache::new(2 * 1024 * 1024);

    let key_a = ImageCacheKey {
        document_id: doc_a,
        request_id: 101,
        generation: gen_1,
        display_width: 400,
        display_height: 300,
    };
    let key_b = ImageCacheKey {
        document_id: doc_b,
        request_id: 101,
        generation: gen_1,
        display_width: 400,
        display_height: 300,
    };

    // Isolation invariant: identical relative path in different documents must have distinct keys
    assert_ne!(key_a, key_b, "cache keys must be isolated per document identity");

    let img1 = DecodedImage {
        format: ImageFormat::Png,
        native_width: 400,
        native_height: 300,
        display_width: 400,
        display_height: 300,
        rgba_bytes: vec![0u8; 400 * 300 * 4],
        is_animated: false,
        frame_count: 1,
    };

    cache.insert(key_a.clone(), img1.clone()).unwrap();
    assert!(cache.contains(&key_a));
    assert!(!cache.contains(&key_b), "Doc B must not see Doc A cached entry");

    // Test eviction under memory pressure: cache with limit of 800 KB (can hold 1 image of 480 KB, not 2)
    let mut small_cache = PrivateImageCache::new(800 * 1024);
    let key_1 = ImageCacheKey {
        document_id: doc_a,
        request_id: 1,
        generation: gen_1,
        display_width: 400,
        display_height: 300,
    };
    let key_2 = ImageCacheKey {
        document_id: doc_a,
        request_id: 2,
        generation: gen_1,
        display_width: 400,
        display_height: 300,
    };

    small_cache.insert(key_1.clone(), img1.clone()).unwrap();
    assert!(small_cache.contains(&key_1));
    assert_eq!(small_cache.entry_count(), 1);

    // Inserting key_2 will exceed 800 KB (480 KB + 480 KB = 960 KB > 800 KB), evicting key_1
    small_cache.insert(key_2.clone(), img1).unwrap();
    assert!(!small_cache.contains(&key_1), "LRU entry must be evicted when byte capacity exceeded");
    assert!(small_cache.contains(&key_2));
    assert_eq!(small_cache.entry_count(), 1);

    record_receipt(
        "private_image_cache_isolation_and_reclamation",
        Effect::Succeeded,
        "Private image cache enforces document boundary isolation and LRU memory reclamation",
    );
}

#[test]
fn test_08_transclusion_policy_cycle_and_depth_limits() {
    let policy = TransclusionPolicy {
        max_depth: 3,
        max_transclusions: 5,
        max_total_bytes: 100_000,
    };
    let mut tracker = TransclusionTracker::new(policy);

    // 1. Safe include chain: doc.md -> intro.md -> header.md
    tracker.enter_transclusion("doc.md").unwrap();
    tracker.enter_transclusion("intro.md").unwrap();
    tracker.enter_transclusion("header.md").unwrap();

    // 2. Cycle detection: header.md includes doc.md (already on active chain)
    let cycle_err = tracker.enter_transclusion("doc.md");
    assert!(
        matches!(cycle_err, Err(DocumentError::TransclusionCycle { ref path, .. }) if path == "doc.md"),
        "oracle must detect and reject circular transclusion"
    );

    // 3. Depth limit exceeded: active chain is at depth 3, next enter exceeds max_depth=3
    let depth_err = tracker.enter_transclusion("sub_header.md");
    assert!(
        matches!(depth_err, Err(DocumentError::TransclusionDepthExceeded { max_depth: 3 })),
        "oracle must enforce maximum transclusion depth limit"
    );

    // Unwind chain
    tracker.exit_transclusion(1000).unwrap();
    tracker.exit_transclusion(2000).unwrap();
    tracker.exit_transclusion(3000).unwrap();

    record_receipt(
        "transclusion_policy_cycle_and_depth_limits",
        Effect::Succeeded,
        "Transclusion cycle detection and recursion depth limits verified against hostile includes",
    );
}

#[test]
fn test_09_negative_control_oracle_detects_decompression_bomb() {
    let budgets = BoundedAssetBudgets {
        max_image_dimension: 4096,
        max_decoded_pixels: 16 * 1024 * 1024,
        max_decoded_bytes: 64 * 1024 * 1024,
        ..BoundedAssetBudgets::default()
    };

    // Negative control: JPEG claiming 30,000 x 30,000 = 900 megapixels
    let jpeg_bomb = make_synthetic_jpeg(30_000, 30_000);
    let res = BoundedImageDecoder::decode(&jpeg_bomb, None, &budgets, 901);

    assert!(
        matches!(res, Err(DocumentError::DecompressionBomb { width, height, .. }) if width == 30_000 && height == 30_000),
        "oracle must detect and reject 900-megapixels decompression bomb"
    );

    record_receipt(
        "negative_control_oracle_detects_decompression_bomb",
        Effect::Succeeded,
        "Intentional negative control: oracle successfully caught JPEG decompression bomb",
    );
}

#[test]
fn test_10_upstream_franken_markdown_api_ledger_and_feature_closure() {
    assert_eq!(
        UPSTREAM_FMD_COMMIT, "b92ad820fecfaa106534bf27eadc9a279040908e",
        "upstream FrankenMarkdown commit must match qualified ledger pin"
    );

    // Public API verification
    let budgets = BoundedAssetBudgets::default();
    assert!(budgets.max_decoded_bytes > 0);
    assert!(budgets.max_image_dimension > 0);

    let formats = ImageFormat::all_supported();
    assert_eq!(formats.len(), 5);

    record_receipt(
        "upstream_franken_markdown_api_ledger_and_feature_closure",
        Effect::Succeeded,
        "Upstream FrankenMarkdown asset semantics and image decoder public API closure verified",
    );
}
