//! Item 2.6a: the draft -> publish gate for tenant agent configs, core
//! (`rulings/2026-10-01-DELEGATED-sr-2.6-core-design.md`, D-1 to D-7 and the
//! conditions in its section 3).
//!
//! `tenant_agents.config` is what runs, and only a publish by a second admin
//! changes it:
//! - every writer of `config` (PUT, create, the template clone and
//!   `set_tenant_agent_model`) writes a draft instead (D-1, D-4);
//! - `POST .../publish` is bound to the digest of the draft the approver
//!   reviewed, and the approver may be none of the draft's authors, the
//!   static admin key counting as one actor (section 3.1, 3.2, D-5);
//! - rollback promotes only a version that was published before (D-6);
//! - the run path refuses a row that was never published (D-7);
//! - the cutover migration publishes every existing row with the one SQL
//!   digest (D-2, D-3). It is tested on a fresh schema of the scratch
//!   database, migrated to the release before the gate.
//!
//! Handlers are called directly with hand-built extractors, as in
//! `audit_writes_live.rs`. The live database is the one named by
//! `TEST_DATABASE_URL` (`tests/common/mod.rs`): configured and unreachable
//! panics, unconfigured skips.

#![cfg(feature = "postgres")]

mod common;

use std::borrow::Cow;
use std::sync::Arc;

use ares_agent::tenant_agent::load_tenant_agent_config;
use ares_http::api::handlers::admin::{
    create_tenant_agent_handler, get_tenant_agent_draft_handler, publish_tenant_agent_handler,
    rollback_tenant_agent_version_handler, update_tenant_agent_handler, AdminActor,
};
use ares_store::agent_versions::AgentVersionRecord;
use ares_store::tenant_agents::{
    clone_templates_for_tenant, get_tenant_agent, get_tenant_agent_publish_state,
    list_tenant_agent_versions, publish_tenant_agent, rollback_tenant_agent_version,
    set_tenant_agent_model, update_tenant_agent_as, AgentTemplateStore, CreateTemplateRequest,
    CreateTenantAgentRequest, PublishOutcome, PublishTenantAgentRequest, TenantAgentPublishState,
    UpdateTenantAgentRequest, CUTOVER_ACTOR,
};
use ares_store::TenantDb;
use ares_types::types::AppError;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use axum::response::IntoResponse;
use axum::Json;
use cordis::Context;
use serde_json::{json, Value};
use sha2::Digest;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Row};

const AUTHOR_A: &str = "publish-gate-admin-a";
const ADMIN_B: &str = "publish-gate-admin-b";
const ADMIN_C: &str = "publish-gate-admin-c";
const PROVISIONER: &str = "publish-gate-provisioner";

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// A root `Context` with a connected `TenantDb` and an empty tool set (the
/// create and PUT handlers validate tool names against it), plus the pool
/// for the test's own assertions. `None` only for the unconfigured skip.
async fn live_ctx() -> Option<(Arc<Context>, PgPool)> {
    common::live_db_url(&common::current_test_name()).await?;
    let pg = ares_test_support::client().await;
    let pool = pg.pool.clone();
    let ctx = Context::new_root();
    ctx.provide_arc(Arc::new(TenantDb::new(Arc::new(pg))));
    ctx.provide(ares_tools::Tools::from_static(Vec::<
        Arc<dyn ares_tools::Tool>,
    >::new()));
    Some((ctx, pool))
}

fn unique(prefix: &str) -> String {
    format!("{prefix}-{}", uuid::Uuid::new_v4())
}

/// An admin authenticated with a JWT: its actor is the token's subject.
fn jwt(subject: &str) -> AdminActor {
    AdminActor {
        subject: Some(subject.to_string()),
        email: None,
        auth: Some("jwt"),
        client_ip: Some("203.0.113.26".to_string()),
    }
}

/// The static `X-Admin-Secret` key: one actor, `admin_secret` (D-5).
fn static_key() -> AdminActor {
    AdminActor {
        subject: None,
        email: None,
        auth: Some("admin_secret"),
        client_ip: Some("203.0.113.26".to_string()),
    }
}

fn prompt_config(prompt: &str) -> Value {
    json!({
        "model": "publish-gate-model",
        "system_prompt": prompt,
        "tools": [],
        "max_tool_iterations": 3
    })
}

