# Interception

The kernel routes its own sensitive operations through the event bus. Five meta-events are veto points. A sixth observes dispatches. The constants live in `cordis::events`:

| Meta-event | Guards |
| --- | --- |
| `internal/get` | Strict service reads (`Context::get`) |
| `internal/set` | Service writes (`provide` paths) |
| `internal/config` | Config resolution before a plugin apply |
| `internal/update` | Restart scheduling on config change |
| `internal/listener` | Listener registration |
| `internal/dispatch` | Observation only; no veto power |

These meta-events do not join the product event catalog, so catalog validation skips them.

## Zero-cost gates

Every interception point calls `EventsService::listener_count` first. The count is one pair of map lookups over the flat and waterfall registries. With zero listeners, the operation proceeds on its historical path with no chain work. This gate makes the whole feature free until someone subscribes.

## Veto semantics

All veto chains run as `bail` chains: handlers run in registration order, and the first non-null result terminates the chain.

### `internal/get`

Payload: `{ "service", "ctx" }`. Rules for `intercept_get`:

- No listeners: `Ok(None)` at map-lookup cost.
- A chain terminal of null passes the read through untouched.
- A non-null result replaces what the consumer sees.
- A chain error vetoes the read.

The synchronous bridge in `Context::get` maps verdicts further:

- Non-null JSON with `"refuse": true` refuses the read outright (`ReadVerdict::Refuse`, lookup returns `None`).
- Any other non-null value redirects the read to the parent frame (`ReadVerdict::RedirectFrame`). This frame's store and intercept bindings are skipped, so a parent binding serves the read.
- Null passes normally.

Test anchors: `get_interceptor_rewrites_read`, `accessor_bypasses_intercept_waterfalls`.

### `internal/set`

Payload: `{ "service", "ctx" }`. A chain error vetoes the write. The previous value stays fully intact; no store, owner, or version mutation happens. Null and pass-through allow the write unchanged.

Test anchor: `set_interceptor_vetoes_write_leaves_old_value`.

### `internal/config`

Input: the raw config value. The chain's non-null terminal **is** the effective configuration. A null terminal passes the raw config through. A chain error fails the activation or update that was resolving config; the fiber rests `Failed`.

The registry captures raw config at registration time. Each refresh pass re-resolves the effective config from that single source through `Fiber::resolve_effective_config`, stages it once, and the runner consumes it via `effective_config_override`. With no interception, the path is byte-identical to the legacy runner.

Test anchors: `config_interceptor_rewrites_effective_config`, `config_waterfall_covers_activation_path`, `interceptor_error_fails_fiber_activation`.

### `internal/update`

Payload: `{ "service" }`. Three outcomes inside `Fiber::update`:

- Proceed: the chain passes (null counts as proceed here), so the restart runs.
- Veto: a bail with explicit JSON `false` parks the proposed config in `vetoed_config` and returns `Ok(())`. No restart runs; the fiber keeps serving its current application. Operators can inspect what was deferred through `Fiber::vetoed_config`.
- Error: a chain error propagates out of `Fiber::update` as an `Err`. The fiber stays `Active` on its old configuration. Nothing was applied and nothing was deferred.

Test anchors: `update_interceptor_veto_skips_restart_keeps_config`, `update_veto_defers_config_and_returns_ok`, `update_error_stays_active_old_config`.

### `internal/listener`

Payload: `{ "event" }`. Registrations are synchronous APIs, so the veto chain runs to completion on the current thread through `block_in_place`. Rules:

- `Ok(true)` or a null terminal lets the registration proceed.
- A bail (non-null, non-true) or a chain error cancels it. Fail-closed.
- A cancelled registration returns an inert handle. Disposing it flips nothing.
- On runtimes that cannot park a worker (single-thread flavors), the registration falls open and a warning records the skipped veto.

Test anchor: `listener_interceptor_bail_cancels_registration_inert_handle`.

### `internal/dispatch`

Observer, not veto. Before every non-internal dispatch, listeners receive `{ mode, name, args }` fire-and-forget. Handler results and errors drop by design: observability must never break or delay the observed operation. Internal meta-events are exempt, so observation cannot recurse into itself.

Test anchor: `internal_dispatch_observes_non_internal_only`.

## Layered intercept chains

Intercept overrides stack per `TypeId`. Every set appends a layer. Layers order outermost to innermost across ancestor frames; the innermost layer is the effective value every getter returns. `Context::intercept_chain` returns the full chain. An isolate label is a realm boundary: layers beyond it do not leak in.

Structural comparison uses shared-instance identity (`Arc::ptr_eq` per layer) via `Context::chains_structurally_equal`. Freshly built values compare unequal by design.

Test anchors: `chained_layers_append_innermost_effective`, `intercept_chain_returns_all_layers_in_order`, `inject_appends_layer`.

## Accessors bypass the waterfalls

Name-keyed accessors live beside the TypeId store. Register them with `Context::register_accessor`; alias names share one slot and dispose together. Reads use `read_property` (typed variant: `read_property_typed`); writes use `write_property`. Accessor reads and writes never consult or re-enter the `internal/get` or `internal/set` waterfalls. An interceptor that refuses all strict reads cannot block accessor traffic.

Test anchor: `accessor_bypasses_intercept_waterfalls`.

## Target-carrying filtered dispatches

The `*_from` variants carry a target filter alongside the payload:

- `bail_from(event, payload, Option<ListenerFilter>)`
- `waterfall_from(event, payload, Option<ListenerFilter>)`
- `waterfall_async_from(event, payload, filter, core)`

Non-global listeners whose registration options fail the filter skip this dispatch but stay registered. Global listeners bypass the filter and always participate. Kernel meta-events ride `bail_from` for their veto chains. A filter-empty snapshot still leaves live registrations intact; only cancelled slots prune.

Test anchors: `target_carrying_dispatches_filter_per_dispatch`, `filter_excludes_nonmatching_contexts`, `global_bypasses_filter`.

## Re-entrancy protection

A thread-local fence (`InterceptFence`) marks the thread while a synchronous bridge drives its chain. Nested operations on the same thread pass through unintercepted, so an `internal/get` listener that reads services does not recurse into its own veto. Bridges also require a multi-thread runtime; without one they fall open and log a warning.
