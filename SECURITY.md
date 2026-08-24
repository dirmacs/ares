# Security policy

## Report a vulnerability

If you believe that you found a security issue in ares, email **security@dirmacs.com** with:
- A clear description of the issue
- The impacted version or versions
- Steps to reproduce, ideally a minimal failing example
- Your disclosure preference (public PR / private patch)

We aim to acknowledge reports within two business days. For confirmed high-severity issues, we ship a fix or workaround within fourteen days. Give us a reasonable window before public disclosure.

## Supported versions

Only the latest `main` branch and the most recent published crate version on crates.io receive security fixes. Older versions do not receive fixes.

## Dependency advisories

ares runs `cargo audit` in CI and tracks GitHub Dependabot alerts on every dependency. When an advisory is unreachable in our code paths, we dismiss the alert with a documented rationale instead of a noisy bump. We record the reasoning in this file so downstream consumers can audit our decisions.

### Advisories reachable in ares (action required)

The following advisories fire on crates in our dependency graph. The vulnerable code path is reachable, or we cannot prove it unreachable at low cost. We track them as open work:

- **`rsa 0.9.10`**, RUSTSEC-2023-0071 (Marvin timing attack). The graph pulls it transitively through the RSA-PSS verification path inside `jsonwebtoken`, and through cloud-storage signing in `sqlx-mysql`. ares signs its own JWTs with HS256 (no RSA) and never instantiates `sqlx-mysql`, so an attacker cannot mount a timing oracle against ares. We adopt the fix as soon as RustCrypto ships a patched release. **Status:** upstream fix pending.

- **`rustls-webpki 0.102.8`**, RUSTSEC-2026-0049 (CRL bypass). Reachable through `libsql → tonic 0.11 → rustls 0.22 → rustls-webpki 0.102`. The fix requires `libsql` to upgrade to `tonic 0.12` (which carries `rustls 0.23` / `rustls-webpki 0.103`). We filed an upstream request on `tursodatabase/libsql`. **Status:** waiting on upstream libsql.

### Advisories dismissed as unreachable

The following Dependabot alerts stay dismissed because the vulnerable code is verifiably not compiled or not called in ares. Each dismissal rests on the reachability check below.

#### `jsonwebtoken 9.3.1`, CVE-2026-25537 (`exp` / `nbf` bypass)

**Decision:** dismissed as `tolerable_risk`.

**Evidence:**

```
$ cargo tree -i jsonwebtoken@9.3.1
error: package ID specification `jsonwebtoken@9.3.1` did not match any packages
help: there are similar package ID specifications:
  jsonwebtoken@10.4.0
```

`jsonwebtoken 9.3.1` is a lockfile orphan. An earlier resolution left the entry in `Cargo.lock`, but the active compile graph does not contain it. Every import path resolves to the patched `10.4.0`:

```
$ cargo tree -i jsonwebtoken@10.4.0
jsonwebtoken v10.4.0
└── ares-server v0.9.1
```

A targeted `cargo update -p jsonwebtoken@9.3.1 --precise 10.4.0` fails because stale constraint metadata from an earlier chain remains in the lockfile. The constraints are stale. The code is not reachable.

**Defense in depth:** even if the old version were compiled, the `exp` / `nbf` bypass requires `Option`-typed claim fields that validation can skip. The `Claims` struct in ares declares `exp: usize` (mandatory and typed), and it has no `nbf` field. The bypass path stays closed regardless of the loaded `jsonwebtoken` version.

#### `lru 0.16.3`, RUSTSEC-2026-0002 (`IterMut` use-after-free)

**Decision:** dismissed as `tolerable_risk`.

**Evidence:**

```
$ cargo tree -i lru
lru v0.16.4
└── ares-server v0.9.1
```

The lockfile carries `lru 0.16.4`, which contains the fix for the `IterMut` use-after-free. The alert fires on the old advisory ID even though the installed version is safe.

**Defense in depth:** ares uses `LruCache::new(capacity)` and only the `get` / `push` / `pop` / `iter` methods on `LruCache`. No `iter_mut()` call targets an `LruCache` instance. The `iter_mut()` calls elsewhere in ares operate on `Vec` values and other containers. The advisory path cannot trigger through ares.

## Reproduce the dismissal checks yourself

```bash
# Orphan check — must return "did not match any packages":
cargo tree -i jsonwebtoken@9.3.1

# Active version check — must show jsonwebtoken@10.4.0:
cargo tree -i jsonwebtoken@10.4.0

# lru version check — must show only 0.16.4:
cargo tree -i lru

# Claims struct shape — exp is mandatory usize, no nbf field:
rg -n "struct Claims" crates/
rg -n "pub (exp|nbf):" crates/

# LruCache usage — only new/get/push/pop/iter, no iter_mut:
rg -n "LruCache::|\.iter_mut\(\)" crates/
```

Run these checks before you reopen a dismissed alert. If any invariant above changes — for example, a new dependency pulls `jsonwebtoken 9.x` into the active graph, or ares adds an `nbf` claim, or starts to use `LruCache::iter_mut` — the dismissal becomes invalid, and someone must re-triage the alert.
