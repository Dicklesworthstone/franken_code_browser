# Distribution status

The native source is a developer preview. A Developer ID-signed DMG and a Mac App Store upload have different signing and review paths.

## Direct download

- [x] The Rust bridge and native SwiftUI/Metal app source are in this checkout; `scripts/build_macos_app.sh` provides the one-checkout `.app` build path. The fresh build remains unverified below.
- [x] App bundle packaging and icon are implemented.
- [x] A Developer ID Application identity is available on the development machine.
- [x] `scripts/package_macos_dmg.sh` stages an app plus an `/Applications` alias, signs the staged app when an identity is supplied, verifies the image, and prints its SHA-256.
- [ ] Rebuild a release app from this exact combined source revision and verify it on a physical Mac. The last local app bundle predates the current feature shell; it is not a release build. Disk-pressure preflight currently blocks a fresh compile on the development machine.
- [ ] Configure a notarytool Keychain profile or approved App Store Connect API-key credentials. The `FrankenCodeBrowser` profile was absent during the 2026-09-22 preflight; other profile names were not audited.
- [ ] Submit the exact DMG to Apple's notary service, staple the accepted ticket, and verify Gatekeeper on the resulting download.
- [ ] Publish the verified DMG and checksum as a GitHub Release.

`--local-test` is only for checking the drag-and-drop image. It deliberately does not claim notarization.

## Mac App Store

- [ ] Add and qualify the App Sandbox entitlement. Project selection, remembered roots, source reads through the Rust bridge, and cache persistence all need sandbox-safe behavior, including restoration of file access after relaunch.
- [ ] Register a matching App ID (`dev.frankencode.browser` or a consciously chosen replacement), create the Mac App Store record, and obtain the proper Mac distribution identity/profile. The present Keychain has Developer ID and development identities, but no Mac Distribution identity was found at preflight.
- [ ] Create a distribution-signed archive/build with the required app metadata, including copyright and privacy answers.
- [ ] Capture real Mac screenshots, complete store listing and review metadata, upload the build, run a TestFlight or equivalent distribution check, then submit the version for App Review.

The App Store build cannot simply reuse the Developer ID-signed DMG. Apple's [App Sandbox guide](https://developer.apple.com/documentation/xcode/configuring-the-macos-app-sandbox), [distribution preparation guide](https://developer.apple.com/documentation/xcode/preparing-your-app-for-distribution), and [submission guide](https://developer.apple.com/help/app-store-connect/manage-submissions-to-app-review/submit-an-app) describe the current requirements.
