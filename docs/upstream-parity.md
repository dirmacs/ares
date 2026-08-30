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
| 8 | Worker supervision | Reserved exit codes plus stdin-EOF death detection |
| 9 | Log routing | One exporter router fans records to every gated sink |
| 10 | Dependency withdrawal | Genuine loss rests working fibers `Pending` (reversible); apply errors stay terminal `Failed` |
| 11 | Dispatch participation knobs | `EventOptions{prepend,global}` + `emit_filtered`; filters never exclude global listeners |
| 12 | Reads during transitions | Strict `get` refuses transitioning owners; `get_relaxed` is the explicit opt-in |

| 14 | Kernel operations are interceptable | Five meta-events veto or rewrite kernel ops; unregistered path stays zero-cost; sync bridges fall open |
| 15 | Readiness gates wait quietly | Closed `ready_when` barriers rest fibers `Pending` (never `Failed`); availability predicates remain the loud path |
| 16 | Config cascades batch | Concurrent provider updates collapse to one dependent convergence wave |
| 17 | Validation errors carry paths | Pre-flight failures surface `message` + `path` issues beside the legacy string |
| 18 | Logger adopted natively | Ring buffer, effect-owned exporters, per-name routing live in the kernel crate |
| 19 | Timers adopted natively | Six fiber-scoped primitives share one std-only wheel thread |
| 20 | Accessor traffic bypasses interception | Name-keyed computed properties resolve outside the `internal/get` / `internal/set` waterfalls entirely |
| 21 | Intercept layers are ordered and inspectable | Append-on-set keeps the innermost layer effective; `intercept_chain` returns outermost..innermost |
| 22 | Restart errors keep the old application | `Fiber::update` propagates errors with the fiber still `Active`; a veto parks `vetoed_config` and returns `Ok` |
| 23 | Config interception covers activation | The `internal/config` waterfall consults on first activation too, not only re-applies |
| 24 | Module changes fan out through one graph | `change_many` computes the affected set read-only first, reloads each plugin once per transaction, and rollback keeps siblings `Active` (EXCEEDS upstream: no module-graph concept) |
| 25 | Entries relocate without losing identity | PATCH move-then-update answers 409 on conflict; `/move` renames the `{id}:*` subtree; in-place refresh preserves the fiber (EXCEEDS upstream: flat key list only) |
| 26 | Subtask cancellation is wired end to end | Sticky cancel tokens honored at step boundaries plus an external trigger (EXCEEDS upstream: reference defines the hook but never wires it) |
| 27 | Deterministic micro calls are cached | LRU+TTL keyed `(model, system, input)` with `cache_hit` telemetry; salvage/retry results never cached (EXCEEDS upstream: roadmap prose only) |
| 28 | Guided grammars ride typed hints | Schema-shaped values become `json_schema` everywhere; raw GBNF stays a provider extension; absent hint = byte-identical wire |
| 29 | Duplicate embeddings cost one backend call | Content-hash dedup on local and HTTP paths fans vectors back to duplicate slots (EXCEEDS upstream: roadmap prose only) |


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

### 8. Worker supervision uses reserved exit codes plus stdin EOF

- Upstream expectation: the paper defines no process supervision model.
- Claim: supervised workers terminate through a fixed exit-code protocol.
- Detail: `EXIT_RESTART` (51) asks the daemon for a fresh worker.
- Detail: `EXIT_QUIT` (52) stops without a restart.
- Detail: `EXIT_BOOT` (53) surfaces boot failure non-zero to the manager.
- Detail: codes sit in the 51-53 band, clear of shell (1-2) and panic (101) codes.
- Detail: workers set `CORDIS_SUPERVISED` watch stdin; EOF means the daemon died.
- Rationale: pipe EOF is the only loss-free signal that survives daemon SIGKILL.
- Source: `crates/cordis/src/worker.rs`, `src/supervisor.rs`
- Tests: `exit_codes_are_distinct`
- Tests: `child_exit_codes_drive_loop`

### 9. Log routing fans out through one exporter router

- Upstream expectation: the paper defines no observability surface.
- Claim: one router fans call records out to every registered exporter.
- Detail: per-exporter level gates filter records before delivery.
- Detail: exporter failures stay contained; inference never fails on them.
- Detail: registration validates once; duplicate registrations are skipped.
- Rationale: a single fan-out point replaces ad hoc sink plumbing per consumer.
- Source: `ExporterRouter` in `crates/ares-llm/src/exporter.rs`
- Tests: `router_fans_out_to_all_exporters`
- Tests: `accepts_gate_filters_records`

