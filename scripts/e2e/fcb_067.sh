#!/bin/sh
# FCB-067.V joint conformance campaign (host provider + capture capability
# types, in-memory provider conformance boundary).
#
# Supported route (documented exact invocation): strict remote execution
# through the RCH committed-tree form, run from the repository toplevel:
#
#   RCH_REQUIRE_REMOTE=1 rch exec --base HEAD --clean-overlay --no-overlay -- \
#     cargo test --manifest-path crates/fcb-source/Cargo.toml \
#       --test fcb_067_scenario
#
# The script itself delegates to cargo for local convenience runs; remote
# execution evidence for closure must come from the strict form above.
set -eu
exec cargo test --manifest-path crates/fcb-source/Cargo.toml --test fcb_067_scenario -- --nocapture
