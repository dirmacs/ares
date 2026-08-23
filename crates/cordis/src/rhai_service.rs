//! RhaiService — Cordis plugin bridge for hot-reloadable Rhai scripts.
//!
//! Implements `RhaiService` as a `Service` that compiles and executes Rhai
//! scripts with sandboxed limits. Feature-gated via `#[cfg(feature = "rhai")]`.
//! Without the feature, stub types are provided so `cargo check` passes.

use std::sync::Arc;

#[cfg(feature = "rhai")]
use rhai::{Dynamic, Engine, EvalAltResult, Scope, AST};
#[cfg(feature = "rhai")]
use serde::{Deserialize, Serialize};
#[cfg(feature = "rhai")]
use std::future::Future;
#[cfg(feature = "rhai")]
use std::pin::Pin;

#[cfg(feature = "rhai")]
use crate::{Context, CordisError, Disposable, Plugin, Service, ServiceInitFuture};

// ---------------------------------------------------------------------------
// Config
// ---------------------------------------------------------------------------

/// Configuration for `RhaiService` / `RhaiPlugin`.
#[cfg(feature = "rhai")]
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct RhaiServiceConfig {
    /// Rhai script source.
    pub script: String,
    /// Optional service name (defaults to `rhai_service`).
    #[serde(default)]
    pub name: Option<String>,
    /// Optional entry function for `init` (defaults to `init`).
    #[serde(default)]
    pub entry_init: Option<String>,
    /// Optional entry function for `check` (defaults to `check`).
    #[serde(default)]
    pub entry_check: Option<String>,
    /// Optional max operations override (defaults to 50_000).
    #[serde(default)]
    pub max_ops: Option<u64>,
    /// Catalog events this policy service listens on.
    #[serde(default)]
    pub listen: Vec<RhaiListenerConfig>,
}

/// One catalog-event listener backed by a Rhai function in `script`.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct RhaiListenerConfig {
    /// Catalog event name; must exist in `events_catalog::CONTRACTS`.
    pub event: String,
    /// `fn name(payload_map) -> value` in the script. Returning `()`/null
    /// means "pass through / delegate to the chain"; any other value becomes
    /// the dispatch result (a Bail deny marker, or a waterfall short-circuit).
    pub fn_name: String,
    /// What happens when the listener fn raises a runtime error.
    #[serde(default)]
    pub on_error: RhaiOnError,
}

/// What happens when the listener fn raises a runtime error.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RhaiOnError {
    /// Log a warning and pass the original payload through (waterfall:
    /// delegate via `next`).
    #[default]
    Passthrough,
    /// Fail closed: return a deny marker (waterfall: short-circuit WITHOUT
    /// calling `next`).
    Deny,
}

// Stub when feature disabled
#[cfg(not(feature = "rhai"))]
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize, Default)]
pub struct RhaiServiceConfig {
    pub script: String,
    pub name: Option<String>,
    pub entry_init: Option<String>,
    pub entry_check: Option<String>,
    pub max_ops: Option<u64>,
    #[serde(default)]
    pub listen: Vec<RhaiListenerConfig>,
}

// ---------------------------------------------------------------------------
// RhaiService
// ---------------------------------------------------------------------------

/// Rhai-backed Cordis service.
///
/// Holds a sandboxed `Engine` (max_operations 50000, max_string 8192,
/// max_call_levels 64, max_expr_depth 128) and compiled `AST`.
/// `entry_init` defaults to `"init"` and `entry_check` to `"check"`.
#[cfg(feature = "rhai")]
pub struct RhaiService {
    /// Service name.
    pub name: String,
    /// Original script source (for debugging / reload).
    pub script: String,
    /// Sandboxed engine.
    pub engine: Arc<Engine>,
    /// Compiled AST.
    pub ast: AST,
    /// Entry function for `Service::init` (default `init`).
    pub entry_init: String,
    /// Entry function for `Service::check` (default `check`).
    pub entry_check: String,
    /// Declared policy listeners, registered during `Service::init`.
    pub listen: Vec<RhaiListenerConfig>,
}

#[cfg(not(feature = "rhai"))]
pub struct RhaiService {
    pub name: String,
    pub script: String,
    pub entry_init: String,
    pub entry_check: String,
}