### 10. Dependency withdrawal is reversible for working fibers

- Upstream expectation: a fiber whose provider disappears rests `Inactive` (or is disposed) and never comes back on its own.
- Claim: a previously-working runner fiber whose dependency genuinely vanished disposes its effects LIFO under `Unloading` and rests a new `Pending` state; when the provider returns it reactivates through `Loading`.
- Detail: `Pending` is reserved for reactive waiting only — an apply error still rests terminal `Failed{error}` (row 1), and a peer-version constraint refusal over an existing-but-incompatible provider still rests `Inactive` because the provider remains available.
- Detail: eligibility requires one fully-satisfied refresh pass first; registration cannot mark a fiber eligible.
- Detail: `Pending` fibers reserve their registry key and survive `prune_disposed`, so reactivation needs no re-registration.
- Rationale: the paper's permanently-Inactive outcome discards a healthy instance that only waits for its dependency; keeping it reversible preserves work.
- Source: `FiberState::Pending`, the reactive-loss branch of `Fiber::refresh` in `crates/cordis/src/fiber.rs`
- Tests: `dependent_reactivates_when_provider_returns`, `failed_stays_failed_on_dep_return`, `pending_fiber_survives_prune_disposed`

### 11. Dispatch participation knobs and filtered emits

- Upstream expectation: listener registration has fixed semantics with no ordering or participation control.
- Claim: flat listeners register through `on_with` / `once_with` with `EventOptions { prepend, global }`; `emit_filtered` runs a per-dispatch predicate over non-global listeners.
- Detail: `prepend: true` inserts at the front of the dispatch-order list; `global: true` marks the listener realm-agnostic, and filters never exclude it.
- Detail: a filter exclusion skips one dispatch without unregistering the listener.
- Detail: the historical `on` / `once` / `emit` signatures delegate unchanged, and the broadcast bus fan-out is not filtered.
- Rationale: per-realm policies need ordered, selectively-participating listeners without duplicating the bus.
- Source: `EventOptions`, `EventsService::on_with` / `once_with` / `emit_filtered` in `crates/cordis/src/events.rs`
- Tests: `prepend_ordering_observed`, `filter_excludes_nonmatching_contexts`, `global_bypasses_filter`

### 12. Reads during transitions are explicit and relaxed

- Upstream expectation: every read either resolves an Active value or fails; mid-transition values are unreachable by construction.
- Claim: strict `Context::get` keeps refusing providers resting in transitional states; `Context::get_relaxed` serves locally-owned values while their owner sits in `Loading` / `Reloading` / `Unloading` / reactive `Pending`.
- Detail: terminal rest states stay refused even relaxed — disposed owners (undos already ran) and `Failed{error}` owners return nothing.
- Rationale: lifecycle and observer code must inspect the value that is about to serve or was just retracted; making that a distinct method keeps the default read conservative.
- Source: `Context::get_relaxed` in `crates/cordis/src/context.rs`
- Test: `relaxed_read_succeeds_while_provider_transitioning`

### 13. Fiber state observers

- Upstream expectation: the model defines no notification surface for individual fiber state changes.
- Claim: `Fiber::subscribe_state` fans every lifecycle transition out to synchronous observers.
- Detail: observers run inline under the short state-lock critical section and MUST NOT call back into the fiber.
- Detail: observer panics are caught, so one broken observer cannot corrupt a transition; cancelled subscriptions are pruned on the next event.
- Rationale: tooling (admin surfaces, tests, supervision) needs transitions as they happen, not just polling after quiescence.

### 14. Kernel operations are interceptable through meta-events

