//! HMR via `libloading` — deferred stub behind `#[cfg(feature = "hmr")]`.
//!
//! **Decision (YAGNI, plan Assumptions §Contingencies):** dynamic code swapping
//! via `libloading` is **deferred** due to ABI fragility. A `.so` built with one
//! Rust toolchain (`1.98`) cannot safely `dlopen` into a binary built with a
//! different patch, `extern "C"` entry points require `unsafe` and a stable ABI
//! boundary (`repr(C)`), and `libloading::Library::new` + `Symbol` (`unsafe`)
//! introduces soundness surface that `rust-doctor` would flag. The fallback
//! that already covers **90% of value** is file-watch + full `Fiber::reload`
//! via re-reading TOON/JSON (`crate::watcher::watch_many`), not dynamic code.
//!
//! This module is `#[cfg(feature = "hmr")]` and **off by default** (`Cargo.toml`
//! `hmr = ["dep:libloading"]`). Enabling it requires `cargo build --features hmr`
//! and a `.so` built with the same compiler + same `libloading` version. When
//! not enabled, hot-reload is still fully functional via `watcher` + `Loader` +
//! `ReflectService::notify` + `Fiber::refresh` (epoch recompute).
//!
//! The stub below shows the intended `libloading` shape so future implementors
//! can complete it without researching `libloading` API again. It is not
//! invoked by `src/main.rs` or `ReflectService` — `watcher` is the production path.

use std::path::Path;

use crate::{Context, CordisError};

/// Example dynamic plugin entry point signature that a `.so` would export.
///
/// ```rust,ignore
/// // In the .so crate (cdylib):
/// #[no_mangle]
/// pub extern "C" fn cordis_plugin_apply(ctx: *const std::ffi::c_void) -> i32 { 0 }
/// ```
pub type HmrEntryFn = unsafe extern "C" fn(*const std::ffi::c_void) -> i32;

/// Load a Cordis plugin `.so` via `libloading` and call its `Plugin::apply`.
///
/// **Safety:** `Library::new` + `get` are `unsafe`; caller must ensure `so_path`
/// was built with the same toolchain and that `entry_symbol` is a valid
/// `extern "C"` function. The `Library` must be retained (e.g. in `HmrLibrary`
/// below which frees on `Drop` / `Disposable::dispose`) so the `Symbol` remains
/// valid for the lifetime of the fiber. A full implementation stores the
/// `Library` in the fiber's `acc` and `dlclose`s on drop (via RAII, not leak).
///
/// Errors are mapped to `CordisError::Configuration` to preserve the
/// single-source discipline (duplicate provider, etc.).
///
/// This function is **not** used by `watcher`; it is a forward-compatibility
/// stub. File-watch + `Fiber::reload` via TOON re-read is the production path
/// (see `crate::watcher`).
#[cfg(feature = "hmr")]
pub fn load_plugin_so(
    so_path: impl AsRef<Path>,
    _ctx: &Context,
    entry_symbol: &[u8],
) -> Result<(), CordisError> {
    use libloading::{Library, Symbol};

    let path = so_path.as_ref();
    // SAFETY: caller guarantees ABI compatibility; documented above.
    let lib = unsafe { Library::new(path).map_err(|e| CordisError::Configuration(format!("dlopen {} failed: {e}", path.display())))? };

    // In a full impl, `lib` would be stored in the fiber's `Disposable` accumulator
    // so it stays alive until `Fiber::dispose` drops it (which `dlclose`s). For
    // this stub we just prove `dlopen` + `dlsym` compile and immediately drop
    // `lib` (no leak); production must retain `Library` in an `Arc` or owned
    // holder that frees on drop — see `HmrLibrary` below for the owned pattern.
    unsafe {
        let func: Symbol<HmrEntryFn> = lib
            .get(entry_symbol)
            .map_err(|e| CordisError::Configuration(format!("dlsym {} failed: {e}", String::from_utf8_lossy(entry_symbol))))?;
        let rc = func(std::ptr::null());
        if rc != 0 {
            return Err(CordisError::Configuration(format!("HMR entry {rc} != 0")));
        }
        // `lib` drops here; full impl would move it into `HmrLibrary` instead.
    }
    tracing::info!(path = %path.display(), "HMR plugin loaded via libloading (stub, no Fiber::reload yet)");
    Ok(())
}

/// Owned holder for an `hmr` dynamic library — RAII owner that frees on drop.
///
/// Use `Arc<HmrLibrary>` or store in the fiber's `Disposable` accumulator so
/// the `.so` stays loaded until `Fiber::dispose`. No `Box::leak`.
#[cfg(feature = "hmr")]
pub struct HmrLibrary {
    _lib: libloading::Library,
}

#[cfg(feature = "hmr")]
impl HmrLibrary {
    /// Retain a loaded library so it is not unloaded until this holder drops.
    pub fn new(lib: libloading::Library) -> Self {
        Self { _lib: lib }
    }
}

/// No-op when `hmr` feature is disabled — documents the deferral.
#[cfg(not(feature = "hmr"))]
pub fn load_plugin_so(
    _so_path: impl AsRef<Path>,
    _ctx: &Context,
    _entry_symbol: &[u8],
) -> Result<(), CordisError> {
    Err(CordisError::Configuration(
        "HMR via libloading is deferred (ABI fragility); use file-watch + Fiber::reload via watcher (90% value). Enable with --features hmr".into(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmr_stub_documents_deferral() {
        let ctx = Context::new_root();
        let err = load_plugin_so("/tmp/fake.so", &ctx, b"cordis_plugin_apply").unwrap_err();
        assert!(err.to_string().contains("deferred") || err.to_string().contains("dlopen") || err.to_string().contains("HMR"));
    }
}