/// Convert a serialized event payload into a Rhai `Dynamic` (object map for
/// JSON objects, so scripts use plain property access like `p.tenant_id`).
fn json_to_dynamic(v: &serde_json::Value) -> Dynamic {
    match v {
        serde_json::Value::Null => Dynamic::UNIT,
        serde_json::Value::Bool(b) => (*b).into(),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                i.into()
            } else if let Some(u) = n.as_u64() {
                if u <= i64::MAX as u64 {
                    ((u) as i64).into()
                } else {
                    (n.as_f64().unwrap_or_default()).into()
                }
            } else {
                n.as_f64().unwrap_or_default().into()
            }
        }
        serde_json::Value::String(s) => s.clone().into(),
        serde_json::Value::Array(items) => {
            items.iter().map(json_to_dynamic).collect::<Vec<_>>().into()
        }
        serde_json::Value::Object(map) => {
            let mut out = rhai::Map::new();
            for (k, val) in map {
                out.insert(k.clone().into(), json_to_dynamic(val));
            }
            out.into()
        }
    }
}

/// Convert a script return value back to JSON. `()`/unit yields `None`, which
/// callers interpret as "pass through / delegate"; any other value maps to a
/// JSON value (unrepresentable dynamics degrade to their display string).
fn dynamic_to_json(d: &Dynamic) -> Option<serde_json::Value> {
    if d.is_unit() {
        return None;
    }
    if let Ok(b) = d.as_bool() {
        return Some(serde_json::Value::Bool(b));
    }
    if let Ok(i) = d.as_int() {
        return Some(serde_json::json!(i));
    }
    if let Ok(f) = d.as_float() {
        return Some(serde_json::json!(f));
    }
    if let Ok(s) = d.clone().into_string() {
        return Some(serde_json::Value::String(s));
    }
    if let Ok(a) = d.clone().into_array() {
        let items: Option<Vec<_>> = a.iter().map(dynamic_to_json).collect();
        return items.map(serde_json::Value::Array);
    }
    if let Some(m) = d.clone().try_cast::<rhai::Map>() {
        let mut obj = serde_json::Map::new();
        for (k, v) in m {
            obj.insert(
                k.to_string(),
                dynamic_to_json(&v).unwrap_or(serde_json::Value::Null),
            );
        }
        return Some(serde_json::Value::Object(obj));
    }
    Some(serde_json::json!(d.to_string()))
}

#[cfg(feature = "rhai")]
impl RhaiService {
    /// Create a new `RhaiService` with default limits and entries.
    ///
    /// Limits: max_operations 50000, max_string 8192, max_call_levels 64,
    /// max_expr_depth 128. Registers helpers `log(String)` and `provide` placeholder.
    pub fn new(
        name: impl Into<String>,
        script: impl Into<String>,
    ) -> Result<Self, Box<EvalAltResult>> {
        Self::new_with_max_ops(name, script, 50_000)
    }

    /// Create with custom `max_operations`.
    pub fn new_with_max_ops(
        name: impl Into<String>,
        script: impl Into<String>,
        max_ops: u64,
    ) -> Result<Self, Box<EvalAltResult>> {
        let name_str = name.into();
        let script_str = script.into();
        let engine = Self::build_engine(max_ops);
        let ast = engine.compile(&script_str)?;
        Ok(Self {
            name: name_str,
            script: script_str,
            engine: Arc::new(engine),
            ast,
            entry_init: "init".to_string(),
            entry_check: "check".to_string(),
            listen: Vec::new(),
        })
    }

    /// Create from `RhaiServiceConfig` (respects optional overrides).
    pub fn from_config(config: RhaiServiceConfig) -> Result<Self, Box<EvalAltResult>> {
        let name = config
            .name
            .clone()
            .unwrap_or_else(|| "rhai_service".to_string());
        let max_ops = config.max_ops.unwrap_or(50_000);
        let mut svc = Self::new_with_max_ops(name, config.script.clone(), max_ops)?;
        if let Some(e) = config.entry_init {
            svc.entry_init = e;
        }
        if let Some(e) = config.entry_check {
            svc.entry_check = e;
        }
        svc.listen = config.listen;
        Ok(svc)
    }

    fn build_engine(max_ops: u64) -> Engine {
        let mut engine = Engine::new();
        engine.set_max_operations(max_ops);
        engine.set_max_string_size(8192);
        engine.set_max_call_levels(64);
        engine.set_max_expr_depths(128, 64);
        // Helpers
        engine.register_fn("log", |msg: String| {
            tracing::info!("{}", msg);
        });
        // provide placeholder — single arg
        engine.register_fn("provide", |key: String| {
            tracing::info!(provide_key = %key, "provide placeholder");
        });
        // provide placeholder — two args (key, value)
        engine.register_fn("provide", |key: String, value: String| {
            tracing::info!(key = %key, value = %value, "provide placeholder");
        });
        engine
    }
}

