//! Admin agents domain — cordis Phase6
//! Bodies moved from `admin.rs` (190KB/5946 lines).

use super::*;

use crate::HttpError;
use crate::Result;
use ::cordis::Context;
use ares_agent::context_provider::AgentRuntimeContext;
use ares_agent::memory::estimate_tokens;
use ares_agent::tenant_agent;
use ares_store::agent_runs;
use ares_store::agent_versions;
use ares_store::audit_log;
use ares_store::tenant_agents::{
    create_tenant_agent_as, deep_merge_config, delete_tenant_agent as db_delete_tenant_agent,
    get_tenant_agent as db_get_tenant_agent, get_tenant_agent_publish_state, list_agent_templates,
    list_tenant_agent_versions, list_tenant_agents as db_list_tenant_agents, publish_tenant_agent,
    rollback_tenant_agent_version, update_tenant_agent_as, AgentTemplate, AgentTemplateStore,
    CreateTemplateRequest, CreateTenantAgentRequest, PublishOutcome, PublishRefusal,
    PublishTenantAgentRequest, TenantAgent, TenantAgentPublishState, UpdateTenantAgentRequest,
};
use ares_types::types::{AgentContext, AppError};
use axum::{
    extract::{Path, Query, State},
    http::StatusCode,
    Json,
};
use sha2::Digest;
use std::collections::HashMap;
use std::sync::Arc;

pub async fn list_tenant_agents_handler(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
) -> Result<Json<Vec<TenantAgent>>> {
    let __pool_1 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let agents = db_list_tenant_agents(&__pool_1, &tenant_id).await?;
    Ok(Json(agents))
}

/// The actor a config draft, a publish or a rollback is attributed to
/// (item 2.6a): the JWT subject, or `admin_secret` for the static key, which
/// counts as one actor (D-5). A request with no admin identity cannot write,
/// approve or roll back an agent config: the two-person rule needs to know
/// who it is.
fn config_actor(actor: &AdminActor) -> Result<&str> {
    actor
        .audit_actor()
        .filter(|id| !id.trim().is_empty())
        .ok_or_else(|| {
            HttpError::from(AppError::Auth(
                "An identified admin is required to write, publish or roll back an agent config"
                    .to_string(),
            ))
        })
}

/// `POST /admin/tenants/{tenant_id}/agents`: the new agent's config is a
/// draft by this admin (item 2.6a, D-4). It runs once another admin
/// publishes it; until then the row's `config` is the empty object.
pub async fn create_tenant_agent_handler(
    State(ctx): State<Arc<Context>>,
    Path(tenant_id): Path<String>,
    actor: AdminActor,
    Json(req): Json<CreateTenantAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let author = config_actor(&actor)?;
    let tools = ctx.get::<ares_tools::Tools>().expect("Tools not provided");
    validate_agent_config_tools(&req.config, tools.as_ref(), &ctx, &tenant_id)?;
    validate_agent_config_delegations(&req.config, &req.agent_name)?;

    let __pool_2 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let agent = create_tenant_agent_as(&__pool_2, &tenant_id, req, Some(author)).await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let aid = agent.id.clone();
    audit_log::record(
        &pool,
        "create_agent",
        "agent",
        &aid,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(agent))
}

/// Splits an incoming config patch into the merged config (for validation)
/// and the forwarded patch (for the store update). Cleared keys (explicit
/// nulls) are re-attached as nulls on the forwarded patch so the store-side
/// re-merge preserves the clear instead of resurrecting the key via
/// absent-keeps.
fn split_merged_config_patch(
    current: &serde_json::Value,
    patch: serde_json::Value,
) -> (serde_json::Value, serde_json::Value) {
    let merged = deep_merge_config(current, &patch);
    let mut forward = merged.clone();
    if let (Some(cur_obj), Some(merged_obj)) = (current.as_object(), merged.as_object()) {
        if let Some(fwd_obj) = forward.as_object_mut() {
            for key in cur_obj.keys() {
                if !merged_obj.contains_key(key) {
                    fwd_obj.insert(key.clone(), serde_json::Value::Null);
                }
            }
        }
    }
    (merged, forward)
}

