//! Native integration of the actual Swift shell and its production camera/source
//! adapters. A native Cargo target gives RCH an honest platform requirement.
#![cfg(target_os = "macos")]

use std::{path::PathBuf, process::Command};

fn checked(command: &mut Command) {
    eprintln!("native Swift command: {command:?}");
    let output = command.output().expect("launch native Swift tool");
    eprint!("{}", String::from_utf8_lossy(&output.stderr));
    print!("{}", String::from_utf8_lossy(&output.stdout));
    assert!(
        output.status.success(),
        "native Swift command failed: {}",
        output.status
    );
}

#[test]
fn production_swift_camera_source_and_shell() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!(
        "fcb-swift-native-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&scratch).unwrap();
    for name in ["AtlasCamera", "AtlasSource", "AtlasSearch"] {
        let binary = scratch.join(name);
        checked(
            Command::new("xcrun")
                .current_dir(&root)
                .args(["swiftc", "-parse-as-library"])
                .arg(format!("swiftui/{name}.swift"))
                .arg(format!("swiftui/tests/{name}Tests.swift"))
                .arg("-o")
                .arg(&binary),
        );
        checked(&mut Command::new(binary));
    }
    let glyph_test = scratch.join("AtlasDocument");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/tests/AtlasDocumentTests.swift",
                "-o",
            ])
            .arg(&glyph_test),
    );
    checked(&mut Command::new(glyph_test));
    let bridge = std::env::var("FCB_BRIDGE_LIB")
        .expect("native shell tests require the real FCB bridge archive");
    let bridge_test = scratch.join("AtlasBridge");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/AtlasSearch.swift",
                "swiftui/AtlasMatch.swift",
                "swiftui/tests/AtlasBridgeTests.swift",
            ])
            .arg(&bridge)
            .arg("-o")
            .arg(&bridge_test),
    );
    checked(&mut Command::new(bridge_test));
    let parcel_test = scratch.join("AtlasParcelLayout");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/AtlasParcelLayout.swift",
                "swiftui/tests/AtlasParcelLayoutTests.swift",
            ])
            .arg(&bridge)
            .arg("-o")
            .arg(&parcel_test),
    );
    checked(&mut Command::new(parcel_test));
    let match_test = scratch.join("AtlasMatch");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/AtlasMatch.swift",
                "swiftui/tests/AtlasMatchTests.swift",
            ])
            .arg(&bridge)
            .arg("-o")
            .arg(&match_test),
    );
    checked(&mut Command::new(match_test));
    let prepared_test = scratch.join("AtlasPreparedText");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/AtlasSearch.swift",
                "swiftui/AtlasSearchCoordinator.swift",
                "swiftui/AtlasProjectIO.swift",
                "swiftui/AtlasProjectWorker.swift",
                "swiftui/AtlasProjectCache.swift",
                "swiftui/tests/AtlasPreparedTextTests.swift",
            ])
            .arg(&bridge)
            .arg("-o")
            .arg(&prepared_test),
    );
    checked(&mut Command::new(prepared_test));

    let project_io_test = scratch.join("AtlasProjectIO");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSearch.swift",
                "swiftui/AtlasSearchCoordinator.swift",
                "swiftui/AtlasProjectIO.swift",
                "swiftui/tests/AtlasProjectIOTests.swift",
                "-o",
            ])
            .arg(&project_io_test),
    );
    checked(&mut Command::new(project_io_test));

    let project_worker_test = scratch.join("AtlasProjectWorker");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSearch.swift",
                "swiftui/AtlasSearchCoordinator.swift",
                "swiftui/AtlasProjectIO.swift",
                "swiftui/AtlasProjectWorker.swift",
                "swiftui/tests/AtlasProjectWorkerTests.swift",
            ])
            .arg(&bridge)
            .arg("-o")
            .arg(&project_worker_test),
    );
    checked(&mut Command::new(project_worker_test));

    checked(Command::new("xcrun").current_dir(&root).args([
        "swiftc",
        "-typecheck",
        "-parse-as-library",
        "swiftui/AtlasCamera.swift",
        "swiftui/AtlasSource.swift",
        "swiftui/AtlasSourceReader.swift",
        "swiftui/AtlasSearch.swift",
        "swiftui/AtlasSearchCoordinator.swift",
        "swiftui/AtlasSearchWorker.swift",
        "swiftui/AtlasProjectIO.swift",
        "swiftui/AtlasProjectWorker.swift",
        "swiftui/AtlasMatch.swift",
        "swiftui/AtlasDocument.swift",
        "swiftui/AtlasPreparedText.swift",
        "swiftui/AtlasProjectCache.swift",
        "swiftui/AppStoreRootAccess.swift",
        "swiftui/AtlasParcelLayout.swift",
        "swiftui/AtlasMetalRasterRenderer.swift",
        "swiftui/AtlasMetalGlyphRenderer.swift",
        "swiftui/AtlasMetalPresentation.swift",
        "swiftui/AtlasRetainedView.swift",
        "swiftui/App.swift",
    ]));
}

#[test]
fn offscreen_metal_glyph_quality() {
    let root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let scratch = std::env::temp_dir().join(format!(
        "fcb-metal-native-{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir(&scratch).unwrap();
    let metal_test = scratch.join("AtlasMetalGlyph");
    checked(
        Command::new("xcrun")
            .current_dir(&root)
            .args([
                "swiftc",
                "-parse-as-library",
                "swiftui/AtlasSource.swift",
                "swiftui/AtlasDocument.swift",
                "swiftui/AtlasPreparedText.swift",
                "swiftui/AtlasMetalRasterRenderer.swift",
                "swiftui/AtlasMetalGlyphRenderer.swift",
                "swiftui/tests/AtlasMetalGlyphTests.swift",
            ])
            .arg("-o")
            .arg(&metal_test),
    );
    checked(&mut Command::new(metal_test));
}
