//! Worker-side supervised-exit protocol.
//!
//! This module is the worker half of a small supervision protocol: a
//! supervising daemon is future work. The daemon, once it exists, starts
//! workers with [`SUPERVISED_ENV`] set and holds the write end of their
//! stdin pipe; workers use the named exit codes and a stdin-EOF watcher to
//! report intent back.
//!
//! Two properties make the protocol loss-free:
//!
//! - stdin-EOF is the death signal: the parent keeps the write end open for
//!   the worker's whole lifetime, so EOF can only mean the parent itself is
//!   gone (clean stop or sudden death). It survives `SIGKILL`, where every
//!   message channel the parent could have opened would die with it — EOF on
//!   an already-open pipe still arrives.
//! - exit codes live in the 51–53 band so they cannot collide with common
//!   runtime codes (1–2 from shells, 101 from panics).
//!
//! Callers should not call [`watch_supervisor`] unsupervised: without the
//! marker the thread would read the terminal's (or test harness's) real
//! stdin and block or consume it.
//!
//! The plugin-dir half of hot reload also lives here:
//! [`watch_plugin_dirs`] watches library directories and fires one callback
//! after a 250 ms quiet window, so the daemon can respawn workers with fresh
//! plugin libraries (in-process unload is not possible for dlopen handles).

use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

/// Exit code asking the parent process for a hot restart.
pub const EXIT_RESTART: i32 = 51;
/// Exit code telling the parent to quit without restarting.
pub const EXIT_QUIT: i32 = 52;
/// Exit code reporting that boot never came up (bad config, unreadable
/// file); the parent exits non-zero instead of masking it.
pub const EXIT_BOOT: i32 = 53;
/// Env marker set by a supervising parent on workers it starts. Such
/// workers watch stdin: the parent holds the write end, so EOF means the
/// parent is gone (clean stop or sudden death) and the worker tears down
/// gracefully. This is the only notification a SIGKILLed parent can still send.
pub const SUPERVISED_ENV: &str = "CORDIS_SUPERVISED";

/// Whether this process was started by a supervising parent.
///
/// Workers must gate their supervisor watch and exit-code reporting on
/// this; unsupervised processes keep normal stdin/exit semantics.
pub fn supervised() -> bool {
    std::env::var_os(SUPERVISED_ENV).is_some()
}

/// Spawn the stdin-EOF watcher; meaningful only when [`supervised`].
///
/// The thread reads stdin byte-by-byte. Data on the pipe is ignored; EOF
/// (`Ok(0)`) or a read error means the parent is gone, so it runs
/// `teardown` and then exits the process with [`EXIT_QUIT`].
///
/// Returns the join handle so tests can observe the thread. Callers should
/// not call this unsupervised because stdin belongs to a terminal there.
pub fn watch_supervisor(
    teardown: impl FnOnce() + Send + 'static,
) -> std::io::Result<std::thread::JoinHandle<()>> {
    std::thread::Builder::new()
        .name("cordis-supervisor-watch".to_string())
        .spawn(move || {
            use std::io::Read;
            loop {
                match std::io::stdin().lock().read(&mut [0u8; 1]) {
                    Ok(0) | Err(_) => {
                        teardown();
                        exit(EXIT_QUIT);
                    }
                    Ok(_) => {}
                }
            }
        })
}

/// Thin indirection over [`std::process::exit`] so plugins and tests do not
/// hardcode literals.
pub fn exit(code: i32) -> ! {
    std::process::exit(code)
}

/// Whether `path` names a loadable plugin library (`.so` / `.dylib` /
/// `.dll`, case-insensitive).
///
/// Extension matching deliberately absorbs macOS realpath noise: dlopen of
/// `/proc`-style canonicalized paths and symlinked framework dirs surface
/// events whose full path differs but whose extension is still a library's.
fn is_plugin_library(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| matches!(e.to_ascii_lowercase().as_str(), "so" | "dylib" | "dll"))
}

const PLUGIN_QUIET_WINDOW: Duration = Duration::from_millis(250);

