use crate::tools::registry::Tool;
use crate::types::Result;
use async_trait::async_trait;
use serde_json::{json, Value};
use std::env;

fn dcrm_base_url() -> String {
    env::var("DCRM_BASE_URL").unwrap_or_else(|_| "http://localhost:3001".to_string())
}

// ─── dcrm_list_contacts ───────────────────────────────────────────────────────

pub struct DcrmListContactsTool {
    client: reqwest::Client,
}

impl DcrmListContactsTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for DcrmListContactsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DcrmListContactsTool {
    fn name(&self) -> &str {
        "dcrm_list_contacts"
    }

    fn description(&self) -> &str {
        "List contacts from DCRM (Dirmacs CRM). Returns a list of contacts with name, email, company, and role."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "company": {
                    "type": "string",
                    "description": "Filter by company name (optional)"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let url = format!("{}/api/contacts", dcrm_base_url());
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;

        // Optional company filter in-memory
        if let Some(company_filter) = args.get("company").and_then(|v| v.as_str()) {
            let filter_lower = company_filter.to_lowercase();
            if let Some(contacts) = json.get("contacts").and_then(|c| c.as_array()) {
                let filtered: Vec<_> = contacts
                    .iter()
                    .filter(|c| {
                        c.get("company")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_lowercase().contains(&filter_lower))
                            .unwrap_or(false)
                    })
                    .collect();
                return Ok(json!({ "contacts": filtered, "total": filtered.len() }));
            }
        }

        Ok(json)
    }
}

// ─── dcrm_create_contact ──────────────────────────────────────────────────────

pub struct DcrmCreateContactTool {
    client: reqwest::Client,
}

impl DcrmCreateContactTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for DcrmCreateContactTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DcrmCreateContactTool {
    fn name(&self) -> &str {
        "dcrm_create_contact"
    }

    fn description(&self) -> &str {
        "Create a new contact in DCRM. Use this when a new business prospect has been identified or surveyed."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "name": {
                    "type": "string",
                    "description": "Full name of the contact"
                },
                "email": {
                    "type": "string",
                    "description": "Email address"
                },
                "company": {
                    "type": "string",
                    "description": "Company name"
                },
                "role": {
                    "type": "string",
                    "description": "Job title or role (optional)"
                },
                "notes": {
                    "type": "string",
                    "description": "Any notes about this contact (optional)"
                },
                "linkedin_url": {
                    "type": "string",
                    "description": "LinkedIn profile URL (optional)"
                }
            },
            "required": ["name", "email", "company"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let url = format!("{}/api/contacts", dcrm_base_url());
        let body = json!({
            "name": args.get("name").and_then(|v| v.as_str()).unwrap_or(""),
            "email": args.get("email").and_then(|v| v.as_str()).unwrap_or(""),
            "company": args.get("company").and_then(|v| v.as_str()).unwrap_or(""),
            "role": args.get("role").and_then(|v| v.as_str()),
            "notes": args.get("notes").and_then(|v| v.as_str()),
            "linkedin_url": args.get("linkedin_url").and_then(|v| v.as_str()),
        });
        let resp = self
            .client
            .post(&url)
            .json(&body)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let status = resp.status();
        let json: Value = resp
            .json()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        if status.is_success() {
            Ok(json!({ "success": true, "contact": json }))
        } else {
            Ok(json!({ "success": false, "error": json }))
        }
    }
}

// ─── dcrm_list_deals ──────────────────────────────────────────────────────────

pub struct DcrmListDealsTool {
    client: reqwest::Client,
}

impl DcrmListDealsTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for DcrmListDealsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DcrmListDealsTool {
    fn name(&self) -> &str {
        "dcrm_list_deals"
    }

    fn description(&self) -> &str {
        "List deals/opportunities from DCRM. Returns pipeline stages, values, and probabilities."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "stage": {
                    "type": "string",
                    "description": "Filter by pipeline stage (optional, e.g. 'qualified', 'proposal', 'closed')"
                }
            },
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> Result<Value> {
        let url = format!("{}/api/deals", dcrm_base_url());
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        Ok(json)
    }
}

// ─── dcrm_pipeline_analytics ──────────────────────────────────────────────────

pub struct DcrmPipelineAnalyticsTool {
    client: reqwest::Client,
}

impl DcrmPipelineAnalyticsTool {
    pub fn new() -> Self {
        Self {
            client: reqwest::Client::builder()
                .timeout(std::time::Duration::from_secs(10))
                .build()
                .unwrap(),
        }
    }
}

impl Default for DcrmPipelineAnalyticsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl Tool for DcrmPipelineAnalyticsTool {
    fn name(&self) -> &str {
        "dcrm_pipeline_analytics"
    }

    fn description(&self) -> &str {
        "Get pipeline analytics from DCRM — total pipeline value, deals by stage, conversion rates."
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {},
            "required": []
        })
    }

    async fn execute(&self, _args: Value) -> Result<Value> {
        let url = format!("{}/api/analytics/pipeline", dcrm_base_url());
        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        let json: Value = resp
            .json()
            .await
            .map_err(|e| crate::types::AppError::Internal(e.to_string()))?;
        Ok(json)
    }
}
