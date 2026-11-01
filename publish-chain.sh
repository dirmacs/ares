#!/usr/bin/env bash
# Publish the ares crate chain to crates.io, safely.
#
# crates.io has no delete: a published version is permanent and only yankable.
# This wrapper exists for two reasons:
#
#  1. Guard the tree. Cargo's dirty-tree check is PER-PACKAGE, not tree-wide:
#     measured on this workspace, editing `src/main.rs` did NOT stop
#     `cargo publish --dry-run -p ares-store` (exit 0). So an uncommitted change
#     anywhere can ride along inside whichever crate happens to package it.
#     That is worse than it sounds: a stray edit in a shared module ships.
#
#  2. Fail fast and loudly, before an irreversible step.
#
#  3. Show the packaged file list for review. Several crates have no `exclude`
#     list, so `cargo package` ships their `tests/` directory:
#
#         ares-http    74 files, 15 test entries
#         ares-server 140 files, 22 test entries
#         ares-agent   42 files,  6 test entries
#         ares-llm     26 files,  4 test entries
#         ares-store / ares-mcp / ares-rag / ares-tools: none
#
#     Verified before the first live publish: dirmacs/ares is a PUBLIC GitHub
#     repo (api.github.com/repos/dirmacs/ares -> private: false), so those test
#     files are already published in the repository. Excluding them from the
#     tarball would buy no confidentiality. It is a footprint decision, not a
#     disclosure one, and it is left alone deliberately rather than silently.
#
#     What does matter is that the packaged surface carries no secrets and no
#     OSS-guard blocklisted identifiers, because the commit hook does not run on
#     `cargo publish`. Both were scanned across the packaged files of every
#     crate with a planted positive control proving the scanners fire:
#     0 secret-shaped hits, 0 blocked-pattern hits.
#
#     That scan originally covered the eight crates then pending. The set has
#     since grown: the train is thirteen publishable members, and ares-vector
#     carries its own version history (0.1.x), so it is compared against its own
#     manifest version rather than the shared 0.11.9 one.
#
# We do NOT re-implement sequencing. `cargo publish --workspace` already walks
# the dependency graph in order and waits for the index. Hand-rolling that was
# strictly worse; it was deleted in favour of this guard around the builtin.
#
# Usage:
#   ./publish-chain.sh              # dry-run the whole workspace (nothing uploads)
#   ./publish-chain.sh --live       # actually publish
#
# Never pass --allow-dirty here, and do not add it as an escape hatch. If the
# guard fires, commit or stash the change.
set -euo pipefail

cd "$(git rev-parse --show-toplevel)"

export RUSTUP_TOOLCHAIN=1.99.0

LIVE=0
case "${1:-}" in
  --live) LIVE=1 ;;
  "") ;;
  *) echo "unknown argument: $1 (use --live to publish for real)" >&2; exit 2 ;;
esac

VERSION=0.11.9

# --- Guard 1: clean tree ------------------------------------------------------
if [ -n "$(git status --porcelain)" ]; then
  echo "REFUSING: the working tree is dirty." >&2
  echo >&2
  git status --short >&2
  echo >&2
  echo "Cargo's own dirty check is per-package, so an uncommitted change can be" >&2
  echo "packaged and published inside a crate it does not belong to." >&2
  echo "Commit or stash first. Do not add --allow-dirty." >&2
  exit 1
fi

