mod agents;
mod api;
mod auth;
mod db;
mod llm;
mod mcp;
mod memory;
mod rag;
mod research;
mod tools;
mod types;
mod utils;

use crate::{
    auth::jwt::AuthService,
    db::{QdrantClient, TursoClient},
    llm::{LLMClientFactory, Provider},
    utils::config::Config,
};
use axum::{
    Router,
    routing::{get, post},
};
use std::sync::Arc;
use tower_http::{
    cors::{Any, CorsLayer},
    trace::TraceLayer,
};
use tracing_subscriber::{layer::SubscriberExt, util::SubscriberInitExt};
use utoipa::OpenApi;
use utoipa_swagger_ui::SwaggerUi;

#[derive(Clone)]
pub struct AppState {
    pub config: Arc<Config>,
    pub turso: Arc<TursoClient>,
    pub qdrant: Arc<QdrantClient>,
    pub llm_factory: Arc<LLMClientFactory>,
    pub auth_service: Arc<AuthService>,
}

fn main() {
    println!("Hello, world!");
}
