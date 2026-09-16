#![forbid(unsafe_code)]

use fcb_core::{ArenaOwnerId, DocumentGeneration, DocumentId};
use fcb_document::{
    AssetRequest, AssetRequestId, BoundedAssetBudgets, BoundedAssetRegistry, BoundedImageDecoder,
    DecodedImage, DocumentError, ImageCacheKey, ImageFormat, PrivateImageCache,
};

fn test_owner() -> ArenaOwnerId {
    ArenaOwnerId::new(202).unwrap()
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

/// Constructs a synthetic multi-frame GIF with `frame_count` frames.
fn make_synthetic_multiframe_gif(width: u16, height: u16, frame_count: usize) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(b"GIF89a");
    bytes.extend_from_slice(&width.to_le_bytes());
    bytes.extend_from_slice(&height.to_le_bytes());
    // GCT flag: 0x80 (present), 2 entries (size 6 bytes)
    bytes.extend_from_slice(&[0x80, 0x00, 0x00]);
    // 2 RGB colors (black and white)
    bytes.extend_from_slice(&[0, 0, 0, 0xFF, 0xFF, 0xFF]);

    for _ in 0..frame_count {
        // Graphic Control Extension (0x21, 0xF9, len 4, packed, delay 2 bytes, transp idx, terminator 0)
        bytes.extend_from_slice(&[0x21, 0xF9, 0x04, 0x00, 0x05, 0x00, 0x00, 0x00]);
        // Image Descriptor (0x2C, left 2, top 2, width 2, height 2, packed 0)
        bytes.push(0x2C);
        bytes.extend_from_slice(&[0, 0, 0, 0]);
        bytes.extend_from_slice(&width.to_le_bytes());
        bytes.extend_from_slice(&height.to_le_bytes());
        bytes.push(0x00);
        // LZW minimum code size
        bytes.push(2);
        // Single data sub-block: len 1, byte 0, terminator 0
        bytes.extend_from_slice(&[1, 0, 0]);
    }
    // Trailer
    bytes.push(0x3B);
    bytes
}

#[test]
fn test_image_format_capabilities_and_metadata() {
    let supported = ImageFormat::all_supported();
    assert_eq!(supported.len(), 5);
    assert!(supported.contains(&ImageFormat::Png));
    assert!(supported.contains(&ImageFormat::Jpeg));
    assert!(supported.contains(&ImageFormat::Gif));
    assert!(supported.contains(&ImageFormat::Webp));
    assert!(supported.contains(&ImageFormat::Svg));

    // Check PNG
    let png_caps = ImageFormat::Png.capabilities();
    assert_eq!(png_caps.mime_type, "image/png");
    assert!(png_caps.file_extensions.contains(&"png"));
    assert!(png_caps.supports_animation);
    assert!(!png_caps.is_vector);
    assert!(png_caps.decoder_available);

    // Check JPEG
    let jpeg_caps = ImageFormat::Jpeg.capabilities();
    assert_eq!(jpeg_caps.mime_type, "image/jpeg");
    assert!(jpeg_caps.file_extensions.contains(&"jpg"));
    assert!(jpeg_caps.file_extensions.contains(&"jpeg"));
    assert!(!jpeg_caps.supports_animation);
    assert!(!jpeg_caps.is_vector);
    assert!(jpeg_caps.decoder_available);

    // Check GIF
    let gif_caps = ImageFormat::Gif.capabilities();
    assert_eq!(gif_caps.mime_type, "image/gif");
    assert!(gif_caps.supports_animation);

    // Check SVG
    let svg_caps = ImageFormat::Svg.capabilities();
    assert_eq!(svg_caps.mime_type, "image/svg+xml");
    assert!(svg_caps.is_vector);
    assert!(!svg_caps.supports_animation);
}

