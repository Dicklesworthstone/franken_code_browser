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
- [x] Rebuilt source commit `4286f92` from this checkout and produced the final `FrankenCodeBrowser-macos-arm64.dmg`. Apple accepted notarization submission `dd298bb5-5c27-41ff-8864-8ab6bb9bdf0e`; stapler validation and `hdiutil verify` passed. SHA-256: `698293fc864d984fda3deea37769abd84adcc7fd44a9dbffd6bd9bc214244b76`.
- [x] Published [v0.1.0](https://github.com/Dicklesworthstone/franken_code_browser/releases/tag/v0.1.0) with the final DMG and matching `.sha256` sidecar. The installer fetched them through the public latest-release URL and installed into an isolated destination after checksum, code-signature, Team ID, and Gatekeeper checks.
- [x] Published the [Homebrew cask](https://github.com/Dicklesworthstone/homebrew-tap/blob/main/Casks/franken-code-browser.rb); `brew style --cask`, `brew audit --cask --strict`, and `brew fetch --cask` passed against the release.
- [ ] Check the final DMG as a quarantined download on a clean Mac, including online/offline first launch and drag-to-Applications installation. Local signing/notarization checks do not prove that matrix.

`--local-test` is only for checking the drag-and-drop image. It deliberately does not claim notarization. The earlier `3d8cece9ec98` image remains a historical local artifact; the `4286f92` image and checksum above are the published release.

## Mac App Store

- [x] Added a separate `FCB_APP_STORE=1` build with App Sandbox, user-selected read-only access, and app-scoped security bookmarks. A native launch selected the SwiftUI test folder, loaded 19 files, quit, reopened the project from its bookmark, and searched its source. The distribution-signed app also launched and read a selected Asupersync checkout (19,478 atlas files). This is a physical-Mac functional check, not a clean-machine or App Review result.
- [x] Registered Bundle ID `dev.frankencode.browser` with Apple; created Mac App Distribution and Mac Installer Distribution certificates, imported matching private keys into the local Keychain, and created Mac App Store provisioning profile `YC8HX93AMG`. The signing material stays outside Git.
- [x] Built a distribution-signed, sandboxed `FrankenCodeBrowser.app` and signed upload package with `scripts/package_macos_app_store.sh`. `codesign --verify --deep --strict`, `pkgutil --check-signature`, and package expansion passed. The local package is `dist/FrankenCodeBrowser-AppStore-0.1.0-3-upload.pkg` (SHA-256 `be25d980a1071e0df4fcbe39cd4e1a29b78529d8a45c8524b15b21e7e59cb4f6`). This package is an upload candidate, not a public download.
- [x] Captured a real Mac screenshot from the signed app viewing the public Asupersync repository: `assets/app-store/01-asupersync-atlas.png` is 2880 × 1800 RGB. Draft listing and review text are in [APP_STORE_LISTING.md](APP_STORE_LISTING.md); [privacy](PRIVACY.md) now describes the native build.
- [ ] Create the App Store Connect app record. Apple's app-record creation requires a signed-in App Store Connect web session; the available API-key session cannot create one. Upload this signed package after the record exists.
- [ ] Complete App Store Connect product-page metadata, app privacy, age rating, pricing/availability, review contact, and export-compliance answers; attach the screenshot and processed build. Run a TestFlight or equivalent distribution check, then submit the version for App Review.

Build the separate sandboxed app with `FCB_APP_STORE=1 ./scripts/build_macos_app.sh --output /absolute/new/path.app`, then use `scripts/package_macos_app_store.sh --help` for the signing inputs. The direct-download DMG remains a separate Developer ID route.

The App Store build cannot simply reuse the Developer ID-signed DMG. Apple's [App Sandbox guide](https://developer.apple.com/documentation/xcode/configuring-the-macos-app-sandbox), [distribution preparation guide](https://developer.apple.com/documentation/xcode/preparing-your-app-for-distribution), and [submission guide](https://developer.apple.com/help/app-store-connect/manage-submissions-to-app-review/submit-an-app) describe the current requirements.
