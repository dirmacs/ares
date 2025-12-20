//! Header component

use leptos::prelude::*;
use leptos_router::hooks::use_navigate;
use crate::state::AppState;

/// Main application header
#[component]
pub fn Header() -> impl IntoView {
    let state = expect_context::<AppState>();
    let navigate = use_navigate();
    
    let is_auth = Signal::derive(move || state.token.get().is_some());

    view! {
        <header class="h-16 bg-slate-800/80 backdrop-blur-md border-b border-slate-700 sticky top-0 z-40">
            <div class="h-full max-w-7xl mx-auto px-4 flex items-center justify-between">
                // Logo
                <a href="/" class="flex items-center gap-3 hover:opacity-80 transition-opacity">
                    <div class="w-10 h-10 rounded-lg bg-gradient-to-br from-blue-500 to-violet-600 flex items-center justify-center text-2xl">
                        "🤖"
                    </div>
                    <div>
                        <h1 class="text-xl font-bold gradient-text">"A.R.E.S"</h1>
                        <p class="text-xs text-slate-500 -mt-0.5">"Agentic Reasoning & Execution"</p>
                    </div>
                </a>
                
                // Navigation
                <nav class="flex items-center gap-4">
                    <Show when=move || is_auth.get()>
                        <a
                            href="/chat"
                            class="px-4 py-2 text-sm font-medium text-slate-300 hover:text-white transition-colors"
                        >
                            "Chat"
                        </a>
                    </Show>
                    
                    {move || {
                        if is_auth.get() {
                            let state = state.clone();
                            let navigate = navigate.clone();
                            view! {
                                <button
                                    on:click=move |_| {
                                        state.clear_auth();
                                        navigate("/", Default::default());
                                    }
                                    class="px-4 py-2 text-sm font-medium text-slate-400 hover:text-white transition-colors"
                                >
                                    "Sign Out"
                                </button>
                            }.into_any()
                        } else {
                            view! {
                                <a
                                    href="/login"
                                    class="px-4 py-2 bg-blue-600 hover:bg-blue-700 rounded-lg text-sm font-medium transition-colors"
                                >
                                    "Sign In"
                                </a>
                            }.into_any()
                        }
                    }}
                </nav>
            </div>
        </header>
    }
}
