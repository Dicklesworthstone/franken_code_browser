#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, DocumentGeneration, DocumentId};
use fcb_document::{
    AssetDomain, AssetKind, AssetRequest, AssetRequestId, BoundedAssetBudgets, BoundedAssetRegistry,
    DocumentError, ImageCodecValidator, ImageFormat, TransclusionPolicy, TransclusionTracker,
};

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(101).unwrap()
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
    // PNG signature: 89 50 4E 47 0D 0A 1A 0A
    bytes.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    // IHDR chunk: 4 bytes length (13), 4 bytes "IHDR"
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x0D]);
    bytes.extend_from_slice(b"IHDR");
    // Width (4 bytes BE)
    bytes.extend_from_slice(&width.to_be_bytes());
    // Height (4 bytes BE)
    bytes.extend_from_slice(&height.to_be_bytes());
    // Bit depth (1), Color type (6 = RGBA), Compression (0), Filter (0), Interlace (0)
    bytes.extend_from_slice(&[8, 6, 0, 0, 0]);
    // CRC (4 bytes dummy)
    bytes.extend_from_slice(&[0x00, 0x00, 0x00, 0x00]);
    bytes
}

/// Constructs a synthetic minimal GIF header.
fn make_synthetic_gif(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    // GIF89a header
    bytes.extend_from_slice(b"GIF89a");
    // Width (2 bytes LE)
    bytes.extend_from_slice(&width.to_le_bytes());
    // Height (2 bytes LE)
    bytes.extend_from_slice(&height.to_le_bytes());
    // Packed fields, background color index, aspect ratio
    bytes.extend_from_slice(&[0x80, 0x00, 0x00]);
    bytes
}

/// Constructs a synthetic minimal JPEG header with SOF0 marker.
fn make_synthetic_jpeg(width: u16, height: u16) -> Vec<u8> {
    let mut bytes = Vec::new();
    // SOI: FF D8
    bytes.extend_from_slice(&[0xFF, 0xD8]);
    // SOF0: FF C0, length (2 bytes: 8 + 3*channels = 17 for 3 channels)
    bytes.extend_from_slice(&[0xFF, 0xC0, 0x00, 0x11]);
    // Precision: 8 bits
    bytes.push(8);
    // Height (2 bytes BE)
    bytes.extend_from_slice(&height.to_be_bytes());
    // Width (2 bytes BE)
    bytes.extend_from_slice(&width.to_be_bytes());
    // 3 components (Y, Cb, Cr), 3 bytes each
    bytes.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    bytes
}

#[test]
fn test_typed_asset_kinds_classification() {
    assert_eq!(AssetKind::parse("image"), AssetKind::Image);
    assert_eq!(AssetKind::parse("img"), AssetKind::Image);
    assert_eq!(AssetKind::parse("IMAGE"), AssetKind::Image);
    assert_eq!(AssetKind::parse("transclusion"), AssetKind::Transclusion);
    assert_eq!(AssetKind::parse("include"), AssetKind::Transclusion);
    assert_eq!(AssetKind::parse("font"), AssetKind::Font);
    assert_eq!(AssetKind::parse("diagram"), AssetKind::Diagram);
    assert_eq!(AssetKind::parse("math"), AssetKind::Diagram);
    assert_eq!(AssetKind::parse("unknown_thing"), AssetKind::Image);

    assert_eq!(AssetKind::Image.as_str(), "image");
    assert_eq!(AssetKind::Transclusion.as_str(), "transclusion");
    assert_eq!(AssetKind::Font.as_str(), "font");
    assert_eq!(AssetKind::Diagram.as_str(), "diagram");
}

