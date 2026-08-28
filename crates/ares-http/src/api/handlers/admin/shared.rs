use std::sync::Arc;
use ::cordis::Context;
pub use ares_store::agent_runs;
pub use ares_store::agent_versions;
pub use ares_store::audit_log;
pub use ares_store::schedules as db_schedules;
pub use ares_store::tenants::UsageSummary;
pub use ares_types::models::{Tenant, TenantTier};
pub use ares_types::types::{AppError};
use crate::Result;
use crate::HttpError;
pub use crate::overlay::BillingConfig;
pub use ares_llm::ProviderConfig;
pub use axum::{
    Json,
    extract::{Path, State},
    http::{HeaderMap, StatusCode},
    middleware::Next,
    response::Response,
};
pub use rust_decimal::prelude::ToPrimitive;
pub use serde::{Deserialize, Serialize};
pub use sha2::{Digest, Sha256};
pub use std::collections::HashMap;

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateTenantRequest {
    pub name: String,
    pub tier: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateApiKeyRequest {
    pub name: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct UpdateQuotaRequest {
    pub tier: String,
}

#[derive(Debug, Serialize)]
pub struct TenantResponse {
    pub id: String,
    pub name: String,
    pub tier: String,
    pub created_at: i64,
}

impl From<Tenant> for TenantResponse {
    fn from(t: Tenant) -> Self {
        Self {
            id: t.id,
            name: t.name,
            tier: t.tier.as_str().to_string(),
            created_at: t.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct ApiKeyResponse {
    pub id: String,
    pub tenant_id: String,
    pub key_prefix: String,
    pub name: String,
    pub is_active: bool,
    pub created_at: i64,
}

impl From<ares_types::models::ApiKey> for ApiKeyResponse {
    fn from(k: ares_types::models::ApiKey) -> Self {
        Self {
            id: k.id,
            tenant_id: k.tenant_id,
            key_prefix: k.key_prefix,
            name: k.name,
            is_active: k.is_active,
            created_at: k.created_at,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct UsageResponse {
    pub monthly_requests: u64,
    pub monthly_tokens: u64,
    pub daily_requests: u64,
}

impl From<UsageSummary> for UsageResponse {
    fn from(u: UsageSummary) -> Self {
        Self {
            monthly_requests: u.monthly_requests,
            monthly_tokens: u.monthly_tokens,
            daily_requests: u.daily_requests,
        }
    }
}

const INVALID_TIER_MSG: &str = "Invalid tier. Must be: free, dev, pro, or enterprise";

/// Validates a tier string from admin request payloads.
pub fn parse_tenant_tier(tier: &str) -> Result<TenantTier> {
    TenantTier::from_str(tier).ok_or_else(|| HttpError::from(AppError::InvalidInput(INVALID_TIER_MSG.to_string())))
}

/// Validates that every tool name referenced in an agent config is executable
/// by this tenant: either an enabled built-in tool or a tenant-visible runtime
/// tool. Checks `allowed_tools` first, then falls back to the legacy `tools`
/// field.
pub fn validate_agent_config_tools(
    config: &serde_json::Value,
    tools: &ares_tools::Tools,
    ctx: &Arc<Context>,
    tenant_id: &str,
) -> Result<()> {
    let tool_names: Vec<String> = if let Some(value) = config.get("allowed_tools") {
        parse_agent_config_tool_names("allowed_tools", value)?
    } else if let Some(value) = config.get("tools") {
        parse_agent_config_tool_names("tools", value)?
    } else {
        return Ok(());
    };

    if tool_names.is_empty() {
        return Ok(());
    }

    let scoped = ctx.isolate::<ares_tools::Tools>(tenant_id);
    let mut invalid = Vec::new();
    for name in &tool_names {
        if tools.resolve(&scoped, name).is_none() {
            invalid.push(name.clone());
        }
    }

    if !invalid.is_empty() {
        return Err(HttpError::from(AppError::InvalidInput(format!(
            "Invalid tool name(s): {}. Available tools can be listed via GET /api/admin/runtime-tools",
            invalid.join(", ")
        ).into())));
    }

    Ok(())
}

pub fn parse_agent_config_tool_names(field: &str, value: &serde_json::Value) -> Result<Vec<String>> {
    match value {
        serde_json::Value::Array(values) => values
            .iter()
            .map(|value| {
                value.as_str().map(|name| name.to_string()).ok_or_else(|| {
                    HttpError::from(AppError::InvalidInput(format!(
                        "Agent config field '{field}' must be an array of strings"
                    )))
                })
            })
            .collect(),
        serde_json::Value::Null => Ok(Vec::new()),
        _ => Err(HttpError::from(AppError::InvalidInput(format!(
            "Agent config field '{field}' must be an array"
        ).into()))),
    }
}

// =============================================================================
// Provision Client
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct ProvisionClientRequest {
    pub name: String,
    pub tier: String,
    pub product_type: String,
    pub api_key_name: String,
}

#[derive(Debug, Serialize)]
pub struct ProvisionClientResponse {
    pub tenant_id: String,
    pub tenant_name: String,
    pub tier: String,
    pub product_type: String,
    pub api_key_id: String,
    pub api_key_prefix: String,
    pub raw_api_key: String,
    pub agents_created: Vec<String>,
}

// =============================================================================
// Tenant Agent CRUD
// =============================================================================

// =============================================================================
// Cross-tenant Agent CRUD (admin-facing)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct CreateAgentRequest {
    pub tenant_id: String,
    pub agent_name: String,
    pub display_name: String,
    pub description: Option<String>,
    pub config: serde_json::Value,
    pub template_id: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct UpdateAgentRequest {
    pub display_name: Option<String>,
    pub description: Option<String>,
    pub config: Option<serde_json::Value>,
    pub enabled: Option<bool>,
}

// =============================================================================
// Agent Template CRUD (admin-facing)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct TestTenantAgentRequest {
    pub message: String,
    pub config: serde_json::Value,
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub use_eruka_context: bool,
}

#[derive(Debug, Serialize)]
pub struct TestTenantAgentResponse {
    pub status: String,
    pub response: Option<String>,
    pub error: Option<String>,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub duration_ms: u64,
    pub model_name: Option<String>,
    pub provider_name: Option<String>,
    pub config_source: String,
    pub config_version: String,
    pub workspace_id: Option<String>,
    pub eruka_context_injected: bool,
}

// =============================================================================
// Templates and Models
// =============================================================================

// =============================================================================
// Alerts
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AlertsQuery {
    pub severity: Option<String>,
    pub resolved: Option<bool>,
    pub limit: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ResolveAlertRequest {
    pub resolved_by: Option<String>,
}

// =============================================================================
// Audit Log
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AuditLogQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

// =============================================================================
// Daily Usage
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct DailyUsageQuery {
    pub days: Option<i64>,
}

#[derive(Debug, Serialize)]
pub struct DailyUsageEntry {
    pub date: i64,
    pub requests: i64,
    pub tokens: i64,
}

// =============================================================================
// Agent Runs (Admin view)
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AgentRunsQuery {
    pub limit: Option<i64>,
    pub offset: Option<i64>,
}

/// Query for tenant-agent feedback summaries.
#[derive(Debug, Deserialize)]
pub struct AgentFeedbackSummaryQuery {
    pub days: Option<i64>,
}

/// Request body for recording reviewer quality feedback on one run.
#[derive(Debug, Deserialize)]
pub struct CreateAgentRunFeedbackRequest {
    pub feedback_type: String,
    pub score: Option<f64>,
    #[serde(default)]
    pub flags: Vec<String>,
    pub notes: Option<String>,
    pub reviewer: Option<String>,
}

/// Estimated cost attached to an admin-visible agent run.
#[derive(Debug, Clone, Serialize)]
pub struct CostEstimateResponse {
    /// Currency for the estimate.
    pub currency: String,
    /// Estimated input-token cost in USD, if pricing is configured.
    pub input_cost_usd: Option<f64>,
    /// Estimated output-token cost in USD, if pricing is configured.
    pub output_cost_usd: Option<f64>,
    /// Estimated total cost in USD, if both input and output rates are configured.
    pub total_cost_usd: Option<f64>,
    /// Whether a matching provider/model pricing entry was found.
    pub pricing_known: bool,
}

impl CostEstimateResponse {
    fn unknown() -> Self {
        Self {
            currency: "USD".to_string(),
            input_cost_usd: None,
            output_cost_usd: None,
            total_cost_usd: None,
            pricing_known: false,
        }
    }
}

/// Admin response for one run plus derived operational metrics.
#[derive(Debug, Clone, Serialize)]
pub struct AgentRunResponse {
    /// Raw persisted run fields.
    #[serde(flatten)]
    pub run: agent_runs::AgentRun,
    /// Estimated cost derived from explicit config pricing.
    pub cost_estimate: CostEstimateResponse,
}

impl AgentRunResponse {
    pub fn from_run(run: agent_runs::AgentRun, billing: &BillingConfig) -> Self {
        let cost_estimate = estimate_run_cost(billing, &run);
        Self { run, cost_estimate }
    }
}

pub fn estimate_run_cost(billing: &BillingConfig, run: &agent_runs::AgentRun) -> CostEstimateResponse {
    let Some(pricing) = billing.pricing_for(&run.provider_name, &run.model_name) else {
        return CostEstimateResponse::unknown();
    };

    let input_cost_usd = pricing
        .input_usd_per_million_tokens
        .map(|rate| tokens_to_cost(run.input_tokens, rate));
    let output_cost_usd = pricing
        .output_usd_per_million_tokens
        .map(|rate| tokens_to_cost(run.output_tokens, rate));
    let total_cost_usd = match (input_cost_usd, output_cost_usd) {
        (Some(input), Some(output)) => Some(input + output),
        _ => None,
    };

    CostEstimateResponse {
        currency: pricing.currency.clone(),
        input_cost_usd,
        output_cost_usd,
        total_cost_usd,
        pricing_known: true,
    }
}

pub fn tokens_to_cost(tokens: i64, usd_per_million_tokens: f64) -> f64 {
    (tokens.max(0) as f64 / 1_000_000.0) * usd_per_million_tokens
}

// =============================================================================
// Tenant Allowlists
// =============================================================================

#[derive(Debug, Deserialize)]
pub struct AllowToolRequest {
    pub tool_name: String,
}

#[derive(Debug, Deserialize)]
pub struct AllowModelRequest {
    pub model_id: String,
}

#[derive(Debug, Deserialize)]
pub struct AllowRagSourceRequest {
    pub rag_source: String,
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::handlers::admin::{has_admin_role, AdminClaims, RoleEntry, admin_token_from_request, runtime_tool_capabilities};
    use crate::overlay::ModelPricingConfig;

    fn run(provider_name: &str, model_name: &str) -> agent_runs::AgentRun {
        agent_runs::AgentRun {
            id: "run-1".into(),
            tenant_id: "tenant-1".into(),
            agent_name: "agent-1".into(),
            user_id: None,
            workspace_id: None,
            session_id: None,
            status: "completed".into(),
            input_tokens: 2_000,
            output_tokens: 500,
            duration_ms: 750,
            error: None,
            created_at: 1_700_000_000,
            model_name: model_name.into(),
            provider_name: provider_name.into(),
            is_streaming: false,
            request_source: Some("api_v1_chat".into()),
            product: None,
            agent_config_source: Some("tenant_db".into()),
            agent_config_version: Some("v1".into()),
            eruka_binding_id: None,
            eruka_context_hit: false,
            eruka_read_count: 0,
            eruka_write_count: 0,
            pipeline_id: None,
            schedule_id: None,
            trigger_id: None,
        }
    }

    fn ensure_oauth_state_signing_key() {
        if std::env::var("FLEET_SECRETS_KEY").is_err() {
            std::env::set_var(
                "FLEET_SECRETS_KEY",
                "test-master-key-for-oauth-state-signing-12345",
            );
        }
    }

    fn billing() -> BillingConfig {
        let mut billing = BillingConfig::default();
        billing.model_pricing.insert(
            "gpt_test".into(),
            ModelPricingConfig {
                provider: "openai".into(),
                model: "gpt-test".into(),
                input_usd_per_million_tokens: Some(5.0),
                output_usd_per_million_tokens: Some(15.0),
                currency: "USD".into(),
            },
        );
        billing
    }

    fn run_cost(run_id: &str, created_at: i64) -> RunCost {
        RunCost {
            run_id: run_id.to_string(),
            tenant_id: "tenant-1".to_string(),
            agent_name: "agent-a".to_string(),
            total_llm_calls: 2,
            total_tool_calls: 1,
            total_prompt_tokens: 100,
            total_completion_tokens: 50,
            total_estimated_cost_usd: rust_decimal::Decimal::new(125, 2),
            total_duration_ms: 250,
            created_at,
        }
    }

    struct TestTool;

    #[async_trait::async_trait]
    impl ares_tools::Tool for TestTool {
        fn name(&self) -> &str {
            "builtin_search"
        }

        fn description(&self) -> &str {
            "test built-in tool"
        }

        fn parameters_schema(&self) -> serde_json::Value {
            serde_json::json!({"type": "object"})
        }

        async fn execute(&self, _args: serde_json::Value) -> std::result::Result<serde_json::Value, AppError> {
            Ok(serde_json::json!({"ok": true}))
        }
    }

    #[test]
    fn validate_runtime_tool_execution_config_rejects_invalid_http_config() {
        let err = validate_runtime_tool_execution_config(
            "http",
            &serde_json::json!({"missing_required_url": true}),
        )
        .expect_err("invalid http config should fail")
        .to_string();

        assert!(err.contains("invalid execution_config"));
        assert!(err.contains("Invalid HTTP tool config"));
    }

    #[test]
    fn draft_test_agent_runtime_tools_must_be_attached() {
        let source = include_str!("agents.rs");
        assert!(source.contains("draft_agent.set_tools("));
        assert!(source.contains("ares_agent::tenant_scope"));
    }

    #[tokio::test]
    async fn validate_agent_config_tools_accepts_builtin_tools() {
        let tools = ares_tools::Tools::from_static([
            Arc::new(TestTool) as Arc<dyn ares_tools::Tool>,
        ]);
        let ctx = Context::new_root();
        let config = serde_json::json!({"allowed_tools": ["builtin_search"]});

        validate_agent_config_tools(&config, &tools, &ctx, "tenant-a")
            .expect("built-in allowed tool should validate");
    }

    #[tokio::test]
    async fn validate_agent_config_tools_rejects_unknown_tools() {
        let tools = ares_tools::Tools::from_static(
            [] as [Arc<dyn ares_tools::Tool>; 0],
        );
        let ctx = Context::new_root();
        let config = serde_json::json!({"allowed_tools": ["ghost"]});

        let err = validate_agent_config_tools(&config, &tools, &ctx, "tenant-a")
            .expect_err("unknown tool should fail validation")
            .to_string();
        assert!(err.contains("ghost"));
    }

    #[tokio::test]
    async fn validate_agent_config_tools_supports_legacy_tools_field() {
        let tools = ares_tools::Tools::from_static([
            Arc::new(TestTool) as Arc<dyn ares_tools::Tool>,
        ]);
        let ctx = Context::new_root();
        let config = serde_json::json!({"tools": ["builtin_search"]});

        validate_agent_config_tools(&config, &tools, &ctx, "tenant-a")
            .expect("legacy tools field should validate built-in tools");
    }

    #[tokio::test]
    async fn validate_agent_config_tools_rejects_non_string_allowed_tools() {
        let tools = ares_tools::Tools::from_static(
            [] as [Arc<dyn ares_tools::Tool>; 0],
        );
        let ctx = Context::new_root();
        let config = serde_json::json!({"allowed_tools": ["builtin_search", 7]});

        let err = validate_agent_config_tools(&config, &tools, &ctx, "tenant-a")
            .expect_err("non-string allowed_tools entry should fail validation")
            .to_string();

        assert!(err.contains("allowed_tools"), "got: {err}");
        assert!(err.contains("array of strings"), "got: {err}");
    }

    #[tokio::test]
    async fn validate_agent_config_tools_rejects_non_array_legacy_tools() {
        let tools = ares_tools::Tools::from_static(
            [] as [Arc<dyn ares_tools::Tool>; 0],
        );
        let ctx = Context::new_root();
        let config = serde_json::json!({"tools": "builtin_search"});

        let err = validate_agent_config_tools(&config, &tools, &ctx, "tenant-a")
            .expect_err("non-array tools field should fail validation")
            .to_string();

        assert!(err.contains("tools"), "got: {err}");
        assert!(err.contains("must be an array"), "got: {err}");
    }

    #[test]
    fn required_skill_tenant_id_rejects_missing_or_blank() {
        let missing = HashMap::new();
        assert!(required_skill_tenant_id(&missing).is_err());

        let mut blank = HashMap::new();
        blank.insert("tenant_id".to_string(), "  ".to_string());
        assert!(required_skill_tenant_id(&blank).is_err());

        let mut present = HashMap::new();
        present.insert("tenant_id".to_string(), " tenant-a ".to_string());
        assert_eq!(required_skill_tenant_id(&present).unwrap(), "tenant-a");
    }

    #[test]
    fn run_skill_request_requires_explicit_tenant_id() {
        let err = serde_json::from_value::<RunSkillRequest>(serde_json::json!({
            "skill_id": "skill-1",
            "input": {"x": 1}
        }))
        .expect_err("missing tenant_id should be rejected");
        assert!(err.to_string().contains("tenant_id"));
        assert_eq!(
            normalized_run_skill_tenant_id("tenant-a").unwrap(),
            "tenant-a"
        );
        assert!(normalized_run_skill_tenant_id("  ").is_err());
    }

    #[test]
    fn admin_skill_run_helpers_mark_history_and_active_source() {
        assert_eq!(admin_skill_agent_name("skill-1"), "skill:skill-1");

        let metadata = admin_skill_run_metadata("run-1");
        assert_eq!(metadata.session_id.as_deref(), Some("run-1"));
        assert_eq!(
            metadata.request_source.as_deref(),
            Some(ADMIN_SKILL_RUN_SOURCE)
        );
        assert_eq!(
            metadata.agent_config_source.as_deref(),
            Some(ADMIN_SKILL_CONFIG_SOURCE)
        );

        let active = admin_skill_active_run("run-1", "tenant-a", "skill-1");
        assert_eq!(active.run_id, "run-1");
        assert_eq!(active.tenant_id, "tenant-a");
        assert_eq!(active.agent_name, "skill:skill-1");
        assert_eq!(active.status, "running");
        assert_eq!(active.tool_name.as_deref(), Some("skill:skill-1"));
        assert_eq!(active.model.as_deref(), Some("skill"));
        assert_eq!(
            active.request_source.as_deref(),
            Some(ADMIN_SKILL_RUN_SOURCE)
        );
        assert!(!active.is_catchup);
    }

    fn oauth_credential_fixture() -> ares_store::oauth_credentials::OAuthCredential {
        let encrypted = ares_store::EncryptedPayload {
            ciphertext: vec![1, 2, 3],
            nonce: vec![4, 5, 6],
        };
        ares_store::oauth_credentials::OAuthCredential {
            id: "oauth-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            provider: "google".to_string(),
            connector_type: "gmail".to_string(),
            client_id: "client-id".to_string(),
            client_secret: encrypted.clone(),
            access_token: Some(encrypted.clone()),
            refresh_token: None,
            expires_at: Some(1_700_000_000),
            scope: Some("email".to_string()),
            created_at: 1_700_000_000,
            updated_at: 1_700_000_001,
        }
    }

    #[test]
    fn billing_month_bounds_accepts_valid_month() {
        let (month, start, end) = billing_month_bounds("2026-06").expect("valid month");
        assert_eq!(month, "2026-06");
        assert_eq!(start, 1_780_272_000);
        assert_eq!(end, 1_782_863_999);
    }

    #[test]
    fn billing_summary_sums_run_costs() {
        let costs = vec![
            run_cost("run-b", 1_780_272_010),
            run_cost("run-a", 1_780_272_000),
        ];
        let summary = billing_summary_from_run_costs(
            "tenant-1",
            "2026-06".to_string(),
            1_780_272_000,
            1_782_863_999,
            &costs,
        );
        assert_eq!(summary.total_input_tokens, 200);
        assert_eq!(summary.total_output_tokens, 100);
        assert_eq!(summary.raw_cost_usd, 2.5);
        assert_eq!(summary.billable_cost_usd, 2.5);
        assert_eq!(summary.line_item_count, 2);
    }

    #[test]
    fn billing_line_item_maps_run_cost_without_unit_fields() {
        let item = billing_line_item_from_run_cost(&run_cost("run-1", 1_780_272_000));
        assert_eq!(item.source_type, "agent_run");
        assert_eq!(item.source_id.as_deref(), Some("run-1"));
        assert_eq!(item.input_tokens, 100);
        assert_eq!(item.output_tokens, 50);
        assert_eq!(item.raw_cost_usd, 1.25);
        assert_eq!(item.unit_quantity, 0.0);
    }

    #[test]
    fn model_rates_are_sorted() {
        let rates = model_rate_responses(&billing());
        assert_eq!(rates.len(), 1);
        assert_eq!(rates[0].model_id, "gpt-test");
        assert_eq!(rates[0].input_usd_per_million, 5.0);
    }

    #[test]
    fn oauth_credential_response_redacts_sensitive_payloads() {
        let response = OAuthCredentialResponse::from(oauth_credential_fixture());
        let value = serde_json::to_value(response).expect("serialize oauth response");
        let obj = value.as_object().expect("oauth response object");

        assert!(!obj.contains_key("client_secret"));
        assert!(!obj.contains_key("access_token"));
        assert!(!obj.contains_key("refresh_token"));
        assert_eq!(value["has_access_token"], true);
        assert_eq!(value["has_refresh_token"], false);
    }

    #[test]
    fn normalize_oauth_credential_request_forces_path_tenant_and_connector_provider() {
        let mut req = ares_store::oauth_credentials::CreateOAuthCredentialRequest {
            tenant_id: "body-tenant".to_string(),
            provider: "wrong-provider".to_string(),
            connector_type: "gmail".to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            scope: None,
        };

        normalize_oauth_credential_request("path-tenant".to_string(), &mut req).unwrap();

        assert_eq!(req.tenant_id, "path-tenant");
        assert_eq!(req.provider, "google");
    }

    #[test]
    fn normalize_oauth_credential_request_rejects_unknown_connector() {
        let mut req = ares_store::oauth_credentials::CreateOAuthCredentialRequest {
            tenant_id: "body-tenant".to_string(),
            provider: "provider".to_string(),
            connector_type: "unknown".to_string(),
            client_id: "client".to_string(),
            client_secret: "secret".to_string(),
            access_token: None,
            refresh_token: None,
            expires_at: None,
            scope: None,
        };

        assert!(normalize_oauth_credential_request("path-tenant".to_string(), &mut req).is_err());
    }

    #[test]
    fn oauth_provider_mapping_covers_google_calendar() {
        let provider = oauth_provider_config("google_calendar").unwrap();
        assert_eq!(provider.provider, "google");
        assert_eq!(provider.token_url, "https://oauth2.googleapis.com/token");
        assert!(provider.scope.contains("calendar"));
    }

    #[test]
    fn oauth_provider_mapping_rejects_unknown_connector() {
        assert!(oauth_provider_config("unknown").is_err());
    }

    #[test]
    fn oauth_state_roundtrips_and_sanitizes_redirect() {
        ensure_oauth_state_signing_key();
        let state = OAuthState {
            tenant_id: "tenant 1".into(),
            connector_type: "gmail".into(),
            redirect_uri: "https://evil.example/callback".into(),
        };
        let encoded = encode_oauth_state(&state).unwrap();
        assert!(encoded.contains("&signature="));
        let decoded = decode_oauth_state(&encoded).unwrap();
        assert_eq!(decoded.tenant_id, "tenant 1");
        assert_eq!(decoded.connector_type, "gmail");
        assert_eq!(decoded.redirect_uri, "/connectors");
    }

    #[test]
    fn oauth_state_rejects_tampering_before_decoding_payload() {
        ensure_oauth_state_signing_key();
        let state = OAuthState {
            tenant_id: "tenant-1".into(),
            connector_type: "gmail".into(),
            redirect_uri: "/connectors".into(),
        };
        let tampered = encode_oauth_state(&state)
            .unwrap()
            .replace("tenant-1", "tenant-2");

        assert!(decode_oauth_state(&tampered).is_err());
    }

    #[test]
    fn safe_callback_redirect_uri_allows_relative_only() {
        assert_eq!(
            safe_callback_redirect_uri("/connectors/callback"),
            "/connectors/callback"
        );
        assert_eq!(
            safe_callback_redirect_uri("//evil.example/path"),
            "/connectors"
        );
        assert_eq!(
            safe_callback_redirect_uri("https://evil.example/path"),
            "/connectors"
        );
        assert_eq!(safe_callback_redirect_uri("/bad\\path"), "/connectors");
    }

    #[test]
    fn build_authorize_url_contains_encoded_oauth_fields() {
        ensure_oauth_state_signing_key();
        let provider = oauth_provider_config("slack").unwrap();
        let state = OAuthState {
            tenant_id: "tenant-1".into(),
            connector_type: "slack".into(),
            redirect_uri: "/connectors".into(),
        };
        let url = build_authorize_url(
            provider,
            "client id",
            "https://app.example/api/oauth/callback",
            &state,
        )
        .unwrap();
        assert!(url.starts_with("https://slack.com/oauth/v2/authorize?"));
        assert!(url.contains("client_id=client%20id"));
        assert!(url.contains("redirect_uri=https%3A%2F%2Fapp.example%2Fapi%2Foauth%2Fcallback"));
        assert!(url.contains(
            "state=tenant_id%3Dtenant-1%26connector_type%3Dslack%26redirect_uri%3D%252Fconnectors"
        ));
    }

    #[test]
    fn build_token_form_uses_authorization_code_grant() {
        let form = build_token_form(
            "code",
            "client",
            "secret",
            "https://app.example/api/oauth/callback",
        );
        assert_eq!(form[0], ("grant_type", "authorization_code"));
        assert!(form.contains(&("code", "code")));
        assert!(form.contains(&("client_secret", "secret")));
    }

    #[test]
    fn oauth_stored_scope_preserves_instance_url_metadata() {
        let scope = oauth_stored_scope(
            "api refresh_token",
            Some("api"),
            Some("https://acme.my.salesforce.com"),
        );
        assert_eq!(scope, "api instance_url=https://acme.my.salesforce.com");
        assert_eq!(
            oauth_stored_scope("fallback", None, Some("https://acme.my.salesforce.com")),
            "fallback instance_url=https://acme.my.salesforce.com"
        );
        assert_eq!(
            oauth_stored_scope("fallback", Some("api"), Some("javascript:bad")),
            "api"
        );
    }

    #[test]
    fn estimate_run_cost_returns_unknown_without_matching_pricing() {
        let estimate = estimate_run_cost(&BillingConfig::default(), &run("openai", "gpt-test"));

        assert!(!estimate.pricing_known);
        assert_eq!(estimate.total_cost_usd, None);
    }

    #[test]
    fn estimate_run_cost_uses_configured_provider_model_pricing() {
        let estimate = estimate_run_cost(&billing(), &run(" OpenAI ", "GPT-Test"));

        assert!(estimate.pricing_known);
        assert_eq!(estimate.input_cost_usd, Some(0.01));
        assert_eq!(estimate.output_cost_usd, Some(0.0075));
        assert_eq!(estimate.total_cost_usd, Some(0.0175));
    }

    fn admin_claims(roles: HashMap<String, Vec<RoleEntry>>) -> AdminClaims {
        AdminClaims {
            sub: "admin-user".into(),
            email: "admin@example.com".into(),
            exp: 9_999_999_999,
            iat: 1_700_000_000,
            roles,
        }
    }

    fn role_entry(role: &str) -> RoleEntry {
        RoleEntry {
            role: role.into(),
            resource_id: None,
        }
    }

    #[test]
    fn admin_token_from_request_decodes_query_token() {
        let req = axum::extract::Request::builder()
            .uri("/api/admin/runs/live?token=header%2Bpayload%2Fsig%3D")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            admin_token_from_request(&req).as_deref(),
            Some("header+payload/sig=")
        );
    }

    #[test]
    fn admin_token_from_request_prefers_authorization_header() {
        let req = axum::extract::Request::builder()
            .uri("/api/admin/runs/live?token=query-token")
            .header("authorization", "Bearer header-token")
            .body(axum::body::Body::empty())
            .unwrap();
        assert_eq!(
            admin_token_from_request(&req).as_deref(),
            Some("header-token")
        );
    }

    #[test]
    fn has_admin_role_accepts_super_admin_in_ares_product() {
        let mut roles = HashMap::new();
        roles.insert("ares".into(), vec![role_entry("super_admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_accepts_admin_in_eruka_product() {
        let mut roles = HashMap::new();
        roles.insert("eruka".into(), vec![role_entry("admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_rejects_non_admin_roles() {
        let mut roles = HashMap::new();
        roles.insert("admin".into(), vec![role_entry("viewer")]);
        assert!(!has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn tokens_to_cost_clamps_negative_tokens_to_zero() {
        assert_eq!(tokens_to_cost(-100, 10.0), 0.0);
    }

    #[test]
    fn tokens_to_cost_scales_per_million() {
        let cost = tokens_to_cost(2_000_000, 5.0);
        assert!((cost - 10.0).abs() < f64::EPSILON);
    }

    #[test]
    fn cost_estimate_unknown_serializes_pricing_flag() {
        let estimate = CostEstimateResponse::unknown();
        let json = serde_json::to_value(&estimate).unwrap();
        assert_eq!(json["pricing_known"], false);
        assert!(json["total_cost_usd"].is_null());
    }

    #[test]
    fn agent_run_response_from_run_attaches_cost_estimate() {
        let response = AgentRunResponse::from_run(run("openai", "gpt-test"), &billing());
        assert!(response.cost_estimate.pricing_known);
        assert_eq!(response.run.id, "run-1");
    }

    fn trigger(event_type: &str, enabled: bool) -> db_schedules::EventTrigger {
        db_schedules::EventTrigger {
            id: "trigger-1".to_string(),
            tenant_id: "tenant-1".to_string(),
            name: "Trigger".to_string(),
            event_type: event_type.to_string(),
            event_config: serde_json::json!({}),
            target_agent: "agent-1".to_string(),
            enabled,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_000,
        }
    }

    #[test]
    fn webhook_trigger_matches_webhook_type_only() {
        assert!(webhook_trigger_matches(&trigger("webhook", true)));
        assert!(webhook_trigger_matches(&trigger("webhook", false)));
        assert!(!webhook_trigger_matches(&trigger("document_upload", true)));
        assert!(!webhook_trigger_matches(&trigger("field_change", true)));
    }

    #[test]
    fn tenant_trigger_update_handler_audits_update_action() {
        let source = include_str!("triggers.rs");
        assert!(source.contains("pub async fn update_tenant_trigger"));
        assert!(source.contains("trigger_update"));
        assert!(source.contains("trigger {id} not found for tenant {tenant_id}"));
    }

    #[test]
    fn create_tenant_request_roundtrip() {
        let req = CreateTenantRequest {
            name: "Acme".into(),
            tier: "pro".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CreateTenantRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Acme");
        assert_eq!(back.tier, "pro");
    }

    #[test]
    fn tenant_response_from_tenant_maps_tier_string() {
        use ares_types::models::{Tenant, TenantTier};
        let tenant = Tenant::new("t-1".into(), "Acme".into(), TenantTier::Pro);
        let resp = TenantResponse::from(tenant);
        assert_eq!(resp.id, "t-1");
        assert_eq!(resp.tier, "pro");
    }

    #[test]
    fn api_key_response_from_model_maps_prefix() {
        use ares_types::models::ApiKey;
        let key = ApiKey::new(
            "key-1".into(),
            "tenant-1".into(),
            "hash".into(),
            "ares_ab".into(),
            "Primary".into(),
        );
        let resp = ApiKeyResponse::from(key);
        assert_eq!(resp.key_prefix, "ares_ab");
        assert!(resp.is_active);
    }

    #[test]
    fn usage_response_from_summary_copies_counters() {
        let summary = UsageSummary {
            monthly_requests: 10,
            monthly_tokens: 20,
            daily_requests: 3,
        };
        let resp = UsageResponse::from(summary);
        assert_eq!(resp.monthly_requests, 10);
        assert_eq!(resp.monthly_tokens, 20);
        assert_eq!(resp.daily_requests, 3);
    }

    #[test]
    fn emergency_stop_status_describes_global_agent_entrypoints() {
        let status = emergency_stop_status(true);
        assert!(status.emergency_stop);
        assert!(status.message.contains("Agent execution entrypoints"));
        assert!(!status.message.contains("/api/v1/chat"));
    }

    #[test]
    fn emergency_stop_request_deserializes_active_flag() {
        let req: EmergencyStopRequest = serde_json::from_str(r#"{"active":true}"#).unwrap();
        assert!(req.active);
        let status = emergency_stop_status(true);
        assert!(status.emergency_stop);
        assert!(status.message.contains("emergency stop mode"));
    }

    #[test]
    fn create_api_key_request_roundtrip() {
        let req = CreateApiKeyRequest {
            name: "Primary".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: CreateApiKeyRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.name, "Primary");
    }

    #[test]
    fn update_quota_request_roundtrip() {
        let req = UpdateQuotaRequest {
            tier: "enterprise".into(),
        };
        let json = serde_json::to_string(&req).unwrap();
        let back: UpdateQuotaRequest = serde_json::from_str(&json).unwrap();
        assert_eq!(back.tier, "enterprise");
    }

    #[test]
    fn admin_claims_deserializes_roles_map() {
        let json = r#"{
            "sub":"user-1",
            "email":"admin@example.com",
            "exp":9999999999,
            "iat":1700000000,
            "roles":{
                "ares":[{"role":"admin","resource_id":null}]
            }
        }"#;
        let claims: AdminClaims = serde_json::from_str(json).unwrap();
        assert_eq!(claims.sub, "user-1");
        assert_eq!(claims.email, "admin@example.com");
        assert!(has_admin_role(&claims));
    }

    #[test]
    fn admin_claims_default_empty_roles_when_omitted() {
        let json = r#"{
            "sub":"user-2",
            "email":"viewer@example.com",
            "exp":9999999999,
            "iat":1700000000
        }"#;
        let claims: AdminClaims = serde_json::from_str(json).unwrap();
        assert!(claims.roles.is_empty());
        assert!(!has_admin_role(&claims));
    }

    #[test]
    fn has_admin_role_accepts_admin_in_admin_product() {
        let mut roles = HashMap::new();
        roles.insert("admin".into(), vec![role_entry("admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn parse_tenant_tier_accepts_case_insensitive_values() {
        assert!(matches!(parse_tenant_tier("PRO").unwrap(), TenantTier::Pro));
        assert!(matches!(
            parse_tenant_tier("Enterprise").unwrap(),
            TenantTier::Enterprise
        ));
    }

    #[test]
    fn parse_tenant_tier_rejects_unknown_tier_with_invalid_input() {
        let err = parse_tenant_tier("platinum").unwrap_err();
        assert!(matches!(err.0, AppError::InvalidInput(_)));
        assert!(err.to_string().contains(INVALID_TIER_MSG));
    }

    #[test]
    fn tokens_to_cost_zero_tokens_returns_zero() {
        assert_eq!(tokens_to_cost(0, 99.0), 0.0);
    }

    #[test]
    fn estimate_run_cost_uses_pricing_currency() {
        let estimate = estimate_run_cost(&billing(), &run("openai", "gpt-test"));
        assert_eq!(estimate.currency, "USD");
    }

    #[test]
    fn estimate_run_cost_partial_input_only_pricing_has_no_total() {
        let mut billing = BillingConfig::default();
        billing.model_pricing.insert(
            "partial".into(),
            ModelPricingConfig {
                provider: "openai".into(),
                model: "gpt-partial".into(),
                input_usd_per_million_tokens: Some(5.0),
                output_usd_per_million_tokens: None,
                currency: "EUR".into(),
            },
        );
        let mut r = run("openai", "gpt-partial");
        r.input_tokens = 1_000_000;
        r.output_tokens = 0;
        let estimate = estimate_run_cost(&billing, &r);
        assert!(estimate.pricing_known);
        assert_eq!(estimate.currency, "EUR");
        assert_eq!(estimate.input_cost_usd, Some(5.0));
        assert_eq!(estimate.output_cost_usd, None);
        assert_eq!(estimate.total_cost_usd, None);
    }

    #[test]
    fn has_admin_role_rejects_empty_roles_map() {
        assert!(!has_admin_role(&admin_claims(HashMap::new())));
    }

    #[test]
    fn has_admin_role_rejects_admin_in_unlisted_product() {
        let mut roles = HashMap::new();
        roles.insert("other".into(), vec![role_entry("admin")]);
        assert!(!has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_accepts_super_admin_in_admin_product() {
        let mut roles = HashMap::new();
        roles.insert("admin".into(), vec![role_entry("super_admin")]);
        assert!(has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn has_admin_role_rejects_multiple_non_admin_roles() {
        let mut roles = HashMap::new();
        roles.insert(
            "ares".into(),
            vec![role_entry("viewer"), role_entry("editor")],
        );
        assert!(!has_admin_role(&admin_claims(roles)));
    }

    #[test]
    fn parse_tenant_tier_accepts_free_and_dev() {
        assert!(matches!(
            parse_tenant_tier("free").unwrap(),
            TenantTier::Free
        ));
        assert!(matches!(parse_tenant_tier("DEV").unwrap(), TenantTier::Dev));
    }

    #[test]
    fn provision_client_request_deserializes() {
        let req: ProvisionClientRequest = serde_json::from_str(
            r#"{"name":"Acme","tier":"pro","product_type":"ares","api_key_name":"bootstrap"}"#,
        )
        .unwrap();
        assert_eq!(req.name, "Acme");
        assert_eq!(req.tier, "pro");
        assert_eq!(req.product_type, "ares");
        assert_eq!(req.api_key_name, "bootstrap");
    }

    #[test]
    fn test_tenant_agent_request_deserializes_defaults() {
        let req: TestTenantAgentRequest =
            serde_json::from_str(r#"{"message":"hi","config":{"model":"gpt-4"}}"#).unwrap();
        assert_eq!(req.message, "hi");
        assert!(!req.use_eruka_context);
        assert!(req.workspace_id.is_none());
    }

    #[test]
    fn tenant_response_serializes_fields() {
        let resp = TenantResponse {
            id: "t1".into(),
            name: "Acme".into(),
            tier: "enterprise".into(),
            created_at: 1_700_000_000,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["id"], "t1");
        assert_eq!(json["tier"], "enterprise");
        assert_eq!(json["created_at"], 1_700_000_000);
    }

    #[test]
    fn daily_usage_query_deserializes_optional_days() {
        let q: DailyUsageQuery = serde_json::from_str(r#"{"days":7}"#).unwrap();
        assert_eq!(q.days, Some(7));
        let default: DailyUsageQuery = serde_json::from_str("{}").unwrap();
        assert!(default.days.is_none());
    }

    #[test]
    fn daily_usage_entry_serializes_counters() {
        let entry = DailyUsageEntry {
            date: 1_700_000_000,
            requests: 5,
            tokens: 100,
        };
        let json = serde_json::to_value(&entry).unwrap();
        assert_eq!(json["requests"], 5);
        assert_eq!(json["tokens"], 100);
    }

    #[test]
    fn agent_runs_query_deserializes_pagination() {
        let q: AgentRunsQuery = serde_json::from_str(r#"{"limit":25,"offset":10}"#).unwrap();
        assert_eq!(q.limit, Some(25));
        assert_eq!(q.offset, Some(10));
    }

    #[test]
    fn agent_feedback_summary_query_deserializes_days() {
        let q: AgentFeedbackSummaryQuery = serde_json::from_str(r#"{"days":14}"#).unwrap();
        assert_eq!(q.days, Some(14));
    }

    #[test]
    fn create_agent_run_feedback_request_deserializes_with_defaults() {
        let req: CreateAgentRunFeedbackRequest =
            serde_json::from_str(r#"{"feedback_type":"quality","score":4.5}"#).unwrap();
        assert_eq!(req.feedback_type, "quality");
        assert_eq!(req.score, Some(4.5));
        assert!(req.flags.is_empty());
        assert!(req.notes.is_none());
    }

    #[test]
    fn alerts_query_deserializes_filters() {
        let q: AlertsQuery =
            serde_json::from_str(r#"{"severity":"critical","resolved":false,"limit":10}"#).unwrap();
        assert_eq!(q.severity.as_deref(), Some("critical"));
        assert_eq!(q.resolved, Some(false));
        assert_eq!(q.limit, Some(10));
    }

    #[test]
    fn audit_log_query_deserializes_pagination() {
        let q: AuditLogQuery = serde_json::from_str(r#"{"limit":100,"offset":50}"#).unwrap();
        assert_eq!(q.limit, Some(100));
        assert_eq!(q.offset, Some(50));
    }

    #[test]
    fn resolve_alert_request_deserializes_optional_reviewer() {
        let req: ResolveAlertRequest = serde_json::from_str(r#"{"resolved_by":"alice"}"#).unwrap();
        assert_eq!(req.resolved_by.as_deref(), Some("alice"));
    }

    #[test]
    fn emergency_stop_request_deserializes_inactive_flag() {
        let req: EmergencyStopRequest = serde_json::from_str(r#"{"active":false}"#).unwrap();
        assert!(!req.active);
    }

    #[test]
    fn cost_estimate_known_serializes_cost_fields() {
        let estimate = estimate_run_cost(&billing(), &run("openai", "gpt-test"));
        let json = serde_json::to_value(&estimate).unwrap();
        assert_eq!(json["pricing_known"], true);
        assert_eq!(json["currency"], "USD");
        assert!(json["total_cost_usd"].is_number());
    }

    #[test]
    fn agent_run_response_serializes_nested_run() {
        let response = AgentRunResponse::from_run(run("openai", "gpt-test"), &billing());
        let json = serde_json::to_value(&response).unwrap();
        assert_eq!(json["id"], "run-1");
        assert_eq!(json["cost_estimate"]["pricing_known"], true);
    }

    #[test]
    fn usage_response_serializes_counters() {
        let resp = UsageResponse::from(UsageSummary {
            monthly_requests: 1,
            monthly_tokens: 2,
            daily_requests: 3,
        });
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["monthly_requests"], 1);
        assert_eq!(json["daily_requests"], 3);
    }

    #[test]
    fn api_key_response_serializes_fields() {
        use ares_types::models::ApiKey;
        let key = ApiKey::new(
            "k".into(),
            "t".into(),
            "hash".into(),
            "prefix".into(),
            "name".into(),
        );
        let json = serde_json::to_value(ApiKeyResponse::from(key)).unwrap();
        assert_eq!(json["key_prefix"], "prefix");
        assert_eq!(json["is_active"], true);
    }

    #[test]
    fn tenant_response_from_tenant_maps_free_dev_enterprise() {
        use ares_types::models::Tenant;
        for (tier, expected) in [
            (TenantTier::Free, "free"),
            (TenantTier::Dev, "dev"),
            (TenantTier::Enterprise, "enterprise"),
        ] {
            let tenant = Tenant::new("id".into(), "n".into(), tier);
            assert_eq!(TenantResponse::from(tenant).tier, expected);
        }
    }

    #[test]
    fn test_tenant_agent_response_serializes_status() {
        let resp = TestTenantAgentResponse {
            status: "ok".into(),
            response: Some("hello".into()),
            error: None,
            input_tokens: 1,
            output_tokens: 2,
            duration_ms: 3,
            model_name: Some("gpt".into()),
            provider_name: Some("openai".into()),
            config_source: "tenant_db".into(),
            config_version: "v1".into(),
            workspace_id: None,
            eruka_context_injected: false,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["status"], "ok");
        assert_eq!(json["eruka_context_injected"], false);
    }

    #[test]
    fn provision_client_response_serializes() {
        let resp = ProvisionClientResponse {
            tenant_id: "t".into(),
            tenant_name: "Acme".into(),
            tier: "pro".into(),
            product_type: "ares".into(),
            api_key_id: "key".into(),
            api_key_prefix: "ares_".into(),
            raw_api_key: "secret".into(),
            agents_created: vec!["a1".into()],
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["agents_created"][0], "a1");
    }

    #[test]
    fn role_entry_deserializes_resource_id() {
        let entry: RoleEntry =
            serde_json::from_str(r#"{"role":"admin","resource_id":"res-1"}"#).unwrap();
        assert_eq!(entry.role, "admin");
        assert_eq!(entry.resource_id.as_deref(), Some("res-1"));
    }

    #[test]
    fn invalid_tier_message_lists_allowed_values() {
        assert!(INVALID_TIER_MSG.contains("free"));
        assert!(INVALID_TIER_MSG.contains("enterprise"));
    }

    #[test]
    fn app_error_not_found_serializes_for_admin_handlers() {
        let err = AppError::NotFound("Tenant not found".to_string());
        assert!(matches!(err, AppError::NotFound(_)));
        assert!(err.to_string().contains("Tenant not found"));
    }

    #[test]
    fn runtime_provider_response_serializes_correctly() {
        let resp = RuntimeProviderResponse {
            id: "test-id".into(),
            tenant_id: None,
            name: "test-provider".into(),
            display_name: "Test Provider".into(),
            provider_type: "openai-compatible".into(),
            api_base: "https://api.openai.com".into(),
            auth_type: "api_key".into(),
            default_model: Some("gpt-4".into()),
            headers: None,
            request_transform: None,
            response_transform: None,
            enabled: true,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_001,
        };
        let json = serde_json::to_value(&resp).unwrap();
        assert_eq!(json["name"], "test-provider");
        assert_eq!(json["provider_type"], "openai-compatible");
        assert_eq!(json["auth_type"], "api_key");
        assert_eq!(json["enabled"], true);
        assert_eq!(json["created_at"], 1_700_000_000);
    }

    #[tokio::test]
    async fn runtime_tool_capabilities_use_storage_contract() {
        let Json(capabilities) = runtime_tool_capabilities().await;
        assert_eq!(
            capabilities.tool_types,
            vec!["http", "mcp", "script", "sql"]
        );
    }

    #[test]
    fn runtime_provider_response_redacts_direct_api_key() {
        let provider = ares_store::runtime_providers::RuntimeProvider {
            id: "test-id".into(),
            tenant_id: None,
            name: "test-provider".into(),
            display_name: "Test Provider".into(),
            provider_type: "openai-compatible".into(),
            api_base: "https://api.openai.com".into(),
            auth_type: "api_key".into(),
            default_model: Some("gpt-4".into()),
            headers: Some(serde_json::json!({
                "api_key": "secret-key",
                "region": "us-east-1"
            })),
            request_transform: None,
            response_transform: None,
            enabled: true,
            created_at: 1_700_000_000,
            updated_at: 1_700_000_001,
        };

        let response = RuntimeProviderResponse::from(provider);
        let headers = response.headers.expect("headers");

        assert_eq!(headers["api_key"], RUNTIME_PROVIDER_SECRET_REDACTION);
        assert_eq!(headers["region"], "us-east-1");
    }

    #[test]
    fn runtime_provider_detects_redacted_api_key() {
        let headers = serde_json::json!({
            "api_key": RUNTIME_PROVIDER_SECRET_REDACTION,
            "region": "us-east-1"
        });

        assert!(runtime_provider_headers_have_redacted_api_key(Some(
            &headers
        )));
        assert_eq!(
            runtime_provider_header_value(Some(&headers), "api_key").as_deref(),
            Some(RUNTIME_PROVIDER_SECRET_REDACTION)
        );
    }

    #[test]
    fn runtime_provider_entry_headers_resolve_api_key_env() {
        let env_name = "ARES_TEST_RUNTIME_PROVIDER_API_KEY";
        std::env::set_var(env_name, "resolved-test-key");
        let headers = serde_json::json!({
            "api_key_env": env_name,
            "api-version": "2024-02-01"
        });

        let (headers, api_key) = runtime_provider_entry_headers_and_key(Some(&headers));

        std::env::remove_var(env_name);
        assert_eq!(api_key.as_deref(), Some("resolved-test-key"));
        assert_eq!(
            headers.get("api-version").map(String::as_str),
            Some("2024-02-01")
        );
        assert!(!headers.contains_key("api_key_env"));
    }

    #[test]
    fn runtime_provider_entry_headers_accept_direct_api_key() {
        let headers = serde_json::json!({
            "api_key": "direct-test-key",
            "region": "us-east-1"
        });

        let (headers, api_key) = runtime_provider_entry_headers_and_key(Some(&headers));

        assert_eq!(api_key.as_deref(), Some("direct-test-key"));
        assert_eq!(headers.get("region").map(String::as_str), Some("us-east-1"));
        assert!(!headers.contains_key("api_key"));
    }
}

// =============================================================================
// Cross-tenant agents list
// =============================================================================

// =============================================================================
// Platform Stats
// =============================================================================

// =============================================================================
// Agent Versioning — Rollback + Kill Switch (Sprint 12)
// =============================================================================

/// List all recorded versions for a TOON agent (most recent first).
pub async fn list_agent_versions_handler(
    State(ctx): State<Arc<Context>>,
    Path(agent_id): Path<String>,
) -> Result<Json<Vec<agent_versions::AgentVersionRecord>>> {
    let __pool_1 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let records = agent_versions::get_agent_version_history(&__pool_1, &agent_id, 50)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    Ok(Json(records))
}

/// Restore a TOON agent to a specific previously-recorded version.
/// Hot-swaps the in-memory config; writes a new "rollback" row to agent_config_versions.
pub async fn rollback_agent_handler(
    State(ctx): State<Arc<Context>>,
    Path((agent_id, version)): Path<(String, String)>,
) -> Result<Json<serde_json::Value>> {
    // Fetch the target version from DB
    let __pool_2 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let history = agent_versions::get_agent_version_history(&__pool_2, &agent_id, 100)
        .await
        .map_err(|e| AppError::Database(e.to_string()))?;

    let record = history
        .into_iter()
        .find(|r| r.version == version)
        .ok_or_else(|| {
            AppError::NotFound(format!(
                "No version '{}' found for agent '{}'",
                version, agent_id
            ))
        })?;

    // Deserialize config_json back to ToonAgentConfig
    let agent_config: crate::toon_config::ToonAgentConfig =
        serde_json::from_value(record.config_json).map_err(|e| {
            AppError::InvalidInput(format!("Failed to deserialize agent config: {}", e))
        })?;

    // Hot-swap into the in-memory DynamicConfigManager
    ctx.get::<crate::toon_config::DynamicConfigManager>().expect("not provided").upsert_agent(agent_config.clone());

    // Record the rollback as a new version entry
    let pool = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let input = ares_store::AgentVersionInput {
        name: agent_config.name.clone(),
        version: agent_config.version.clone(),
        config_json: serde_json::to_value(&agent_config)
            .unwrap_or_else(|_| serde_json::json!({"name": agent_config.name})),
    };
    let _ = agent_versions::record_agent_versions(&pool, &[input], "rollback").await;

    // Audit log
    let __pool_3 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let aid = agent_id.clone();
    let ver = version.clone();
    tokio::spawn(async move {
        let _ = audit_log::log_admin_action(
            &__pool_3,
            "agent_rollback",
            "agent",
            &aid,
            Some(&format!("Rolled back to version {}", ver)),
            None,
        )
        .await;
    });

    tracing::info!(agent_id = %agent_id, version = %version, "Agent rolled back");

    Ok(Json(serde_json::json!({
        "agent_id": agent_id,
        "version": version,
        "status": "rolled_back"
    })))
}

#[derive(Debug, Deserialize)]
pub struct EmergencyStopRequest {
    pub active: bool,
}

#[derive(Debug, serde::Serialize)]
pub struct EmergencyStopStatus {
    pub emergency_stop: bool,
    pub message: &'static str,
}

pub fn emergency_stop_status(active: bool) -> EmergencyStopStatus {
    EmergencyStopStatus {
        emergency_stop: active,
        message: if active {
            "All agents are now in emergency stop mode. Agent execution entrypoints will be rejected with 503."
        } else {
            "Emergency stop cleared. Agents are operational."
        },
    }
}

// =============================================================================
// Fleet Provider Secrets (encrypted at rest, hot-swap in memory)
// =============================================================================

pub use ares_store::fleet_provider_secrets as fps;
pub use ares_store::{MasterKey, decrypt_api_key, last_n_visible};

/// ]
/// ```
pub async fn list_fleet_providers(
    State(ctx): State<Arc<Context>>,
) -> Result<Json<Vec<serde_json::Value>>> {
    let __pool_4 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = fps::FleetProviderSecretsStore::new(&__pool_4);
    let master = MasterKey::from_env();
    // Decrypt all rows so the UI can show the last-4 of the stored key.
    // Decryption errors on individual rows are logged and skipped inside
    // `load_all`, so this call never fails because of a single bad row.
    let map = store.load_all(master.as_ref()).await?;

    let mut out: Vec<serde_json::Value> = map
        .into_iter()
        .map(|(name, entry)| {
            let api_key_last4 = entry.api_key.as_deref().and_then(|k| last_n_visible(k, 4));
            serde_json::json!({
                "name": name,
                "provider_type": name,
                "has_api_key": entry.api_key.is_some(),
                "api_key_last4": api_key_last4,
                "api_base": entry.api_base,
                "default_model": entry.default_model,
                "fallback_providers": entry.fallback_providers,
                "updated_at": entry.updated_at,
                "updated_by": entry.updated_by,
            })
        })
        .collect();
    out.sort_by(|a, b| {
        a.get("name")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .cmp(b.get("name").and_then(|v| v.as_str()).unwrap_or(""))
    });
    Ok(Json(out))
}

#[derive(Debug, Deserialize)]
pub struct FleetProviderUpsertRequest {
    /// Optional. If `None`, the existing key is preserved.
    pub api_key: Option<String>,
    /// Optional. If `None`, the existing api_base is preserved.
    pub api_base: Option<String>,
    /// Optional. If `None`, the existing default_model is preserved.
    pub default_model: Option<String>,
    /// Optional. If `None`, the existing fallback_providers list is preserved.
    /// Fallback providers are resolved after the primary fails.
    pub fallback_providers: Option<Vec<String>>,
}

#[derive(Debug, Serialize)]
pub struct FleetProviderVerifyResponse {
    pub name: String,
    pub ok: bool,
    pub latency_ms: u64,
    pub model_count: usize,
    pub models: Vec<String>,
    pub error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct FleetProviderCapabilities {
    /// Provider type identifiers supported by this build.
    pub providers: Vec<&'static str>,
    /// Whether fleet-secrets encryption is enabled (FLEET_SECRETS_KEY set).
    pub encryption_enabled: bool,
}

pub fn default_api_base(pc: &ProviderConfig) -> Option<String> {
    match pc {
        ProviderConfig::OpenAI { api_base, .. } => Some(api_base.clone()),
        ProviderConfig::Azure { base_url_env, .. } => std::env::var(base_url_env).ok(),
        ProviderConfig::Anthropic { .. } => Some("https://api.anthropic.com".to_string()),
        ProviderConfig::Bedrock { region_env, .. } => std::env::var(region_env)
            .ok()
            .map(|region| format!("https://bedrock-runtime.{region}.amazonaws.com")),
        ProviderConfig::Ollama { base_url, .. } => Some(base_url.clone()),
        _ => None,
    }
}

pub fn resolve_env_key(pc: &ProviderConfig) -> Option<String> {
    let var = match pc {
        ProviderConfig::OpenAI { api_key_env, .. } => api_key_env,
        ProviderConfig::Azure { api_key_env, .. } => api_key_env,
        ProviderConfig::Anthropic { api_key_env, .. } => api_key_env,
        ProviderConfig::Bedrock { api_key_env, .. } => api_key_env,
        ProviderConfig::Ollama { api_key_env, .. } => api_key_env,
        _ => return None,
    };
    std::env::var(var).ok()
}

// =============================================================================
// Runtime Tools (CRUD + versions + rollback + test)
// =============================================================================

pub use ares_store::runtime_tools::{
    CreateRuntimeToolRequest, RuntimeToolStore, UpdateRuntimeToolRequest,
    validate_runtime_tool_update_scope_preflight,
};

pub fn validate_runtime_tool_execution_config(
    tool_type: &str,
    execution_config: &serde_json::Value,
) -> Result<()> {
    ares_tools::Tools::validate_runtime_tool_execution_config(tool_type, execution_config)
    .map_err(|e| HttpError::from(AppError::InvalidInput(format!("invalid execution_config: {e}"))))
}

#[derive(Debug, Deserialize)]
pub struct TestRuntimeToolRequest {
    /// JSON arguments passed to the tool's `execute` method.
    pub input_args: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct TestRuntimeToolResponse {
    pub ok: bool,
    pub output: Option<serde_json::Value>,
    pub error: Option<String>,
    pub latency_ms: u64,
}

// =============================================================================
// Runtime Providers
// =============================================================================

pub use ares_store::runtime_providers::{CreateRuntimeProviderRequest, RuntimeProviderStore};

const RUNTIME_PROVIDER_SECRET_REDACTION: &str = "********";

#[derive(Debug, Serialize)]
pub struct RuntimeToolCapabilitiesResponse {
    pub tool_types: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct RuntimeProviderResponse {
    pub id: String,
    pub tenant_id: Option<String>,
    pub name: String,
    pub display_name: String,
    pub provider_type: String,
    pub api_base: String,
    pub auth_type: String,
    pub default_model: Option<String>,
    pub headers: Option<serde_json::Value>,
    pub request_transform: Option<serde_json::Value>,
    pub response_transform: Option<serde_json::Value>,
    pub enabled: bool,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<ares_store::runtime_providers::RuntimeProvider> for RuntimeProviderResponse {
    fn from(p: ares_store::runtime_providers::RuntimeProvider) -> Self {
        Self {
            id: p.id,
            tenant_id: p.tenant_id,
            name: p.name,
            display_name: p.display_name,
            provider_type: p.provider_type,
            api_base: p.api_base,
            auth_type: p.auth_type,
            default_model: p.default_model,
            headers: redact_runtime_provider_headers(p.headers),
            request_transform: p.request_transform,
            response_transform: p.response_transform,
            enabled: p.enabled,
            created_at: p.created_at,
            updated_at: p.updated_at,
        }
    }
}

pub fn redact_runtime_provider_headers(
    headers: Option<serde_json::Value>,
) -> Option<serde_json::Value> {
    let mut headers = headers?;
    if let Some(object) = headers.as_object_mut() {
        if object.contains_key("api_key") {
            object.insert(
                "api_key".to_string(),
                serde_json::Value::String(RUNTIME_PROVIDER_SECRET_REDACTION.to_string()),
            );
        }
    }
    Some(headers)
}

#[derive(Debug, Deserialize)]
pub struct RuntimeProviderScopeQuery {
    pub tenant_id: Option<String>,
}

pub(crate) async fn preserve_redacted_runtime_provider_secret(
    store: &RuntimeProviderStore<'_>,
    req: &mut CreateRuntimeProviderRequest,
) -> Result<()> {
    if !runtime_provider_headers_have_redacted_api_key(req.headers.as_ref()) {
        return Ok(());
    }

    let Some(existing) = store
        .get_scoped(req.tenant_id.as_deref(), &req.name)
        .await?
    else {
        return Err(HttpError::from(AppError::InvalidInput(
            "runtime provider api_key is redacted but no existing provider secret was found".into()
        )));
    };
    let Some(existing_api_key) =
        runtime_provider_header_value(existing.headers.as_ref(), "api_key")
    else {
        return Err(HttpError::from(AppError::InvalidInput(
            "runtime provider api_key is redacted but existing provider has no direct api_key"
                .into()
        )));
    };

    if let Some(headers) = req
        .headers
        .as_mut()
        .and_then(|headers| headers.as_object_mut())
    {
        headers.insert(
            "api_key".to_string(),
            serde_json::Value::String(existing_api_key),
        );
    }
    Ok(())
}

pub fn runtime_provider_headers_have_redacted_api_key(headers: Option<&serde_json::Value>) -> bool {
    runtime_provider_header_value(headers, "api_key").as_deref()
        == Some(RUNTIME_PROVIDER_SECRET_REDACTION)
}

pub fn runtime_provider_header_value(headers: Option<&serde_json::Value>, key: &str) -> Option<String> {
    headers
        .and_then(|headers| headers.as_object())
        .and_then(|headers| headers.get(key))
        .and_then(|value| value.as_str())
        .map(str::to_string)
        .filter(|value| !value.is_empty())
}

pub fn runtime_provider_entry_headers_and_key(
    headers: Option<&serde_json::Value>,
) -> (HashMap<String, String>, Option<String>) {
    let mut headers = headers
        .and_then(|value| value.as_object())
        .map(|object| {
            object
                .iter()
                .filter_map(|(key, value)| {
                    value.as_str().map(|value| (key.clone(), value.to_string()))
                })
                .collect::<HashMap<_, _>>()
        })
        .unwrap_or_default();
    let api_key = runtime_provider_api_key(&mut headers);
    (headers, api_key)
}

pub fn runtime_provider_api_key(headers: &mut HashMap<String, String>) -> Option<String> {
    if let Some(env_name) = headers.remove("api_key_env") {
        return std::env::var(env_name)
            .ok()
            .filter(|value| !value.is_empty());
    }

    headers.remove("api_key").filter(|value| !value.is_empty())
}

// =============================================================================
// Run History
// =============================================================================

pub use ares_store::run_history::{
    AcknowledgeBudgetAlertRequest, AgentHealthMetrics, BudgetAlert, CacheHitStat,
    ListBudgetAlertsQuery, ListLlmCallsQuery, ListToolCallsQuery, LogLlmCallRequest,
    LogToolCallRequest, ModelHealthMetrics, RunCost, RunHistoryStore, RunLlmCall, RunToolCall,
    SetTenantBudgetRequest, TenantBudget,
};
pub use ares_store::token_budgets::{BudgetStatus, TokenBudget, TokenBudgetStore, TokenUsageEntry};

#[derive(Debug, Deserialize)]
pub struct BillingMonthQuery {
    pub month: String,
}

#[derive(Debug, Serialize)]
pub struct BillingSummaryResponse {
    pub tenant_id: String,
    pub month: String,
    pub period_start: i64,
    pub period_end: i64,
    pub total_input_tokens: i64,
    pub total_output_tokens: i64,
    pub raw_cost_usd: f64,
    pub multiplier: f64,
    pub billable_cost_usd: f64,
    pub currency: String,
    pub status: String,
    pub invoice_id: Option<String>,
    pub line_item_count: i64,
    pub unit_line_item_count: i64,
    pub total_unit_quantity: f64,
}

#[derive(Debug, Serialize)]
pub struct BillingLineItemsResponse {
    pub tenant_id: String,
    pub month: String,
    pub items: Vec<BillingLineItemResponse>,
}

#[derive(Debug, Serialize)]
pub struct BillingLineItemResponse {
    pub source_type: String,
    pub source_id: Option<String>,
    pub run_id: String,
    pub agent_name: Option<String>,
    pub model_id: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub input_usd_per_million: Option<f64>,
    pub output_usd_per_million: Option<f64>,
    pub input_cost_usd: f64,
    pub output_cost_usd: f64,
    pub unit_type: Option<String>,
    pub unit_sku: Option<String>,
    pub unit_quantity: f64,
    pub unit_usd_per_unit: Option<f64>,
    pub unit_cost_usd: f64,
    pub raw_cost_usd: f64,
    pub multiplier: f64,
    pub billable_cost_usd: f64,
    pub run_created_at: Option<i64>,
    pub billed_at: i64,
}

#[derive(Debug, Serialize)]
pub struct ModelRateResponse {
    pub model_id: String,
    pub input_usd_per_million: f64,
    pub output_usd_per_million: f64,
    pub description: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct UnitRateResponse {
    pub sku: String,
    pub unit_type: String,
    pub provider: String,
    pub unit_name: String,
    pub usd_per_unit: f64,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct ListRunCostsQuery {
    pub tenant_id: String,
    #[serde(default = "default_list_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
    pub created_after: Option<i64>,
    pub created_before: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct ListHealthMetricsQuery {
    pub tenant_id: String,
    #[serde(default = "default_list_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
}

#[derive(Debug, Deserialize)]
pub struct ListModelMetricsQuery {
    pub tenant_id: String,
    #[serde(default = "default_list_limit")]
    pub limit: i32,
    #[serde(default)]
    pub offset: i32,
}

#[derive(Debug, Deserialize)]
pub struct SetTokenBudgetRequest {
    pub token_limit: i64,
    pub period: String,
}

#[derive(Debug, Deserialize)]
pub struct ListTokenUsageQuery {
    #[serde(default = "default_list_limit_i64")]
    pub limit: i64,
}

pub fn default_list_limit() -> i32 {
    50
}

pub fn default_list_limit_i64() -> i64 {
    50
}

pub fn billing_month_bounds(month: &str) -> Result<(String, i64, i64)> {
    let (year, month_num) = month
        .split_once('-')
        .ok_or_else(|| HttpError::from(AppError::InvalidInput("month must use YYYY-MM format".to_string())))?;
    let year: i32 = year
        .parse()
        .map_err(|_| AppError::InvalidInput("month year must be numeric".to_string()))?;
    let month_num: u32 = month_num
        .parse()
        .map_err(|_| AppError::InvalidInput("month value must be numeric".to_string()))?;
    let start_date = chrono::NaiveDate::from_ymd_opt(year, month_num, 1).ok_or_else(|| {
        AppError::InvalidInput("month must be a valid calendar month".to_string())
    })?;
    let (next_year, next_month) = if month_num == 12 {
        (year + 1, 1)
    } else {
        (year, month_num + 1)
    };
    let next_date = chrono::NaiveDate::from_ymd_opt(next_year, next_month, 1).ok_or_else(|| {
        AppError::InvalidInput("month must be a valid calendar month".to_string())
    })?;
    let period_start = start_date
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
        .timestamp();
    let period_end = next_date
        .and_hms_opt(0, 0, 0)
        .expect("midnight is valid")
        .and_utc()
        .timestamp()
        - 1;
    Ok((
        format!("{year:04}-{month_num:02}"),
        period_start,
        period_end,
    ))
}

pub fn decimal_to_f64(value: rust_decimal::Decimal) -> f64 {
    value.to_f64().unwrap_or(0.0)
}

pub fn billing_line_item_from_run_cost(cost: &RunCost) -> BillingLineItemResponse {
    let raw_cost_usd = decimal_to_f64(cost.total_estimated_cost_usd);
    BillingLineItemResponse {
        source_type: "agent_run".to_string(),
        source_id: Some(cost.run_id.clone()),
        run_id: cost.run_id.clone(),
        agent_name: Some(cost.agent_name.clone()),
        model_id: "aggregate".to_string(),
        input_tokens: cost.total_prompt_tokens,
        output_tokens: cost.total_completion_tokens,
        input_usd_per_million: None,
        output_usd_per_million: None,
        input_cost_usd: 0.0,
        output_cost_usd: raw_cost_usd,
        unit_type: None,
        unit_sku: None,
        unit_quantity: 0.0,
        unit_usd_per_unit: None,
        unit_cost_usd: 0.0,
        raw_cost_usd,
        multiplier: 1.0,
        billable_cost_usd: raw_cost_usd,
        run_created_at: Some(cost.created_at),
        billed_at: cost.created_at,
    }
}

pub fn billing_summary_from_run_costs(
    tenant_id: &str,
    month: String,
    period_start: i64,
    period_end: i64,
    costs: &[RunCost],
) -> BillingSummaryResponse {
    let total_input_tokens = costs.iter().map(|cost| cost.total_prompt_tokens).sum();
    let total_output_tokens = costs.iter().map(|cost| cost.total_completion_tokens).sum();
    let raw_cost_usd: f64 = costs
        .iter()
        .map(|cost| decimal_to_f64(cost.total_estimated_cost_usd))
        .sum();
    BillingSummaryResponse {
        tenant_id: tenant_id.to_string(),
        month,
        period_start,
        period_end,
        total_input_tokens,
        total_output_tokens,
        raw_cost_usd,
        multiplier: 1.0,
        billable_cost_usd: raw_cost_usd,
        currency: "USD".to_string(),
        status: "unbilled".to_string(),
        invoice_id: None,
        line_item_count: costs.len() as i64,
        unit_line_item_count: 0,
        total_unit_quantity: 0.0,
    }
}

pub fn model_rate_responses(config: &BillingConfig) -> Vec<ModelRateResponse> {
    let mut rates: Vec<ModelRateResponse> = config
        .model_pricing
        .values()
        .map(|pricing| ModelRateResponse {
            model_id: pricing.model.clone(),
            input_usd_per_million: pricing.input_usd_per_million_tokens.unwrap_or(0.0),
            output_usd_per_million: pricing.output_usd_per_million_tokens.unwrap_or(0.0),
            description: Some(format!("{} / {}", pricing.provider, pricing.model)),
        })
        .collect();
    rates.sort_by(|left, right| left.model_id.cmp(&right.model_id));
    rates
}

// =============================================================================
// Tenant Model Tiers (per-tenant abstract tier -> concrete provider/model)
// =============================================================================

// =============================================================================
// Skills
// =============================================================================

pub fn required_skill_tenant_id(params: &HashMap<String, String>) -> Result<&str> {
    params
        .get("tenant_id")
        .map(String::as_str)
        .ok_or_else(|| HttpError::from(AppError::InvalidInput("tenant_id is required".into())))
        .and_then(normalized_skill_tenant_id)
}

pub fn normalized_skill_tenant_id(tenant_id: &str) -> Result<&str> {
    let trimmed = tenant_id.trim();
    if trimmed.is_empty() {
        Err(HttpError::from(AppError::InvalidInput("tenant_id must not be empty".to_string())))
    } else {
        Ok(trimmed)
    }
}

#[derive(Debug, Deserialize)]
pub struct RunSkillRequest {
    pub skill_id: String,
    pub tenant_id: String,
    pub input: serde_json::Value,
}

const ADMIN_SKILL_RUN_SOURCE: &str = "admin_skill_run";
const ADMIN_SKILL_CONFIG_SOURCE: &str = "admin_skill";

pub fn normalized_run_skill_tenant_id(tenant_id: &str) -> Result<&str> {
    normalized_skill_tenant_id(tenant_id)
}

pub fn admin_skill_agent_name(skill_id: &str) -> String {
    format!("skill:{skill_id}")
}

pub fn admin_skill_run_metadata(run_id: &str) -> agent_runs::AgentRunMetadata {
    agent_runs::AgentRunMetadata {
        session_id: Some(run_id.to_string()),
        request_source: Some(ADMIN_SKILL_RUN_SOURCE.to_string()),
        agent_config_source: Some(ADMIN_SKILL_CONFIG_SOURCE.to_string()),
        ..Default::default()
    }
}

pub fn admin_skill_active_run(
    run_id: &str,
    tenant_id: &str,
    skill_id: &str,
) -> crate::active_runs::ActiveRun {
    let now = chrono::Utc::now().timestamp();
    crate::active_runs::ActiveRun {
        run_id: run_id.to_string(),
        tenant_id: tenant_id.to_string(),
        agent_name: admin_skill_agent_name(skill_id),
        started_at: now,
        status: "running".to_string(),
        current_step: 0,
        total_steps: 0,
        last_update: now,
        tool_name: Some(admin_skill_agent_name(skill_id)),
        model: Some("skill".to_string()),
        is_catchup: false,
        request_source: Some(ADMIN_SKILL_RUN_SOURCE.to_string()),
        pipeline_id: None,
        schedule_id: None,
        trigger_id: None,
    }
}

// =============================================================================
// Connectors
// =============================================================================

#[derive(Debug, Serialize)]
pub struct OAuthCredentialResponse {
    pub id: String,
    pub tenant_id: String,
    pub provider: String,
    pub connector_type: String,
    pub client_id: String,
    pub has_access_token: bool,
    pub has_refresh_token: bool,
    pub expires_at: Option<i64>,
    pub scope: Option<String>,
    pub created_at: i64,
    pub updated_at: i64,
}

impl From<ares_store::oauth_credentials::OAuthCredential> for OAuthCredentialResponse {
    fn from(value: ares_store::oauth_credentials::OAuthCredential) -> Self {
        Self {
            id: value.id,
            tenant_id: value.tenant_id,
            provider: value.provider,
            connector_type: value.connector_type,
            client_id: value.client_id,
            has_access_token: value.access_token.is_some(),
            has_refresh_token: value.refresh_token.is_some(),
            expires_at: value.expires_at,
            scope: value.scope,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct OAuthAuthorizeQuery {
    pub tenant_id: String,
    pub connector_type: String,
    pub redirect_uri: String,
}

#[derive(Debug, Deserialize)]
pub struct OAuthCallbackQuery {
    pub code: String,
    pub state: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OAuthProviderConfig {
    pub provider: &'static str,
    pub auth_url: &'static str,
    pub token_url: &'static str,
    pub scope: &'static str,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OAuthState {
    pub tenant_id: String,
    pub connector_type: String,
    pub redirect_uri: String,
}

pub fn oauth_provider_config(connector_type: &str) -> Result<OAuthProviderConfig> {
    match connector_type.trim() {
        "google_calendar" => Ok(OAuthProviderConfig {
            provider: "google",
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scope: "https://www.googleapis.com/auth/calendar.events",
        }),
        "gmail" => Ok(OAuthProviderConfig {
            provider: "google",
            auth_url: "https://accounts.google.com/o/oauth2/v2/auth",
            token_url: "https://oauth2.googleapis.com/token",
            scope: "https://www.googleapis.com/auth/gmail.modify",
        }),
        "slack" => Ok(OAuthProviderConfig {
            provider: "slack",
            auth_url: "https://slack.com/oauth/v2/authorize",
            token_url: "https://slack.com/api/oauth.v2.access",
            scope: "channels:read chat:write users:read",
        }),
        "linkedin" => Ok(OAuthProviderConfig {
            provider: "linkedin",
            auth_url: "https://www.linkedin.com/oauth/v2/authorization",
            token_url: "https://www.linkedin.com/oauth/v2/accessToken",
            scope: "openid profile email w_member_social",
        }),
        "hubspot" => Ok(OAuthProviderConfig {
            provider: "hubspot",
            auth_url: "https://app.hubspot.com/oauth/authorize",
            token_url: "https://api.hubapi.com/oauth/v1/token",
            scope: "crm.objects.contacts.read crm.objects.contacts.write crm.objects.deals.read crm.objects.deals.write",
        }),
        "salesforce" => Ok(OAuthProviderConfig {
            provider: "salesforce",
            auth_url: "https://login.salesforce.com/services/oauth2/authorize",
            token_url: "https://login.salesforce.com/services/oauth2/token",
            scope: "api refresh_token offline_access",
        }),
        other => Err(HttpError::from(AppError::InvalidInput(format!(
            "unsupported OAuth connector_type: {other}"
        ).into()))),
    }
}

pub fn percent_encode_component(value: &str) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' => {
                out.push(byte as char);
            }
            _ => {
                out.push('%');
                out.push(HEX[(byte >> 4) as usize] as char);
                out.push(HEX[(byte & 0x0f) as usize] as char);
            }
        }
    }
    out
}

pub fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

pub fn percent_decode_component(value: &str) -> Result<String> {
    let bytes = value.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        match bytes[i] {
            b'+' => {
                out.push(b' ');
                i += 1;
            }
            b'%' => {
                if i + 2 >= bytes.len() {
                    return Err(HttpError::from(AppError::InvalidInput("malformed OAuth state".to_string())));
                }
                let hi = hex_value(bytes[i + 1])
                    .ok_or_else(|| HttpError::from(AppError::InvalidInput("malformed OAuth state".into())))?;
                let lo = hex_value(bytes[i + 2])
                    .ok_or_else(|| HttpError::from(AppError::InvalidInput("malformed OAuth state".into())))?;
                out.push((hi << 4) | lo);
                i += 3;
            }
            other => {
                out.push(other);
                i += 1;
            }
        }
    }
    String::from_utf8(out).map_err(|_| HttpError::from(AppError::InvalidInput("malformed OAuth state".into())))
}

pub fn safe_callback_redirect_uri(value: &str) -> String {
    let trimmed = value.trim();
    if trimmed.starts_with('/') && !trimmed.starts_with("//") && !trimmed.contains("\\") {
        trimmed.to_string()
    } else {
        "/connectors".to_string()
    }
}

pub fn oauth_state_signing_key() -> Result<String> {
    std::env::var("FLEET_SECRETS_KEY")
        .map_err(|_| HttpError::from(AppError::Configuration("FLEET_SECRETS_KEY not set".into())))
}

pub fn bytes_to_hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for &byte in bytes {
        out.push(HEX[(byte >> 4) as usize] as char);
        out.push(HEX[(byte & 0x0f) as usize] as char);
    }
    out
}

pub fn hmac_sha256_hex(key: &[u8], message: &[u8]) -> String {
    const BLOCK_SIZE: usize = 64;
    let mut key_block = [0_u8; BLOCK_SIZE];

    if key.len() > BLOCK_SIZE {
        let digest = Sha256::digest(key);
        key_block[..digest.len()].copy_from_slice(&digest);
    } else {
        key_block[..key.len()].copy_from_slice(key);
    }

    let mut inner_pad = [0x36_u8; BLOCK_SIZE];
    let mut outer_pad = [0x5c_u8; BLOCK_SIZE];
    for i in 0..BLOCK_SIZE {
        inner_pad[i] ^= key_block[i];
        outer_pad[i] ^= key_block[i];
    }

    let mut inner = Sha256::new();
    inner.update(inner_pad);
    inner.update(message);
    let inner_hash = inner.finalize();

    let mut outer = Sha256::new();
    outer.update(outer_pad);
    outer.update(inner_hash);
    bytes_to_hex(&outer.finalize())
}

pub fn encode_oauth_state_payload(state: &OAuthState) -> String {
    format!(
        "tenant_id={}&connector_type={}&redirect_uri={}",
        percent_encode_component(&state.tenant_id),
        percent_encode_component(&state.connector_type),
        percent_encode_component(&safe_callback_redirect_uri(&state.redirect_uri))
    )
}

pub fn encode_oauth_state(state: &OAuthState) -> Result<String> {
    let payload = encode_oauth_state_payload(state);
    let signing_key = oauth_state_signing_key()?;
    let signature = hmac_sha256_hex(signing_key.as_bytes(), payload.as_bytes());
    Ok(format!("{payload}&signature={signature}"))
}

pub fn constant_time_eq(left: &[u8], right: &[u8]) -> bool {
    if left.len() != right.len() {
        return false;
    }

    let mut diff = 0_u8;
    for (&l, &r) in left.iter().zip(right) {
        diff |= l ^ r;
    }
    diff == 0
}

pub fn decode_oauth_state(value: &str) -> Result<OAuthState> {
    let (payload, signature) = value
        .rsplit_once("&signature=")
        .ok_or_else(|| HttpError::from(AppError::InvalidInput("invalid OAuth state".into())))?;
    let signing_key = oauth_state_signing_key()?;
    let expected = hmac_sha256_hex(signing_key.as_bytes(), payload.as_bytes());
    if !constant_time_eq(signature.as_bytes(), expected.as_bytes()) {
        return Err(HttpError::from(AppError::InvalidInput("invalid OAuth state".to_string())));
    }

    decode_oauth_state_payload(payload)
}

pub fn decode_oauth_state_payload(value: &str) -> Result<OAuthState> {
    let mut tenant_id = None;
    let mut connector_type = None;
    let mut redirect_uri = None;

    for pair in value.split('&') {
        let (key, raw_value) = pair
            .split_once('=')
            .ok_or_else(|| HttpError::from(AppError::InvalidInput("malformed OAuth state".into())))?;
        match key {
            "tenant_id" => tenant_id = Some(percent_decode_component(raw_value)?),
            "connector_type" => connector_type = Some(percent_decode_component(raw_value)?),
            "redirect_uri" => redirect_uri = Some(percent_decode_component(raw_value)?),
            _ => return Err(HttpError::from(AppError::InvalidInput("malformed OAuth state".to_string()))),
        }
    }

    let tenant_id =
        tenant_id.ok_or_else(|| HttpError::from(AppError::InvalidInput("malformed OAuth state".into())))?;
    let connector_type =
        connector_type.ok_or_else(|| HttpError::from(AppError::InvalidInput("malformed OAuth state".into())))?;
    let redirect_uri =
        redirect_uri.ok_or_else(|| HttpError::from(AppError::InvalidInput("malformed OAuth state".into())))?;

    if tenant_id.trim().is_empty() || connector_type.trim().is_empty() {
        return Err(HttpError::from(AppError::InvalidInput("malformed OAuth state".to_string())));
    }

    Ok(OAuthState {
        tenant_id,
        connector_type,
        redirect_uri: safe_callback_redirect_uri(&redirect_uri),
    })
}

pub fn header_value(headers: &HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(',').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

pub fn oauth_callback_url(headers: &HeaderMap) -> String {
    let proto = header_value(headers, "x-forwarded-proto").unwrap_or_else(|| "https".to_string());
    let host = header_value(headers, "x-forwarded-host")
        .or_else(|| header_value(headers, "host"))
        .unwrap_or_else(|| "localhost".to_string());
    format!("{proto}://{host}/api/oauth/callback")
}

pub fn build_authorize_url(
    provider: OAuthProviderConfig,
    client_id: &str,
    callback_url: &str,
    state: &OAuthState,
) -> Result<String> {
    Ok(format!(
        "{}?response_type=code&client_id={}&redirect_uri={}&scope={}&state={}&access_type=offline&prompt=consent",
        provider.auth_url,
        percent_encode_component(client_id),
        percent_encode_component(callback_url),
        percent_encode_component(provider.scope),
        percent_encode_component(&encode_oauth_state(state)?)
    ))
}

pub fn build_token_form<'a>(
    code: &'a str,
    client_id: &'a str,
    client_secret: &'a str,
    callback_url: &'a str,
) -> [(&'static str, &'a str); 5] {
    [
        ("grant_type", "authorization_code"),
        ("code", code),
        ("client_id", client_id),
        ("client_secret", client_secret),
        ("redirect_uri", callback_url),
    ]
}

pub fn oauth_stored_scope(
    default_scope: &str,
    token_scope: Option<&str>,
    instance_url: Option<&str>,
) -> String {
    let mut scope = token_scope
        .filter(|scope| !scope.trim().is_empty())
        .unwrap_or(default_scope)
        .trim()
        .to_string();
    if let Some(instance_url) = instance_url.and_then(|url| safe_oauth_instance_url(url)) {
        if !scope.is_empty() {
            scope.push(' ');
        }
        scope.push_str("instance_url=");
        scope.push_str(instance_url);
    }
    scope
}

pub fn safe_oauth_instance_url(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if (trimmed.starts_with("https://") || trimmed.starts_with("http://"))
        && !trimmed.contains(char::is_whitespace)
    {
        Some(trimmed)
    } else {
        None
    }
}

#[derive(Debug, Deserialize)]
pub struct OAuthTokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<i64>,
    #[serde(default)]
    pub scope: Option<String>,
    #[serde(default)]
    pub instance_url: Option<String>,
}

pub fn normalize_oauth_credential_request(
    tenant_id: String,
    req: &mut ares_store::oauth_credentials::CreateOAuthCredentialRequest,
) -> Result<()> {
    let provider = oauth_provider_config(&req.connector_type)?;
    req.tenant_id = tenant_id;
    req.provider = provider.provider.to_string();
    Ok(())
}

// =============================================================================
// Agent Schedules
// =============================================================================

// =============================================================================
// Event Triggers
// =============================================================================

// =============================================================================
// Agent Pipelines
// =============================================================================

// =============================================================================
// Webhook Receiver (public — no admin middleware)
// =============================================================================

pub fn webhook_trigger_matches(trigger: &db_schedules::EventTrigger) -> bool {
    trigger.event_type == "webhook"
}

///
/// Public webhook endpoint that receives events and triggers the
/// associated agent when the trigger is enabled.
pub async fn receive_webhook(
    Path(trigger_id): Path<String>,
    State(ctx): State<Arc<Context>>,
    Json(payload): Json<serde_json::Value>,
) -> Result<Json<serde_json::Value>> {
    // Prefer TriggerService via Cordis DI (owns DB + Execute).
    if let Some(svc) = ctx.get::<ares_agent::trigger::TriggerService>() {
        match svc
            .dispatch_webhook(&trigger_id, payload.clone(), &ctx)
            .await
        {
            Ok(v) => return Ok(Json(v)),
            Err(e) if e.contains("not found") => {
                return Err(HttpError::from(AppError::NotFound(e)));
            }
            Err(e) => {
                tracing::warn!(trigger_id=%trigger_id, error=%e, "Webhook TriggerService dispatch failed");
                return Err(HttpError::from(AppError::Internal(e)));
            }
        }
    }
    let __pool_5 = ctx.get::<ares_store::TenantDb>().expect("not provided").pool().clone();
    let store = db_schedules::EventTriggerStore::new(&__pool_5);
    let trigger = store.get_trigger(&trigger_id).await?;
    if let Some(trigger) = trigger {
        if !webhook_trigger_matches(&trigger) {
            return Err(HttpError::from(AppError::NotFound(format!(
                "Webhook trigger {trigger_id} not found"
            ).into())));
        }
        if trigger.enabled {
            tracing::info!(
                trigger_id = %trigger_id,
                agent = %trigger.target_agent,
                payload = %payload,
                "Webhook received — triggering agent"
            );
            let message = serde_json::to_string(&payload).unwrap_or_default();
            if let Err(e) = ares_agent::trigger::execute_triggered_agent(
                &trigger,
                &message,
                &ctx,
            )
            .await
            {
                tracing::warn!(
                    trigger_id = %trigger_id,
                    agent = %trigger.target_agent,
                    error = %e,
                    "Webhook trigger execution failed"
                );
            }
            Ok(Json(
                serde_json::json!({"status": "triggered", "agent": trigger.target_agent}),
            ))
        } else {
            Ok(Json(
                serde_json::json!({"status": "ignored", "reason": "disabled"}),
            ))
        }
    } else {
        Err(HttpError::from(AppError::NotFound(format!(
            "Trigger {trigger_id} not found"
        ).into())))
    }
}

// =============================================================================
// Live Runs SSE
// =============================================================================
