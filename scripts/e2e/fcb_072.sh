#!/bin/sh
# FCB-072.V joint verification campaign: Early bounded off-UI CPU retirement
# (FCB-072.A, fcb-core::retirement) + lossless GPU terminal record drain
# (FCB-072.B, fcb-runtime::terminal).
#
# Supported route (documented exact invocation), from the repository toplevel:
#   RCH_REQUIRE_REMOTE=1 rch exec --base HEAD --clean-overlay --no-overlay -- \
#     cargo test --manifest-path crates/fcb-core/Cargo.toml \
#       --test bounded_retirement
#     cargo test --manifest-path crates/fcb-runtime/Cargo.toml \
#       --test terminal_drain
#
# The campaign exercises BOTH children together: retirement progress
# surviving ordinary-memory saturation (last-Arc drop never stalls the UI)
# and lossless terminal record drain (one preallocated record per submit,
# coalesced wakes never losing completion state, retain-through-close with
# exactly-once release, negative control for defect detection).
set -eu
cd "$(dirname "$0")/../.."
echo "=== FCB-072.V campaign: CPU retirement (fcb-core) ==="
cargo test --manifest-path crates/fcb-core/Cargo.toml --test bounded_retirement
echo "=== FCB-072.V campaign: GPU terminal record drain (fcb-runtime) ==="
cargo test --manifest-path crates/fcb-runtime/Cargo.toml --test terminal_drain
