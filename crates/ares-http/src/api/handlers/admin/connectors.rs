//! Admin connectors domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;

use ares_store::agent_runs;
use ares_store::audit_log;
use ares_store::skills as db_skills;
use ares_types::types::{AppError};
use crate::Result;
use crate::HttpError;
use axum::{
    Json,
    extract::{Path, Query, State},
    http::{HeaderMap, StatusCode},
    response::Redirect,
};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Arc;
use ::cordis::Context;

pub async fn list_skills(
    State(ctx): State<Arc<Context>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_skills::Skill>>> {
    let tenant_id = required_skill_tenant_id(&params)?.to_string();
    let __pool_1 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::SkillStore::new(&__pool_1);
    let skills = store.list_skills(Some(&tenant_id)).await?;
    Ok(Json(skills))
}

pub async fn get_skill(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<db_skills::Skill>> {
    let tenant_id = required_skill_tenant_id(&params)?.to_string();
    let __pool_2 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::SkillStore::new(&__pool_2);
    let skill = store
        .get_skill_for_tenant(&id, &tenant_id)
        .await?
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("skill {id} not found"))))?;
    Ok(Json(skill))
}

pub async fn create_skill(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<db_skills::CreateSkillRequest>,
) -> Result<Json<db_skills::Skill>> {
    let __pool_3 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::SkillStore::new(&__pool_3);
    let skill = store.create_skill(&req).await?;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let t_id = skill.tenant_id.clone();
    let s_name = skill.name.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "skill_create", "skill", &s_name, Some(&t_id), None)
                .await;
    });

    Ok(Json(skill))
}

pub async fn update_skill(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Json(req): Json<db_skills::CreateSkillRequest>,
) -> Result<Json<db_skills::Skill>> {
    normalized_skill_tenant_id(&req.tenant_id)?;
    let __pool_4 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::SkillStore::new(&__pool_4);
    let skill = store
        .update_skill(&id, &req)
        .await?
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("skill {id} not found"))))?;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let t_id = skill.tenant_id.clone();
    let s_name = skill.name.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "skill_update", "skill", &s_name, Some(&t_id), None)
                .await;
    });

    Ok(Json(skill))
}

pub async fn delete_skill(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<StatusCode> {
    let tenant_id = required_skill_tenant_id(&params)?.to_string();
    let __pool_5 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::SkillStore::new(&__pool_5);
    let rows = store.delete_skill_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(HttpError::from(AppError::NotFound(format!("skill {id} not found"))));
    }

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let sid = id.clone();
    let t_id = tenant_id.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "skill_delete", "skill", &sid, Some(&t_id), None)
                .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn run_skill(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<RunSkillRequest>,
) -> Result<Json<serde_json::Value>> {
    let run_id = uuid::Uuid::new_v4().to_string();
    let tenant_id = normalized_run_skill_tenant_id(&req.tenant_id)?.to_string();
    let skill_id = req.skill_id;
    let agent_name = admin_skill_agent_name(&skill_id);
    ctx.get::<crate::active_runs::ActiveRuns>().expect("not provided")
        .start(admin_skill_active_run(&run_id, &tenant_id, &skill_id));

    let metadata = admin_skill_run_metadata(&run_id);
    let __pool_6 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    agent_runs::insert_agent_run_with_id_and_metadata(&__pool_6,
        &run_id,
        &tenant_id,
        &agent_name,
        None,
        "running",
        0,
        0,
        0,
        None,
        "skill",
        "skill",
        false,
        Some(&metadata),
    )
    .await?;

    let __pool_7 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let obs = Arc::new(crate::observability::RunObservability {
        run_id: run_id.clone(),
        tenant_id: tenant_id.clone(),
        agent_name: agent_name.clone(),
        pool: __pool_7,
    });
    let start = std::time::Instant::now();
    let result = ctx.get::<ares_agent::skills::SkillEngine>().expect("not provided")
        .execute_skill(&skill_id, &tenant_id, req.input, &run_id)
        .await;
    let duration_ms = start.elapsed().as_millis() as i64;
    let status = if result.is_ok() {
        "completed"
    } else {
        "failed"
    };
    let active_status = if result.is_ok() { "completed" } else { "error" };
    ctx.get::<crate::active_runs::ActiveRuns>().expect("not provided").finish(&run_id, active_status);

    let (input_tokens, output_tokens) = result
        .as_ref()
        .map(ares_agent::skills::skill_result_token_counts)
        .unwrap_or((0, 0));
    let error_message = result.as_ref().err().cloned();

    let __pool_8 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    sqlx::query(
        "UPDATE agent_runs
         SET status = $2, input_tokens = $3, output_tokens = $4,
             duration_ms = $5, error = $6
         WHERE id = $1",
    )
    .bind(&run_id)
    .bind(status)
    .bind(input_tokens)
    .bind(output_tokens)
    .bind(duration_ms)
    .bind(error_message.as_deref())
    .execute(&__pool_8)
    .await
    .map_err(|e| AppError::Database(e.to_string()))?;

    let obs_for_spawn = obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(duration_ms).await;
    });

    match result {
        Ok(result) => Ok(Json(result)),
        Err(e) => Err(HttpError::from(AppError::InvalidInput(e))),
    }
}

