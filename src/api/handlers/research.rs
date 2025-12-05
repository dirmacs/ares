use crate::{
    AppState,
    auth::middleware::AuthUser,
    research::coordinator::ResearchCoordinator,
    types::{ResearchRequest, ResearchResponse, Result},
};
use axum::{Json, extract::State};
use std::time::Instant;

/// Perform deep research on a query
#[utoipa::path(
    post,
    path = "/api/research",
    request_body = ResearchRequest,
    responses(
        (status = 200, description = "Research completed", body = ResearchResponse),
        (status = 400, description = "Invalid input"),
        (status = 401, description = "Unauthorized")
    ),
    tag = "research",
    security(("bearer" = []))
)]
pub async fn deep_research(
    State(state): State<AppState>,
    AuthUser(_claims): AuthUser,
    Json(payload): Json<ResearchRequest>,
) -> Result<Json<ResearchResponse>> {
    let start = Instant::now();

    let depth = payload.depth.unwrap_or(2);
    let max_iterations = payload.max_iterations.unwrap_or(5);

    // Create research coordinator
    let llm_client = state.llm_factory.create_default().await?;
    let coordinator = ResearchCoordinator::new(llm_client, depth, max_iterations);

    // Execute research
    let (findings, sources) = coordinator.research(&payload.query).await?;

    let duration = start.elapsed();

    Ok(Json(ResearchResponse {
        findings,
        sources,
        duration_ms: duration.as_millis() as u64,
    }))
}
