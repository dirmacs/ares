//! Memory management module for conversation context and user memory.
//!
//! This module provides utilities for:
//! - Building agent context with memory
//! - Formatting memory for LLM prompts
//! - Managing conversation history windows
//!
//! User memory facts and preferences are stored in the database (PostgresClient).
//! This module provides utilities for working with that stored memory.

use ares_types::types::{AgentContext, MemoryFact, Message, Preference, UserMemory};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fmt;

/// Default number of recent messages to include in context.
pub const DEFAULT_HISTORY_WINDOW: usize = 10;

/// Maximum number of facts to include in a prompt to avoid token overflow.
pub const MAX_FACTS_IN_PROMPT: usize = 20;

/// Maximum number of preferences to include in a prompt.
pub const MAX_PREFERENCES_IN_PROMPT: usize = 10;

/// Formats user memory into a string suitable for inclusion in system prompts.
///
/// # Arguments
/// * `memory` - The user memory to format
///
/// # Returns
/// A formatted string containing preferences and facts, or an empty string if memory is empty.
///
/// # Example
/// ```ignore
/// let memory = UserMemory { user_id: "123".into(), preferences: vec![...], facts: vec![...] };
/// let context = format_memory_for_prompt(&memory);
/// // context: "User Preferences:\n- communication: concise\n\nKnown Facts:\n- work: engineer"
/// ```
pub fn format_memory_for_prompt(memory: &UserMemory) -> String {
    let mut parts = Vec::new();

    // Format preferences (limited to avoid token overflow)
    if !memory.preferences.is_empty() {
        let prefs: Vec<String> = memory
            .preferences
            .iter()
            .take(MAX_PREFERENCES_IN_PROMPT)
            .filter(|p| p.confidence >= 0.5) // Only include confident preferences
            .map(|p| format!("- {}/{}: {}", p.category, p.key, p.value))
            .collect();

        if !prefs.is_empty() {
            parts.push(format!("User Preferences:\n{}", prefs.join("\n")));
        }
    }

    // Format facts (limited and filtered by confidence)
    if !memory.facts.is_empty() {
        let facts: Vec<String> = memory
            .facts
            .iter()
            .take(MAX_FACTS_IN_PROMPT)
            .filter(|f| f.confidence >= 0.5) // Only include confident facts
            .map(|f| format!("- {}/{}: {}", f.category, f.fact_key, f.fact_value))
            .collect();

        if !facts.is_empty() {
            parts.push(format!("Known Facts about User:\n{}", facts.join("\n")));
        }
    }

    parts.join("\n\n")
}

/// Formats user preferences into a compact string for prompt inclusion.
///
/// This is a lighter-weight alternative to `format_memory_for_prompt` when
/// only preferences are needed (e.g., for routing decisions).
pub fn format_preferences_compact(preferences: &[Preference]) -> String {
    preferences
        .iter()
        .filter(|p| p.confidence >= 0.5)
        .take(MAX_PREFERENCES_IN_PROMPT)
        .map(|p| format!("{}: {}", p.key, p.value))
        .collect::<Vec<_>>()
        .join(", ")
}

/// Truncates conversation history to a window of recent messages.
///
/// # Arguments
/// * `history` - Full conversation history
/// * `window_size` - Maximum number of messages to keep
///
/// # Returns
/// A new vector containing only the most recent messages.
pub fn truncate_history(history: &[Message], window_size: usize) -> Vec<Message> {
    if history.len() <= window_size {
        history.to_vec()
    } else {
        history[history.len() - window_size..].to_vec()
    }
}

/// Estimates token count for a message.
///
/// Uses an improved heuristic combining word count and character count:
/// - ~1.3 tokens per word for typical English text
/// - ~4 characters per token as a fallback floor
///
/// This provides a safer (higher) estimate for billing purposes.
/// Actual token counts vary by tokenizer (GPT-3/4, Claude, etc.).
pub fn estimate_tokens(text: &str) -> usize {
    let words = text.split_whitespace().count();
    let chars = text.len();
    // Heuristic: ~1.3 tokens per word for English, with floor from char count
    let word_estimate = (words as f64 * 1.3) as usize;
    let char_estimate = chars.div_ceil(4);
    // Use the higher estimate for safety (billing should overcount not undercount)
    word_estimate.max(char_estimate).max(1)
}

/// Truncates history to fit within a token budget.
///
/// Removes oldest messages until the total estimated tokens is under the budget.
///
/// # Arguments
/// * `history` - Full conversation history
/// * `token_budget` - Maximum tokens to allow
///
/// # Returns
/// A truncated history that fits within the token budget.
pub fn truncate_history_to_tokens(history: &[Message], token_budget: usize) -> Vec<Message> {
    let mut result: Vec<Message> = Vec::new();
    let mut total_tokens = 0;

    // Work backwards from most recent messages
    for msg in history.iter().rev() {
        let msg_tokens = estimate_tokens(&msg.content);
        if total_tokens + msg_tokens > token_budget {
            break;
        }
        result.push(msg.clone());
        total_tokens += msg_tokens;
    }

    // Reverse to restore chronological order
    result.reverse();
    result
}

