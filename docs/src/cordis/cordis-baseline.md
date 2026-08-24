# Cordis redesign, baseline gates (Phase -1, steps 1,2)

> **STATUS (2026-08-24):** Historical planning document from the pre-round-4 Cordis migration.
> Rounds 4–9 have since shipped everything relevant; the branch references below are deleted or merged.
> Current state of record: `docs/cordis-mapping.md` (§10–§19) and `ARCHITECTURE.md`.

**Branch:** `cordis-redesign` forked from `main` at `e4f3bcca2397f25b237246faef0d10bbceb234de`
**Date:** 2026-08-20
**Toolchain:** rustc 1.95.0, cargo 1.95.0, clippy 0.1.95

## Step 1, rust-doctor baseline

**Command:** `npx rust-doctor@latest . --json` from `/opt/ares` (no `rust-doctor.toml`, defaults)

| Metric | Value |
|--------|-------|
| `audit.score.value` | 86 |
| `audit.score.label` | Great |
| `audit.score.model` | core-v2 |
| `audit.score.authoritative` | false |
| `worst_tier` | P2 |
| `applied_ceiling` | null |
| `projected_after_top_three` | null |
| `projected_rule_ids` | [] |
| `gate.blocking` | error |
| `gate.status` | passed |
| `gate.blocking_diagnostics` | 0 |
| `status` | complete |
| `complete` | true |

### Per-Dimension Sub-Scores

| Dimension | Score |
|-----------|-------|
| security (×2) | 100 |
| reliability (×1.5) | 75 |
| maintainability | 72 |
| performance | 99 |
| dependencies | 75 |

### Category breakdown (audit.categories)

| Category | warnings | distinct |
|----------|----------|----------|
| Bugs | 225 | 225 |
| Performance | 1 | 1 |
| Dependencies | 51 | 51 |
| Maintainability | 1683 | 717 |
| Other | 543 | 543 |
| **Total diagnostics** | 2503 |, |

### Tier distribution (mapped via policy rules)

| Tier | Count |
|------|-------|
| P0 | 0 |
| P1 | 0 |
| P2 | 53 |
| P3 | 941 |
| unknown (missing_docs, dead_code, etc.) | 543 |

No P0/P1 means ceiling not applied, `applied_ceiling=null`. This is the ceiling to raise.

**Top P2 diagnostics** are all `rust_doctor::cargo::duplicate_major_versions`: duplicate crates (http-body, http, hyper, toml, thiserror, and more), not logic bugs. No `disabled_tls_verification`, no `hardcoded_credential`, no `unpinned_git_dependency`, no `arc_with_non_send_sync`.

**Source files:** 143

---

## Step 2, Build/Test baseline (on `main` @ e4f3bcc)

### `cargo check --no-default-features --features openai,postgres,mcp`

**Result: PASSED**

- Finished `dev` profile in 30.02s
- 528 warnings (missing_docs for `src/cli/rag.rs`, `src/middleware/*`, `src/skill_engine.rs` etc.), not errors
- `cargo fix --lib -p ares-server` suggests 2 auto-fixes

Note: `ares-vector` excluded per `CLAUDE.md` build gate (`cargo build --release --no-default-features --features openai,postgres,mcp`). The workspace builds with this feature set.

### `cargo clippy, -D warnings`

**Result: FAILED, 4 errors**

```
error: function `cosine_similarity_scalar` is never used --> crates/ares-vector/src/distance.rs:344:4
error: function `l2_distance_scalar` is never used --> crates/ares-vector/src/distance.rs:353:4
error: function `dot_product_scalar` is never used --> crates/ares-vector/src/distance.rs:369:4
  = note: `-D dead-code` implied by `-D warnings`

error: method `from_str` can be confused for the standard trait method `std::str::FromStr::from_str`
  --> crates/ares-types/src/models/tenant.rs:13:5
  = help: consider implementing the trait `std::str::FromStr`
  = note: `-D clippy::should-implement-trait` implied by `-D warnings`
```

So `cargo clippy, -D warnings` is red on main, verification matrix will require fixing `dead_code` (add `#[allow]` or remove) and `should_implement_trait`.

### `cargo test` (default features, `ares-server` lib + crates)

**Result: 521 passed, 21 failed, 0 ignored**

Failures grouped:

1. **DB auth failures (10 tests)**, `Failed to connect to ares_test. Ensure it exists and migrations are applied.: Database("Failed to connect to Postgres: error returned from database: password authentication failed for user \"dirmacs\"")`
 - `middleware::api_key_auth::tests::*` (8 tests: daily_quota, daily_usage_db_error, invalid_api_key_rejected, invalid_auth_header_bytes, monthly_quota_exceeded, monthly_usage_db_error, valid_api_key_passes, verify_api_key_db_error)
 - `workflows::engine::tests::*` (6 tests: available_workflows, execute_workflow_orchestrator_single_step, router_invalid_route_uses_fallback, router_routes_to_product, unknown_name, get_workflow_config, workflow_engine_creation), all panic at `src/workflows/engine.rs:327:14` with same DB auth error
 - Indicates test env lacks `DATABASE_URL` or `ares_test` DB role, not code regression

2. **Env lock poisoned (2 tests)**:
 - `api::handlers::document_upload::tests::verify_webhook_secret_empty_env_allows_all`, `env lock poisoned: PoisonError`
 - `api::handlers::document_upload::tests::verify_webhook_secret_rejects_mismatch`, same
 - Caused by first `verify_webhook_secret_accepts_match` failure poisoning the shared env lock, cascading.

3. **Webhook secret logic (1 test)**:
 - `api::handlers::document_upload::tests::verify_webhook_secret_accepts_match`, `assertion failed: verify_webhook_secret(&headers).is_ok()`

4. **CLI init template (3 tests)**:
 - `cli::init::tests::test_generate_ares_toml_both`, `assertion failed: content.contains("[providers.ollama-local]")`
 - `test_generate_ares_toml_ollama`, same
 - `test_generate_ares_toml_openai`, `assertion failed: content.contains("[providers.openai]")`
 - These expect Ollama/OpenAI templates that were removed in the NVIDIA-only migration (37f6c6e), tests stale.

5. **Coverage:** Many subsystems green: scheduler, skill_engine, pipeline_engine, trigger_engine, observability, middleware usage, rag, tools all `ok`.

### `cargo test --doc`

**Result: 0 passed, 0 failed, 10 ignored**

- 528 warnings (same missing_docs)
- All 10 doc-tests are `ignored` (annotated with `ignore` in lib.rs line 24,46,59 etc.)
- No doc-test failures, but also no doc-test coverage.

### Summary

| Gate | Result |
|------|--------|
| cargo check (openai,postgres,mcp) | ✅ passes |
| cargo check --no-default-features | not yet run, required in Phase 7 (proves cfg cleanup) |
| cargo clippy -D warnings | ❌ 4 errors |
| cargo test | ⚠️ 521/542 (21 DB/env/template failures) |
| cargo test --doc | ✅ 0/0/10 ignored |
| cargo miri test | not run, Phase 7 only, leaf crates |
| rust-doctor | ✅ 86/Great/P2/passed, 0 blocking |

**Action for redesign:** Fix clippy dead_code + should_implement_trait before Phase 7 gate; fix or remove stale cli::init provider template tests; fix webhook secret env isolation; ensure test DB available for CI.
