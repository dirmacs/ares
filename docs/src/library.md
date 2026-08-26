# ARES as a Library

ARES runs as an embedded library. The `ares-server` package publishes a
library facade next to the server binary. You build one Cordis
[context](kernel/index.md), put services on it, and call them. No HTTP port
opens unless you start the router yourself.

This chapter shows every step with code that mirrors real tests in the
repository. Each section names the source it adapts.

## Add the dependency

```toml
[dependencies]
ares-server = "0.10"
ares-cordis = "0.10"   # kernel types beyond the facade re-exports
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
serde_json = "1"
```

The default features are the server defaults: `postgres`, `openai`,
`ares-vector`, `mcp`, `inventory`, and `rhai-policy`. For a lean embed build,
turn them off:

```toml
ares-server = { version = "0.10", default-features = false }
```

The Axum HTTP stack stays a compiled dependency of the package either way.
It never binds a socket unless the `Http` service runs.

## The public surface

`ares_server` re-exports this set (see `src/lib.rs`):

| Export | Purpose |
| ------------------------- | ---------------------------------------------- |
| `Context` | The Cordis context; holds every service |
| `Execute` | Unified agent execution service |
| `Tools` | Tool listing, resolution, and dispatch |
| `Llm` | Large language model client coordination |
| `Store` | Tenant database (feature `postgres`) |
| `Plugin`, `Service` | Factory and service traits from the kernel |
| `Loader`, `PluginRegistry` | Entries-file loader and its factory table |
| `Dispatch` | Event dispatch modes |
| `register_plugins` | Registers all capability-crate factories |

Types such as `AgentRequest`, `ExecutionResult`, `LLMClient`, `LLMResponse`,
`Tool`, `ToolDefinition`, `TenantContext`, and `AppError` ride the same
re-export list. Deeper types live in the capability crates: `ares-tools`,
`ares-llm`, `ares-agent`, and `ares-store`.

## Minimal embed through the loader

The server boots as one ordered pass over an entries file. A library embed
can run the same pass with two calls. This example adapts the in-tree test
`calculator_entry_loads_factory_and_executes_tool` (`src/main.rs`) and the
factory-registration helper `register_loader_factories` in the same file:

```rust
use ares_server::{Context, Loader, PluginRegistry, register_plugins};
use cordis::loader::Entry;

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One context is the whole graph.
    let ctx = Context::new_root();

    // Fill the string-keyed factory table, then put it on the context.
    ctx.provide(PluginRegistry::new());
    let registry = ctx.get::<PluginRegistry>().expect("registry");
    register_plugins(&registry);

    // One entry describes one service instance.
    let entry = Entry {
        id: "calc".to_string(),
        plugin: "CalculatorService".to_string(),
        config: serde_json::json!({}),
        ..Default::default()
    };
    Loader::instantiate(&ctx, &entry.plugin, &entry.config, &entry.id)?;

    // The factory provided a live service. Call it.
    let calc = ctx.get::<ares_tools::CalculatorService>().expect("calculator");
    let output = calc
        .execute(serde_json::json!({ "operation": "add", "a": 2.0, "b": 3.0 }))
        .await?;
    println!("{}", output["result"]); // 5

    Ok(())
}
```

Three facts matter here:

- Use a multi-threaded Tokio runtime. Factories run
  `tokio::task::block_in_place`, which current-thread runtimes reject
  (`crates/ares-tools/src/plugins.rs`).
- `register_plugins` registers the manual fallback chain. With the default
  `inventory` feature, compile-time collected nodes add the same keys; both
  paths land identical factory names (`tests/server_inventory_probe.rs`).
- A second instantiation of the same service type fails with a
  duplicate-provider error. Single-source discipline is active in libraries
  too (`src/main.rs`, same test).

## Pulling facades at call time

Services are values behind their types. Code anywhere in your process pulls
one with `ctx.get::<T>()`. The kernel walks parent contexts, honors tenant
realms, and returns `None` when nothing provides the type. This mirrors
`execute_lists_tools_using_tenant_context_intercept` in
`crates/ares-agent/src/execution.rs`:

```rust
use ares_server::{AgentRequest, Context, Execute, Tools};

async fn run_agent(ctx: &std::sync::Arc<Context>) -> Result<String, ares_server::AppError> {
    // Shared engine, pulled at call time like the tests do.
    let execute = ctx.get::<Execute>().expect("Execute on context");

    if let Some(tools) = ctx.get::<Tools>() {
        for def in tools.list(ctx) {
            println!("available tool: {}", def.name);
        }
    }

    let req = AgentRequest {
        agent_name: "echo".to_string(),
        message: "hi".to_string(),
        ..Default::default()
    };
    let result = execute.run(&req, ctx).await?;
    Ok(result.response.content)
}
```

