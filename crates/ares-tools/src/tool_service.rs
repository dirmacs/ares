//! Tools capability: tenant-aware tool resolution.
//!
//! Precedence: `tenant runtime → fleet runtime → static` (static includes
//! `mcp_bridge` registrations). Callers obtain the service via
//! `ctx.get::<Tools>()` and isolate with `ctx.isolate::<Tools>(tenant_id)`.

use std::any::TypeId;
use std::collections::HashSet;
use std::future::Future;
use std::sync::Arc;

use ares_types::types::{Result, ToolDefinition};
use cordis::{CordisError, EventsService, Service};
use serde_json::{json, Value};

use crate::registry::{Tool, ToolRegistry};

#[cfg(any(feature = "postgres", test))]
use crate::runtime_registry::RuntimeToolRegistry;

/// Tenant-aware tool capability.
///
/// Isolate labels on [`Tools`] win over a `TenantContext` intercept.
pub struct Tools {
    static_registry: Arc<ToolRegistry>,
    #[cfg(any(feature = "postgres", test))]
    runtime: Option<Arc<RuntimeToolRegistry>>,
}

impl Clone for Tools {
    fn clone(&self) -> Self {
        Self {
            static_registry: Arc::clone(&self.static_registry),
            #[cfg(any(feature = "postgres", test))]
            runtime: self.runtime.clone(),
        }
    }
}

impl Tools {
    pub(crate) fn new(static_registry: Arc<ToolRegistry>) -> Self {
        Self {
            static_registry,
            #[cfg(any(feature = "postgres", test))]
            runtime: None,
        }
    }

    #[cfg(any(feature = "postgres", test))]
    #[allow(dead_code)]
    pub(crate) fn with_runtime(
        static_registry: Arc<ToolRegistry>,
        runtime: Option<Arc<RuntimeToolRegistry>>,
    ) -> Self {
        Self {
            static_registry,
            runtime,
        }
    }

    /// Build Tools from a static tool set. Runtime is unset.
    pub fn from_static(tools: impl IntoIterator<Item = Arc<dyn Tool>>) -> Self {
        let mut registry = ToolRegistry::new();
        for tool in tools {
            registry.register(tool);
        }
        Self::new(Arc::new(registry))
    }

    /// Validate runtime tool execution_config without exposing the registry.
    pub fn validate_runtime_tool_execution_config(
        tool_type: &str,
        execution_config: &Value,
    ) -> Result<()> {
        #[cfg(any(feature = "postgres", test))]
        {
            RuntimeToolRegistry::validate_execution_config(tool_type, execution_config)
        }
        #[cfg(not(any(feature = "postgres", test)))]
        {
            let _ = (tool_type, execution_config);
            Ok(())
        }
    }

    /// Resolve a tool using the tenant derived from `ctx` (isolate, then intercept).
    pub fn resolve(&self, ctx: &Arc<cordis::Context>, name: &str) -> Option<Arc<dyn Tool>> {
        let tenant = tenant_id_from_tool_ctx(ctx);
        let Some(events) = ctx.get::<EventsService>() else {
            return self.resolve_named(name, tenant.as_deref());
        };
        let payload = json!({ "name": name, "tenant": tenant });
        let this = self.clone();
        let out = match run_waterfall(&events, "tools.resolve", payload, move |p| {
            async move {
                let n = p.get("name").and_then(Value::as_str).unwrap_or("").to_string();
                let tenant = p.get("tenant").and_then(Value::as_str).map(str::to_string);
                let found = this.resolve_named(&n, tenant.as_deref()).is_some();
                Ok(json!({
                    "name": n,
                    "tenant": p.get("tenant").cloned().unwrap_or(Value::Null),
                    "found": found,
                }))
            }
        }) {
            Ok(v) => v,
            Err(_) => return self.resolve_named(name, tenant.as_deref()),
        };
        if out.get("deny").and_then(Value::as_bool) == Some(true) {
            return None;
        }
        out.get("found")?;
        let resolved_name = out
            .get("name")
            .and_then(Value::as_str)
            .unwrap_or(name);
        self.resolve_named(resolved_name, tenant.as_deref())
    }

