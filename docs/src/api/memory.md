# Memory

ARES manages conversation memory and user context across agent interactions.

## Features

- Sliding window over the conversation history (`DEFAULT_HISTORY_WINDOW = 10`)
- History truncation within a token budget
- Formatting of user memory (facts and preferences) for system prompts
- Integration with Eruka for persistent context across sessions

## Core functions

### History functions

```rust
use ares_agent::memory::{truncate_history, truncate_history_to_tokens};

// Keep last N messages
let recent = truncate_history(&messages, 10);

// Keep messages within a token budget
let within_budget = truncate_history_to_tokens(&messages, 4096);
```

### Context building

```rust
use ares_agent::memory::{build_context, format_memory_for_prompt};

// Format user memory (facts + preferences) into a system prompt section
let memory_text = format_memory_for_prompt(&user_memory);

// Build full context with history window and memory injection
let context = build_context(user_id, session_id, history, Some(user_memory), None);
```

### Filtering

```rust
use ares_agent::memory::{filter_facts_by_category, filter_preferences_by_category};

// Filter facts by category (e.g., "health", "technical")
let health_facts = filter_facts_by_category(&facts, "health");

// Filter preferences similarly
let prefs = filter_preferences_by_category(&preferences, "communication");
```

## Constants

| Constant | Value | Purpose |
|----------|-------|---------|
| `DEFAULT_HISTORY_WINDOW` | 10 | Default number of messages to keep |
| `MAX_FACTS_IN_PROMPT` | 20 | Max facts injected into system prompt |
| `MAX_PREFERENCES_IN_PROMPT` | 10 | Max preferences injected |

## Token estimation

```rust
use ares_agent::memory::estimate_tokens;

let tokens = estimate_tokens("Hello, how are you?");
// Rough estimate: ~5 tokens (word count * 1.3)
```

## Eruka integration

When you pair ARES with Eruka through the `ContextProvider` trait, the memory flow has these steps:

1. On session start, `ContextProvider::get_context()` fetches user state from Eruka
2. Facts and preferences are formatted and injected into the agent system prompt
3. After exchanges, agent signals (emotional state, topics, preferences) are written back to Eruka
4. Next session starts with updated context, agents remember users across conversations
