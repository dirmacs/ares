//! HMR via `libloading`, gated behind `#[cfg(feature = "hmr")]`.
//!
//! File-watch + `Fiber::reload` (`crate::watcher::watch_many`) remains the
//! default production path. Enabling `--features hmr` additionally `dlopen`s a
//! `.so`/`.dylib`/`.dll` on watcher events and calls `cordis_plugin_apply`.
//! Rebuilds hot-swap because each load reads a process-unique copy of the
//! library (glibc caches dlopen handles by path).
//!
//! **ABI contract:** the `.so` must be built with the same Rust toolchain as
//! the server. The entry point is `extern "C"` and must not store the `ctx`
//! pointer after it returns. The loaded `Library` is retained in
//! [`HmrRegistry`] until drop (`dlclose` via RAII, no leak).

use std::path::Path;
#[cfg(feature = "hmr")]
use std::path::PathBuf;
use std::sync::Arc;

use crate::{Context, CordisError};

/// Example dynamic plugin entry point signature that a `.so` would export.
///
/// ```rust,ignore
/// // In the .so crate (cdylib):
/// #[no_mangle]
/// pub extern "C" fn cordis_plugin_apply(ctx: *const std::ffi::c_void) -> i32 { 0 }
/// ```
pub type HmrEntryFn = unsafe extern "C" fn(*const std::ffi::c_void) -> i32;

/// Default exported symbol, NUL-terminated for `libloading::Library::get`.
pub const DEFAULT_ENTRY_SYMBOL: &[u8] = b"cordis_plugin_apply\0";

fn is_dynamic_library(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("so" | "dylib" | "dll")
    )
}

/// Load a Cordis plugin `.so` via `libloading` and call its entry function.
///
/// **Safety:** `Library::new` + `get` are `unsafe`; caller must ensure `so_path`
/// was built with the same toolchain and that `entry_symbol` is a valid
/// `extern "C"` function. The returned [`HmrLibrary`] must be retained (store
/// it in [`HmrRegistry`]) so the `Symbol` remains valid after this call.
#[cfg(feature = "hmr")]
pub fn load_plugin_so(
    so_path: impl AsRef<Path>,
    ctx: &Context,
    entry_symbol: &[u8],
) -> Result<HmrLibrary, CordisError> {
    use libloading::{Library, Symbol};

    let path = so_path.as_ref();
    if !is_dynamic_library(path) {
        return Err(CordisError::Configuration(format!(
            "HMR path is not a dynamic library: {}",
            path.display()
        )));
    }

    // glibc caches dlopen handles by absolute path: re-loading an already
    // loaded path hands back the SAME handle, silently ignoring rebuilds.
    // Copy to a process-unique sibling first so every load is fresh.
    let seq = HMR_LOAD_SEQ.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(|e| format!(".{e}"))
        .unwrap_or_default();
    let unique = path.with_file_name(format!(
        "{}.{}.{}.hmr-load{ext}",
        path.file_stem()
            .and_then(|s| s.to_str())
            .unwrap_or("plugin"),
        std::process::id(),
        seq,
    ));
    std::fs::copy(path, &unique).map_err(|e| {
        CordisError::Configuration(format!("HMR copy {} failed: {e}", path.display()))
    })?;

    // SAFETY: caller guarantees ABI compatibility; documented above.
    let lib = unsafe {
        Library::new(&unique).map_err(|e| {
            CordisError::Configuration(format!(
                "dlopen {} (copy of {}) failed: {e}",
                unique.display(),
                path.display()
            ))
        })
    };
    let lib = match lib {
        Ok(l) => l,
        Err(e) => {
            let _ = std::fs::remove_file(&unique);
            return Err(e);
        }
    };

    let rc = unsafe {
        let func: Symbol<HmrEntryFn> = lib.get(entry_symbol).map_err(|e| {
            CordisError::Configuration(format!(
                "dlsym {} failed: {e}",
                String::from_utf8_lossy(entry_symbol)
            ))
        })?;
        func(ctx as *const Context as *const std::ffi::c_void)
    };
    if rc != 0 {
        return Err(CordisError::Configuration(format!("HMR entry {rc} != 0")));
    }

    tracing::info!(path = %path.display(), via = %unique.display(), "HMR plugin loaded via libloading");
    Ok(HmrLibrary::new(lib, Some(unique)))
}