/// `PUT /admin/tenants/{tenant_id}/agents/{agent_name}`: a config patch is
/// merged into the draft and never touches what runs (item 2.6a, D-1);
/// display name, description and `enabled` apply directly.
pub async fn update_tenant_agent_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    actor: AdminActor,
    Json(mut req): Json<UpdateTenantAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let __pool_3 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    if let Some(patch) = req.config.take() {
        config_actor(&actor)?;
        let current = get_tenant_agent_publish_state(&__pool_3, &tenant_id, &agent_name).await?;
        let (merged, forward) = split_merged_config_patch(current.draft_base(), patch);
        let tools = ctx.get::<ares_tools::Tools>().expect("Tools not provided");
        validate_agent_config_tools(&merged, tools.as_ref(), &ctx, &tenant_id)?;
        validate_agent_config_delegations(&merged, &agent_name)?;
        req.config = Some(forward);
    }

    let agent =
        update_tenant_agent_as(&__pool_3, &tenant_id, &agent_name, req, actor.audit_actor())
            .await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let aid = agent.id.clone();
    audit_log::record(
        &pool,
        "update_agent",
        "agent",
        &aid,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(agent))
}

pub async fn delete_tenant_agent_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    actor: AdminActor,
) -> Result<StatusCode> {
    let __pool_4 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    db_delete_tenant_agent(&__pool_4, &tenant_id, &agent_name).await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let resource_id = format!("{}:{}", tenant_id, agent_name);
    audit_log::record(
        &pool,
        "delete_agent",
        "agent",
        &resource_id,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn list_tenant_agent_versions_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<Vec<agent_versions::AgentVersionRecord>>> {
    let __pool_5 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    // GET is read-only (row 22): no seeding write here. The lookup still 404s
    // for unknown agents; a row without version history returns an empty list.
    let _agent = db_get_tenant_agent(&__pool_5, &tenant_id, &agent_name).await?;
    let __pool_6 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let records = list_tenant_agent_versions(&__pool_6, &tenant_id, &agent_name, 50).await?;
    Ok(Json(records))
}

/// `POST /admin/tenants/{tenant_id}/agents/{agent_name}/rollback/{version}`:
/// promote a previously published version (item 2.6a, D-6). A version that
/// was never published is refused (400); the rollback actor is recorded as
/// its approver.
pub async fn rollback_tenant_agent_version_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name, version)): Path<(String, String, String)>,
    actor: AdminActor,
) -> Result<Json<TenantAgent>> {
    let approver = config_actor(&actor)?;
    let __pool_9 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let rolled_back =
        rollback_tenant_agent_version(&__pool_9, &tenant_id, &agent_name, &version, approver)
            .await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let resource_id = format!("{}:{}", tenant_id, agent_name);
    let details = format!(
        "Rolled back tenant agent to version {} (published digest {})",
        version, rolled_back.published_digest
    );
    audit_log::record(
        &pool,
        "tenant_agent_rollback",
        "agent",
        &resource_id,
        Some(&details),
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(rolled_back.agent))
}

/// `GET /admin/tenants/{tenant_id}/agents/{agent_name}/draft` (item 2.6a):
/// what runs and under which digest, and the pending draft with its digest
/// and authors. The reviewer approves the `draft_digest` shown here.
pub async fn get_tenant_agent_draft_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<TenantAgentPublishState>> {
    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let state = get_tenant_agent_publish_state(&pool, &tenant_id, &agent_name).await?;
    Ok(Json(state))
}

