//! API client for communicating with ARES server

use crate::state::AppState;
use crate::types::*;
use gloo_net::http::Request;
use leptos::prelude::*;
use leptos::task::spawn_local;

/// Make authenticated API requests
pub async fn fetch_with_auth<T: serde::de::DeserializeOwned>(
    url: &str,
    token: Option<String>,
) -> Result<T, String> {
    let req = if let Some(t) = token {
        Request::get(url)
            .header("Authorization", &format!("Bearer {}", t))
    } else {
        Request::get(url)
    };
    
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;
    
    if !resp.ok() {
        let status = resp.status();
        if let Ok(err) = resp.json::<ApiError>().await {
            return Err(err.error);
        }
        return Err(format!("Request failed with status {}", status));
    }
    
    resp.json::<T>()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))
}

/// POST request with authentication
pub async fn post_with_auth<T, R>(
    url: &str,
    body: &T,
    token: Option<String>,
) -> Result<R, String>
where
    T: serde::Serialize,
    R: serde::de::DeserializeOwned,
{
    let req = Request::post(url)
        .header("Content-Type", "application/json");
    
    let req = if let Some(t) = token {
        req.header("Authorization", &format!("Bearer {}", t))
    } else {
        req
    };
    
    let req = req
        .json(body)
        .map_err(|e| format!("Failed to serialize request: {}", e))?;
    
    let resp = req
        .send()
        .await
        .map_err(|e| format!("Network error: {}", e))?;
    
    if !resp.ok() {
        let status = resp.status();
        if let Ok(err) = resp.json::<ApiError>().await {
            return Err(err.error);
        }
        return Err(format!("Request failed with status {}", status));
    }
    
    resp.json::<R>()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))
}

/// Login to the API
pub async fn login(base_url: &str, email: &str, password: &str) -> Result<AuthResponse, String> {
    let url = format!("{}/api/auth/login", base_url);
    let body = LoginRequest {
        email: email.to_string(),
        password: password.to_string(),
    };
    post_with_auth::<_, AuthResponse>(&url, &body, None).await
}

/// Register a new user
pub async fn register(
    base_url: &str,
    email: &str,
    password: &str,
    name: &str,
) -> Result<AuthResponse, String> {
    let url = format!("{}/api/auth/register", base_url);
    let body = RegisterRequest {
        email: email.to_string(),
        password: password.to_string(),
        name: name.to_string(),
    };
    post_with_auth::<_, AuthResponse>(&url, &body, None).await
}

/// Fetch available agents
pub async fn fetch_agents(base_url: &str) -> Result<Vec<AgentInfo>, String> {
    let url = format!("{}/api/agents", base_url);
    let resp: AgentsListResponse = fetch_with_auth(&url, None).await?;
    Ok(resp.agents)
}

/// Fetch available workflows (requires auth)
pub async fn fetch_workflows(base_url: &str, token: &str) -> Result<Vec<WorkflowInfo>, String> {
    let url = format!("{}/api/workflows", base_url);
    let resp: WorkflowsListResponse = fetch_with_auth(&url, Some(token.to_string())).await?;
    Ok(resp.workflows)
}

/// Send a chat message
pub async fn send_chat(
    base_url: &str,
    token: &str,
    message: &str,
    context_id: Option<String>,
    agent_type: Option<String>,
) -> Result<ChatResponse, String> {
    let url = format!("{}/api/chat", base_url);
    let body = ChatRequest {
        message: message.to_string(),
        context_id,
        agent_type,
    };
    post_with_auth::<_, ChatResponse>(&url, &body, Some(token.to_string())).await
}

/// Fetch user memory
pub async fn fetch_memory(base_url: &str, token: &str) -> Result<UserMemory, String> {
    let url = format!("{}/api/memory", base_url);
    fetch_with_auth(&url, Some(token.to_string())).await
}

/// Load agents into app state
pub fn load_agents(state: AppState) {
    spawn_local(async move {
        let base = state.api_base.get_untracked();
        match fetch_agents(&base).await {
            Ok(agents) => state.agents.set(agents),
            Err(e) => tracing::error!("Failed to load agents: {}", e),
        }
    });
}

/// Load workflows into app state (requires auth)
pub fn load_workflows(state: AppState) {
    spawn_local(async move {
        let base = state.api_base.get_untracked();
        if let Some(token) = state.token.get_untracked() {
            match fetch_workflows(&base, &token).await {
                Ok(workflows) => state.workflows.set(workflows),
                Err(e) => tracing::error!("Failed to load workflows: {}", e),
            }
        }
    });
}