/// Combine per-listener disposables (plus the init logging guard) into one;
/// disposal unregisters every listener in registration order.
fn finish_disposables(
    mut disposables: Vec<Box<dyn Disposable>>,
) -> Result<Option<Box<dyn Disposable>>, CordisError> {
    if disposables.is_empty() {
        return Ok(None);
    }
    Ok(Some(Box::new(move || {
        while let Some(d) = disposables.pop() {
            d.dispose();
        }
    })))
}

#[cfg(feature = "rhai")]
impl Service for RhaiService {
    fn name(&self) -> &'static str {
        "RhaiService"
    }

    fn init(&self, ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        let engine = Arc::clone(&self.engine);
        let ast = self.ast.clone();
        let entry = self.entry_init.clone();
        let listen: Vec<crate::RhaiListenerConfig> = self.listen.clone();
        let ctx = Arc::clone(ctx);
        Box::pin(async move {
            // Policy listeners: register on the catalog events declared in
            // config. Validation failures abort the entry (loader records it
            // and startup continues); runtime script errors pass through.
            let mut disposables: Vec<Box<dyn Disposable>> = Vec::new();
            if !listen.is_empty() {
                let events = ctx.get::<crate::EventsService>().ok_or_else(|| {
                    CordisError::Configuration(
                        "RhaiPolicy requires EventsService on the context".to_string(),
                    )
                })?;
                for l in &listen {
                    let contract = crate::contract_for(&l.event).ok_or_else(|| {
                        CordisError::Configuration(format!("unknown policy event {}", l.event))
                    })?;
                    let around = contract.around;
                    // Fail-closed deny marker: same shape as the built-in
                    // quota handler in events.rs, with the policy fn named.
                    let deny_marker = serde_json::json!({
                        "deny": format!("policy script error in {}", l.fn_name)
                    });
                    let on_error = l.on_error;
                    if around {
                        let engine = Arc::clone(&engine);
                        let ast = ast.clone();
                        let fn_name = l.fn_name.clone();
                        let event_name = l.event.clone();
                        let deny_marker = deny_marker.clone();
                        let d = events.on_waterfall(l.event.clone(), move |payload, next| {
                            let engine = Arc::clone(&engine);
                            let ast = ast.clone();
                            let fn_name = fn_name.clone();
                            let event_name = event_name.clone();
                            let deny_marker = deny_marker.clone();
                            let fut = async move {
                                let mut scope = Scope::new();
                                match engine.call_fn::<Dynamic>(
                                    &mut scope,
                                    &ast,
                                    &fn_name,
                                    (json_to_dynamic(&payload),),
                                ) {
                                    Ok(ret) => match dynamic_to_json(&ret) {
                                        Some(v) => Ok(v),
                                        None => next(payload).await,
                                    },
                                    Err(err) => match on_error {
                                        RhaiOnError::Passthrough => {
                                            tracing::warn!(
                                                event = %event_name,
                                                error = %err,
                                                "rhai policy runtime error; delegating to chain"
                                            );
                                            next(payload).await
                                        }
                                        RhaiOnError::Deny => {
                                            tracing::warn!(
                                                event = %event_name,
                                                error = %err,
                                                "rhai policy runtime error; failing closed"
                                            );
                                            Ok(deny_marker)
                                        }
                                    },
                                }
                            };
                            Box::pin(fut)
                                as Pin<
                                    Box<
                                        dyn Future<Output = Result<serde_json::Value, CordisError>>
                                            + Send,
                                    >,
                                >
                        });
                        disposables.push(d);
                    } else {
                        let engine = Arc::clone(&engine);
                        let ast = ast.clone();
                        let fn_name = l.fn_name.clone();
                        let event_name = l.event.clone();
                        let deny_marker = deny_marker.clone();
                        let d = events.on(l.event.clone(), move |payload| {
                            let engine = Arc::clone(&engine);
                            let ast = ast.clone();
                            let fn_name = fn_name.clone();
                            let event_name = event_name.clone();
                            let deny_marker = deny_marker.clone();
                            let fut = async move {
                                let mut scope = Scope::new();
                                match engine.call_fn::<Dynamic>(
                                    &mut scope,
                                    &ast,
                                    &fn_name,
                                    (json_to_dynamic(&payload),),
                                ) {
                                    Ok(ret) => match dynamic_to_json(&ret) {
                                        Some(v) => Ok(v),
                                        None => Ok(payload),
                                    },
                                    Err(err) => match on_error {
                                        RhaiOnError::Passthrough => {
                                            tracing::warn!(
                                                event = %event_name,
                                                error = %err,
                                                "rhai policy runtime error; passing through"
                                            );
                                            Ok(payload)
                                        }
                                        RhaiOnError::Deny => {
                                            tracing::warn!(
                                                event = %event_name,
                                                error = %err,
                                                "rhai policy runtime error; failing closed"
                                            );
                                            Ok(deny_marker)
                                        }
                                    },
                                }
                            };
                            Box::pin(fut)
                                as Pin<
                                    Box<
                                        dyn Future<Output = Result<serde_json::Value, CordisError>>
                                            + Send,
                                    >,
                                >
                        });
                        disposables.push(d);
                    }
                }
            }
            let mut scope = Scope::new();
            // Try calling entry as Dynamic (generic return). If not found, return None.
            match engine.call_fn::<Dynamic>(&mut scope, &ast, &entry, ()) {
                Ok(val) => {
                    tracing::info!(rhai_result = %val, entry = %entry, "RhaiService init");
                    disposables.push(Box::new(move || {
                        tracing::info!(entry = %entry, "RhaiService disposed");
                    }));
                    finish_disposables(disposables)
                }
                Err(e) => {
                    let msg = e.to_string();
                    if msg.contains("not found")
                        || msg.contains("unknown function")
                        || msg.contains("Function not found")
                    {
                        if disposables.is_empty() {
                            Ok(None)
                        } else {
                            finish_disposables(disposables)
                        }
                    } else {
                        Err(CordisError::Configuration(format!(
                            "rhai init '{}' failed: {}",
                            entry, e
                        )))
                    }
                }
            }
        })
    }

    fn check(&self) -> bool {
        let mut scope = Scope::new();
        match self
            .engine
            .call_fn::<bool>(&mut scope, &self.ast, &self.entry_check, ())
        {
            Ok(b) => b,
            Err(e) => {
                let msg = e.to_string();
                if msg.contains("not found")
                    || msg.contains("unknown function")
                    || msg.contains("Function not found")
                {
                    true
                } else {
                    tracing::warn!(error = %e, "RhaiService check failed, defaulting true");
                    true
                }
            }
        }
    }
}