The same pattern reaches the other facades:

```rust
// Llm: complete a prompt (crates/ares-llm/src/llm_service.rs, `complete`).
if let Some(llm) = ctx.get::<ares_server::Llm>() {
    let reply = llm.complete(ctx, "Summarize this document.").await?;
}

// Store: tenant database handle. Requires the postgres feature.
#[cfg(feature = "postgres")]
if let Some(store) = ctx.get::<ares_server::Store>() {
    let pool = store.pool();
    let _ = pool;
}
```

For tests and proofs, pin an in-process client instead of a network
provider. `Llm::from_client` exists for exactly this
(`crates/ares-llm/src/llm_service.rs`). The echo client below adapts the
crate's own test double:

```rust
use ares_server::{AppError, ConversationMessage, LLMClient, LLMResponse, ToolDefinition};

struct EchoClient;

#[async_trait::async_trait]
impl LLMClient for EchoClient {
    async fn generate(&self, prompt: &str) -> Result<String, AppError> {
        Ok(format!("echo:{prompt}"))
    }

    async fn generate_with_system(&self, _system: &str, prompt: &str) -> Result<String, AppError> {
        self.generate(prompt).await
    }

    async fn generate_with_history(
        &self,
        _messages: &[(String, String)],
    ) -> Result<LLMResponse, AppError> {
        Ok(LLMResponse { content: String::new(), tool_calls: vec![], finish_reason: "stop".into(), usage: None })
    }

    async fn generate_with_tools(
        &self,
        _prompt: &str,
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse, AppError> {
        Ok(LLMResponse { content: String::new(), tool_calls: vec![], finish_reason: "stop".into(), usage: None })
    }

    async fn generate_with_tools_and_history(
        &self,
        _messages: &[ConversationMessage],
        _tools: &[ToolDefinition],
    ) -> Result<LLMResponse, AppError> {
        Ok(LLMResponse { content: String::new(), tool_calls: vec![], finish_reason: "stop".into(), usage: None })
    }

    async fn stream(
        &self,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String, AppError>> + Send + Unpin>, AppError> {
        Err(AppError::Internal("echo stream not implemented".into()))
    }

    async fn stream_with_system(
        &self,
        _system: &str,
        _prompt: &str,
    ) -> Result<Box<dyn futures::Stream<Item = Result<String, AppError>> + Send + Unpin>, AppError> {
        Err(AppError::Internal("echo stream not implemented".into()))
    }

    async fn stream_with_history(
        &self,
        _messages: &[(String, String)],
    ) -> Result<Box<dyn futures::Stream<Item = Result<String, AppError>> + Send + Unpin>, AppError> {
        Err(AppError::Internal("echo stream not implemented".into()))
    }

    fn model_name(&self) -> &str {
        "echo"
    }
}

let llm = ares_server::Llm::from_client(std::sync::Arc::new(EchoClient));
let reply = llm.complete(&ctx, "hi").await?;
assert_eq!(reply, "echo:hi");
```

## Register a custom plugin

A plugin is a typed factory. It declares a config type and the service it
provides; the kernel calls `apply` once, inserts the value, and tracks the
fiber for lifecycle and hot reload. The trait lives in
`crates/cordis/src/registry.rs`; the shape below follows `PipelinePlugin`
(`crates/ares-agent/src/pipeline.rs`) and the kernel's own registry tests:

```rust
use std::sync::Arc;

use ares_server::{Context, Plugin, Service};
use cordis::{CordisError, RegistryService};

pub struct GreetingService {
    prefix: String,
}

impl GreetingService {
    pub fn greet(&self, name: &str) -> String {
        format!("{}{name}", self.prefix)
    }
}

impl Service for GreetingService {}

#[derive(Default, serde::Serialize, serde::Deserialize)]
pub struct GreetingConfig {
    pub prefix: String,
}

pub struct GreetingPlugin;

impl Plugin for GreetingPlugin {
    type Config = GreetingConfig;
    type Provides = GreetingService;

    fn apply(
        &self,
        _ctx: &Arc<Context>,
        config: Self::Config,
    ) -> Result<Arc<Self::Provides>, CordisError> {
        Ok(Arc::new(GreetingService { prefix: config.prefix }))
    }
}
```

Register through the kernel registry and pull the service back:

```rust
let ctx = Context::new_root();
let registry = RegistryService::new();

registry.register(
    &ctx,
    GreetingPlugin,
    GreetingConfig { prefix: "hello, ".to_string() },
)?;

let greeting = ctx.get::<GreetingService>().expect("provided by apply");
assert_eq!(greeting.greet("ares"), "hello, ares");
```

To expose the same plugin to the entries loader, add a string-keyed factory.
The closure-free function form follows the capability-crate factories in
`crates/ares-tools/src/plugins.rs`:

```rust
fn factory_greeting(
    ctx: &Arc<Context>,
    config: &serde_json::Value,
) -> Result<cordis::FiberId, cordis::CordisError> {
    let cfg: GreetingConfig = serde_json::from_value(config.clone())
        .map_err(|e| CordisError::Configuration(e.to_string()))?;
    let svc = GreetingService { prefix: cfg.prefix };
    tokio::task::block_in_place(|| {
        tokio::runtime::Handle::current().block_on(ctx.plugin(svc))
    })
}


// Pull the factory table from the context, then add the key
// after register_plugins(&registry).
let factories = ctx.get::<ares_server::PluginRegistry>().expect("registry");
factories.register("Greeting", std::sync::Arc::new(factory_greeting));
```

An entry can now name it:

```toml
[[entry]]
id = "greeter"
plugin = "Greeting"
disabled = false

[entry.config]
prefix = "hello, "
```

## Loader entries versus pure-library composition

Pick one composition style per process:

| Aspect | Loader entries | Direct provides |
| --------------- | ------------------------------------ | -------------------------- |
| Declaration | `config/cordis-entries.toml` | Rust code |
| Service shape | `[entry]`: `id`, `plugin`, JSON `config`, `disabled`, optional `isolate` and `intercept` | Typed values |
| Hot reload | Yes, through the admin surface and journal | No; rebuild and restart |
| Server boot | Required; the binary exits without a working `Overlay` entry | Not applicable |
| Best fit | Deployment-time wiring, operator edits | Tests, embedded agents, fixed topologies |

The server treats the entries file as the program: it loads the tree, composes
includes, instantiates each enabled entry in file order, and fills empty
configs after the `Overlay` entry lands (`boot_loader_program`,
`src/main.rs`; entry fields in `crates/cordis/src/loader.rs`). A library
embed can reuse that machinery with `Loader::load_from_file` and
`Loader::instantiate_entry`, or skip it and call `ctx.provide(...)` directly.
Direct provides are what every kernel and agent test in the repository does.

## Tenant isolation

Multi-tenant hosts keep shared services on a root context and scope
per-request data into child contexts. Two mechanisms exist:

- **Isolate** labels a service type with a realm name. `get` on the labeled
  child resolves only matching realms through `get_isolated`.
- **Intercept** shadows one service type with a request-scoped value, such
  as a `TenantContext`.

Both come from the kernel context (`crates/cordis/src/context.rs`). The
production helper is `tenant_scope` in `crates/ares-agent/src/execution.rs`;
its behavior is pinned by the test
`execute_isolate_label_wins_over_intercept_for_tools`:

```rust

use ares_server::{Context, TenantContext, TenantTier, Tool, Tools};

// Root context holds shared services.
let root = Context::new_root();

// Scope to one tenant: realm label on Tools, then provide inside the realm.
let scoped = root.isolate::<Tools>("acme");
scoped.provide(Tools::from_static(Vec::<Arc<dyn Tool>>::new()));

// Add request data on top. Isolate wins over intercept for resolution.
let request = scoped.with_intercept(TenantContext::new(
    "acme".to_string(),
    TenantTier::Pro,
));

// The realm's tool set resolves on the request context...
assert!(request.get::<Tools>().is_some());
assert!(request.get_isolated::<Tools>("acme").is_some());

// ...and stays invisible under a different label.
assert!(request.get_isolated::<Tools>("other").is_none());
```

Rules the tests enforce:

- An isolate label beats a `TenantContext` intercept during agent resolution
  (`user_id_from_ctx_isolate_label_wins_over_intercept`, resolver tests).
- `Execute` stays shared. It is a stateless engine; isolating it hid the root
  instance and broke request paths, so `tenant_scope` isolates `Tools` only.
- Background jobs scope with isolate alone; HTTP requests add the intercept
  afterward (`request_tenant_ctx`, same module).

## Where to go next

- Kernel concepts: [Ideas and Map](kernel/index.md)
- Interception points around reads, writes, and events:
  [Interception Points](kernel/interception.md)
- The HTTP surface the binary adds: [HTTP API](http-api.md)