#[test]
fn test_display_resolution_decode_downsampling() {
    let budgets = BoundedAssetBudgets {
        max_image_dimension: 8192,
        max_decoded_pixels: 32 * 1024 * 1024,
        max_decoded_bytes: 128 * 1024 * 1024,
        ..BoundedAssetBudgets::default()
    };

    // 1. High-resolution synthetic PNG: 4000 x 3000 (12 megapixels)
    let native_w = 4000u32;
    let native_h = 3000u32;
    let payload = make_synthetic_png(native_w, native_h);

    // 2. Decode with thumbnail/display constraints: 400 x 300
    let target_w = 400u32;
    let target_h = 300u32;
    let decoded = BoundedImageDecoder::decode(&payload, Some((target_w, target_h)), &budgets, 1)
        .expect("decode should succeed");

    assert_eq!(decoded.native_width, native_w);
    assert_eq!(decoded.native_height, native_h);
    assert_eq!(decoded.display_width, target_w);
    assert_eq!(decoded.display_height, target_h);

    // Verify 100x memory savings:
    // Native uncompressed would be 4000 * 3000 * 4 = 48,000,000 bytes (48 MB)
    // Display resolution retained is 400 * 300 * 4 = 480,000 bytes (480 KB)
    assert_eq!(decoded.memory_bytes(), 400 * 300 * 4);
    assert!(decoded.memory_bytes() < 500_000);
    assert!(decoded.is_valid());

    // 3. Decode without target constraints: retains native dimensions
    let native_decoded = BoundedImageDecoder::decode(&payload, None, &budgets, 2)
        .expect("native decode should succeed");
    assert_eq!(native_decoded.display_width, native_w);
    assert_eq!(native_decoded.display_height, native_h);
    assert_eq!(native_decoded.memory_bytes(), (native_w as usize) * (native_h as usize) * 4);
}

#[test]
fn test_frame_count_limits_and_multi_frame_defense() {
    let budgets = BoundedAssetBudgets {
        max_frame_count: 8,
        ..BoundedAssetBudgets::default()
    };

    // 1. Valid multi-frame GIF with 5 frames (under limit 8)
    let gif_5_frames = make_synthetic_multiframe_gif(64, 64, 5);
    let count = BoundedImageDecoder::sniff_frame_count(&gif_5_frames, ImageFormat::Gif, 10)
        .expect("sniff frame count");
    assert_eq!(count, 5);

    let decoded = BoundedImageDecoder::decode(&gif_5_frames, None, &budgets, 10)
        .expect("decoding 5-frame GIF should pass");
    assert_eq!(decoded.frame_count, 5);
    assert!(decoded.is_animated);

    // 2. Hostile animated GIF with 15 frames (exceeds budget limit 8)
    let gif_15_frames = make_synthetic_multiframe_gif(64, 64, 15);
    let err = BoundedImageDecoder::decode(&gif_15_frames, None, &budgets, 11)
        .expect_err("decoding 15-frame GIF should exceed budget");

    match err {
        DocumentError::FrameCountExceeded { frame_count, max_frames } => {
            assert_eq!(frame_count, 15);
            assert_eq!(max_frames, 8);
            assert_eq!(err.code(), "DOCUMENT_FRAME_COUNT_EXCEEDED");
            assert!(format!("{}", err).contains("image frame count 15 exceeds limit 8"));
        }
        other => assert!(false, "Expected FrameCountExceeded, got {:?}", other),
    }
}

#[test]
fn test_private_image_cache_accounting_and_lru_eviction() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 1);
    let gen1 = test_generation(owner, 1);

    // Cache capacity: 100,000 bytes (~100 KB)
    let mut cache = PrivateImageCache::new(100_000);
    assert_eq!(cache.max_bytes(), 100_000);
    assert_eq!(cache.current_bytes(), 0);
    assert_eq!(cache.entry_count(), 0);

    // Create 3 images, each 100 x 100 RGBA = 40,000 bytes
    let make_image = |req_id: u64| -> (ImageCacheKey, DecodedImage) {
        let key = ImageCacheKey {
            document_id: doc_id,
            request_id: req_id,
            generation: gen1,
            display_width: 100,
            display_height: 100,
        };
        let img = DecodedImage {
            format: ImageFormat::Png,
            native_width: 100,
            native_height: 100,
            display_width: 100,
            display_height: 100,
            rgba_bytes: vec![0u8; 40_000],
            is_animated: false,
            frame_count: 1,
        };
        (key, img)
    };

    let (k1, img1) = make_image(1);
    let (k2, img2) = make_image(2);
    let (k3, img3) = make_image(3);

    // Insert image 1: 40,000 bytes
    cache.insert(k1.clone(), img1).expect("insert 1");
    assert_eq!(cache.current_bytes(), 40_000);
    assert_eq!(cache.entry_count(), 1);

    // Insert image 2: 80,000 bytes
    cache.insert(k2.clone(), img2).expect("insert 2");
    assert_eq!(cache.current_bytes(), 80_000);
    assert_eq!(cache.entry_count(), 2);

    // Access image 1 so image 2 becomes the least recently used
    assert!(cache.get(&k1).is_some());

    // Insert image 3: 40,000 bytes. Total would be 120,000 > 100,000.
    // Image 2 should be evicted because Image 1 was accessed more recently.
    cache.insert(k3.clone(), img3).expect("insert 3");
    assert_eq!(cache.current_bytes(), 80_000);
    assert_eq!(cache.entry_count(), 2);

    // Image 1 and Image 3 should be present; Image 2 should have been evicted
    assert!(cache.contains(&k1));
    assert!(!cache.contains(&k2));
    assert!(cache.contains(&k3));

    // Inserting an oversized image that exceeds max_bytes completely is rejected
    let oversized_key = ImageCacheKey {
        document_id: doc_id,
        request_id: 999,
        generation: gen1,
        display_width: 200,
        display_height: 200,
    };
    let oversized_img = DecodedImage {
        format: ImageFormat::Png,
        native_width: 200,
        native_height: 200,
        display_width: 200,
        display_height: 200,
        rgba_bytes: vec![0u8; 160_000], // 160 KB > 100 KB limit
        is_animated: false,
        frame_count: 1,
    };
    let err = cache.insert(oversized_key, oversized_img).expect_err("oversized insert");
    match err {
        DocumentError::AssetBudgetExceeded { reason } => {
            assert!(reason.contains("exceeds maximum cache capacity"));
        }
        other => assert!(false, "Expected AssetBudgetExceeded, got {:?}", other),
    }
}