#[test]
fn test_inert_network_and_url_restrictions() {
    // Disallowed network and active content schemes
    let disallowed = [
        "http://example.com/image.png",
        "https://example.com/asset.svg",
        "ftp://mirror.lan/pkg.tar",
        "ws://live.server/events",
        "wss://secure.live/events",
        "javascript:alert(1)",
        "vbscript:msgbox(1)",
        "data:image/png;base64,iVBORw0KGgoAAAANSUhEUg==",
        "blob:d395833e-1147-49d7-8c3e-32432ff2",
        "HTTP://UPPERCASE.COM/IMG.PNG",
        "HTTPS://UPPERCASE.COM/IMG.PNG",
    ];

    for url in disallowed {
        let domain = AssetDomain::classify(url);
        assert!(
            !domain.is_confined(),
            "Expected URL '{}' to be rejected, got {:?}",
            url,
            domain
        );
        match domain {
            AssetDomain::Rejected { reason } => {
                assert!(
                    reason.contains("disallowed") || reason.contains("network"),
                    "Unexpected rejection reason: {}",
                    reason
                );
            }
            other => assert!(false, "Expected Rejected, got {:?}", other),
        }
    }

    // Hostile control characters, nulls, traversal, absolute paths
    let hostile = [
        "images/test\0.png",
        "assets/\u{202E}gnp.tset",
        "../secret/key.pem",
        "/etc/shadow",
        "nested/../../outside.png",
        "   ",
        "",
        "images\\win32\\backslashes.png",
    ];

    for url in hostile {
        let domain = AssetDomain::classify(url);
        assert!(
            !domain.is_confined(),
            "Expected hostile path '{}' to be rejected, got {:?}",
            url,
            domain
        );
    }

    // Legitimate confined relative paths
    let allowed = [
        "images/diagram.png",
        "./assets/architecture.svg",
        "docs/subfolder/spec.md",
        "logo_2026.webp",
    ];

    for url in allowed {
        let domain = AssetDomain::classify(url);
        assert!(
            domain.is_confined(),
            "Expected valid path '{}' to be confined, got {:?}",
            url,
            domain
        );
    }
}

#[test]
fn test_symlink_escape_confinement_defense() {
    let mock_fs = |path: &str| -> bool {
        // Mock check: paths starting with "symlinks/escaped" point outside repository root
        path.starts_with("symlinks/escaped")
    };

    // Legitimate relative path within root
    let safe_domain = AssetDomain::classify_with_symlinks("images/chart.png", &mock_fs);
    assert!(safe_domain.is_confined());

    // Symlink that resolves outside repository root
    let escaped_domain =
        AssetDomain::classify_with_symlinks("symlinks/escaped_target.png", &mock_fs);
    assert!(!escaped_domain.is_confined());
    match escaped_domain {
        AssetDomain::Rejected { reason } => {
            assert!(
                reason.contains("symlink escapes"),
                "Unexpected reason: {}",
                reason
            );
        }
        other => assert!(false, "Expected Rejected for symlink escape, got {:?}", other),
    }

    // Network URLs are still rejected even before symlink check
    let net_domain = AssetDomain::classify_with_symlinks("https://evil.corp/payload", &mock_fs);
    assert!(!net_domain.is_confined());
}