pub async fn list_connectors(
    State(ctx): State<Arc<Context>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<db_skills::Connector>>> {
    let tenant_id = params.get("tenant_id").map(|s| s.as_str()).unwrap_or("");
    if tenant_id.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "tenant_id query param is required".into(),
        )));
    }
    let __pool_9 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::ConnectorStore::new(&__pool_9);
    let connectors = store.list_connectors(tenant_id).await?;
    Ok(Json(connectors))
}

pub async fn create_connector(
    State(ctx): State<Arc<Context>>,
    Json(req): Json<db_skills::CreateConnectorRequest>,
) -> Result<Json<db_skills::Connector>> {
    let __pool_10 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::ConnectorStore::new(&__pool_10);
    let connector = store.create_connector(&req).await?;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let t_id = connector.tenant_id.clone();
    let c_name = connector.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "connector_create",
            "connector",
            &c_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(connector))
}

pub async fn update_connector(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    Json(req): Json<db_skills::CreateConnectorRequest>,
) -> Result<Json<db_skills::Connector>> {
    let __pool_11 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::ConnectorStore::new(&__pool_11);
    let connector = store
        .update_connector(&id, &req)
        .await?
        .ok_or_else(|| HttpError::from(AppError::NotFound(format!("connector {id} not found"))))?;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let t_id = connector.tenant_id.clone();
    let c_name = connector.name.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "connector_update",
            "connector",
            &c_name,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(connector))
}

pub async fn delete_connector(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
) -> Result<StatusCode> {
    let __pool_12 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::ConnectorStore::new(&__pool_12);
    let rows = store.delete_connector(&id).await?;
    if rows == 0 {
        return Err(HttpError::from(AppError::NotFound(format!("connector {id} not found"))));
    }

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let cid = id.clone();
    tokio::spawn(async move {
        let _ =
            audit_log::log_admin_action(&pool, "connector_delete", "connector", &cid, None, None)
                .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn delete_tenant_connector(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_13 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_skills::ConnectorStore::new(&__pool_13);
    let rows = store.delete_connector_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(HttpError::from(AppError::NotFound(format!(
            "connector {id} not found for tenant {tenant_id}"
        ))));
    }

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let cid = id.clone();
    let t_id = tenant_id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "connector_delete",
            "connector",
            &cid,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub async fn oauth_authorize(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Query(query): Query<OAuthAuthorizeQuery>,
) -> Result<Redirect> {
    if query.tenant_id.trim().is_empty() || query.connector_type.trim().is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "tenant_id and connector_type are required".into(),
        )));
    }

    let provider = oauth_provider_config(&query.connector_type)?;
    let __pool_14 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = ares_store::oauth_credentials::OAuthCredentialStore::new(&__pool_14);
    let credential = store
        .get(&query.tenant_id, provider.provider, &query.connector_type)
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "OAuth credential for tenant {} connector {} not found",
                query.tenant_id, query.connector_type
            ))
        })?;

    let oauth_state = OAuthState {
        tenant_id: query.tenant_id,
        connector_type: query.connector_type,
        redirect_uri: safe_callback_redirect_uri(&query.redirect_uri),
    };
    let auth_url = build_authorize_url(
        provider,
        &credential.client_id,
        &oauth_callback_url(&headers),
        &oauth_state,
    )?;

    Ok(Redirect::temporary(&auth_url))
}

