#!/bin/bash
# FCB-036.V production verification scenario:
# FCB source/preview/split panels consuming upstream selection and provenance,
# truthful rendered vs Markdown copy, disjoint/contiguous classification,
# pre-publication budget refusal, encoding preservation, and anchor synchronization.
#
# Supported routes:
#   scripts/e2e/fcb_036.sh [cargo-test-args...]
#   scripts/e2e/fcb_036.sh --lane copy
#   scripts/e2e/fcb_036.sh --lane production
#   scripts/e2e/fcb_036.sh --lane all
#
# Environment:
#   FCB_036_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-036-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-036/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_036_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_036_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-036/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-036-receipts-$RUN_ID"
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

echo "[fcb-036] route: headless cargo test (fcb-document/fcb_036_production, truthful_rendered_and_markdown_selection_copy)"
echo "[fcb-036] lane: $LANE"
echo "[fcb-036] run id: $RUN_ID"

case "$LANE" in
    copy)
        echo "[fcb-036] running truthful_rendered_and_markdown_selection_copy tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test truthful_rendered_and_markdown_selection_copy "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-036] running fcb_036_production tests..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_036_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-036] running all FCB-036 test suites..."
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test truthful_rendered_and_markdown_selection_copy "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb-document/Cargo.toml \
            --test fcb_036_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-036] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-036] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 10 ]; then
            echo "[fcb-036] ERROR: expected at least 10 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

echo "[fcb-036] PASS (all required routes executed)"