#[test]
fn test_transclusion_recursion_depth_and_cycle_policy() {
    let policy = TransclusionPolicy {
        max_depth: 4,
        max_transclusions: 10,
        max_total_bytes: 1024,
    };
    let mut tracker = TransclusionTracker::new(policy);

    assert_eq!(tracker.active_depth(), 0);
    assert_eq!(tracker.resolved_count(), 0);
    assert_eq!(tracker.total_bytes(), 0);

    // Enter depth 1..=4
    tracker.enter_transclusion("root.md").expect("enter root");
    assert_eq!(tracker.active_depth(), 1);
    tracker.enter_transclusion("chapter1.md").expect("enter c1");
    assert_eq!(tracker.active_depth(), 2);
    tracker.enter_transclusion("section1.md").expect("enter s1");
    assert_eq!(tracker.active_depth(), 3);
    tracker.enter_transclusion("snippet1.md").expect("enter sn1");
    assert_eq!(tracker.active_depth(), 4);

    // Depth 5 exceeds max_depth = 4
    let err = tracker
        .enter_transclusion("overflow.md")
        .expect_err("should exceed depth");
    match err {
        DocumentError::TransclusionDepthExceeded { max_depth } => {
            assert_eq!(max_depth, 4);
        }
        other => assert!(false, "Expected TransclusionDepthExceeded, got {:?}", other),
    }

    // Exit 2 levels
    tracker.exit_transclusion(100).expect("exit sn1");
    assert_eq!(tracker.active_depth(), 3);
    assert_eq!(tracker.resolved_count(), 1);
    assert_eq!(tracker.total_bytes(), 100);

    tracker.exit_transclusion(100).expect("exit s1");
    assert_eq!(tracker.active_depth(), 2);
    assert_eq!(tracker.resolved_count(), 2);

    // Cycle detection: try to enter "root.md" again while "root.md" is in active chain
    let cycle_err = tracker
        .enter_transclusion("root.md")
        .expect_err("should detect cycle");
    match cycle_err {
        DocumentError::TransclusionCycle { path, chain } => {
            assert_eq!(path, "root.md");
            assert_eq!(chain, vec!["root.md", "chapter1.md"]);
            let fallback = TransclusionTracker::source_fallback_for_cycle(&path, &chain);
            assert!(fallback.contains("transclusion cycle detected"));
            assert!(fallback.contains("root.md"));
            assert!(fallback.contains("execution/fetch refused"));
        }
        other => assert!(false, "Expected TransclusionCycle, got {:?}", other),
    }

    // Transclusion byte budget defense
    let byte_err = tracker
        .exit_transclusion(1000)
        .expect_err("should exceed byte budget");
    match byte_err {
        DocumentError::AssetBudgetExceeded { reason } => {
            assert!(reason.contains("accumulated transclusion bytes"));
        }
        other => assert!(false, "Expected AssetBudgetExceeded, got {:?}", other),
    }
}

#[test]
fn test_decompression_bomb_metadata_defense() {
    let budgets = BoundedAssetBudgets {
        max_pending_requests: 10,
        max_total_asset_bytes: 1024 * 1024,
        max_single_asset_bytes: 512 * 1024,
        max_image_dimension: 8192,
        max_decoded_pixels: 32 * 1024 * 1024, // 32 megapixels
        max_decoded_bytes: 128 * 1024 * 1024, // 128 MiB
    };

    // 1. Valid dimensions pass
    assert!(budgets.validate_image_dimensions(1920, 1080).is_ok());
    assert!(budgets.validate_image_dimensions(4096, 4096).is_ok());

    // 2. Single dimension exceeds max_image_dimension (e.g. 100,000 x 10)
    let err1 = budgets
        .validate_image_dimensions(100_000, 10)
        .expect_err("width exceeds limit");
    match err1 {
        DocumentError::DecompressionBomb {
            width,
            height,
            reason,
        } => {
            assert_eq!(width, 100_000);
            assert_eq!(height, 10);
            assert!(reason.contains("exceeds maximum permitted dimension"));
        }
        other => assert!(false, "Expected DecompressionBomb, got {:?}", other),
    }

    // 3. Dimensions within 8192, but product exceeds 32M pixels (e.g. 7000 x 7000 = 49M pixels)
    let err2 = budgets
        .validate_image_dimensions(7000, 7000)
        .expect_err("pixels exceed limit");
    match err2 {
        DocumentError::DecompressionBomb { reason, .. } => {
            assert!(reason.contains("exceeds maximum decoded pixel budget"));
        }
        other => assert!(false, "Expected DecompressionBomb, got {:?}", other),
    }
}

