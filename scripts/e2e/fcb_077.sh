#!/bin/bash
# FCB-077.V production verification scenario:
# Early real native source-reader accessibility and keyboard/IME smoke path.
# Real small multi-script source (ASCII, CJK multibyte, astral emoji, combining mark),
# VoiceOver/AX route, virtualized pending text range resolver, subrange bounds,
# hit-testing, focus walk, intermediate marked text search query suppression,
# actual source multi-flavor clipboard round-trip, host responder chain,
# and intentional negative controls (sentinel refusal, wrong offset, budget limit).
#
# Supported routes:
#   scripts/e2e/fcb_077.sh [cargo-test-args...]
#   scripts/e2e/fcb_077.sh --lane production
#   scripts/e2e/fcb_077.sh --lane smoke
#   scripts/e2e/fcb_077.sh --lane runtime
#   scripts/e2e/fcb_077.sh --lane all
#
# Environment:
#   FCB_077_RUN_ID  run identifier for receipt retention (default: UTC ts)
#
# Evidence: bounded redacted ScenarioReceipts retained under
#   ${TMPDIR:-/tmp}/fcb-077-receipts-<run_id>/ and archived after the run
#   under scripts/e2e/artifacts/fcb-077/<run_id>/receipts/.
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$REPO_ROOT"

RUN_ID="${FCB_077_RUN_ID:-$(date -u +%Y%m%dT%H%M%SZ)}"
export FCB_077_RUN_ID="$RUN_ID"

ARTIFACT_DIR="scripts/e2e/artifacts/fcb-077/$RUN_ID"
mkdir -p "$ARTIFACT_DIR"

RECEIPTS_DIR="${TMPDIR:-/tmp}/fcb-077-receipts-$RUN_ID"
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

echo "[fcb-077] route: headless cargo test (fcb_077_production, accessibility_smoke, native_focus_ime_text)"
echo "[fcb-077] lane: $LANE"
echo "[fcb-077] run id: $RUN_ID"

case "$LANE" in
    smoke)
        echo "[fcb-077] running accessibility_smoke test..."
        cargo test --manifest-path crates/fcb-conformance/Cargo.toml \
            --test accessibility_smoke "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    runtime)
        echo "[fcb-077] running native_focus_ime_text test..."
        cargo test --manifest-path crates/fcb-runtime/Cargo.toml \
            --test native_focus_ime_text "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    production)
        echo "[fcb-077] running fcb_077_production tests..."
        cargo test --manifest-path crates/fcb-runtime/Cargo.toml \
            --test fcb_077_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    all)
        echo "[fcb-077] running all FCB-077 test suites..."
        cargo test --manifest-path crates/fcb-conformance/Cargo.toml \
            --test accessibility_smoke "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb-runtime/Cargo.toml \
            --test native_focus_ime_text "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        cargo test --manifest-path crates/fcb-runtime/Cargo.toml \
            --test fcb_077_production "${EXTRA_ARGS[@]+"${EXTRA_ARGS[@]}"}"
        ;;
    *)
        echo "[fcb-077] unknown lane: $LANE" >&2
        exit 1
        ;;
esac

# Retain the bounded redacted receipts produced by this run.
if [ -d "$RECEIPTS_DIR" ]; then
    mkdir -p "$ARTIFACT_DIR/receipts"
    cp -R "$RECEIPTS_DIR/." "$ARTIFACT_DIR/receipts/"
    RECEIPT_COUNT=$(find "$ARTIFACT_DIR/receipts" -name "*.receipt" | wc -l | tr -d ' ')
    echo "[fcb-077] receipts archived ($RECEIPT_COUNT receipts): $ARTIFACT_DIR/receipts"
    if [ "$LANE" = "production" ] || [ "$LANE" = "all" ]; then
        if [ "$RECEIPT_COUNT" -lt 10 ]; then
            echo "[fcb-077] ERROR: expected at least 10 receipts, got $RECEIPT_COUNT" >&2
            exit 1
        fi
    fi
fi

echo "[fcb-077] PASS (all required routes executed)"
