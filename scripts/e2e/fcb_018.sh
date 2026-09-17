#!/bin/bash
# FCB-018.V production verification scenario: Retained rectangle/glyph/clip
# renderer, atlas and text frame composition, CPU geometry oracle, and independent
# invalidation axes.
#
# Supported routes:
#   scripts/e2e/fcb_018.sh [cargo-test-args...]
#   scripts/e2e/fcb_018.sh --lane atlas
#   scripts/e2e/fcb_018.sh --lane text
#   scripts/e2e/fcb_018.sh --lane invalidation
#   scripts/e2e/fcb_018.sh --lane all
#
# Environment:
#   FCB_018_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-018-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-018/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_018_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_018_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-018/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-018-receipts-$RUN_ID"
mkdir -p "$RECEIPTS_DIR"
export FCB_RECEIPTS_DIR="$RECEIPTS_DIR"

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

echo "[fcb-018] route: headless cargo test (fcb_018_production)"
echo "[fcb-018] lane: $LANE"
echo "[fcb-018] run id: $RUN_ID"

case "$LANE" in
    atlas)
        echo "[fcb-018] running atlas frame tests..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test fcb_018_production -- atlas_frame_composition "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    text)
        echo "[fcb-018] running text frame tests..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test fcb_018_production -- text_frame_composition "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    invalidation)
        echo "[fcb-018] running invalidation and ordering tests..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test fcb_018_production -- invalidation "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-018] running all fcb_018_production tests..."
        cargo test --manifest-path crates/fcb/Cargo.toml \
            --test fcb_018_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-018] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-018] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$RECEIPT_COUNT" -eq 0 ]; then
        echo "[fcb-018] ERROR: no receipts generated" >&2
        exit 1
    fi
fi

echo "[fcb-018] PASS (all required routes executed)"
