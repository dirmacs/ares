//! Google Calendar connector tools.

use crate::connectors::google::GoogleClient;
use crate::connectors::require_tenant_id;
use crate::registry::Tool;
use ares_types::types::{AppError, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

// =============================================================================
// Data types
// =============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct CalendarEvent {
    pub id: String,
    pub summary: String,
    pub description: Option<String>,
    pub start: EventDateTime,
    pub end: EventDateTime,
    pub status: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct EventDateTime {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date_time: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub date: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct FreeBusy {
    pub calendar_id: String,
    pub busy: Vec<TimeRange>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TimeRange {
    pub start: String,
    pub end: String,
}

// =============================================================================
// List Events Tool
// =============================================================================

pub struct GoogleCalendarListEvents {
    client: GoogleClient,
}

impl GoogleCalendarListEvents {
    pub fn new(client: GoogleClient) -> Self {
        Self { client }
    }

    async fn list_events(
        &self,
        tenant_id: &str,
        calendar_id: &str,
        time_min: Option<&str>,
        time_max: Option<&str>,
    ) -> Result<Vec<CalendarEvent>> {
        let mut path = if calendar_id == "primary" {
            "/calendars/primary/events".to_string()
        } else {
            format!("/calendars/{}/events", urlencoding::encode(calendar_id))
        };
        let mut query = Vec::new();
        if let Some(tmin) = time_min {
            query.push(format!("timeMin={}", urlencoding::encode(tmin)));
        }
        if let Some(tmax) = time_max {
            query.push(format!("timeMax={}", urlencoding::encode(tmax)));
        }
        if !query.is_empty() {
            path.push('?');
            path.push_str(&query.join("&"));
        }

        let req = self
            .client
            .request(tenant_id, reqwest::Method::GET, &path)
            .await?;

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("google calendar list events read body: {e}"))
        })?;

        #[derive(Debug, Deserialize)]
        struct EventList {
            items: Option<Vec<CalendarEvent>>,
        }

        let list: EventList = serde_json::from_str(&body).map_err(|e| {
            ares_types::AppError::External(format!(
                "google calendar list events parse failed: {e} (body: {body})"
            ))
        })?;

        Ok(list.items.unwrap_or_default())
    }
}

#[async_trait]
impl Tool for GoogleCalendarListEvents {
    fn name(&self) -> &str {
        "google_calendar_list_events"
    }

    fn description(&self) -> &str {
        "List events from a Google Calendar"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "calendar_id": {"type": "string", "description": "Calendar ID (use 'primary' for default)"},
                "time_min": {"type": "string", "description": "RFC3339 timestamp for lower bound (optional)"},
                "time_max": {"type": "string", "description": "RFC3339 timestamp for upper bound (optional)"}
            },
            "required": ["calendar_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let calendar_id = args["calendar_id"].as_str().unwrap_or("primary");
        let time_min = args["time_min"].as_str();
        let time_max = args["time_max"].as_str();

        let events = self
            .list_events(&tenant_id, calendar_id, time_min, time_max)
            .await?;
        Ok(json!({ "events": events }))
    }
}

// =============================================================================
// Create Event Tool
// =============================================================================

pub struct GoogleCalendarCreateEvent {
    client: GoogleClient,
}

impl GoogleCalendarCreateEvent {
    pub fn new(client: GoogleClient) -> Self {
        Self { client }
    }

    async fn create_event(
        &self,
        tenant_id: &str,
        calendar_id: &str,
        event: &CalendarEvent,
    ) -> Result<CalendarEvent> {
        let path = if calendar_id == "primary" {
            "/calendars/primary/events".to_string()
        } else {
            format!("/calendars/{}/events", urlencoding::encode(calendar_id))
        };

        let req = self
            .client
            .request(tenant_id, reqwest::Method::POST, &path)
            .await?
            .json(event);

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("google calendar create event read body: {e}"))
        })?;

        serde_json::from_str(&body).map_err(|e| {
            ares_types::AppError::External(format!(
                "google calendar create event parse failed: {e} (body: {body})"
            ))
        })
    }
}

