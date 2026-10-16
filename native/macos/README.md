# Native macOS app

This directory contains the SwiftUI/Metal FrankenCodeBrowser app and the narrow `franken-macos` Rust facade. It is part of the **same `franken_code_browser` repository** as the engine. The app links the workspace's `fcb-bridge` static library; no second checkout is needed.

From the repository root on macOS 14+:

```sh
./scripts/install_macos_app.sh --build
```

To inspect a built app before installation, run `APP="$(./scripts/build_macos_app.sh)"`,
then `open "$APP"` and `./scripts/install_macos_app.sh "$APP"` if desired.

## CLI to native project workflow

Direct-distribution builds now include the matching `fcb` executable at
`Contents/MacOS/fcb`. The build script builds the bridge and CLI together, signs
the nested executable before sealing the bundle, and the installer preserves it.
No shell profile, PATH directory, or existing command is changed automatically.

```sh
APP="$(./scripts/build_macos_app.sh)"
"$APP/Contents/MacOS/fcb" open /absolute/path/to/project
"$APP/Contents/MacOS/fcb" .
"$APP/Contents/MacOS/fcb" read README.md --json
```

On macOS, bare `fcb` launches/activates a compatible app. `fcb ROOT` and
`fcb open ROOT` start a new native instance with that explicit project directory.
Relative roots are resolved against the invoking directory. The existing native
`ContentView` consumes the per-launch `fcb.root` argument-domain override; the
launcher does not rewrite the preferences database or execute anything from the
project. Search, catalog discovery and reader admission remain native engine work.

A separately installed CLI locates the app using `FCB_APP_BUNDLE` (an absolute
`.app` path), its own enclosing bundle, `~/Applications/FrankenCodeBrowser.app`,
then `/Applications/FrankenCodeBrowser.app`. An invalid explicit override or
invalid enclosing bundle is an error, never permission to launch another copy.
For a local build in a nonstandard location:

```sh
FCB_APP_BUNDLE="$APP" fcb open /absolute/path/to/project
```

The launcher requires a versioned marker created by the current bundler. Older
bundles must be rebuilt. App Store builds advertise native-picker access, not
command-line root access, and omit the unsandboxed CLI from the app. Supplying a
root to that mode is refused: launch the app without a root and use its folder
picker/security-scoped access flow.

`--json`, `read`, `search`, `desk` and other machine commands retain their headless
routes. The reusable `fcb_app::run` service does not start native applications;
this handoff belongs only to the macOS executable. Other platforms remain
headless. Native activation of an individual file is **not connected yet**;
`fcb open FILE` reports that limitation instead of silently opening the wrong
scope. Use `fcb read FILE`, `fcb open FILE --json`, or open its parent project and
select it in the native catalog. Non-UTF-8 project names are refused by the
UTF-8 native startup path rather than being lossily rewritten.

`CLI_NATIVE_LAUNCH_SUBMITTED` means Launch Services accepted the handoff, **not**
that project discovery finished or a frame was presented. A canceled/timed-out
handoff can already have launched a window; inspect it before retrying. The
launcher reaps only its own `open` helper, never that window. Native compilation,
physical-Mac launch tests and product-gate qualification remain pending.

The portable packaging contract checks can be run with:

```sh
python3 scripts/test_native_launch_packaging.py
```

They execute the actual packaging scripts with fake compiler/signing tools and
retain their fixture logs. They establish packaging control flow, not native
compilation, code-signing validity, Launch Services behavior or rendering.

## Local test image

For a local drag-to-Applications test image:

```sh
./scripts/package_macos_dmg.sh --app "$APP" \
  --output "$PWD/dist/FrankenCodeBrowser-local-test.dmg" --local-test
```

The script refuses to replace an existing output. `--local-test` is not notarized and is not a public release. See the root [README](../../README.md) and [distribution status](../../DISTRIBUTION.md) for the supported build and release paths.

The `swiftui/` sources own the project picker, atlas, search, reader, prepared-text cache, and retained Metal presentation. `src/` owns typed Apple object and callback lifetimes for native consumers; it does not own source discovery, parsing, or search policy. The native sources were imported from the former local `franken_macos` checkout at commit `8ffd16f795e9064753d95d28988b7433c0132f8e`; that checkout is preserved as a history archive, not needed for this build.
