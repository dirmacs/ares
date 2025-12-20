//! Sidebar component

use leptos::prelude::*;
use crate::state::AppState;

/// Sidebar with agent list and settings
#[component]
pub fn Sidebar(
    /// Whether sidebar is open (mobile)
    is_open: RwSignal<bool>,
    /// Selected agent name
    selected_agent: RwSignal<Option<String>>,
) -> impl IntoView {
    let state = expect_context::<AppState>();
    
    let new_chat = move |_| {
        state.conversation.update(|c| {
            c.id = None;
            c.messages.clear();
        });
    };

    view! {
        // Overlay for mobile
        <Show when=move || is_open.get()>
            <div
                class="fixed inset-0 bg-black/60 backdrop-blur-sm z-30 lg:hidden animate-fade-in"
                on:click=move |_| is_open.set(false)
            ></div>
        </Show>
        
        // Sidebar
        <aside class=move || format!(
            "sidebar fixed lg:relative inset-y-0 left-0 z-40 w-72 
             flex flex-col transform transition-transform duration-300 lg:translate-x-0 {}",
            if is_open.get() { "translate-x-0" } else { "-translate-x-full" }
        )>
            // Header
            <div class="p-4 border-b border-[var(--border-default)]">
                <button
                    on:click=new_chat
                    class="btn btn-primary w-full"
                >
                    <svg xmlns="http://www.w3.org/2000/svg" class="w-5 h-5" viewBox="0 0 20 20" fill="currentColor">
                        <path fill-rule="evenodd" d="M10 3a1 1 0 011 1v5h5a1 1 0 110 2h-5v5a1 1 0 11-2 0v-5H4a1 1 0 110-2h5V4a1 1 0 011-1z" clip-rule="evenodd" />
                    </svg>
                    "New Chat"
                </button>
            </div>
            
            // Agents section
            <div class="flex-1 overflow-y-auto p-4 space-y-6">
                <div>
                    <h3 class="text-xs font-semibold text-[var(--text-muted)] uppercase tracking-wider mb-3 px-2">
                        "Agents"
                    </h3>
                    <div class="space-y-1">
                        // Auto router
                        <AgentButton
                            name="Auto (Router)".to_string()
                            emoji="🔀".to_string()
                            description="Automatic routing".to_string()
                            is_selected=Signal::derive(move || selected_agent.get().is_none())
                            on_click=move |_| selected_agent.set(None)
                        />
                        
                        // Dynamic agents
                        {move || {
                            let agents = state.agents.get();
                            agents.into_iter().enumerate().map(|(i, agent)| {
                                let agent_type = agent.agent_type.clone();
                                let agent_type_clone = agent_type.clone();
                                let emoji = match agent_type.as_str() {
                                    "orchestrator" => "🎭",
                                    "product" => "📦",
                                    "sales" => "💰",
                                    "finance" => "📊",
                                    "hr" => "👥",
                                    "invoice" => "📄",
                                    _ => "🤖",
                                };
                                view! {
                                    <div class=format!("animate-fade-in-up stagger-{}", (i % 5) + 1)>
                                        <AgentButton
                                            name=agent.name.clone()
                                            emoji=emoji.to_string()
                                            description=agent.description.clone()
                                            is_selected=Signal::derive(move || selected_agent.get().as_deref() == Some(&agent_type))
                                            on_click=move |_| selected_agent.set(Some(agent_type_clone.clone()))
                                        />
                                    </div>
                                }
                            }).collect::<Vec<_>>()
                        }}
                    </div>
                </div>
                
                // Workflows section
                <div>
                    <h3 class="text-xs font-semibold text-[var(--text-muted)] uppercase tracking-wider mb-3 px-2">
                        "Workflows"
                    </h3>
                    <div class="space-y-1">
                        {move || {
                            let workflows = state.workflows.get();
                            if workflows.is_empty() {
                                view! {
                                    <p class="text-sm text-[var(--text-muted)] italic px-2">"No workflows available"</p>
                                }.into_any()
                            } else {
                                workflows.into_iter().map(|wf| {
                                    let description = format!("Entry: {} | Max depth: {}", wf.entry_agent, wf.max_depth);
                                    view! {
                                        <div class="sidebar-item relative">
                                            <div class="flex-1 min-w-0">
                                                <div class="font-medium text-[var(--text-primary)]">{wf.name.clone()}</div>
                                                <div class="text-xs text-[var(--text-muted)] truncate">{description}</div>
                                            </div>
                                        </div>
                                    }
                                }).collect::<Vec<_>>().into_any()
                            }
                        }}
                    </div>
                </div>
            </div>
            
            // Footer
            <div class="p-4 border-t border-[var(--border-default)]">
                <div class="text-xs text-[var(--text-muted)] text-center">
                    "A.R.E.S v0.2.1"
                </div>
            </div>
        </aside>
    }
}

/// Agent button in sidebar
#[component]
fn AgentButton(
    name: String,
    emoji: String,
    description: String,
    is_selected: Signal<bool>,
    on_click: impl Fn(web_sys::MouseEvent) + 'static,
) -> impl IntoView {
    view! {
        <button
            on:click=on_click
            class=move || format!(
                "sidebar-item relative w-full text-left transition-all duration-150 {}",
                if is_selected.get() {
                    "sidebar-item-active"
                } else {
                    ""
                }
            )
        >
            <span class="text-lg">{emoji}</span>
            <div class="flex-1 min-w-0">
                <div class="text-sm font-medium truncate">{name}</div>
                <div class="text-xs text-[var(--text-muted)] truncate">{description}</div>
            </div>
        </button>
    }
}