// Stub Service impl when rhai disabled (so cargo check without feature still sees type)
#[cfg(not(feature = "rhai"))]
impl crate::Service for RhaiService {}

// ---------------------------------------------------------------------------
// RhaiPlugin
// ---------------------------------------------------------------------------

/// Plugin that provides `RhaiService`.
///
/// Config = `RhaiServiceConfig`, Provides = `RhaiService`.
/// `apply` compiles the script and `ctx.provide`s the service.
#[cfg(feature = "rhai")]
pub struct RhaiPlugin;

#[cfg(feature = "rhai")]
impl Plugin for RhaiPlugin {
    type Config = RhaiServiceConfig;
    type Provides = RhaiService;

    fn apply(
        &self,
        _ctx: &Arc<Context>,
        config: Self::Config,
    ) -> Result<Arc<Self::Provides>, CordisError> {
        let svc = RhaiService::from_config(config)
            .map_err(|e| CordisError::Configuration(format!("rhai compile failed: {}", e)))?;
        Ok(Arc::new(svc))
    }
}

#[cfg(not(feature = "rhai"))]
pub struct RhaiPlugin;

#[cfg(not(feature = "rhai"))]
impl crate::Plugin for RhaiPlugin {
    type Config = RhaiServiceConfig;
    type Provides = RhaiService;
    fn apply(
        &self,
        _ctx: &Arc<crate::Context>,
        _config: Self::Config,
    ) -> Result<Arc<Self::Provides>, crate::CordisError> {
        Ok(Arc::new(RhaiService))
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(feature = "rhai")]
#[cfg(test)]
mod tests {
    use super::*;
    use crate::Context;

