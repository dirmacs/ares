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
                class="fixed inset-0 bg-black/50 z-30 lg:hidden"
                on:click=move |_| is_open.set(false)
            ></div>
        </Show>
        
        // Sidebar
        <aside class=move || format!(
            "fixed lg:relative inset-y-0 left-0 z-40 w-72 bg-slate-800 border-r border-slate-700 
             flex flex-col transform transition-transform duration-300 lg:translate-x-0 {}",
            if is_open.get() { "translate-x-0" } else { "-translate-x-full" }
        )>
            // Header
            <div class="p-4 border-b border-slate-700">
                <button
                    on:click=new_chat
                    class="w-full flex items-center justify-center gap-2 px-4 py-3 
                           bg-blue-600 hover:bg-blue-700 rounded-lg font-medium transition-colors"
                >
                    <svg xmlns="http://www.w3.org/2000/svg" class="w-5 h-5" viewBox="0 0 20 20" fill="currentColor">
                        <path fill-rule="evenodd" d="M10 3a1 1 0 011 1v5h5a1 1 0 110 2h-5v5a1 1 0 11-2 0v-5H4a1 1 0 110-2h5V4a1 1 0 011-1z" clip-rule="evenodd" />
                    </svg>
                    "New Chat"
                </button>
            </div>
            
            // Agents section
            <div class="flex-1 overflow-y-auto p-4 space-y-4">
                <div>
                    <h3 class="text-xs font-semibold text-slate-500 uppercase tracking-wider mb-3">
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
                            agents.into_iter().map(|agent| {
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
                                    <AgentButton
                                        name=agent.name.clone()
                                        emoji=emoji.to_string()
                                        description=agent.description.clone()
                                        is_selected=Signal::derive(move || selected_agent.get().as_deref() == Some(&agent_type))
                                        on_click=move |_| selected_agent.set(Some(agent_type_clone.clone()))
                                    />
                                }
                            }).collect::<Vec<_>>()
                        }}
                    </div>
                </div>
                
                // Workflows section
                <div>
                    <h3 class="text-xs font-semibold text-slate-500 uppercase tracking-wider mb-3">
                        "Workflows"
                    </h3>
                    <div class="space-y-1">
                        {move || {
                            let workflows = state.workflows.get();
                            if workflows.is_empty() {
                                view! {
                                    <p class="text-sm text-slate-500 italic">"No workflows available"</p>
                                }.into_any()
                            } else {
                                workflows.into_iter().map(|wf| {
                                    let description = format!("Entry: {} | Max depth: {}", wf.entry_agent, wf.max_depth);
                                    view! {
                                        <div class="px-3 py-2 rounded-lg text-sm text-slate-400 hover:bg-slate-700/50 cursor-pointer">
                                            <div class="font-medium text-slate-300">{wf.name.clone()}</div>
                                            <div class="text-xs truncate">{description}</div>
                                        </div>
                                    }
                                }).collect::<Vec<_>>().into_any()
                            }
                        }}
                    </div>
                </div>
            </div>
            
            // Footer
            <div class="p-4 border-t border-slate-700">
                <div class="text-xs text-slate-500 text-center">
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
                "w-full flex items-center gap-3 px-3 py-2 rounded-lg text-left transition-colors {}",
                if is_selected.get() {
                    "bg-blue-600/20 text-blue-400"
                } else {
                    "text-slate-400 hover:bg-slate-700/50 hover:text-slate-300"
                }
            )
        >
            <span class="text-lg">{emoji}</span>
            <div class="flex-1 min-w-0">
                <div class="text-sm font-medium truncate">{name}</div>
                <div class="text-xs text-slate-500 truncate">{description}</div>
            </div>
        </button>
    }
}
