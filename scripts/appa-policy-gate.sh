#!/usr/bin/env bash
# The OpenAPPA policy gate for ARES.
#
# Three checks, in order, all against the TEST posture (`config/appa.test.toml`):
#
#   1. `appa describe --check` — the complete configuration loads. Nonzero
#      unless it does.
#   2. `appa replay policy-tests/` — every scripted call matches its expected
#      decision. Exit 0 when all pass, 1 on any mismatch, 2 on a
#      could-not-run.
#   3. `appa replay policy-tests-negative/` — the intentionally mismatched
#      set MUST exit 1. A gate that cannot fail is not a gate; this arm is the
#      proof that the replay harness proves something.
#
# Usage: scripts/appa-policy-gate.sh [path-to-appa]
# The APPA CLI is built from the pinned source (see the `policy` job in
# .github/workflows/ci.yml); this script never installs anything itself.

set -uo pipefail

APPA="${1:-appa}"
CONFIG="config/appa.test.toml"
PASS_SET="policy-tests"
NEGATIVE_SET="policy-tests-negative"

if ! command -v "$APPA" >/dev/null 2>&1 && [ ! -x "$APPA" ]; then
    echo "appa-policy-gate: no appa CLI at '$APPA'; build it from the pinned source" >&2
    exit 2
fi

fail=0

echo "== 1/3 the complete test configuration loads"
"$APPA" describe --config "$CONFIG" --check
if [ $? -ne 0 ]; then
    echo "appa-policy-gate: describe --check failed for $CONFIG" >&2
    fail=1
fi

echo "== 2/3 the pass set replays clean"
"$APPA" replay --config "$CONFIG" "$PASS_SET"
pass_rc=$?
if [ $pass_rc -ne 0 ]; then
    echo "appa-policy-gate: the pass set exited $pass_rc" >&2
    fail=1
fi

echo "== 3/3 the intentionally mismatched set must fail (exit 1)"
"$APPA" replay --config "$CONFIG" "$NEGATIVE_SET"
negative_rc=$?
if [ $negative_rc -ne 1 ]; then
    echo "appa-policy-gate: the negative set exited $negative_rc, want 1 — the replay harness cannot fail, which proves nothing" >&2
    fail=1
fi

if [ $fail -ne 0 ]; then
    echo "appa-policy-gate: FAILED"
    exit 1
fi
echo "appa-policy-gate: ok (pass set clean, negative set fails as it must)"
