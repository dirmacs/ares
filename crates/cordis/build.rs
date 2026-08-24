//! Emits the Cordis HMR ABI fingerprint ingredients as compile-time
//! environment variables, baked separately into this crate and into every
//! plugin dylib that links it.
//!
//! Format string (see `crate::hmr::fingerprint`):
//! `cordis-hmr/{ABI} cordis/{version} rustc/{release}-{commit} target/{target} panic/{panic}`

use std::process::Command;

fn main() {
    println!("cargo:rustc-env=CORDIS_ABI_VERSION=1");

    let crate_version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0".to_string());
    println!("cargo:rustc-env=CORDIS_CRATE_VERSION={crate_version}");

    // Parse `rustc --version --verbose` for the exact compiler build the
    // dylib is produced with; a toolchain mismatch must refuse the load.
    let mut rustc_release = "unknown".to_string();
    let mut rustc_commit = "unknown".to_string();
    if let Some(rustc) = rustc_path() {
        if let Ok(output) = Command::new(rustc)
            .arg("--version")
            .arg("--verbose")
            .output()
        {
            for line in String::from_utf8_lossy(&output.stdout).lines() {
                if let Some(rest) = line.strip_prefix("release: ") {
                    rustc_release = rest.trim().to_string();
                } else if let Some(rest) = line.strip_prefix("commit-hash: ") {
                    rustc_commit = rest.trim().to_string();
                    if rustc_commit.is_empty() {
                        rustc_commit = "unknown".to_string();
                    }
                }
            }
        }
    }
    println!("cargo:rustc-env=CORDIS_RUSTC_RELEASE={rustc_release}");
    println!("cargo:rustc-env=CORDIS_RUSTC_COMMIT_HASH={rustc_commit}");

    let target = std::env::var("TARGET").unwrap_or_else(|_| "unknown-target".to_string());
    println!("cargo:rustc-env=CORDIS_BUILD_TARGET={target}");

    let panic = match std::env::var("CARGO_PROFILE_PANIC") {
        Ok(p) if !p.is_empty() => p,
        _ => "unwind".to_string(),
    };
    println!("cargo:rustc-env=CORDIS_BUILD_PANIC={panic}");
}

/// Resolve the rustc binary cargo is driving: `$RUSTC` when set, else plain
/// `rustc` on PATH.
fn rustc_path() -> Option<String> {
    match std::env::var("RUSTC") {
        Ok(r) if !r.is_empty() => Some(r),
        _ => Some("rustc".to_string()),
    }
}
