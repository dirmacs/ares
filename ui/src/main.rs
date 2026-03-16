use ares_ui::App;
use leptos::prelude::*;

fn main() {
    // Initialize panic hook for better error messages
    console_error_panic_hook::set_once();
    
    // Initialize tracing for logging
    tracing_wasm::set_as_global_default();
    
    // Mount the app
    mount_to_body(App);
}
