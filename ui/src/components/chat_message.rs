//! Chat message component

use leptos::prelude::*;
use crate::types::{Message, MessageRole, ToolCallInfo};

/// Render a single chat message
#[component]
pub fn ChatMessage(message: Message) -> impl IntoView {
    let is_user = message.role == MessageRole::User;
    let has_tools = !message.tool_calls.is_empty();
    
    view! {
        <div class=format!(
            "flex items-start gap-3 message-appear {}",
            if is_user { "flex-row-reverse" } else { "" }
        )>
            // Avatar
            <div class=format!(
                "w-8 h-8 rounded-full flex items-center justify-center text-white text-sm font-medium shrink-0 {}",
                if is_user {
                    "bg-gradient-to-br from-blue-500 to-cyan-500"
                } else {
                    "bg-gradient-to-br from-violet-500 to-purple-600"
                }
            )>
                {if is_user { "👤" } else { "🤖" }}
            </div>
            
            // Message content
            <div class=format!(
                "flex flex-col gap-1 max-w-[80%] {}",
                if is_user { "items-end" } else { "items-start" }
            )>
                // Agent type badge
                {(!is_user && message.agent_type.is_some()).then(|| {
                    let agent = message.agent_type.clone().unwrap_or_default();
                    view! {
                        <span class="text-xs text-slate-500 font-medium capitalize">
                            {agent}
                        </span>
                    }
                })}
                
                // Message bubble
                <div class=format!(
                    "px-4 py-3 rounded-2xl {} {}",
                    if is_user {
                        "bg-blue-600 text-white rounded-tr-sm"
                    } else {
                        "bg-slate-800 text-slate-100 rounded-tl-sm"
                    },
                    if message.is_streaming { "min-w-[100px]" } else { "" }
                )>
                    // Message text with markdown-like formatting
                    <div class="whitespace-pre-wrap break-words">
                        <MessageContent content=message.content.clone() />
                    </div>
                    
                    // Streaming cursor
                    {message.is_streaming.then(|| view! {
                        <span class="typing-cursor text-blue-400 ml-0.5">"▋"</span>
                    })}
                </div>
                
                // Tool calls section
                {has_tools.then(|| view! {
                    <ToolCallsDisplay tool_calls=message.tool_calls.clone() />
                })}
                
                // Timestamp
                <span class="text-xs text-slate-600 mt-1">
                    {message.timestamp.format("%H:%M").to_string()}
                </span>
            </div>
        </div>
    }
}

/// Render message content with basic markdown support
#[component]
fn MessageContent(content: String) -> impl IntoView {
    // Simple code block detection
    let parts: Vec<_> = content.split("```").collect();
    
    if parts.len() > 1 {
        // Has code blocks
        view! {
            <div>
                {parts.iter().enumerate().map(|(i, part)| {
                    if i % 2 == 1 {
                        // Code block
                        let (lang, code) = part.split_once('\n').unwrap_or(("", part));
                        view! {
                            <div class="my-2">
                                {(!lang.is_empty()).then(|| view! {
                                    <div class="text-xs text-slate-500 bg-slate-900 px-3 py-1 rounded-t-lg font-mono">
                                        {lang.to_string()}
                                    </div>
                                })}
                                <pre class="bg-slate-900 p-3 rounded-lg overflow-x-auto font-mono text-sm">
                                    <code class="text-green-400">{code.to_string()}</code>
                                </pre>
                            </div>
                        }.into_any()
                    } else {
                        // Regular text
                        view! {
                            <span>{part.to_string()}</span>
                        }.into_any()
                    }
                }).collect::<Vec<_>>()}
            </div>
        }.into_any()
    } else {
        // No code blocks, render as-is with inline code support
        view! {
            <span>
                {content.split('`').enumerate().map(|(i, part)| {
                    if i % 2 == 1 {
                        view! {
                            <code class="bg-slate-700 px-1.5 py-0.5 rounded text-sm font-mono text-blue-300">
                                {part.to_string()}
                            </code>
                        }.into_any()
                    } else {
                        view! { <span>{part.to_string()}</span> }.into_any()
                    }
                }).collect::<Vec<_>>()}
            </span>
        }.into_any()
    }
}

/// Display tool calls
#[component]
fn ToolCallsDisplay(tool_calls: Vec<ToolCallInfo>) -> impl IntoView {
    view! {
        <div class="flex flex-col gap-2 mt-2 w-full">
            {tool_calls.into_iter().map(|tool| view! {
                <div class="bg-slate-800/50 border border-slate-700 rounded-lg p-3 text-sm">
                    <div class="flex items-center gap-2 mb-2">
                        <span class="text-amber-400">"🔧"</span>
                        <span class="font-medium text-slate-300">{tool.name.clone()}</span>
                    </div>
                    <div class="bg-slate-900 rounded p-2 font-mono text-xs text-slate-400 overflow-x-auto">
                        {serde_json::to_string_pretty(&tool.arguments).unwrap_or_default()}
                    </div>
                    {tool.result.map(|result| view! {
                        <div class="mt-2 text-xs text-slate-400">
                            <span class="text-green-400">"→ "</span>
                            {result}
                        </div>
                    })}
                </div>
            }).collect::<Vec<_>>()}
        </div>
    }
}
