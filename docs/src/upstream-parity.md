# Upstream Parity Ledger

## What this document is

This document records every deliberate divergence between our kernel and the upstream model.
The paper states the reference model.
Our kernel sometimes chooses a different behavior on purpose.
This file is the audit ledger for those decisions.

- Date: 2026-08-24
- Scope: `crates/cordis` versus the model in the paper
- Method: every divergence lists the claim, the rationale, and the enforcement or test point

## Divergence table

| # | Divergence | Decision |
|---|---|---|
| 1 | Failed registrations stay visible | `Failed{error}` is a terminal rest state with reflective wiring |
| 2 | Peer-dependency compatibility | Majors-only version buckets, no structural checks |
| 3 | Late inject declarations | Eager reconciliation on `Active` fibers |
| 4 | Hot swap mechanics | Out-of-band trial then promote, honest `swap_mode` reporting |
| 5 | Dynamic library loading | Strictly opt-in behind `hmr`, exact fingerprint handshake |
| 6 | Factory collection | Inventory primary, manual chains as fallback |
| 7 | Serial dispatch | Direct alias of `Bail`, waterfall uses real `next` continuations |

Each row expands below with the claim, the rationale, and the evidence.

### 1. Failed registrations stay visible

- Upstream expectation: a factory error leaves the fiber permanently `Inactive` and unreachable.
- Claim: `Failed{error}` is a terminal VISIBLE rest state.
- Detail: `RegistryService::register` returns `Err`.
- Detail: the fiber enters the bookkeeping graph through `RegistryService::wire_failed_registration`.
- Detail: `ReflectService` wiring registers the fiber against the attempted provider key.
- Detail: notify fans out, so dependents observe the provider loss reactively and rest `Inactive`.
- Detail: a later successful registration allocates a fresh fiber id and supersedes the failed fiber.
- Rationale: operators inspect failures directly.
- Rationale: dependents get a real notification instead of silence.
- Rationale: fresh-id supersession keeps the provided slot free for retry.
- Source: `crates/cordis/src/registry.rs`, `wire_failed_registration`
- Test: metatheory property 3 legs D-H, `metatheory_dependent_never_active_without_provider`

### 2. Peer dependencies use majors-only buckets

- Upstream expectation: compatibility needs full structural interface checks.
- Claim: compatibility compares majors only.
- Detail: versions are plain `u64` values.
- Detail: `major(v) = v / VERSION_MAJOR_SCALE` and `floor(v) = v % VERSION_MAJOR_SCALE`.
- Detail: `VERSION_MAJOR_SCALE` equals `100_000`.
- Detail: a requirement binds when the major matches AND the provider reaches the floor.
- Detail: any mismatch leaves the dependent fiber `Inactive`.
- Detail: structural interface compatibility is deliberately NOT attempted.
- Rationale: majors-only buckets cover practical drift between builds.
- Rationale: full structural compatibility remains the open problem the paper defers.
- Source: `Context::provide_versioned` and `VERSION_MAJOR_SCALE` in `crates/cordis/src/context.rs`
- Source: `Fiber::declare_inject_versioned` in `crates/cordis/src/fiber.rs`
- Tests: the `version_conformance` module in `crates/cordis/src/metatheory.rs`

### 3. Late inject declarations reconcile eagerly

- Upstream expectation: a late declaration waits for an external refresh trigger.
- Claim: a declaration landing on a fiber resting `Active` reconciles eagerly.
- Detail: `Fiber::reconcile_after_declare` runs with the same transition shape as `refresh`.
- Detail: satisfied declarations update the epoch in place.
- Detail: unsatisfied declarations undo effects and rest the fiber `Inactive`.
- Detail: a declaration that races an in-flight refresh folds into that refresh through a pending flag.
- Detail: declarations on `Inactive` and `Failed` fibers wait for the next transition.
- Rationale: eager recompute loses no racing declaration.
- Rationale: the quiescence invariant survives every declaration path.
- Source: `Fiber::declare_inject` and `Fiber::reconcile_after_declare` in `crates/cordis/src/fiber.rs`
- Source: register paths in `crates/cordis/src/registry.rs`
- Test: reactive invariant leg of `dependent_never_active_without_provider`

### 4. Hot swap drains then shifts