#[test]
fn test_codec_validation_and_corrupt_payload_defense() {
    let budgets = BoundedAssetBudgets::default();

    // 1. Valid synthetic PNG
    let png_bytes = make_synthetic_png(1280, 720);
    let (fmt, w, h) = ImageCodecValidator::sniff_and_validate(&png_bytes, &budgets, 1)
        .expect("valid PNG should pass");
    assert_eq!(fmt, ImageFormat::Png);
    assert_eq!(w, 1280);
    assert_eq!(h, 720);

    // 2. Valid synthetic GIF
    let gif_bytes = make_synthetic_gif(320, 240);
    let (fmt, w, h) = ImageCodecValidator::sniff_and_validate(&gif_bytes, &budgets, 2)
        .expect("valid GIF should pass");
    assert_eq!(fmt, ImageFormat::Gif);
    assert_eq!(w, 320);
    assert_eq!(h, 240);

    // 3. Valid synthetic JPEG
    let jpeg_bytes = make_synthetic_jpeg(800, 600);
    let (fmt, w, h) = ImageCodecValidator::sniff_and_validate(&jpeg_bytes, &budgets, 3)
        .expect("valid JPEG should pass");
    assert_eq!(fmt, ImageFormat::Jpeg);
    assert_eq!(w, 800);
    assert_eq!(h, 600);

    // 4. Truncated PNG IHDR
    let truncated_png = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 0x00];
    let err_trunc = ImageCodecValidator::sniff_and_validate(&truncated_png, &budgets, 4)
        .expect_err("truncated PNG should fail");
    match err_trunc {
        DocumentError::CorruptAssetPayload {
            request_id, reason, ..
        } => {
            assert_eq!(request_id, 4);
            assert!(reason.contains("truncated PNG IHDR"));
        }
        other => assert!(false, "Expected CorruptAssetPayload, got {:?}", other),
    }

    // 5. Zero dimensions PNG
    let zero_png = make_synthetic_png(0, 100);
    let err_zero = ImageCodecValidator::sniff_and_validate(&zero_png, &budgets, 5)
        .expect_err("zero dimension PNG should fail");
    match err_zero {
        DocumentError::CorruptAssetPayload { reason, .. } => {
            assert!(reason.contains("zero dimensions"));
        }
        other => assert!(false, "Expected CorruptAssetPayload for zero dim, got {:?}", other),
    }

    // 6. Unknown / corrupt signature (e.g. ELF executable or arbitrary bytes)
    let corrupt_bytes = vec![0x7F, b'E', b'L', b'F', 0x02, 0x01, 0x01, 0x00];
    let err_unknown = ImageCodecValidator::sniff_and_validate(&corrupt_bytes, &budgets, 6)
        .expect_err("unknown codec should fail");
    match err_unknown {
        DocumentError::CorruptAssetPayload { reason, .. } => {
            assert!(reason.contains("unrecognized or unsupported"));
        }
        other => assert!(false, "Expected CorruptAssetPayload, got {:?}", other),
    }

    // 7. Decompression bomb PNG (dimension in IHDR declared as 50,000 x 50,000)
    let bomb_png = make_synthetic_png(50_000, 50_000);
    let err_bomb = ImageCodecValidator::sniff_and_validate(&bomb_png, &budgets, 7)
        .expect_err("bomb PNG should fail");
    match err_bomb {
        DocumentError::DecompressionBomb { width, height, .. } => {
            assert_eq!(width, 50_000);
            assert_eq!(height, 50_000);
        }
        other => assert!(false, "Expected DecompressionBomb, got {:?}", other),
    }
}