pub async fn oauth_callback(
    State(ctx): State<Arc<Context>>,
    headers: HeaderMap,
    Query(query): Query<OAuthCallbackQuery>,
) -> Result<Redirect> {
    if query.code.trim().is_empty() {
        return Err(HttpError::from(AppError::InvalidInput("OAuth code is required".to_string())));
    }

    let oauth_state = decode_oauth_state(&query.state)?;
    let provider = oauth_provider_config(&oauth_state.connector_type)?;
    let __pool_15 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = ares_store::oauth_credentials::OAuthCredentialStore::new(&__pool_15);
    let credential = store
        .get(
            &oauth_state.tenant_id,
            provider.provider,
            &oauth_state.connector_type,
        )
        .await?
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "OAuth credential for tenant {} connector {} not found",
                oauth_state.tenant_id, oauth_state.connector_type
            ))
        })?;

    let master = MasterKey::from_env()
        .ok_or_else(|| HttpError::from(AppError::Configuration("FLEET_SECRETS_KEY not set".into())))?;
    let client_secret = decrypt_api_key(&credential.client_secret, &master)
        .map_err(|e| AppError::Configuration(format!("decrypt failed: {e}")))?;
    let callback_url = oauth_callback_url(&headers);
    let form = build_token_form(
        &query.code,
        &credential.client_id,
        &client_secret,
        &callback_url,
    );

    let response = reqwest::Client::new()
        .post(provider.token_url)
        .form(&form)
        .send()
        .await
        .map_err(|e| AppError::External(format!("OAuth token request failed: {e}")))?;

    if !response.status().is_success() {
        let status = response.status();
        let body = response.text().await.unwrap_or_default();
        return Err(HttpError::from(AppError::External(format!(
            "OAuth token exchange failed with {status}: {body}"
        ))));
    }

    let token: OAuthTokenResponse = response
        .json()
        .await
        .map_err(|e| AppError::External(format!("OAuth token response parse failed: {e}")))?;
    let expires_at = chrono::Utc::now().timestamp() + token.expires_in.unwrap_or(3600).max(0);
    let stored_scope = oauth_stored_scope(
        provider.scope,
        token.scope.as_deref(),
        token.instance_url.as_deref(),
    );
    store
        .update_tokens_and_scope(
            &credential.id,
            &token.access_token,
            token.refresh_token.as_deref(),
            expires_at,
            Some(&stored_scope),
        )
        .await?;

    Ok(Redirect::temporary(&oauth_state.redirect_uri))
}

pub async fn list_oauth_credentials(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<OAuthCredentialResponse>>> {
    let __pool_16 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = ares_store::oauth_credentials::OAuthCredentialStore::new(&__pool_16);
    let credentials = store
        .list_by_tenant(&tenant_id)
        .await?
        .into_iter()
        .map(OAuthCredentialResponse::from)
        .collect();
    Ok(Json(credentials))
}

pub async fn create_oauth_credential(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    Json(mut req): Json<ares_store::oauth_credentials::CreateOAuthCredentialRequest>,
) -> Result<Json<OAuthCredentialResponse>> {
    normalize_oauth_credential_request(tenant_id, &mut req)?;
    let __pool_17 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = ares_store::oauth_credentials::OAuthCredentialStore::new(&__pool_17);
    let credential = store.create(&req).await?;

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let cred_id = credential.id.clone();
    let t_id = credential.tenant_id.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "oauth_credential_create",
            "oauth_credential",
            &cred_id,
            Some(&t_id),
            None,
        )
        .await;
    });

    Ok(Json(credential.into()))
}

pub async fn delete_oauth_credential(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, id)): Path<(String, String)>,
) -> Result<StatusCode> {
    let __pool_18 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = ares_store::oauth_credentials::OAuthCredentialStore::new(&__pool_18);
    let rows = store.delete_for_tenant(&tenant_id, &id).await?;
    if rows == 0 {
        return Err(HttpError::from(AppError::NotFound(format!(
            "oauth credential {id} not found for tenant {tenant_id}"
        ))));
    }

    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &pool,
            "oauth_credential_delete",
            "oauth_credential",
            &id,
            Some(&tenant_id),
            None,
        )
        .await;
    });

    Ok(StatusCode::NO_CONTENT)
}

pub fn routes() -> axum::Router<Arc<Context>> {
    use axum::routing::{delete, get, post, put};
    axum::Router::new()
        .route("/connectors/list_skills", get(list_skills))
        .route("/connectors/get_skill", get(get_skill))
        .route("/connectors/create_skill", post(create_skill))
        .route("/connectors/update_skill", put(update_skill))
        .route("/connectors/delete_skill", delete(delete_skill))
        .route("/connectors/run_skill", post(run_skill))
        .route("/connectors/list_connectors", get(list_connectors))
        .route("/connectors/create_connector", post(create_connector))
        .route("/connectors/update_connector", put(update_connector))
        .route("/connectors/delete_connector", delete(delete_connector))
        .route("/connectors/delete_tenant_connector", delete(delete_tenant_connector))
        .route("/connectors/oauth_authorize", post(oauth_authorize))
        .route("/connectors/oauth_callback", post(oauth_callback))
        .route("/connectors/list_oauth_credentials", get(list_oauth_credentials))
        .route("/connectors/create_oauth_credential", post(create_oauth_credential))
        .route("/connectors/delete_oauth_credential", delete(delete_oauth_credential))
}

// cordis Phase6: RouteSet Service — registered via build_routes(ctx)
use ::cordis::Service;
