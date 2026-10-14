# ARES 0.11.7 release provenance

Published to crates.io 2026-10-14. Tag `v0.11.7` on origin points at
`5f8be30` (annotated tag `4cd56093`, "ARES 0.11.7", bkataru, 2026-09-19).

## Provenance table (from each published tarball's .cargo_vcs_info.json)

| crate | version | sha1 | dirty | reproducible from |
|---|---|---|---|---|
| ares-cordis | 0.11.7 | 5f8be30b8 | **true** | NO — uncommitted state |
| ares-vector | 0.1.5 | — | — | 9624cda (0.1.4→0.1.5 bump) |
| ares-types | 0.11.7 | 5f8be30b8 | **true** | NO — uncommitted state |
| ares-store | 0.11.7 | 5f8be30b8 | **true** | NO — contains uncommitted dev-dep patch |
| ares-mcp | 0.11.7 | 5f8be30b8 | **true** | NO — same worktree state |
| ares-tools | 0.11.7 | 564e0b6a1 | false | commit 564e0b6 |
| ares-llm | 0.11.7 | 564e0b6a1 | false | commit 564e0b6 |
| ares-rag | 0.11.7 | 564e0b6a1 | false | commit 564e0b6 |
| ares-agent | 0.11.7 | 564e0b6a1 | false | commit 564e0b6 |
| ares-http | 0.11.7 | 564e0b6a1 | false | commit 564e0b6 |
| ares-server | 0.11.7 | d947ee882 | false | commit d947ee8 |
| ares-test-support | — | — | — | deliberately NOT published (never public) |

## What is NOT reproducible, and why

Four tarballs (cordis, types, store, mcp) were published with `--allow-dirty`
from a worktree at 5f8be30 carrying uncommitted edits. Their `.cargo_vcs_info.json`
records `sha1=5f8be30b8, dirty=true`. **No commit holds those exact bytes.**

The `dirty` state for cordis/types was benign in content (verified: the tarball
differs from a clean 5f8be30 build ONLY in `.cargo_vcs_info.json`), but for
store/mcp the uncommitted state included the `ares-test-support` dev-dep patch
that later became commit `9624cda` — so their CONTENTS also differ from any
commit.

## Release branches (durable)

- `origin/release/ares-0.11.7` -> d947ee8 (vector bump, ares-store + member +
  root dev-dep patches). Contains the patches store/mcp shipped uncommitted.
- `origin/release/ares-0.11.8` -> cd4f95e (same patches for 0.11.8, on 6f7475b).

## Known corrections to earlier claims in this work

- "cordis/types are byte-identical to a clean rebuild" — TRUE for content, but
  the tarballs are NOT reproducible from a commit (dirty=true). Stated as
  "traceability intact"; that was wrong for these four crates.
- "ares-vector 0.1.5 matches the clean build" — the published 0.1.5 equals a
  build WITH the 0.1.5 bump (sha 1893b95d), which lives in commit 9624cda, not
  in the tag.
