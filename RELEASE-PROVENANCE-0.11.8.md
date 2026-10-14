# ARES 0.11.8 release provenance

Published to crates.io 2026-10-14 from release branch
`origin/release/ares-0.11.8`. Release marker: **d8eff95**
(`chore(ares): release 0.11.8`, empty commit on top of cd4f95e, which
carries the publish adjustments; 6f7475b is the feat commit in its
ancestry).

## Per-crate provenance (verified from each tarball's .cargo_vcs_info.json)

| crate | version | sha1 | note |
|---|---|---|---|
| ares-cordis | 0.11.8 | d8eff95aa | release marker |
| ares-vector | 0.1.6 | d8eff95aa | release marker |
| ares-types | 0.11.8 | d8eff95aa | release marker |
| ares-store | 0.11.8 | d2f26b9cc | member dev-dep patches |
| ares-mcp | 0.11.8 | d2f26b9cc | member dev-dep patches |
| ares-tools | 0.11.8 | (pending) | |
| ares-llm | 0.11.8 | (pending) | |
| ares-rag | 0.11.8 | (pending) | |
| ares-agent | 0.11.8 | (pending) | |
| ares-http | 0.11.8 | (pending) | |
| ares-server | 0.11.8 | (pending) | |
| ares-test-support | — | — | deliberately NOT published |

All landed tarballs stamp a clean, named commit (`dirty` absent). No
`dirty=true` debt in this round, unlike 0.11.7.

### Commit map on origin/release/ares-0.11.8

- `d8eff95` — `chore(ares): release 0.11.8` (empty marker on cd4f95e)
- `d2f26b9` — member `ares-test-support` dev-dep patches (the four member
  manifests; root was already patched in cd4f95e)

The root cause of the member blocker is `[workspace.dependencies]
ares-test-support = { version = "0.11.8" }` at line 42, which every member
inherits via `workspace = true`. Patching each member individually worked,
but for a future round the single edit is to make line 42 a range
`>=0.11.0, <0.12.0` instead of `^0.11.8`.

Unlike 0.11.7 (where four crates shipped dirty=true from uncommitted
state), 0.11.8 was published from a named release marker with a clean
tree, so every landed tarball is reproducible from commit d8eff95.

## Pre-existing tag conflict (needs owner awareness, NOT silently changed)

The repo already has an annotated tag `v0.11.8` -> **b8e74ec6b**
(bkataru, 2026-10-01, `feat(llm): stream the provider's own token counts
(0.11.8)`), which is NOT on origin/main and is a *different commit* from
this session's 0.11.8 line. I did not force-move it. The session's
release marker d8eff95 is a different, named commit on
`release/ares-0.11.8`.

Also: `v0.11.7` is annotated tag 4cd56093 -> 5f8be30 (bkataru,
2026-09-19). It does NOT reproduce the published bytes for cordis/types/
store/mcp, which shipped dirty. See RELEASE-PROVENANCE-0.11.7.md.

## Publish adjustments (same as 0.11.7, on cd4f95e)

- ares-vector 0.1.4 -> 0.1.5 (0.11.7) and 0.1.5 -> 0.1.6 (0.11.8);
  crates.io versions are immutable so each round needs a fresh
  ares-vector version.
- `ares-test-support = { workspace = true }` in [dev-dependencies] of
  ares-store, ares-agent, ares-http, ares-tools AND the root
  ares-server manifest -> replaced with the range `>=0.11.0, <0.12.0`
  that ares-test-support itself declares. ares-test-support has never
  been published (index 404) and publishing it would be a permanent
  first-ever public disclosure; the range makes it unnecessary.
- RUSTUP_TOOLCHAIN=1.98.0 must be set explicitly: the host's ambient
  1.95.0 overrides rust-toolchain.toml, and ares-server declares
  rust-version 1.98, which `cargo publish` verifies.

## FINAL per-crate stamp map (verified from all 11 tarballs)

