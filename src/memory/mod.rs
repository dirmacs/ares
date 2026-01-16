//! Memory management module for conversation context and user memory.
//!
//! This module provides utilities for:
//! - Building agent context with memory
//! - Formatting memory for LLM prompts
//! - Managing conversation history windows
//!
//! User memory facts and preferences are stored in the database (TursoClient).
//! This module provides utilities for working with that stored memory.

use crate::types::{AgentContext, MemoryFact, Message, Preference, UserMemory};

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

/// Estimates token count for a message (rough approximation).
///
/// Uses a simple heuristic of ~4 characters per token for English text.
/// This is an approximation and may vary by tokenizer.
pub fn estimate_tokens(text: &str) -> usize {
    // Rough approximation: ~4 chars per token for English
    text.len().div_ceil(4)
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::MessageRole;
    use chrono::Utc;

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
            })
            .collect();

        let truncated = truncate_history(&history, 3);
        assert_eq!(truncated.len(), 3);
        assert!(truncated[0].content.contains("7"));
        assert!(truncated[2].content.contains("9"));
    }

    #[test]
    fn test_estimate_tokens() {
        assert_eq!(estimate_tokens(""), 0);
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
}