/// A config shaped like a production conversational agent: a long
/// multi-line prompt with quotes, escapes and non-ASCII text, tools and an
/// allow-list, sampling settings and a version, its keys written out of
/// order. The digest must be the one of Postgres's canonical `jsonb` text,
/// not of what was sent.
fn conversational_agent_config() -> Value {
    json!({
        "version": "2026-09-30.1",
        "temperature": 0.7,
        "system_prompt": "You are a supportive conversational assistant.\n\n\
            - Listen first and reflect back what you heard.\n\
            - Keep replies short, warm and plain.\n\
            - When the person asks for a human, say: \"I'll connect you with someone now.\"\n\n\
            Unicode and escapes: caf\u{e9}, na\u{ef}ve, \u{65e5}\u{672c}\u{8a9e}, \u{1F642}, \
            tab\there, back\\slash.",
        "model": "conversation-tier",
        "tools": ["memory_lookup", "handback"],
        "allowed_tools": ["memory_lookup", "handback"],
        "max_tool_iterations": 4,
        "max_tokens": 600,
        "top_p": 0.9,
        "stop": ["\nUser:"],
        "parallel_tools": false
    })
}

/// sha256 over `text`, hex, computed in Rust: independent of the database
/// function that defines the digest.
fn rust_digest(text: &str) -> String {
    hex::encode(sha2::Sha256::digest(text.as_bytes()))
}

/// The digest of a stored JSONB column (`config` or `draft_config`),
/// computed independently: its `::text` read back and hashed in Rust.
async fn independent_digest(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
    column: &str,
) -> String {
    assert!(column == "config" || column == "draft_config");
    let text: String = sqlx::query_scalar(&format!(
        "SELECT {column}::text FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2"
    ))
    .bind(tenant_id)
    .bind(agent_name)
    .fetch_one(pool)
    .await
    .unwrap_or_else(|e| panic!("read {column}::text: {e}"));
    rust_digest(&text)
}

/// What the run path serves for the row: the version and config JSON that
/// `load_tenant_agent_config` returns, or its refusal.
async fn served(
    pool: &PgPool,
    tenant_id: &str,
    agent_name: &str,
) -> Result<(String, Value), AppError> {
    match load_tenant_agent_config(pool, tenant_id, agent_name).await {
        Ok(Some((_config, version, json))) => Ok((version, json)),
        Ok(None) => panic!("no tenant_agents row for {tenant_id}/{agent_name}"),
        Err(e) => Err(e),
    }
}

async fn served_config(pool: &PgPool, tenant_id: &str, agent_name: &str) -> Value {
    served(pool, tenant_id, agent_name)
        .await
        .unwrap_or_else(|e| panic!("{tenant_id}/{agent_name} must run: {e:?}"))
        .1
}

/// The run path refuses the row as never published, like a disabled agent.
fn assert_not_published(result: Result<(String, Value), AppError>) {
    match result {
        Err(AppError::NotFound(m)) if m.contains("not published") => {}
        other => panic!("expected the run path to refuse with \"not published\", got {other:?}"),
    }
}

async fn state(pool: &PgPool, tenant_id: &str, agent_name: &str) -> TenantAgentPublishState {
    get_tenant_agent_publish_state(pool, tenant_id, agent_name)
        .await
        .expect("publish state")
}

async fn create_as(
    ctx: &Arc<Context>,
    actor: AdminActor,
    tenant_id: &str,
    agent_name: &str,
    config: Value,
) {
    let Json(_created) = create_tenant_agent_handler(
        State(ctx.clone()),
        Path(tenant_id.to_string()),
        actor,
        Json(CreateTenantAgentRequest {
            agent_name: agent_name.to_string(),
            display_name: format!("{agent_name} display"),
            description: None,
            config,
        }),
    )
    .await
    .expect("create_tenant_agent_handler");
}

async fn put_config_as(
    ctx: &Arc<Context>,
    actor: AdminActor,
    tenant_id: &str,
    agent_name: &str,
    patch: Value,
) {
    let Json(_updated) = update_tenant_agent_handler(
        State(ctx.clone()),
        Path((tenant_id.to_string(), agent_name.to_string())),
        actor,
        Json(UpdateTenantAgentRequest {
            display_name: None,
            description: None,
            config: Some(patch),
            enabled: None,
        }),
    )
    .await
    .expect("update_tenant_agent_handler");
}

/// The digest a reviewer reads from `GET .../draft` before approving.
async fn reviewed_digest(ctx: &Arc<Context>, tenant_id: &str, agent_name: &str) -> String {
    let Json(view) = get_tenant_agent_draft_handler(
        State(ctx.clone()),
        Path((tenant_id.to_string(), agent_name.to_string())),
    )
    .await
    .expect("get_tenant_agent_draft_handler");
    view.draft_digest.expect("the row has a draft")
}