| crate | version | sha1 | dirty |
|---|---|---|---|
| ares-cordis | 0.11.8 | d8eff95aa | absent (clean) |
| ares-vector | 0.1.6 | d8eff95aa | absent (clean) |
| ares-types | 0.11.8 | d8eff95aa | absent (clean) |
| ares-store | 0.11.8 | d2f26b9cc | absent (clean) |
| ares-mcp | 0.11.8 | d2f26b9cc | absent (clean) |
| ares-tools | 0.11.8 | d2f26b9cc | absent (clean) |
| ares-llm | 0.11.8 | d2f26b9cc | absent (clean) |
| ares-rag | 0.11.8 | d2f26b9cc | absent (clean) |
| ares-agent | 0.11.8 | d2f26b9cc | absent (clean) |
| ares-http | 0.11.8 | d2f26b9cc | absent (clean) |
| ares-server | 0.11.8 | d2f26b9cc | absent (clean) |

Both stamps are named, durable commits on `origin/release/ares-0.11.8`.
No `dirty=true` debt in this round — `--allow-dirty` was on every
command line but the worktree was clean, which the stamps confirm.

ares-test-support remains deliberately unpublished (index 404).

## Release summary (both rounds, 2026-10-14)

**0.11.7** — 11 crates + ares-vector 0.1.5. Tag `v0.11.7` is a
pre-existing annotated tag (4cd56093, bkataru, 2026-09-19) pointing at
5f8be30. Four tarballs (cordis, types, store, mcp) shipped
`dirty=true`, so no commit reproduces those bytes; documented in
`RELEASE-PROVENANCE-0.11.7.md` on `release/ares-0.11.7`.

**0.11.8** — 11 crates + ares-vector 0.1.6. All tarballs clean, stamped
either d8eff95 (release marker) or d2f26b9 (member dev-dep patches).
Documented above and committed as `RELEASE-PROVENANCE-0.11.8.md` on
`release/ares-0.11.8` (abad5a3).

`ares-test-support` remains deliberately unpublished in both rounds: it
has never been public (sparse index 404), and the `ares-test-support`
dev-dependency was repointed at the range `>=0.11.0, <0.12.0` precisely
so it would not need to be disclosed.

## Root cause worth fixing before the next round

`[workspace.dependencies] ares-test-support = { version = "0.11.x" }`
(Cargo.toml line 42) is inherited by every member via
`workspace = true`, and `cargo publish` resolves it even for
[dev-dependencies]. Each round required patching the root manifest plus
five consumers individually. A single edit at line 42 to
`>=0.11.0, <0.12.0` removes the whole class of blocker.

## Release summary (both rounds, 2026-10-14)

**0.11.7** — 11 crates + ares-vector 0.1.5. Tag `v0.11.7` is a
pre-existing annotated tag (4cd56093, bkataru, 2026-09-19) pointing at
5f8be30. Four tarballs (cordis, types, store, mcp) shipped
`dirty=true`, so no commit reproduces those bytes; documented in
`RELEASE-PROVENANCE-0.11.7.md` on `release/ares-0.11.7`.

**0.11.8** — 11 crates + ares-vector 0.1.6. All tarballs clean, stamped
either d8eff95 (release marker) or d2f26b9 (member dev-dep patches).

`ares-test-support` remains deliberately unpublished in both rounds: it
has never been public (sparse index 404), and the `ares-test-support`
dev-dependency was repointed at the range `>=0.11.0, <0.12.0` so it did
not need disclosing.

### Verified after publication

- The published graph resolves for a fresh consumer: a probe with only
  `ares-server = "0.11.8"` pulls all 11 crates from the index.
- `ares-store` and `ares-http` require `^0.1.6` of `ares-vector`
  (caret, not exact), so future minor bumps won't break consumers.

## Root cause worth fixing before the next round

`[workspace.dependencies] ares-test-support = { version = "0.11.x" }`
(Cargo.toml line 42) is inherited by every member via
`workspace = true`, and `cargo publish` resolves it even for
[dev-dependencies]. Each round required patching the root manifest plus
five consumers individually. A single edit at line 42 to
`>=0.11.0, <0.12.0` removes the whole class of blocker.

## Consumer-side follow-up

A downstream consumer converted its 10 `/opt` ares path references to
`version = "0.11.8"`; its own manifests now carry zero such references.
Note the consumer's repo is private, so it is not named here.

It still cannot resolve on a fresh runner: the `dirmacs/core` crate
(private, unpublished) deep-links ares-agent by path, so `cargo metadata`
exits 101 until that repo gets the same conversion.
