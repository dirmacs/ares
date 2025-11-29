mod agents;
mod api;
mod auth;
mod db;
mod llm;
mod research;
mod types;
mod utils;

use std::sync::Arc;

use crate::{
    auth::jwt::AuthService,
    db::{QdrantClient, TursoClient},
    llm::{LLMClientFactory, Provider},
    utils::config::Config,
};

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
