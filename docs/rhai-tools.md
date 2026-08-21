# Rhai as Default for ARES Tools + Cordis

Rhai `1.25` `serde` is the default `tool_type` for `runtime_tools` and the safe HMR bridge for Cordis.

## Why Rhai

- `toon`/`toml` are data, not programs — no `if`/`for`/`fn`. Rhai gives `fn execute(args){ if args["amount"]>3*avg { "CRITICAL" } }` with `Scope` isolation.
- `boa_engine 0.20` (JS) + `python3 -c` subprocess are two runtimes, `fork` + `GIL` + `python3` on VPS. Rhai is `~500KB` pure Rust, `AST` compiles once at `RuntimeToolRegistry::materialise`, reused via `Engine::call_fn(&mut scope, &ast, "execute", (Dynamic,))` inside `spawn_blocking` + `timeout` + `max_operations`.
- Cordis `libloading` HMR (`hmr.rs` `dlopen` `unsafe`) is deferred. Rhai `AST` is `Send+Sync`, `compile` pure, `Scope::rewind_scope(true)` gives LIFO `EffectGuard` discipline, `watch_many` hashes `script` for `Fiber::refresh` epoch.

## Tool Usage

DB `runtime_tools` row:
```json
{
  "tool_type": "rhai",
  "parameters_schema": {"type":"object","properties":{"a":{"type":"number"}}},
  "execution_config": {
    "script": "fn execute(args){ args[\"a\"] + args[\"b\"] }",
    "entry": "execute",
    "max_ops": 50000,
    "timeout_ms": 2000
  }
}
```

Validation: `RuntimeToolRegistry::validate_execution_config("rhai", &value)` does `Engine::new().set_max_operations(...).compile(script)` and returns `400` with `line:col` on syntax error.

Execution: `RhaiTool::execute(Value) -> Value`
- `Value` -> `Dynamic` via `rhai::serde::to_dynamic`
- `Scope::push("args", dynamic)` plus individual keys
- `engine.call_fn(&mut scope, &ast, &entry, (dynamic,))` inside `tokio::task::spawn_blocking` with `tokio::time::timeout`
- `Dynamic` -> `Value` via `rhai::serde::from_dynamic` / `serde_json::to_value`, fallback `Null` for `()`

Limits: `max_operations 50k`, `max_string 8192`, `max_call_levels 64`, `max_expr_depth 128`, `on_print false`, no `eval`, no `fs`/`net` unless registered. Module allow-list only `json`, `math`.

Tests: `crates/ares-tools/src/rhai_tool.rs` 10 tests `cargo test -p ares-tools --lib rhai_tool --features postgres,mcp`.

## Cordis Bridge

`crates/ares-cordis-core/src/rhai_service.rs` `#[cfg(feature="rhai")]` `RhaiService{engine Arc<Engine>, ast AST}` `impl Service { name/init/check via call_fn }` + `RhaiPlugin: Plugin<Config=RhaiServiceConfig, Provides=RhaiService>` `apply: ctx.provide(service)`.

Feature: `ares-cordis-core/rhai = ["dep:rhai"]` optional, default `inventory`. `cargo check --features rhai` and `cargo test --features rhai` 27 tests `hmd`.

Loader: `Entry{plugin:"rhai:my_tool", config: Value::String(script)}` — `Loader::reconcile` hashes `script`, `Fiber::refresh` epoch `:uid` changes → `reload()` without `libloading`.

Hot-reload: `AST` compiled once at `build_and_swap`, `ArcSwap` readers get `Arc<RhaiTool>`. Source stored in `runtime_tool_versions`, recomputed on `reload()`.

## Call-fn Reference

```rust
let ast = engine.compile(script)?;
let mut scope = Scope::new();
scope.push("args", dynamic);
let result: Dynamic = engine.call_fn(&mut scope, &ast, "execute", (dynamic,))?;
// options:
CallFnOptions::new().eval_ast(true).rewind_scope(true).bind_this_ptr(&mut ctx)
```

Limits: `engine.set_max_operations(50000)`, `set_max_string_size`, `set_max_call_levels`, `set_max_expr_depth`, `on_progress`.

See `https://rhai.rs/book/engine/call-fn.html` for `Scope` rewind, `FuncArgs`, `this_ptr`.
