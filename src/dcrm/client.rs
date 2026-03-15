use anyhow::Result;
use reqwest::Client;
use serde::{Serialize, Deserialize};
use std::time::Duration;

/// DCRM HTTP client for interacting with the DCRM API
#[derive(Clone)]
pub struct DcrmClient {
    http: Client,
    base_url: String,
}

/// Request payload for creating a contact
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ContactCreate {
    pub name: String,
    pub email: String,
    pub company: Option<String>,
    pub industry: Option<String>,
    pub source: Option<String>,
    pub metadata: serde_json::Value,
}

/// Request payload for creating a deal
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DealCreate {
    pub contact_id: String,
    pub title: String,
    pub stage: String,
    pub value: f64,
    pub currency: String,
    pub pipeline: String,
    pub metadata: serde_json::Value,
}

/// Request payload for logging an activity
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct ActivityCreate {
    pub contact_id: Option<String>,
    pub deal_id: Option<String>,
    pub activity_type: String,
    pub description: String,
    pub metadata: serde_json::Value,
}

/// Request payload for updating a deal stage
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct DealStageUpdate {
    pub stage: String,
}

impl DcrmClient {
    /// Creates a new DCRM client with the given base URL
    pub fn new(base_url: &str) -> Self {
        let http = Client::builder()
            .timeout(Duration::from_secs(10))
            .build()
            .expect("Failed to build reqwest client");

        Self {
            http,
            base_url: base_url.to_string(),
        }
    }

    /// Creates a new contact in DCRM
    pub async fn create_contact(&self, payload: &ContactCreate) -> Result<serde_json::Value> {
        let url = format!("{}/api/contacts", self.base_url);
        let response = self.http.post(&url).json(payload).send().await?;
        let response_value = response.json().await?;
        Ok(response_value)
    }

    /// Creates a new deal in DCRM
    pub async fn create_deal(&self, payload: &DealCreate) -> Result<serde_json::Value> {
        let url = format!("{}/api/deals", self.base_url);
        let response = self.http.post(&url).json(payload).send().await?;
        let response_value = response.json().await?;
        Ok(response_value)
    }

    /// Logs an activity (note, email, call, etc.) in DCRM
    pub async fn log_activity(&self, payload: &ActivityCreate) -> Result<serde_json::Value> {
        let url = format!("{}/api/activities", self.base_url);
        let response = self.http.post(&url).json(payload).send().await?;
        let response_value = response.json().await?;
        Ok(response_value)
    }

    /// Updates the stage of a deal in DCRM
    pub async fn update_deal_stage(
        &self,
        deal_id: &str,
        payload: &DealStageUpdate,
    ) -> Result<serde_json::Value> {
        let url = format!("{}/api/deals/{}/stage", self.base_url, deal_id);
        let response = self.http.put(&url).json(payload).send().await?;
        let response_value = response.json().await?;
        Ok(response_value)
    }
}