async fn publish_as(
    ctx: &Arc<Context>,
    actor: AdminActor,
    tenant_id: &str,
    agent_name: &str,
    digest: &str,
) -> (StatusCode, Value) {
    let (status, Json(body)) = publish_tenant_agent_handler(
        State(ctx.clone()),
        Path((tenant_id.to_string(), agent_name.to_string())),
        actor,
        Json(PublishTenantAgentRequest {
            draft_digest: digest.to_string(),
        }),
    )
    .await
    .expect("publish_tenant_agent_handler");
    (status, body)
}

/// Created by A and published by B: a row whose `config` runs. Returns the
/// published digest.
async fn published_row(
    ctx: &Arc<Context>,
    tenant_id: &str,
    agent_name: &str,
    config: Value,
) -> String {
    create_as(ctx, jwt(AUTHOR_A), tenant_id, agent_name, config).await;
    let digest = reviewed_digest(ctx, tenant_id, agent_name).await;
    let (status, body) = publish_as(ctx, jwt(ADMIN_B), tenant_id, agent_name, &digest).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    digest
}

async fn versions(pool: &PgPool, tenant_id: &str, agent_name: &str) -> Vec<AgentVersionRecord> {
    list_tenant_agent_versions(pool, tenant_id, agent_name, 50)
        .await
        .expect("versions")
}

/// `(actor, details)` of every `admin_audit_log` row for one action and
/// resource.
async fn audit_rows(
    pool: &PgPool,
    action: &str,
    resource_id: &str,
) -> Vec<(Option<String>, Option<String>)> {
    sqlx::query(
        "SELECT actor, details FROM admin_audit_log WHERE action = $1 AND resource_id = $2 \
         ORDER BY created_at",
    )
    .bind(action)
    .bind(resource_id)
    .fetch_all(pool)
    .await
    .expect("audit rows")
    .into_iter()
    .map(|row| (row.get("actor"), row.get("details")))
    .collect()
}

// ---------------------------------------------------------------------------
// The two-person rule (section 3.2, D-5)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn single_author_approving_is_refused() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-single");
    create_as(&ctx, jwt(AUTHOR_A), &tenant, "agent", prompt_config("one")).await;
    let digest = reviewed_digest(&ctx, &tenant, "agent").await;

    let (status, body) = publish_as(&ctx, jwt(AUTHOR_A), &tenant, "agent", &digest).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["published"], json!(false), "{body}");
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(s.published_digest, None, "nothing was published");
    assert_eq!(
        s.draft_config,
        Some(prompt_config("one")),
        "the draft is kept"
    );
    assert_not_published(served(&pool, &tenant, "agent").await);
}

#[tokio::test]
async fn a_writes_b_edits_a_approves_is_refused() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-aba");
    create_as(&ctx, jwt(AUTHOR_A), &tenant, "agent", prompt_config("one")).await;
    put_config_as(
        &ctx,
        jwt(ADMIN_B),
        &tenant,
        "agent",
        json!({"system_prompt": "two"}),
    )
    .await;
    assert_eq!(
        state(&pool, &tenant, "agent").await.draft_authors,
        vec![AUTHOR_A.to_string(), ADMIN_B.to_string()],
        "both writers of the draft are its authors"
    );
    let digest = reviewed_digest(&ctx, &tenant, "agent").await;

    let (status, body) = publish_as(&ctx, jwt(AUTHOR_A), &tenant, "agent", &digest).await;

    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(state(&pool, &tenant, "agent").await.published_digest, None);
    assert_not_published(served(&pool, &tenant, "agent").await);
}