#[test]
fn test_generational_and_workspace_cache_eviction() {
    let owner = test_owner();
    let doc1 = test_doc_id(owner, 1);
    let doc2 = test_doc_id(owner, 2);
    let gen1 = test_generation(owner, 1);
    let gen2 = test_generation(owner, 2);

    let mut cache = PrivateImageCache::new(500_000);

    let img = DecodedImage {
        format: ImageFormat::Png,
        native_width: 50,
        native_height: 50,
        display_width: 50,
        display_height: 50,
        rgba_bytes: vec![0u8; 10_000],
        is_animated: false,
        frame_count: 1,
    };

    let k1 = ImageCacheKey {
        document_id: doc1,
        request_id: 1,
        generation: gen1,
        display_width: 50,
        display_height: 50,
    };
    let k2 = ImageCacheKey {
        document_id: doc1,
        request_id: 2,
        generation: gen2,
        display_width: 50,
        display_height: 50,
    };
    let k3 = ImageCacheKey {
        document_id: doc2,
        request_id: 1,
        generation: gen1,
        display_width: 50,
        display_height: 50,
    };

    cache.insert(k1.clone(), img.clone()).expect("k1");
    cache.insert(k2.clone(), img.clone()).expect("k2");
    cache.insert(k3.clone(), img).expect("k3");
    assert_eq!(cache.entry_count(), 3);
    assert_eq!(cache.current_bytes(), 30_000);

    // 1. Evict older generations for doc1 (min generation 2)
    let evicted = cache.evict_older_generations(doc1, gen2);
    assert_eq!(evicted, 1);
    assert_eq!(cache.entry_count(), 2);
    assert_eq!(cache.current_bytes(), 20_000);
    assert!(!cache.contains(&k1)); // Gen 1 for doc1 evicted
    assert!(cache.contains(&k2));  // Gen 2 for doc1 kept
    assert!(cache.contains(&k3));  // Doc2 kept unaffected

    // 2. Evict document 2 (e.g. document close)
    let evicted_doc = cache.evict_document(doc2);
    assert_eq!(evicted_doc, 1);
    assert_eq!(cache.entry_count(), 1);
    assert_eq!(cache.current_bytes(), 10_000);
    assert!(!cache.contains(&k3));

    // 3. Purge entire workspace
    let purged = cache.purge_workspace();
    assert_eq!(purged, 1);
    assert_eq!(cache.entry_count(), 0);
    assert_eq!(cache.current_bytes(), 0);
}