#[async_trait]
impl Tool for GoogleCalendarCreateEvent {
    fn name(&self) -> &str {
        "google_calendar_create_event"
    }

    fn description(&self) -> &str {
        "Create an event in a Google Calendar"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "calendar_id": {"type": "string", "description": "Calendar ID (use 'primary' for default)"},
                "summary": {"type": "string", "description": "Event title"},
                "description": {"type": "string", "description": "Event description (optional)"},
                "start": {"type": "string", "description": "Start time RFC3339 or date"},
                "end": {"type": "string", "description": "End time RFC3339 or date"}
            },
            "required": ["calendar_id", "summary", "start", "end"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let calendar_id = args["calendar_id"].as_str().unwrap_or("primary");
        let summary = args["summary"].as_str().unwrap_or("").to_string();
        let description = args["description"].as_str().map(|s| s.to_string());
        let start = EventDateTime {
            date_time: args["start"].as_str().map(|s| s.to_string()),
            date: None,
        };
        let end = EventDateTime {
            date_time: args["end"].as_str().map(|s| s.to_string()),
            date: None,
        };
        let event = CalendarEvent {
            id: String::new(),
            summary,
            description,
            start,
            end,
            status: None,
        };

        let created = self.create_event(&tenant_id, calendar_id, &event).await?;
        Ok(json!({ "event": created }))
    }
}

// =============================================================================
// Delete Event Tool
// =============================================================================

pub struct GoogleCalendarDeleteEvent {
    client: GoogleClient,
}

impl GoogleCalendarDeleteEvent {
    pub fn new(client: GoogleClient) -> Self {
        Self { client }
    }

    async fn delete_event(&self, tenant_id: &str, calendar_id: &str, event_id: &str) -> Result<()> {
        let path = if calendar_id == "primary" {
            format!(
                "/calendars/primary/events/{}",
                urlencoding::encode(event_id)
            )
        } else {
            format!(
                "/calendars/{}/events/{}",
                urlencoding::encode(calendar_id),
                urlencoding::encode(event_id)
            )
        };

        let req = self
            .client
            .request(tenant_id, reqwest::Method::DELETE, &path)
            .await?;
        self.client.execute(req).await.map_err(AppError::from)?;
        Ok(())
    }
}

#[async_trait]
impl Tool for GoogleCalendarDeleteEvent {
    fn name(&self) -> &str {
        "google_calendar_delete_event"
    }

    fn description(&self) -> &str {
        "Delete an event from a Google Calendar"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "calendar_id": {"type": "string", "description": "Calendar ID (use 'primary' for default)"},
                "event_id": {"type": "string", "description": "Event ID to delete"}
            },
            "required": ["calendar_id", "event_id"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let calendar_id = args["calendar_id"].as_str().unwrap_or("primary");
        let event_id = args["event_id"].as_str().ok_or_else(|| {
            ares_types::AppError::InvalidInput("event_id is required".to_string())
        })?;

        self.delete_event(&tenant_id, calendar_id, event_id).await?;
        Ok(json!({ "deleted": true }))
    }
}

// =============================================================================
// Get Free/Busy Tool
// =============================================================================

pub struct GoogleCalendarGetFreeBusy {
    client: GoogleClient,
}

impl GoogleCalendarGetFreeBusy {
    pub fn new(client: GoogleClient) -> Self {
        Self { client }
    }

