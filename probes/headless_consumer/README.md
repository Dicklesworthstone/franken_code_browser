# FCB headless consumer probe

This standalone, dependency-free Rust 2024 consumer is the smallest production
path for the selected platform contract. Its construction is inert: it does
not create a runtime, read the environment, scan a root, watch files, open a
window, access fonts, use the network, or write an export. The binary prints a
bounded JSON contract only after validating the compiled contract.

The pin is `nightly-2026-09-07`, target `aarch64-apple-darwin`, deployment
floor macOS 14.0, and SDK/Xcode baseline macOS SDK 26.1 / Xcode 26.1.1. The
read-only-root-grants sandbox model keeps IPC, watchers, fonts, exports, and
signing explicit host responsibilities. `scripts/platform_probe.py` observes
the installed metadata; it does not compile this consumer.

This probe is implementation evidence only. Compilation, native ABI, GPU,
signing, and physical-Mac qualification remain pending strict-RCH verification.