- Upstream expectation: reads, writes, config resolution, restart schedules, and listener registration are fixed kernel behavior with no override points.
- Claim: five veto meta-events (`internal/get`, `internal/set`, `internal/config`, `internal/update`, `internal/listener`) wrap those operations and an `internal/dispatch` observer reports every non-internal dispatch with `(mode, name, args)`; the un-intercepted path is a zero-cost gate, and synchronous bridges FALL OPEN on runtimes that cannot park the worker.
- Detail: `internal/get` — a non-null terminal replaces the value a strict read returns, `{"refuse": true}` fails the lookup outright, null passes, a chain error refuses the read; a redirect verdict continues the lookup at the parent frame.
- Detail: `internal/set` — a chain error vetoes THIS write; the previous binding stays fully intact (no store/owners/version mutation).
- Detail: `internal/config` — the chain's non-null terminal IS the effective config staged for that apply pass; a chain error rests the fiber terminal `Failed{error}` (row 1 semantics unchanged).
- Detail: `internal/update` — a bail or explicit JSON false skips the restart; the fiber keeps serving its current application and the deferred config stays visible via `vetoed_config`.
- Detail: `internal/listener` — a bail or chain error cancels the registration and the caller receives an INERT handle; neither registry ever sees the listener (fail-closed).
- Detail: every consult checks `listener_count == 0` first (map-lookup cost); a thread-local fence keeps operations made inside a chain un-intercepted; single-thread tokio flavors log a warning and fall open, matching historical behavior.
- Detail: `bail_from` / `waterfall_from` / `waterfall_async_from` carry the operating context through an optional per-dispatch `ListenerFilter`; exclusions skip one dispatch without unregistering.
- Rationale: policy layers need to observe and veto kernel operations without duplicating them; the zero-cost gate keeps the default path byte-identical for every existing caller.
- Source: `INTERNAL_*_EVENT` constants, `intercept_get/set/config/update/listener`, `bail_from` / `waterfall_from` / `waterfall_async_from`, and the synchronous bridges in `crates/cordis/src/events.rs`
- Source: consult points in `Context::get` / the provider-write path (`crates/cordis/src/context.rs`); config staging and the update veto in `crates/cordis/src/fiber.rs`
- Tests: `get_interceptor_rewrites_read`, `set_interceptor_vetoes_write_leaves_old_value`, `config_interceptor_rewrites_effective_config`, `update_interceptor_veto_skips_restart_keeps_config`, `listener_interceptor_bail_cancels_registration_inert_handle`, `internal_dispatch_observes_non_internal_only`, `interceptor_error_fails_fiber_activation`, `target_carrying_dispatches_filter_per_dispatch`

### 15. Readiness gates wait quietly; availability predicates fail loudly

- Upstream expectation: the model defines no way to hold a produced service out of rotation while its environment warms up (and nothing distinguishes that from failure).
- Claim: `register_with_readiness` installs a composable `ReadinessBarrier` consulted before every activation pass; while it reports not-ready the fiber rests inspectable `Pending` — quiet waiting that NEVER becomes `Failed` — while availability predicates (`Service::check`) remain the loud complement resting `Failed{error: "availability predicate rejected service"}`.
- Detail: `ReadinessBarrier::new(pred)` wraps one `Fn(&Arc<Context>) -> bool`; `.and(other)` AND-composes; `with_readiness([a, b, c])` folds any number of barriers (an empty list is vacuously ready).
- Detail: `.watching([TypeId])` unions the provider keys whose settlements re-kick the gated fiber through the `ReflectService` fan-out — an external provide or withdrawal re-evaluates the gate without touching the fiber.
- Detail: the factory runs once at registration (config errors still surface immediately); a closed gate only keeps the produced service OUT of consumer reach because strict `get` refuses non-`Active` owners; opening the gate activates without re-running the factory.
- Rationale: not-ready-yet (warming caches, absent external system) differs fundamentally from broken; conflating them buries healthy waiting fibers under failure noise, and separating them lets operators read intent from state alone.
- Source: `ReadinessBarrier`, `with_readiness`, `register_with_readiness`, and the re-kick wiring in `crates/cordis/src/registry.rs`
- Source: the readiness consult in `Fiber::refresh` (`crates/cordis/src/fiber.rs`)
- Tests: `ready_when_holds_pending_until_true_then_activates`, `readiness_composes_and_semantics`, `external_rekick_reactivates_waiting_fiber`

### 16. Concurrent config updates collapse to one cascade wave

