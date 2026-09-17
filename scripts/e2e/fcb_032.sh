#!/bin/bash
# FCB-032.V production verification scenario:
# FrankenMarkdown-owned paragraph/list/quote/heading flow, checked height indexing,
# fractional point accumulation beyond 2³², stable scroll anchoring across resize/edits,
# background refinement transactions, and upstream API closure.
#
# Supported routes:
#   scripts/e2e/fcb_032.sh [cargo-test-args...]
#   scripts/e2e/fcb_032.sh --lane flow
#   scripts/e2e/fcb_032.sh --lane refinement
#   scripts/e2e/fcb_032.sh --lane production
#   scripts/e2e/fcb_032.sh --lane all
#
# Environment:
#   FCB_032_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-032-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-032/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_032_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_032_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-032/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-032-receipts-$RUN_ID"
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

echo "[fcb-032] route: headless cargo test (fcb_032_production, continuous_block_flow, paged_height_refinement)"
echo "[fcb-032] lane: $LANE"
echo "[fcb-032] run id: $RUN_ID"

case "$LANE" in
    flow)
        echo "[fcb-032] running continuous_block_flow_and_scroll_anchoring tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test continuous_block_flow_and_scroll_anchoring "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    refinement)
        echo "[fcb-032] running paged_height_refinement_consumer tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test paged_height_refinement_consumer "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-032] running fcb_032_production tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_032_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-032] running all FCB-032 test suites..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test continuous_block_flow_and_scroll_anchoring "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test paged_height_refinement_consumer "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_032_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-032] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-032] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 10 ]; then
            echo "[fcb-032] ERROR: expected at least 10 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

echo "[fcb-032] PASS (all required routes executed)"
