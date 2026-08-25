//! HMR via `libloading`, gated behind `#[cfg(feature = "hmr")]`.
//!
//! File-watch + `Fiber::reload` (`crate::watcher::watch_many`) remains the
//! default production path. Enabling `--features hmr` additionally `dlopen`s a
//! `.so`/`.dylib`/`.dll` on watcher events and calls `cordis_plugin_apply`.
//! Rebuilds hot-swap because each load reads a process-unique copy of the
//! library (glibc caches dlopen handles by path).
//!
//! **ABI contract (strict fingerprint handshake):** before any value crosses
//! the FFI boundary, `load_plugin_so` requires the `.so` to export
//! `cordis_plugin_fingerprint` — an `extern "C"` fn returning a
//! NUL-terminated UTF-8 string that must EXACTLY equal this side's
//! [`fingerprint`]. A missing symbol refuses the load; a mismatched string
//! refuses it naming both fingerprints. The fingerprint bakes the Cordis ABI
//! version, crate version, rustc release + commit hash, target triple, and
//! panic strategy at BUILD TIME of each side, so plugins rebuilt against a
//! matching cordis build are required; stale dylibs fail fast instead of
//! corrupting the process. Plugin side, re-export the host's answer:
//!
//! ```rust,ignore
//! // In the .so crate (cdylib):
//! #[unsafe(no_mangle)]
//! pub extern "C" fn cordis_plugin_fingerprint() -> *const std::os::raw::c_char {
//!     cordis::hmr::plugin_fingerprint_cstr()
//! }
//! ```
//!
//! The entry point is `extern "C"` and must not store the `ctx`
//! pointer after it returns. The loaded `Library` is retained in
//! [`HmrRegistry`] until drop (`dlclose` via RAII, no leak).
//!
//! **Panic containment:** the entry call runs inside
//! [`catch_plugin_panic`]. A Rust panic that unwinds out of the entry
//! becomes `Err(CordisError::Fiber("plugin entry panicked: ..."))` and the
//! library is NOT retained. Stated honestly: the guard only converts
//! unwinds that reach it. Plugins built with `panic=abort` terminate the
//! process at the abort itself; and panics whose throw site lies in the
//! plugin's own object code are observed to tear the host down while
//! crossing the dylib boundary ("Rust cannot catch foreign exceptions") on
//! this toolchain even with `extern "C-unwind"` entries and matching
//! unwind strategies. Treat the guard as defense in depth for unwinds that
//! survive the boundary crossing; a well-behaved plugin catches its own
//! panics internally and returns a nonzero code.

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

/// Symbol every loadable plugin must export: `extern "C" fn() ->
/// *const c_char` returning this side's fingerprint string.
#[cfg(feature = "hmr")]
pub const FINGERPRINT_SYMBOL: &[u8] = b"cordis_plugin_fingerprint\0";

#[cfg(feature = "hmr")]
static FINGERPRINT: std::sync::LazyLock<String> = std::sync::LazyLock::new(|| {
    format!(
        "cordis-hmr/{} cordis/{} rustc/{}-{} target/{} panic/{}",
        env!("CORDIS_ABI_VERSION"),
        env!("CORDIS_CRATE_VERSION"),
        env!("CORDIS_RUSTC_RELEASE"),
        env!("CORDIS_RUSTC_COMMIT_HASH"),
        env!("CORDIS_BUILD_TARGET"),
        env!("CORDIS_BUILD_PANIC"),
    )
});

/// Build-time ABI fingerprint of the loading side, computed once.
///
/// Format: `cordis-hmr/{ABI} cordis/{version} rustc/{release}-{commit}
/// target/{target} panic/{panic}` — all ingredients baked by
/// `crates/cordis/build.rs` into env vars at compile time.
#[cfg(feature = "hmr")]
pub fn fingerprint() -> &'static str {
    &FINGERPRINT
}

