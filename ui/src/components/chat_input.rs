//! Chat input component

use leptos::prelude::*;
use web_sys::HtmlTextAreaElement;
use wasm_bindgen::JsCast;

/// Chat input with auto-resize textarea
#[component]
pub fn ChatInput(
    /// Current input value
    value: RwSignal<String>,
    /// Called when user submits
    on_submit: impl Fn() + 'static + Clone,
    /// Whether input is disabled
    #[prop(default = false)]
    disabled: bool,
    /// Placeholder text
    #[prop(default = "Type your message...")]
    placeholder: &'static str,
) -> impl IntoView {
    let textarea_ref = NodeRef::<leptos::html::Textarea>::new();
    let on_submit_clone = on_submit.clone();
    
    // Auto-resize textarea
    let resize_textarea = move || {
        if let Some(textarea) = textarea_ref.get() {
            let el: &HtmlTextAreaElement = textarea.as_ref();
            // Use set_attribute for inline styles
            let scroll_height = el.scroll_height();
            let max_height = 200;
            let new_height = scroll_height.min(max_height);
            let _ = el.set_attribute("style", &format!("height: {}px; max-height: 200px;", new_height));
        }
    };
    
    // Handle input changes
    let on_input = move |ev: web_sys::Event| {
        let target = ev.target().unwrap();
        let textarea = target.dyn_into::<HtmlTextAreaElement>().unwrap();
        value.set(textarea.value());
        resize_textarea();
    };
    
    // Handle key press (Enter to submit, Shift+Enter for newline)
    let on_keydown = {
        let on_submit = on_submit_clone.clone();
        move |ev: web_sys::KeyboardEvent| {
            if ev.key() == "Enter" && !ev.shift_key() {
                ev.prevent_default();
                if !value.get().trim().is_empty() {
                    on_submit();
                }
            }
        }
    };
    
    // Handle button click
    let on_button_click = {
        let on_submit = on_submit.clone();
        move |_| {
            if !value.get().trim().is_empty() {
                on_submit();
            }
        }
    };

    view! {
        <div class="flex items-end gap-3 p-4 glass border-t border-[var(--border-default)]">
            <div class="flex-1 relative">
                <textarea
                    node_ref=textarea_ref
                    prop:value=move || value.get()
                    on:input=on_input
                    on:keydown=on_keydown
                    placeholder=placeholder
                    disabled=disabled
                    rows="1"
                    class="input resize-none scrollbar-thin"
                    style="max-height: 200px; padding-right: 3rem;"
                ></textarea>
            </div>
            
            {
                let is_disabled = disabled;
                let is_empty = Signal::derive(move || value.get().trim().is_empty());
                view! {
                    <button
                        on:click=on_button_click
                        disabled=move || is_disabled || is_empty.get()
                        class="btn btn-primary p-3 disabled:opacity-40 disabled:cursor-not-allowed 
                               disabled:transform-none disabled:shadow-none"
                    >
                        <svg
                            xmlns="http://www.w3.org/2000/svg"
                            class="w-5 h-5"
                            viewBox="0 0 24 24"
                            fill="none"
                            stroke="currentColor"
                            stroke-width="2"
                            stroke-linecap="round"
                            stroke-linejoin="round"
                        >
                            <line x1="22" y1="2" x2="11" y2="13"></line>
                            <polygon points="22 2 15 22 11 13 2 9 22 2"></polygon>
                        </svg>
                    </button>
                }
            }
        </div>
    }
}