#[test]
fn test_stale_generation_and_denied_asset_delivery() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 50);
    let gen1 = test_generation(owner, 1);
    let gen2 = test_generation(owner, 2);

    let budgets = BoundedAssetBudgets {
        max_pending_requests: 4,
        max_total_asset_bytes: 64 * 1024,
        max_single_asset_bytes: 16 * 1024,
        ..BoundedAssetBudgets::default()
    };

    let mut registry = BoundedAssetRegistry::new(doc_id, gen1, budgets);

    // Register an asset in generation 1
    let req = AssetRequest {
        id: AssetRequestId(10),
        kind: "image",
        url: "figures/arch.png".to_string(),
        source_offset: 120,
        generation: gen1.get(),
        estimated_width: 400,
        estimated_height: 300,
        alt_text: "System Architecture".to_string(),
    };
    registry
        .authorize_and_register(&req)
        .expect("registration");

    // Source fallback verification
    let fallback = registry.source_fallback(10).expect("source fallback");
    assert_eq!(fallback, "[Image: System Architecture (figures/arch.png)]");

    // Attempt to deliver resolution with stale generation 2 while current is 1
    let png_bytes = make_synthetic_png(400, 300);
    let stale_err = registry
        .deliver_resolution_validated(10, gen2, 400, 300, Some(png_bytes.clone()))
        .expect_err("stale generation should fail");
    match stale_err {
        DocumentError::StaleAssetGeneration { expected, actual } => {
            assert_eq!(expected, gen1);
            assert_eq!(actual, gen2);
        }
        other => assert!(false, "Expected StaleAssetGeneration, got {:?}", other),
    }

    // Now register a second request for denial testing
    let req_denied = AssetRequest {
        id: AssetRequestId(20),
        kind: "image",
        url: "secret/confidential.png".to_string(),
        source_offset: 200,
        generation: gen1.get(),
        estimated_width: 200,
        estimated_height: 200,
        alt_text: "Confidential Diagram".to_string(),
    };
    registry
        .authorize_and_register(&req_denied)
        .expect("register denied req");

    let denial_fallback = registry
        .deliver_denied(20, "policy access revoked")
        .expect("denial delivery");
    assert_eq!(
        denial_fallback,
        "[Confidential Diagram: policy access revoked]"
    );
    assert!(!registry.is_pending(20));

    // Valid resolution of request 10
    let res = registry
        .deliver_resolution_validated(10, gen1, 400, 300, Some(png_bytes))
        .expect("valid delivery");
    assert_eq!(res.request_id.0, 10);
    assert_eq!(res.generation, gen1.get());
    assert_eq!(res.width, 400);
    assert_eq!(res.height, 300);
    assert!(res.bytes.is_some());
    assert!(!registry.is_pending(10));
    assert!(registry.is_resolved(10));
}

#[test]
fn test_negative_control_oracle_and_error_codes() {
    let err_bomb = DocumentError::DecompressionBomb {
        width: 9000,
        height: 9000,
        reason: "too big".to_string(),
    };
    assert_eq!(err_bomb.code(), "DOCUMENT_DECOMPRESSION_BOMB");
    assert!(format!("{}", err_bomb).contains("decompression bomb rejected"));

    let err_corrupt = DocumentError::CorruptAssetPayload {
        request_id: 42,
        reason: "bad magic".to_string(),
    };
    assert_eq!(err_corrupt.code(), "DOCUMENT_CORRUPT_ASSET_PAYLOAD");
    assert!(format!("{}", err_corrupt).contains("corrupt asset payload"));

    let err_cycle = DocumentError::TransclusionCycle {
        path: "a.md".to_string(),
        chain: vec!["a.md".to_string()],
    };
    assert_eq!(err_cycle.code(), "DOCUMENT_TRANSCLUSION_CYCLE");
    assert!(format!("{}", err_cycle).contains("transclusion cycle detected"));

    let err_depth = DocumentError::TransclusionDepthExceeded { max_depth: 16 };
    assert_eq!(err_depth.code(), "DOCUMENT_TRANSCLUSION_DEPTH_EXCEEDED");
    assert!(format!("{}", err_depth).contains("transclusion nesting depth exceeds limit"));

    let err_denied = DocumentError::AssetDenied {
        request_id: 11,
        reason: "restricted".to_string(),
    };
    assert_eq!(err_denied.code(), "DOCUMENT_ASSET_DENIED");
    assert!(format!("{}", err_denied).contains("asset request 11 denied"));

    let err_net = DocumentError::NetworkDisabled {
        uri: "http://example.com".to_string(),
    };
    assert_eq!(err_net.code(), "DOCUMENT_NETWORK_DISABLED");
    assert!(format!("{}", err_net).contains("network disabled by default"));
}