/// Builds an agent context from components.
///
/// This is a convenience function for constructing AgentContext with
/// appropriate defaults and optional memory/history truncation.
///
/// # Arguments
/// * `user_id` - User identifier
/// * `session_id` - Session/conversation identifier
/// * `history` - Full conversation history (will be truncated)
/// * `memory` - Optional user memory
/// * `history_window` - Maximum messages to include (defaults to DEFAULT_HISTORY_WINDOW)
pub fn build_context(
    user_id: String,
    session_id: String,
    history: Vec<Message>,
    memory: Option<UserMemory>,
    history_window: Option<usize>,
) -> AgentContext {
    let window = history_window.unwrap_or(DEFAULT_HISTORY_WINDOW);
    let truncated_history = truncate_history(&history, window);

    AgentContext {
        user_id,
        session_id,
        conversation_history: truncated_history,
        user_memory: memory,
    }
}

/// Filters memory facts by category.
///
/// Useful for retrieving only relevant facts for specific agent types.
pub fn filter_facts_by_category(facts: &[MemoryFact], category: &str) -> Vec<MemoryFact> {
    facts
        .iter()
        .filter(|f| f.category == category)
        .cloned()
        .collect()
}

/// Filters preferences by category.
pub fn filter_preferences_by_category(
    preferences: &[Preference],
    category: &str,
) -> Vec<Preference> {
    preferences
        .iter()
        .filter(|p| p.category == category)
        .cloned()
        .collect()
}


// =============================================================================
// In-memory session store (R43)
// =============================================================================

/// Configuration for the in-memory session store.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct MemoryConfig {
    /// Maximum number of sessions retained across all tenants.
    pub max_sessions: usize,
    /// Session time-to-live in seconds (`0` disables TTL expiry).
    pub session_ttl_secs: u64,
}

impl Default for MemoryConfig {
    fn default() -> Self {
        Self {
            max_sessions: 128,
            session_ttl_secs: 3600,
        }
    }
}

/// A tenant-scoped conversation session stored in memory.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct MemorySession {
    pub tenant_id: String,
    pub session_id: String,
    pub user_id: String,
    pub payload: serde_json::Value,
    pub created_at: i64,
    pub updated_at: i64,
}

/// Errors from session store operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MemoryError {
    NotFound {
        tenant_id: String,
        session_id: String,
    },
    CapacityExceeded {
        max_sessions: usize,
    },
    InvalidTenant {
        expected: String,
        actual: String,
    },
}

impl fmt::Display for MemoryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            MemoryError::NotFound {
                tenant_id,
                session_id,
            } => write!(
                f,
                "session '{session_id}' not found for tenant '{tenant_id}'"
            ),
            MemoryError::CapacityExceeded { max_sessions } => {
                write!(f, "session store capacity exceeded (max {max_sessions})")
            }
            MemoryError::InvalidTenant { expected, actual } => write!(
                f,
                "invalid tenant: expected '{expected}', got '{actual}'"
            ),
        }
    }
}

impl std::error::Error for MemoryError {}

/// Composite map key for tenant-scoped sessions.
pub fn session_key(tenant_id: &str, session_id: &str) -> String {
    format!("{tenant_id}\x1f{session_id}")
}

/// Returns true when `now - created_at > ttl_secs` (TTL disabled when `ttl_secs == 0`).
pub fn ttl_expired(created_at: i64, now: i64, ttl_secs: u64) -> bool {
    ttl_secs > 0 && now.saturating_sub(created_at) > ttl_secs as i64
}

/// Promote `key` to most-recently-used (end of `order`); returns whether it was present.
pub fn lru_get(order: &mut Vec<String>, key: &str) -> bool {
    if let Some(pos) = order.iter().position(|k| k == key) {
        let entry = order.remove(pos);
        order.push(entry);
        true
    } else {
        false
    }
}

/// Insert or promote `key` to most-recently-used.
pub fn lru_put(order: &mut Vec<String>, key: String) {
    if let Some(pos) = order.iter().position(|k| k == &key) {
        order.remove(pos);
    }
    order.push(key);
}

/// Evict least-recently-used entries until `order.len() <= max_sessions`.
pub fn lru_evict(
    order: &mut Vec<String>,
    sessions: &mut HashMap<String, MemorySession>,
    max_sessions: usize,
) {
    while order.len() > max_sessions {
        if let Some(victim) = order.first().cloned() {
            order.remove(0);
            sessions.remove(&victim);
        } else {
            break;
        }
    }
}

