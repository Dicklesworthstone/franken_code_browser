//! Measures real native bitmap drawing, explicitly not display presentation.
#![cfg(target_os = "macos")]
use std::{path::PathBuf, process::Command};
fn checked(command: &mut Command) {
    eprintln!("render profile command: {command:?}");
    assert!(
        command
            .status()
            .expect("execute native render profile")
            .success()
    );
}
#[test]
#[ignore = "requires explicit immutable corpus, oracle, native bridge and project cache"]
fn profile_native_rendering() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let bridge = std::env::var("FCB_BRIDGE_LIB").expect("verified bridge archive");
    let corpus = std::env::var("FCB_PROFILE_CORPUS").expect("frozen corpus");
    let oracle = std::env::var("FCB_PROFILE_ORACLE").expect("original content oracle");
    let cache = std::env::var("FCB_PROFILE_CACHE").expect("real APFS project cache");
    let scratch = std::env::temp_dir().join(format!("fcb-render-profile-{}", std::process::id()));
    std::fs::create_dir(&scratch).expect("new private output folder");
    let binary = scratch.join("AtlasRenderProfile");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-O",
                "-g",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasMatch.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/AtlasProjectCache.swift",
                "swiftui/AtlasMetalRasterRenderer.swift",
                "swiftui/AtlasMetalGlyphRenderer.swift",
                "swiftui/AtlasMetalPresentation.swift",
                "swiftui/AtlasRetainedView.swift",
                "swiftui/AtlasParcelLayout.swift",
                "swiftui/tests/AtlasMetalRasterTests.swift",
                "swiftui/tests/AtlasMetalImageOracle.swift",
                "swiftui/tests/AtlasContinuousZoomProfile.swift",
                "swiftui/tests/AtlasRenderProfile.swift",
            ])
            .arg(bridge)
            .arg("-o")
            .arg(&binary),
    );
    checked(
        Command::new("/usr/bin/time")
            .arg("-l")
            .arg(binary)
            .arg(corpus)
            .arg(oracle)
            .arg(cache),
    );
}
