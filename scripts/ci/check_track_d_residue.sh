#!/usr/bin/env bash
set -euo pipefail

BASELINE_COMMIT="fd02588cd7623ae12596c174a359753650cb166a"
ROOT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
cd "$ROOT_DIR"

echo "=== Running Track D residue checks ([INV-RESIDUE-01]) ==="

# Check 1: FilterControls.tsx must have 0 occurrences of '来源' and 'Layers'
echo "[1/5] Checking FilterControls.tsx for '来源' and 'Layers'..."
if grep -q "来源" frontend/src/dashboard/FilterControls.tsx 2>/dev/null; then
    echo "ERROR: Found Chinese literal '来源' in frontend/src/dashboard/FilterControls.tsx" >&2
    exit 1
fi
if grep -q "Layers" frontend/src/dashboard/FilterControls.tsx 2>/dev/null; then
    echo "ERROR: Found symbol 'Layers' in frontend/src/dashboard/FilterControls.tsx" >&2
    exit 1
fi
echo "  -> PASSED"

# Check 2: src/antigravity/** must have 0 occurrences of 'immutable=1'
echo "[2/5] Checking src/antigravity for 'immutable=1'..."
if [ -d "src/antigravity" ]; then
    if grep -rq "immutable=1" src/antigravity 2>/dev/null; then
        echo "ERROR: Found 'immutable=1' in src/antigravity" >&2
        exit 1
    fi
fi
echo "  -> PASSED"

# Check 3: src/** must have 0 occurrences of symbol 'AntigravitySessionErrorSidecar'
echo "[3/5] Checking src for 'AntigravitySessionErrorSidecar'..."
if grep -rq "AntigravitySessionErrorSidecar" src 2>/dev/null; then
    echo "ERROR: Found symbol 'AntigravitySessionErrorSidecar' in src" >&2
    exit 1
fi
echo "  -> PASSED"

# Check 4: src/cost must have 0 case-insensitive whole-word occurrences of 'antigravity'
echo "[4/5] Checking src/cost for 'antigravity'..."
if [ -d "src/cost" ]; then
    set +e
    rg -iw 'antigravity' src/cost
    RG_EXIT=$?
    set -e
    if [ "$RG_EXIT" -eq 0 ]; then
        echo "ERROR: Found whole-word 'antigravity' in src/cost" >&2
        exit 1
    elif [ "$RG_EXIT" -ne 1 ]; then
        echo "ERROR: rg command failed with exit code $RG_EXIT" >&2
        exit "$RG_EXIT"
    fi
fi
echo "  -> PASSED"

# Check 5: src/cost relative to baseline must have no diff and no untracked files
echo "[5/5] Checking src/cost diff against baseline $BASELINE_COMMIT..."
if ! git diff --exit-code "$BASELINE_COMMIT" -- src/cost; then
    echo "ERROR: Tracked diff found in src/cost relative to baseline $BASELINE_COMMIT" >&2
    exit 1
fi

UNTRACKED_COST="$(git ls-files --others --exclude-standard -- src/cost)"
if [ -n "$UNTRACKED_COST" ]; then
    echo "ERROR: Untracked files found in src/cost:" >&2
    echo "$UNTRACKED_COST" >&2
    exit 1
fi
echo "  -> PASSED"

echo "=== All Track D residue checks PASSED ==="