    /// List tools using the tenant derived from `ctx` (isolate, then intercept).
    pub fn list(&self, ctx: &Arc<cordis::Context>) -> Vec<ToolDefinition> {
        let tenant = tenant_id_from_tool_ctx(ctx);
        let Some(events) = ctx.get::<EventsService>() else {
            return self.list_named(tenant.as_deref());
        };
        let payload = json!({ "tenant": tenant });
        let this = self.clone();
        let out = match run_waterfall(&events, "tools.list", payload, move |p| {
            async move {
                let tenant = p.get("tenant").and_then(Value::as_str).map(str::to_string);
                let tools = this.list_named(tenant.as_deref());
                Ok(json!({
                    "tenant": p.get("tenant").cloned().unwrap_or(Value::Null),
                    "tools": tools,
                }))
            }
        }) {
            Ok(v) => v,
            Err(_) => return self.list_named(tenant.as_deref()),
        };
        match out
            .get("tools")
            .cloned()
            .and_then(|t| serde_json::from_value::<Vec<ToolDefinition>>(t).ok())
        {
            Some(defs) => defs,
            None => self.list_named(tenant.as_deref()),
        }
    }

    /// Execute a named tool, wrapping the call in `tools.execute` around-middleware
    /// when [`EventsService`] is on `ctx`.
    pub async fn execute(
        &self,
        ctx: &Arc<cordis::Context>,
        name: &str,
        args: Value,
    ) -> Result<Value> {
        let tool = self.resolve(ctx, name).ok_or_else(|| {
            ares_types::AppError::NotFound(format!("Tool not found: {name}"))
        })?;
        let Some(events) = ctx.get::<EventsService>() else {
            return tool.execute(args).await;
        };
        let payload = json!({ "name": name, "args": args });
        let out = events
            .waterfall_around("tools.execute".into(), payload, move |p| {
                async move {
                    let exec_args = p.get("args").cloned().unwrap_or(Value::Null);
                    let result = tool
                        .execute(exec_args)
                        .await
                        .map_err(|e| CordisError::Fiber(e.to_string()))?;
                    let mut out = p;
                    if let Some(obj) = out.as_object_mut() {
                        obj.insert("result".into(), result);
                    } else {
                        out = json!({ "result": result });
                    }
                    Ok(out)
                }
            })
            .await
            .map_err(|e| ares_types::AppError::Internal(e.to_string()))?;
        Ok(out.get("result").cloned().unwrap_or(Value::Null))
    }

    /// Reload runtime tools from the database when a runtime registry is attached.
    pub async fn reload(&self) -> Result<()> {
        #[cfg(any(feature = "postgres", test))]
        if let Some(rt) = &self.runtime {
            rt.reload().await?;
        }
        Ok(())
    }

    /// Runtime registry for admin mutation. Not a Service; do not `ctx.get` it.
    #[cfg(any(feature = "postgres", test))]
    #[allow(dead_code)]
    pub(crate) fn runtime(&self) -> Option<Arc<RuntimeToolRegistry>> {
        self.runtime.clone()
    }

    /// Concrete runtime tool type after tenant visibility checks.
    pub fn tool_type(&self, ctx: &Arc<cordis::Context>, name: &str) -> Option<String> {
        #[cfg(any(feature = "postgres", test))]
        {
            let tenant = tenant_id_from_tool_ctx(ctx);
            self
                .runtime
                .as_ref()
                .and_then(|rt| rt.tool_type_for_tenant(name, tenant.as_deref()))
        }
        #[cfg(not(any(feature = "postgres", test)))]
        {
            let _ = (ctx, name);
            None
        }
    }

    fn resolve_named(&self, name: &str, tenant: Option<&str>) -> Option<Arc<dyn Tool>> {
        #[cfg(any(feature = "postgres", test))]
        if let Some(rt) = &self.runtime {
            if let Some(tid) = tenant {
                if let Some(tool) = rt.get_for_tenant(name, Some(tid)) {
                    return Some(tool);
                }
            }
            if let Some(tool) = rt.get(name) {
                return Some(tool);
            }
        }
        #[cfg(not(any(feature = "postgres", test)))]
        let _ = tenant;
        self.static_registry.get(name).cloned()
    }