/// Watch plugin directories for library changes; fire once after a quiet
/// window.
///
/// One **non-recursive** watcher per directory. Events are collected into a
/// channel; a deadline loop arms a quiet window on the first library-touching
/// event and pushes it forward on every later one, so a burst of writes from
/// a build (linker emitting `.so`, then touching it again) collapses into a
/// single callback instead of firing mid-write. The callback cannot fire
/// before any counted event arrived.
///
/// Only files passing [`is_plugin_library`] arm or reset the timer; READMEs,
/// editor swap files, and partial artifacts with other extensions never
/// trigger a reload, and the callback cannot fire before any library change
/// happened at all.
///
/// When `on_quiet` runs the process must NOT try to unload old libraries:
/// libloading handles cannot be safely unloaded in-process while borrowed
/// services still point into them. The caller's contract is to persist state
/// if needed and call [`exit(EXIT_RESTART)`] so the supervising daemon
/// respawns the worker with fresh libraries loaded from disk.
pub fn watch_plugin_dirs(
    dirs: &[PathBuf],
    on_quiet: impl FnOnce() + Send + 'static,
) -> Result<(), String> {
    /// Whether a channel item counts toward the reload timer. A counted
    /// item touches a plugin library, carries no paths at all (cannot be
    /// proven harmless), or is a watcher-side error — all err toward
    /// reloading rather than silently swallowing a possible library change.
    fn counted(item: &Result<notify::Event, notify::Error>) -> bool {
        match item {
            Ok(event) => event.paths.is_empty() || event.paths.iter().any(|p| is_plugin_library(p)),
            Err(_) => true,
        }
    }

    let (tx, rx) = mpsc::channel();
    use notify::Watcher;
    let mut watcher = notify::RecommendedWatcher::new(tx, notify::Config::default())
        .map_err(|e| e.to_string())?;
    for dir in dirs {
        watcher
            .watch(dir, notify::RecursiveMode::NonRecursive)
            .map_err(|e| format!("{}: {e}", dir.display()))?;
    }
    std::thread::Builder::new()
        .name("cordis-plugin-dir-watch".to_string())
        .spawn(move || {
            // `watcher` (and its watches) live until this thread ends.
            let _watcher = watcher;
            // The quiet window is armed by the first counted event and
            // pushed forward by every later one; unrelated writes never
            // move it.
            let mut quiet_at: Option<std::time::Instant> = None;
            loop {
                match quiet_at {
                    None => match rx.recv() {
                        Ok(item) => {
                            if counted(&item) {
                                quiet_at = Some(std::time::Instant::now() + PLUGIN_QUIET_WINDOW);
                            }
                        }
                        Err(_) => break, // channel closed, nothing more to watch
                    },
                    Some(deadline) => {
                        let Some(remaining) =
                            deadline.checked_duration_since(std::time::Instant::now())
                        else {
                            break; // window already elapsed
                        };
                        match rx.recv_timeout(remaining) {
                            Ok(item) => {
                                if counted(&item) {
                                    quiet_at =
                                        Some(std::time::Instant::now() + PLUGIN_QUIET_WINDOW);
                                }
                            }
                            Err(mpsc::RecvTimeoutError::Timeout) => break,
                            Err(mpsc::RecvTimeoutError::Disconnected) => break,
                        }
                    }
                }
            }
            on_quiet();
        })
        // Detached watcher thread: its JoinHandle is intentionally dropped.
        .map(|_| ())
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;

    #[test]
    fn exit_codes_are_distinct() {
        assert_ne!(EXIT_RESTART, EXIT_QUIT);
        assert_ne!(EXIT_RESTART, EXIT_BOOT);
        assert_ne!(EXIT_QUIT, EXIT_BOOT);
        for code in [EXIT_RESTART, EXIT_QUIT, EXIT_BOOT] {
            assert!(
                (50..60).contains(&code),
                "exit code {code} outside the reserved 50..60 band"
            );
        }
    }

    #[test]
    fn supervised_marker_is_stable() {
        assert_eq!(SUPERVISED_ENV, "CORDIS_SUPERVISED");
    }

    #[test]
    fn watch_supervisor_spawns_named_thread() {
        // Teardown panics instead of no-op: if stdin were already at EOF
        // (headless run), exit(EXIT_QUIT) would kill the whole test binary;
        // a panic stays contained to the detached watcher thread.
        let handle = watch_supervisor(|| panic!("supervisor watcher must stay blocked on stdin"))
            .expect("watcher thread spawns");
        assert_eq!(handle.thread().name(), Some("cordis-supervisor-watch"));
        // The thread stays blocked on stdin in tests; park the main test
        // thread briefly so the spawned thread is observed started.
        std::thread::sleep(std::time::Duration::from_millis(10));
    }

    #[test]
    fn plugin_library_extension_matches_case_insensitively() {
        for name in [
            "libdemo.so",
            "PLUGIN.DYLIB",
            "thing.Dll",
            "/plugins/libdemo.SO",
        ] {
            assert!(is_plugin_library(Path::new(name)), "{name} must match");
        }
        for name in [
            "no-extension",
            "notes.md",
            "README.md",
            "lib.so.bak",
            ".hidden.so.cfg",
        ] {
            assert!(!is_plugin_library(Path::new(name)), "{name} must not match");
        }
    }

    #[test]
    fn watch_plugin_dirs_fires_on_quiet_after_library_write() {
        let dir = tempfile::tempdir().unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        watch_plugin_dirs(&[dir.path().to_path_buf()], move || {
            flag.store(true, Ordering::SeqCst)
        })
        .expect("watcher starts");
        // Give inotify a beat to arm, then drop a fake library in.
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(dir.path().join("fake_test.so"), b"pretend elf").unwrap();
        // 250 ms quiet window + scheduling slack, comfortably under 2 s.
        let deadline = std::time::Instant::now() + Duration::from_secs(2);
        while !fired.load(Ordering::SeqCst) && std::time::Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(
            fired.load(Ordering::SeqCst),
            "library write must fire on_quiet"
        );
    }

    #[test]
    fn watch_plugin_dirs_ignores_non_library_extensions() {
        let dir = tempfile::tempdir().unwrap();
        let fired = Arc::new(AtomicBool::new(false));
        let flag = fired.clone();
        watch_plugin_dirs(&[dir.path().to_path_buf()], move || {
            flag.store(true, Ordering::SeqCst)
        })
        .expect("watcher starts");
        std::thread::sleep(Duration::from_millis(50));
        std::fs::write(dir.path().join("README.md"), b"docs").unwrap();
        std::thread::sleep(Duration::from_millis(500));
        assert!(
            !fired.load(Ordering::SeqCst),
            "non-library writes must not reset or fire the quiet timer"
        );
    }
}
