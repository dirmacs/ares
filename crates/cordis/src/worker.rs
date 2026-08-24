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

#[cfg(test)]
mod tests {
    use super::*;

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
}
