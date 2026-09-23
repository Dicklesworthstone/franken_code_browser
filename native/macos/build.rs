use std::{
    env,
    ffi::OsStr,
    path::PathBuf,
    process::Command,
    time::{SystemTime, UNIX_EPOCH},
};

fn main() {
    println!("cargo:rerun-if-env-changed=FCB_BRIDGE_LIB");
    for source in [
        "scripts/make_app_bundle.sh",
        "swiftui/Resources/AppIcon.icns",
        "swiftui/App.swift",
        "swiftui/AtlasCamera.swift",
        "swiftui/AtlasSource.swift",
        "swiftui/AtlasSearch.swift",
        "swiftui/AtlasMatch.swift",
        "swiftui/AtlasDocument.swift",
        "swiftui/AtlasPreparedText.swift",
        "swiftui/AtlasProjectCache.swift",
        "swiftui/AtlasMetalRasterRenderer.swift",
        "swiftui/AtlasMetalGlyphRenderer.swift",
        "swiftui/AtlasMetalPresentation.swift",
        "swiftui/AtlasRetainedView.swift",
        "swiftui/AtlasParcelLayout.swift",
    ] {
        println!("cargo:rerun-if-changed={source}");
    }
    if env::var_os("CARGO_FEATURE_SWIFTUI_APP").is_none() {
        return;
    }
    let swift_target = match env::var("TARGET").as_deref() {
        Ok("aarch64-apple-darwin") => "arm64-apple-macosx14.0",
        Ok("x86_64-apple-darwin") => "x86_64-apple-macosx14.0",
        _ => panic!("swiftui-app requires an Apple macOS target and a native Apple SDK"),
    };
    let bridge = PathBuf::from(env::var_os("FCB_BRIDGE_LIB").expect(
        "swiftui-app requires FCB_BRIDGE_LIB pointing to an explicitly built fcb-bridge archive",
    ));
    let bridge = bridge.canonicalize().expect("resolve FCB_BRIDGE_LIB");
    assert!(
        bridge.is_file(),
        "FCB_BRIDGE_LIB must be a regular archive file"
    );
    println!("cargo:rerun-if-changed={}", bridge.display());

    // Publish this opt-in application artifact beside the Cargo executables,
    // including the target triple/profile selected by the actual build.
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo OUT_DIR"));
    let profile = out
        .ancestors()
        .find(|path| path.file_name() == Some(OsStr::new("build")))
        .and_then(|path| path.parent())
        .expect("Cargo build directory must have a profile parent");
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system time before Unix epoch")
        .as_nanos();
    let bundle = profile.join(format!("FrankenCodeBrowser-{stamp}.app"));
    let status = Command::new("/bin/sh")
        .arg("scripts/make_app_bundle.sh")
        .current_dir(env::var_os("CARGO_MANIFEST_DIR").expect("Cargo manifest directory"))
        .env("FCB_BRIDGE_LIB", &bridge)
        .env("FCB_APP_OUTPUT", &bundle)
        .env("FCB_SWIFT_TARGET", swift_target)
        .status()
        .expect("launch SwiftUI application packaging");
    assert!(
        status.success(),
        "SwiftUI application packaging failed: {status}"
    );
    assert!(
        bundle.join("Contents/MacOS/FrankenCodeBrowser").is_file(),
        "missing application executable"
    );
    println!("cargo:warning=SwiftUI app bundle: {}", bundle.display());
}
