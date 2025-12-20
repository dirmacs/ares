//! Chat page - main conversation interface

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use web_sys::{ScrollBehavior, ScrollIntoViewOptions};
use crate::api::{load_agents, load_workflows, send_chat};
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
    
    // Send message handler
    let state_for_send = state.clone();
    let send_message = {
        move || {
            let message_text = input.get().trim().to_string();
            if message_text.is_empty() || is_sending.get() {
                return;
            }
            
            let state = state_for_send.clone();
            // Add user message
            let user_msg = Message::user(&message_text);
            state.conversation.update(|c| {
                c.messages.push(user_msg);
            });
            
            // Clear input
            input.set(String::new());
            is_sending.set(true);
            
            // Scroll after adding user message
            scroll_to_bottom();
            
            // Send to API
            let state = state.clone();
            let agent = selected_agent.get();
            spawn_local(async move {
                let base_url = state.api_base.get_untracked();
                let token = state.token.get_untracked().unwrap_or_default();
                let context_id = state.conversation.get_untracked().id.clone();
                
                match send_chat(&base_url, &token, &message_text, context_id, agent.clone()).await {
                    Ok(response) => {
                        // Add assistant response
                        let mut assistant_msg = Message::assistant(&response.response, Some(response.agent_type));
                        assistant_msg.tool_calls = response.tool_calls;
                        
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
                
                is_sending.set(false);
                scroll_to_bottom();
            });
        }
    };
    
    // Toggle sidebar on mobile
    let toggle_sidebar = move |_| sidebar_open.update(|v| *v = !*v);

    view! {
        <div class="h-screen flex flex-col bg-slate-900">
            <Header />
            
            <div class="flex-1 flex overflow-hidden">
                // Sidebar
                <Sidebar is_open=sidebar_open selected_agent=selected_agent />
                
                // Main chat area
                <main class="flex-1 flex flex-col min-w-0">
                    // Chat header
                    <div class="h-14 px-4 flex items-center justify-between border-b border-slate-800 bg-slate-900/50 backdrop-blur-sm">
                        // Mobile menu button
                        <button
                            on:click=toggle_sidebar
                            class="lg:hidden p-2 text-slate-400 hover:text-white"
                        >
                            <svg xmlns="http://www.w3.org/2000/svg" class="w-6 h-6" fill="none" viewBox="0 0 24 24" stroke="currentColor">
                                <path stroke-linecap="round" stroke-linejoin="round" stroke-width="2" d="M4 6h16M4 12h16M4 18h16" />
                            </svg>
                        </button>
                        
                        // Current agent display
                        <div class="flex items-center gap-3">
                            <div class="w-8 h-8 rounded-full bg-gradient-to-br from-violet-500 to-purple-600 
                                        flex items-center justify-center text-sm">
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
                                <div class="font-medium text-sm">
                                    {move || selected_agent.get()
                                        .map(|a| a.replace('_', " "))
                                        .unwrap_or_else(|| "Auto Router".to_string())}
                                </div>
                                <div class="text-xs text-slate-500">
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
                                if is_sending.get() { "bg-amber-400 animate-pulse" } else { "bg-green-400" }
                            )></div>
                            <span class="text-xs text-slate-500">
                                {move || if is_sending.get() { "Processing..." } else { "Ready" }}
                            </span>
                        </div>
                    </div>
                    
                    // Messages area
                    <div class="flex-1 overflow-y-auto px-4 py-6 space-y-6 scrollbar-thin scrollbar-thumb-slate-700 scrollbar-track-slate-900">
                        // Empty state
                        {
                            let state = state.clone();
                            move || {
                                if state.conversation.get().messages.is_empty() {
                                    view! { <EmptyState selected_agent=selected_agent /> }.into_any()
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
fn EmptyState(selected_agent: RwSignal<Option<String>>) -> impl IntoView {
    let state = expect_context::<AppState>();
    
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
        <div class="h-full flex flex-col items-center justify-center text-center px-4">
            <div class="w-20 h-20 rounded-2xl bg-gradient-to-br from-blue-500 via-violet-500 to-purple-600 
                        flex items-center justify-center text-4xl mb-6 shadow-xl shadow-violet-500/20">
                "🤖"
            </div>
            
            <h2 class="text-2xl font-bold mb-2">"How can I help you today?"</h2>
            <p class="text-slate-400 mb-8 max-w-md">
                {move || if selected_agent.get().is_some() {
                    "You're chatting directly with a specialized agent."
                } else {
                    "I'll automatically route your question to the best agent."
                }}
            </p>
            
            // Quick prompts
            <div class="w-full max-w-2xl grid sm:grid-cols-2 gap-3">
                {prompts.iter().map(|(emoji, prompt)| {
                    let prompt = *prompt;
                    view! {
                        <button
                            on:click=move |_| {
                                state.conversation.update(|c| {
                                    c.messages.push(Message::user(prompt));
                                });
                            }
                            class="flex items-center gap-3 p-4 bg-slate-800/50 hover:bg-slate-800 
                                   border border-slate-700 hover:border-slate-600 rounded-xl 
                                   text-left transition-all group"
                        >
                            <span class="text-2xl group-hover:scale-110 transition-transform">{*emoji}</span>
                            <span class="text-sm text-slate-300">{prompt}</span>
                        </button>
                    }
                }).collect::<Vec<_>>()}
            </div>
            
            // Features hint
            <div class="mt-8 flex flex-wrap justify-center gap-4 text-xs text-slate-500">
                <span class="flex items-center gap-1">
                    <span>"🔧"</span> "Tool calling"
                </span>
                <span class="flex items-center gap-1">
                    <span>"💾"</span> "Memory"
                </span>
                <span class="flex items-center gap-1">
                    <span>"🔀"</span> "Smart routing"
                </span>
                <span class="flex items-center gap-1">
                    <span>"📚"</span> "RAG support"
                </span>
            </div>
        </div>
    }
}
