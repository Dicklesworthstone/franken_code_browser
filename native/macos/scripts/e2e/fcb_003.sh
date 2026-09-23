#!/bin/sh
# FCB-003.V joint verification campaign: First-party macOS object/ABI
# ownership kernel (FCB-003.A audited Apple ABI and object ownership kernel
# + FCB-003.B instance-safe callbacks and native registration).
#
# Supported route: NATIVE Apple Silicon execution. This crate binds AppKit
# and Metal; Linux workers cannot compile or run it. The script therefore
# requires an admitted native macOS execution lane; do not bypass the configured Cargo shim.
#
# Exact invocation from the repository toplevel:
#   ./scripts/e2e/fcb_003.sh
#
# The campaign exercises both layers together:
#   1. Unit & oracle test suites (cargo test --lib + cargo test --test ownership_kernel)
#      - Retain/release counters track owned wrappers
#      - Retain count overflow refused before native call
#      - Exception policy explicitly unqualified
#      - Attach/detach is idempotence-safe and preserves ownership
#      - Wrong thread/affinity rejected before attach
#      - Instance-safe unique class name generation & collision rejection
#      - Consumer tag validation
#      - Tombstoning of issued class names for process lifetime
#      - Exclusive access during invocation
#      - Nested reentrancy rejection and guard release on unwind
#      - Shutdown retires state exactly once and rejects late invocations
#      - Isolation: shutting down one instance never affects another
#      - Wrong affinity invocation rejected and counted
#   2. Native host main-thread live qualification (src/bin/main_thread_qualification.rs)
#      - MainThreadToken capture on true AppKit main thread
#      - Real CAMetalLayer construction with ownership snapshot pairing
#      - Clone retain / drop release pairing on real object
#      - Real MTLCreateSystemDefaultDevice adopted exactly once
#      - Real host main thread CallbackCell invocation, reentrancy rejection,
#        aggregate counters, and late-shutdown rejection
#      - Bounded redacted output and exit 0 outcome
set -eu
cd "$(dirname "$0")/../.."

echo "=== FCB-003.V campaign: object/ABI ownership kernel + callbacks ==="
echo "--- 1. Library tests (ownership & callbacks) ---"
cargo test --manifest-path Cargo.toml --lib -- --nocapture

echo "--- 2. Ownership kernel test suite ---"
cargo test --manifest-path Cargo.toml --test ownership_kernel -- --nocapture

echo "--- 3. Host main-thread live qualification ---"
cargo run --manifest-path Cargo.toml --bin main-thread-qualification

echo "=== FCB-003.V PASS (all required routes executed) ==="
