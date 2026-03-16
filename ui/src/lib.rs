//! A.R.E.S Chat UI - Modern Leptos frontend
//!
//! A sleek, responsive chat interface for the ARES agentic server.

pub mod api;
pub mod components;
pub mod pages;
pub mod state;
pub mod types;

use leptos::prelude::*;
use leptos_router::{
    components::{Route, Router, Routes},
    path,
};

use pages::{chat::ChatPage, home::HomePage, login::LoginPage};
use state::AppState;

/// Main application component
#[component]
pub fn App() -> impl IntoView {
    // Initialize global state
    let app_state = AppState::new();
    provide_context(app_state);

    view! {
        <Router>
            <main class="min-h-screen bg-slate-900 text-slate-100">
                <Routes fallback=|| view! { <NotFound /> }>
                    <Route path=path!("/") view=HomePage />
                    <Route path=path!("/login") view=LoginPage />
                    <Route path=path!("/chat") view=ChatPage />
                    <Route path=path!("/chat/:agent") view=ChatPage />
                </Routes>
            </main>
        </Router>
    }
}

/// 404 Not Found page
#[component]
fn NotFound() -> impl IntoView {
    view! {
        <div class="min-h-screen flex items-center justify-center">
            <div class="text-center">
                <h1 class="text-6xl font-bold text-slate-500 mb-4">"404"</h1>
                <p class="text-xl text-slate-400 mb-8">"Page not found"</p>
                <a
                    href="/"
                    class="px-6 py-3 bg-blue-600 hover:bg-blue-700 rounded-lg font-medium transition-colors"
                >
                    "Go Home"
                </a>
            </div>
        </div>
    }
}