/// The response to a refused publish: 403 when the approver may not approve
/// this draft, 409 when there is no draft or it is not the one reviewed.
fn publish_refusal_response(refusal: PublishRefusal) -> (StatusCode, Json<serde_json::Value>) {
    let status = match refusal {
        PublishRefusal::NoRecordedAuthor | PublishRefusal::ApproverIsAuthor => {
            StatusCode::FORBIDDEN
        }
        PublishRefusal::NoDraft | PublishRefusal::DraftChanged => StatusCode::CONFLICT,
    };
    (
        status,
        Json(serde_json::json!({
            "published": false,
            "error": refusal.message(),
            "code": refusal.code(),
        })),
    )
}

/// `POST /admin/tenants/{tenant_id}/agents/{agent_name}/publish`, body
/// `{"draft_digest": "<hex>"}` (item 2.6a): publish the stored draft.
///
/// The approval is bound to the draft that was reviewed: in one transaction
/// the server recomputes the stored draft's digest and publishes only if it
/// equals `draft_digest`; the published digest is computed by the server
/// from what now runs. The approver may be none of the draft's authors, and
/// the static admin key is one actor. Status: 200 published; 401 no admin
/// identity; 403 the approver wrote the draft, or no author is recorded;
/// 404 no such agent; 409 no draft, or the draft changed after review.
pub async fn publish_tenant_agent_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    actor: AdminActor,
    Json(req): Json<PublishTenantAgentRequest>,
) -> Result<(StatusCode, Json<serde_json::Value>)> {
    let approver = config_actor(&actor)?;
    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let outcome =
        publish_tenant_agent(&pool, &tenant_id, &agent_name, &req.draft_digest, approver).await?;
    let published = match outcome {
        PublishOutcome::Published(published) => published,
        PublishOutcome::Refused(refusal) => return Ok(publish_refusal_response(refusal)),
    };

    let details = serde_json::json!({
        "published_digest": &published.published_digest,
        "draft_authors": &published.draft_authors,
        "published_by": &published.published_by,
        "approved_by": &published.approved_by,
    })
    .to_string();
    audit_log::record(
        &pool,
        "publish_tenant_agent",
        "agent",
        &published.agent.id,
        Some(&details),
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok((
        StatusCode::OK,
        Json(serde_json::json!({
            "published": true,
            "tenant_id": tenant_id,
            "agent_name": agent_name,
            "published_digest": published.published_digest,
            "published_by": published.published_by,
            "approved_by": published.approved_by,
            "published_at": published.published_at,
            "draft_authors": published.draft_authors,
            "agent": published.agent,
        })),
    ))
}

pub async fn list_agents(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<agent_runs::AllAgentsEntry>>> {
    let __pool_10 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let agents = agent_runs::list_all_agents(&__pool_10).await?;
    Ok(Json(agents))
}

pub async fn get_agent(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<TenantAgent>> {
    let __pool_11 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let agent = db_get_tenant_agent(&__pool_11, &tenant_id, &agent_name).await?;
    Ok(Json(agent))
}

/// `POST /admin/agents`: like [`create_tenant_agent_handler`], the new
/// agent's config is a draft by this admin (item 2.6a).
pub async fn create_agent(
    State(ctx): State<Arc<Context>>,
    actor: AdminActor,
    Json(req): Json<CreateAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let author = config_actor(&actor)?;
    let config = if let Some(tpl_id) = &req.template_id {
        let __pool_12 = ctx
            .get::<ares_store::TenantDb>()
            .expect("not provided")
            .pool()
            .clone();
        let store = AgentTemplateStore::new(__pool_12);
        let tpl = store.get_template(tpl_id).await?.ok_or_else(|| {
            HttpError::from(AppError::InvalidInput(format!(
                "Template '{}' not found",
                tpl_id
            )))
        })?;
        tpl.config
    } else {
        req.config
    };

    let tools = ctx.get::<ares_tools::Tools>().expect("Tools not provided");
    validate_agent_config_tools(&config, tools.as_ref(), &ctx, &req.tenant_id)?;
    validate_agent_config_delegations(&config, &req.agent_name)?;

    let db_req = CreateTenantAgentRequest {
        agent_name: req.agent_name,
        display_name: req.display_name,
        description: req.description,
        config,
    };

    let __pool_13 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let agent = create_tenant_agent_as(&__pool_13, &req.tenant_id, db_req, Some(author)).await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let aid = agent.id.clone();
    audit_log::record(
        &pool,
        "create_agent",
        "agent",
        &aid,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(agent))
}

/// `PUT /admin/agents/{tenant_id}/{agent_name}`: like
/// [`update_tenant_agent_handler`], a config patch is merged into the draft
/// and never touches what runs (item 2.6a).
pub async fn update_agent(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    actor: AdminActor,
    Json(mut req): Json<UpdateAgentRequest>,
) -> Result<Json<TenantAgent>> {
    let __pool_14 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    if let Some(patch) = req.config.take() {
        config_actor(&actor)?;
        let current = get_tenant_agent_publish_state(&__pool_14, &tenant_id, &agent_name).await?;
        let (merged, forward) = split_merged_config_patch(current.draft_base(), patch);
        let tools = ctx.get::<ares_tools::Tools>().expect("Tools not provided");
        validate_agent_config_tools(&merged, tools.as_ref(), &ctx, &tenant_id)?;
        validate_agent_config_delegations(&merged, &agent_name)?;
        req.config = Some(forward);
    }

    let db_req = UpdateTenantAgentRequest {
        display_name: req.display_name,
        description: req.description,
        config: req.config,
        enabled: req.enabled,
    };
    let agent = update_tenant_agent_as(
        &__pool_14,
        &tenant_id,
        &agent_name,
        db_req,
        actor.audit_actor(),
    )
    .await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let aid = agent.id.clone();
    audit_log::record(
        &pool,
        "update_agent",
        "agent",
        &aid,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(agent))
}

pub async fn delete_agent(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    actor: AdminActor,
) -> Result<StatusCode> {
    let __pool_15 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    db_delete_tenant_agent(&__pool_15, &tenant_id, &agent_name).await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let resource_id = format!("{}:{}", tenant_id, agent_name);
    audit_log::record(
        &pool,
        "delete_agent",
        "agent",
        &resource_id,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn get_agent_versions(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
) -> Result<Json<Vec<agent_versions::AgentVersionRecord>>> {
    let __pool_16 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    // GET is read-only (row 22): no seeding write here. The lookup still 404s
    // for unknown agents; a row without version history returns an empty list.
    let _agent = db_get_tenant_agent(&__pool_16, &tenant_id, &agent_name).await?;
    let __pool_17 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let records = list_tenant_agent_versions(&__pool_17, &tenant_id, &agent_name, 50).await?;
    Ok(Json(records))
}

/// `POST /admin/agents/{tenant_id}/{agent_name}/rollback/{version}`: like
/// [`rollback_tenant_agent_version_handler`], only a previously published
/// version is promoted (item 2.6a, D-6).
pub async fn rollback_agent(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name, version)): Path<(String, String, String)>,
    actor: AdminActor,
) -> Result<Json<TenantAgent>> {
    let approver = config_actor(&actor)?;
    let __pool_20 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let rolled_back =
        rollback_tenant_agent_version(&__pool_20, &tenant_id, &agent_name, &version, approver)
            .await?;
    let agent = rolled_back.agent;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let resource_id = format!("{}:{}", tenant_id, agent_name);
    let details = format!(
        "Rolled back agent to version {} (published digest {})",
        version, rolled_back.published_digest
    );
    audit_log::record(
        &pool,
        "agent_rollback",
        "agent",
        &resource_id,
        Some(&details),
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(agent))
}

pub async fn create_agent_template_handler(
    State(ctx): State<Arc<Context>>,
    actor: AdminActor,
    Json(req): Json<CreateTemplateRequest>,
) -> Result<Json<AgentTemplate>> {
    let __pool_21 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = AgentTemplateStore::new(__pool_21);
    let tpl = store.create_template(&req).await?;

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let tid = tpl.id.clone();
    audit_log::record(
        &pool,
        "create_agent_template",
        "agent_template",
        &tid,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(tpl))
}

pub async fn delete_agent_template_handler(
    State(ctx): State<Arc<Context>>,
    Path(id): Path<String>,
    actor: AdminActor,
) -> Result<StatusCode> {
    let __pool_22 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let store = AgentTemplateStore::new(__pool_22);
    let deleted = store.delete_template(&id).await?;
    if deleted == 0 {
        return Err(HttpError::from(AppError::NotFound(format!(
            "Template '{}' not found",
            id
        ))));
    }

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    audit_log::record(
        &pool,
        "delete_agent_template",
        "agent_template",
        &id,
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(StatusCode::NO_CONTENT)
}

pub async fn test_tenant_agent_handler(
    State(ctx): State<Arc<Context>>,
    Path((tenant_id, agent_name)): Path<(String, String)>,
    Json(req): Json<TestTenantAgentRequest>,
) -> Result<Json<TestTenantAgentResponse>> {
    if ctx
        .get::<ares_agent::EmergencyStop>()
        .expect("not provided")
        .is_active()
    {
        return Err(HttpError::from(AppError::Unavailable(
            "All agents are currently under human review. Please try again later.".to_string(),
        )));
    }

    let message = req.message.trim();
    if message.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(
            "Test Agent requires a non-empty message".to_string(),
        )));
    }

    let __pool_23 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    db_get_tenant_agent(&__pool_23, &tenant_id, &agent_name).await?;
    validate_agent_config_delegations(&req.config, &agent_name)?;
    let agent_config = tenant_agent::agent_config_from_json(&req.config)?;
    let __pool_24 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let mut draft_agent = ctx
        .get::<ares_agent::AgentRegistry>()
        .expect("AgentRegistry not provided")
        .create_agent_from_config_with_fallbacks(
            &agent_name,
            &agent_config,
            &tenant_id,
            &__pool_24,
            &ctx.get::<ares_store::FleetSecrets>().expect("not provided"),
        )
        .await?;

    // Attach observability
    let run_id = uuid::Uuid::new_v4().to_string();
    let __pool_25 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let obs = Arc::new(crate::observability::RunObservability {
        run_id: run_id.clone(),
        tenant_id: tenant_id.clone(),
        agent_name: agent_name.clone(),
        pool: __pool_25,
    });
    draft_agent.set_observability(obs.clone());
    let ctx = ares_agent::tenant_scope(&ctx, &tenant_id);
    if let Some(tools) = ctx.get::<ares_tools::Tools>() {
        draft_agent.set_tools(tools);
    }
    draft_agent.bind_request_ctx(ctx.clone());
    draft_agent.set_run_id(run_id.clone());

    // Record the draft run's own `agent_runs` row (#53): the run's
    // `run_llm_calls`, `run_costs` and `run_tool_calls` all reference it, so
    // without the parent every one of those writes fails its foreign key and
    // the draft run leaves no trace. Best-effort: a failed insert must not
    // fail the test run.
    let (draft_model_name, draft_provider_name) = {
        let (model, provider) = draft_agent.resolved_model_and_provider();
        (model.to_string(), provider.to_string())
    };
    let draft_run_metadata = agent_runs::AgentRunMetadata {
        agent_config_source: Some("draft".to_string()),
        agent_config_version: Some("draft".to_string()),
        request_source: Some("admin_test".to_string()),
        ..Default::default()
    };
    if let Err(error) = agent_runs::insert_agent_run_with_id_and_metadata(
        &obs.pool,
        &run_id,
        &tenant_id,
        &agent_name,
        None,
        "running",
        0,
        0,
        0,
        None,
        &draft_model_name,
        &draft_provider_name,
        false,
        Some(&draft_run_metadata),
    )
    .await
    {
        tracing::warn!(error = %error, run_id = %run_id, "Failed to insert draft run record");
    }

    ctx.get::<crate::active_runs::ActiveRuns>()
        .expect("not provided")
        .start(crate::active_runs::ActiveRun {
            run_id: run_id.clone(),
            tenant_id: tenant_id.clone(),
            agent_name: agent_name.clone(),
            started_at: chrono::Utc::now().timestamp(),
            status: "running".to_string(),
            current_step: 0,
            total_steps: 0,
            last_update: chrono::Utc::now().timestamp(),
            tool_name: None,
            model: None,
            is_catchup: false,
            request_source: Some("admin_test_agent".to_string()),
            pipeline_id: None,
            schedule_id: None,
            trigger_id: None,
        });

    let agent_context = AgentContext {
        user_id: tenant_id.clone(),
        session_id: format!("admin-test-{}", uuid::Uuid::new_v4()),
        conversation_history: vec![],
        user_memory: None,
    };

    let mut runtime_context =
        AgentRuntimeContext::new(tenant_id.clone(), agent_name.clone(), "admin_test_agent");
    runtime_context.workspace_id = req
        .workspace_id
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| value.to_string());
    runtime_context.session_id = Some(agent_context.session_id.clone());

    let eruka_context = if req.use_eruka_context {
        ctx.get::<ares_agent::ContextProviderHandle>()
            .expect("not provided")
            .0
            .get_context_for_run(&runtime_context)
            .await
    } else {
        None
    };
    let eruka_context_injected = eruka_context.is_some();
    let effective_message = if let Some(ctx) = eruka_context {
        format!("{}\n\n---\nUser message: {}", ctx, message)
    } else {
        message.to_string()
    };

    let start = std::time::Instant::now();
    use ares_agent::Agent;
    let result = draft_agent
        .execute(&effective_message, &agent_context)
        .await;
    let duration_ms = start.elapsed().as_millis() as u64;

    // Aggregate run costs (fire-and-forget)
    let dur_i64 = duration_ms as i64;
    let obs_for_spawn = obs.clone();
    tokio::spawn(async move {
        obs_for_spawn.aggregate_run_cost(dur_i64).await;
    });

    let config_version = req
        .config
        .get("version")
        .and_then(|value| value.as_str())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| format!("draft:{}", value))
        .unwrap_or_else(|| "draft".to_string());

    match result {
        Ok(response) => {
            ctx.get::<crate::active_runs::ActiveRuns>()
                .expect("not provided")
                .finish(&run_id, "completed");
            let (input_tokens, output_tokens) = if let Some(ref usage) = response.usage {
                (usage.prompt_tokens as u64, usage.completion_tokens as u64)
            } else {
                (
                    estimate_tokens(&effective_message) as u64,
                    estimate_tokens(&response.content) as u64,
                )
            };
            let model_name = response
                .metadata
                .as_ref()
                .map(|metadata| metadata.model_name.clone());
            let provider_name = response
                .metadata
                .as_ref()
                .map(|metadata| metadata.provider_name.clone());
            finish_draft_run_row(
                &obs.pool,
                &run_id,
                "completed",
                input_tokens,
                output_tokens,
                duration_ms,
                None,
                model_name.as_deref().unwrap_or(&draft_model_name),
                provider_name.as_deref().unwrap_or(&draft_provider_name),
            )
            .await;

            Ok(Json(TestTenantAgentResponse {
                status: "completed".to_string(),
                response: Some(response.content),
                error: None,
                input_tokens,
                output_tokens,
                duration_ms,
                model_name,
                provider_name,
                config_source: "draft".to_string(),
                config_version,
                workspace_id: runtime_context.workspace_id,
                eruka_context_injected,
            }))
        }
        Err(error) => {
            ctx.get::<crate::active_runs::ActiveRuns>()
                .expect("not provided")
                .finish(&run_id, "error");
            // The row's error honours the tenant's no-retain flag, as the v1
            // close-out does; the response still carries the raw text.
            let raw_error = error.to_string();
            let row_error = ares_store::run_history::redact_agent_run_error(
                ares_store::run_history::tenant_no_retain(&obs.pool, &tenant_id).await,
                Some(&raw_error),
            );
            finish_draft_run_row(
                &obs.pool,
                &run_id,
                "failed",
                estimate_tokens(&effective_message) as u64,
                0,
                duration_ms,
                row_error.as_deref(),
                &draft_model_name,
                &draft_provider_name,
            )
            .await;
            Ok(Json(TestTenantAgentResponse {
                status: "failed".to_string(),
                response: None,
                error: Some(error.to_string()),
                input_tokens: estimate_tokens(&effective_message) as u64,
                output_tokens: 0,
                duration_ms,
                model_name: None,
                provider_name: None,
                config_source: "draft".to_string(),
                config_version,
                workspace_id: runtime_context.workspace_id,
                eruka_context_injected,
            }))
        }
    }
}

