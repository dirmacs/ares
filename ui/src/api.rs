//! API client for communicating with ARES server

use crate::state::AppState;
use crate::types::*;
use gloo_net::http::Request;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::prelude::*;
use wasm_bindgen_futures::JsFuture;
use js_sys::{Reflect, Uint8Array};
use web_sys::{
    Headers, ReadableStreamDefaultReader, Request as WebRequest, RequestInit, RequestMode, Response,
};

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
    // Backend returns array directly, not wrapped
    fetch_with_auth(&url, None).await
}

/// Fetch available workflows (requires auth)
pub async fn fetch_workflows(base_url: &str, token: &str) -> Result<Vec<WorkflowInfo>, String> {
    let url = format!("{}/api/workflows", base_url);
    // Backend returns array directly, not wrapped
    fetch_with_auth(&url, Some(token.to_string())).await
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

/// Stream chat response using fetch + ReadableStream
/// This allows POST-based SSE streaming in WASM
/// 
/// The callback is called for each streaming event:
/// - "start" - streaming has begun, agent field contains the assigned agent
/// - "token" - content field contains a token to append
/// - "done" - streaming complete, context_id contains the conversation ID
/// - "error" - error field contains the error message
pub async fn stream_chat<F>(
    base_url: &str,
    token: &str,
    message: &str,
    context_id: Option<String>,
    agent_type: Option<String>,
    mut on_event: F,
) -> Result<(), String>
where
    F: FnMut(StreamEvent) + 'static,
{
    let url = format!("{}/api/chat/stream", base_url);
    
    // Build request body
    let body = ChatRequest {
        message: message.to_string(),
        context_id,
        agent_type,
    };
    let body_json = serde_json::to_string(&body)
        .map_err(|e| format!("Failed to serialize request: {}", e))?;
    
    // Create headers
    let headers = Headers::new()
        .map_err(|e| format!("Failed to create headers: {:?}", e))?;
    headers.set("Content-Type", "application/json")
        .map_err(|e| format!("Failed to set content-type: {:?}", e))?;
    headers.set("Authorization", &format!("Bearer {}", token))
        .map_err(|e| format!("Failed to set auth header: {:?}", e))?;
    
    // Create request init
    let opts = RequestInit::new();
    opts.set_method("POST");
    opts.set_headers(&headers);
    opts.set_body(&JsValue::from_str(&body_json));
    opts.set_mode(RequestMode::Cors);
    
    // Create and send request
    let request = WebRequest::new_with_str_and_init(&url, &opts)
        .map_err(|e| format!("Failed to create request: {:?}", e))?;
    
    let window = web_sys::window().ok_or("No window object")?;
    let resp_promise = window.fetch_with_request(&request);
    let resp: Response = JsFuture::from(resp_promise)
        .await
        .map_err(|e| format!("Fetch failed: {:?}", e))?
        .dyn_into()
        .map_err(|_| "Response is not a Response object")?;
    
    if !resp.ok() {
        let status = resp.status();
        return Err(format!("Request failed with status {}", status));
    }
    
    // Get the readable stream body
    let body = resp.body().ok_or("Response has no body")?;
    
    // Get a reader from the stream
    let reader_obj = body.get_reader();
    let reader: ReadableStreamDefaultReader = reader_obj
        .dyn_into()
        .map_err(|_| "Failed to get ReadableStreamDefaultReader")?;
    
    // Buffer for incomplete lines
    let mut buffer = String::new();
    let text_decoder = web_sys::TextDecoder::new()
        .map_err(|e| format!("Failed to create TextDecoder: {:?}", e))?;
    
    // Read loop
    loop {
        let read_promise = reader.read();
        let result = JsFuture::from(read_promise)
            .await
            .map_err(|e| format!("Read failed: {:?}", e))?;
        
        // Check if done
        let done = Reflect::get(&result, &JsValue::from_str("done"))
            .map_err(|_| "Failed to get done property")?
            .as_bool()
            .unwrap_or(true);
        
        if done {
            break;
        }
        
        // Get the value (Uint8Array)
        let value = Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|_| "Failed to get value property")?;
        
        if value.is_undefined() {
            continue;
        }
        
        let chunk: Uint8Array = value
            .dyn_into()
            .map_err(|_| "Value is not a Uint8Array")?;
        
        // Decode the chunk to string
        let decoded = text_decoder.decode_with_buffer_source(&chunk)
            .map_err(|e| format!("Failed to decode chunk: {:?}", e))?;
        
        buffer.push_str(&decoded);
        
        // Process complete lines (SSE format: "data: {...}\n\n")
        while let Some(pos) = buffer.find("\n\n") {
            let line = buffer[..pos].to_string();
            buffer = buffer[pos + 2..].to_string();
            
            // Parse SSE line - it starts with "data: "
            if let Some(data) = line.strip_prefix("data: ") {
                // Skip keep-alive messages
                if data.trim() == "keep-alive" {
                    continue;
                }
                
                // Parse the JSON event
                if let Ok(event) = serde_json::from_str::<StreamEvent>(data) {
                    on_event(event);
                } else {
                    tracing::warn!("Failed to parse SSE event: {}", data);
                }
            }
        }
    }
    
    Ok(())
}