#[test]
fn test_registry_deliver_resolution_decoded_lifecycle() {
    let owner = test_owner();
    let doc_id = test_doc_id(owner, 10);
    let gen1 = test_generation(owner, 1);
    let gen2 = test_generation(owner, 2);

    let budgets = BoundedAssetBudgets {
        max_pending_requests: 4,
        max_total_asset_bytes: 256 * 1024,
        max_single_asset_bytes: 64 * 1024,
        ..BoundedAssetBudgets::default()
    };

    let mut registry = BoundedAssetRegistry::new(doc_id, gen1, budgets);

    let req = AssetRequest {
        id: AssetRequestId(101),
        kind: "image",
        url: "media/photo.jpg".to_string(),
        source_offset: 40,
        generation: gen1.get(),
        estimated_width: 800,
        estimated_height: 600,
        alt_text: "Photo".to_string(),
    };
    registry.authorize_and_register(&req).expect("register");

    let jpeg_payload = make_synthetic_jpeg(800, 600);

    // Deliver decoded with display bounds 400x300
    let (res, decoded) = registry
        .deliver_resolution_decoded(101, gen1, Some((400, 300)), jpeg_payload)
        .expect("deliver decoded");

    assert_eq!(res.request_id.0, 101);
    assert_eq!(res.width, 400);
    assert_eq!(res.height, 300);
    assert_eq!(decoded.display_width, 400);
    assert_eq!(decoded.display_height, 300);
    assert_eq!(decoded.native_width, 800);
    assert_eq!(decoded.native_height, 600);

    // Verify it is cached in the registry's private cache
    assert_eq!(registry.image_cache().entry_count(), 1);
    assert_eq!(registry.image_cache().current_bytes(), 400 * 300 * 4);

    // Advance to generation 2
    registry.advance_generation(gen2);
    assert_eq!(registry.current_generation(), gen2);
    // Cached entry from gen1 should have been evicted automatically
    assert_eq!(registry.image_cache().entry_count(), 0);
    assert_eq!(registry.image_cache().current_bytes(), 0);

    // Workspace purge
    registry.purge_workspace();
    assert_eq!(registry.pending_count(), 0);
    assert_eq!(registry.resolved_count(), 0);
    assert_eq!(registry.total_resolved_bytes(), 0);
}

#[test]
fn test_hostile_input_bombs_and_corrupt_payload_defense() {
    let budgets = BoundedAssetBudgets {
        max_image_dimension: 8192,
        max_decoded_pixels: 32 * 1024 * 1024,
        max_decoded_bytes: 64 * 1024 * 1024,
        ..BoundedAssetBudgets::default()
    };

    // 1. Extreme dimension bomb in PNG header (60,000 x 60,000)
    let bomb_png = make_synthetic_png(60_000, 60_000);
    let err_dim = BoundedImageDecoder::decode(&bomb_png, None, &budgets, 1)
        .expect_err("dimension bomb rejected");
    match err_dim {
        DocumentError::DecompressionBomb { width, height, reason } => {
            assert_eq!(width, 60_000);
            assert_eq!(height, 60_000);
            assert!(reason.contains("dimension exceeds maximum"));
        }
        other => assert!(false, "Expected DecompressionBomb, got {:?}", other),
    }

    // 2. Truncated PNG payload
    let truncated = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A];
    let err_trunc = BoundedImageDecoder::decode(&truncated, None, &budgets, 2)
        .expect_err("truncated payload rejected");
    match err_trunc {
        DocumentError::CorruptAssetPayload { request_id, .. } => {
            assert_eq!(request_id, 2);
        }
        other => assert!(false, "Expected CorruptAssetPayload, got {:?}", other),
    }

    // 3. Corrupt magic signature
    let corrupt = vec![0x00, 0x01, 0x02, 0x03, 0x04, 0x05];
    let err_corrupt = BoundedImageDecoder::decode(&corrupt, None, &budgets, 3)
        .expect_err("corrupt signature rejected");
    match err_corrupt {
        DocumentError::CorruptAssetPayload { request_id, .. } => {
            assert_eq!(request_id, 3);
        }
        other => assert!(false, "Expected CorruptAssetPayload, got {:?}", other),
    }
}

#[test]
fn test_negative_control_oracle() {
    // 1. Validate error code and message for FrameCountExceeded
    let err_frame = DocumentError::FrameCountExceeded {
        frame_count: 500,
        max_frames: 64,
    };
    assert_eq!(err_frame.code(), "DOCUMENT_FRAME_COUNT_EXCEEDED");
    assert!(format!("{}", err_frame).contains("image frame count 500 exceeds limit 64"));

    // 2. Decoder refuses to invent valid images from arbitrary garbage
    let budgets = BoundedAssetBudgets::default();
    let garbage = b"malicious binary payload without valid headers";
    let res = BoundedImageDecoder::decode(garbage, None, &budgets, 99);
    assert!(res.is_err());

    // 3. Frame count sniffer defaults to 1 for non-animated codecs
    let png = make_synthetic_png(100, 100);
    let frames = BoundedImageDecoder::sniff_frame_count(&png, ImageFormat::Png, 100)
        .expect("png frame count");
    assert_eq!(frames, 1);
}
