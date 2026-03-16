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

#[derive(Debug, Deserialize)]
pub struct SubmitPayload {
    pub company_name: String,
    pub industry: String,
    pub team_size: String,
    pub email: String,
    pub answers: HashMap<String, serde_json::Value>,
    #[serde(rename = "ref")]
    pub referral: Option<String>,
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
    State(_app): State<AppState>,
    Json(payload): Json<SubmitPayload>,
) -> Result<Json<SubmitResponse>> {
    // 1. Create ErukaProxy from env
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

    // 3. Create Sisyphos session (graceful failure)
    let session = eruka.sisyphos_create_session(&payload.email).await;
    let session_id = match &session {
        Ok(v) => v["id"].as_str().or(v["session_id"].as_str()).unwrap_or("unknown").to_string(),
        Err(e) => {
            tracing::warn!("Eruka sisyphos_create_session failed: {e}");
            uuid::Uuid::new_v4().to_string()
        }
    };

    // 4. Write answer fields to Eruka (fire-and-forget, log errors)
    let fields_confirmed = payload.answers.len() as u32;
    for (path, value) in &payload.answers {
        let parts: Vec<&str> = path.splitn(2, '.').collect();
        if parts.len() == 2 {
            let http = reqwest::Client::new();
            let url = format!("{}/api/workspaces/{}/context", eruka_url, workspace_id);
            let body = serde_json::json!({
                "category": parts[0],
                "field": parts[1],
                "value": value,
                "confidence": 1.0,
                "source": "dsprint_survey"
            });
            if let Err(e) = http.post(&url).json(&body).send().await {
                tracing::warn!("Eruka write field {path} failed: {e}");
            }
        }
    }

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

    // 10. Return response
    let tree_stats = TreeStats {
        roots_planted: payload.answers.keys().filter(|k| k.contains('.')).map(|k| k.split('.').next().unwrap_or("")).collect::<std::collections::HashSet<_>>().len() as u32,
        branches_initialized: payload.answers.len() as u32,
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
    let eruka = ErukaProxy::new(&eruka_url);

    // 1. Get workspace data from Eruka
    let ws_data = eruka.get_workspace(&workspace_id).await;
    let company_name = match &ws_data {
        Ok(v) => v["name"].as_str().unwrap_or("Unknown").to_string(),
        Err(e) => {
            tracing::warn!("Eruka get_workspace failed: {e}");
            "Unknown".to_string()
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
        pain_points: vec![],
        team_size: "11-50".to_string(), // Default, would come from workspace data
        industry: "General".to_string(),
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

    // 9. Build tree summary
    let _total_gaps: u32 = domains.values().map(|d| d.gaps).sum();
    let total_nodes: u32 = domains.values().map(|d| d.score / 10).sum();
    let tree_summary = TreeSummary {
        roots: domains.len() as u32,
        trunk: 0,
        branches: domains.iter().map(|(name, detail)| (name.clone(), detail.score / 20)).collect(),
        leaves: 0,
        total_nodes,
        coverage: format!("{}%", (overall_completeness * 100.0) as u32),
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
