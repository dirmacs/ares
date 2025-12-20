//! Loading indicators

use leptos::prelude::*;

/// Animated loading dots
#[component]
pub fn LoadingDots() -> impl IntoView {
    view! {
        <div class="typing-indicator">
            <span class="typing-dot"></span>
            <span class="typing-dot"></span>
            <span class="typing-dot"></span>
        </div>
    }
}

/// Spinner loading indicator
#[component]
pub fn LoadingSpinner(
    #[prop(default = "w-5 h-5")] size: &'static str,
) -> impl IntoView {
    view! {
        <div class=format!("loading-spinner {}", size)></div>
    }
}

/// Typing indicator for assistant messages
#[component]
pub fn TypingIndicator(
    #[prop(optional)] agent_name: Option<String>,
) -> impl IntoView {
    view! {
        <div class="flex items-start gap-4 animate-fade-in-up">
            <div class="w-9 h-9 rounded-lg bg-gradient-to-br from-violet-500 to-purple-600 
                        flex items-center justify-center text-white text-sm font-medium shrink-0
                        shadow-lg shadow-purple-500/20">
                "A"
            </div>
            <div class="flex flex-col gap-1.5">
                {agent_name.map(|name| view! {
                    <span class="agent-badge">
                        <span class="w-1.5 h-1.5 rounded-full bg-current animate-pulse"></span>
                        {name}
                    </span>
                })}
                <div class="message-assistant px-4 py-3">
                    <LoadingDots />
                </div>
            </div>
        </div>
    }
}

/// Full-page loading overlay
#[component]
pub fn LoadingOverlay(
    #[prop(default = "Loading...")] message: &'static str,
) -> impl IntoView {
    view! {
        <div class="fixed inset-0 bg-[var(--bg-primary)]/80 backdrop-blur-sm flex items-center justify-center z-50 animate-fade-in">
            <div class="flex flex-col items-center gap-4">
                <div class="loading-spinner w-12 h-12"></div>
                <p class="text-[var(--text-secondary)] font-medium">{message}</p>
            </div>
        </div>
    }
}

/// Skeleton loader for content
#[component]
pub fn Skeleton(
    #[prop(default = "h-4 w-full")] class: &'static str,
) -> impl IntoView {
    view! {
        <div class=format!("skeleton {}", class)></div>
    }
}
