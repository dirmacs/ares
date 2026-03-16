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
        <header class="header h-16 sticky top-0 z-40">
            <div class="h-full max-w-7xl mx-auto px-4 flex items-center justify-between">
                // Logo
                <a href="/" class="logo hover:opacity-80 transition-opacity">
                    <img 
                        src="/assets/ares.png" 
                        alt="ARES Logo" 
                        class="logo-image"
                    />
                    <div>
                        <h1 class="text-xl font-bold text-gradient">"A.R.E.S"</h1>
                        <p class="text-xs text-[var(--text-muted)] -mt-0.5">"Agentic Reasoning & Execution"</p>
                    </div>
                </a>
                
                // Navigation
                <nav class="flex items-center gap-2">
                    <Show when=move || is_auth.get()>
                        <a
                            href="/chat"
                            class="btn btn-ghost"
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
                                    class="btn btn-ghost"
                                >
                                    "Sign Out"
                                </button>
                            }.into_any()
                        } else {
                            view! {
                                <a
                                    href="/login"
                                    class="btn btn-primary"
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
