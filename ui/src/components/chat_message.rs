//! Chat message component

use crate::types::{ContentPart, Message, MessageRole, ToolCallInfo};
use leptos::prelude::*;
use pulldown_cmark::{html, Options, Parser};

/// Convert markdown to HTML using pulldown-cmark with enhanced options
fn markdown_to_html(markdown: &str) -> String {
    // Pre-process: Convert LaTeX-style math to readable format
    let processed = preprocess_math(markdown);

    // Enable additional markdown features
    let mut options = Options::empty();
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_SMART_PUNCTUATION);

    let parser = Parser::new_ext(&processed, options);
    let mut html_output = String::new();
    html::push_html(&mut html_output, parser);
    html_output
}

/// Pre-process math notation for better rendering
fn preprocess_math(text: &str) -> String {
    text
        // Convert \times to ×
        .replace("\\times", "×")
        // Convert \div to ÷
        .replace("\\div", "÷")
        // Convert [ and ] LaTeX delimiters to code blocks
        .replace("[ ", "")
        .replace(" ]", "")
        // Convert \frac{a}{b} to a/b (basic)
        .replace("\\frac", "")
        // Remove remaining backslashes before common math symbols
        .replace("\\", "")
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
    let content = message.content.clone();
    let parts = message.parts.clone();
    let show_text = !content.is_empty();

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
                    {parts_preview(&parts, &content)}
                    {if !show_text {
                        None::<leptos::prelude::AnyView>
                    } else if is_user {
                        Some(view! {
                            <div class="whitespace-pre-wrap break-words">
                                {content.clone()}
                            </div>
                        }.into_any())
                    } else {
                        Some(view! {
                            <div
                                class="markdown break-words"
                                inner_html=rendered_content
                            />
                        }.into_any())
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

fn parts_preview(parts: &[ContentPart], message_content: &str) -> impl IntoView {
    let views: Vec<_> = parts
        .iter()
        .filter_map(|part| match part {
            ContentPart::Text { text } if text == message_content => None,
            ContentPart::Text { text } => Some(
                view! {
                    <div class="whitespace-pre-wrap break-words text-sm">{text.clone()}</div>
                }
                .into_any(),
            ),
            ContentPart::ImageUrl { url } => Some(
                view! {
                    <img src=url.clone() alt="attachment" class="max-w-xs max-h-64 rounded-lg object-contain" />
                }
                .into_any(),
            ),
            ContentPart::ImageBase64 { mime, data } => {
                let src = format!("data:{mime};base64,{data}");
                Some(
                    view! {
                        <img src=src alt="attachment" class="max-w-xs max-h-64 rounded-lg object-contain" />
                    }
                    .into_any(),
                )
            }
            ContentPart::FileUrl { url, mime } => {
                let label = part.chip_label();
                Some(file_chip(label, mime.clone(), Some(url.clone())))
            }
            ContentPart::FileBase64 { name, mime, .. } => {
                let label = name.clone().unwrap_or_else(|| mime.clone());
                Some(file_chip(label, Some(mime.clone()), None))
            }
        })
        .collect();

    if views.is_empty() {
        None::<leptos::prelude::AnyView>
    } else {
        Some(
            view! {
                <div class="flex flex-col gap-2 mb-2">
                    {views}
                </div>
            }
            .into_any(),
        )
    }
}

fn file_chip(
    label: String,
    mime: Option<String>,
    href: Option<String>,
) -> leptos::prelude::AnyView {
    view! {
        <span class="inline-flex items-center gap-2 px-2 py-1 rounded-lg text-xs
                     border border-[var(--border-default)] bg-[var(--bg-secondary)]
                     text-[var(--text-secondary)]">
            <svg xmlns="http://www.w3.org/2000/svg" class="w-3.5 h-3.5 shrink-0" fill="none"
                 viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                <path stroke-linecap="round" stroke-linejoin="round"
                      d="M15.172 7l-6.586 6.586a2 2 0 102.828 2.828l6.414-6.586a4 4 0 00-5.656-5.656l-6.415 6.585a6 6 0 108.486 8.486L20.5 13" />
            </svg>
            {
                if let Some(href) = href {
                    view! {
                        <a href=href target="_blank" rel="noopener noreferrer" class="underline truncate max-w-[12rem]">{label}</a>
                    }
                    .into_any()
                } else {
                    view! { <span class="truncate max-w-[12rem]">{label}</span> }.into_any()
                }
            }
            {mime.map(|m| view! { <span class="text-[var(--text-muted)]">{m}</span> })}
        </span>
    }
    .into_any()
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