/// Remove expired sessions; returns the number removed.
pub fn cleanup_expired(
    sessions: &mut HashMap<String, MemorySession>,
    order: &mut Vec<String>,
    now: i64,
    ttl_secs: u64,
) -> usize {
    if ttl_secs == 0 {
        return 0;
    }
    let expired: Vec<String> = sessions
        .iter()
        .filter(|(_, s)| ttl_expired(s.created_at, now, ttl_secs))
        .map(|(k, _)| k.clone())
        .collect();
    for key in &expired {
        sessions.remove(key);
        if let Some(pos) = order.iter().position(|k| k == key) {
            order.remove(pos);
        }
    }
    expired.len()
}

/// In-memory LRU session store with per-tenant isolation and TTL.
#[derive(Debug, Clone)]
pub struct MemoryStore {
    config: MemoryConfig,
    sessions: HashMap<String, MemorySession>,
    lru_order: Vec<String>,
}

impl MemoryStore {
    pub fn new(config: MemoryConfig) -> Self {
        Self {
            config,
            sessions: HashMap::new(),
            lru_order: Vec::new(),
        }
    }

    pub fn with_defaults() -> Self {
        Self::new(MemoryConfig::default())
    }

    pub fn config(&self) -> &MemoryConfig {
        &self.config
    }

    pub fn len(&self) -> usize {
        self.sessions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.sessions.is_empty()
    }

    pub fn upsert(&mut self, session: MemorySession, now: i64) -> Result<(), MemoryError> {
        let key = session_key(&session.tenant_id, &session.session_id);
        lru_put(&mut self.lru_order, key.clone());
        self.sessions.insert(key, session);
        lru_evict(
            &mut self.lru_order,
            &mut self.sessions,
            self.config.max_sessions,
        );
        if self.sessions.len() > self.config.max_sessions {
            return Err(MemoryError::CapacityExceeded {
                max_sessions: self.config.max_sessions,
            });
        }
        let _ = now;
        Ok(())
    }

    pub fn put_for_tenant(
        &mut self,
        tenant_id: &str,
        session: MemorySession,
        now: i64,
    ) -> Result<(), MemoryError> {
        if session.tenant_id != tenant_id {
            return Err(MemoryError::InvalidTenant {
                expected: tenant_id.to_string(),
                actual: session.tenant_id.clone(),
            });
        }
        self.upsert(session, now)
    }

    pub fn get(
        &mut self,
        tenant_id: &str,
        session_id: &str,
        now: i64,
    ) -> Result<MemorySession, MemoryError> {
        let key = session_key(tenant_id, session_id);
        let Some(session) = self.sessions.get(&key).cloned() else {
            return Err(MemoryError::NotFound {
                tenant_id: tenant_id.to_string(),
                session_id: session_id.to_string(),
            });
        };
        if ttl_expired(session.created_at, now, self.config.session_ttl_secs) {
            self.remove(tenant_id, session_id);
            return Err(MemoryError::NotFound {
                tenant_id: tenant_id.to_string(),
                session_id: session_id.to_string(),
            });
        }
        lru_get(&mut self.lru_order, &key);
        Ok(session)
    }

    pub fn remove(&mut self, tenant_id: &str, session_id: &str) -> bool {
        let key = session_key(tenant_id, session_id);
        if self.sessions.remove(&key).is_some() {
            if let Some(pos) = self.lru_order.iter().position(|k| k == &key) {
                self.lru_order.remove(pos);
            }
            true
        } else {
            false
        }
    }

