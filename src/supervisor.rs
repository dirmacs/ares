//! Daemon-side half of the supervised-worker protocol.
//!
//! A server started under [`supervise`] does its real work in a child copy
//! of the same executable. The child carries the [`CHILD_ENV_MARKER`]
//! environment variable, so the worker-side stdin watcher inside the child
//! knows it is supervised and can watch standard input to detect daemon
//! death.
//!
//! Protocol summary:
//!
//! - Exit code [`EXIT_RESTART`] asks the loop to start a fresh child.
//! - Exit code [`EXIT_QUIT`] and every other terminal status end the loop.
//! - Exit code [`EXIT_BOOT`] reports a failed boot. The loop treats it like
//!   any normal terminal status and returns `Ok`; the caller mirrors the
//!   real child exit code to its own process exit, so the service manager
//!   still sees the non-zero failure.
//!
//! [`spawn_self_supervised`] creates the child and hands back the write end
//! of its standard input. Dropping that handle closes the pipe, the child's
//! standard input reaches end-of-file, and the worker-side watcher performs
//! a graceful teardown.

use std::future::Future;
use std::io;
use std::time::{Duration, Instant};

/// Environment variable that marks a supervised child process.
///
/// Same literal as the worker-side `SUPERVISED_ENV` constant. Presence
/// alone matters; the value is ignored.
pub const CHILD_ENV_MARKER: &str = "CORDIS_SUPERVISED";

/// Child exit code: restart me with a fresh process.
pub const EXIT_RESTART: i32 = 51;

/// Child exit code: shut down for good.
pub const EXIT_QUIT: i32 = 52;

/// Child exit code: boot failed; do not restart.
pub const EXIT_BOOT: i32 = 53;

/// Time window checked by the rapid-restart guard.
const RAPID_RESTART_WINDOW: Duration = Duration::from_secs(30);

/// Number of restarts inside [`RAPID_RESTART_WINDOW`] that stops the loop.
const RAPID_RESTART_LIMIT: usize = 5;

/// A running child together with the write end of its standard input.
///
/// # Lifetime contract
///
/// Hold [`stdin`](SupervisedChild::stdin) for as long as the child should
/// live. Dropping it closes the pipe: the child sees end-of-file on its
/// standard input, and the worker-side watcher tears the child down
/// gracefully. The drop alone does not kill the child; the shutdown path
/// relies on the pipe close.
pub struct SupervisedChild {
    /// The running child process.
    pub child: std::process::Child,
    /// Write end of the child's standard input pipe.
    pub stdin: std::process::ChildStdin,
}

/// Returns true when this process itself runs as a supervised child.
///
/// Children never self-supervise; see [`supervise`].
pub fn is_supervised() -> bool {
    std::env::var_os(CHILD_ENV_MARKER).is_some()
}

/// Runs the restart loop around `run_child`.
///
/// `run_child` starts one child run and yields its exit code as
/// `Option<i32>`: `Some(code)` for a known code, `None` when the status
/// carries no code (death by signal, for example). Translate
/// [`std::process::ExitStatus`] with [`std::process::ExitStatus::code`] and
/// pass its result straight through; `None` behaves like a normal terminal
/// status and ends the loop.
///
/// Behaviour:
///
/// - When this process is already a supervised child, nested supervision is
///   refused: the function returns `Ok` at once and never calls
///   `run_child`.
/// - [`EXIT_RESTART`] respawns the child.
/// - Every other outcome ends the loop with `Ok`: [`EXIT_QUIT`],
///   [`EXIT_BOOT`], any other code, and unknown statuses. [`EXIT_BOOT`]
///   therefore surfaces as an ordinary return; the caller should exit with
///   the child's real code so the service manager observes the failure.
/// - [`RAPID_RESTART_LIMIT`] restarts packed inside
///   [`RAPID_RESTART_WINDOW`] (a plugin that crashes at boot, for example)
///   stop the loop with an error instead of spinning.
pub async fn supervise<F, Fut>(run_child: F) -> Result<(), io::Error>
where
    F: Fn() -> Fut,
    Fut: Future<Output = Option<i32>>,
{
    if is_supervised() {
        return Ok(());
    }

    let mut restarts: Vec<Instant> = Vec::new();

    loop {
        let code = run_child().await;

        match code {
            Some(EXIT_RESTART) => {}
            // EXIT_QUIT, EXIT_BOOT, other codes, and unknown statuses all
            // end the loop. The caller mirrors the child's real exit code,
            // so EXIT_BOOT still reaches the service manager as a failure.
            _ => return Ok(()),
        }

        let now = Instant::now();
        restarts.retain(|at| now.duration_since(*at) < RAPID_RESTART_WINDOW);
        restarts.push(now);
        if restarts.len() >= RAPID_RESTART_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "rapid restart loop detected",
            ));
        }
    }
}