#[tokio::test]
async fn a_writes_b_approves_publishes() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-ab");
    let config = prompt_config("approved by a second admin");
    create_as(&ctx, jwt(AUTHOR_A), &tenant, "agent", config.clone()).await;
    let digest = reviewed_digest(&ctx, &tenant, "agent").await;
    assert_eq!(
        digest,
        independent_digest(&pool, &tenant, "agent", "draft_config").await,
        "the draft view shows the digest of the stored draft"
    );

    let (status, body) = publish_as(&ctx, jwt(ADMIN_B), &tenant, "agent", &digest).await;

    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(body["published"], json!(true), "{body}");
    assert_eq!(body["published_digest"], json!(digest), "{body}");
    assert_eq!(body["published_by"], json!(AUTHOR_A), "{body}");
    assert_eq!(body["approved_by"], json!(ADMIN_B), "{body}");

    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(s.config, config, "the draft moved into config");
    assert_eq!(s.published_digest.as_deref(), Some(digest.as_str()));
    assert_eq!(
        s.published_digest.as_deref(),
        Some(
            independent_digest(&pool, &tenant, "agent", "config")
                .await
                .as_str()
        ),
        "the published digest is the digest of what now runs"
    );
    assert_eq!(s.published_by.as_deref(), Some(AUTHOR_A));
    assert_eq!(s.approved_by.as_deref(), Some(ADMIN_B));
    assert!(s.published_at.is_some());
    assert_eq!(s.draft_config, None, "the draft is cleared");
    assert_eq!(s.draft_digest, None);
    assert_eq!(s.draft_by, None);
    assert!(s.draft_authors.is_empty(), "the author list is cleared");
    assert_eq!(s.draft_at, None);
    assert_eq!(served_config(&pool, &tenant, "agent").await, config);

    let publish_versions: Vec<AgentVersionRecord> = versions(&pool, &tenant, "agent")
        .await
        .into_iter()
        .filter(|v| v.change_source == format!("publish:{digest}"))
        .collect();
    assert_eq!(
        publish_versions.len(),
        1,
        "one Publish version row carrying the digest"
    );
    assert!(publish_versions[0].is_active);
    assert_eq!(
        publish_versions[0].config_json["published_digest"],
        json!(digest)
    );

    let row_id = get_tenant_agent(&pool, &tenant, "agent")
        .await
        .expect("row")
        .id;
    let rows = audit_rows(&pool, "publish_tenant_agent", &row_id).await;
    assert_eq!(rows.len(), 1, "one audit row for the publish: {rows:?}");
    assert_eq!(
        rows[0].0.as_deref(),
        Some(ADMIN_B),
        "the approver is the actor"
    );
    let details = rows[0].1.as_deref().expect("details");
    for needle in [digest.as_str(), AUTHOR_A, ADMIN_B] {
        assert!(
            details.contains(needle),
            "details must name {needle}: {details}"
        );
    }
}

#[tokio::test]
async fn static_key_cannot_approve_its_own_draft() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-static");
    create_as(
        &ctx,
        static_key(),
        &tenant,
        "agent",
        prompt_config("static"),
    )
    .await;
    assert_eq!(
        state(&pool, &tenant, "agent").await.draft_authors,
        vec!["admin_secret".to_string()],
        "the static key is recorded as one actor"
    );
    let digest = reviewed_digest(&ctx, &tenant, "agent").await;

    let (status, body) = publish_as(&ctx, static_key(), &tenant, "agent", &digest).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(state(&pool, &tenant, "agent").await.published_digest, None);
    assert_not_published(served(&pool, &tenant, "agent").await);

    // A second actor may approve the key's draft (D-5: "or the reverse").
    let (status, body) = publish_as(&ctx, jwt(ADMIN_B), &tenant, "agent", &digest).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        served_config(&pool, &tenant, "agent").await,
        prompt_config("static")
    );
}

#[tokio::test]
async fn an_admin_without_an_identity_cannot_publish() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-anon");
    create_as(&ctx, jwt(AUTHOR_A), &tenant, "agent", prompt_config("one")).await;
    let digest = reviewed_digest(&ctx, &tenant, "agent").await;

    let err = publish_tenant_agent_handler(
        State(ctx.clone()),
        Path((tenant.clone(), "agent".to_string())),
        AdminActor::default(),
        Json(PublishTenantAgentRequest {
            draft_digest: digest,
        }),
    )
    .await
    .expect_err("a request with no admin identity cannot approve");
    assert_eq!(err.into_response().status(), StatusCode::UNAUTHORIZED);
    assert_eq!(state(&pool, &tenant, "agent").await.published_digest, None);
}

// ---------------------------------------------------------------------------
// The approval is bound to the draft it approved (section 3.1)
// ---------------------------------------------------------------------------

#[tokio::test]
async fn a_draft_changed_after_review_is_refused_and_nothing_is_published() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-bound");
    let v1 = prompt_config("v1");
    let d1 = published_row(&ctx, &tenant, "agent", v1.clone()).await;

    put_config_as(
        &ctx,
        jwt(AUTHOR_A),
        &tenant,
        "agent",
        json!({"system_prompt": "v2"}),
    )
    .await;
    let reviewed = reviewed_digest(&ctx, &tenant, "agent").await;
    put_config_as(
        &ctx,
        jwt(AUTHOR_A),
        &tenant,
        "agent",
        json!({"system_prompt": "v3, written after the review"}),
    )
    .await;

    let (status, body) = publish_as(&ctx, jwt(ADMIN_B), &tenant, "agent", &reviewed).await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["published"], json!(false), "{body}");
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(
        s.published_digest.as_deref(),
        Some(d1.as_str()),
        "nothing was published"
    );
    assert_eq!(s.config, v1, "the published config is untouched");
    assert_eq!(
        served_config(&pool, &tenant, "agent").await,
        v1,
        "v1 still runs"
    );

    // The request's digest is compared, never stored: the current digest in
    // another spelling is refused too ...
    let current = reviewed_digest(&ctx, &tenant, "agent").await;
    assert_ne!(current, reviewed);
    let (status, body) = publish_as(
        &ctx,
        jwt(ADMIN_B),
        &tenant,
        "agent",
        &current.to_uppercase(),
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(
        state(&pool, &tenant, "agent")
            .await
            .published_digest
            .as_deref(),
        Some(d1.as_str())
    );

    // ... and on a match the stored value is the server's digest of what now runs.
    let (status, body) = publish_as(&ctx, jwt(ADMIN_B), &tenant, "agent", &current).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(
        s.published_digest.as_deref(),
        Some(
            independent_digest(&pool, &tenant, "agent", "config")
                .await
                .as_str()
        )
    );
    assert_eq!(
        s.config["system_prompt"],
        json!("v3, written after the review")
    );
}

