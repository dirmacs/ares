//! Login/Register page

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use crate::api::{login, register};
use crate::components::{Header, LoadingSpinner};
use crate::state::AppState;

/// Login/Register page
#[component]
pub fn LoginPage() -> impl IntoView {
    let state = expect_context::<AppState>();
    let navigate = use_navigate();
    
    // Form state
    let is_register = RwSignal::new(false);
    let email = RwSignal::new(String::new());
    let password = RwSignal::new(String::new());
    let name = RwSignal::new(String::new());
    let is_loading = RwSignal::new(false);
    let error = RwSignal::new(Option::<String>::None);
    
    // Redirect if already logged in
    let navigate_for_redirect = navigate.clone();
    Effect::new(move |_| {
        if state.token.get().is_some() {
            navigate_for_redirect("/chat", Default::default());
        }
    });
    
    // Handle form submission
    let navigate_for_submit = navigate.clone();
    let state_for_submit = state.clone();
    let on_submit = move |ev: web_sys::SubmitEvent| {
        ev.prevent_default();
        
        let email_val = email.get();
        let password_val = password.get();
        let name_val = name.get();
        let is_reg = is_register.get();
        let state = state_for_submit.clone();
        let navigate = navigate_for_submit.clone();
        
        spawn_local(async move {
            is_loading.set(true);
            error.set(None);
            
            let base_url = state.api_base.get_untracked();
            
            let result = if is_reg {
                register(&base_url, &email_val, &password_val, &name_val).await
            } else {
                login(&base_url, &email_val, &password_val).await
            };
            
            is_loading.set(false);
            
            match result {
                Ok(auth) => {
                    state.save_auth(&auth);
                    navigate("/chat", Default::default());
                }
                Err(e) => {
                    error.set(Some(e));
                }
            }
        });
    };

    view! {
        <div class="min-h-screen flex flex-col">
            <Header />
            
            <main class="flex-1 flex items-center justify-center px-4 py-16">
                <div class="w-full max-w-md">
                    // Card
                    <div class="bg-slate-800 rounded-2xl border border-slate-700 p-8 shadow-xl">
                        // Header
                        <div class="text-center mb-8">
                            <div class="w-16 h-16 mx-auto rounded-xl bg-gradient-to-br from-blue-500 to-violet-600 
                                        flex items-center justify-center text-3xl mb-4">
                                "🤖"
                            </div>
                            <h1 class="text-2xl font-bold">
                                {move || if is_register.get() { "Create Account" } else { "Welcome Back" }}
                            </h1>
                            <p class="text-slate-400 mt-2">
                                {move || if is_register.get() {
                                    "Sign up to start using A.R.E.S"
                                } else {
                                    "Sign in to continue"
                                }}
                            </p>
                        </div>
                        
                        // Error message
                        <Show when=move || error.get().is_some()>
                            <div class="mb-6 p-4 bg-red-500/10 border border-red-500/50 rounded-lg text-red-400 text-sm">
                                {move || error.get().unwrap_or_default()}
                            </div>
                        </Show>
                        
                        // Form
                        <form on:submit=on_submit class="space-y-5">
                            // Name field (register only)
                            <Show when=move || is_register.get()>
                                <div>
                                    <label class="block text-sm font-medium text-slate-300 mb-2">
                                        "Name"
                                    </label>
                                    <input
                                        type="text"
                                        prop:value=move || name.get()
                                        on:input=move |ev| name.set(event_target_value(&ev))
                                        placeholder="Your name"
                                        required=is_register.get()
                                        class="w-full px-4 py-3 bg-slate-900 border border-slate-700 rounded-lg
                                               text-white placeholder-slate-500
                                               focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                    />
                                </div>
                            </Show>
                            
                            // Email field
                            <div>
                                <label class="block text-sm font-medium text-slate-300 mb-2">
                                    "Email"
                                </label>
                                <input
                                    type="email"
                                    prop:value=move || email.get()
                                    on:input=move |ev| email.set(event_target_value(&ev))
                                    placeholder="you@example.com"
                                    required=true
                                    class="w-full px-4 py-3 bg-slate-900 border border-slate-700 rounded-lg
                                           text-white placeholder-slate-500
                                           focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                />
                            </div>
                            
                            // Password field
                            <div>
                                <label class="block text-sm font-medium text-slate-300 mb-2">
                                    "Password"
                                </label>
                                <input
                                    type="password"
                                    prop:value=move || password.get()
                                    on:input=move |ev| password.set(event_target_value(&ev))
                                    placeholder="••••••••"
                                    required=true
                                    minlength="8"
                                    class="w-full px-4 py-3 bg-slate-900 border border-slate-700 rounded-lg
                                           text-white placeholder-slate-500
                                           focus:outline-none focus:ring-2 focus:ring-blue-500 focus:border-transparent"
                                />
                                <Show when=move || is_register.get()>
                                    <p class="text-xs text-slate-500 mt-1">"Minimum 8 characters"</p>
                                </Show>
                            </div>
                            
                            // Submit button
                            <button
                                type="submit"
                                disabled=move || is_loading.get()
                                class="w-full py-3 bg-blue-600 hover:bg-blue-700 disabled:bg-blue-600/50
                                       rounded-lg font-semibold transition-colors flex items-center justify-center gap-2"
                            >
                                <Show when=move || is_loading.get()>
                                    <LoadingSpinner size="w-5 h-5" />
                                </Show>
                                {move || if is_register.get() { "Create Account" } else { "Sign In" }}
                            </button>
                        </form>
                        
                        // Toggle login/register
                        <div class="mt-6 text-center text-sm text-slate-400">
                            {move || if is_register.get() {
                                "Already have an account? "
                            } else {
                                "Don't have an account? "
                            }}
                            <button
                                on:click=move |_| {
                                    is_register.update(|v| *v = !*v);
                                    error.set(None);
                                }
                                class="text-blue-400 hover:text-blue-300 font-medium"
                            >
                                {move || if is_register.get() { "Sign in" } else { "Sign up" }}
                            </button>
                        </div>
                    </div>
                    
                    // Demo credentials hint
                    <div class="mt-6 p-4 bg-slate-800/50 rounded-lg border border-slate-700 text-sm text-slate-400">
                        <p class="font-medium text-slate-300 mb-1">"💡 Demo Mode"</p>
                        <p>"Register with any email to get started. No email verification required."</p>
                    </div>
                </div>
            </main>
        </div>
    }
}