/// Best-effort close-out of a Test Draft run's `agent_runs` row (#53): the row
/// was inserted `running` before the run; here it gets its final status,
/// tokens, duration and the model/provider names the response carries, the
/// same shape as the v1 close-out (`complete_llm_run` / `fail_llm_run` in
/// `handlers/v1/agents.rs`). A failed update is logged and never fails the
/// test request.
#[allow(clippy::too_many_arguments)]
async fn finish_draft_run_row(
    pool: &sqlx::PgPool,
    run_id: &str,
    status: &str,
    input_tokens: u64,
    output_tokens: u64,
    duration_ms: u64,
    error: Option<&str>,
    model_name: &str,
    provider_name: &str,
) {
    if let Err(e) = sqlx::query(
        "UPDATE agent_runs SET status = $2, input_tokens = $3, output_tokens = $4, \
         duration_ms = $5, error = $6, model_name = $7, provider_name = $8, \
         updated_at = $9 WHERE id = $1",
    )
    .bind(run_id)
    .bind(status)
    .bind(input_tokens as i64)
    .bind(output_tokens as i64)
    .bind(duration_ms as i64)
    .bind(error)
    .bind(model_name)
    .bind(provider_name)
    .bind(chrono::Utc::now().timestamp())
    .execute(pool)
    .await
    {
        tracing::warn!(error = %e, run_id = %run_id, "Failed to finish draft run record");
    }
}

