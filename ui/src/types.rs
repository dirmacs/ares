//! API types matching the ARES server

use serde::{Deserialize, Serialize};

/// Login request
#[derive(Debug, Clone, Serialize)]
pub struct LoginRequest {
    pub email: String,
    pub password: String,
}

/// Register request
#[derive(Debug, Clone, Serialize)]
pub struct RegisterRequest {
    pub email: String,
    pub password: String,
    pub name: String,
}

/// Authentication response
#[derive(Debug, Clone, Deserialize)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    pub expires_in: i64,
}

/// A multimodal content part on a chat request.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ContentPart {
    Text {
        text: String,
    },
    ImageUrl {
        url: String,
    },
    ImageBase64 {
        mime: String,
        data: String,
    },
    FileUrl {
        url: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        mime: Option<String>,
    },
    FileBase64 {
        mime: String,
        data: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        name: Option<String>,
    },
}

impl ContentPart {
    /// Parse a `data:<mime>;base64,<data>` URL into an image or file part.
    pub fn from_data_url(data_url: &str, filename: impl Into<String>) -> Result<Self, String> {
        let rest = data_url
            .strip_prefix("data:")
            .ok_or_else(|| "FileReader result is not a data URL".to_string())?;
        let (meta, data) = rest
            .split_once(',')
            .ok_or_else(|| "data URL missing payload".to_string())?;
        let mime = meta
            .split(';')
            .next()
            .map(str::trim)
            .filter(|m| !m.is_empty())
            .unwrap_or("application/octet-stream")
            .to_string();
        let data = data.to_string();
        if mime.starts_with("image/") {
            Ok(ContentPart::ImageBase64 { mime, data })
        } else {
            Ok(ContentPart::FileBase64 {
                mime,
                data,
                name: Some(filename.into()),
            })
        }
    }

    pub fn chip_label(&self) -> String {
        match self {
            ContentPart::Text { text } => text.clone(),
            ContentPart::ImageUrl { url } => url.clone(),
            ContentPart::ImageBase64 { mime, .. } => format!("image ({mime})"),
            ContentPart::FileUrl { url, .. } => url
                .rsplit('/')
                .next()
                .filter(|s| !s.is_empty())
                .unwrap_or(url)
                .to_string(),
            ContentPart::FileBase64 { name, mime, .. } => {
                name.clone().unwrap_or_else(|| mime.clone())
            }
        }
    }

    pub fn image_src(&self) -> Option<String> {
        match self {
            ContentPart::ImageUrl { url } => Some(url.clone()),
            ContentPart::ImageBase64 { mime, data } => Some(format!("data:{mime};base64,{data}")),
            _ => None,
        }
    }
}

/// Chat request
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub context_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub agent_type: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parts: Option<Vec<ContentPart>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_response_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub web_search: Option<bool>,
}

/// Chat response
#[derive(Debug, Clone, Deserialize)]
pub struct ChatResponse {
    pub response: String,
    pub context_id: String,
    pub agent: String,
    #[serde(default)]
    pub sources: Option<Vec<Source>>,
}

/// Source reference in responses
#[derive(Debug, Clone, Deserialize)]
pub struct Source {
    pub title: String,
    pub url: Option<String>,
    pub relevance_score: f32,
}

/// Tool call information
#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct ToolCallInfo {
    pub name: String,
    pub arguments: serde_json::Value,
    #[serde(default)]
    pub result: Option<String>,
}

/// Agent info from the API
#[derive(Debug, Clone, Deserialize)]
pub struct AgentInfo {
    pub agent_type: String,
    pub name: String,
    pub description: String,
}

/// Workflow info
#[derive(Debug, Clone, Deserialize)]
pub struct WorkflowInfo {
    pub name: String,
    pub entry_agent: String,
    pub fallback_agent: Option<String>,
    pub max_depth: u8,
    pub max_iterations: u8,
    pub parallel_subagents: bool,
}

/// User memory
#[derive(Debug, Clone, Deserialize)]
pub struct UserMemory {
    #[serde(default)]
    pub preferences: std::collections::HashMap<String, String>,
    #[serde(default)]
    pub facts: Vec<String>,
}

/// Error response from API
#[derive(Debug, Clone, Deserialize)]
pub struct ApiError {
    pub error: String,
    #[serde(default)]
    pub details: Option<String>,
}

/// Message in a conversation
#[derive(Debug, Clone, PartialEq)]
pub struct Message {
    pub id: String,
    pub role: MessageRole,
    pub content: String,
    pub parts: Vec<ContentPart>,
    pub timestamp: chrono::DateTime<chrono::Utc>,
    pub agent_type: Option<String>,
    pub tool_calls: Vec<ToolCallInfo>,
    pub is_streaming: bool,
}

/// Message role
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MessageRole {
    User,
    Assistant,
    System,
}

