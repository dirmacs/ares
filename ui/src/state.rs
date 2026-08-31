//! Global application state

use leptos::prelude::*;
use gloo_storage::{LocalStorage, Storage};
use crate::types::{AuthResponse, Conversation, AgentInfo, WorkflowInfo};

const STORAGE_KEY_TOKEN: &str = "ares_token";
const STORAGE_KEY_REFRESH: &str = "ares_refresh_token";
const STORAGE_KEY_CONVERSATION: &str = "ares_conversation_id";

/// Global application state
#[derive(Clone)]
pub struct AppState {
    /// Authentication token
    pub token: RwSignal<Option<String>>,
    /// Refresh token
    pub refresh_token: RwSignal<Option<String>>,
    /// Available agents
    pub agents: RwSignal<Vec<AgentInfo>>,
    /// Available workflows
    pub workflows: RwSignal<Vec<WorkflowInfo>>,
    /// Current conversation
    pub conversation: RwSignal<Conversation>,
    /// Loading state
    pub is_loading: RwSignal<bool>,
    /// Error message
    pub error: RwSignal<Option<String>>,
    /// API base URL
    pub api_base: RwSignal<String>,
}

impl AppState {
    pub fn new() -> Self {
        // Try to load from localStorage
        let (token, refresh, conversation_id) = Self::load_from_storage();

        Self {
            token: RwSignal::new(token),
            refresh_token: RwSignal::new(refresh),
            agents: RwSignal::new(vec![]),
            workflows: RwSignal::new(vec![]),
            conversation: RwSignal::new(Conversation {
                id: conversation_id,
                ..Conversation::default()
            }),
            is_loading: RwSignal::new(false),
            error: RwSignal::new(None),
            api_base: RwSignal::new("http://localhost:3000".to_string()),
        }
    }

    fn load_from_storage() -> (Option<String>, Option<String>, Option<String>) {
        let token: Option<String> = LocalStorage::get(STORAGE_KEY_TOKEN).ok();
        let refresh: Option<String> = LocalStorage::get(STORAGE_KEY_REFRESH).ok();
        let conversation_id: Option<String> = LocalStorage::get(STORAGE_KEY_CONVERSATION)
            .ok()
            .filter(|id: &String| !id.trim().is_empty());
        (token, refresh, conversation_id)
    }

    pub fn persist_conversation_id(&self, id: Option<&str>) {
        match id.map(str::trim).filter(|s| !s.is_empty()) {
            Some(id) => {
                let _ = LocalStorage::set(STORAGE_KEY_CONVERSATION, id);
            }
            None => LocalStorage::delete(STORAGE_KEY_CONVERSATION),
        }
    }

    pub fn save_auth(&self, auth: &AuthResponse) {
        let _ = LocalStorage::set(STORAGE_KEY_TOKEN, &auth.access_token);
        let _ = LocalStorage::set(STORAGE_KEY_REFRESH, &auth.refresh_token);
        
        self.token.set(Some(auth.access_token.clone()));
        self.refresh_token.set(Some(auth.refresh_token.clone()));
    }

    pub fn clear_auth(&self) {
        LocalStorage::delete(STORAGE_KEY_TOKEN);
        LocalStorage::delete(STORAGE_KEY_REFRESH);
        LocalStorage::delete(STORAGE_KEY_CONVERSATION);

        self.token.set(None);
        self.refresh_token.set(None);
        self.conversation.set(Conversation::default());
    }

    pub fn is_authenticated(&self) -> bool {
        self.token.get().is_some()
    }

    pub fn set_error(&self, msg: impl Into<String>) {
        self.error.set(Some(msg.into()));
    }

    pub fn clear_error(&self) {
        self.error.set(None);
    }
}

impl Default for AppState {
    fn default() -> Self {
        Self::new()
    }
}
