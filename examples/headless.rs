//! Headless embedding proof: everything below is reached through the
//! `ares_server` facade alone — no capability-crate dependencies in the
//! consumer manifest, no HTTP stack in the graph.
//!
//! Run with:
//!
//! ```text
//! cargo run -p ares-server --no-default-features --example headless
//! ```

use std::sync::Arc;

use ares_server::{AgentRequest, Context, Execute, EventsService, Tool, Tools};

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Root context: the event bus plus a static tool set, nothing else.
    let ctx = Context::new_root();
    ctx.provide(EventsService::new());
    ctx.provide(Tools::from_static([Arc::new(ares_server::Calculator) as Arc<dyn Tool>]));

    // Tool call straight off the context.
    let tools = ctx.get::<Tools>().expect("tools on context");
    let out = tools
        .execute(
            &ctx,
            "calculator",
            serde_json::json!({"operation": "add", "a": 2, "b": 3}),
        )
        .await?;
    println!("calculator -> {}", out["result"]);

    // Agent run through the unified Execute entry point. With no agent
    // registry configured, the built-in echo fallback answers.
    let execute = Execute::new();
    let req = AgentRequest {
        agent_name: "echo".to_string(),
        message: "hello".to_string(),
        ..Default::default()
    };
    let result = execute.run(&req, &ctx).await?;
    println!("agent -> {}", result.response.content);

    Ok(())
}
