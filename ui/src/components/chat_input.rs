//! Chat input component

use crate::types::ContentPart;
use js_sys::Promise;
use leptos::prelude::*;
use leptos::task::spawn_local;
use wasm_bindgen::closure::Closure;
use wasm_bindgen::JsCast;
use wasm_bindgen::JsValue;
use wasm_bindgen_futures::JsFuture;
use web_sys::{File, HtmlInputElement, HtmlTextAreaElement, ProgressEvent};

const MAX_ATTACHMENT_BYTES: f64 = 4.0 * 1024.0 * 1024.0;

#[derive(Clone, PartialEq)]
struct PendingPart {
    id: String,
    part: ContentPart,
}

/// Chat input with auto-resize textarea and file attachments
#[component]
pub fn ChatInput(
    /// Current input value
    value: RwSignal<String>,
    /// Called when user submits with text and any pending ContentParts
    on_submit: impl Fn(String, Vec<ContentPart>) + 'static + Clone,
    /// Whether input is disabled
    #[prop(default = false)]
    disabled: bool,
    /// Placeholder text
    #[prop(default = "Type your message...")]
    placeholder: &'static str,
) -> impl IntoView {
    let textarea_ref = NodeRef::<leptos::html::Textarea>::new();
    let file_input_ref = NodeRef::<leptos::html::Input>::new();
    let pending = RwSignal::new(Vec::<PendingPart>::new());
    let can_submit =
        Signal::derive(move || !value.get().trim().is_empty() || !pending.get().is_empty());

    let submit = move || {
        if disabled {
            return;
        }
        let text = value.get().trim().to_string();
        let parts: Vec<ContentPart> = pending.get().into_iter().map(|p| p.part).collect();
        if text.is_empty() && parts.is_empty() {
            return;
        }
        pending.set(Vec::new());
        on_submit(text, parts);
    };

    // Auto-resize textarea
    let resize_textarea = move || {
        if let Some(textarea) = textarea_ref.get() {
            let el: &HtmlTextAreaElement = textarea.as_ref();
            let scroll_height = el.scroll_height();
            let max_height = 200;
            let new_height = scroll_height.min(max_height);
            let _ = el.set_attribute(
                "style",
                &format!("height: {}px; max-height: 200px;", new_height),
            );
        }
    };

    let on_input = move |ev: web_sys::Event| {
        let target = ev.target().unwrap();
        let textarea = target.dyn_into::<HtmlTextAreaElement>().unwrap();
        value.set(textarea.value());
        resize_textarea();
    };

    let on_keydown = {
        let submit = submit.clone();
        move |ev: web_sys::KeyboardEvent| {
            if ev.key() == "Enter" && !ev.shift_key() {
                ev.prevent_default();
                submit();
            }
        }
    };

    let on_button_click = {
        let submit = submit.clone();
        move |_| submit()
    };

    let open_file_picker = move |_| {
        if disabled {
            return;
        }
        if let Some(input) = file_input_ref.get() {
            input.click();
        }
    };

    let on_files = move |ev: web_sys::Event| {
        let Some(target) = ev.target() else {
            return;
        };
        let Ok(input) = target.dyn_into::<HtmlInputElement>() else {
            return;
        };
        let mut files: Vec<File> = Vec::new();
        if let Some(list) = input.files() {
            for i in 0..list.length() {
                if let Some(file) = list.item(i) {
                    files.push(file);
                }
            }
        }
        input.set_value("");
        spawn_local(async move {
            for file in files {
                match file_to_content_part(file).await {
                    Ok(part) => {
                        pending.update(|p| {
                            p.push(PendingPart {
                                id: uuid::Uuid::new_v4().to_string(),
                                part,
                            });
                        });
                    }
                    Err(e) => tracing::warn!("attachment skipped: {e}"),
                }
            }
        });
    };

    view! {
        <div class="glass border-t border-[var(--border-default)]">
            <Show when=move || !pending.get().is_empty()>
                <div class="flex flex-wrap gap-2 px-4 pt-3">
                    {move || {
                        pending.get().into_iter().map(|item| {
                            let id = item.id.clone();
                            let label = item.part.chip_label();
                            let thumb = item.part.image_src();
                            view! {
                                <span class="inline-flex items-center gap-2 px-2 py-1 rounded-lg text-xs
                                             border border-[var(--border-default)] bg-[var(--bg-secondary)]
                                             text-[var(--text-secondary)]">
                                    {thumb.map(|src| view! {
                                        <img src=src alt="" class="w-8 h-8 rounded object-cover" />
                                    })}
                                    <span class="max-w-[10rem] truncate">{label}</span>
                                    <button
                                        type="button"
                                        class="text-[var(--text-muted)] hover:text-[var(--text-primary)]"
                                        aria-label="Remove attachment"
                                        on:click=move |_| {
                                            pending.update(|p| p.retain(|x| x.id != id));
                                        }
                                    >
                                        <svg xmlns="http://www.w3.org/2000/svg" class="w-3.5 h-3.5" fill="none"
                                             viewBox="0 0 24 24" stroke="currentColor" stroke-width="2">
                                            <path stroke-linecap="round" stroke-linejoin="round" d="M6 18L18 6M6 6l12 12" />
                                        </svg>
                                    </button>
                                </span>
                            }
                        }).collect::<Vec<_>>()
                    }}
                </div>
            </Show>

            <div class="flex items-end gap-3 p-4">
                <input
                    type="file"
                    accept="image/*,application/pdf"
                    multiple
                    node_ref=file_input_ref
                    on:change=on_files
                    class="hidden"
                    disabled=disabled
                />
                <button
                    type="button"
                    on:click=open_file_picker
                    disabled=disabled
                    class="btn btn-ghost p-3 disabled:opacity-40 disabled:cursor-not-allowed"
                    title="Attach image or PDF"
                    aria-label="Attach image or PDF"
                >
                    <svg xmlns="http://www.w3.org/2000/svg" class="w-5 h-5" fill="none" viewBox="0 0 24 24"
                         stroke="currentColor" stroke-width="2">
                        <path stroke-linecap="round" stroke-linejoin="round"
                              d="M15.172 7l-6.586 6.586a2 2 0 102.828 2.828l6.414-6.586a4 4 0 00-5.656-5.656l-6.415 6.585a6 6 0 108.486 8.486L20.5 13" />
                    </svg>
                </button>
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
                    view! {
                        <button
                            type="button"
                            on:click=on_button_click
                            disabled=move || is_disabled || !can_submit.get()
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
        </div>
    }
}

async fn file_to_content_part(file: File) -> Result<ContentPart, String> {
    let name = file.name();
    let size = file.size();
    if size > MAX_ATTACHMENT_BYTES {
        let msg = format!("Skipping oversized file {name} ({size:.0} bytes; max 4 MiB)");
        tracing::warn!("{msg}");
        web_sys::console::warn_1(&JsValue::from_str(&msg));
        return Err(msg);
    }

    let data_url = read_file_as_data_url(&file).await?;
    ContentPart::from_data_url(&data_url, name)
}

async fn read_file_as_data_url(file: &File) -> Result<String, String> {
    let reader =
        web_sys::FileReader::new().map_err(|e| format!("failed to create FileReader: {e:?}"))?;
    let reader_for_cb = reader.clone();
    let file_for_read = file.clone();

    let promise = Promise::new(&mut |resolve, reject| {
        let reject_onload = reject.clone();
        let reject_onerror = reject.clone();
        let reject_read = reject.clone();

        let reader_cb = reader_for_cb.clone();
        let onload =
            Closure::<dyn FnMut(ProgressEvent)>::once(
                move |_ev: ProgressEvent| match reader_cb.result() {
                    Ok(value) => {
                        let _ = resolve.call1(&JsValue::UNDEFINED, &value);
                    }
                    Err(err) => {
                        let _ = reject_onload.call1(&JsValue::UNDEFINED, &err);
                    }
                },
            );
        let onerror = Closure::<dyn FnMut(ProgressEvent)>::once(move |_ev: ProgressEvent| {
            let _ =
                reject_onerror.call1(&JsValue::UNDEFINED, &JsValue::from_str("FileReader error"));
        });

        reader.set_onload(Some(onload.as_ref().unchecked_ref()));
        reader.set_onerror(Some(onerror.as_ref().unchecked_ref()));
        onload.forget();
        onerror.forget();

        if let Err(err) = reader.read_as_data_url(&file_for_read) {
            let _ = reject_read.call1(&JsValue::UNDEFINED, &err);
        }
    });

    let result = JsFuture::from(promise)
        .await
        .map_err(|e| format!("FileReader failed: {e:?}"))?;
    result
        .as_string()
        .ok_or_else(|| "FileReader result is not a string".to_string())
}