- Upstream expectation: every provider settle triggers its own full dependent refresh wave.
- Claim: an in-flight ledger marks providers mid-reapply; dependents defer during the window and converge EXACTLY ONCE per settled batch.
- Detail: `CASCADE_INFLIGHT` maps fiber id to open-window count (reentrant-safe); `Loader::drive_fiber_update` opens and closes windows around one live re-apply.
- Detail: the kernel refresh path consults `cascade_any_inflight`, so a storm of racing patches costs one dependent apply pass and ends Active with the final config.
- Rationale: N concurrent patches against one provider must not cost N dependent convergence waves.
- Source: `CASCADE_INFLIGHT`, `cascade_begin` / `cascade_end` / `cascade_any_inflight` in `crates/cordis/src/loader.rs`
- Test: `concurrent_config_updates_collapse_to_single_cascade`

### 17. Config pre-flight failures carry structured issues

- Upstream expectation: configuration errors are lossy prose strings.
- Claim: plugins reject configs with `ValidationIssue { message, path }` items aggregated in a `ValidationError`; `CordisError::validation` lifts the aggregate into the existing `invalid config:` class, and the loader trial stashes per-entry failures so the admin PATCH answers 4xx with a machine-readable `issues` array beside the legacy `error` string.
- Detail: stash slots mirror the LATEST trial outcome; recording a non-validation error clears the entry and consumption removes it, so a later successful patch carries no `issues`.
- Rationale: API consumers need to render field-level feedback, not parse sentences.
- Source: `ValidationIssue` / `ValidationError` / trial stash in `crates/cordis/src/error.rs`; `CordisError::validation` in `crates/cordis/src/service.rs`; issue attachment in `crates/ares-http/src/api/handlers/admin/cordis.rs`
- Test: `patch_endpoint_returns_structured_issues_on_bad_config`

### 18. The logger lives in the kernel crate

- Upstream expectation: logging ships as a satellite console package beside the kernel.
- Claim: the logger is adopted NATIVELY (`cordis::logger`) with upstream-style semantics: bounded ring, effect-owned exporter sinks, per-name level routing, printf rendering.
- Detail: `LoggerService` keeps the last 1000 `Message`s (monotonic sequence, timestamp, name, kind, numeric level, args, fiber label) and snapshots without copying payloads.
- Detail: exporters are effect-owned — `register` returns a `Disposable` whose disposal removes the sink; `ExporterConfig` gates per name and truncates rendered text (default cap 4096 chars, char-boundary safe).
- Detail: thresholds resolve per-name pin, then the `LoggerIntercept` override (read through the relaxed channel, so per-fiber overrides apply on child contexts), then the default level (`Debug`); `enabled` bails BEFORE argument assembly.
- Detail: rendering supports `%s %d %i %f %o %O %c %C %%`; unknown specifiers and exhausted arguments stay literal; `%c` picks a stable ANSI16 slot by FNV-1a hash of the logger name, `%C` adds bold; `hyphenate` / `derived_name` yield kebab-case logger names.
- Detail: the `Context` facade (`ctx.log/info/warn/debug/error/log_with`) is a no-op when no logger is provided.
- Rationale: observability belongs where fibers dispose, so sink lifetimes tie to effects instead of a satellite package boundary; the multi-package layering ceremony is deliberately not replicated.
- Source: `LoggerService`, `Exporter`, `ExporterConfig`, `LoggerIntercept`, `Message::render`, `hyphenate` in `crates/cordis/src/logger.rs`
- Tests: `buffer_bounded_at_capacity_snapshot_reads`, `level_routing_per_name_with_default_fallback`, `printf_placeholders_format_correctly`, `logger_intercept_overrides_level`, `hyphenate_and_derived_names`, `exporter_disposal_removes_sink`

### 19. Timer primitives are fiber-scoped and std-only

