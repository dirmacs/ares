//! Chat page - main conversation interface

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use web_sys::{ScrollBehavior, ScrollIntoViewOptions};
use crate::api::{load_agents, load_workflows, send_chat, stream_chat};
use crate::components::{ChatInput, ChatMessage, Header, Sidebar, TypingIndicator};
use crate::state::AppState;
use crate::types::{Message, MessageRole};

/// Main chat page
#[component]
pub fn ChatPage() -> impl IntoView {
    let state = expect_context::<AppState>();
    let navigate = use_navigate();
    
    // Local state
    let input = RwSignal::new(String::new());
    let is_sending = RwSignal::new(false);
    let sidebar_open = RwSignal::new(false);
    let selected_agent = RwSignal::new(Option::<String>::None);
    let messages_end_ref = NodeRef::<leptos::html::Div>::new();
    
    // Streaming state - these are used to track the in-progress streaming response
    let streaming_content = RwSignal::new(String::new());
    let streaming_agent = RwSignal::new(Option::<String>::None);
    let streaming_msg_id = RwSignal::new(Option::<String>::None);
    
    // Redirect if not authenticated
    let navigate_clone = navigate.clone();
    Effect::new(move |_| {
        if state.token.get().is_none() {
            navigate_clone("/login", Default::default());
        }
    });
    
    // Load agents and workflows on mount
    let state_for_load = state.clone();
    Effect::new(move |_| {
        load_agents(state_for_load.clone());
        load_workflows(state_for_load.clone());
    });
    
    // Auto-scroll to bottom when new messages arrive
    let scroll_to_bottom = move || {
        if let Some(el) = messages_end_ref.get() {
            let options = ScrollIntoViewOptions::new();
            options.set_behavior(ScrollBehavior::Smooth);
            el.scroll_into_view_with_scroll_into_view_options(&options);
        }
    };
    
    // Send message helper function with streaming support
    let state_for_send = state.clone();
    let do_send_message = move |message_text: String| {
        if message_text.is_empty() || is_sending.get() {
            return;
        }
        
        let state = state_for_send.clone();
        // Add user message
        let user_msg = Message::user(&message_text);
        state.conversation.update(|c| {
            c.messages.push(user_msg);
        });
        
        is_sending.set(true);
        streaming_content.set(String::new());
        streaming_agent.set(None);
        
        // Generate a message ID for the streaming response
        let msg_id = uuid::Uuid::new_v4().to_string();
        streaming_msg_id.set(Some(msg_id.clone()));
        
        // Add a placeholder streaming message
        state.conversation.update(|c| {
            c.messages.push(Message {
                id: msg_id.clone(),
                role: MessageRole::Assistant,
                content: String::new(),
                timestamp: chrono::Utc::now(),
                agent_type: None,
                tool_calls: vec![],
                is_streaming: true,
            });
        });
        
        // Scroll after adding messages
        scroll_to_bottom();
        
        // Start streaming
        let state = state.clone();
        let agent = selected_agent.get();
        
        spawn_local(async move {
            let base_url = state.api_base.get_untracked();
            let token = state.token.get_untracked().unwrap_or_default();
            let context_id = state.conversation.get_untracked().id.clone();
            let msg_id_clone = msg_id.clone();
            
            // Try streaming first
            let stream_result = stream_chat(
                &base_url,
                &token,
                &message_text,
                context_id.clone(),
                agent.clone(),
                move |event| {
                    match event.event.as_str() {
                        "start" => {
                            // Update the agent info
                            if let Some(agent) = event.agent {
                                streaming_agent.set(Some(agent));
                            }
                        }
                        "token" => {
                            // Append token to streaming content
                            if let Some(content) = event.content {
                                streaming_content.update(|c| c.push_str(&content));
                                
                                // Update the message in the conversation
                                let current_content = streaming_content.get_untracked();
                                let msg_id = msg_id_clone.clone();
                                state.conversation.update(|c| {
                                    if let Some(msg) = c.messages.iter_mut().find(|m| m.id == msg_id) {
                                        msg.content = current_content;
                                    }
                                });
                            }
                        }
                        "done" => {
                            // Finalize the message
                            if let Some(ctx_id) = event.context_id {
                                state.conversation.update(|c| {
                                    c.id = Some(ctx_id);
                                });
                            }
                            
                            let final_agent = streaming_agent.get_untracked();
                            let msg_id = msg_id_clone.clone();
                            state.conversation.update(|c| {
                                if let Some(msg) = c.messages.iter_mut().find(|m| m.id == msg_id) {
                                    msg.is_streaming = false;
                                    msg.agent_type = final_agent;
                                }
                            });
                        }
                        "error" => {
                            // Handle error
                            let error_msg = event.error.unwrap_or_else(|| "Unknown error".to_string());
                            let msg_id = msg_id_clone.clone();
                            state.conversation.update(|c| {
                                if let Some(msg) = c.messages.iter_mut().find(|m| m.id == msg_id) {
                                    msg.content = format!("Error: {}", error_msg);
                                    msg.is_streaming = false;
                                    msg.role = MessageRole::System;
                                }
                            });
                        }
                        _ => {}
                    }
                },
            ).await;
            
            // If streaming failed, fall back to non-streaming API
            if let Err(e) = stream_result {
                tracing::warn!("Streaming failed, falling back to regular API: {}", e);
                
                // Remove the placeholder streaming message
                state.conversation.update(|c| {
                    c.messages.retain(|m| m.id != msg_id);
                });
                
                // Use regular chat endpoint
                match send_chat(&base_url, &token, &message_text, context_id, agent.clone()).await {
                    Ok(response) => {
                        // Add assistant response
                        let assistant_msg = Message::assistant(&response.response, Some(response.agent));
                        
                        state.conversation.update(|c| {
                            c.id = Some(response.context_id);
                            c.messages.push(assistant_msg);
                        });
                    }
                    Err(e) => {
                        // Add error as system message
                        state.conversation.update(|c| {
                            c.messages.push(Message {
                                id: uuid::Uuid::new_v4().to_string(),
                                role: MessageRole::System,
                                content: format!("Error: {}", e),
                                timestamp: chrono::Utc::now(),
                                agent_type: None,
                                tool_calls: vec![],
                                is_streaming: false,
                            });
                        });
                        state.set_error(e);
                    }
                }
            }
            
            is_sending.set(false);
            streaming_msg_id.set(None);
            scroll_to_bottom();
        });
    };
    
    // Wrapper for input-based sending
    let do_send_for_input = do_send_message.clone();
    let send_message = move || {
        let message_text = input.get().trim().to_string();
        input.set(String::new());
        do_send_for_input(message_text);
    };
    
    // Toggle sidebar on mobile
    let toggle_sidebar = move |_| sidebar_open.update(|v| *v = !*v);

    view! {
        <div class="h-screen flex flex-col bg-[var(--bg-primary)]">
            <Header />
            
            <div class="flex-1 flex overflow-hidden">
                // Sidebar
                <Sidebar is_open=sidebar_open selected_agent=selected_agent />
                
                // Main chat area
                <main class="flex-1 flex flex-col min-w-0">
                    // Chat header
                    <div class="h-14 px-4 flex items-center justify-between border-b border-[var(--border-default)] glass">
                        // Mobile menu button
                        <button
                            on:click=toggle_sidebar
                            class="lg:hidden btn btn-ghost p-2"
                        >
                            <svg xmlns="http://www.w3.org/2000/svg" class="w-6 h-6" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 12h16M4 18h16" />
                            </svg>
                        </button>
                        
                        // Current agent display
                        <div class="flex items-center gap-3">
                            <div class="w-9 h-9 rounded-lg bg-gradient-to-br from-violet-500 to-purple-600 
                                        flex items-center justify-center text-sm shadow-lg shadow-purple-500/20">
                                {move || {
                                    match selected_agent.get().as_deref() {
                                        None => "🔀",
                                        Some("orchestrator") => "🎭",
                                        Some("product") => "📦",
                                        Some("sales") => "💰",
                                        Some("finance") => "📊",
                                        Some("hr") => "👥",
                                        Some("invoice") => "📄",
                                        _ => "🤖",
                                    }
                                }}
                            </div>
                            <div>
                                <div class="font-medium text-sm text-[var(--text-primary)]">
                                    {move || selected_agent.get()
                                        .map(|a| a.replace('_', " "))
                                        .unwrap_or_else(|| "Auto Router".to_string())}
                                </div>
                                <div class="text-xs text-[var(--text-muted)]">
                                    {move || if selected_agent.get().is_none() {
                                        "Automatically routes to best agent"
                                    } else {
                                        "Direct agent chat"
                                    }}
                                </div>
                            </div>
                        </div>
                        
                        // Status indicator
                        <div class="flex items-center gap-2">
                            <div class=move || format!(
                                "w-2 h-2 rounded-full {}",
                                if is_sending.get() { "bg-[var(--accent-warning)] animate-pulse" } else { "bg-[var(--accent-success)]" }
                            )></div>
                            <span class="text-xs text-[var(--text-muted)]">
                                {move || if is_sending.get() { "Processing..." } else { "Ready" }}
                            </span>
                        </div>
                    </div>
                    
                    // Messages area
                    <div class="flex-1 overflow-y-auto px-4 py-6 space-y-6">
                        // Empty state
                        {
                            let state = state.clone();
                            let do_send = do_send_message.clone();
                            move || {
                                if state.conversation.get().messages.is_empty() {
                                    let do_send = do_send.clone();
                                    view! { <EmptyState selected_agent=selected_agent on_prompt=do_send /> }.into_any()
                                } else {
                                    view! {}.into_any()
                                }
                            }
                        }
                        
                        // Messages
                        {
                            let state = state.clone();
                            move || {
                                let messages = state.conversation.get().messages;
                                messages.into_iter().map(|msg| view! {
                                    <ChatMessage message=msg />
                                }).collect::<Vec<_>>()
                            }
                        }
                        
                        // Typing indicator
                        <Show when=move || is_sending.get()>
                            <TypingIndicator agent_name=selected_agent.get().unwrap_or_else(|| "AI".to_string()) />
                        </Show>
                        
                        // Scroll anchor
                        <div node_ref=messages_end_ref></div>
                    </div>
                    
                    // Input area
                    <ChatInput
                        value=input
                        on_submit=send_message
                        disabled=is_sending.get()
                        placeholder="Type your message... (Shift+Enter for new line)"
                    />
                </main>
            </div>
        </div>
    }
}

