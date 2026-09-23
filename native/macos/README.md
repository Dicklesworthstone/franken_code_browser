# Native macOS app

This directory contains the SwiftUI/Metal FrankenCodeBrowser app and the narrow `franken-macos` Rust facade. It is part of the **same `franken_code_browser` repository** as the engine. The app links the workspace's `fcb-bridge` static library; no second checkout is needed.

From the repository root on macOS 14+:

```sh
./scripts/install_macos_app.sh --build
```

To inspect a built app before installation, run `APP="$(./scripts/build_macos_app.sh)"`,
then `open "$APP"` and `./scripts/install_macos_app.sh "$APP"` if desired.

For a local drag-to-Applications test image:

```sh
./scripts/package_macos_dmg.sh --app "$APP" \
  --output "$PWD/dist/FrankenCodeBrowser-local-test.dmg" --local-test
```

The script refuses to replace an existing output. `--local-test` is not notarized and is not a public release. See the root [README](../../README.md) and [distribution status](../../DISTRIBUTION.md) for the supported build and release paths.

The `swiftui/` sources own the project picker, atlas, search, reader, prepared-text cache, and retained Metal presentation. `src/` owns typed Apple object and callback lifetimes for native consumers; it does not own source discovery, parsing, or search policy. The native sources were imported from the former local `franken_macos` checkout at commit `8ffd16f795e9064753d95d28988b7433c0132f8e`; that checkout is preserved as a history archive, not needed for this build.