- Upstream expectation: timing ships as a dedicated satellite package with its own runtime assumptions.
- Claim: six primitives (`timeout`, `sleep`, `interval`, `interval_stream`, `debounce`, `throttle`) live natively in `cordis::timer`, run on ONE shared wheel thread, and attach to the owning fiber through labeled undos.
- Detail: the wheel is a min-heap on a dedicated `cordis-timer` thread; due entries drain under one short critical section and callbacks run outside the lock; panics are caught and the thread survives.
- Detail: registrations made under `with_current_fiber` push `timer:`-labeled undos, so `Fiber::dispose` (or a reactive unload) cancels them; dropping a handle does NOT cancel; out-of-scope registrations degrade to warned orphan handles that stay explicitly disposable.
- Detail: a disposed `Interval` stream yields exactly ONE final `Err(InactiveEffect)` then closes; queued live ticks are discarded so teardown is the final observation.
- Detail: `debounce` collapses a burst into one trailing delivery after the last call; `throttle` delivers leading-edge plus optional trailing in a fixed window.
- Rationale: timers must die with the fiber that owns them or they leak firings past teardown; a shared thread keeps thousands of registrations at one thread's cost with no async runtime dependency.
- Source: `timeout` / `sleep` / `interval` / `interval_stream` / `debounce` / `throttle`, `with_current_fiber`, `Scheduled`, `Interval` in `crates/cordis/src/timer.rs`
- Tests: `timeout_fires_once_and_disposes_with_fiber`, `timeout_dispose_before_deadline_prevents_fire`, `interval_ticks_repeatedly_and_stops_on_dispose`, `interval_stream_final_err_on_dispose`, `debounce_collapses_bursts`, `throttle_trailing_edge_respected`

### 20. Accessor traffic bypasses interception

- Upstream expectation: every value read or write consults the `internal/get` / `internal/set` veto waterfalls.
- Claim: name-keyed computed properties (`register_accessor`) resolve OUTSIDE both waterfalls — resolving an accessor never consults or re-enters a veto chain.
- Detail: `Accessor::{read_only, read_write, setter_only}` installs a getter/setter pair beside the TypeId service store; registration returns an `EffectHandle` whose disposal removes the declaration and every alias.
- Detail: `Context::alias` binds an alternate name through the SAME registration; duplicate declarations (including alias collisions) are rejected with `DuplicateProvider`.
- Detail: typed reads surface `CordisError::PropertyTypeMismatch` instead of a silent `None`; writes to a read-only property are refused with `CordisError::ReadOnlyProperty`.
- Rationale: computed properties are policy plumbing, not provider state — a veto lets an interceptor break accessor invariants it cannot see.
- Source: `Context::register_accessor`, `Accessor`, `Context::alias`, `EffectHandle` in `crates/cordis/src/context.rs`
- Tests: `accessor_read_write_roundtrip`, `duplicate_accessor_declaration_rejected`, `readonly_property_rejects_set`, `dispose_accessor_resolves_none`, `alias_resolves_same_value`, `accessor_bypasses_intercept_waterfalls`

### 21. Intercept layers are ordered, append-on-set, and inspectable

- Upstream expectation: one intercept binding per key; later registrations replace earlier ones (last-write-wins).
- Claim: intercept layers per TypeId form an ordered outermost..innermost sequence; NEW registrations APPEND, so the innermost layer stays effective for all existing getters.
- Detail: append-on-set means no existing caller observes a behavior change when another layer joins.
- Detail: `Context::intercept_chain(tid)` returns every layer outermost..innermost for inspection and restart-decision logic.
- Detail: `Context::chains_structurally_equal` compares two chains by shared-instance identity (`Arc::ptr_eq`) per layer pair; erased values carry no comparable contract, so freshly-built values compare unequal by design.
- Rationale: layered policies need composition without clobbering, and restart decisions need an honest equality test over opaque layers.
- Source: `Context::intercept_chain`, `Context::chains_structurally_equal`, layer storage in `crates/cordis/src/context.rs`
- Tests: `chained_layers_append_innermost_effective`, `intercept_chain_returns_all_layers_in_order`

### 22. Restart errors keep the old application; vetoes defer loudly

- Upstream expectation: an update-pass failure leaves the fiber in an unspecified mid-transition state.
- Claim: `Fiber::update` returns `Result<(), CordisError>` — a restart-path error propagates to the caller and the fiber stays `Active` serving its OLD configuration; an `internal/update` veto parks the deferred config in `Fiber::vetoed_config` and returns `Ok`.
- Detail: error propagation never disposes effects of the still-running application.
- Detail: the vetoed config remains inspectable through `vetoed_config()` so operators can see what was declined and why.
- Rationale: a failed restart must not destroy the working instance, and a silent skip must still be observable.
- Source: `Fiber::update`, `vetoed_config` in `crates/cordis/src/fiber.rs`
- Tests: `update_error_stays_active_old_config`, `update_veto_defers_config_and_returns_ok`

### 23. Config interception covers the activation path

