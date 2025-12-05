use rmcp::{
    ErrorData as McpError, ServerHandler,
    handler::server::{tool::ToolRouter, wrapper::Parameters},
    model::*,
    tool, tool_router,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Parameters for web search
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SearchParams {
    /// The search query
    pub query: String,
    /// Maximum number of results to return
    pub limit: Option<usize>,
}

/// Parameters for fetching a web page
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct FetchPageParams {
    /// URL of the page to fetch
    pub url: String,
}

/// Parameters for calculator
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct CalculateParams {
    /// The operation to perform: add, subtract, multiply, divide
    pub operation: String,
    /// First operand
    pub a: f64,
    /// Second operand
    pub b: f64,
}

/// MCP Server for ARES - provides tools for AI assistants
#[derive(Clone)]
pub struct AresMcpServer {
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

#[tool_router]
impl AresMcpServer {
    pub fn new() -> Self {
        Self {
            tool_router: Self::tool_router(),
        }
    }

    /// Search the web for information on a given query
    #[tool(description = "Search the web for information on a given query")]
    async fn search(&self, params: Parameters<SearchParams>) -> Result<CallToolResult, McpError> {
        let limit = params.0.limit.unwrap_or(10);
        let query = params.0.query;

        // Use daedra for web search
        let search_args = daedra::SearchArgs {
            query: query.clone(),
            options: Some(daedra::SearchOptions {
                num_results: limit,
                ..Default::default()
            }),
        };

        match daedra::tools::search::perform_search(&search_args).await {
            Ok(response) => {
                // SearchResponse has `data: Vec<SearchResult>`, not `results`
                let results: Vec<String> = response
                    .data
                    .iter()
                    .map(|r| format!("**{}**\n{}\nURL: {}", r.title, r.description, r.url))
                    .collect();

                let content = if results.is_empty() {
                    format!("No results found for: {}", query)
                } else {
                    results.join("\n\n---\n\n")
                };

                Ok(CallToolResult::success(vec![Content::text(content)]))
            }
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Search failed: {}",
                e
            ))])),
        }
    }

    /// Fetch a web page and convert it to markdown
    #[tool(description = "Fetch a web page and convert it to markdown")]
    async fn fetch_page(
        &self,
        params: Parameters<FetchPageParams>,
    ) -> Result<CallToolResult, McpError> {
        let url = params.0.url;

        let args = daedra::VisitPageArgs {
            url: url.clone(),
            include_images: false,
            selector: None,
        };

        match daedra::tools::fetch::fetch_page(&args).await {
            // PageContent has a `content` field that is a String
            Ok(page_content) => Ok(CallToolResult::success(vec![Content::text(
                page_content.content,
            )])),
            Err(e) => Ok(CallToolResult::error(vec![Content::text(format!(
                "Failed to fetch {}: {}",
                url, e
            ))])),
        }
    }

    /// Perform basic arithmetic calculations
    #[tool(description = "Perform basic arithmetic calculations")]
    async fn calculate(
        &self,
        params: Parameters<CalculateParams>,
    ) -> Result<CallToolResult, McpError> {
        let operation = params.0.operation;
        let a = params.0.a;
        let b = params.0.b;

        let result = match operation.to_lowercase().as_str() {
            "add" => a + b,
            "subtract" => a - b,
            "multiply" => a * b,
            "divide" => {
                if b == 0.0 {
                    return Ok(CallToolResult::error(vec![Content::text(
                        "Cannot divide by zero",
                    )]));
                }
                a / b
            }
            _ => {
                return Ok(CallToolResult::error(vec![Content::text(format!(
                    "Unknown operation: {}. Supported: add, subtract, multiply, divide",
                    operation
                ))]));
            }
        };

        Ok(CallToolResult::success(vec![Content::text(format!(
            "{} {} {} = {}",
            a, operation, b, result
        ))]))
    }
}

impl Default for AresMcpServer {
    fn default() -> Self {
        Self::new()
    }
}

impl ServerHandler for AresMcpServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo {
            protocol_version: ProtocolVersion::default(),
            capabilities: ServerCapabilities::default(),
            server_info: Implementation {
                name: "ares-mcp-server".into(),
                version: env!("CARGO_PKG_VERSION").into(),
                title: None,
                icons: None,
                website_url: None,
            },
            instructions: Some(
                "ARES MCP Server - provides web search, page fetching, and calculation tools"
                    .into(),
            ),
        }
    }
}

/// Start the MCP server with stdio transport
pub async fn start_stdio_server() -> crate::types::Result<()> {
    use rmcp::{ServiceExt, transport::io::stdio};

    let server = AresMcpServer::new();
    let transport = stdio();

    server
        .serve(transport)
        .await
        .map_err(|e| crate::types::AppError::Internal(format!("MCP server error: {}", e)))?;

    Ok(())
}