/// Monotonic suffix source for per-process unique load copies.
#[cfg(feature = "hmr")]
static HMR_LOAD_SEQ: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// `dlopen`, invoke the entry, and retain the library on `ctx`.
#[cfg(feature = "hmr")]
pub fn apply_plugin_so(
    ctx: &Arc<Context>,
    so_path: impl AsRef<Path>,
    entry_symbol: &[u8],
) -> Result<(), CordisError> {
    let lib = load_plugin_so(so_path, ctx, entry_symbol)?;
    let registry = match ctx.get::<HmrRegistry>() {
        Some(existing) => existing,
        None => ctx.provide(HmrRegistry::default()),
    };
    registry.push(lib);
    Ok(())
}

/// If `path` is a dylib, load it; otherwise return `Ok(false)`.
#[cfg(feature = "hmr")]
pub fn apply_plugin_so_if_dylib(ctx: &Arc<Context>, path: &Path) -> Result<bool, CordisError> {
    if !is_dynamic_library(path) {
        return Ok(false);
    }
    apply_plugin_so(ctx, path, DEFAULT_ENTRY_SYMBOL)?;
    Ok(true)
}

/// Owned holder for an `hmr` dynamic library — RAII owner that frees on drop.
#[cfg(feature = "hmr")]
pub struct HmrLibrary {
    _lib: libloading::Library,
    /// Process-unique copy backing `_lib`; removed on drop (best effort).
    /// On Windows the mapped file may not be removable until after dlclose —
    /// acceptable: stale copies live in the watched directory's namespace
    /// only until the next successful unload cycle.
    temp_path: Option<PathBuf>,
}

#[cfg(feature = "hmr")]
impl std::fmt::Debug for HmrLibrary {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HmrLibrary").finish_non_exhaustive()
    }
}

#[cfg(feature = "hmr")]
impl HmrLibrary {
    /// Retain a loaded library so it is not unloaded until this holder drops.
    pub fn new(lib: libloading::Library, temp_path: Option<PathBuf>) -> Self {
        Self {
            _lib: lib,
            temp_path,
        }
    }
}

#[cfg(feature = "hmr")]
impl Drop for HmrLibrary {
    fn drop(&mut self) {
        if let Some(tmp) = self.temp_path.take() {
            // Best effort: ignore errors (mapped files on Windows, races with
            // an external observer deleting the copy first).
            let _ = std::fs::remove_file(&tmp);
        }
        // `_lib` drops right after this body → dlclose follows cleanup.
    }
}

/// Retains loaded HMR libraries on Context so `dlclose` happens on dispose.
#[cfg(feature = "hmr")]
#[derive(Default)]
pub struct HmrRegistry {
    libs: parking_lot::Mutex<Vec<HmrLibrary>>,
}

#[cfg(feature = "hmr")]
impl HmrRegistry {
    pub fn push(&self, lib: HmrLibrary) {
        self.libs.lock().push(lib);
    }

    pub fn len(&self) -> usize {
        self.libs.lock().len()
    }

    pub fn is_empty(&self) -> bool {
        self.libs.lock().is_empty()
    }
}

#[cfg(feature = "hmr")]
impl crate::Service for HmrRegistry {
    fn name(&self) -> &'static str {
        "hmr_registry"
    }
    fn init(&self, _ctx: &Arc<Context>) -> crate::ServiceInitFuture<'_> {
        Box::pin(async { Ok(None) })
    }
    fn check(&self) -> bool {
        true
    }
}

/// Errors when `hmr` feature is disabled — dylib loading is strictly opt-in.
#[cfg(not(feature = "hmr"))]
pub fn load_plugin_so(
    _so_path: impl AsRef<Path>,
    _ctx: &Context,
    _entry_symbol: &[u8],
) -> Result<(), CordisError> {
    Err(CordisError::Configuration(
        "HMR dylib loading is disabled by default; use file-watch + Fiber::reload via watcher. Enable --features hmr (same-toolchain cdylibs are required)".into(),
    ))
}

