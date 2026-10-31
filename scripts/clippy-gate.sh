#!/usr/bin/env bash
# Clippy gate: fail when the workspace gains warnings.
#
# The usual form of this gate is `cargo clippy -- -D warnings`, and on a clean
# tree that is the right one. This tree is not clean. `ares-agent` carries 27
# warnings at the time this gate was added, and they are not noise: 12 unused
# functions, 4 unused structs and 3 unused methods in `pipeline.rs`,
# `resolver.rs` and `scheduler.rs`, which look like an abandoned
# cost-aggregation subsystem. `-D warnings` would make the job red on its first
# run, and a gate that is red the day it lands is a gate people learn to ignore.
#
# So this ratchets instead. The warning count is recorded in
# `scripts/clippy-baseline.txt`. CI passes when the count is at or below the
# baseline and fails when it rises. New code therefore cannot add a warning,
# while the existing debt stays visible and counted rather than hidden behind
# a blanket `allow`.
#
# It also fails when the count DROPS without the baseline being updated, which
# is the part that matters more than it looks. A stale baseline slowly becomes
# permission: fix twenty warnings, leave the baseline alone, and the next
# twenty can be reintroduced without the gate noticing.
#
# What this deliberately does NOT do:
#
#   - It does not gate on `--all-features`. The 27 warnings appear on the
#     default build; `--all-features` adds more in feature-gated paths that the
#     workspace does not build by default. Gating a configuration nobody builds
#     measures the wrong tree.
#   - It does not count warnings per lint. The ratchet is on the total, because
#     the goal is "no new debt", not "this particular lint is banned".
#   - It does not run `clippy --fix`. Fixes are a decision about the abandoned
#     subsystem, and that belongs to whoever owns it.
#
# Usage:
#   scripts/clippy-gate.sh              # check against the baseline
#   scripts/clippy-gate.sh --update     # rewrite the baseline (deliberate act)
set -uo pipefail

cd "$(dirname "$0")/.." || exit 2
BASELINE_FILE="scripts/clippy-baseline.txt"

if ! command -v cargo >/dev/null 2>&1; then
  echo "clippy-gate: cargo not found" >&2
  exit 2
fi

echo "clippy-gate: running cargo clippy --workspace --all-targets"
# The warning lines are the summary form each crate emits:
#   warning: `ares-agent` (lib) generated 27 warnings (run `cargo clippy ...`)
# Counting those totals avoids double-counting the per-diagnostic lines, which
# clippy repeats in the crate summary anyway. The cache makes reruns cheap.
OUT=$(cargo clippy --workspace --all-targets 2>&1)
RC=$?
if [ $RC -ne 0 ]; then
  # A hard error (not a warning) is always a failure, independent of the count.
  #
  # Strip ANSI before matching. CI sets CARGO_TERM_COLOR=always, so every
  # diagnostic line arrives as \e[1m\e[31merror\e[0m... and a plain `^error`
  # anchor matches nothing -- the job then reports "a compile error" with no
  # error text, which is how this went undiagnosed. Match the coloured form
  # too, so the report survives either CARGO_TERM_COLOR setting.
  CLEAN=$(printf '%s\n' "$OUT" | sed $'s/\033\\[[0-9;]*m//g')
  printf '%s\n' "$CLEAN" | grep -E "^error" | head -20
  echo "clippy-gate: clippy exited $RC; a compile error is not a warning budget"
  exit 1
fi

# Clippy counts per target, and reports two numbers in the same line shape:
#
#   warning: `ares-agent` (lib)      generated 27 warnings (...)
#   warning: `ares-agent` (lib test) generated 25 warnings (22 duplicates) (...)
#
# Summing the first number double-counts: the 25 includes 22 warnings the lib
# target already reported, so 27 + 25 = 52 overstates the tree by 22. When this
# note was written the deduplicated total was sum(generated) - sum(duplicates)
# = 30. Getting this wrong would have set the baseline 22 too high on the first
# run, which is exactly the silent permission the ratchet exists to prevent.
# Count with awk, not python3. The runner image has NO python3 and NO python
# (verified on dirmacs-runner:14: "command -v python3" -> not found, while jq,
# sed and awk are present). With python3 missing the old counter produced an
# EMPTY $ACTUAL, both later integer comparisons errored out, and the script fell
# through to "unchanged from baseline" and exited 0 -- a gate that passed every
# tree it was pointed at. Fail closed instead: no number is a gate failure.
ACTUAL=$(printf '%s\n' "$OUT" | awk '
  match($0, /generated [0-9]+ warnings?/) {
    g = substr($0, RSTART + 10, RLENGTH - 10)
    sub(/ warnings?$/, "", g)
    gen += g
    if (match($0, /\(([0-9]+) duplicates?\)/)) {
      dup += substr($0, RSTART + 1, RLENGTH - 2) + 0
    }
  }
  END {
    if (gen == 0 && dup == 0) { print ""; exit }
    print gen - dup
  }
')

if ! printf '%s' "$ACTUAL" | grep -Eq '^[0-9]+$'; then
  echo "clippy-gate: FAILED, could not compute a warning count" >&2
  echo "  The gate will not report a pass it did not measure. Clippy output was:" >&2
  printf '%s\n' "$OUT" | tail -20 >&2
  exit 1
fi

if [ ! -f "$BASELINE_FILE" ]; then
  echo "clippy-gate: no baseline at $BASELINE_FILE; writing $ACTUAL"
  echo "$ACTUAL" > "$BASELINE_FILE"
  exit 0
fi
BASELINE=$(tr -dc '0-9' < "$BASELINE_FILE")

if [ "${1:-}" = "--update" ]; then
  printf '%s\n' "$ACTUAL" > "$BASELINE_FILE"
  echo "clippy-gate: baseline updated $BASELINE -> $ACTUAL"
  exit 0
fi

echo "clippy-gate: baseline $BASELINE, actual $ACTUAL"

if [ "$ACTUAL" -gt "$BASELINE" ]; then
  echo "clippy-gate: FAILED, $((ACTUAL - BASELINE)) new warning(s)"
  echo
  echo "  The warnings this change added:"
  echo "$OUT" | grep -E "^warning: " | grep -vE "generated [0-9]+ warnings?" | head -20
  echo
  echo "  Fix them, or if they are deliberate, raise the baseline with"
  echo "    scripts/clippy-gate.sh --update"
  echo "  and say why in the commit message. Raising it silently is how a"
  echo "  ratchet becomes decoration."
  exit 1
fi

if [ "$ACTUAL" -lt "$BASELINE" ]; then
  echo "clippy-gate: FAILED, the tree improved ($BASELINE -> $ACTUAL) but the baseline is stale"
  echo
  echo "  A baseline above the real count is permission to reintroduce that many"
  echo "  warnings. Lower it to lock the improvement in:"
  echo "    scripts/clippy-gate.sh --update"
  exit 1
fi

echo "clippy-gate: $ACTUAL warning(s), unchanged from baseline"
exit 0
