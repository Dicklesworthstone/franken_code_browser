#!/bin/bash
# FCB-082.V production verification scenario: old-capture anchor resolution and
# qualified huge-line visual-context routes (see crates/fcb-source/tests/
# old_anchor_lane.rs and huge_line_lane.rs for required cases).
#
# Supported routes:
#   scripts/e2e/fcb_082.sh [cargo-test-args...]
#   scripts/e2e/fcb_082.sh --lane anchor
#   scripts/e2e/fcb_082.sh --lane huge-line
#
# Environment:
#   FCB_082_RUN_ID    run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-082-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-082/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_082_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_082_RUN_ID="$RUN_ID"
export FCB_082_B_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-082/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

LANE="all"
EXTRA_ARGS=()

while [[ $# -gt 0 ]]; do
    case "$1" in
        --lane)
            LANE="$2"
            shift 2
            ;;
        *)
            EXTRA_ARGS+=("$1")
            shift
            ;;
    esac
done

echo "[fcb-082] route: headless cargo test (fcb-source)"
echo "[fcb-082] lane: $LANE"
echo "[fcb-082] run id: $RUN_ID"

if [ "$LANE" = "anchor" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-082] running old_anchor_lane..."
    cargo test --manifest-path crates/fcb-source/Cargo.toml \
        --test old_anchor_lane "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

if [ "$LANE" = "huge-line" ] || [ "$LANE" = "all" ]; then
    echo "[fcb-082] running huge_line_lane..."
    cargo test --manifest-path crates/fcb-source/Cargo.toml \
        --test huge_line_lane "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
fi

# Retain the bounded redacted receipts produced by this run.
RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-082-receipts-$RUN_ID"
RECEIPTS_B_DIR="${TMPDIR:-/tmp}/fcb-082-b-receipts-$RUN_ID"

mkdir -p "$ARTIFACT_DIR/receipts"
if [ -d "$RECEIPTS_DIR" ]; then
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
fi
if [ -d "$RECEIPTS_B_DIR" ]; then
    cp -R "$RECEIPTS_B_DIR/." "$ARTIFACT_DIR/receipts/"
fi
echo "[fcb-082] receipts archived: $ARTIFACT_DIR/receipts"

echo "[fcb-082] PASS (all required routes executed)"