#[tokio::test]
async fn publishing_with_no_draft_is_refused() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-nodraft");
    let d1 = published_row(&ctx, &tenant, "agent", prompt_config("v1")).await;

    let (status, body) = publish_as(&ctx, jwt(ADMIN_C), &tenant, "agent", &d1).await;

    assert_eq!(status, StatusCode::CONFLICT, "{body}");
    assert_eq!(body["published"], json!(false), "{body}");
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(s.published_digest.as_deref(), Some(d1.as_str()));
    assert_eq!(
        s.approved_by.as_deref(),
        Some(ADMIN_B),
        "the last publish stands"
    );
}

// ---------------------------------------------------------------------------
// One test per writer of `config` (section 3.3)
// ---------------------------------------------------------------------------

/// PUT writes the draft, and a run before and after it serves the same
/// published config (and reports the same version).
#[tokio::test]
async fn put_writes_a_draft_and_changes_nothing_that_runs() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-put");
    let v1 = prompt_config("published");
    let d1 = published_row(&ctx, &tenant, "agent", v1.clone()).await;
    let before = served(&pool, &tenant, "agent")
        .await
        .expect("the published row runs");

    put_config_as(
        &ctx,
        jwt(AUTHOR_A),
        &tenant,
        "agent",
        json!({"system_prompt": "draft only"}),
    )
    .await;

    let after = served(&pool, &tenant, "agent").await.expect("still runs");
    assert_eq!(before, after, "editing a draft changes nothing that runs");
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(s.config, v1);
    assert_eq!(s.published_digest.as_deref(), Some(d1.as_str()));
    assert_eq!(
        s.draft_config.expect("the edit is a draft")["system_prompt"],
        json!("draft only")
    );
}

/// Create writes a never-published draft. Its `config`, which the v1 run
/// route's skill branch, triggers, schedules and pipelines read through
/// `get_tenant_agent`, carries nothing runnable, and the run path refuses it.
#[tokio::test]
async fn create_writes_a_never_published_draft() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-create");
    let config = json!({
        "model": "publish-gate-model",
        "system_prompt": "a draft that names a skill",
        "skill_id": "publish-gate-skill"
    });
    create_as(&ctx, jwt(AUTHOR_A), &tenant, "agent", config.clone()).await;

    let row = get_tenant_agent(&pool, &tenant, "agent")
        .await
        .expect("row");
    assert_eq!(row.config, json!({}), "a never-published row runs nothing");
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(s.published_digest, None);
    assert_eq!(s.draft_config, Some(config));
    assert_eq!(s.draft_by.as_deref(), Some(AUTHOR_A));
    assert_eq!(s.draft_authors, vec![AUTHOR_A.to_string()]);
    assert_not_published(served(&pool, &tenant, "agent").await);
}

/// The template clone at provisioning writes drafts (D-4): a new tenant's
/// agents run only after a second admin publishes them.
#[tokio::test]
async fn clone_templates_for_tenant_writes_drafts() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let product = unique("publish-gate-product");
    let templates = AgentTemplateStore::new(pool.clone());
    let template = templates
        .create_template(&CreateTemplateRequest {
            product_type: product.clone(),
            agent_name: "cloned-agent".to_string(),
            display_name: "Cloned".to_string(),
            description: None,
            config: prompt_config("from the template"),
        })
        .await
        .expect("seed template");
    let tenant = unique("t26a-clone");

    let states = clone_templates_for_tenant(&pool, &tenant, &product, Some(PROVISIONER))
        .await
        .expect("clone");

    assert_eq!(states.len(), 1, "{states:?}");
    let s = &states[0];
    assert_eq!(s.agent_name, "cloned-agent");
    assert_eq!(s.published_digest, None);
    assert_eq!(s.config, json!({}));
    assert_eq!(s.draft_config, Some(prompt_config("from the template")));
    assert_eq!(s.draft_authors, vec![PROVISIONER.to_string()]);
    assert_not_published(served(&pool, &tenant, "cloned-agent").await);

    // The provisioner cannot publish what it cloned; a second admin can.
    let digest = reviewed_digest(&ctx, &tenant, "cloned-agent").await;
    let (status, body) = publish_as(&ctx, jwt(PROVISIONER), &tenant, "cloned-agent", &digest).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    let (status, body) = publish_as(&ctx, jwt(ADMIN_B), &tenant, "cloned-agent", &digest).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_eq!(
        served_config(&pool, &tenant, "cloned-agent").await,
        prompt_config("from the template")
    );

    templates
        .delete_template(&template.id)
        .await
        .expect("cleanup template");
}