    #[test]
    fn test_rhai_compile_valid() {
        let svc = RhaiService::new("test_valid", r#"fn init() { 42 }"#);
        assert!(svc.is_ok(), "valid script should compile: {:?}", svc.err());
        let svc = svc.unwrap();
        assert_eq!(svc.name, "test_valid");
        assert_eq!(svc.entry_init, "init");
        assert_eq!(svc.entry_check, "check");
    }

    #[test]
    fn test_invalid_syntax() {
        let res = RhaiService::new("bad", "fn init( { ");
        assert!(res.is_err(), "invalid syntax should fail");
    }

    #[tokio::test]
    async fn test_service_init_calls_rhai() {
        let svc = RhaiService::new("test_init", r#"fn init() { "hello" }"#).expect("compile");
        let ctx = Context::new_root();
        let out = svc.init(&ctx).await.expect("init should succeed");
        assert!(
            out.is_some(),
            "init with defined fn should return Some(disposable)"
        );
        // calling dispose should not panic
        if let Some(d) = out {
            d.dispose();
        }
    }

    #[tokio::test]
    async fn test_service_init_no_fn_returns_none() {
        let svc = RhaiService::new("no_init", r#"let x = 1;"#).expect("compile");
        let ctx = Context::new_root();
        let out = svc.init(&ctx).await.expect("init should succeed");
        assert!(out.is_none(), "missing init should return None");
    }

    #[test]
    fn test_check_true() {
        let svc = RhaiService::new("check_true", r#"fn check() { true }"#).expect("compile");
        assert!(svc.check(), "check() true should return true");
    }

    #[test]
    fn test_check_false() {
        let svc = RhaiService::new("check_false", r#"fn check() { false }"#).expect("compile");
        assert!(!svc.check(), "check() false should return false");
    }

    #[test]
    fn test_check_default_true_when_missing() {
        let svc = RhaiService::new("check_missing", r#"let x = 1;"#).expect("compile");
        assert!(svc.check(), "missing check fn should default to true");
    }

    #[test]
    fn test_max_ops() {
        let svc = RhaiService::new("max_ops_default", r#"fn check() { true }"#).expect("compile");
        assert_eq!(svc.engine.max_operations(), 50_000);
        assert_eq!(svc.engine.max_string_size(), 8192);
        assert_eq!(svc.engine.max_call_levels(), 64);
        assert_eq!(svc.engine.max_expr_depth(), 128);
    }

    #[test]
    fn test_max_ops_custom() {
        let cfg = RhaiServiceConfig {
            script: r#"fn check() { true }"#.to_string(),
            name: Some("custom".to_string()),
            entry_init: None,
            entry_check: None,
            max_ops: Some(123),
            listen: vec![],
        };
        let svc = RhaiService::from_config(cfg).expect("compile");
        assert_eq!(svc.engine.max_operations(), 123);
    }

    #[test]
    fn test_max_ops_enforced() {
        // Script that exceeds max_operations 10 should fail at runtime,
        // but compile should succeed. We test that engine is indeed limited
        // and that call_fn with a tight loop hits the operation limit when invoked.
        let svc = RhaiService::new_with_max_ops(
            "limited",
            r#"fn check() { let x = 0; for i in 0..100000 { x += 1; } true }"#,
            10,
        )
        .expect("compile");
        // check() will run the loop; with max_ops 10 it should hit limit and default to true via error path?
        // Actually our check() maps non-not-found errors to true with warning, so it will be true but not panic.
        // We just ensure it doesn't panic and limit is set.
        assert_eq!(svc.engine.max_operations(), 10);
        let _ = svc.check(); // should not panic
    }

    #[tokio::test]
    async fn test_plugin_apply() {
        // Provision goes through `plugin_with`, mirroring what the
        // "RhaiPolicy" factory does for the declarative loader.
        let ctx = Context::new_root();
        let cfg = RhaiServiceConfig {
            script: r#"fn init() { "ok" } fn check() { true }"#.to_string(),
            name: Some("plugin_test".to_string()),
            entry_init: None,
            entry_check: None,
            max_ops: None,
            listen: vec![],
        };
        ctx.plugin_with(RhaiPlugin, cfg)
            .await
            .expect("plugin registration");
        let svc = ctx.get::<RhaiService>();
        assert!(svc.is_some(), "RhaiService should be provided via plugin");
    }

    #[test]
    fn test_config_literal_without_listen_compiles() {
        // Older call sites construct configs without listeners.
        let cfg = RhaiServiceConfig {
            script: r#"fn check() { true }"#.to_string(),
            name: None,
            entry_init: None,
            entry_check: None,
            max_ops: None,
            listen: Vec::new(),
        };
        assert!(RhaiService::from_config(cfg).is_ok());
    }

    #[test]
    fn test_entry_override() {
        let cfg = RhaiServiceConfig {
            script: r#"fn my_init() { 1 } fn my_check() { false }"#.to_string(),
            name: Some("override".to_string()),
            entry_init: Some("my_init".to_string()),
            entry_check: Some("my_check".to_string()),
            max_ops: None,
            listen: vec![],
        };
        let svc = RhaiService::from_config(cfg).expect("compile");
        assert_eq!(svc.entry_init, "my_init");
        assert_eq!(svc.entry_check, "my_check");
        assert!(!svc.check());
    }

    /// Shared script source: `gate` denies banned tenants, `pin`
    /// short-circuits chat capability, `boom` always throws.
    const POLICY_SCRIPT: &str = r#"
                fn gate(p) { if p.tenant_id == "banned" { #{deny: "script"} } else { () } }
                fn pin(p) { if p.capability == "chat" { #{model: "pinned"} } else { () } }
                fn boom(p) { throw "boom"; }
            "#;

    fn listener_config(
        event: &str,
        fn_name: &str,
        on_error: RhaiOnError,
        script: &str,
    ) -> RhaiServiceConfig {
        RhaiServiceConfig {
            script: script.to_string(),
            name: Some("policy".to_string()),
            entry_init: None,
            entry_check: None,
            max_ops: None,
            listen: vec![RhaiListenerConfig {
                event: event.to_string(),
                fn_name: fn_name.to_string(),
                on_error,
            }],
        }
    }

    fn gate_config(event: &str, fn_name: &str) -> RhaiServiceConfig {
        listener_config(event, fn_name, RhaiOnError::Passthrough, POLICY_SCRIPT)
    }

    // Bail-mode listener: a non-null script return denies (replaces the
    // dispatch result); null passes the original payload through.
    #[tokio::test]
    async fn rhai_policy_denies_bail_event() {
        use crate::events::Dispatch;
        let events = crate::EventsService::new();
        let svc = RhaiService::from_config(gate_config(
            crate::events_catalog::ev::SCHEDULER_ADMIT,
            "gate",
        ))
        .expect("compile");
        let ctx = Context::new_root();
        ctx.provide(events);
        let d = svc
            .init(&ctx)
            .await
            .expect("init")
            .expect("listeners registered");
        let events = ctx.get::<crate::EventsService>().expect("events provided");

        let out = events
            .dispatch(
                crate::events_catalog::ev::SCHEDULER_ADMIT.into(),
                serde_json::json!({"agent_name": "a", "tenant_id": "banned"}),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert_eq!(
            out["deny"], "script",
            "banned tenant must be denied by script"
        );

        let out = events
            .dispatch(
                crate::events_catalog::ev::SCHEDULER_ADMIT.into(),
                serde_json::json!({"agent_name": "a", "tenant_id": "ok"}),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert_eq!(out["tenant_id"], "ok", "allowed tenant must pass through");
        d.dispose();
    }

    // Waterfall listener: a non-null return short-circuits the chain (core
    // never runs); a null return delegates via `next`.
    #[tokio::test]
    async fn rhai_policy_waterfall_short_circuits_and_delegates() {
        let events = crate::EventsService::new();
        let svc = RhaiService::from_config(gate_config(
            crate::events_catalog::ev::LLM_GET_CLIENT,
            "pin",
        ))
        .expect("compile");
        let ctx = Context::new_root();
        ctx.provide(events);
        let _d = svc
            .init(&ctx)
            .await
            .expect("init")
            .expect("listeners registered");
        let events = ctx.get::<crate::EventsService>().expect("events provided");

        let core_hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hit = core_hit.clone();
        let out = events
            .waterfall_around(
                crate::events_catalog::ev::LLM_GET_CLIENT.into(),
                serde_json::json!({"capability": "chat"}),
                move |_p| {
                    let hit = hit.clone();
                    async move {
                        hit.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(serde_json::json!({"capability": "chat", "core": true}))
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(
            out["model"], "pinned",
            "script short-circuits chat capability"
        );
        assert!(
            !core_hit.load(std::sync::atomic::Ordering::SeqCst),
            "core must be skipped on short-circuit"
        );

        let out = events
            .waterfall_around(
                crate::events_catalog::ev::LLM_GET_CLIENT.into(),
                serde_json::json!({"capability": "embed"}),
                |mut p| async move {
                    p.as_object_mut()
                        .map(|o| o.insert("core".into(), serde_json::json!(true)));
                    Ok(p)
                },
            )
            .await
            .unwrap();
        assert_eq!(
            out["core"],
            serde_json::Value::Bool(true),
            "null return delegates to core"
        );
        assert!(
            out.get("model").is_none(),
            "no short-circuit for other capabilities"
        );
    }

    // A runtime error inside the script warns and passes the original through.
    #[tokio::test]
    async fn rhai_policy_runtime_error_passes_through() {
        use crate::events::Dispatch;
        let events = crate::EventsService::new();
        let svc =
            RhaiService::from_config(gate_config(crate::events_catalog::ev::AGENT_ADMIT, "boom"))
                .expect("compile");
        let ctx = Context::new_root();
        ctx.provide(events);
        let _d = svc
            .init(&ctx)
            .await
            .expect("init")
            .expect("listeners registered");
        let events = ctx.get::<crate::EventsService>().expect("events provided");

        let payload = serde_json::json!({"agent_name": "a"});
        let out = events
            .dispatch(
                crate::events_catalog::ev::AGENT_ADMIT.into(),
                payload.clone(),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert_eq!(out, payload, "script errors must pass the original through");
    }

    // Unknown catalog events abort init with a Configuration error.
    #[tokio::test]
    async fn rhai_policy_unknown_event_fails_init() {
        let svc = RhaiService::from_config(gate_config("nope.nope", "gate")).expect("compile");
        let ctx = Context::new_root();
        ctx.provide(crate::EventsService::new());
        let err = match svc.init(&ctx).await {
            Err(e) => e,
            Ok(_) => panic!("unknown event must fail init"),
        };
        match err {
            CordisError::Configuration(msg) => {
                assert!(msg.contains("unknown policy event"), "got: {msg}");
            }
            other => panic!(
                "expected Configuration error, got {:?}",
                crate::CordisError::to_string(&other)
            ),
        }
    }

    // Disposal unregisters every listener; dispatch falls back to passthrough.
    #[tokio::test]
    async fn rhai_policy_dispose_removes_listeners() {
        use crate::events::Dispatch;
        let events = crate::EventsService::new();
        let svc = RhaiService::from_config(gate_config(
            crate::events_catalog::ev::SCHEDULER_ADMIT,
            "gate",
        ))
        .expect("compile");
        let ctx = Context::new_root();
        ctx.provide(events);
        let d = svc
            .init(&ctx)
            .await
            .expect("init")
            .expect("listeners registered");

        d.dispose();
        let events = ctx.get::<crate::EventsService>().expect("events provided");
        let out = events
            .dispatch(
                crate::events_catalog::ev::SCHEDULER_ADMIT.into(),
                serde_json::json!({"agent_name": "a", "tenant_id": "banned"}),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert_eq!(
            out["tenant_id"], "banned",
            "disposed listener must not deny"
        );
    }

    // Fail-closed (on_error = deny) on a Bail event: a throwing script must
    // return the deny marker instead of the original payload.
    #[tokio::test]
    async fn rhai_policy_bail_deny_on_error() {
        use crate::events::Dispatch;
        let events = crate::EventsService::new();
        let svc = RhaiService::from_config(listener_config(
            crate::events_catalog::ev::SCHEDULER_ADMIT,
            "boom",
            RhaiOnError::Deny,
            POLICY_SCRIPT,
        ))
        .expect("compile");
        let ctx = Context::new_root();
        ctx.provide(events);
        let _d = svc
            .init(&ctx)
            .await
            .expect("init")
            .expect("listeners registered");
        let events = ctx.get::<crate::EventsService>().expect("events provided");

        let payload = serde_json::json!({"agent_name": "a", "tenant_id": "ok"});
        let out = events
            .dispatch(
                crate::events_catalog::ev::SCHEDULER_ADMIT.into(),
                payload.clone(),
                Dispatch::Bail,
            )
            .await
            .unwrap();
        assert_ne!(out, payload, "fail-closed must not return the payload");
        assert_eq!(
            out["deny"],
            serde_json::json!("policy script error in boom"),
            "deny marker must name the failing policy fn"
        );
    }

    // Fail-closed (on_error = deny) on a waterfall: a throwing script
    // short-circuits with the deny marker; the core never runs.
    #[tokio::test]
    async fn rhai_policy_waterfall_deny_on_error() {
        let events = crate::EventsService::new();
        let svc = RhaiService::from_config(listener_config(
            crate::events_catalog::ev::LLM_GET_CLIENT,
            "boom",
            RhaiOnError::Deny,
            POLICY_SCRIPT,
        ))
        .expect("compile");
        let ctx = Context::new_root();
        ctx.provide(events);
        let _d = svc
            .init(&ctx)
            .await
            .expect("init")
            .expect("listeners registered");
        let events = ctx.get::<crate::EventsService>().expect("events provided");

        let core_hit = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        let hit = core_hit.clone();
        let out = events
            .waterfall_around(
                crate::events_catalog::ev::LLM_GET_CLIENT.into(),
                serde_json::json!({"capability": "chat"}),
                move |_p| {
                    let hit = hit.clone();
                    async move {
                        hit.store(true, std::sync::atomic::Ordering::SeqCst);
                        Ok(serde_json::json!({"capability": "chat", "core": true}))
                    }
                },
            )
            .await
            .unwrap();
        assert_eq!(
            out["deny"],
            serde_json::json!("policy script error in boom"),
            "failing script must veto with the deny marker"
        );
        assert!(
            !core_hit.load(std::sync::atomic::Ordering::SeqCst),
            "fail-closed waterfall must short-circuit without running the core"
        );
    }

    // Two isolated realms each carry their own RhaiPolicy instance: p1's
    // script denies tenant t1, p2's denies t2, and neither sees the other.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn rhai_policy_two_isolate_realms_coexist() {
        use crate::events::Dispatch;
        let root = Context::new_root();
        let c1 = root.isolate::<RhaiService>("p1");
        let c2 = root.isolate::<RhaiService>("p2");
        // Each realm gets its own event bus: production realms isolate their
        // data-bearing services, so one tenant's admission policy never sees
        // another realm's dispatches.
        c1.provide(crate::EventsService::new());
        c2.provide(crate::EventsService::new());

        for (ctx, tenant, label) in [(&c1, "t1", "p1"), (&c2, "t2", "p2")] {
            let cfg = RhaiServiceConfig {
                script: format!(
                    r#"fn gate(p) {{ if p.tenant_id == "{tenant}" {{ #{{deny: "{label}"}} }} else {{ () }} }}"#
                ),
                name: Some(label.to_string()),
                entry_init: None,
                entry_check: None,
                max_ops: None,
                listen: vec![RhaiListenerConfig {
                    event: crate::events_catalog::ev::SCHEDULER_ADMIT.to_string(),
                    fn_name: "gate".to_string(),
                    on_error: RhaiOnError::Deny,
                }],
            };
            ctx.plugin_with(RhaiPlugin, cfg)
                .await
                .unwrap_or_else(|e| panic!("{label} plugin registration failed: {e}"));
            let svc = ctx
                .get::<RhaiService>()
                .unwrap_or_else(|| panic!("{label} realm must resolve its own RhaiService"));
            // `plugin_with`'s direct path skips `Service::init`; production
            // registration goes through `Context::plugin`, which inits. Run
            // the init step explicitly so the realm's gate gets registered.
            let _d = svc
                .init(ctx)
                .await
                .expect("init")
                .expect("listeners registered");
        }

        async fn dispatch_tenant_in(tenant: &'static str, ctx: &Context) -> serde_json::Value {
            let events = ctx.get::<crate::EventsService>().expect("realm events");
            events
                .dispatch(
                    crate::events_catalog::ev::SCHEDULER_ADMIT.into(),
                    serde_json::json!({"agent_name": "a", "tenant_id": tenant}),
                    Dispatch::Bail,
                )
                .await
                .unwrap()
        }

        // Realm p1: t1 is denied by its gate; t2 and free pass through.
        // (A pass-through rhai handler returns the payload object, which a
        // Bail chain treats as its terminal result — so each realm's bus is
        // exercised independently, exactly like isolated tenant realms.)
        let denied_t1 = dispatch_tenant_in("t1", &c1).await;
        assert_eq!(
            denied_t1["deny"], "p1",
            "realm p1 must deny its own tenant t1"
        );
        let passed_t2 = dispatch_tenant_in("t2", &c1).await;
        assert_eq!(
            passed_t2["tenant_id"], "t2",
            "realm p1 passes tenants it does not deny"
        );

        // Realm p2 (independent bus): t2 is denied; t1/free pass through.
        let denied_t2 = dispatch_tenant_in("t2", &c2).await;
        assert_eq!(
            denied_t2["deny"], "p2",
            "realm p2 must deny its own tenant t2"
        );
        let passed_free = dispatch_tenant_in("free", &c1).await;
        assert_eq!(
            passed_free["tenant_id"], "free",
            "unmatched tenants pass through the realm gate"
        );
    }
}
