//! Chat message component

use leptos::prelude::*;
use pulldown_cmark::{Parser, html};
use crate::types::{Message, MessageRole, ToolCallInfo};

/// Convert markdown to HTML using pulldown-cmark
fn markdown_to_html(markdown: &str) -> String {
    let parser = Parser::new(markdown);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

/// Render a single chat message
#[component]
pub fn ChatMessage(message: Message) -> impl IntoView {
    let is_user = message.role == MessageRole::User;
    let has_tools = !message.tool_calls.is_empty();
    
    // Convert markdown content to HTML for assistant messages
    let rendered_content = if is_user {
        message.content.clone()
    } else {
        markdown_to_html(&message.content)
    };
    
    view! {
        <div class=format!(
            "flex items-start gap-4 message animate-fade-in-up {}",
            if is_user { "flex-row-reverse" } else { "" }
        )>
            // Avatar
            <div class=format!(
                "w-9 h-9 rounded-lg flex items-center justify-center text-white text-sm font-medium shrink-0 {}",
                if is_user {
                    "bg-gradient-to-br from-blue-500 to-blue-600 shadow-lg shadow-blue-500/20"
                } else {
                    "bg-gradient-to-br from-violet-500 to-purple-600 shadow-lg shadow-purple-500/20"
                }
            )>
                {if is_user { "U" } else { "A" }}
            </div>
            
            // Message content
            <div class=format!(
                "flex flex-col gap-1.5 max-w-[85%] {}",
                if is_user { "items-end" } else { "items-start" }
            )>
                // Agent type badge
                {(!is_user && message.agent_type.is_some()).then(|| {
                    let agent = message.agent_type.clone().unwrap_or_default();
                    let badge_class = match agent.as_str() {
                        "finance" => "agent-badge-finance",
                        "sales" => "agent-badge-sales",
                        "hr" => "agent-badge-hr",
                        _ => "agent-badge",
                    };
                    view! {
                        <span class=format!("agent-badge {}", badge_class)>
                            <span class="w-1.5 h-1.5 rounded-full bg-current"></span>
                            {agent}
                        </span>
                    }
                })}
                
                // Message bubble
                <div class=format!(
                    "px-4 py-3 {} {}",
                    if is_user {
                        "message-user"
                    } else {
                        "message-assistant"
                    },
                    if message.is_streaming { "min-w-[100px]" } else { "" }
                )>
                    // Message text with markdown rendering
                    {if is_user {
                        // User messages - plain text
                        view! {
                            <div class="whitespace-pre-wrap break-words">
                                {message.content.clone()}
                            </div>
                        }.into_any()
                    } else {
                        // Assistant messages - rendered markdown
                        view! {
                            <div 
                                class="markdown break-words"
                                inner_html=rendered_content
                            />
                        }.into_any()
                    }}
                    
                    // Streaming cursor
                    {message.is_streaming.then(|| view! {
                        <span class="inline-block w-2 h-5 bg-current animate-pulse ml-1">"▋"</span>
                    })}
                </div>
                
                // Tool calls section
                {has_tools.then(|| view! {
                    <ToolCallsDisplay tool_calls=message.tool_calls.clone() />
                })}
                
                // Timestamp
                <span class="text-xs text-[var(--text-muted)] mt-0.5">
                    {message.timestamp.format("%H:%M").to_string()}
                </span>
            </div>
        </div>
    }
}

/// Display tool calls
#[component]
fn ToolCallsDisplay(tool_calls: Vec<ToolCallInfo>) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-2 mt-2 w-full">
            {tool_calls.into_iter().map(|tool| view! {
                <div class="card p-3 text-sm animate-fade-in">
                    <div class="flex items-center gap-2 mb-2">
                        <span class="text-[var(--accent-warning)]">"🔧"</span>
                        <span class="font-medium text-[var(--text-primary)]">{tool.name.clone()}</span>
                    </div>
                    <div class="code-block">
                        <div class="code-block-content text-xs">
                            {serde_json::to_string_pretty(&tool.arguments).unwrap_or_default()}
                        </div>
                    </div>
                    {tool.result.map(|result| view! {
                        <div class="mt-2 text-xs text-[var(--text-secondary)]">
                            <span class="text-[var(--accent-success)]">"→ "</span>
                            {result}
                        </div>
                    })}
                </div>
            }).collect::<Vec<_>>()}
        </div>
    }
}