    pub fn cleanup(&mut self, now: i64) -> usize {
        cleanup_expired(
            &mut self.sessions,
            &mut self.lru_order,
            now,
            self.config.session_ttl_secs,
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ares_types::types::MessageRole;
    use chrono::Utc;
    use std::collections::HashMap;

    #[test]
    fn test_format_memory_for_prompt_empty() {
        let memory = UserMemory {
            user_id: "test".to_string(),
            preferences: vec![],
            facts: vec![],
        };
        assert_eq!(format_memory_for_prompt(&memory), "");
    }

    #[test]
    fn test_format_memory_for_prompt_with_preferences() {
        let memory = UserMemory {
            user_id: "test".to_string(),
            preferences: vec![Preference {
                category: "communication".to_string(),
                key: "style".to_string(),
                value: "concise".to_string(),
                confidence: 0.9,
            }],
            facts: vec![],
        };
        let result = format_memory_for_prompt(&memory);
        assert!(result.contains("User Preferences:"));
        assert!(result.contains("communication/style: concise"));
    }

    #[test]
    fn test_format_memory_filters_low_confidence() {
        let memory = UserMemory {
            user_id: "test".to_string(),
            preferences: vec![
                Preference {
                    category: "test".to_string(),
                    key: "high".to_string(),
                    value: "yes".to_string(),
                    confidence: 0.8,
                },
                Preference {
                    category: "test".to_string(),
                    key: "low".to_string(),
                    value: "no".to_string(),
                    confidence: 0.3, // Below threshold
                },
            ],
            facts: vec![],
        };
        let result = format_memory_for_prompt(&memory);
        assert!(result.contains("high"));
        assert!(!result.contains("low"));
    }

    #[test]
    fn test_truncate_history() {
        let history: Vec<Message> = (0..10)
            .map(|i| Message {
                role: MessageRole::User,
                content: format!("Message {}", i),
                timestamp: Utc::now(),
                parts: vec![],
            })
            .collect();

        let truncated = truncate_history(&history, 3);
        assert_eq!(truncated.len(), 3);
        assert!(truncated[0].content.contains("7"));
        assert!(truncated[2].content.contains("9"));
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 1); // floors at 1 for billing safety
        assert_eq!(estimate_tokens("test"), 1);
        assert_eq!(estimate_tokens("this is a longer test string"), 7);
    }

    #[test]
    fn test_format_preferences_compact() {
        let prefs = vec![
            Preference {
                category: "output".to_string(),
                key: "format".to_string(),
                value: "markdown".to_string(),
                confidence: 0.9,
            },
            Preference {
                category: "output".to_string(),
                key: "length".to_string(),
                value: "brief".to_string(),
                confidence: 0.8,
            },
        ];
        let result = format_preferences_compact(&prefs);
        assert_eq!(result, "format: markdown, length: brief");
    }

    #[test]
    fn test_build_context() {
        let history: Vec<Message> = (0..20)
            .map(|i| Message {
                role: MessageRole::User,
                content: format!("Message {}", i),
                timestamp: Utc::now(),
                parts: vec![],
            })
            .collect();

        let context = build_context(
            "user1".to_string(),
            "session1".to_string(),
            history,
            None,
            Some(5),
        );

        assert_eq!(context.user_id, "user1");
        assert_eq!(context.session_id, "session1");
        assert_eq!(context.conversation_history.len(), 5);
        assert!(context.user_memory.is_none());
    }

    #[test]
    fn test_filter_facts_by_category() {
        let facts = vec![
            MemoryFact {
                id: "1".to_string(),
                user_id: "test".to_string(),
                category: "work".to_string(),
                fact_key: "role".to_string(),
                fact_value: "engineer".to_string(),
                confidence: 0.9,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
            MemoryFact {
                id: "2".to_string(),
                user_id: "test".to_string(),
                category: "personal".to_string(),
                fact_key: "hobby".to_string(),
                fact_value: "reading".to_string(),
                confidence: 0.8,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            },
        ];

        let work_facts = filter_facts_by_category(&facts, "work");
        assert_eq!(work_facts.len(), 1);
        assert_eq!(work_facts[0].fact_key, "role");
    }

    #[test]
    fn test_format_memory_for_prompt_with_facts() {
        let memory = UserMemory {
            user_id: "test".to_string(),
            preferences: vec![],
            facts: vec![MemoryFact {
                id: "f1".to_string(),
                user_id: "test".to_string(),
                category: "work".to_string(),
                fact_key: "role".to_string(),
                fact_value: "engineer".to_string(),
                confidence: 0.9,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }],
        };
        let result = format_memory_for_prompt(&memory);
        assert!(result.contains("Known Facts about User:"));
        assert!(result.contains("work/role: engineer"));
    }

    #[test]
    fn test_truncate_history_noop_when_within_window() {
        let history: Vec<Message> = vec![Message {
            role: MessageRole::User,
            content: "only one".to_string(),
            timestamp: Utc::now(),
            parts: vec![],
        }];
        let truncated = truncate_history(&history, 5);
        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0].content, "only one");
    }

    #[test]
    fn test_truncate_history_to_tokens() {
        let history: Vec<Message> = (0..5)
            .map(|i| Message {
                role: MessageRole::User,
                content: format!("word {}", i),
                timestamp: Utc::now(),
                parts: vec![],
            })
            .collect();

        let truncated = truncate_history_to_tokens(&history, 3);
        assert!(!truncated.is_empty());
        assert!(truncated.len() < history.len());
        assert_eq!(truncated.last().unwrap().content, "word 4");
    }

    #[test]
    fn test_filter_preferences_by_category() {
        let prefs = vec![
            Preference {
                category: "output".to_string(),
                key: "format".to_string(),
                value: "markdown".to_string(),
                confidence: 0.9,
            },
            Preference {
                category: "language".to_string(),
                key: "locale".to_string(),
                value: "en".to_string(),
                confidence: 0.9,
            },
        ];

        let output = filter_preferences_by_category(&prefs, "output");
        assert_eq!(output.len(), 1);
        assert_eq!(output[0].key, "format");
    }