# --- Guard 2: nothing publishable may depend on a publish=false crate ---------
# Cargo's error for this is clear but arrives per-package, after several uploads
# have already happened when using --workspace.
echo "==> checking for edges into publish=false crates"
violations=$(cargo metadata --no-deps --format-version 1 \
  | python3 -c '
import sys, json
d = json.load(sys.stdin)
members = {p["name"]: p for p in d["packages"]}
unpublished = {n for n, p in members.items() if p.get("publish") == []}
bad = [
    (p["name"], dep["name"])
    for p in d["packages"]
    if p.get("publish") != []
    for dep in p["dependencies"]
    if dep["name"] in unpublished
]
for src, dst in bad:
    print(f"{src} -> {dst}")
')
if [ -n "$violations" ]; then
  echo "REFUSING: these publishable crates depend on a publish=false crate:" >&2
  echo "$violations" | sed 's/^/  /' >&2
  exit 1
fi
echo "    ok"

# --- Guard 3: version is not already taken -----------------------------------
# Re-publishing an existing version is an error, but only after packaging.
echo "==> checking that ${CRATE_VERSION_CHECK:-0.11.9} is not already published"
    stale=$(python3 -c '
import json, urllib.request
# Membership is derived from `cargo metadata` below, not listed here. This guard
# is a courtesy notice printed before packaging; a hardcoded list would go stale
# every time a crate joined the train, which has already happened twice.
import subprocess
d = json.loads(subprocess.run(["cargo", "metadata", "--no-deps",
                               "--format-version", "1"],
                              capture_output=True, text=True).stdout)
for m in sorted({p["name"] for p in d["packages"] if p.get("publish") != []}):
    req = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{m}",
        headers={"User-Agent": "ares-publish-guard"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            data = json.load(r)
        cur = data.get("crate", {}).get("max_version", "?")
    except Exception:
        cur = "?"
    if cur == "0.11.9":
        print(f"{m} already at 0.11.9")
')
if [ -n "$stale" ]; then
  echo "NOTE: some crates are already at 0.11.9; they will be skipped by cargo:" >&2
  echo "$stale" | sed 's/^/  /' >&2
fi

# --- Guard 4: show the packaged surface, so it is reviewed before upload ------
# `cargo publish` does not run the repo's commit guard, so this is the last
# point at which the shipped bytes can be inspected.
if [ "${SHOW_MANIFEST:-0}" = "1" ]; then
  echo "==> packaged files per crate"
  for c in ares-store ares-mcp ares-llm ares-rag ares-tools ares-agent ares-http ares-server; do
    total=$(cargo package --list -p "$c" 2>/dev/null | wc -l)
    tests=$(cargo package --list -p "$c" 2>/dev/null | grep -cE '^(tests|benches|examples)/' || true)
    printf '    %-14s %4s files, %2s test/bench/example\n' "$c" "$total" "$tests"
  done
fi

# --- Publish ------------------------------------------------------------------
if [ "$LIVE" -eq 1 ]; then
  # `cargo publish --workspace` cannot be used live here: it attempts every
  # member, including the ones already at this version, and aborts on the first
  # with "crate ares-types@0.11.9 already exists" before uploading anything.
  # So the live path publishes an explicit, dependency-ordered list of only the
  # crates that still need this version. The list is derived, not hardcoded.
  echo
  echo "==> computing crates that still need $VERSION"
  # The publish SET and its ORDER both come from live data, not a literal list:
  # order from `cargo metadata`'s dependency graph (topological), membership
  # from the crates.io index. A hardcoded order would silently skip any crate
  # added later, which is exactly the class of mistake this train has already
  # produced twice.
  todo=$(cargo metadata --no-deps --format-version 1 | python3 -c '
import json, sys, urllib.request

d = json.load(sys.stdin)
members = {p["name"]: p for p in d["packages"]}
publishable = {n for n, p in members.items() if p.get("publish") != []}

# Edges: which publishable crates depend on which other publishable crates.
deps = {n: set() for n in publishable}
for name in publishable:
    for dep in members[name]["dependencies"]:
        if dep["name"] in publishable:
            deps[name].add(dep["name"])

# Kahn topological sort; ties broken alphabetically for determinism.
order, remaining = [], {n: set(v) for n, v in deps.items()}
while remaining:
    ready = sorted(n for n, ds in remaining.items() if not ds)
    if not ready:
        raise SystemExit("cycle in the dependency graph: " + ", ".join(sorted(remaining)))
    for n in ready:
        order.append(n)
        del remaining[n]
    for ds in remaining.values():
        ds.difference_update(ready)

# Compare each crate against ITS OWN manifest version, never a shared constant:
# ares-vector is versioned 0.1.7 independently of the 0.11.9 family, so a single
# $VERSION comparison would wrongly queue it for republishing.
for crate in order:
    want = members[crate]["version"]
    req = urllib.request.Request(
        f"https://crates.io/api/v1/crates/{crate}",
        headers={"User-Agent": "ares-publish-guard"},
    )
    try:
        with urllib.request.urlopen(req, timeout=15) as r:
            cur = json.load(r).get("crate", {}).get("max_version", "")
    except Exception:
        cur = ""
    if cur != want:
        print(crate)
' "$VERSION"
)
  if [ -z "$todo" ]; then
    echo "    nothing to do; all crates already at $VERSION"
    exit 0
  fi
  echo "$todo" | sed 's/^/    /'

  echo
  echo "==> LIVE PUBLISH. This is irreversible; crates.io cannot delete a version."
  echo "    Publishing: $(echo "$todo" | tr '\n' ' ')"
  echo "    Press Ctrl-C within 15s to abort."
  sleep 15

  while IFS= read -r crate; do
    [ -z "$crate" ] && continue
    # Each crate's own manifest version — ares-vector is 0.1.7 while the rest of
    # the family is 0.11.9, so waiting on a shared constant would time out.
    want=$(cargo metadata --no-deps --format-version 1 \
      | python3 -c '
import json, sys
for p in json.load(sys.stdin)["packages"]:
    if p["name"] == sys.argv[1]:
        print(p["version"])
' "$crate")
    echo
    echo "==> $crate $want"
    cargo publish -p "$crate"
    # The index is a CDN and lags the upload; the next crate's
    # `ares-<prev> = "^..."` requirement cannot resolve until this lands.
    echo "    waiting for $crate $want on the index..."
    ok=0
    for _ in $(seq 1 60); do
      got=$(curl -s --max-time 10 -H 'User-Agent: ares-publish-guard' \
            "https://crates.io/api/v1/crates/$crate" \
            | python3 -c 'import sys,json;print(json.load(sys.stdin).get("crate",{}).get("max_version",""))' 2>/dev/null || true)
      if [ "$got" = "$want" ]; then ok=1; break; fi
      sleep 5
    done
    if [ "$ok" -ne 1 ]; then
      echo "REFUSING to continue: $crate $want did not appear on the index." >&2
      echo "Remaining crates were NOT published." >&2
      exit 1
    fi
    echo "    ok"
  done <<< "$todo"

  echo
  echo "==> verify"
  for c in ares-store ares-mcp ares-llm ares-rag ares-tools ares-agent ares-http ares-server; do
    printf '  %-14s ' "$c"
    curl -s --max-time 10 -H 'User-Agent: ares-publish-guard' \
      "https://crates.io/api/v1/crates/$c" \
      | python3 -c 'import sys,json;print(json.load(sys.stdin).get("crate",{}).get("max_version","?"))'
  done
else
  # Dry-run CAN use --workspace: it resolves dependencies from the workspace
  # itself rather than the index, so it validates the whole chain in one pass
  # even when nothing is published yet.
  echo
  echo "==> DRY RUN (nothing uploads)"
  cargo publish --workspace --dry-run
  echo
  echo "Dry run complete. Re-run with --live to publish."
fi