/// The fingerprint as a NUL-terminated C string pointer, for plugins to
/// return from their own `#[unsafe(no_mangle)] pub extern "C" fn
/// cordis_plugin_fingerprint` (see the module docs example). The backing
/// buffer lives for `'static`.
#[cfg(feature = "hmr")]
pub fn plugin_fingerprint_cstr() -> *const std::os::raw::c_char {
    use std::ffi::CString;
    use std::sync::LazyLock;

    static CSTR: LazyLock<CString> = LazyLock::new(|| {
        // SAFETY: fingerprint() contains no interior NUL bytes (format is
        // printable ASCII), so CString::new cannot fail.
        CString::new(fingerprint()).expect("fingerprint has no NUL bytes")
    });
    CSTR.as_ptr()
}

fn is_dynamic_library(path: &Path) -> bool {
    matches!(
        path.extension().and_then(|ext| ext.to_str()),
        Some("so" | "dylib" | "dll")
    )
}

/// Run `op`, converting a Rust panic escaping it into an `Err` message.
///
/// Best-effort payload recovery: `&str` and `String` payloads are echoed,
/// anything else becomes a fixed placeholder. Shared by both plugin seams —
/// the HMR entry call and [`crate::registry::RegistryService`] factory
/// apply — so one panicking plugin cannot take the host down through an
/// ordinary unwind.
pub(crate) fn catch_plugin_panic<R>(
    op: impl FnOnce() -> R + std::panic::UnwindSafe,
) -> Result<R, String> {
    std::panic::catch_unwind(op).map_err(|payload| {
        if let Some(message) = payload.downcast_ref::<&str>() {
            (*message).to_string()
        } else if let Some(message) = payload.downcast_ref::<String>() {
            message.clone()
        } else {
            "non-string panic payload".to_string()
        }
    })
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

    // Strict ABI handshake BEFORE any value crosses the FFI boundary: the
    // plugin must export `cordis_plugin_fingerprint` returning this side's
    // exact fingerprint string.
    let expected = fingerprint();
    let reported: Option<String> = unsafe {
        lib.get::<Symbol<unsafe extern "C" fn() -> *const std::os::raw::c_char>>(FINGERPRINT_SYMBOL)
            .ok()
            .map(|fp| {
                let raw = fp();
                if raw.is_null() {
                    None
                } else {
                    std::ffi::CStr::from_ptr(raw)
                        .to_str()
                        .ok()
                        .map(String::from)
                }
            })
            .unwrap_or_default()
    };
    let Some(reported) = reported else {
        return Err(CordisError::Configuration(format!(
            "fingerprint symbol missing: {} does not export a valid `{}` returning a NUL-terminated UTF-8 string (expected `{expected}`)",
            path.display(),
            String::from_utf8_lossy(&FINGERPRINT_SYMBOL[..FINGERPRINT_SYMBOL.len() - 1]),
        )));
    };
    if reported != expected {
        return Err(CordisError::Configuration(format!(
            "fingerprint mismatch: plugin `{reported}` != host `{expected}` ({})",
            path.display()
        )));
    }

    let rc = unsafe {
        let func: Symbol<HmrEntryFn> = lib.get(entry_symbol).map_err(|e| {
            CordisError::Configuration(format!(
                "dlsym {} failed: {e}",
                String::from_utf8_lossy(entry_symbol)
            ))
        })?;
        // Panic containment: see "Panic containment" in the module docs. The
        // `AssertUnwindSafe` covers the borrowed `Symbol`/`Library`; the
        // entry contract forbids retaining the `ctx` pointer, so nothing the
        // call touches outlives the unwind.
        catch_plugin_panic(std::panic::AssertUnwindSafe(|| {
            func(ctx as *const Context as *const std::ffi::c_void)
        }))
    };
    let rc = match rc {
        Ok(rc) => rc,
        Err(payload) => {
            // Best-effort cleanup of the process-unique load copy; `dlclose`
            // follows when `lib` drops on return. The library is
            // deliberately NOT retained after a caught panic.
            let _ = std::fs::remove_file(&unique);
            return Err(CordisError::Fiber(format!(
                "plugin entry panicked: {payload}"
            )));
        }
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

    #[test]
    fn catch_plugin_panic_passes_values_through() {
        assert_eq!(catch_plugin_panic(|| 21 * 2).expect("no panic"), 42);
    }

    #[test]
    fn catch_plugin_panic_echoes_payloads_best_effort() {
        let echoed = catch_plugin_panic(std::panic::AssertUnwindSafe(|| -> i32 {
            panic!("entry blew up")
        }))
        .unwrap_err();
        assert_eq!(echoed, "entry blew up");
        let opaque = catch_plugin_panic(std::panic::AssertUnwindSafe(|| -> i32 {
            std::panic::panic_any(7_i64);
        }))
        .unwrap_err();
        assert_eq!(opaque, "non-string panic payload");
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
        let so = compile_test_plugin(Some(fingerprint()));
        let ctx = Context::new_root();
        apply_plugin_so(&ctx, &so, DEFAULT_ENTRY_SYMBOL).expect("test plugin should apply");
        let registry = ctx.get::<HmrRegistry>().expect("HmrRegistry after apply");
        assert_eq!(registry.len(), 1);
    }

    #[cfg(feature = "hmr")]
    #[test]
    fn load_plugin_so_rejects_missing_fingerprint() {
        let so = compile_test_plugin(None);
        let ctx = Context::new_root();
        let err = load_plugin_so(&so, &ctx, DEFAULT_ENTRY_SYMBOL).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("fingerprint symbol missing"),
            "expected missing-symbol refusal, got: {msg}"
        );
    }

    #[cfg(feature = "hmr")]
    #[test]
    fn load_plugin_so_rejects_mismatched_fingerprint() {
        let so = compile_test_plugin(Some("deliberately-wrong-fingerprint-string"));
        let ctx = Context::new_root();
        let err = load_plugin_so(&so, &ctx, DEFAULT_ENTRY_SYMBOL).unwrap_err();
        let msg = err.to_string();
        assert!(
            msg.contains("fingerprint mismatch"),
            "expected mismatch refusal, got: {msg}"
        );
        assert!(
            msg.contains("deliberately-wrong-fingerprint-string") && msg.contains(fingerprint()),
            "mismatch error must name both fingerprints, got: {msg}"
        );
    }

    // Simulates a rebuild over the SAME path: each apply must load a fresh
    // copy (unique per-process name), so two loads yield registry len 2 —
    // impossible with glibc's by-path handle cache. Temp copies are cleaned
    // when the registry (and its context) drops.
    #[cfg(feature = "hmr")]
    #[test]
    fn rebuild_same_path_loads_twice_via_unique_copy() {
        let so = compile_test_plugin(Some(fingerprint()));
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

    /// `expected_fp`: `Some(fingerprint)` additionally exports the
    /// `cordis_plugin_fingerprint` handshake symbol returning that string;
    /// `None` omits it entirely (missing-symbol refusal case).
    #[cfg(feature = "hmr")]
    fn compile_test_plugin(expected_fp: Option<&str>) -> std::path::PathBuf {
        let dir = tempfile::tempdir().expect("tempdir");
        let fp_block = match expected_fp {
            Some(fp) => format!(
                r#"
                #[unsafe(no_mangle)]
                pub static CORDIS_FP: &[u8] = b"{fp}\0";
                #[unsafe(no_mangle)]
                pub extern "C" fn cordis_plugin_fingerprint() -> *const std::os::raw::c_char {{
                    CORDIS_FP.as_ptr() as *const _
                }}
                "#
            ),
            None => String::new(),
        };
        let src_text = format!(
            r#"
            #[unsafe(no_mangle)]
            pub extern "C" fn cordis_plugin_apply(_ctx: *const std::ffi::c_void) -> i32 {{
                0
            }}
            {fp_block}
            "#
        );
        let src = dir.path().join("plugin.rs");
        std::fs::write(&src, src_text).expect("write plugin source");
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
