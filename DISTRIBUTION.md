# Distribution status

The native source is a developer preview. A Developer ID-signed DMG and a Mac App Store upload have different signing and review paths.

## Direct download

- [x] The Rust bridge and native SwiftUI/Metal app source are in this checkout; `scripts/build_macos_app.sh` provides the one-checkout `.app` build path.
- [x] App bundle packaging and icon are implemented.
- [x] A Developer ID Application identity is available on the development machine.
- [x] `scripts/package_macos_dmg.sh` stages an app plus an `/Applications` alias, signs the staged app, verifies the image, and prints its SHA-256. It supports an authenticated `asc` CLI or a `notarytool` Keychain profile for notarization.
- [x] Built commit `3d8cece9ec98` from this checkout on the physical `mac-mini-old`, installed and opened it on `~/projects/asupersync`, and observed the 20,619-file atlas. This is a native launch check, not the clean-machine distribution matrix.
- [x] The authenticated `asc` API-key route submitted the local `FrankenCodeBrowser-0.1.0-3d8cece9ec98-notarized-macos-arm64.dmg`. Apple returned **Accepted** (submission `42ff7bad-9507-41a8-8261-79e269e3a1c4`); stapling and ticket validation succeeded. SHA-256: `34aa86c0a86a15909937087554af860eb6722efa035623bc8456fd9ed25a1e39`.
- [x] The staged app passed `codesign --verify --deep --strict` and local `spctl --assess --type execute` reported `Notarized Developer ID`. The image also passed `hdiutil verify`.
- [ ] Check the final DMG as a quarantined download on a clean Mac, including online/offline first launch and drag-to-Applications installation. Local signing/notarization checks do not prove that matrix.
- [ ] Publish the verified DMG and checksum as a GitHub Release.

`--local-test` is only for checking the drag-and-drop image. It deliberately does not claim notarization. The signed/notarized artifact above remains local; there is no public release download yet.

## Mac App Store

- [ ] Add and qualify the App Sandbox entitlement. Project selection, remembered roots, source reads through the Rust bridge, and cache persistence all need sandbox-safe behavior, including restoration of file access after relaunch.
- [ ] Register a matching App ID (`dev.frankencode.browser` or a consciously chosen replacement), create the Mac App Store record, and obtain the proper Mac distribution identity/profile. The present Keychain has Developer ID and development identities, but no Mac Distribution identity was found at preflight.
- [ ] Create a distribution-signed archive/build with the required app metadata, including copyright and privacy answers.
- [ ] Capture real Mac screenshots, complete store listing and review metadata, upload the build, run a TestFlight or equivalent distribution check, then submit the version for App Review.

The App Store build cannot simply reuse the Developer ID-signed DMG. Apple's [App Sandbox guide](https://developer.apple.com/documentation/xcode/configuring-the-macos-app-sandbox), [distribution preparation guide](https://developer.apple.com/documentation/xcode/preparing-your-app-for-distribution), and [submission guide](https://developer.apple.com/help/app-store-connect/manage-submissions-to-app-review/submit-an-app) describe the current requirements.