- Upstream expectation: config rewriting applies only on later re-applies; first activation runs the raw config.
- Claim: the `internal/config` waterfall is consulted on the ACTIVATION path too, so rewrites apply on first activation identically to re-applies.
- Detail: the same non-null-terminal-becomes-effective-config semantics hold on both paths (row 14).
- Rationale: activation-time-only rewrites make a policy's effect depend on whether the fiber starts fresh.
- Source: config-waterfall consult in the register/activation path in `crates/cordis/src/registry.rs`
- Test: `config_waterfall_covers_activation_path`

### 24. Module changes fan out through one dependency graph

- Upstream expectation: no module-level change propagation exists; each watcher event maps to at most one reload target.
- Claim: `ModuleGraph` maps module keys to dependencies; `change_many` computes the TRANSITIVE affected plugin set read-only FIRST, then reloads each affected plugin EXACTLY ONCE per transaction; a failing reload rolls back that plugin while successfully reloaded siblings stay `Active`.
- Detail: `ModuleReload` implementations perform the reloads; `ChangeOutcome` classifies the transaction result.
- Detail: the file watcher's debounced batch fans through a registered `ModuleGraph` when one is provided on the context; WITHOUT registration the watcher path is unchanged (opt-in).
- Rationale: batched filesystem events must not reload shared dependents N times or leave siblings dead because one peer failed.
- Source: `ModuleGraph`, `ModuleEntry`, `ModuleReload`, `ChangeOutcome`, `change_many` in `crates/cordis/src/module_graph.rs`; fan-out wiring in `crates/cordis/src/watcher.rs`
- Tests: `dependency_change_reloads_dependents_transitively`, `batched_changes_reload_each_plugin_once`, `rollback_keeps_successful_siblings_active`, `watcher_module_graph_fan_out_reloads_dependents`, `module_graph_without_registration_is_ignored`

### 25. Entries relocate without losing fiber identity (we exceed upstream)

- Upstream expectation: entries form a flat id-keyed list; relocation means delete-plus-recreate with a fresh fiber.
- Claim: PATCH accepts optional `parent` / `position` (`EntryPosition`) applied move-THEN-update (invalid placements answer 409 before any mutation), `POST /admin/cordis/entries/{id}/move` relocates an entry with its whole `{id}:*` subtree in one rename cascade, and a valid move preserves fiber identity via in-place refresh.
- Detail: moving into a descendant is refused; disabled groups move suppressed-then-restored.
- Detail: invalid moves touch neither the entries file nor the live tree; unknown ids answer 404.
- Rationale: hierarchical entry organization must not cost consumer-visible dispose/recreate windows.
- Source: `EntryPosition`, `EntryTree::move_entry`, `subtree_ids`, `Loader::move_entry` in `crates/cordis/src/loader.rs`; `patch` / `move_cordis_entry` handlers in `crates/ares-http/src/api/handlers/admin/cordis.rs`
- Tests: `move_preserves_fiber_identity_and_lands_update_in_new_parent`, `descendant_move_refused`, `subtree_rename_cascades_descendants`, `disabled_group_move_suppresses_start_then_restores`, `patch_endpoint_moves_entry`, `patch_endpoint_invalid_move_conflicts_without_mutating`

### 26. Subtask cancellation is wired end to end (we exceed upstream)

- Upstream expectation: the reference client defines cancellation hooks but never wires them into its delegation loop.
- Claim: delegated subtasks register sticky cancel tokens keyed by run/skill id; `SkillEngine::cancel_subtask()` flips a token exactly once and is honored at step boundaries alongside the `EmergencyStop` hook; an aborted subtask integrates nothing into the parent.
- Detail: quote-aware delegation argument parsing: double-quoted segments are single tokens (backslash escapes inside quotes); `--parallel` latches split-per-token mode with `|` separators ignored, `--model` consumes exactly one token, `--tools` enables the inner tool loop; precedence is flags > profile > global.
- Rationale: long-running delegated work needs an external off switch that lands between model rounds, not only at process exit.
- Source: `SubtaskCancelToken`, `SkillEngine::cancel_subtask`, `registered_cancel_token`, step-boundary checks in `crates/ares-agent/src/skills/engine.rs`; `parse_flags` tokenizer in `crates/ares-agent/src/skills/mod.rs`
- Tests: `cancel_token_aborts_subtask_between_rounds`, `parse_flags_quote_aware_tokens`

