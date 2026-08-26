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

## End-to-end in one file

This complete program builds a context, registers services, dispatches one
tool call, and runs one agent turn. Every piece is adapted from code that
compiles and passes in this repository:

- The static tool list follows `tools_with_probe()` in
  `crates/ares-agent/src/execution.rs` (`Tools::from_static`).
- The calculator arguments and result shape come from `Calculator` in
  `crates/ares-tools/src/tools/calculator.rs`, re-exported by the facade.
- The event bus provide follows the middleware tests in
  `crates/ares-http/src/middleware/api_key_auth.rs`
  (`ctx.provide(cordis::EventsService::new())`).
- The agent turn uses `Execute::run`; with no `Llm` on the context it takes
  the documented echo fallback in `crates/ares-agent/src/execution.rs`.

```toml
[dependencies]
ares-server = "0.10"
ares-cordis = "0.10"
tokio = { version = "1", features = ["rt-multi-thread", "macros"] }
serde_json = "1"
```

```rust
use std::sync::Arc;

use ares_server::{AgentRequest, Context, Execute, Tool, Tools};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // One context is the whole graph.
    let ctx = Context::new_root();

    // The event bus. Tool calls fan their arguments through a
    // `tools.execute` waterfall when this service is present.
    ctx.provide(cordis::EventsService::new());

    // A tool set with one real tool. Calculator answers
    // {"result": <number>} for basic arithmetic.
    ctx.provide(Tools::from_static(
        [Arc::new(ares_server::Calculator) as Arc<dyn Tool>],
    ));

    let tools = ctx.get::<Tools>().expect("tools on context");
    let out = tools
        .execute(&ctx, "calculator", serde_json::json!({
            "operation": "add", "a": 2, "b": 3
        }))
        .await?;
    println!("calculator -> {}", out["result"]); // calculator -> 5.0

    // One agent turn through the shared engine. No Llm is provided, so
    // run() returns the message itself through the echo fallback path.
    let execute = Execute::new();
    let req = AgentRequest {
        agent_name: "echo".to_string(),
        message: "hello".to_string(),
        ..Default::default()
    };
    let result = execute.run(&req, &ctx).await?;
    println!("agent -> {}", result.response.content); // agent -> hello

    Ok(())
}
```

Run it with `cargo run`. This exact program was compiled and executed against
the workspace; its output:

```console
calculator -> 5.0
agent -> hello
```

Three details keep this working:

- Use a multi-threaded Tokio flavor. Kernel plugin activation calls
  `tokio::task::block_in_place`, which current-thread runtimes reject.
- `Execute` is stateless; construct or provide one instance anywhere. Do
  not isolate it per tenant — isolating it hides the root instance from
  request paths (the regression guard test
  `request_tenant_ctx_keeps_root_execute_resolvable` pins this).
- Add an `Llm::from_client(...)` provider to make the agent turn produce
  real model output instead of the echo fallback.

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

### Lifecycle hooks every service can implement

The `Service` trait (`crates/cordis/src/service.rs`) has three methods with
working defaults. Override only what you need:

| Hook | Default | When the kernel calls it |
| --- | --- | --- |
| `name()` | The Rust type name | Diagnostics and duplicate-provider messages |
| `init(ctx)` | Returns `Ok(None)` | Once per activation, after `apply` produces the value |
| `check()` | Returns `true` | Every time a freshly built instance meets the graph |

`init` returns an optional cleanup handle. The boxed value is any closure
with the `Disposable` shape; the kernel pushes it onto the owning fiber's
undo accumulator. Use it to close connections or cancel background work:

```rust
use cordis::effect::Disposable;
use cordis::ServiceInitFuture;

impl Service for PoolService {
    fn name(&self) -> &'static str { "pool" }

    fn init(&self, _ctx: &Arc<Context>) -> ServiceInitFuture<'_> {
        Box::pin(async move {
            println!("pool online");
            // Returned cleanup runs once when the fiber disposes.
            let guard: Box<dyn Disposable> = Box::new(|| println!("pool closed"));
            Ok(Some(guard))
        })
    }
}
```