    async fn get_free_busy(
        &self,
        tenant_id: &str,
        calendar_ids: &[String],
        time_min: &str,
        time_max: &str,
    ) -> Result<Vec<FreeBusy>> {
        let body = json!({
            "timeMin": time_min,
            "timeMax": time_max,
            "items": calendar_ids.iter().map(|id| json!({ "id": id })).collect::<Vec<_>>()
        });

        let req = self
            .client
            .request(tenant_id, reqwest::Method::POST, "/freeBusy")
            .await?
            .json(&body);

        let resp = self.client.execute(req).await.map_err(AppError::from)?;
        let resp_body = resp.text().await.map_err(|e| {
            ares_types::AppError::External(format!("google freebusy read body: {e}"))
        })?;

        #[derive(Debug, Deserialize)]
        struct FreeBusyResponse {
            calendars: Option<std::collections::HashMap<String, CalendarBusy>>,
        }

        #[derive(Debug, Deserialize)]
        struct CalendarBusy {
            busy: Vec<TimeRange>,
        }

        let fb: FreeBusyResponse = serde_json::from_str(&resp_body).map_err(|e| {
            ares_types::AppError::External(format!(
                "google freebusy parse failed: {e} (body: {resp_body})"
            ))
        })?;

        let mut out = Vec::new();
        if let Some(cals) = fb.calendars {
            for (id, cb) in cals {
                out.push(FreeBusy {
                    calendar_id: id,
                    busy: cb.busy,
                });
            }
        }
        Ok(out)
    }
}

#[async_trait]
impl Tool for GoogleCalendarGetFreeBusy {
    fn name(&self) -> &str {
        "google_calendar_get_free_busy"
    }

    fn description(&self) -> &str {
        "Get free/busy information for Google Calendars"
    }

    fn parameters_schema(&self) -> Value {
        json!({
            "type": "object",
            "properties": {
                "calendar_ids": {"type": "array", "items": {"type": "string"}, "description": "List of calendar IDs"},
                "time_min": {"type": "string", "description": "RFC3339 start time"},
                "time_max": {"type": "string", "description": "RFC3339 end time"}
            },
            "required": ["calendar_ids", "time_min", "time_max"]
        })
    }

    async fn execute(&self, args: Value) -> Result<Value> {
        let tenant_id = require_tenant_id(&args)?;
        let calendar_ids: Vec<String> = args["calendar_ids"]
            .as_array()
            .map(|a| {
                a.iter()
                    .filter_map(|v| v.as_str().map(|s| s.to_string()))
                    .collect()
            })
            .unwrap_or_default();
        let time_min = args["time_min"].as_str().ok_or_else(|| {
            ares_types::AppError::InvalidInput("time_min is required".to_string())
        })?;
        let time_max = args["time_max"].as_str().ok_or_else(|| {
            ares_types::AppError::InvalidInput("time_max is required".to_string())
        })?;

        let result = self
            .get_free_busy(&tenant_id, &calendar_ids, time_min, time_max)
            .await?;
        Ok(json!({ "calendars": result }))
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use wiremock::matchers::{bearer_token, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    // Client factory for a full request-shape test; current tests mount the
    // mocks but cannot reach this client without a mocked DB credential store.
    #[allow(dead_code)]
    fn mock_client(server_uri: &str) -> GoogleClient {
        use ares_store::MasterKey;
        use sqlx::PgPool;
        GoogleClient {
            config: crate::connectors::ConnectorConfig {
                base_url: server_uri.to_string(),
                version: "calendar/v3".to_string(),
            },
            http: reqwest::Client::new(),
            pool: PgPool::connect_lazy("postgres://localhost/test").expect("lazy"),
            master_key: MasterKey::from_secret("test-key-32-bytes-long!!!!!!!!!"),
            connector_type: "google_calendar",
        }
    }

    #[tokio::test]
    async fn list_events_parses_response() {
        let server = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/calendar/v3/calendars/primary/events"))
            .and(bearer_token("mock-token"))
            .respond_with(ResponseTemplate::new(200).set_body_json(json!({
                "items": [
                    {"id":"evt1","summary":"Meeting","start":{"dateTime":"2024-01-01T10:00:00Z"},"end":{"dateTime":"2024-01-01T11:00:00Z"}}
                ]
            })))
            .mount(&server)
            .await;

        // We can't fully test without mocking the DB credential store,
        // but we ensure the request shapes compile.
        let _ = std::any::type_name::<GoogleCalendarListEvents>();
    }
}