/// Empty state when no messages
#[component]
fn EmptyState<F>(selected_agent: RwSignal<Option<String>>, on_prompt: F) -> impl IntoView 
where
    F: Fn(String) + Clone + 'static
{
    // Example prompts
    let prompts = [
        ("📊", "Analyze our Q4 sales performance"),
        ("📦", "What products are in the tech category?"),
        ("🧮", "Calculate 15% of $2,450"),
        ("🔍", "Search for latest AI news"),
        ("💼", "What are our HR policies on remote work?"),
        ("📄", "Show me pending invoices"),
    ];
    
    view! {
        <div class="empty-state h-full">
            <img 
                src="/assets/ares.png" 
                alt="ARES Logo"
                class="empty-state-icon"
            />
            
            <h2 class="empty-state-title text-gradient">"How can I help you today?"</h2>
            <p class="empty-state-description">
                {move || if selected_agent.get().is_some() {
                    "You're chatting directly with a specialized agent."
                } else {
                    "I'll automatically route your question to the best agent."
                }}
            </p>
            
            // Quick prompts
            <div class="quick-prompts w-full max-w-2xl grid sm:grid-cols-2 gap-3">
                {prompts.iter().enumerate().map(|(i, (emoji, prompt))| {
                    let prompt = *prompt;
                    let on_prompt = on_prompt.clone();
                    view! {
                        <button
                            on:click=move |_| {
                                on_prompt(prompt.to_string());
                            }
                            class=format!("quick-prompt text-left animate-fade-in-up stagger-{}", (i % 5) + 1)
                        >
                            <span class="text-2xl mr-3 transition-transform hover:scale-110">{*emoji}</span>
                            <span>{prompt}</span>
                        </button>
                    }
                }).collect::<Vec<_>>()}
            </div>
            
            // Features hint
            <div class="mt-8 flex flex-wrap justify-center gap-6 text-xs text-[var(--text-muted)]">
                <span class="flex items-center gap-2 animate-fade-in stagger-1">
                    <span class="w-1.5 h-1.5 rounded-full bg-[var(--accent-primary)]"></span>
                    "Tool calling"
                </span>
                <span class="flex items-center gap-2 animate-fade-in stagger-2">
                    <span class="w-1.5 h-1.5 rounded-full bg-[var(--accent-secondary)]"></span>
                    "Memory"
                </span>
                <span class="flex items-center gap-2 animate-fade-in stagger-3">
                    <span class="w-1.5 h-1.5 rounded-full bg-[var(--accent-success)]"></span>
                    "Smart routing"
                </span>
                <span class="flex items-center gap-2 animate-fade-in stagger-4">
                    <span class="w-1.5 h-1.5 rounded-full bg-[var(--accent-warning)]"></span>
                    "RAG support"
                </span>
            </div>
        </div>
    }
}