/// `set_tenant_agent_model` writes the draft (D-4).
#[tokio::test]
async fn set_tenant_agent_model_writes_a_draft() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-model");
    let v1 = json!({"model": "model-one", "system_prompt": "p"});
    let d1 = published_row(&ctx, &tenant, "agent", v1.clone()).await;

    set_tenant_agent_model(&pool, &tenant, "agent", "model-two", Some(PROVISIONER))
        .await
        .expect("set model");

    assert_eq!(
        served_config(&pool, &tenant, "agent").await,
        v1,
        "model-one still runs"
    );
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(s.published_digest.as_deref(), Some(d1.as_str()));
    assert_eq!(s.draft_config.expect("draft")["model"], json!("model-two"));
    assert_eq!(s.draft_authors, vec![PROVISIONER.to_string()]);

    // On a row never published, it stays refused.
    create_as(
        &ctx,
        jwt(AUTHOR_A),
        &tenant,
        "unpublished",
        prompt_config("x"),
    )
    .await;
    set_tenant_agent_model(
        &pool,
        &tenant,
        "unpublished",
        "model-two",
        Some(PROVISIONER),
    )
    .await
    .expect("set model");
    assert_not_published(served(&pool, &tenant, "unpublished").await);
    let s = state(&pool, &tenant, "unpublished").await;
    assert_eq!(s.draft_config.expect("draft")["model"], json!("model-two"));
    assert_eq!(
        s.draft_authors,
        vec![AUTHOR_A.to_string(), PROVISIONER.to_string()]
    );
}

/// Rollback is the bounded exception (D-6): it refuses a snapshot that was
/// never published.
#[tokio::test]
async fn rollback_refuses_an_unpublished_snapshot() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-rb-refuse");
    let v1 = prompt_config("v1");
    let d1 = published_row(&ctx, &tenant, "agent", v1.clone()).await;
    put_config_as(
        &ctx,
        jwt(AUTHOR_A),
        &tenant,
        "agent",
        json!({"system_prompt": "a draft"}),
    )
    .await;

    let records = versions(&pool, &tenant, "agent").await;
    for source in ["admin_create", "admin_update"] {
        let version = records
            .iter()
            .find(|v| v.change_source == source)
            .unwrap_or_else(|| panic!("a {source} version: {records:?}"))
            .version
            .clone();
        let err = rollback_tenant_agent_version_handler(
            State(ctx.clone()),
            Path((tenant.clone(), "agent".to_string(), version.clone())),
            jwt(ADMIN_C),
        )
        .await
        .expect_err("an unpublished snapshot cannot be promoted");
        assert!(
            matches!(&err.0, AppError::InvalidInput(m) if m.contains("never published")),
            "{source} {version}: {err:?}"
        );
        assert_eq!(err.into_response().status(), StatusCode::BAD_REQUEST);
    }

    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(s.published_digest.as_deref(), Some(d1.as_str()));
    assert_eq!(s.config, v1);
    assert_eq!(served_config(&pool, &tenant, "agent").await, v1);
}