    #[test]
    fn test_user_memory_serde_roundtrip() {
        let memory = UserMemory {
            user_id: "user-42".to_string(),
            preferences: vec![Preference {
                category: "output".to_string(),
                key: "format".to_string(),
                value: "markdown".to_string(),
                confidence: 0.75,
            }],
            facts: vec![MemoryFact {
                id: "fact-1".to_string(),
                user_id: "user-42".to_string(),
                category: "work".to_string(),
                fact_key: "team".to_string(),
                fact_value: "platform".to_string(),
                confidence: 0.95,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            }],
        };

        let json = serde_json::to_string(&memory).expect("serialize UserMemory");
        let parsed: UserMemory = serde_json::from_str(&json).expect("deserialize UserMemory");
        assert_eq!(parsed.user_id, "user-42");
        assert_eq!(parsed.preferences.len(), 1);
        assert_eq!(parsed.facts[0].fact_key, "team");
    }

    #[test]
    fn test_message_serde_roundtrip() {
        let msg = Message {
            role: MessageRole::Assistant,
            content: "hello".to_string(),
            timestamp: Utc::now(),
            parts: vec![],
        };
        let json = serde_json::to_string(&msg).expect("serialize Message");
        let parsed: Message = serde_json::from_str(&json).expect("deserialize Message");
        assert_eq!(parsed.content, "hello");
        assert!(matches!(parsed.role, MessageRole::Assistant));
    }

    #[test]
    fn test_format_memory_respects_max_limits() {
        let prefs: Vec<Preference> = (0..MAX_PREFERENCES_IN_PROMPT + 5)
            .map(|i| Preference {
                category: "output".to_string(),
                key: format!("key-{i}"),
                value: "v".to_string(),
                confidence: 0.9,
            })
            .collect();
        let facts: Vec<MemoryFact> = (0..MAX_FACTS_IN_PROMPT + 5)
            .map(|i| MemoryFact {
                id: format!("id-{i}"),
                user_id: "u".to_string(),
                category: "work".to_string(),
                fact_key: format!("fact-{i}"),
                fact_value: "v".to_string(),
                confidence: 0.9,
                created_at: Utc::now(),
                updated_at: Utc::now(),
            })
            .collect();

        let memory = UserMemory {
            user_id: "u".to_string(),
            preferences: prefs,
            facts,
        };
        let formatted = format_memory_for_prompt(&memory);
        for i in MAX_PREFERENCES_IN_PROMPT..MAX_PREFERENCES_IN_PROMPT + 5 {
            assert!(
                !formatted.contains(&format!("key-{i}")),
                "preference beyond cap should be omitted"
            );
        }
        for i in MAX_FACTS_IN_PROMPT..MAX_FACTS_IN_PROMPT + 5 {
            assert!(
                !formatted.contains(&format!("fact-{i}")),
                "fact beyond cap should be omitted"
            );
        }
        assert!(formatted.contains("key-0"));
        assert!(formatted.contains("fact-0"));
    }

