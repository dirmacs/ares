use crate::dcrm::client::{DcrmClient, ContactCreate, DealCreate, ActivityCreate};
use crate::mcp::eruka_proxy::ErukaProxy;
use crate::dsprint::recommend::{recommend_agents, RecommendationRequest, DomainScore};
use crate::types::{AppError, Result};
use crate::AppState;
use axum::extract::{Multipart, Path, State};
use axum::Json;
use reqwest;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::env;
use chrono::Utc;

#[derive(Debug, Deserialize)]
pub struct SubmitPayload {
    pub company_name: String,
    pub industry: String,
    pub team_size: String,
    pub email: String,
    pub answers: HashMap<String, serde_json::Value>,
    #[serde(rename = "ref")]
 pub referral: Option<String>,
 pub entity_type: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct SubmitResponse {
    pub workspace_id: String,
    pub session_id: String,
    pub scores: DomainScores,
    pub recommendations: Vec<crate::dsprint::recommend::AgentRecommendation>,
    pub recommended_tier: String,
    pub recommended_price: String,
    pub tree_stats: TreeStats,
}

#[derive(Debug, Serialize)]
pub struct DomainScores {
    pub overall: u32,
    pub sales: u32,
    pub marketing: u32,
    pub operations: u32,
    pub customer_service: u32,
    pub finance: u32,
}

#[derive(Debug, Serialize)]
pub struct TreeStats {
    pub roots_planted: u32,
    pub branches_initialized: u32,
    pub fields_confirmed: u32,
    pub fields_inferred: u32,
    pub gaps_identified: u32,
}

pub async fn submit(
    State(app): State<AppState>,
    Json(payload): Json<SubmitPayload>,
) -> Result<Json<SubmitResponse>> {
    // 1. Create ErukaProxy from env — NO bom auth for DSprint
    // All DSprint Eruka calls use X-Service-Key + X-Workspace-Id
    // so workspaces + sessions are scoped to the visitor, not bom@dirmacs.com
    let eruka_url = env::var("ERUKA_API_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());
    let eruka = ErukaProxy::new(&eruka_url);

    // 2. Create workspace via Eruka (graceful failure)
    let ws = eruka.create_workspace(&payload.company_name, &payload.email).await;
    let workspace_id = match &ws {
        Ok(v) => v["id"].as_str().unwrap_or("unknown").to_string(),
        Err(e) => {
            tracing::warn!("Eruka create_workspace failed: {e}");
            uuid::Uuid::new_v4().to_string()
        }
    };

    // 3. Create Sisyphos session scoped to workspace (NOT email)
    tracing::info!("DSprint submit: creating Sisyphos session for workspace_id={}", workspace_id);
    let session = eruka.sisyphos_create_session(&workspace_id).await;
    let session_id = match &session {
        Ok(v) => v["id"].as_str().or(v["session_id"].as_str()).unwrap_or("unknown").to_string(),
        Err(e) => {
            tracing::warn!("Eruka sisyphos_create_session failed: {e}");
            uuid::Uuid::new_v4().to_string()
        }
    };

	// Add entity_type to answers if provided
	let mut answers = payload.answers.clone();
	if let Some(ref entity_type) = payload.entity_type {
		answers.insert("identity.entity_type".to_string(), serde_json::json!(entity_type));
	}

	// 4. Write answer fields to Eruka using batch import
	let import_fields: Vec<serde_json::Value> = answers.iter()
		.filter_map(|(path, value)| {
			let parts: Vec<&str> = path.splitn(2, '.').collect();
			if parts.len() == 2 {
				Some(serde_json::json!({
					"category": parts[0],
					"field": parts[1],
					"value": value,
					"confidence": 1.0,
					"source": "dsprint_survey"
				}))
			} else {
				None
			}
		})
		.collect();
	let import_result = eruka.import_workspace_fields(&workspace_id, &import_fields, true).await;
	let fields_confirmed = match &import_result {
		Ok(v) => v["fields_written"].as_u64().unwrap_or(0) as u32,
		Err(e) => {
			tracing::warn!("Eruka batch import failed: {e}");
			payload.answers.len() as u32
		}
	};

    // 5. Get gaps + completeness (graceful failure)
    let gaps = eruka.get_gaps(&payload.email).await;
    let gaps_count = match &gaps {
        Ok(v) => v["gaps"].as_array().map(|a| a.len() as u32).unwrap_or(0),
        Err(_) => 0,
    };

    // 6. Compute basic domain scores from answer keys
    let scores = compute_domain_scores(&payload.answers);

    // 7. Call recommendation engine (returns empty vec if no domain scores match)
    let rec_request = RecommendationRequest {
        domain_scores: vec![
            DomainScore { domain: "sales".to_string(), score: scores.sales, completeness: 0.5, gaps: 0 },
            DomainScore { domain: "marketing".to_string(), score: scores.marketing, completeness: 0.5, gaps: 0 },
            DomainScore { domain: "operations".to_string(), score: scores.operations, completeness: 0.5, gaps: 0 },
            DomainScore { domain: "customer_service".to_string(), score: scores.customer_service, completeness: 0.5, gaps: 0 },
            DomainScore { domain: "finance".to_string(), score: scores.finance, completeness: 0.5, gaps: 0 },
        ],
        pain_points: payload.answers.keys()
            .filter(|k| k.contains("pain") || k.contains("challenge") || k.contains("problem"))
            .filter_map(|k| payload.answers[k].as_str().map(|s| s.to_string()))
            .collect(),
        team_size: payload.team_size.clone(),
        industry: payload.industry.clone(),
        current_tools: vec![],
    };
    let recommendations = recommend_agents(&rec_request);

    // 8. Determine recommended tier from recommendations
    let recommended_tier = if recommendations.iter().any(|r| r.tier_required == "growth") {
        "growth".to_string()
    } else {
        "starter".to_string()
    };
    let recommended_price = match recommended_tier.as_str() {
        "growth" => "Rs.15,000/mo".to_string(),
        _ => "Rs.5,000/mo".to_string(),
    };

    // 9. Async: fire DCRM writes (contact + deal + activity)
    let dcrm_url = env::var("DCRM_BASE_URL").unwrap_or_else(|_| "http://localhost:3001".to_string());
    let dcrm = DcrmClient::new(&dcrm_url);
    let company = payload.company_name.clone();
    let email = payload.email.clone();
    let industry = payload.industry.clone();
    let ws_id = workspace_id.clone();
    tokio::spawn(async move {
        // Create contact
        let contact_result = dcrm.create_contact(&ContactCreate {
            name: company.clone(),
            email: email.clone(),
            company: Some(company.clone()),
            industry: Some(industry),
            source: Some("dsprint_survey".to_string()),
            metadata: serde_json::json!({"workspace_id": ws_id}),
        }).await;

        if let Ok(contact) = contact_result {
            let contact_id = contact["id"].as_str().unwrap_or("").to_string();
            // Create deal
            let _ = dcrm.create_deal(&DealCreate {
                contact_id: contact_id.clone(),
                title: format!("DSprint Survey - {}", company),
                stage: "dsprint_submitted".to_string(),
                value: 0.0,
                currency: "INR".to_string(),
                pipeline: "dsprint".to_string(),
                metadata: serde_json::json!({"workspace_id": ws_id}),
            }).await;

            // Log activity
            let _ = dcrm.log_activity(&ActivityCreate {
                contact_id: Some(contact_id),
                deal_id: None,
                activity_type: "dsprint_survey_submitted".to_string(),
                description: format!("Survey submitted by {} from {}", email, company),
                metadata: serde_json::json!({"workspace_id": ws_id}),
            }).await;
        }
    });
    // 10. Async: fire BOM surveyor agent
    {
        let agent_reg = app.agent_registry.clone();
        let eruka_url_bom = env::var("ERUKA_API_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());
        let ws_id_bom = workspace_id.clone();
        let company_bom = payload.company_name.clone();
        let industry_bom = payload.industry.clone();
        tokio::spawn(async move {
            use crate::agents::Agent;
            use crate::types::AgentContext;
            let ctx = AgentContext {
                user_id: "dsprint-system".to_string(),
                session_id: ws_id_bom.clone(),
                conversation_history: vec![],
                user_memory: None,
            };
            match agent_reg.create_agent("bom-dsprint-surveyor").await {
                Ok(agent) => {
                    let prompt = format!(
                        "Analyze workspace {} for company '{}' in industry '{}'. Provide automation recommendations.",
                        ws_id_bom, company_bom, industry_bom
                    );
                    match agent.execute(&prompt, &ctx).await {
                        Ok(response) => {
                            let http = reqwest::Client::new();
                            let url = format!("{}/api/v1/context", eruka_url_bom);
                            let body = serde_json::json!({
                                "path": format!("workspaces/{}/metadata/dsprint_analysis", ws_id_bom),
                                "value": response.content,
                                "knowledge_state": "CONFIRMED",
                                "confidence": 0.8
                            });
                            let _ = http.post(&url).json(&body).send().await;
                            tracing::info!("BOM surveyor completed for workspace {}", ws_id_bom);
                        }
                        Err(e) => tracing::warn!("BOM surveyor execution failed: {e}"),
                    }
                }
                Err(e) => tracing::warn!("BOM surveyor agent not available: {e}"),
            }
        });
    }

	// 11. Return response
	let tree_stats = TreeStats {
		roots_planted: import_result.as_ref().ok().and_then(|v| v["tree"]["root"].as_u64()).unwrap_or(0) as u32,
		branches_initialized: import_result.as_ref().ok().and_then(|v| v["tree"]["total"].as_u64()).unwrap_or(0) as u32,
		fields_confirmed,
		fields_inferred: 0,
		gaps_identified: gaps_count,
	};

    Ok(Json(SubmitResponse {
        workspace_id,
        session_id,
        scores,
        recommendations,
        recommended_tier,
        recommended_price,
        tree_stats,
    }))
}

/// Compute domain scores from answer keys (heuristic based on which domain keys are present)
fn compute_domain_scores(answers: &HashMap<String, serde_json::Value>) -> DomainScores {
    let mut domain_hits: HashMap<&str, u32> = HashMap::new();
    for key in answers.keys() {
        let domain = if key.starts_with("market.") || key.starts_with("sales.") {
            "sales"
        } else if key.starts_with("marketing.") || key.starts_with("content.") {
            "marketing"
        } else if key.starts_with("goals.") || key.starts_with("operations.") {
            "operations"
        } else if key.starts_with("support.") || key.starts_with("customer.") {
            "customer_service"
        } else if key.starts_with("finance.") || key.starts_with("billing.") {
            "finance"
        } else {
            continue
        };
        *domain_hits.entry(domain).or_default() += 1;
    }

    let total_fields = answers.len().max(1) as f64;
    let score = |domain: &str| -> u32 {
        let hits = *domain_hits.get(domain).unwrap_or(&0) as f64;
        ((hits / total_fields) * 100.0).min(100.0) as u32
    };

    let sales = score("sales").max(40);
    let marketing = score("marketing").max(30);
    let operations = score("operations").max(35);
    let customer_service = score("customer_service").max(25);
    let finance = score("finance").max(20);
    let overall = (sales + marketing + operations + customer_service + finance) / 5;

    DomainScores { overall, sales, marketing, operations, customer_service, finance }
}

#[derive(Debug, Serialize)]
pub struct ResultsResponse {
    pub company_name: String,
    pub overall_score: u32,
    pub domains: HashMap<String, DomainDetail>,
    pub recommendations: Vec<crate::dsprint::recommend::AgentRecommendation>,
    pub recommended_tier: String,
    pub recommended_price: String,
    pub total_savings: TotalSavings,
    pub gaps_to_fill: Vec<GapInfo>,
    pub tree_summary: TreeSummary,
    pub mcp_available: bool,
    pub workspace_id: String,
}

#[derive(Debug, Serialize)]
pub struct DomainDetail {
    pub score: u32,
    pub completeness: u32,
    pub gaps: u32,
    pub potential_savings_hrs: String,
}

#[derive(Debug, Serialize)]
pub struct TotalSavings {
    pub hours_per_week: String,
    pub inr_per_month: String,
}

#[derive(Debug, Serialize)]
pub struct GapInfo {
    pub field: String,
    pub impact: String,
    pub question: String,
    pub unlocks: String,
}

#[derive(Debug, Serialize)]
pub struct TreeSummary {
    pub roots: u32,
    pub trunk: u32,
    pub branches: HashMap<String, u32>,
    pub leaves: u32,
    pub total_nodes: u32,
    pub coverage: String,
}

pub async fn results(
    State(_app): State<AppState>,
    Path(workspace_id): Path<String>,
) -> Result<Json<ResultsResponse>> {
    let eruka_url = env::var("ERUKA_API_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());
    // Service auth — read workspace context, not bom's
    let eruka = ErukaProxy::new(&eruka_url);

    // 1. Get workspace data from Eruka
    let ws_data = eruka.get_workspace(&workspace_id).await;
    let (company_name, team_size, industry, pain_points) = match &ws_data {
    Ok(v) => {
    let name = v["name"].as_str().unwrap_or("Unknown").to_string();
    
    // Extract team_size from workspace metadata (identity.company_size or defaults)
    let team_size = v["metadata"]["identity"]["company_size"]
    .as_str()
    .or_else(|| v["metadata"]["team_size"].as_str())
    .unwrap_or("11-50")
    .to_string();
    
    // Extract industry from workspace metadata (identity.industry or defaults)
    let industry = v["metadata"]["identity"]["industry"]
    .as_str()
    .or_else(|| v["metadata"]["industry"].as_str())
    .unwrap_or("General")
    .to_string();
    
    // Extract pain_points from workspace metadata (goals.* or market.* fields)
    let pain_points: Vec<String> = v["metadata"]["goals"]
    .as_object()
    .map(|goals| {
    goals.iter()
    .filter_map(|(k, v)| {
    if k.starts_with("pain_") || k.contains("challenge") {
    v.as_str().map(|s| s.to_string())
    } else {
    None
    }
    })
    .collect()
    })
    .unwrap_or_default();
    
    (name, team_size, industry, pain_points)
    }
    Err(e) => {
    tracing::warn!("Eruka get_workspace failed: {e}");
    ("Unknown".to_string(), "11-50".to_string(), "General".to_string(), vec![])
    }
    };

    // 2. Get gaps from Eruka
    let gaps_data = eruka.get_gaps(&workspace_id).await;
    let gaps_list: Vec<GapInfo> = match &gaps_data {
        Ok(v) => v["gaps"].as_array().map(|arr| {
            arr.iter().map(|g| GapInfo {
                field: g["field"].as_str().unwrap_or("").to_string(),
                impact: g["impact"].as_str().unwrap_or("medium").to_string(),
                question: g["question"].as_str().unwrap_or("").to_string(),
                unlocks: g["unlocks"].as_str().unwrap_or("").to_string(),
            }).collect()
        }).unwrap_or_default(),
        Err(_) => vec![],
    };

    // 3. Get completeness from Eruka
    let completeness = eruka.get_completeness(&workspace_id, "overall").await;
    let overall_completeness = match &completeness {
        Ok(v) => v["completeness"].as_f64().unwrap_or(0.5),
        Err(_) => 0.5,
    };

    // 4. Build domain details with static defaults (will be refined as Eruka data improves)
    let domains = build_domain_details(overall_completeness, &gaps_list);

    // 5. Compute overall score
    let overall_score = domains.values().map(|d| d.score).sum::<u32>() / domains.len().max(1) as u32;

    // 6. Run recommendation engine
    let rec_request = RecommendationRequest {
        domain_scores: domains.iter().map(|(name, detail)| DomainScore {
            domain: name.clone(),
            score: detail.score,
            completeness: detail.completeness as f64 / 100.0,
            gaps: detail.gaps,
        }).collect(),
    pain_points,
    team_size,
    industry,
        current_tools: vec![],
    };
    let recommendations = recommend_agents(&rec_request);

    // 7. Determine tier and price
    let recommended_tier = if recommendations.iter().any(|r| r.tier_required == "growth") {
        "growth".to_string()
    } else {
        "starter".to_string()
    };
    let recommended_price = match recommended_tier.as_str() {
        "growth" => "Rs.15,000/mo".to_string(),
        _ => "Rs.5,000/mo".to_string(),
    };

    // 8. Compute total savings from recommendations
    let total_savings = compute_total_savings(&recommendations);

    // 9. Build tree summary from real Eruka data (fall back to heuristic if unavailable)
    let tree_data = eruka.build_workspace_tree(&workspace_id).await;
    let tree_summary = match &tree_data {
        Ok(v) => {
            let nodes = v["nodes"].as_array();
            let root_count = nodes.map(|n| n.iter().filter(|node| node["tier"] == "root").count()).unwrap_or(0) as u32;
            let trunk_count = nodes.map(|n| n.iter().filter(|node| node["tier"] == "trunk").count()).unwrap_or(0) as u32;
            let branch_count = nodes.map(|n| n.iter().filter(|node| node["tier"] == "branch").count()).unwrap_or(0) as u32;
            let leaf_count = nodes.map(|n| n.iter().filter(|node| node["tier"] == "leaf").count()).unwrap_or(0) as u32;
            let total = root_count + trunk_count + branch_count + leaf_count;
            TreeSummary {
                roots: root_count,
                trunk: trunk_count,
                branches: domains.iter().map(|(name, _)| (name.clone(), branch_count / domains.len().max(1) as u32)).collect(),
                leaves: leaf_count,
                total_nodes: total,
                coverage: format!("{}%", if total > 0 { ((root_count + trunk_count) as f64 / total as f64 * 100.0) as u32 } else { 0 }),
            }
        }
        Err(e) => {
            tracing::warn!("Tree build for results failed: {e}");
            let total_nodes: u32 = domains.values().map(|d| d.score / 10).sum();
            TreeSummary {
                roots: domains.len() as u32,
                trunk: 0,
                branches: domains.iter().map(|(name, detail)| (name.clone(), detail.score / 20)).collect(),
                leaves: 0,
                total_nodes,
                coverage: format!("{}%", (overall_completeness * 100.0) as u32),
            }
        }
    };

    Ok(Json(ResultsResponse {
        company_name,
        overall_score,
        domains,
        recommendations,
        recommended_tier,
        recommended_price,
        total_savings,
        gaps_to_fill: gaps_list,
        tree_summary,
        mcp_available: true,
        workspace_id,
    }))
}

fn build_domain_details(overall_completeness: f64, gaps: &[GapInfo]) -> HashMap<String, DomainDetail> {
    let mut domains = HashMap::new();
    let base_completeness = (overall_completeness * 100.0) as u32;

    for (name, base_score, savings) in [
        ("sales", 55u32, "20-30"),
        ("marketing", 45u32, "14-21"),
        ("operations", 65u32, "30-50"),
        ("customer_service", 40u32, "12-18"),
        ("finance", 35u32, "10-16"),
    ] {
        let domain_gaps = gaps.iter().filter(|g| g.field.starts_with(name) || g.field.starts_with(&name[..3.min(name.len())])).count() as u32;
        domains.insert(name.to_string(), DomainDetail {
            score: base_score,
            completeness: base_completeness,
            gaps: domain_gaps,
            potential_savings_hrs: savings.to_string(),
        });
    }
    domains
}

fn compute_total_savings(recommendations: &[crate::dsprint::recommend::AgentRecommendation]) -> TotalSavings {
    let mut total_min_hrs: u32 = 0;
    let mut total_max_hrs: u32 = 0;

    for rec in recommendations {
        let parts: Vec<&str> = rec.savings_hours.split('-').collect();
        if parts.len() == 2 {
            total_min_hrs += parts[0].parse::<u32>().unwrap_or(0);
            total_max_hrs += parts[1].parse::<u32>().unwrap_or(0);
        }
    }

    let min_inr = total_min_hrs as u64 * 2000;
    let max_inr = total_max_hrs as u64 * 2000;

    TotalSavings {
        hours_per_week: format!("{}-{}", total_min_hrs, total_max_hrs),
        inr_per_month: format!("Rs.{}K-{}K", min_inr / 1000, max_inr / 1000),
    }
}

#[derive(Debug, Serialize)]
pub struct UploadResponse {
    pub fields_extracted: u32,
    pub fields_confirmed: u32,
    pub fields_inferred: u32,
    pub gaps_remaining: u32,
    pub follow_up_questions: Vec<String>,
}

/// POST /v1/dsprint/upload — multipart file upload for document extraction
pub async fn upload(
    State(_app): State<AppState>,
    mut multipart: Multipart,
) -> Result<Json<UploadResponse>> {
    let eruka_url = env::var("ERUKA_API_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());
    let mut workspace_id = String::new();
    let mut file_data: Option<(String, Vec<u8>)> = None;

    // Parse multipart fields
    while let Some(field) = multipart.next_field().await.map_err(|e| {
        AppError::InvalidInput(format!("Failed to read multipart field: {}", e))
    })? {
        let name = field.name().unwrap_or("").to_string();
        match name.as_str() {
            "workspace_id" => {
                workspace_id = field.text().await.map_err(|e| {
                    AppError::InvalidInput(format!("Failed to read workspace_id: {}", e))
                })?;
            }
            "file" => {
                let filename = field.file_name().unwrap_or("document").to_string();
                let data = field.bytes().await.map_err(|e| {
                    AppError::InvalidInput(format!("Failed to read file: {}", e))
                })?;
                file_data = Some((filename, data.to_vec()));
            }
            _ => {}
        }
    }

    if workspace_id.is_empty() {
        return Err(AppError::InvalidInput("workspace_id is required".to_string()));
    }

    let (filename, data) = match file_data {
        Some(fd) => fd,
        None => return Err(AppError::InvalidInput("file is required".to_string())),
    };

    // Forward file to Eruka Sisyphos upload endpoint
    let http = reqwest::Client::new();
    let upload_url = format!("{}/api/v1/sisyphos/sessions/{}/upload", eruka_url, workspace_id);

    let part = reqwest::multipart::Part::bytes(data)
        .file_name(filename.clone())
        .mime_str("application/octet-stream")
        .unwrap_or_else(|_| reqwest::multipart::Part::bytes(vec![]));
    let form = reqwest::multipart::Form::new()
        .text("workspace_id", workspace_id.clone())
        .part("file", part);

    let eruka_response = http.post(&upload_url).multipart(form).send().await;

    match eruka_response {
        Ok(resp) if resp.status().is_success() => {
            let body: serde_json::Value = resp.json().await.unwrap_or_default();
            Ok(Json(UploadResponse {
                fields_extracted: body["fields_extracted"].as_u64().unwrap_or(0) as u32,
                fields_confirmed: body["fields_confirmed"].as_u64().unwrap_or(0) as u32,
                fields_inferred: body["fields_inferred"].as_u64().unwrap_or(0) as u32,
                gaps_remaining: body["gaps_remaining"].as_u64().unwrap_or(0) as u32,
                follow_up_questions: body["follow_up_questions"]
                    .as_array()
                    .map(|arr| arr.iter().filter_map(|v| v.as_str().map(String::from)).collect())
                    .unwrap_or_default(),
            }))
        }
        Ok(resp) => {
            let status = resp.status().as_u16();
            let body = resp.text().await.unwrap_or_default();
            tracing::warn!("Eruka upload failed: {} {}", status, body);
            // Return graceful response — Eruka may not have the upload endpoint yet
            Ok(Json(UploadResponse {
                fields_extracted: 0,
                fields_confirmed: 0,
                fields_inferred: 0,
                gaps_remaining: 0,
                follow_up_questions: vec!["Document processing is not yet available. Please answer the questions manually.".to_string()],
            }))
        }
        Err(e) => {
            tracing::warn!("Eruka upload request failed: {}", e);
            Ok(Json(UploadResponse {
                fields_extracted: 0,
                fields_confirmed: 0,
                fields_inferred: 0,
                gaps_remaining: 0,
                follow_up_questions: vec!["Document processing is temporarily unavailable.".to_string()],
            }))
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct ChatPayload {
    pub workspace_id: String,
    pub session_id: Option<String>,
    pub message: String,
}

#[derive(Debug, Serialize)]
pub struct ChatResponse {
    pub reply: String,
    pub extracted_fields: Vec<serde_json::Value>,
    pub inferred_fields: Vec<serde_json::Value>,
    pub completeness: CompletenessChange,
    pub phase: String,
    pub next_gap: Option<String>,
    pub ui_schema: Option<serde_json::Value>,
    pub session_id: String,
}

#[derive(Debug, Serialize)]
pub struct CompletenessChange {
    pub before: f64,
    pub after: f64,
}

/// POST /v1/dsprint/chat — adaptive chat with Sisyphos
pub async fn chat(
    State(_app): State<AppState>,
    Json(payload): Json<ChatPayload>,
) -> Result<Json<ChatResponse>> {
    let eruka_url = env::var("ERUKA_API_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());
    // DO NOT call ensure_authenticated() — we use X-Service-Key + X-Workspace-Id
    // so Sisyphos reads the VISITOR's workspace context, not bom@dirmacs.com's
    let eruka = ErukaProxy::new(&eruka_url);

    // 1. Get or create session
    let session_id = match payload.session_id {
        Some(sid) => sid,
        None => {
            let session = eruka.sisyphos_create_session(&payload.workspace_id).await;
            match session {
                Ok(v) => v["session_id"].as_str().unwrap_or("unknown").to_string(),
                Err(e) => {
                    tracing::warn!("Failed to create Sisyphos session: {e}");
                    uuid::Uuid::new_v4().to_string()
                }
            }
        }
    };

    // 2. Forward message to Eruka Sisyphos
    let chat_result = eruka.sisyphos_chat(&session_id, &payload.message, &payload.workspace_id).await;

    return match chat_result {
        Ok(v) => {
            let reply = v["response"].as_str().unwrap_or("").to_string();
            let extracted = v["extracted_fields"].as_array().cloned().unwrap_or_default();
            let inferred = v["inferred_fields"].as_array().cloned().unwrap_or_default();
            let before = v["completeness_before"].as_f64().unwrap_or(0.0);
            let after = v["completeness_after"].as_f64().unwrap_or(0.0);
            let phase = v["phase"].as_str().unwrap_or("ActiveExtraction").to_string();
            let next_gap = v["next_suggested_gap"].as_str().map(|s| s.to_string());

            Ok(Json(ChatResponse {
                reply,
                extracted_fields: extracted,
                inferred_fields: inferred,
                completeness: CompletenessChange { before, after },
                phase,
                next_gap,
                ui_schema: None, // Will be populated by generative UI (T21+)
                session_id,
            }))
        }
        Err(e) => {
            tracing::warn!("Sisyphos chat failed: {e}");
            Err(AppError::External(format!("Chat service unavailable: {e}")))
        }
    };
}

#[derive(Debug, Deserialize)]
pub struct ActivatePayload {
    pub workspace_id: String,
    pub email: String,
    pub tier: String,
}

#[derive(Debug, Serialize)]
pub struct ActivateResponse {
    pub tenant_id: String,
    pub api_key: String,
    pub agents_provisioned: Vec<String>,
    pub portal_url: String,
    pub mcp_url: String,
    pub workspace_id: String,
}

/// POST /v1/dsprint/activate — create tenant from survey results
pub async fn activate(
    State(app): State<AppState>,
    Json(payload): Json<ActivatePayload>,
) -> Result<Json<ActivateResponse>> {
    let pool = app.tenant_db.pool();
    let now = chrono::Utc::now().timestamp();

    // 1. Check if tenant already exists for this email (stored in name column)
    let existing = sqlx::query_scalar!(
        r#"SELECT id FROM tenants WHERE name = $1"#,
        payload.email
    ).fetch_optional(pool).await.map_err(|e| {
        tracing::warn!("Tenant lookup failed: {e}");
        AppError::External(format!("Database error: {e}"))
    })?;

    let tenant_id = if let Some(existing_id) = existing {
        // Update existing tenant
        let _ = sqlx::query!(
            r#"UPDATE tenants SET tier = $1, updated_at = $2 WHERE id = $3"#,
            payload.tier, now, existing_id
        ).execute(pool).await.map_err(|e| {
            tracing::warn!("Tenant update failed: {e}");
            AppError::External(format!("Failed to update tenant: {e}"))
        })?;
        existing_id
    } else {
        // Insert new tenant
        let new_id = uuid::Uuid::new_v4().to_string();
        let _ = sqlx::query!(
            r#"INSERT INTO tenants (id, name, tier, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5)"#,
            new_id, payload.email, payload.tier, now, now
        ).execute(pool).await.map_err(|e| {
            tracing::warn!("Tenant creation failed: {e}");
            AppError::External(format!("Failed to create tenant: {e}"))
        })?;
        new_id
    };

    // Generate API key
    let api_key = format!("ares_{}_{}", payload.email.split('@').next().unwrap_or("user"), &tenant_id[..8]);

    let key_id = uuid::Uuid::new_v4().to_string();
    let key_hash = format!("hash_{}", &api_key[..16]);
    let key_prefix = &api_key[..12];
    let _ = sqlx::query!(
        r#"INSERT INTO api_keys (id, tenant_id, key_hash, key_prefix, name, is_active, created_at, expires_at)
           VALUES ($1, $2, $3, $4, $5, $6, $7, $8)"#,
        key_id,
        tenant_id,
        key_hash,
        key_prefix,
        "dsprint-auto",
        1i32,
        now,
        None::<i64>
    ).execute(pool).await.map_err(|e| {
        tracing::warn!("API key creation failed: {e}");
        AppError::External(format!("Failed to create API key: {e}"))
    })?;

    // 3. Provision recommended agents (based on tier)
    let agents = match payload.tier.as_str() {
        "growth" => vec!["deployment-monitor", "meeting-summarizer", "dinkedin-post-creator", "lead-qualifier", "ticket-router"],
        "pro" => vec!["deployment-monitor", "meeting-summarizer", "dinkedin-post-creator", "lead-qualifier", "ticket-router", "document-qa", "gtm-pipeline", "invoice-processor"],
        _ => vec!["deployment-monitor", "meeting-summarizer", "dinkedin-post-creator"],
    };

    for agent_name in &agents {
        let agent_id = uuid::Uuid::new_v4().to_string();
        let display_name = agent_name.to_string();
        let description = format!("Auto-provisioned agent for {}", agent_name);
        let config = serde_json::json!({
            "model": "fast",
            "system_prompt": "",
            "tools": [],
            "max_tool_iterations": 3
        });
        let _ = sqlx::query!(
            r#"INSERT INTO tenant_agents (id, tenant_id, agent_name, display_name, description, config, enabled, created_at, updated_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
               ON CONFLICT (tenant_id, agent_name) DO NOTHING"#,
            agent_id,
            tenant_id,
            agent_name,
            display_name,
            description,
            config,
            true,
            now,
            now
        ).execute(pool).await;
    }

    // 4. Link workspace to tenant via Eruka (async, service auth)
    let eruka_url = env::var("ERUKA_API_URL").unwrap_or_else(|_| "http://localhost:8081".to_string());
    let ws_id = payload.workspace_id.clone();
    let tid = tenant_id.clone();
    tokio::spawn(async move {
        let eruka = ErukaProxy::new(&eruka_url);
        if let Err(e) = eruka.link_tenant(&ws_id, &tid).await {
            tracing::warn!("Failed to link workspace to tenant: {e}");
        }
    });

    // 5. Update DCRM deal stage (async)
    let dcrm_url = env::var("DCRM_BASE_URL").unwrap_or_else(|_| "http://localhost:3001".to_string());
    let email_dcrm = payload.email.clone();
    let tier_dcrm = payload.tier.clone();
    tokio::spawn(async move {
        let dcrm = DcrmClient::new(&dcrm_url);
        let _ = dcrm.log_activity(&ActivityCreate {
            contact_id: None,
            deal_id: None,
            activity_type: "dsprint_activated".to_string(),
            description: format!("Tenant activated for {}", email_dcrm),
            metadata: serde_json::json!({"tier": tier_dcrm}),
        }).await;
    });

    Ok(Json(ActivateResponse {
        tenant_id,
        api_key,
        agents_provisioned: agents.iter().map(|s| s.to_string()).collect(),
        portal_url: "portal.dirmacs.com".to_string(),
        mcp_url: "eruka.dirmacs.com/mcp".to_string(),
        workspace_id: payload.workspace_id,
    }))
}

/// GET /v1/dsprint/analytics/funnel — conversion funnel metrics
pub async fn funnel_analytics(
    State(_app): State<AppState>,
) -> Result<Json<serde_json::Value>> {
    let dcrm_url = env::var("DCRM_BASE_URL").unwrap_or_else(|_| "http://localhost:3001".to_string());
    let http = reqwest::Client::new();

    // Query DCRM for deal counts by stage
    let deals_resp = http.get(format!("{}/api/deals", dcrm_url))
        .send().await
        .map_err(|e| AppError::External(format!("DCRM unavailable: {e}")))?;

    let deals: Vec<serde_json::Value> = deals_resp.json().await.unwrap_or_default();

    // Count deals at each DSprint PLG stage
    let mut funnel = serde_json::json!({
        "survey_submitted": 0,
        "results_viewed": 0,
        "activated": 0,
        "first_agent_run": 0,
        "total_dsprint_deals": 0,
    });

    for deal in &deals {
        let stage = deal["stage"].as_str().unwrap_or("");
        let pipeline = deal["pipeline"].as_str().unwrap_or("");
        if pipeline == "dsprint" || stage.starts_with("dsprint") || stage == "first_agent_run" {
            funnel["total_dsprint_deals"] = serde_json::json!(
                funnel["total_dsprint_deals"].as_u64().unwrap_or(0) + 1
            );
            match stage {
                "dsprint_submitted" => funnel["survey_submitted"] = serde_json::json!(funnel["survey_submitted"].as_u64().unwrap_or(0) + 1),
                "dsprint_activated" => funnel["activated"] = serde_json::json!(funnel["activated"].as_u64().unwrap_or(0) + 1),
                "first_agent_run" => funnel["first_agent_run"] = serde_json::json!(funnel["first_agent_run"].as_u64().unwrap_or(0) + 1),
                _ => {}
            }
        }
    }

    // Compute conversion rates
    let total = funnel["total_dsprint_deals"].as_f64().unwrap_or(1.0).max(1.0);
    let activated = funnel["activated"].as_f64().unwrap_or(0.0);

    Ok(Json(serde_json::json!({
        "funnel": funnel,
        "conversion_rates": {
            "submit_to_activate": format!("{:.1}%", (activated / total) * 100.0),
            "activate_to_first_run": format!("{:.1}%",
                (funnel["first_agent_run"].as_f64().unwrap_or(0.0) / activated.max(1.0)) * 100.0),
        },
        "timestamp": chrono::Utc::now().to_rfc3339(),
    })))
}