### 27. Deterministic micro calls are cached; repaired answers are not (we exceed upstream)

- Upstream expectation: the reference roadmap describes response caching but ships none; every identical micro call re-hits the network.
- Claim: deterministic-class micro outcomes serve from a bounded LRU map keyed by a content hash over `(model, system template, input)`; answers reached through retries or salvage fallback are NEVER cached.
- Detail: default 256 entries, 15-minute TTL, master switch via `MicroCacheConfig`.
- Detail: hits skip the network entirely, report `latency_ms: 0`, and carry the `cache_hit` telemetry flag.
- Rationale: classify/tag-style enrichment calls dominate micro traffic and their answers are stable; a repeated or repaired request proves the call was NOT deterministic-class, so a cache of it pins a bad answer.
- Source: `MicroCacheConfig`, `MicroOutcome::cache_hit`, `cache_key`, `LruOutcomeCache` in `crates/ares-llm/src/micro.rs`
- Tests: `identical_inputs_serve_cached_outcome`, `retries_exhausted_falls_back_to_salvage` (salvage path stays uncached by construction)

### 28. Guided grammars ride typed hints with byte-identical absence

- Upstream expectation: constrained output requires per-provider request surgery with no portable hint channel.
- Claim: `GenerationHints::guided_grammar` carries a schema-shaped JSON value as `response_format` `json_schema` on every OpenAI-compatible path; raw GBNF/EBNF text rides the provider-specific `guided_grammar` extension field on NON-streaming OpenAI-compatible requests; providers without a channel silently ignore the hint; an ABSENT hint leaves the wire byte-identical.
- Detail: classification is structural — a JSON object with an object root is a schema; anything else is raw grammar text.
- Rationale: one opt-in hint field covers structured outputs where supported and vendor grammar extensions where they are not, without changing default requests.
- Source: `GenerationHints::guided_grammar` in `crates/ares-llm/src/client.rs`; classification and `GUIDED_GRAMMAR_EXTENSION` in `crates/ares-llm/src/genai_client.rs`
- Test: `grammar_hint_present_reaches_request_builder`

### 29. Duplicate embeddings cost exactly one backend call (we exceed upstream)

- Upstream expectation: the reference roadmap mentions embedding dedup but implements none; every input in a batch hits the backend.
- Claim: per-request content-hash dedup collapses duplicate inputs (whitespace-normalized SHA-256) BEFORE the backend call on BOTH local and HTTP embedding paths; computed vectors fan back to every duplicate slot.
- Detail: `DedupPlan` maps duplicates to the first occurrence's slot; callers receive full-length results.
- Rationale: identical texts in one batch are common (templates, retries) and each costs a paid embedding call.
- Source: `DedupPlan`, `content_hash_hex`, `normalize_for_dedup` in `crates/ares-rag/src/embeddings.rs`
- Tests: `dedup_plan_maps_duplicates_to_first_occurrence_slot`, `duplicate_texts_single_backend_call_vectors_fanned_back`, `content_hash_hex_ignores_whitespace_differences_only`

## Properties we prove beyond the paper

`crates/cordis/src/metatheory.rs` proves five properties as executable checks.
These hold regardless of the divergence choices above.

1. **Quiescence after every operation** (`quiescence_after_every_op`). Every fiber rests in a well-defined state between operations. Transitional states appear only mid-await, never at rest. Allowed rest states include the reversible `Pending` (row 10) and terminal `Failed{error}` (row 1); only `Active` fibers must hold all declared injects available.
2. **Registration confluence** (`order_confluence_of_registrations`). Registration order does not change the final graph.
3. **Reactive spatial invariant** (`dependent_never_active_without_provider`). A dependent never activates while its provider is absent. It activates reactively when the provider appears.
4. **LIFO dispose restores the store** (`lifo_dispose_restores_store`). Disposal unwinds effects in strict LIFO order. The store returns to its pre-registration contents.
5. **Version-conformance flips** (`version_conformance` module). A compatible upgrade flips the dependent back to `Active`. A mismatch holds it at `Inactive`.

## Maintenance note

Any pull request that changes kernel semantics MUST add a row here or update an existing row.
State the claim, the rationale, and the enforcement point in the entry.
Reviewers reject semantic kernel changes without a ledger entry.
Before merge, re-run the cited tests.
