# Security policy

## Reporting a vulnerability

If you believe you've found a security issue in ares, please email
**security@dirmacs.com** with:

- A clear description of the issue
- The impacted version(s)
- Steps to reproduce, ideally a minimal failing example
- Your disclosure preference (public PR / private patch)

We aim to acknowledge reports within two business days and ship a
fix or workaround within fourteen days for confirmed high-severity
issues. Please give us a reasonable window before public disclosure.

## Supported versions

Only the latest `main` branch and the most recent published crate
version on crates.io receive security fixes. Older versions are
not supported.

## Dependency advisories

ares runs `cargo audit` in CI and tracks GitHub Dependabot alerts
on every dependency. When a dependency advisory is unreachable in
our code paths, we dismiss the alert with a documented rationale
rather than forcing a noisy bump, but we record the reasoning in
this file so downstream consumers can audit our decisions.

### Advisories reachable in ares (action required)

The following advisories fire on crates in our dependency graph and
the vulnerable code path is reachable or cannot be cheaply proven
unreachable. They are tracked as open work:

- **`rsa 0.9.10`**, RUSTSEC-2023-0071 (Marvin timing attack).
Pulled transitively via the RSA-PSS verification path inside
 `jsonwebtoken`, and via cloud-storage signing in `sqlx-mysql` and
 `lancedb`. ares signs its own JWTs with HS256 (no RSA), never
instantiates `sqlx-mysql`, and only uses `lancedb` over the local
filesystem, so a timing oracle is not practically mountable
against ares. We will adopt the fix as soon as RustCrypto ships
a patched release. **Status:** upstream fix pending.

- **`rustls-webpki 0.102.8`**, RUSTSEC-2026-0049 (CRL bypass).
Reachable via `libsql → tonic 0.11 → rustls 0.22 → rustls-webpki
  0.102`. The fix requires `libsql` to upgrade to `tonic 0.12`
 (which carries `rustls 0.23` / `rustls-webpki 0.103`). We have
filed an upstream request on `tursodatabase/libsql`. **Status:**
waiting on upstream libsql.

### Advisories dismissed as unreachable

The following Dependabot alerts have been dismissed because the
vulnerable code is verifiably not compiled or not called in ares.
Each dismissal is backed by a reachability check described below.

#### `jsonwebtoken 9.3.1`, CVE-2026-25537 (`exp` / `nbf` bypass)

**Decision:** dismissed as `tolerable_risk`.

**Evidence:**

```
$ cargo tree -i jsonwebtoken@9.3.1
error: package ID specification `jsonwebtoken@9.3.1` did not match any packages
help: there are similar package ID specifications:
  jsonwebtoken@10.3.0
```

`jsonwebtoken 9.3.1` is a lockfile orphan, it is present in
`Cargo.lock` from an earlier resolution but is **not in the active
compile graph**. Every path that imports `jsonwebtoken` today
resolves to the patched `10.3.0`:

```
$ cargo tree -i jsonwebtoken@10.3.0
jsonwebtoken v10.3.0
└── ares-server v0.7.5
```

A targeted `cargo update -p jsonwebtoken@9.3.1 --precise 10.3.0`
is rejected by cargo because a separate entry in the lockfile
still records a `^9.2` constraint from an earlier `reqsign →
opendal → lance-io → lance` chain that no longer compiles. The
constraint metadata is stale; the code is not reachable.

**Defense in depth:** even if the old version were compiled, the
`exp` / `nbf` bypass requires `Option`-typed claim fields that can
be skipped during validation. ares's `Claims` struct declares
`exp: usize` (mandatory, typed), and has no `nbf` field at all.
The bypass path is closed regardless of which `jsonwebtoken`
version loads.

#### `lru 0.16.3`, RUSTSEC-2026-0002 (`IterMut` use-after-free)

**Decision:** dismissed as `tolerable_risk`.

**Evidence:**

```
$ cargo tree -i lru
lru v0.16.3
└── ares-server v0.7.5
```

`lru 0.16.3` is already the patched version, `0.16.3` carries
the fix for the `IterMut` UAF. The alert fires on the older
advisory ID even though the installed version is safe.

**Defense in depth:** ares uses `LruCache::new(capacity)` and
only the `get` / `put` methods on `LruCache`. There are no
`iter_mut()` or `IterMut` calls on any `LruCache` instance in
the code base. The `iter_mut()` calls elsewhere in ares operate
on `Vec`, not on `LruCache`. The advisory's code path cannot be
triggered via ares.

## How to reproduce the dismissal checks yourself

```bash
# Orphan check — should return "did not match any packages":
cargo tree -i jsonwebtoken@9.3.1

# Active version check — should show jsonwebtoken@10.3.0:
cargo tree -i jsonwebtoken@10.3.0

# lru version check — should show only 0.16.3:
cargo tree -i lru

# Claims struct shape — exp is mandatory usize, no nbf field:
rg -n "struct Claims" src/
rg -n "pub (exp|nbf):" src/

# LruCache usage — only new/get/put, no iter_mut:
rg -n "LruCache::|\.iter_mut\(\)" src/
```

Run these before reopening a dismissed alert. If any of the
invariants above changes, e.g. a new dep pulls `jsonwebtoken 9.x`
into the active graph, or ares adds an `nbf` claim, or starts
using `LruCache::iter_mut`, the dismissal becomes invalid and
the alert should be re-triaged.