    #[test]
    fn test_format_memory_filters_low_confidence_facts() {
        let memory = UserMemory {
            user_id: "test".to_string(),
            preferences: vec![],
            facts: vec![
                MemoryFact {
                    id: "1".to_string(),
                    user_id: "test".to_string(),
                    category: "work".to_string(),
                    fact_key: "kept".to_string(),
                    fact_value: "yes".to_string(),
                    confidence: 0.9,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
                MemoryFact {
                    id: "2".to_string(),
                    user_id: "test".to_string(),
                    category: "work".to_string(),
                    fact_key: "dropped".to_string(),
                    fact_value: "no".to_string(),
                    confidence: 0.2,
                    created_at: Utc::now(),
                    updated_at: Utc::now(),
                },
            ],
        };
        let result = format_memory_for_prompt(&memory);
        assert!(result.contains("kept"));
        assert!(!result.contains("dropped"));
    }

    #[test]
    fn test_truncate_history_zero_window_returns_empty() {
        let history: Vec<Message> = (0..3)
            .map(|i| Message {
                role: MessageRole::User,
                content: format!("msg {i}"),
                timestamp: Utc::now(),
                parts: vec![],
            })
            .collect();
        assert!(truncate_history(&history, 0).is_empty());
    }

    #[test]
    fn test_truncate_history_to_tokens_zero_budget_returns_empty() {
        let history: Vec<Message> = vec![Message {
            role: MessageRole::User,
            content: "non-empty".to_string(),
            timestamp: Utc::now(),
            parts: vec![],
        }];
        assert!(truncate_history_to_tokens(&history, 0).is_empty());
    }

    #[test]
    fn test_build_context_uses_default_history_window() {
        let history: Vec<Message> = (0..DEFAULT_HISTORY_WINDOW + 5)
            .map(|i| Message {
                role: MessageRole::User,
                content: format!("Message {}", i),
                timestamp: Utc::now(),
                parts: vec![],
            })
            .collect();

        let context = build_context(
            "user".to_string(),
            "session".to_string(),
            history,
            None,
            None,
        );
        assert_eq!(context.conversation_history.len(), DEFAULT_HISTORY_WINDOW);
    }

    #[test]
    fn test_format_preferences_compact_filters_low_confidence() {
        let prefs = vec![
            Preference {
                category: "output".to_string(),
                key: "keep".to_string(),
                value: "yes".to_string(),
                confidence: 0.9,
            },
            Preference {
                category: "output".to_string(),
                key: "drop".to_string(),
                value: "no".to_string(),
                confidence: 0.1,
            },
        ];
        let result = format_preferences_compact(&prefs);
        assert!(result.contains("keep"));
        assert!(!result.contains("drop"));
    }

    #[test]
    fn test_filter_facts_by_category_no_matches() {
        let facts = vec![MemoryFact {
            id: "1".to_string(),
            user_id: "u".to_string(),
            category: "work".to_string(),
            fact_key: "role".to_string(),
            fact_value: "engineer".to_string(),
            confidence: 0.9,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }];
        assert!(filter_facts_by_category(&facts, "missing").is_empty());
    }

    #[test]
    fn test_truncate_history_to_tokens_keeps_recent_within_budget() {
        let history: Vec<Message> = vec![
            Message {
                role: MessageRole::User,
                content: "a".repeat(40),
                timestamp: Utc::now(),
                parts: vec![],
            },
            Message {
                role: MessageRole::User,
                content: "short".to_string(),
                timestamp: Utc::now(),
                parts: vec![],
            },
        ];
        let truncated = truncate_history_to_tokens(&history, 5);
        assert_eq!(truncated.len(), 1);
        assert_eq!(truncated[0].content, "short");
    }
    // =====================================================================
    // In-memory session store (R43)
    // =====================================================================

    fn sample_session(tenant: &str, session: &str, created_at: i64) -> MemorySession {
        MemorySession {
            tenant_id: tenant.into(),
            session_id: session.into(),
            user_id: "user-1".into(),
            payload: serde_json::json!({"turn": 1}),
            created_at,
            updated_at: created_at,
        }
    }

    #[test]
    fn memory_config_serde_roundtrip() {
        let cfg = MemoryConfig {
            max_sessions: 4,
            session_ttl_secs: 120,
        };
        let json = serde_json::to_string(&cfg).expect("serialize");
        let back: MemoryConfig = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, cfg);
    }

    #[test]
    fn memory_session_serde_roundtrip() {
        let session = sample_session("tenant-a", "sess-1", 100);
        let json = serde_json::to_string(&session).expect("serialize");
        let back: MemorySession = serde_json::from_str(&json).expect("deserialize");
        assert_eq!(back, session);
    }

    #[test]
    fn memory_config_default_values() {
        let cfg = MemoryConfig::default();
        assert_eq!(cfg.max_sessions, 128);
        assert_eq!(cfg.session_ttl_secs, 3600);
    }

    #[test]
    fn session_key_includes_tenant_and_session() {
        let key = session_key("tenant-a", "sess-1");
        assert!(key.contains("tenant-a"));
        assert!(key.contains("sess-1"));
        assert_ne!(session_key("tenant-a", "sess-1"), session_key("tenant-b", "sess-1"));
    }

    #[test]
    fn ttl_expired_false_when_within_ttl() {
        assert!(!ttl_expired(100, 150, 60));
    }

    #[test]
    fn ttl_expired_true_when_past_ttl() {
        assert!(ttl_expired(100, 200, 60));
    }

    #[test]
    fn ttl_expired_disabled_when_ttl_zero() {
        assert!(!ttl_expired(0, 1_000_000, 0));
    }

    #[test]
    fn ttl_expired_false_at_exact_boundary() {
        assert!(!ttl_expired(100, 160, 60));
    }

    #[test]
    fn lru_put_appends_new_key() {
        let mut order = Vec::new();
        lru_put(&mut order, "a".into());
        lru_put(&mut order, "b".into());
        assert_eq!(order, vec!["a", "b"]);
    }

    #[test]
    fn lru_put_promotes_existing_key() {
        let mut order = vec!["a".into(), "b".into(), "c".into()];
        lru_put(&mut order, "a".into());
        assert_eq!(order, vec!["b", "c", "a"]);
    }

    #[test]
    fn lru_get_promotes_key_to_end() {
        let mut order = vec!["a".into(), "b".into(), "c".into()];
        assert!(lru_get(&mut order, "a"));
        assert_eq!(order, vec!["b", "c", "a"]);
    }

    #[test]
    fn lru_get_missing_returns_false() {
        let mut order = vec!["a".into()];
        assert!(!lru_get(&mut order, "missing"));
    }

