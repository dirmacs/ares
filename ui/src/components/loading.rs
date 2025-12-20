//! Loading indicators

use leptos::prelude::*;

/// Animated loading dots
#[component]
pub fn LoadingDots() -> impl IntoView {
    view! {
        <div class="flex items-center gap-1">
            <span class="w-2 h-2 bg-blue-400 rounded-full dot-bounce-1"></span>
            <span class="w-2 h-2 bg-blue-400 rounded-full dot-bounce-2"></span>
            <span class="w-2 h-2 bg-blue-400 rounded-full dot-bounce-3"></span>
        </div>
    }
}

/// Spinner loading indicator
#[component]
pub fn LoadingSpinner(
    #[prop(default = "w-5 h-5")] size: &'static str,
) -> impl IntoView {
    view! {
        <svg
            class=format!("{} animate-spin text-blue-500", size)
            xmlns="http://www.w3.org/2000/svg"
            fill="none"
            viewBox="0 0 24 24"
        >
            <circle
                class="opacity-25"
                cx="12"
                cy="12"
                r="10"
                stroke="currentColor"
                stroke-width="4"
            ></circle>
            <path
                class="opacity-75"
                fill="currentColor"
                d="M4 12a8 8 0 018-8V0C5.373 0 0 5.373 0 12h4zm2 5.291A7.962 7.962 0 014 12H0c0 3.042 1.135 5.824 3 7.938l3-2.647z"
            ></path>
        </svg>
    }
}

/// Typing indicator for assistant messages
#[component]
pub fn TypingIndicator(
    #[prop(optional)] agent_name: Option<String>,
) -> impl IntoView {
    view! {
        <div class="flex items-start gap-3 message-appear">
            <div class="w-8 h-8 rounded-full bg-gradient-to-br from-violet-500 to-purple-600 flex items-center justify-center text-white text-sm font-medium shrink-0">
                "🤖"
            </div>
            <div class="flex flex-col gap-1">
                {agent_name.map(|name| view! {
                    <span class="text-xs text-slate-500 font-medium">{name}</span>
                })}
                <div class="px-4 py-3 bg-slate-800 rounded-2xl rounded-tl-sm">
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
        <div class="fixed inset-0 bg-slate-900/80 backdrop-blur-sm flex items-center justify-center z-50">
            <div class="flex flex-col items-center gap-4">
                <LoadingSpinner size="w-12 h-12" />
                <p class="text-slate-300 font-medium">{message}</p>
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
        <div class=format!("bg-slate-700 rounded animate-pulse {}", class)></div>
    }
}