pub async fn list_agent_templates_handler(
    State(ctx): State<Arc<Context>>,
    Query(params): Query<HashMap<String, String>>,
) -> Result<Json<Vec<AgentTemplate>>> {
    let product_type = params.get("product_type").map(|s| s.as_str());
    let __pool_26 = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    let templates = list_agent_templates(&__pool_26, product_type).await?;
    Ok(Json(templates))
}

/// GET /api/admin/agents/emergency-stop
/// Return whether the global emergency stop is active.
pub async fn get_emergency_stop_handler(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<EmergencyStopStatus>> {
    Ok(Json(emergency_stop_status(
        ctx.get::<ares_agent::EmergencyStop>()
            .expect("not provided")
            .is_active(),
    )))
}

/// POST /api/admin/agents/emergency-stop
/// Enable or disable the global emergency stop.
/// When active, agent execution entrypoints are rejected with 503.
pub async fn emergency_stop_handler(
    State(ctx): State<Arc<Context>>,
    actor: AdminActor,
    Json(payload): Json<EmergencyStopRequest>,
) -> Result<Json<EmergencyStopStatus>> {
    ctx.get::<ares_agent::EmergencyStop>()
        .expect("not provided")
        .set_active(payload.active);

    let action = if payload.active {
        "emergency_stop_enabled"
    } else {
        "emergency_stop_disabled"
    };
    tracing::warn!(active = payload.active, "Emergency stop toggled");

    let pool = ctx
        .get::<ares_store::TenantDb>()
        .expect("not provided")
        .pool()
        .clone();
    audit_log::record(
        &pool,
        action,
        "platform",
        "all_agents",
        None,
        actor.ip(),
        actor.audit_actor(),
    )
    .await;

    Ok(Json(emergency_stop_status(payload.active)))
}

use ::cordis::Service;