`check()` is an availability predicate evaluated at build-and-met points,
such as after `RegistryService::register` runs your factory and before the
value is provided. A `false` verdict is terminal and inspectable: the fiber
rests in state `Failed`, never `Pending` (`crates/cordis/src/service.rs`,
`check` documentation). Services whose availability changes later — circuit
breakers, feature gates — must re-provide or notify instead of relying on
spontaneous re-checks. The kernel holds no downcasting machinery for
per-read checks.

### Dispose ordering

One rule covers every teardown path: effects run in reverse registration
order, last-in first-out (`docs/src/kernel/runtime.md`;
`Fiber::dispose`). Concretely, for a plugin that registered a listener,
then a timer, then returned an `init` cleanup handle:

1. The init cleanup handle runs.
2. The timer cancels.
3. The listener detaches.

The same LIFO pass runs during reactive loss (`Unloading`), during loader
rollback (newest-first across applied steps), and at process shutdown.
Nothing observes a half-torn configuration: by the time an undo starts,
every effect registered after it is already gone. Design `init` cleanups to
depend on nothing registered later than themselves.

## Error handling patterns

Kernel calls return `Result<_, CordisError>` (`crates/cordis/src/service.rs`).
The enum has ten variants. Match the ones you can act on and let the rest
bubble:

```rust
use cordis::{CordisError, ValidationIssue};

fn describe(err: &CordisError) -> String {
    match err {
        // Config failed to deserialize into the plugin's Config type.
        CordisError::Configuration(msg) => format!("bad wiring: {msg}"),
        // Structured pre-flight failures carry placed issues.
        CordisError::Validation(err) => err
            .issues
            .iter()
            .map(|i: &ValidationIssue| format!("{} at {}", i.message, i.path.join(".")))
            .collect::<Vec<_>>()
            .join("; "),
        // Two plugins provide the same service type.
        CordisError::DuplicateProvider { name, owner } => {
            format!("{name} provided twice; second by {owner}")
        }
        // A transition lease timed out after 10 s of contention.
        CordisError::TransitionStuck { fiber, waited_ms } => {
            format!("fiber {fiber} stuck for {waited_ms} ms")
        }
        // Typed property reads that fail to downcast.
        CordisError::PropertyTypeMismatch { name, expected } => {
            format!("property {name} is not a {expected}")
        }
        other => other.message(), // Fiber, ServiceNotFound, InvalidConfig,
                                  // Internal, ReadOnlyProperty
    }
}
```

Guidance per variant, grounded in kernel behavior:

- `ServiceNotFound` means nothing provides the type on this context or its
  parents. Check isolate labels first: `get_isolated::<T>("other-realm")`
  fails even when another realm serves the type.
- `DuplicateProvider` is single-source discipline firing. Drop one factory
  or move it to a realm.
- `InvalidConfig` is stringly; `Validation` is structured. Only `Validation`
  exposes machine-readable issues through `validation_error()`. The loader
  lifts structured issues with `CordisError::validation(...)`, keeping the
  `"invalid config: ..."` Display prefix.
- Apply errors recorded on a fiber are terminal (`Failed`). Retrying the
  same registration never recovers it; re-register with a fresh fiber
  instead (`docs/src/kernel/lifecycle.md`).

At HTTP edges, convert with the existing adapter: `HttpError::from(app_err)`
wraps an `AppError`, and `app_error_into_response` renders the
`{"error", "code"}` body (`crates/ares-http/src/error.rs`).

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

### Choosing a composition style

Answer two questions. Who changes the wiring? How often does it change?

| Situation | Style | Why |
| --- | --- | --- |
| Wiring is fixed at build time; you own all call sites | Direct provides | One less file format; the compiler checks every reference |
| Operators re-wire services between releases without rebuilds | Loader entries | Editing TOML and reloading beats shipping a binary |
| You need admin-surface retire/replace/patch per service | Loader entries | The journal tracks each entry's fiber for lifecycle routes |
| Unit tests and examples | Direct provides | Every kernel and agent test composes this way |
| Product boots from entries; tests exercise one plugin | Hybrid | Both styles feed the same context and coexist |

The hybrid form registers your string-keyed factory beside
`register_plugins`, then names it from an entry. The factory table and the
typed store meet inside the kernel: an entry instantiates through the
factory, and the produced value lands as a typed provider like any direct
provide. Use this when library users should wire your plugin declaratively
while your own tests keep constructing it directly.

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