    #[test]
    fn lru_evict_drops_least_recently_used() {
        let mut order = vec!["old".into(), "mid".into(), "new".into()];
        let mut sessions = HashMap::new();
        sessions.insert("old".into(), sample_session("t", "old", 1));
        sessions.insert("mid".into(), sample_session("t", "mid", 1));
        sessions.insert("new".into(), sample_session("t", "new", 1));
        lru_evict(&mut order, &mut sessions, 2);
        assert_eq!(order, vec!["mid", "new"]);
        assert!(!sessions.contains_key("old"));
    }

    #[test]
    fn lru_evict_noop_when_within_capacity() {
        let mut order = vec!["a".into()];
        let mut sessions = HashMap::new();
        sessions.insert("a".into(), sample_session("t", "a", 1));
        lru_evict(&mut order, &mut sessions, 2);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn cleanup_expired_removes_stale_sessions() {
        let mut sessions = HashMap::new();
        let mut order = Vec::new();
        let key = session_key("t", "stale");
        sessions.insert(key.clone(), sample_session("t", "stale", 0));
        order.push(key);
        assert_eq!(cleanup_expired(&mut sessions, &mut order, 500, 60), 1);
        assert!(sessions.is_empty());
    }

    #[test]
    fn cleanup_expired_keeps_fresh_sessions() {
        let mut sessions = HashMap::new();
        let mut order = Vec::new();
        let key = session_key("t", "fresh");
        sessions.insert(key.clone(), sample_session("t", "fresh", 400));
        order.push(key);
        assert_eq!(cleanup_expired(&mut sessions, &mut order, 450, 60), 0);
        assert_eq!(sessions.len(), 1);
    }

    #[test]
    fn memory_store_put_and_get_roundtrip() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 4,
            session_ttl_secs: 0,
        });
        let session = sample_session("tenant-a", "sess-1", 10);
        store.put_for_tenant("tenant-a", session.clone(), 10).expect("put");
        let got = store.get("tenant-a", "sess-1", 10).expect("get");
        assert_eq!(got.session_id, "sess-1");
    }

    #[test]
    fn memory_store_get_not_found() {
        let mut store = MemoryStore::with_defaults();
        let err = store.get("tenant-a", "missing", 0).unwrap_err();
        assert!(matches!(err, MemoryError::NotFound { .. }));
    }

    #[test]
    fn memory_store_tenant_isolation() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 4,
            session_ttl_secs: 0,
        });
        store
            .put_for_tenant("tenant-a", sample_session("tenant-a", "shared-id", 1), 1)
            .expect("put");
        assert!(store.get("tenant-b", "shared-id", 1).is_err());
    }

    #[test]
    fn memory_store_invalid_tenant_on_put() {
        let mut store = MemoryStore::with_defaults();
        let err = store
            .put_for_tenant("tenant-a", sample_session("tenant-b", "s", 1), 1)
            .unwrap_err();
        assert!(matches!(err, MemoryError::InvalidTenant { .. }));
    }

    #[test]
    fn memory_store_capacity_evicts_lru() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 2,
            session_ttl_secs: 0,
        });
        store
            .put_for_tenant("t", sample_session("t", "one", 1), 1)
            .expect("put one");
        store
            .put_for_tenant("t", sample_session("t", "two", 2), 2)
            .expect("put two");
        store
            .put_for_tenant("t", sample_session("t", "three", 3), 3)
            .expect("put three");
        assert_eq!(store.len(), 2);
        assert!(store.get("t", "one", 3).is_err());
        assert!(store.get("t", "three", 3).is_ok());
    }

    #[test]
    fn memory_store_get_promotes_lru_entry() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 2,
            session_ttl_secs: 0,
        });
        store
            .put_for_tenant("t", sample_session("t", "a", 1), 1)
            .expect("a");
        store
            .put_for_tenant("t", sample_session("t", "b", 2), 2)
            .expect("b");
        store.get("t", "a", 3).expect("touch a");
        store
            .put_for_tenant("t", sample_session("t", "c", 4), 4)
            .expect("c");
        assert!(store.get("t", "a", 4).is_ok());
        assert!(store.get("t", "b", 4).is_err());
    }

    #[test]
    fn memory_store_get_expired_session_returns_not_found() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 4,
            session_ttl_secs: 60,
        });
        store
            .put_for_tenant("t", sample_session("t", "s", 100), 100)
            .expect("put");
        let err = store.get("t", "s", 200).unwrap_err();
        assert!(matches!(err, MemoryError::NotFound { .. }));
    }

    #[test]
    fn memory_store_cleanup_removes_expired() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 4,
            session_ttl_secs: 30,
        });
        store
            .put_for_tenant("t", sample_session("t", "s", 10), 10)
            .expect("put");
        assert_eq!(store.cleanup(100), 1);
        assert!(store.is_empty());
    }

    #[test]
    fn memory_store_remove_deletes_session() {
        let mut store = MemoryStore::with_defaults();
        store
            .put_for_tenant("t", sample_session("t", "s", 1), 1)
            .expect("put");
        assert!(store.remove("t", "s"));
        assert!(store.get("t", "s", 1).is_err());
    }

    #[test]
    fn memory_store_remove_missing_is_false() {
        let mut store = MemoryStore::with_defaults();
        assert!(!store.remove("t", "missing"));
    }

    #[test]
    fn memory_store_clone_preserves_sessions() {
        let mut store = MemoryStore::with_defaults();
        store
            .put_for_tenant("t", sample_session("t", "s", 1), 1)
            .expect("put");
        let cloned = store.clone();
        assert_eq!(cloned.len(), 1);
    }

    #[test]
    fn memory_store_debug_contains_type_name() {
        let store = MemoryStore::with_defaults();
        assert!(format!("{store:?}").contains("MemoryStore"));
    }

    #[test]
    fn memory_error_display_not_found() {
        let msg = MemoryError::NotFound {
            tenant_id: "t".into(),
            session_id: "s".into(),
        }
        .to_string();
        assert!(msg.contains("t") && msg.contains("s"));
    }

    #[test]
    fn memory_error_display_capacity_exceeded() {
        let msg = MemoryError::CapacityExceeded { max_sessions: 2 }.to_string();
        assert!(msg.contains("2"));
    }

    #[test]
    fn memory_error_display_invalid_tenant() {
        let msg = MemoryError::InvalidTenant {
            expected: "a".into(),
            actual: "b".into(),
        }
        .to_string();
        assert!(msg.contains("a") && msg.contains("b"));
    }

    #[test]
    fn memory_session_clone_eq() {
        let a = sample_session("t", "s", 1);
        assert_eq!(a.clone(), a);
    }

    #[test]
    fn memory_config_clone_eq() {
        let a = MemoryConfig::default();
        assert_eq!(a.clone(), a);
    }

    #[test]
    fn memory_error_clone_eq() {
        let err = MemoryError::CapacityExceeded { max_sessions: 1 };
        assert_eq!(err.clone(), err);
    }

    #[test]
    fn memory_store_new_is_empty() {
        let store = MemoryStore::with_defaults();
        assert!(store.is_empty());
    }

    #[test]
    fn memory_store_upsert_updates_existing_payload() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 4,
            session_ttl_secs: 0,
        });
        store
            .put_for_tenant("t", sample_session("t", "s", 1), 1)
            .expect("first");
        let mut updated = sample_session("t", "s", 2);
        updated.payload = serde_json::json!({"turn": 2});
        store.put_for_tenant("t", updated, 2).expect("update");
        let got = store.get("t", "s", 2).expect("get");
        assert_eq!(got.payload["turn"], 2);
    }

    #[test]
    fn memory_store_multiple_tenants_do_not_collide() {
        let mut store = MemoryStore::new(MemoryConfig {
            max_sessions: 8,
            session_ttl_secs: 0,
        });
        store
            .put_for_tenant("tenant-a", sample_session("tenant-a", "s", 1), 1)
            .expect("a");
        store
            .put_for_tenant("tenant-b", sample_session("tenant-b", "s", 1), 1)
            .expect("b");
        assert_eq!(store.len(), 2);
    }

    #[test]
    fn lru_order_least_recently_used_evicted_first() {
        let mut order = vec!["first".into(), "second".into()];
        let mut sessions = HashMap::new();
        sessions.insert("first".into(), sample_session("t", "first", 1));
        sessions.insert("second".into(), sample_session("t", "second", 1));
        lru_evict(&mut order, &mut sessions, 1);
        assert!(!sessions.contains_key("first"));
        assert!(sessions.contains_key("second"));
    }

    #[test]
    fn memory_store_config_accessor_returns_config() {
        let cfg = MemoryConfig {
            max_sessions: 9,
            session_ttl_secs: 5,
        };
        let store = MemoryStore::new(cfg.clone());
        assert_eq!(store.config(), &cfg);
    }

    #[test]
    fn cleanup_expired_noop_when_ttl_disabled() {
        let mut sessions = HashMap::new();
        let mut order = Vec::new();
        let key = session_key("t", "s");
        sessions.insert(key.clone(), sample_session("t", "s", 0));
        order.push(key);
        assert_eq!(cleanup_expired(&mut sessions, &mut order, 999, 0), 0);
    }

    #[test]
    fn reexports_memory_constants() {
        assert_eq!(DEFAULT_HISTORY_WINDOW, 10);
        assert_eq!(MAX_FACTS_IN_PROMPT, 20);
        assert_eq!(MAX_PREFERENCES_IN_PROMPT, 10);
    }

}