impl MessageRole {
    /// Parse a REST transcript role (`user` / `assistant` / `system`).
    pub fn from_api(role: &str) -> Result<Self, String> {
        match role.trim().to_ascii_lowercase().as_str() {
            "user" => Ok(Self::User),
            "assistant" => Ok(Self::Assistant),
            "system" => Ok(Self::System),
            other => Err(format!("invalid conversation message role: {other}")),
        }
    }
}

impl Message {
    pub fn user(content: impl Into<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: MessageRole::User,
            content: content.into(),
            parts: vec![],
            timestamp: chrono::Utc::now(),
            agent_type: None,
            tool_calls: vec![],
            is_streaming: false,
        }
    }

    pub fn assistant(content: impl Into<String>, agent_type: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: MessageRole::Assistant,
            content: content.into(),
            parts: vec![],
            timestamp: chrono::Utc::now(),
            agent_type,
            tool_calls: vec![],
            is_streaming: false,
        }
    }

    pub fn streaming(agent_type: Option<String>) -> Self {
        Self {
            id: uuid::Uuid::new_v4().to_string(),
            role: MessageRole::Assistant,
            content: String::new(),
            parts: vec![],
            timestamp: chrono::Utc::now(),
            agent_type,
            tool_calls: vec![],
            is_streaming: true,
        }
    }
}

/// Conversation state
#[derive(Debug, Clone, Default)]
pub struct Conversation {
    pub id: Option<String>,
    pub messages: Vec<Message>,
    pub selected_agent: Option<String>,
}

/// REST transcript message from `GET /api/conversations/{id}`.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationMessage {
    pub id: String,
    pub role: String,
    pub content: String,
    pub created_at: String,
    /// Omitted from JSON when empty on the server.
    #[serde(default)]
    pub parts: Vec<ContentPart>,
}

/// REST transcript from `GET /api/conversations/{id}`.
#[derive(Debug, Clone, Deserialize)]
pub struct ConversationDetails {
    pub id: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub messages: Vec<ConversationMessage>,
    #[serde(default)]
    pub created_at: String,
    #[serde(default)]
    pub updated_at: String,
}

impl ConversationMessage {
    /// Map a REST message into UI state. Invalid role or timestamp is an error.
    pub fn into_message(self) -> Result<Message, String> {
        let role = MessageRole::from_api(&self.role)?;
        let timestamp = chrono::DateTime::parse_from_rfc3339(&self.created_at)
            .map(|dt| dt.with_timezone(&chrono::Utc))
            .map_err(|e| format!("invalid conversation message timestamp: {e}"))?;
        Ok(Message {
            id: self.id,
            role,
            content: self.content,
            parts: self.parts,
            timestamp,
            agent_type: None,
            tool_calls: vec![],
            is_streaming: false,
        })
    }
}

impl ConversationDetails {
    /// Map every REST message. Any invalid row fails the whole transcript.
    pub fn into_messages(self) -> Result<Vec<Message>, String> {
        self.messages
            .into_iter()
            .map(ConversationMessage::into_message)
            .collect()
    }
}

/// Merge a server transcript with in-memory local messages.
///
/// Walks both lists in order. A local row with the same role+content as the next
/// server row is consumed (server wins, so persisted `parts` replace the local
/// copy). Empty streaming placeholders are skipped. Trailing unmatched local
/// rows (the just-sent turn) are appended so a reload cannot duplicate or drop
/// an in-flight send.
pub fn merge_transcript(server: Vec<Message>, local: Vec<Message>) -> Vec<Message> {
    let mut result = Vec::with_capacity(server.len().max(local.len()));
    let mut local = local.into_iter().peekable();
    for s in server {
        loop {
            match local.peek() {
                Some(l) if l.is_streaming && l.content.is_empty() => {
                    let _ = local.next();
                }
                Some(l) if l.role == s.role && l.content == s.content => {
                    let _ = local.next();
                    break;
                }
                _ => break,
            }
        }
        result.push(s);
    }
    for l in local {
        if l.is_streaming && l.content.is_empty() {
            continue;
        }
        result.push(l);
    }
    result
}

/// Streaming event from the chat/stream endpoint
#[derive(Debug, Clone, Deserialize)]
pub struct StreamEvent {
    /// Event type: "start", "token", "done", "error"
    pub event: String,
    /// Token content (for "token" events)
    #[serde(default)]
    pub content: Option<String>,
    /// Agent type that handled the request
    #[serde(default)]
    pub agent: Option<String>,
    /// Context ID for the conversation
    #[serde(default)]
    pub context_id: Option<String>,
    /// Error message (for "error" events)
    #[serde(default)]
    pub error: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{TimeZone, Utc};