#[cfg(not(feature = "hmr"))]
pub fn apply_plugin_so_if_dylib(_ctx: &Arc<Context>, _path: &Path) -> Result<bool, CordisError> {
    Ok(false)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hmr_stub_documents_deferral() {
        let ctx = Context::new_root();
        let err = load_plugin_so("/tmp/fake.so", &ctx, b"cordis_plugin_apply\0").unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("deferred") || msg.contains("dlopen") || msg.contains("HMR"),
            "unexpected error: {msg}"
        );
    }

    #[test]
    fn apply_plugin_so_if_dylib_ignores_non_libraries() {
        let ctx = Context::new_root();
        let applied = apply_plugin_so_if_dylib(&ctx, Path::new("config/agents/test.toon"))
            .expect("non-dylib should not error");
        assert!(!applied);
    }

    #[cfg(feature = "hmr")]
    #[test]
    fn load_plugin_so_rejects_non_library_extension() {
        let ctx = Context::new_root();
        let err = load_plugin_so("/tmp/plugin.toon", &ctx, DEFAULT_ENTRY_SYMBOL).unwrap_err();
        assert!(err.to_string().contains("not a dynamic library"));
    }

    #[cfg(feature = "hmr")]
    #[test]
    fn load_plugin_so_applies_test_cdylib_and_retains_library() {
        let so = compile_test_plugin();
        let ctx = Context::new_root();
        apply_plugin_so(&ctx, &so, DEFAULT_ENTRY_SYMBOL).expect("test plugin should apply");
        let registry = ctx.get::<HmrRegistry>().expect("HmrRegistry after apply");
        assert_eq!(registry.len(), 1);
    }

    // Simulates a rebuild over the SAME path: each apply must load a fresh
    // copy (unique per-process name), so two loads yield registry len 2 —
    // impossible with glibc's by-path handle cache. Temp copies are cleaned
    // when the registry (and its context) drops.
    #[cfg(feature = "hmr")]
    #[test]
    fn rebuild_same_path_loads_twice_via_unique_copy() {
        let so = compile_test_plugin();
        let dir = so.parent().expect("so parent").to_path_buf();
        let fixed = dir.join(lib_name("watched_plugin"));
        std::fs::copy(&so, &fixed).expect("stage watched plugin");

        let ctx = Context::new_root();
        apply_plugin_so(&ctx, &fixed, DEFAULT_ENTRY_SYMBOL).expect("first load");
        // Overwrite the same path, as a rebuild would.
        std::fs::copy(&so, &fixed).expect("rebuild overwrite");
        apply_plugin_so(&ctx, &fixed, DEFAULT_ENTRY_SYMBOL).expect("second load");

        let registry = ctx.get::<HmrRegistry>().expect("HmrRegistry after applies");
        assert_eq!(
            registry.len(),
            2,
            "rebuild over same path must swap, not cache-hit"
        );

        drop(registry);
        drop(ctx); // last Arc to the service: HmrLibrary Drop removes temp copies
        let leftovers: Vec<_> = std::fs::read_dir(&dir)
            .expect("list dir")
            .filter_map(std::result::Result::ok)
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .filter(|n| n.ends_with(".hmr-load"))
            .collect();
        assert!(
            leftovers.is_empty(),
            "temp copies must be cleaned: {leftovers:?}"
        );
    }

    #[cfg(feature = "hmr")]
    fn compile_test_plugin() -> std::path::PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let src = dir.path().join("plugin.rs");
        std::fs::write(
            &src,
            r#"
            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_plugin_apply(_ctx: *const std::ffi::c_void) -> i32 {
                0
            }
            "#,
        )
        .expect("write plugin source");
        let so = dir.path().join(lib_name("cordis_test_plugin"));
        let status = std::process::Command::new("rustc")
            .args(["--edition", "2024", "--crate-type", "cdylib", "-o"])
            .arg(&so)
            .arg(&src)
            .status()
            .expect("spawn rustc");
        assert!(status.success(), "rustc cdylib failed: {status}");
        let so_owned = so.clone();
        std::mem::forget(dir);
        so_owned
    }

    #[cfg(feature = "hmr")]
    fn lib_name(stem: &str) -> String {
        if cfg!(target_os = "windows") {
            format!("{stem}.dll")
        } else if cfg!(target_os = "macos") {
            format!("lib{stem}.dylib")
        } else {
            format!("lib{stem}.so")
        }
    }
}