/// Rollback restores the previous digest.
#[tokio::test]
async fn rollback_restores_the_previous_digest() {
    let Some((ctx, pool)) = live_ctx().await else {
        return;
    };
    let tenant = unique("t26a-rb");
    let v1 = prompt_config("v1");
    let d1 = published_row(&ctx, &tenant, "agent", v1.clone()).await;
    put_config_as(
        &ctx,
        jwt(AUTHOR_A),
        &tenant,
        "agent",
        json!({"system_prompt": "v2"}),
    )
    .await;
    let d2 = reviewed_digest(&ctx, &tenant, "agent").await;
    let (status, body) = publish_as(&ctx, jwt(ADMIN_B), &tenant, "agent", &d2).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    assert_ne!(d1, d2);
    assert_eq!(
        served_config(&pool, &tenant, "agent").await["system_prompt"],
        json!("v2")
    );

    let p1 = versions(&pool, &tenant, "agent")
        .await
        .into_iter()
        .find(|v| v.change_source == format!("publish:{d1}"))
        .expect("the first publish's version")
        .version;
    let Json(agent) = rollback_tenant_agent_version_handler(
        State(ctx.clone()),
        Path((tenant.clone(), "agent".to_string(), p1.clone())),
        jwt(ADMIN_C),
    )
    .await
    .expect("a published version can be promoted");

    assert_eq!(agent.config, v1);
    let s = state(&pool, &tenant, "agent").await;
    assert_eq!(
        s.published_digest.as_deref(),
        Some(d1.as_str()),
        "the previous digest is restored"
    );
    assert_eq!(
        s.published_digest.as_deref(),
        Some(
            independent_digest(&pool, &tenant, "agent", "config")
                .await
                .as_str()
        )
    );
    assert_eq!(
        s.approved_by.as_deref(),
        Some(ADMIN_C),
        "the rollback actor approved it"
    );
    assert_eq!(served_config(&pool, &tenant, "agent").await, v1);

    let rollback_versions: Vec<AgentVersionRecord> = versions(&pool, &tenant, "agent")
        .await
        .into_iter()
        .filter(|v| v.change_source == format!("rollback:{p1}"))
        .collect();
    assert_eq!(rollback_versions.len(), 1, "one Rollback version row");
    assert_eq!(
        rollback_versions[0].config_json["published_digest"],
        json!(d1)
    );

    let rows = audit_rows(&pool, "tenant_agent_rollback", &format!("{tenant}:agent")).await;
    assert_eq!(rows.len(), 1, "{rows:?}");
    assert_eq!(rows[0].0.as_deref(), Some(ADMIN_C));
    assert!(
        rows[0].1.as_deref().expect("details").contains(&d1),
        "{rows:?}"
    );
}

// ---------------------------------------------------------------------------
// The cutover migration (D-2, D-3, section 3.4)
// ---------------------------------------------------------------------------

