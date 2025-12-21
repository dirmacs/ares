//! Build script for A.R.E.S
//!
//! This script runs at compile time and performs the following checks:
//! - When the `ui` feature is enabled, verifies that a Node.js runtime
//!   (bun, npm, or deno) is available for building the UI assets.

fn main() {
    // Re-run this script if build.rs changes
    println!("cargo:rerun-if-changed=build.rs");

    // Check for Node.js runtime only when UI feature is enabled
    #[cfg(feature = "ui")]
    check_node_runtime();

    // Re-run if UI source files change (when UI feature is enabled)
    #[cfg(feature = "ui")]
    {
        println!("cargo:rerun-if-changed=ui/dist/");
        println!("cargo:rerun-if-changed=ui/src/");
        println!("cargo:rerun-if-changed=ui/index.html");
        println!("cargo:rerun-if-changed=ui/Cargo.toml");
    }
}

/// Check that a Node.js runtime is available when building with UI feature
#[cfg(feature = "ui")]
fn check_node_runtime() {
    use std::process::Command;

    let runtimes = [
        ("bun", vec!["--version"]),
        ("npm", vec!["--version"]),
        ("deno", vec!["--version"]),
    ];

    let mut found_runtime: Option<&str> = None;

    for (runtime, args) in &runtimes {
        if let Ok(output) = Command::new(runtime).args(args).output() {
            if output.status.success() {
                found_runtime = Some(runtime);
                let version = String::from_utf8_lossy(&output.stdout);
                let version = version.trim();
                println!("cargo:warning=A.R.E.S UI: Found {} ({})", runtime, version);
                break;
            }
        }
    }

    if found_runtime.is_none() {
        println!("cargo:warning=");
        println!("cargo:warning=╔══════════════════════════════════════════════════════════════╗");
        println!("cargo:warning=║  ERROR: No Node.js runtime found!                            ║");
        println!("cargo:warning=║                                                              ║");
        println!("cargo:warning=║  The 'ui' feature requires one of the following:             ║");
        println!("cargo:warning=║    • bun   - https://bun.sh (recommended)                    ║");
        println!("cargo:warning=║    • npm   - https://nodejs.org                              ║");
        println!("cargo:warning=║    • deno  - https://deno.land                               ║");
        println!("cargo:warning=║                                                              ║");
        println!("cargo:warning=║  Install one of these runtimes and try again.                ║");
        println!("cargo:warning=║                                                              ║");
        println!("cargo:warning=║  Quick install (bun):                                        ║");
        println!("cargo:warning=║    curl -fsSL https://bun.sh/install | bash                  ║");
        println!("cargo:warning=╚══════════════════════════════════════════════════════════════╝");
        println!("cargo:warning=");

        // Fail the build
        panic!(
            "\n\nBuild failed: No Node.js runtime (bun, npm, or deno) found.\n\
             The 'ui' feature requires a Node.js runtime for building UI assets.\n\
             Please install bun, npm, or deno and try again.\n\n\
             Quick install (bun): curl -fsSL https://bun.sh/install | bash\n"
        );
    }

    // Check if UI dist directory exists and has content
    let ui_dist = std::path::Path::new("ui/dist");
    if !ui_dist.exists() || !ui_dist.join("index.html").exists() {
        println!("cargo:warning=");
        println!("cargo:warning=╔══════════════════════════════════════════════════════════════╗");
        println!("cargo:warning=║  WARNING: UI assets not found in ui/dist/                    ║");
        println!("cargo:warning=║                                                              ║");
        println!("cargo:warning=║  You need to build the UI before compiling with --features ui║");
        println!("cargo:warning=║                                                              ║");
        println!("cargo:warning=║  Run these commands first:                                   ║");
        println!(
            "cargo:warning=║    cd ui && {} install",
            found_runtime.unwrap_or("npm")
        );
        println!("cargo:warning=║    cd ui && trunk build --release                            ║");
        println!("cargo:warning=║                                                              ║");
        println!("cargo:warning=║  Or use just:                                                ║");
        println!("cargo:warning=║    just build-ui                                             ║");
        println!("cargo:warning=╚══════════════════════════════════════════════════════════════╝");
        println!("cargo:warning=");

        // This is a warning, not an error - the build might still work if
        // the user is doing a two-step build process
    }
}
