use crate::types::{AppError, Result};
use axum::Json;
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct InquiryRequest {
    pub company_name: String,
    pub contact_email: String,
    pub tier_interest: String,
    pub message: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct InquiryResponse {
    pub status: String,
    pub message: String,
}

pub async fn public_inquire(Json(req): Json<InquiryRequest>) -> Result<Json<InquiryResponse>> {
    // Validate inputs
    if req.company_name.trim().is_empty() || req.contact_email.trim().is_empty() {
        return Err(AppError::InvalidInput(
            "company_name and contact_email are required".to_string(),
        ));
    }

    // Create a POM dissue for the new lead
    let pom_url = "http://localhost:3002/api/dissues";
    let title = format!(
        "New Lead: {} -- {}",
        req.company_name.trim(),
        req.tier_interest.trim()
    );
    let description = format!(
        "Contact: {}\nTier interest: {}\nMessage: {}",
        req.contact_email,
        req.tier_interest,
        req.message.as_deref().unwrap_or("(none)")
    );

    let body = serde_json::json!({
        "title": title,
        "priority": "high",
        "description": description,
        "sprint_number": 22
    });

    // Fire-and-forget POM dissue creation (don't fail the response if POM is down)
    let client = reqwest::Client::new();
    let _ = client
        .post(pom_url)
        .json(&body)
        .timeout(std::time::Duration::from_secs(5))
        .send()
        .await;

    Ok(Json(InquiryResponse {
        status: "ok".to_string(),
        message: format!(
            "Thanks for your inquiry, {}! We'll be in touch at {} shortly.",
            req.company_name, req.contact_email
        ),
    }))
}
