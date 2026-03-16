//! Tracks DCRM deal stage transitions based on usage events.
//! On the first usage_event for a new tenant, updates their DCRM deal to 'first_agent_run'.

use crate::dcrm::client::{DcrmClient, DealStageUpdate};
use std::env;

/// Check if this is the first usage event for a tenant and update DCRM stage.
/// Called after agent execution in the usage tracking middleware.
pub async fn track_first_usage(tenant_id: &str, pool: &sqlx::PgPool) {
    // Check if this tenant has exactly 1 usage event (i.e., this is their first)
    let count = sqlx::query_scalar!(
        r#"SELECT COUNT(*)::bigint as "count!" FROM usage_events WHERE tenant_id = $1"#,
        tenant_id
    )
    .fetch_one(pool)
    .await;

    match count {
        Ok(1) => {
            // First usage event — update DCRM deal stage
            let dcrm_url = env::var("DCRM_BASE_URL")
                .unwrap_or_else(|_| "http://localhost:3001".to_string());
            let dcrm = DcrmClient::new(&dcrm_url);

            // Find deals for this tenant and update stage
            let http = reqwest::Client::new();
            let deals_url = format!("{}/api/deals?tenant_id={}", dcrm_url, tenant_id);
            if let Ok(resp) = http.get(&deals_url).send().await {
                if let Ok(deals) = resp.json::<serde_json::Value>().await {
                    if let Some(deal_id) = deals.as_array()
                        .and_then(|arr| arr.first())
                        .and_then(|d| d["id"].as_str())
                    {
                        let _ = dcrm.update_deal_stage(
                            deal_id,
                            &DealStageUpdate { stage: "first_agent_run".to_string() },
                        ).await;
                        tracing::info!("Updated DCRM deal stage to first_agent_run for tenant {}", tenant_id);
                    }
                }
            }
        }
        _ => {} // Not first event, or query failed — skip
    }
}