/// On a fresh schema of the scratch database (a copy of the template),
/// migrated to the release before the gate: seed rows, among them one shaped
/// like a production conversational agent and a disabled one, apply the
/// gate's migration, and check that every row is published with the digest
/// computed independently, that the run path still serves every enabled row,
/// and that the cutover config stays a version rollback can promote.
#[tokio::test]
async fn cutover_publishes_every_existing_row_with_the_sql_digest() {
    let Some(url) = common::live_db_url(&common::current_test_name()).await else {
        return;
    };
    let schema = format!("cutover_{}", uuid::Uuid::new_v4().simple());
    let admin = PgPool::connect(&url).await.expect("connect");
    sqlx::query(&format!("CREATE SCHEMA {schema}"))
        .execute(&admin)
        .await
        .expect("create the cutover schema");
    let options = url
        .parse::<PgConnectOptions>()
        .expect("parse the test database URL")
        .options([("search_path", schema.as_str())]);
    let pool = PgPoolOptions::new()
        .max_connections(2)
        .connect_with(options)
        .await
        .expect("connect to the cutover schema");

    let gate: Vec<i64> = ares_store::MIGRATOR
        .iter()
        .filter(|m| m.description.contains("publish gate"))
        .map(|m| m.version)
        .collect();
    assert_eq!(
        gate.len(),
        1,
        "exactly one publish-gate migration: {gate:?}"
    );
    let before_gate = sqlx::migrate::Migrator {
        migrations: Cow::Owned(
            ares_store::MIGRATOR
                .iter()
                .filter(|m| m.version != gate[0])
                .cloned()
                .collect(),
        ),
        ignore_missing: false,
        locking: true,
        no_tx: false,
    };
    before_gate
        .run(&pool)
        .await
        .expect("migrate the schema to the release before the gate");

    let tenant = unique("t26a-cutover");
    let rows: [(&str, Value, bool); 3] = [
        ("conversational-agent", conversational_agent_config(), true),
        (
            "disabled-agent",
            prompt_config("disabled before the cutover"),
            false,
        ),
        ("plain-agent", json!({"model": "fast"}), true),
    ];
    for (name, config, enabled) in &rows {
        sqlx::query(
            "INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, \
             config, enabled, created_at, updated_at) VALUES ($1, $2, $3, $3, NULL, $4, $5, 1, 1)",
        )
        .bind(unique("cutover-row"))
        .bind(&tenant)
        .bind(name)
        .bind(config)
        .bind(enabled)
        .execute(&pool)
        .await
        .expect("seed a pre-gate row");
    }

    ares_store::MIGRATOR
        .run(&pool)
        .await
        .expect("apply the publish-gate migration");

    for (name, config, enabled) in &rows {
        let row = sqlx::query(
            "SELECT config::text AS config_text, published_digest, published_by, approved_by, \
             published_at, draft_config, draft_by, draft_authors, draft_at \
             FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2",
        )
        .bind(&tenant)
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("read the row");
        let digest = row
            .get::<Option<String>, _>("published_digest")
            .unwrap_or_else(|| panic!("{name}: the cutover publishes every row"));
        // Independently, 1: sha256 in Rust over the `config::text` read back.
        assert_eq!(
            digest,
            rust_digest(&row.get::<String, _>("config_text")),
            "{name}"
        );
        // Independently, 2: the D-2 expression written out in its own statement.
        let written_out: String = sqlx::query_scalar(
            "SELECT encode(sha256(convert_to(config::text, 'UTF8')), 'hex') \
             FROM tenant_agents WHERE tenant_id = $1 AND agent_name = $2",
        )
        .bind(&tenant)
        .bind(name)
        .fetch_one(&pool)
        .await
        .expect("written-out digest");
        assert_eq!(digest, written_out, "{name}");
        assert_eq!(
            row.get::<Option<String>, _>("published_by").as_deref(),
            Some(CUTOVER_ACTOR)
        );
        assert_eq!(
            row.get::<Option<String>, _>("approved_by").as_deref(),
            Some(CUTOVER_ACTOR)
        );
        assert!(
            row.get::<Option<i64>, _>("published_at")
                .is_some_and(|t| t > 1_700_000_000),
            "{name}: published_at is now"
        );
        assert!(
            row.get::<Option<Value>, _>("draft_config").is_none(),
            "{name}"
        );
        assert!(row.get::<Option<String>, _>("draft_by").is_none(), "{name}");
        assert!(
            row.get::<Option<Vec<String>>, _>("draft_authors").is_none(),
            "{name}"
        );
        assert!(row.get::<Option<i64>, _>("draft_at").is_none(), "{name}");

        // The cutover's version record carries the digest (D-6).
        let snapshot: Value = sqlx::query_scalar(
            "SELECT config_json FROM agent_config_versions \
             WHERE agent_id = $1 AND version = 'cutover' AND change_source = 'cutover' AND is_active",
        )
        .bind(format!("tenant:{tenant}:{name}"))
        .fetch_one(&pool)
        .await
        .expect("the cutover version record");
        assert_eq!(snapshot["published_digest"], json!(digest), "{name}");
        assert_eq!(&snapshot["tenant_agent"]["config"], config, "{name}");

        // The run path still serves every enabled row, unchanged.
        let now_served = served(&pool, &tenant, name).await;
        if *enabled {
            assert_eq!(
                &now_served.expect("an enabled row keeps running").1,
                config,
                "{name}"
            );
        } else {
            assert!(
                matches!(&now_served, Err(AppError::NotFound(m)) if m.contains("disabled")),
                "{name}: {now_served:?}"
            );
        }
    }

    // Re-enabling the disabled row needs no publish: the cutover published it.
    update_tenant_agent_as(
        &pool,
        &tenant,
        "disabled-agent",
        UpdateTenantAgentRequest {
            display_name: None,
            description: None,
            config: None,
            enabled: Some(true),
        },
        Some(AUTHOR_A),
    )
    .await
    .expect("re-enable");
    assert_eq!(
        served_config(&pool, &tenant, "disabled-agent").await,
        rows[1].1
    );

    // The cutover config stays promotable: publish another, roll back to it.
    let cutover_digest = state(&pool, &tenant, "plain-agent")
        .await
        .published_digest
        .expect("published by the cutover");
    update_tenant_agent_as(
        &pool,
        &tenant,
        "plain-agent",
        UpdateTenantAgentRequest {
            display_name: None,
            description: None,
            config: Some(json!({"model": "slow"})),
            enabled: None,
        },
        Some(AUTHOR_A),
    )
    .await
    .expect("draft");
    let reviewed = state(&pool, &tenant, "plain-agent")
        .await
        .draft_digest
        .expect("a draft");
    match publish_tenant_agent(&pool, &tenant, "plain-agent", &reviewed, ADMIN_B)
        .await
        .expect("publish")
    {
        PublishOutcome::Published(_) => {}
        other => panic!("expected a publish, got {other:?}"),
    }
    assert_eq!(
        served_config(&pool, &tenant, "plain-agent").await["model"],
        json!("slow")
    );
    let rolled_back =
        rollback_tenant_agent_version(&pool, &tenant, "plain-agent", "cutover", ADMIN_C)
            .await
            .expect("roll back to the cutover version");
    assert_eq!(rolled_back.published_digest, cutover_digest);
    assert_eq!(
        served_config(&pool, &tenant, "plain-agent").await,
        rows[2].1
    );

    pool.close().await;
    sqlx::query(&format!("DROP SCHEMA {schema} CASCADE"))
        .execute(&admin)
        .await
        .expect("drop the cutover schema");
}
