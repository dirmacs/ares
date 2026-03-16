//! Login/Register page

use leptos::prelude::*;
use leptos::task::spawn_local;
use leptos_router::hooks::use_navigate;
use crate::api::{login, register};
use crate::components::Header;
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
        <div class="min-h-screen flex flex-col bg-[var(--bg-primary)]">
            <Header />
            
            <main class="auth-container flex-1">
                <div class="w-full max-w-md px-4">
                    // Card
                    <div class="auth-card">
                        // Header
                        <div class="auth-header">
                            <img 
                                src="/assets/ares.png" 
                                alt="ARES Logo"
                                class="auth-logo"
                            />
                            <h1 class="auth-title text-gradient">
                                {move || if is_register.get() { "Create Account" } else { "Welcome Back" }}
                            </h1>
                            <p class="auth-subtitle">
                                {move || if is_register.get() {
                                    "Sign up to start using A.R.E.S"
                                } else {
                                    "Sign in to continue"
                                }}
                            </p>
                        </div>
                        
                        // Error message
                        <Show when=move || error.get().is_some()>
                            <div class="mb-6 p-4 bg-[var(--accent-error)]/10 border border-[var(--accent-error)]/50 
                                        rounded-[var(--radius-md)] text-[var(--accent-error)] text-sm animate-fade-in">
                                {move || error.get().unwrap_or_default()}
                            </div>
                        </Show>
                        
                        // Form
                        <form on:submit=on_submit class="auth-form">
                            // Name field (register only)
                            <Show when=move || is_register.get()>
                                <div class="auth-input-group animate-fade-in-down">
                                    <label class="auth-label">"Name"</label>
                                    <input
                                        type="text"
                                        prop:value=move || name.get()
                                        on:input=move |ev| name.set(event_target_value(&ev))
                                        placeholder="Your name"
                                        required=is_register.get()
                                        class="input"
                                    />
                                </div>
                            </Show>
                            
                            // Email field
                            <div class="auth-input-group">
                                <label class="auth-label">"Email"</label>
                                <input
                                    type="email"
                                    prop:value=move || email.get()
                                    on:input=move |ev| email.set(event_target_value(&ev))
                                    placeholder="you@example.com"
                                    required=true
                                    class="input"
                                />
                            </div>
                            
                            // Password field
                            <div class="auth-input-group">
                                <label class="auth-label">"Password"</label>
                                <input
                                    type="password"
                                    prop:value=move || password.get()
                                    on:input=move |ev| password.set(event_target_value(&ev))
                                    placeholder="••••••••"
                                    required=true
                                    minlength="8"
                                    class="input"
                                />
                                <Show when=move || is_register.get()>
                                    <p class="text-xs text-[var(--text-muted)] mt-1">"Minimum 8 characters"</p>
                                </Show>
                            </div>
                            
                            // Submit button
                            <button
                                type="submit"
                                disabled=move || is_loading.get()
                                class="btn btn-primary w-full py-3"
                            >
                                <Show when=move || is_loading.get()>
                                    <div class="loading-spinner"></div>
                                </Show>
                                {move || if is_register.get() { "Create Account" } else { "Sign In" }}
                            </button>
                        </form>
                        
                        // Toggle login/register
                        <div class="auth-footer">
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
                                class="auth-link"
                            >
                                {move || if is_register.get() { "Sign in" } else { "Sign up" }}
                            </button>
                        </div>
                    </div>
                    
                    // Demo credentials hint
                    <div class="mt-6 card p-4 text-sm text-[var(--text-secondary)] animate-fade-in-up stagger-3">
                        <p class="font-medium text-[var(--text-primary)] mb-1">"💡 Demo Mode"</p>
                        <p>"Register with any email to get started. No email verification required."</p>
                    </div>
                </div>
            </main>
        </div>
    }
}
