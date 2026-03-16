//! Agent selector dropdown

use leptos::prelude::*;
use crate::state::AppState;
use crate::types::AgentInfo;

/// Agent selector dropdown
#[component]
pub fn AgentSelector(
    /// Currently selected agent (None = auto-route)
    selected: RwSignal<Option<String>>,
) -> impl IntoView {
    let state = expect_context::<AppState>();
    let is_open = RwSignal::new(false);
    
    let toggle = move |_| is_open.update(|v| *v = !*v);
    
    let select_auto = move |_| {
        selected.set(None);
        is_open.set(false);
    };

    view! {
        <div class="relative">
            // Dropdown trigger
            <button
                on:click=toggle
                class="flex items-center gap-2 px-3 py-2 bg-slate-800 border border-slate-700 rounded-lg
                       hover:bg-slate-700 transition-colors text-sm"
            >
                <span class="text-slate-400">"Agent:"</span>
                <span class="text-white font-medium">
                    {move || selected.get().unwrap_or_else(|| "Auto".to_string())}
                </span>
                <svg
                    class=move || format!("w-4 h-4 text-slate-400 transition-transform {}", 
                        if is_open.get() { "rotate-180" } else { "" })
                    xmlns="http://www.w3.org/2000/svg"
                    viewBox="0 0 20 20"
                    fill="currentColor"
                >
                    <path
                        fill-rule="evenodd"
                        d="M5.293 7.293a1 1 0 011.414 0L10 10.586l3.293-3.293a1 1 0 111.414 1.414l-4 4a1 1 0 01-1.414 0l-4-4a1 1 0 010-1.414z"
                        clip-rule="evenodd"
                    />
                </svg>
            </button>
            
            // Dropdown menu
            <Show when=move || is_open.get()>
                <div class="absolute top-full left-0 mt-2 w-72 bg-slate-800 border border-slate-700 
                            rounded-lg shadow-xl overflow-hidden z-50 animate-fade-in">
                    // Auto option
                    <button
                        on:click=select_auto
                        class="w-full px-4 py-3 text-left hover:bg-slate-700 transition-colors border-b border-slate-700"
                    >
                        <div class="flex items-center gap-3">
                            <div class="w-8 h-8 rounded-full bg-gradient-to-br from-green-500 to-emerald-600 
                                        flex items-center justify-center text-sm">
                                "🔀"
                            </div>
                            <div>
                                <div class="font-medium text-white">"Auto (Router)"</div>
                                <div class="text-xs text-slate-400">"Automatically route to best agent"</div>
                            </div>
                        </div>
                    </button>
                    
                    // Agent list
                    <div class="max-h-64 overflow-y-auto scrollbar-thin scrollbar-thumb-slate-600 scrollbar-track-slate-800">
                        {move || {
                            let agents = state.agents.get();
                            agents.into_iter().map(|agent| {
                                let name = agent.name.clone();
                                view! {
                                    <AgentOption
                                        agent=agent.clone()
                                        is_selected=Signal::derive(move || selected.get().as_deref() == Some(name.as_str()))
                                        selected=selected
                                        is_open=is_open
                                    />
                                }
                            }).collect::<Vec<_>>()
                        }}
                    </div>
                </div>
            </Show>
        </div>
    }
}

/// Individual agent option in dropdown
#[component]
fn AgentOption(
    agent: AgentInfo,
    is_selected: Signal<bool>,
    selected: RwSignal<Option<String>>,
    is_open: RwSignal<bool>,
) -> impl IntoView {
    let emoji = match agent.name.as_str() {
        "orchestrator" => "🎭",
        "product" => "📦",
        "sales" => "💰",
        "finance" => "📊",
        "hr" => "👥",
        "invoice" => "📄",
        _ => "🤖",
    };
    
    let agent_name = agent.name.clone();
    let on_click = move |_| {
        selected.set(Some(agent_name.clone()));
        is_open.set(false);
    };
    
    view! {
        <button
            on:click=on_click
            class=move || format!(
                "w-full px-4 py-3 text-left hover:bg-slate-700 transition-colors flex items-center gap-3 {}",
                if is_selected.get() { "bg-slate-700/50" } else { "" }
            )
        >
            <div class="w-8 h-8 rounded-full bg-gradient-to-br from-violet-500 to-purple-600 
                        flex items-center justify-center text-sm">
                {emoji}
            </div>
            <div class="flex-1 min-w-0">
                <div class="font-medium text-white capitalize">{agent.name.clone()}</div>
                <div class="text-xs text-slate-400 truncate">{agent.description.clone()}</div>
            </div>
            <Show when=move || is_selected.get()>
                <svg class="w-5 h-5 text-blue-400" xmlns="http://www.w3.org/2000/svg" viewBox="0 0 20 20" fill="currentColor">
                    <path fill-rule="evenodd" d="M16.707 5.293a1 1 0 010 1.414l-8 8a1 1 0 01-1.414 0l-4-4a1 1 0 011.414-1.414L8 12.586l7.293-7.293a1 1 0 011.414 0z" clip-rule="evenodd" />
                </svg>
            </Show>
        </button>
    }
}
