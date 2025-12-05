#[cfg(feature = "mcp")]
use rmcp::{ServerBuilder, tool};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize, Serialize, JsonSchema)]
struct SearchArgs {
    query: String,
    limit: Option<usize>,
}

#[cfg(feature = "mcp")]
pub struct McpServer;

#[cfg(feature = "mcp")]
impl McpServer {
    pub async fn start() -> crate::types::Result<()> {
        let server = ServerBuilder::new()
            .name("chatbot-mcp-server")
            .version("1.0.0")
            .build()
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;

        // Run with stdio
        server
            .run_stdio()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;

        Ok(())
    }
}