    fn list_named(&self, tenant: Option<&str>) -> Vec<ToolDefinition> {
        let mut seen: HashSet<String> = HashSet::new();
        let mut out: Vec<ToolDefinition> = Vec::new();

        let push_defs =
            |defs: Vec<ToolDefinition>, seen: &mut HashSet<String>, out: &mut Vec<ToolDefinition>| {
                for d in defs {
                    if seen.insert(d.name.clone()) {
                        out.push(d);
                    }
                }
            };

        #[cfg(any(feature = "postgres", test))]
        if let Some(rt) = &self.runtime {
            push_defs(
                rt.get_tool_definitions_for_tenant(tenant),
                &mut seen,
                &mut out,
            );
            if tenant.is_some() {
                let remaining: Vec<ToolDefinition> = rt
                    .get_tool_definitions()
                    .into_iter()
                    .filter(|d| !seen.contains(&d.name))
                    .collect();
                push_defs(remaining, &mut seen, &mut out);
            }
        }
        #[cfg(not(any(feature = "postgres", test)))]
        let _ = tenant;

        push_defs(
            self.static_registry.get_tool_definitions(),
            &mut seen,
            &mut out,
        );
        out
    }
}

impl Service for Tools {
    fn check(&self) -> bool {
        true
    }
}

/// Derive the tenant id for tool resolution from `ctx`.
///
/// Isolate labels on [`Tools`] win. A leading `tenant:` or `user:` prefix is
/// stripped; a non-empty remainder is the tenant. If the isolate label is
/// missing or empty after stripping, fall back to a `TenantContext` intercept.
/// Unlabeled contexts with no intercept yield `None`.
fn tenant_id_from_tool_ctx(ctx: &Arc<cordis::Context>) -> Option<String> {
    if let Some(label) = ctx.isolate_label(TypeId::of::<Tools>()) {
        let trimmed = label
            .strip_prefix("tenant:")
            .or_else(|| label.strip_prefix("user:"))
            .unwrap_or(&label);
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    ctx.get::<ares_types::models::TenantContext>()
        .map(|tc| tc.tenant_id.clone())
        .filter(|id| !id.is_empty())
}

fn run_waterfall<F, Fut>(
    events: &EventsService,
    event: &str,
    payload: Value,
    core: F,
) -> std::result::Result<Value, CordisError>
where
    F: FnOnce(Value) -> Fut + Send + 'static,
    Fut: Future<Output = std::result::Result<Value, CordisError>> + Send + 'static,
{
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        return Err(CordisError::Fiber("no tokio runtime".into()));
    };
    tokio::task::block_in_place(|| {
        handle.block_on(events.waterfall_around(event.into(), payload, core))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::models::{TenantContext, TenantTier};
    use async_trait::async_trait;
    use cordis::Context;
    use std::sync::atomic::{AtomicBool, Ordering};

    struct ProbeTool {
        name: String,
        ran: Option<Arc<AtomicBool>>,
    }

    impl ProbeTool {
        fn new(name: impl Into<String>) -> Self {
            Self {
                name: name.into(),
                ran: None,
            }
        }
    }

    #[async_trait]
    impl Tool for ProbeTool {
        fn name(&self) -> &str {
            &self.name
        }
        fn description(&self) -> &str {
            "probe"
        }
        fn parameters_schema(&self) -> Value {
            json!({})
        }
        async fn execute(&self, _args: Value) -> Result<Value> {
            if let Some(ran) = &self.ran {
                ran.store(true, Ordering::SeqCst);
            }
            Ok(json!({ "ok": self.name }))
        }
    }

    #[test]
    fn unlabeled_root_yields_no_tenant() {
        let ctx = Context::new_root();
        assert_eq!(tenant_id_from_tool_ctx(&ctx), None);
    }

    #[test]
    fn intercept_tenant_context_yields_acme() {
        let ctx = Context::new_root()
            .with_intercept(TenantContext::new("acme".into(), TenantTier::Pro));
        assert_eq!(tenant_id_from_tool_ctx(&ctx).as_deref(), Some("acme"));
    }

    #[test]
    fn isolate_wins_over_intercept() {
        let ctx = Context::new_root()
            .with_intercept(TenantContext::new("acme".into(), TenantTier::Pro))
            .isolate::<Tools>("tenant:iso");
        assert_eq!(tenant_id_from_tool_ctx(&ctx).as_deref(), Some("iso"));
    }

    #[test]
    fn resolve_missing_tool_is_none() {
        let svc = Tools::new(Arc::new(ToolRegistry::new()));
        let ctx = Context::new_root();
        assert!(svc.resolve(&ctx, "missing").is_none());
        assert!(svc.list(&ctx).is_empty());
    }

    #[test]
    fn list_and_resolve_use_ctx_isolate() {
        let mut registry = ToolRegistry::new();
        registry.register(Arc::new(crate::calculator::Calculator));
        let svc = Tools::with_runtime(Arc::new(registry), None);
        let ctx = Context::new_root().isolate::<Tools>("tenant:acme");
        assert!(svc.resolve(&ctx, "calculator").is_some());
        assert!(svc.list(&ctx).iter().any(|d| d.name == "calculator"));
        assert!(svc.resolve(&ctx, "unknown").is_none());
    }

    #[test]
    fn from_static_resolves_calculator() {
        let svc = Tools::from_static([Arc::new(crate::calculator::Calculator) as Arc<dyn Tool>]);
        let ctx = Context::new_root();
        assert!(svc.resolve(&ctx, "calculator").is_some());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tools_list_waterfall_filters_tool() {
        let svc = Tools::from_static([
            Arc::new(ProbeTool::new("a")) as Arc<dyn Tool>,
            Arc::new(ProbeTool::new("b")) as Arc<dyn Tool>,
        ]);
        let ctx = Context::new_root();
        ctx.provide(EventsService::new());
        let names: Vec<_> = svc.list(&ctx).into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(names.contains(&"b".to_string()));

        let events = ctx.get::<EventsService>().expect("events");
        events.on_waterfall("tools.list".into(), |payload, next| async move {
            let mut out = next(payload).await?;
            if let Some(arr) = out.get_mut("tools").and_then(Value::as_array_mut) {
                arr.retain(|t| t.get("name").and_then(Value::as_str) != Some("b"));
            }
            Ok(out)
        });
        let names: Vec<_> = svc.list(&ctx).into_iter().map(|d| d.name).collect();
        assert!(names.contains(&"a".to_string()));
        assert!(!names.contains(&"b".to_string()));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tools_resolve_waterfall_deny() {
        let svc = Tools::from_static([Arc::new(ProbeTool::new("a")) as Arc<dyn Tool>]);
        let ctx = Context::new_root();
        ctx.provide(EventsService::new());
        assert!(svc.resolve(&ctx, "a").is_some());
        let events = ctx.get::<EventsService>().expect("events");
        events.on_waterfall("tools.resolve".into(), |payload, _next| async move {
            Ok(payload)
        });
        assert!(svc.resolve(&ctx, "a").is_none());
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tools_execute_core_runs() {
        let svc = Tools::from_static([Arc::new(ProbeTool::new("probe")) as Arc<dyn Tool>]);
        let ctx = Context::new_root();
        ctx.provide(EventsService::new());
        let out = svc
            .execute(&ctx, "probe", json!({}))
            .await
            .expect("execute");
        assert_eq!(out, json!({ "ok": "probe" }));
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn tools_execute_short_circuit_skips_tool() {
        let ran = Arc::new(AtomicBool::new(false));
        let svc = Tools::from_static([Arc::new(ProbeTool {
            name: "probe".into(),
            ran: Some(Arc::clone(&ran)),
        }) as Arc<dyn Tool>]);
        let ctx = Context::new_root();
        ctx.provide(EventsService::new());
        let events = ctx.get::<EventsService>().expect("events");
        events.on_waterfall("tools.execute".into(), |_payload, _next| async move {
            Ok(json!({ "result": { "short": true } }))
        });
        let out = svc
            .execute(&ctx, "probe", json!({}))
            .await
            .expect("execute");
        assert_eq!(out, json!({ "short": true }));
        assert!(
            !ran.load(Ordering::SeqCst),
            "tool.execute must not run when handler short-circuits"
        );
    }
}