- Upstream expectation: swap mutates providers in place.
- Claim: swap builds out-of-band, then promotes.
- Detail: new instances build inside a scratch context.
- Detail: `SwapPromotion` bridges the new values through intercept bindings.
- Detail: the old fiber disposes while the bridge keeps serving consumers.
- Detail: promotion moves bridge values into the store before intercept removal.
- Detail: consumers never observe an absence window.
- Detail: an unverifiable swap reports `swap_mode = "unverified"` instead of a fake success state.
- Rationale: drain-and-shift proves a zero absence window under concurrency.
- Rationale: honest reporting lets operators tell verified swaps from unverified ones.
- Source: `Loader::replace_provider` and `SwapPromotion` in `crates/cordis/src/loader.rs`
- Tests: `replace_provider_zero_absence_window`
- Tests: `rebuild_same_type_verified_swap` probes resolution from a concurrent task during the swap

### 5. Dylib loading is strictly opt-in with a fingerprint handshake

- Upstream expectation: dynamic library loading runs as a first-class default.
- Claim: dylib loading requires the `hmr` cargo feature.
- Detail: the default production path is file-watch plus `Fiber::reload` through `watcher::watch_many`.
- Detail: as of this change, every dylib load performs an exact ABI fingerprint handshake.
- Detail: the plugin must export `cordis_plugin_fingerprint`.
- Detail: the returned string must equal the host fingerprint exactly.
- Detail: a missing symbol refuses the load.
- Detail: a mismatched string refuses the load and names both fingerprints.
- Rationale: stale libraries fail fast at load time.
- Rationale: unchecked dylibs corrupt the process across the FFI boundary.
- Source: `load_plugin_so` and `FINGERPRINT_SYMBOL` in `crates/cordis/src/hmr.rs`
- Tests: `load_plugin_so_rejects_missing_fingerprint`
- Tests: `load_plugin_so_rejects_mismatched_fingerprint`

### 6. Inventory collection is the primary registration path

- Upstream expectation: registration walks an explicit hand-written factory list.
- Claim: inventory collection gathers factories automatically as the primary path.
- Detail: hand-written `register_plugins` chains remain as the fallback without the `inventory` feature.
- Detail: the linker drops inventory nodes from crates that nothing references.
- Detail: parity tests force-link every contributing crate before collection.
- Rationale: collection deletes a hand-maintained list and its drift bugs.
- Rationale: the linker failure is silent, so it needs a written warning.
- Source: `register_inventory_factories` in `crates/cordis/src/lib.rs`
- Test: `inventory_registry_matches_expected_factory_set` in `tests/inventory_parity.rs`

### 7. Serial dispatch aliases Bail, waterfall composes handlers

- Upstream expectation: serial dispatch differs from bail semantics.
- Claim: `Dispatch::Serial` is a direct alias of `Dispatch::Bail`.
- Detail: both variants run the same `run_bail_handlers` code path.
- Detail: waterfall is around-middleware.
- Detail: every waterfall handler receives a real `next` continuation.
- Detail: the terminal `next` runs the core operation.
- Detail: no sentinel value ever stops a chain.
- Rationale: one shared bail mode removes a near-duplicate implementation.
- Rationale: around-middleware composition matches the paper shape directly.
- Source: `EventsService::dispatch` in `crates/cordis/src/events.rs`
- Tests: `serial_stops_at_first_non_null_result`
- Tests: `waterfall_around_short_circuit_skips_core`

## Properties we prove beyond the paper

`crates/cordis/src/metatheory.rs` proves five properties as executable checks.
These hold regardless of the divergence choices above.

1. **Quiescence after every operation** (`quiescence_after_every_op`). Every fiber rests in a well-defined state between operations. Transitional states appear only mid-await, never at rest.
2. **Registration confluence** (`order_confluence_of_registrations`). Registration order does not change the final graph.
3. **Reactive spatial invariant** (`dependent_never_active_without_provider`). A dependent never activates while its provider is absent. It activates reactively when the provider appears.
4. **LIFO dispose restores the store** (`lifo_dispose_restores_store`). Disposal unwinds effects in strict LIFO order. The store returns to its pre-registration contents.
5. **Version-conformance flips** (`version_conformance` module). A compatible upgrade flips the dependent back to `Active`. A mismatch holds it at `Inactive`.

## Maintenance note

Any pull request that changes kernel semantics MUST add a row here or update an existing row.
State the claim, the rationale, and the enforcement point in the entry.
Reviewers reject semantic kernel changes without a ledger entry.
Before merge, re-run the cited tests.