    fn ts() -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2024, 6, 1, 12, 0, 0).unwrap()
    }

    fn msg(role: MessageRole, content: &str, parts: Vec<ContentPart>, id: &str) -> Message {
        Message {
            id: id.to_string(),
            role,
            content: content.to_string(),
            parts,
            timestamp: ts(),
            agent_type: None,
            tool_calls: vec![],
            is_streaming: false,
        }
    }

    #[test]
    fn conversation_details_omitted_parts_default_empty() {
        let json = r#"{
            "id":"c1",
            "title":null,
            "messages":[{
                "id":"c1-0",
                "role":"user",
                "content":"hi",
                "created_at":"2024-01-01T00:00:00Z"
            }],
            "created_at":"2024-01-01T00:00:00Z",
            "updated_at":"2024-01-01T00:00:00Z"
        }"#;
        let details: ConversationDetails = serde_json::from_str(json).unwrap();
        assert!(details.messages[0].parts.is_empty());
        let mapped = details.into_messages().unwrap();
        assert_eq!(mapped[0].role, MessageRole::User);
        assert_eq!(mapped[0].content, "hi");
        assert!(mapped[0].parts.is_empty());
    }

    #[test]
    fn conversation_details_maps_image_and_file_parts() {
        let json = r#"{
            "id":"c1",
            "messages":[{
                "id":"c1-0",
                "role":"user",
                "content":"see image",
                "created_at":"2024-06-01T12:00:00Z",
                "parts":[
                    {"type":"image_url","url":"https://example.com/a.png"},
                    {"type":"file_url","url":"https://example.com/doc.pdf","mime":"application/pdf"}
                ]
            }]
        }"#;
        let details: ConversationDetails = serde_json::from_str(json).unwrap();
        let mapped = details.into_messages().unwrap();
        assert_eq!(
            mapped[0].parts,
            vec![
                ContentPart::ImageUrl {
                    url: "https://example.com/a.png".to_string(),
                },
                ContentPart::FileUrl {
                    url: "https://example.com/doc.pdf".to_string(),
                    mime: Some("application/pdf".to_string()),
                },
            ]
        );
    }

    #[test]
    fn into_message_rejects_unknown_role() {
        let row = ConversationMessage {
            id: "c1-0".to_string(),
            role: "narrator".to_string(),
            content: "hi".to_string(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            parts: vec![],
        };
        let err = row.into_message().unwrap_err();
        assert!(err.starts_with("invalid conversation message role:"));
    }

    #[test]
    fn into_message_rejects_invalid_timestamp() {
        let row = ConversationMessage {
            id: "c1-0".to_string(),
            role: "user".to_string(),
            content: "hi".to_string(),
            created_at: "not-a-date".to_string(),
            parts: vec![],
        };
        let err = row.into_message().unwrap_err();
        assert!(err.starts_with("invalid conversation message timestamp:"));
    }

    #[test]
    fn into_messages_fails_closed_on_one_bad_row() {
        let details = ConversationDetails {
            id: "c1".to_string(),
            title: None,
            messages: vec![
                ConversationMessage {
                    id: "c1-0".to_string(),
                    role: "user".to_string(),
                    content: "hi".to_string(),
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    parts: vec![],
                },
                ConversationMessage {
                    id: "c1-1".to_string(),
                    role: "nope".to_string(),
                    content: "x".to_string(),
                    created_at: "2024-01-01T00:00:00Z".to_string(),
                    parts: vec![],
                },
            ],
            created_at: String::new(),
            updated_at: String::new(),
        };
        assert!(details.into_messages().is_err());
    }

    #[test]
    fn merge_transcript_prefers_server_parts_and_keeps_trailing_local() {
        let server = vec![
            msg(
                MessageRole::User,
                "hello",
                vec![ContentPart::ImageUrl {
                    url: "https://example.com/a.png".to_string(),
                }],
                "c1-0",
            ),
            msg(MessageRole::Assistant, "hi!", vec![], "c1-1"),
        ];
        let local = vec![
            msg(MessageRole::User, "hello", vec![], "local-u"),
            msg(MessageRole::Assistant, "hi!", vec![], "local-a"),
            msg(MessageRole::User, "next", vec![], "local-u2"),
        ];
        let merged = merge_transcript(server, local);
        assert_eq!(merged.len(), 3);
        assert_eq!(merged[0].id, "c1-0");
        assert_eq!(
            merged[0].parts,
            vec![ContentPart::ImageUrl {
                url: "https://example.com/a.png".to_string(),
            }]
        );
        assert_eq!(merged[1].id, "c1-1");
        assert_eq!(merged[2].id, "local-u2");
        assert_eq!(merged[2].content, "next");
    }

    #[test]
    fn merge_transcript_skips_empty_streaming_placeholder() {
        let server = vec![msg(MessageRole::User, "hello", vec![], "c1-0")];
        let mut placeholder = Message::streaming(None);
        placeholder.content.clear();
        let local = vec![
            msg(MessageRole::User, "hello", vec![], "local-u"),
            placeholder,
        ];
        let merged = merge_transcript(server, local);
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].id, "c1-0");
    }
}