/// Re-execs the current executable as a supervised child.
///
/// The child inherits every command-line argument after the program name,
/// the whole environment, and the parent's standard output and standard
/// error. Two things change:
///
/// - [`CHILD_ENV_MARKER`] is set, so the child knows it is supervised and
///   its stdin watcher can detect daemon death.
/// - Standard input becomes a pipe whose write end is returned inside the
///   [`SupervisedChild`] handle.
///
/// Drop the [`SupervisedChild::stdin`] handle when the child should stop;
/// see the lifetime contract on [`SupervisedChild`].
pub fn spawn_self_supervised() -> Result<SupervisedChild, io::Error> {
    use std::process::{Command, Stdio};

    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .args(std::env::args_os().skip(1))
        .env(CHILD_ENV_MARKER, "1")
        .stdin(Stdio::piped())
        .stdout(Stdio::inherit())
        .stderr(Stdio::inherit());

    let mut child = command.spawn()?;
    let stdin = child.stdin.take().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            "child standard input pipe was not created",
        )
    })?;

    Ok(SupervisedChild { child, stdin })
}

#[cfg(test)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Arc;

    /// Serialises every test that touches the process-wide environment.
    ///
    /// A Tokio mutex because the guard is deliberately held across
    /// `supervise(...)` await points.
    static ENV_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

    /// Status provider for [`supervise`]: serves one queued status per
    /// call and records how often it ran.
    #[derive(Clone)]
    struct Script {
        statuses: Arc<Vec<Option<i32>>>,
        calls: Arc<AtomicUsize>,
    }

    impl Script {
        fn new(statuses: Vec<Option<i32>>) -> Self {
            Self {
                statuses: Arc::new(statuses),
                calls: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn calls(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        /// One child run: hand out the next queued status. Exhausted
        /// scripts report a clean exit so loops cannot spin past the queue.
        async fn step(self) -> Option<i32> {
            let nth = self.calls.fetch_add(1, Ordering::SeqCst);
            match self.statuses.get(nth) {
                Some(code) => *code,
                None => Some(0),
            }
        }
    }

    #[tokio::test]
    async fn child_exit_codes_drive_loop() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(CHILD_ENV_MARKER);

        let script = Script::new(vec![Some(EXIT_RESTART), Some(0)]);
        let result = supervise(|| script.clone().step()).await;

        assert!(result.is_ok());
        assert_eq!(script.calls(), 2);
    }

    #[tokio::test]
    async fn rapid_restart_cap_trips() {
        let _guard = ENV_LOCK.lock().await;
        std::env::remove_var(CHILD_ENV_MARKER);

        let script = Script::new(vec![Some(EXIT_RESTART); RAPID_RESTART_LIMIT]);
        let result = supervise(|| script.clone().step()).await;

        let err = result.expect_err("five rapid restarts must stop the loop");
        assert!(err.to_string().contains("rapid restart loop"));
        assert_eq!(script.calls(), RAPID_RESTART_LIMIT);
    }

    #[tokio::test]
    async fn supervised_mode_short_circuits() {
        let _guard = ENV_LOCK.lock().await;
        std::env::set_var(CHILD_ENV_MARKER, "1");
        assert!(is_supervised());

        let script = Script::new(Vec::new());
        let result = supervise(|| script.clone().step()).await;

        assert!(result.is_ok());
        assert_eq!(script.calls(), 0);

        std::env::remove_var(CHILD_ENV_MARKER);
    }
}
